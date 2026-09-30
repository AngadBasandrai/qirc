use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::mem;
use std::str::FromStr;

use crate::calibration::Calibration;
use crate::progress::{self, Progress};
use crate::resources;
use crate::simulator::state::Rng;

const LANES: usize = 64;
const Z_ORDER: [usize; 4] = [0, 2, 1, 3];
const SINGLE: [(bool, bool); 3] = [(true, false), (true, true), (false, true)];

type Injected = HashMap<usize, Vec<(Effect, u64)>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    X,
    Z,
}

struct Stabilizer {
    kind: Kind,
    ancilla: usize,
    corners: [Option<usize>; 4],
}

impl Stabilizer {
    fn support(&self) -> Vec<usize> {
        self.corners.iter().flatten().copied().collect()
    }
}

struct Code {
    distance: usize,
    stabilizers: Vec<Stabilizer>,
}

impl Code {
    fn new(d: usize) -> Code {
        let data = |r: isize, c: isize| {
            (r >= 0 && c >= 0 && (r as usize) < d && (c as usize) < d)
                .then(|| r as usize * d + c as usize)
        };
        let mut stabilizers = Vec::new();
        for i in 0..=d as isize {
            for j in 0..=d as isize {
                let kind = if (i + j) % 2 == 0 { Kind::X } else { Kind::Z };
                let corners = [
                    data(i - 1, j - 1),
                    data(i - 1, j),
                    data(i, j - 1),
                    data(i, j),
                ];
                let edge_row = i == 0 || i == d as isize;
                let edge_column = j == 0 || j == d as isize;
                let keep = match corners.iter().flatten().count() {
                    4 => true,
                    2 => (edge_row && kind == Kind::X) || (edge_column && kind == Kind::Z),
                    _ => false,
                };
                if keep {
                    stabilizers.push(Stabilizer {
                        kind,
                        ancilla: d * d + stabilizers.len(),
                        corners,
                    });
                }
            }
        }
        Code {
            distance: d,
            stabilizers,
        }
    }

    fn qubits(&self) -> usize {
        self.distance * self.distance + self.stabilizers.len()
    }

    fn logical(&self) -> Vec<usize> {
        (0..self.distance).collect()
    }
}

#[derive(Clone)]
enum Step {
    Reset(Vec<usize>),
    Hadamard(Vec<usize>),
    Cnot(Vec<(usize, usize)>),
    Measure(Vec<usize>),
    Noise(Vec<usize>),
    Pairs(Vec<(usize, usize)>),
}

struct Circuit {
    steps: Vec<Step>,
    qubits: usize,
    measurements: usize,
    detectors: Vec<Vec<usize>>,
    logical: Vec<usize>,
}

fn memory(code: &Code, rounds: usize) -> Circuit {
    let d = code.distance;
    let data: Vec<usize> = (0..d * d).collect();
    let ancillas: Vec<usize> = code.stabilizers.iter().map(|s| s.ancilla).collect();
    let xs: Vec<usize> = code
        .stabilizers
        .iter()
        .filter(|s| s.kind == Kind::X)
        .map(|s| s.ancilla)
        .collect();
    let mut steps = vec![Step::Reset(data.clone())];
    let mut measured = 0;
    let mut previous: HashMap<usize, usize> = HashMap::new();
    let mut detectors = Vec::new();
    for _ in 0..rounds {
        steps.push(Step::Noise(data.clone()));
        steps.push(Step::Reset(ancillas.clone()));
        steps.push(Step::Hadamard(xs.clone()));
        steps.push(Step::Noise(xs.clone()));
        for (layer, &z_corner) in Z_ORDER.iter().enumerate() {
            let pairs: Vec<(usize, usize)> = code
                .stabilizers
                .iter()
                .filter_map(|s| match s.kind {
                    Kind::Z => s.corners[z_corner].map(|q| (q, s.ancilla)),
                    Kind::X => s.corners[layer].map(|q| (s.ancilla, q)),
                })
                .collect();
            steps.push(Step::Cnot(pairs.clone()));
            steps.push(Step::Pairs(pairs));
        }
        steps.push(Step::Hadamard(xs.clone()));
        steps.push(Step::Noise(xs.clone()));
        steps.push(Step::Measure(ancillas.clone()));
        for s in code.stabilizers.iter().filter(|s| s.kind == Kind::Z) {
            let index = measured + (s.ancilla - d * d);
            let mut detector = vec![index];
            detector.extend(previous.get(&s.ancilla));
            detectors.push(detector);
            previous.insert(s.ancilla, index);
        }
        measured += ancillas.len();
    }
    steps.push(Step::Measure(data.clone()));
    for s in code.stabilizers.iter().filter(|s| s.kind == Kind::Z) {
        let mut detector: Vec<usize> = s.support().iter().map(|q| measured + q).collect();
        detector.extend(previous.get(&s.ancilla));
        detectors.push(detector);
    }
    let logical = code.logical().iter().map(|q| measured + q).collect();
    measured += data.len();
    Circuit {
        steps,
        qubits: code.qubits(),
        measurements: measured,
        detectors,
        logical,
    }
}

#[derive(Clone, Copy)]
enum Effect {
    Pauli(usize, bool, bool),
    Pair(usize, usize, u8),
    Flip(usize),
}

struct Frames {
    x: Vec<u64>,
    z: Vec<u64>,
    record: Vec<u64>,
}

fn hits(rng: &mut Rng, p: f64) -> u64 {
    if p <= 0.0 {
        return 0;
    }
    let mut mask = 0u64;
    let skip = (1.0 - p).ln();
    let mut at = 0.0f64;
    loop {
        at += (rng.next_unit().max(f64::MIN_POSITIVE).ln() / skip).floor();
        if at >= LANES as f64 {
            return mask;
        }
        mask |= 1 << at as u32;
        at += 1.0;
    }
}

fn pick(rng: &mut Rng, mask: u64, choices: u64) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    let mut rest = mask;
    while rest != 0 {
        let lane = rest.trailing_zeros();
        out.push((lane, rng.next_u64() % choices));
        rest &= rest - 1;
    }
    out
}

fn inject(batch: &[(usize, Effect)]) -> Injected {
    let mut injected = Injected::new();
    for (lane, &(at, effect)) in batch.iter().enumerate() {
        injected.entry(at).or_default().push((effect, 1 << lane));
    }
    injected
}

fn lit(syndrome: &[u64], lane: usize) -> Vec<usize> {
    let bit = 1u64 << lane;
    syndrome
        .iter()
        .enumerate()
        .filter(|&(_, f)| f & bit != 0)
        .map(|(i, _)| i)
        .collect()
}

impl Frames {
    fn flip(&mut self, q: usize, x: bool, z: bool, lanes: u64) {
        if x {
            self.x[q] ^= lanes;
        }
        if z {
            self.z[q] ^= lanes;
        }
    }

    fn apply(&mut self, effect: Effect, lanes: u64) {
        match effect {
            Effect::Pauli(q, x, z) => self.flip(q, x, z, lanes),
            Effect::Pair(a, b, pauli) => {
                for (q, bits) in [(a, pauli & 3), (b, pauli >> 2)] {
                    self.flip(q, bits & 1 == 1, bits & 2 == 2, lanes);
                }
            }
            Effect::Flip(m) => self.record[m] ^= lanes,
        }
    }

    fn run(circuit: &Circuit, p: f64, rng: &mut Rng, injected: &Injected) -> Frames {
        let mut frames = Frames {
            x: vec![0; circuit.qubits],
            z: vec![0; circuit.qubits],
            record: vec![0; circuit.measurements],
        };
        let mut measured = 0;
        for (at, step) in circuit.steps.iter().enumerate() {
            match step {
                Step::Reset(qubits) => {
                    for &q in qubits {
                        frames.x[q] = hits(rng, p);
                        frames.z[q] = 0;
                    }
                }
                Step::Hadamard(qubits) => {
                    for &q in qubits {
                        mem::swap(&mut frames.x[q], &mut frames.z[q]);
                    }
                }
                Step::Cnot(pairs) => {
                    for &(c, t) in pairs {
                        frames.x[t] ^= frames.x[c];
                        frames.z[c] ^= frames.z[t];
                    }
                }
                Step::Measure(qubits) => {
                    for &q in qubits {
                        frames.record[measured] = frames.x[q] ^ hits(rng, p);
                        measured += 1;
                    }
                }
                Step::Noise(qubits) => {
                    for &q in qubits {
                        let mask = hits(rng, p);
                        for (lane, pauli) in pick(rng, mask, 3) {
                            let (x, z) = SINGLE[pauli as usize];
                            frames.flip(q, x, z, 1 << lane);
                        }
                    }
                }
                Step::Pairs(pairs) => {
                    for &(a, b) in pairs {
                        let mask = hits(rng, p);
                        for (lane, pauli) in pick(rng, mask, 15) {
                            frames.apply(Effect::Pair(a, b, pauli as u8 + 1), 1 << lane);
                        }
                    }
                }
            }
            if let Some(effects) = injected.get(&at) {
                for &(effect, lanes) in effects {
                    frames.apply(effect, lanes);
                }
            }
        }
        frames
    }

    fn parity(&self, measurements: &[usize]) -> u64 {
        measurements.iter().fold(0, |acc, &m| acc ^ self.record[m])
    }

    fn syndrome(&self, circuit: &Circuit) -> Vec<u64> {
        circuit.detectors.iter().map(|d| self.parity(d)).collect()
    }
}

fn faults(circuit: &Circuit) -> Vec<(usize, Effect)> {
    let mut out = Vec::new();
    let mut measured = 0;
    for (at, step) in circuit.steps.iter().enumerate() {
        match step {
            Step::Reset(qubits) => {
                out.extend(qubits.iter().map(|&q| (at, Effect::Pauli(q, true, false))));
            }
            Step::Measure(qubits) => {
                for _ in qubits {
                    out.push((at, Effect::Flip(measured)));
                    measured += 1;
                }
            }
            Step::Noise(qubits) => {
                for &q in qubits {
                    out.extend(SINGLE.map(|(x, z)| (at, Effect::Pauli(q, x, z))));
                }
            }
            Step::Pairs(pairs) => {
                for &(a, b) in pairs {
                    out.extend((1..16u8).map(|pauli| (at, Effect::Pair(a, b, pauli))));
                }
            }
            Step::Hadamard(_) | Step::Cnot(_) => {}
        }
    }
    out
}

fn root(parent: &mut [usize], mut n: usize) -> usize {
    while parent[n] != n {
        parent[n] = parent[parent[n]];
        n = parent[n];
    }
    n
}

struct Graph {
    nodes: usize,
    edges: Vec<(usize, usize, bool)>,
    around: Vec<Vec<usize>>,
}

impl Graph {
    fn build(circuit: &Circuit) -> Graph {
        let boundary = circuit.detectors.len();
        let mut found: BTreeMap<(usize, usize), bool> = BTreeMap::new();
        let mut rng = Rng::new(0);
        for batch in faults(circuit).chunks(LANES) {
            let frames = Frames::run(circuit, 0.0, &mut rng, &inject(batch));
            let syndrome = frames.syndrome(circuit);
            let logical = frames.parity(&circuit.logical);
            for lane in 0..batch.len() {
                let key = match lit(&syndrome, lane)[..] {
                    [a] => (a, boundary),
                    [a, b] => (a.min(b), a.max(b)),
                    _ => continue,
                };
                found.entry(key).or_insert(logical >> lane & 1 == 1);
            }
        }
        let edges: Vec<(usize, usize, bool)> =
            found.into_iter().map(|((a, b), o)| (a, b, o)).collect();
        let mut around = vec![Vec::new(); boundary + 1];
        for (i, &(a, b, _)) in edges.iter().enumerate() {
            around[a].push(i);
            around[b].push(i);
        }
        Graph {
            nodes: boundary + 1,
            edges,
            around,
        }
    }

    fn decode(&self, defects: &[usize]) -> bool {
        let boundary = self.nodes - 1;
        let mut parent: Vec<usize> = (0..self.nodes).collect();
        let mut odd = vec![false; self.nodes];
        let mut open = vec![true; self.nodes];
        let mut members: Vec<Vec<usize>> = (0..self.nodes).map(|n| vec![n]).collect();
        let mut marked = vec![false; self.nodes];
        let mut growth = vec![0u8; self.edges.len()];
        for &d in defects {
            odd[d] ^= true;
            marked[d] ^= true;
        }
        open[boundary] = false;
        loop {
            let active: Vec<usize> = (0..self.nodes)
                .filter(|&n| parent[n] == n && odd[n] && open[n])
                .collect();
            if active.is_empty() {
                break;
            }
            let mut grown = Vec::new();
            let mut changed = false;
            for &cluster in &active {
                for &node in &members[cluster] {
                    for &e in &self.around[node] {
                        if growth[e] < 2 {
                            growth[e] += 1;
                            changed = true;
                            if growth[e] == 2 {
                                grown.push(e);
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
            for e in grown {
                let (a, b, _) = self.edges[e];
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                if ra == rb {
                    continue;
                }
                let (big, small) = if members[ra].len() >= members[rb].len() {
                    (ra, rb)
                } else {
                    (rb, ra)
                };
                parent[small] = big;
                let moved = mem::take(&mut members[small]);
                members[big].extend(moved);
                odd[big] ^= odd[small];
                open[big] = open[big] && open[small];
            }
        }
        let mut seen = vec![false; self.nodes];
        let mut prediction = false;
        for start in [boundary].into_iter().chain(0..boundary) {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let mut order = vec![(start, None)];
            let mut at = 0;
            while at < order.len() {
                let (node, _) = order[at];
                at += 1;
                for &e in &self.around[node] {
                    if growth[e] < 2 {
                        continue;
                    }
                    let (a, b, _) = self.edges[e];
                    let other = if a == node { b } else { a };
                    if !seen[other] {
                        seen[other] = true;
                        order.push((other, Some(e)));
                    }
                }
            }
            for &(node, edge) in order.iter().rev() {
                let Some(edge) = edge.filter(|_| marked[node]) else {
                    continue;
                };
                let (a, b, observable) = self.edges[edge];
                let other = if a == node { b } else { a };
                marked[node] = false;
                marked[other] ^= true;
                prediction ^= observable;
            }
        }
        prediction
    }
}

struct Point {
    distance: usize,
    rounds: usize,
    shots: u64,
    failures: u64,
    per_round: f64,
    model: f64,
}

fn simulate(distance: usize, rounds: usize, p: f64, shots: u64, seed: u64) -> Point {
    let circuit = memory(&Code::new(distance), rounds);
    let graph = Graph::build(&circuit);
    let mut rng = Rng::new(seed);
    let none = Injected::new();
    let mut failures = 0;
    let mut progress = Progress::new("surface code, shots", shots);
    let mut done = 0;
    while done < shots {
        if progress::stopped() {
            break;
        }
        progress.tick(done);
        let frames = Frames::run(&circuit, p, &mut rng, &none);
        let syndrome = frames.syndrome(&circuit);
        let logical = frames.parity(&circuit.logical);
        let lanes = (shots - done).min(LANES as u64) as usize;
        for lane in 0..lanes {
            if graph.decode(&lit(&syndrome, lane)) != (logical >> lane & 1 == 1) {
                failures += 1;
            }
        }
        done += lanes as u64;
    }
    let total = failures as f64 / done.max(1) as f64;
    let per_round = (1.0 - (1.0 - 2.0 * total).max(0.0).powf(1.0 / rounds as f64)) / 2.0;
    Point {
        distance,
        rounds,
        shots: done,
        failures,
        per_round,
        model: resources::logical_rate(p, distance),
    }
}

fn report(points: &[Point], p: f64) -> String {
    let mut out =
        format!("surface code memory, physical error {p} on every gate, reset and measurement\n\n");
    writeln!(
        out,
        "{:>8} {:>7} {:>9} {:>9} {:>14} {:>14}",
        "distance", "rounds", "shots", "failures", "per round", "model"
    )
    .unwrap();
    for point in points {
        writeln!(
            out,
            "{:>8} {:>7} {:>9} {:>9} {:>14.3e} {:>14.3e}",
            point.distance, point.rounds, point.shots, point.failures, point.per_round, point.model
        )
        .unwrap();
    }
    out.push_str("\nper round is the simulated logical error per round of stabilizer measurement, decoded by union find;\nmodel is 0.03 (p/0.01)^((d+1)/2), the rate --emit resources assumes\n");
    out
}

fn whole<T: FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} needs a whole number, not `{text}`"))
}

pub fn command(args: &[String]) -> Result<String, String> {
    let mut distances = vec![3, 5, 7];
    let mut rounds = None;
    let mut p = 1e-3;
    let mut shots = 100_000u64;
    let mut seed = 1u64;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--distance" => {
                distances = value
                    .split(',')
                    .map(|d| match d.trim().parse::<usize>() {
                        Ok(d) if (3..=15).contains(&d) && d % 2 == 1 => Ok(d),
                        _ => Err(format!("distance `{d}` must be odd, from 3 to 15")),
                    })
                    .collect::<Result<_, _>>()?;
            }
            "--rounds" => match whole(flag, value)? {
                0 => return Err("--rounds needs at least one round".into()),
                n => rounds = Some(n),
            },
            "--error" => {
                p = match value.parse::<f64>() {
                    Ok(e) if (0.0..0.5).contains(&e) => e,
                    _ => return Err("--error needs a probability below 0.5".into()),
                };
            }
            "--calibration" => {
                let text = fs::read_to_string(value)
                    .map_err(|error| format!("cannot read calibration {value}: {error}"))?;
                p = Calibration::parse(&text)?
                    .surface()
                    .0
                    .ok_or("the calibration gives no cx error rates")?;
            }
            "--shots" => shots = whole(flag, value)?,
            "--seed" => seed = whole(flag, value)?,
            other => return Err(format!("unknown option `{other}` for qirc surface")),
        }
    }
    let points: Vec<Point> = distances
        .iter()
        .map(|&d| simulate(d, rounds.unwrap_or(d), p, shots, seed))
        .collect();
    Ok(report(&points, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commutes(a: &[usize], b: &[usize]) -> bool {
        a.iter().filter(|q| b.contains(q)).count() % 2 == 0
    }

    #[test]
    fn layouts() {
        for d in [3, 5, 7] {
            let code = Code::new(d);
            let supports = |kind: Kind| -> Vec<Vec<usize>> {
                code.stabilizers
                    .iter()
                    .filter(|s| s.kind == kind)
                    .map(Stabilizer::support)
                    .collect()
            };
            let (xs, zs) = (supports(Kind::X), supports(Kind::Z));
            assert_eq!(
                (xs.len(), zs.len()),
                ((d * d - 1) / 2, (d * d - 1) / 2),
                "{d}"
            );
            for x in &xs {
                assert!(zs.iter().all(|z| commutes(x, z)), "{d}");
                assert!(commutes(x, &code.logical()), "{d}");
            }
            let column: Vec<usize> = (0..d).map(|r| r * d).collect();
            assert!(zs.iter().all(|z| commutes(z, &column)), "{d}");
            assert!(!commutes(&code.logical(), &column), "{d}");
        }
    }

    #[test]
    fn corrects_faults() {
        for d in [3, 5] {
            let circuit = memory(&Code::new(d), d);
            let graph = Graph::build(&circuit);
            let mut rng = Rng::new(1);
            for batch in faults(&circuit).chunks(LANES) {
                let frames = Frames::run(&circuit, 0.0, &mut rng, &inject(batch));
                let syndrome = frames.syndrome(&circuit);
                let logical = frames.parity(&circuit.logical);
                for lane in 0..batch.len() {
                    let defects = lit(&syndrome, lane);
                    assert_eq!(
                        graph.decode(&defects),
                        logical >> lane & 1 == 1,
                        "{d} {defects:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn distance_helps() {
        let noiseless = simulate(3, 3, 0.0, 640, 1);
        assert_eq!(noiseless.failures, 0);
        let small = simulate(3, 3, 0.004, 64_000, 2);
        let large = simulate(5, 5, 0.004, 64_000, 3);
        assert!(
            small.per_round > 2.0 * large.per_round,
            "{} {}",
            small.per_round,
            large.per_round
        );
    }
}
