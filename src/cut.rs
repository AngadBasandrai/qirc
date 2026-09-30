use std::collections::HashMap;

use crate::diag::Span;
use crate::ir::*;
use crate::progress::Progress;
use crate::simulator::exec::{Backend, Sample};
use crate::simulator::matrix::{C64, Matrix2, matrix_for};
use crate::simulator::state::{self, Rng, Sampler, State};
use crate::transpile::{self, GateSet};

pub(crate) type Weighted = (f64, Vec<(usize, bool, bool)>);

const MAX_BYTES: u64 = if cfg!(target_arch = "wasm32") {
    1 << 28
} else {
    1 << 31
};
const MAX_WORK: f64 = 2e10;
const MAX_MEASURED: usize = 20;
const MIN_PIECE: usize = 8;

enum Step {
    Gate(Gate, Vec<f64>),
    Factor {
        cut: usize,
        wire: usize,
        first: bool,
    },
}

struct Piece {
    qubits: usize,
    steps: Vec<Step>,
    cuts: Vec<usize>,
}

type Terms = Vec<(Matrix2, Matrix2)>;
type Constant = (Gate, Vec<f64>);

struct Tree<'a> {
    piece: &'a Piece,
    cuts: &'a [Terms],
    strides: Vec<usize>,
    leaves: Vec<(usize, State)>,
}

impl Tree<'_> {
    fn grow(&mut self, from: usize, mut state: State, index: usize) {
        for (at, step) in self.piece.steps.iter().enumerate().skip(from) {
            match *step {
                Step::Gate(ref gate, ref params) => state.gate(gate, params),
                Step::Factor { cut, wire, first } => {
                    let last = self.cuts[cut].len() - 1;
                    for digit in 0..last {
                        self.branch(at, state.clone(), (cut, wire, first, digit), index);
                    }
                    self.branch(at, state, (cut, wire, first, last), index);
                    return;
                }
            }
        }
        self.leaves.push((index, state));
    }

    fn branch(
        &mut self,
        at: usize,
        mut state: State,
        (cut, wire, first, digit): (usize, usize, bool, usize),
        index: usize,
    ) {
        let (mine, theirs) = self.cuts[cut][digit];
        state.apply(if first { &mine } else { &theirs }, wire, 0);
        self.grow(at + 1, state, index + digit * self.strides[cut]);
    }
}

fn crossing(weight: &[Vec<usize>], side: &[bool]) -> usize {
    let mut total = 0;
    for a in 0..side.len() {
        for b in a + 1..side.len() {
            if side[a] != side[b] {
                total += weight[a][b];
            }
        }
    }
    total
}

fn split(qubits: usize, pairs: &[(usize, usize)], low: usize, high: usize) -> Option<Vec<bool>> {
    let (low, high) = (low.max(1), high.min(qubits - 1));
    if low > high {
        return None;
    }
    let mut weight = vec![vec![0; qubits]; qubits];
    for &(a, b) in pairs {
        weight[a][b] += 1;
        weight[b][a] += 1;
    }
    let fits = |side: &[bool]| (low..=high).contains(&side.iter().filter(|&&s| s).count());
    let mut side = (low..=high)
        .map(|at| (0..qubits).map(|q| q >= qubits - at).collect::<Vec<bool>>())
        .filter(|side| fits(side))
        .min_by_key(|side| crossing(&weight, side))?;
    let mut current = crossing(&weight, &side);
    loop {
        let mut moves: Vec<Vec<usize>> = (0..qubits).map(|q| vec![q]).collect();
        for a in 0..qubits {
            for b in a + 1..qubits {
                if side[a] != side[b] {
                    moves.push(vec![a, b]);
                }
            }
        }
        let better = moves.into_iter().find_map(|flips| {
            let mut trial = side.clone();
            for q in flips {
                trial[q] = !trial[q];
            }
            let cost = crossing(&weight, &trial);
            (fits(&trial) && cost < current).then_some((trial, cost))
        });
        match better {
            Some((trial, cost)) => {
                side = trial;
                current = cost;
            }
            None => return Some(side),
        }
    }
}

fn partition(
    members: Vec<usize>,
    pairs: &[(usize, usize)],
    limit: usize,
    owner: &mut [usize],
    pieces: &mut usize,
) -> Option<()> {
    if members.len() <= limit {
        for q in members {
            owner[q] = *pieces;
        }
        *pieces += 1;
        return Some(());
    }
    let position: HashMap<usize, usize> =
        members.iter().enumerate().map(|(i, &q)| (q, i)).collect();
    let inside: Vec<(usize, usize)> = pairs
        .iter()
        .filter_map(|(a, b)| Some((*position.get(a)?, *position.get(b)?)))
        .collect();
    let rest = (members.len().div_ceil(limit) - 1) * limit;
    let side = split(
        members.len(),
        &inside,
        members.len().saturating_sub(rest),
        limit,
    )?;
    let (right, left): (Vec<usize>, Vec<usize>) =
        members.iter().partition(|&&q| side[position[&q]]);
    partition(left, pairs, limit, owner, pieces)?;
    partition(right, pairs, limit, owner, pieces)
}

fn factors(gate: &Gate, params: &[f64]) -> Option<(Terms, usize, usize)> {
    let half = |m: Matrix2| {
        let s = C64::new(0.5, 0.0);
        Matrix2::new(m.a * s, m.b * s, m.c * s, m.d * s)
    };
    let (zero, one) = (C64::default(), C64::new(1.0, 0.0));
    match (gate.kind, &gate.controls[..], &gate.targets[..]) {
        (GateKind::Swap, [], [a, b]) => Some((
            [
                Matrix2::identity(),
                Matrix2::x(),
                Matrix2::y(),
                Matrix2::z(),
            ]
            .into_iter()
            .map(|p| (half(p), p))
            .collect(),
            a.index(),
            b.index(),
        )),
        (kind, [c], [t]) if kind != GateKind::Swap => Some((
            vec![
                (Matrix2::new(one, zero, zero, zero), Matrix2::identity()),
                (
                    Matrix2::new(zero, zero, zero, one),
                    matrix_for(kind, params),
                ),
            ],
            c.index(),
            t.index(),
        )),
        _ => None,
    }
}

pub(crate) struct Cutting {
    owner: Vec<usize>,
    local: Vec<usize>,
    pieces: Vec<Piece>,
    cuts: Vec<Terms>,
    count: usize,
    work: f64,
}

impl Cutting {
    fn fitted(gates: &[Constant], qubits: usize) -> Result<Cutting, String> {
        let mut error = String::new();
        let mut best: Option<Cutting> = None;
        for limit in (MIN_PIECE..=state::MAX_QUBITS).rev() {
            match Cutting::plan(gates, qubits, limit) {
                Ok(plan) if best.as_ref().is_none_or(|b| plan.work < b.work) => best = Some(plan),
                Ok(_) => {}
                Err(reason) if limit == state::MAX_QUBITS => error = reason,
                Err(_) => {}
            }
        }
        best.ok_or(error)
    }

    fn plan(gates: &[Constant], qubits: usize, limit: usize) -> Result<Cutting, String> {
        let pairs: Vec<(usize, usize)> = gates
            .iter()
            .filter_map(
                |(gate, _)| match gate.wires().map(|q| q.index()).collect::<Vec<_>>()[..] {
                    [a, b] => Some((a, b)),
                    _ => None,
                },
            )
            .collect();
        let mut owner = vec![0; qubits];
        let mut count = 0;
        partition((0..qubits).collect(), &pairs, limit, &mut owner, &mut count)
            .ok_or_else(|| format!("{qubits} qubits cannot be cut into state vectors"))?;
        let mut sizes = vec![0; count];
        let mut local = vec![0; qubits];
        for q in 0..qubits {
            local[q] = sizes[owner[q]];
            sizes[owner[q]] += 1;
        }
        let mut pieces: Vec<Piece> = sizes
            .iter()
            .map(|&qubits| Piece {
                qubits,
                steps: Vec::new(),
                cuts: Vec::new(),
            })
            .collect();
        let mut cuts = Vec::new();
        for (gate, params) in gates {
            let params = params.clone();
            let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
            if wires.iter().all(|&q| owner[q] == owner[wires[0]]) {
                let mut moved = gate.clone();
                for q in moved.controls.iter_mut().chain(moved.targets.iter_mut()) {
                    *q = QubitId(local[q.index()] as u32);
                }
                pieces[owner[wires[0]]]
                    .steps
                    .push(Step::Gate(moved, params));
                continue;
            }
            let (terms, first, second) = factors(gate, &params)
                .ok_or_else(|| format!("cannot cut through `{}`", gate.kind.name()))?;
            for (q, is_first) in [(first, true), (second, false)] {
                let piece = &mut pieces[owner[q]];
                piece.steps.push(Step::Factor {
                    cut: cuts.len(),
                    wire: local[q],
                    first: is_first,
                });
                piece.cuts.push(cuts.len());
            }
            cuts.push(terms);
        }
        let too_big = || {
            let sizes: Vec<String> = sizes.iter().map(ToString::to_string).collect();
            format!(
                "cutting it into pieces of {} qubits takes {} cut{}, too many to recombine",
                sizes.join(", "),
                cuts.len(),
                if cuts.len() == 1 { "" } else { "s" }
            )
        };
        let count = cuts
            .iter()
            .map(Vec::len)
            .try_fold(1usize, |total, n| total.checked_mul(n))
            .ok_or_else(too_big)?;
        let mut bytes = 0u64;
        let mut work = (count as f64).powi(2) / 2.0 * pieces.len() as f64;
        for piece in &pieces {
            let states: usize = piece.cuts.iter().map(|&c| cuts[c].len()).product();
            let amplitudes = 1u64 << piece.qubits;
            bytes = bytes.saturating_add(
                (states as u64)
                    .saturating_mul(amplitudes)
                    .saturating_mul(16),
            );
            let mut branches = 1.0;
            let mut evolved = 0.0;
            for step in &piece.steps {
                match *step {
                    Step::Gate(..) => evolved += branches,
                    Step::Factor { cut, .. } => branches *= cuts[cut].len() as f64,
                }
            }
            work += ((states as f64).powi(2) / 2.0 + evolved / 4.0) * amplitudes as f64;
        }
        if bytes > MAX_BYTES || work > MAX_WORK {
            return Err(too_big());
        }
        Ok(Cutting {
            owner,
            local,
            pieces,
            cuts,
            count,
            work,
        })
    }

    fn states(&self) -> Vec<Vec<State>> {
        let mut progress = Progress::new("cutting, pieces", self.pieces.len() as u64);
        self.pieces
            .iter()
            .enumerate()
            .map(|(at, piece)| {
                progress.tick(at as u64);
                let mut strides = vec![0; self.cuts.len()];
                let mut stride = 1;
                for &cut in &piece.cuts {
                    strides[cut] = stride;
                    stride *= self.cuts[cut].len();
                }
                let mut tree = Tree {
                    piece,
                    cuts: &self.cuts,
                    strides,
                    leaves: Vec::new(),
                };
                tree.grow(0, State::new(piece.qubits), 0);
                tree.leaves.sort_by_key(|(index, _)| *index);
                tree.leaves.into_iter().map(|(_, state)| state).collect()
            })
            .collect()
    }

    fn locals(&self) -> Vec<Vec<usize>> {
        self.pieces
            .iter()
            .map(|piece| {
                (0..self.count)
                    .map(|mut index| {
                        let mut digits = vec![0; self.cuts.len()];
                        for (cut, terms) in self.cuts.iter().enumerate() {
                            digits[cut] = index % terms.len();
                            index /= terms.len();
                        }
                        let mut local = 0;
                        let mut stride = 1;
                        for &cut in &piece.cuts {
                            local += digits[cut] * stride;
                            stride *= self.cuts[cut].len();
                        }
                        local
                    })
                    .collect()
            })
            .collect()
    }

    fn combine(&self, locals: &[Vec<usize>], tables: &[(usize, &[C64])]) -> f64 {
        let mut sum = 0.0;
        for ket in 0..self.count {
            for bra in 0..=ket {
                let product: C64 = tables
                    .iter()
                    .zip(locals)
                    .map(|((width, table), local)| table[local[bra] * width + local[ket]])
                    .product();
                sum += if bra == ket {
                    product.re
                } else {
                    2.0 * product.re
                };
            }
        }
        sum
    }

    fn expectation(&self, terms: &[Weighted]) -> f64 {
        let states = self.states();
        let locals = self.locals();
        let mut total = 0.0;
        for (weight, paulis) in terms {
            let mut masks = vec![(0, 0); self.pieces.len()];
            for &(q, flips, phases) in paulis {
                let (x, z) = &mut masks[self.owner[q]];
                *x |= usize::from(flips) << self.local[q];
                *z |= usize::from(phases) << self.local[q];
            }
            let tables: Vec<(usize, Vec<C64>)> = states
                .iter()
                .zip(&masks)
                .map(|(piece, &(x, z))| {
                    let width = piece.len();
                    let table = (0..width * width)
                        .map(|i| piece[i / width].pauli_element(&piece[i % width], x, z))
                        .collect();
                    (width, table)
                })
                .collect();
            let borrowed: Vec<(usize, &[C64])> =
                tables.iter().map(|(w, t)| (*w, t.as_slice())).collect();
            total += weight * self.combine(&locals, &borrowed);
        }
        total
    }

    fn distribution(&self, measured: &[usize]) -> Vec<f64> {
        let states = self.states();
        let locals = self.locals();
        let mut tables: Vec<Vec<(usize, Vec<C64>)>> = Vec::new();
        let mut positions: Vec<Vec<usize>> = Vec::new();
        for (p, piece) in states.iter().enumerate() {
            let bits: Vec<(usize, usize)> = measured
                .iter()
                .enumerate()
                .filter(|&(_, &q)| self.owner[q] == p)
                .map(|(i, &q)| (i, self.local[q]))
                .collect();
            let width = piece.len();
            let mut table = vec![vec![C64::default(); width * width]; 1 << bits.len()];
            for j in 0..piece.first().map_or(0, State::len) {
                let y = bits
                    .iter()
                    .enumerate()
                    .fold(0, |y, (k, &(_, l))| y | ((j >> l) & 1) << k);
                for (i, cell) in table[y].iter_mut().enumerate() {
                    *cell += piece[i / width].amplitude(j).conj() * piece[i % width].amplitude(j);
                }
            }
            tables.push(table.into_iter().map(|t| (width, t)).collect());
            positions.push(bits.into_iter().map(|(i, _)| i).collect());
        }
        let mut progress = Progress::new("cutting, outcomes", 1u64 << measured.len());
        let mut probabilities: Vec<f64> = (0..1usize << measured.len())
            .map(|outcome| {
                progress.tick(outcome as u64);
                let chosen: Vec<(usize, &[C64])> = tables
                    .iter()
                    .zip(&positions)
                    .map(|(table, bits)| {
                        let y = bits
                            .iter()
                            .enumerate()
                            .fold(0, |y, (k, &i)| y | ((outcome >> i) & 1) << k);
                        (table[y].0, table[y].1.as_slice())
                    })
                    .collect();
                self.combine(&locals, &chosen).max(0.0)
            })
            .collect();
        let total: f64 = probabilities.iter().sum();
        if total > 0.0 {
            probabilities.iter_mut().for_each(|p| *p /= total);
        }
        probabilities
    }
}

fn gates(program: &Program) -> Result<(Vec<Constant>, usize), String> {
    let mut program = program.clone();
    if program.gates().any(|gate| gate.wires().count() > 2) {
        transpile::transpile(&mut program, &GateSet::native().local());
    }
    let mut gates = Vec::new();
    for gate in program.gates() {
        let params: Option<Vec<f64>> = gate
            .params
            .iter()
            .map(|param| param.constant().map(Const::as_f64))
            .collect();
        let params = params.ok_or("cutting needs every angle known at compile time")?;
        gates.push((gate.clone(), params));
    }
    Ok((gates, program.num_qubits as usize))
}

pub(crate) fn expectation(program: &Program, terms: &[Weighted]) -> Result<f64, String> {
    let (gates, qubits) = gates(program)?;
    Ok(Cutting::fitted(&gates, qubits)?.expectation(terms))
}

pub(crate) fn samples(program: &Program, measured: &[QubitId]) -> bool {
    let mut distinct: Vec<QubitId> = measured.to_vec();
    distinct.sort();
    distinct.dedup();
    distinct.len() == measured.len()
        && measured.len() <= MAX_MEASURED
        && gates(program)
            .and_then(|(gates, qubits)| Cutting::fitted(&gates, qubits))
            .is_ok_and(|cutting| {
                (cutting.count as f64).powi(2) / 2.0
                    * cutting.pieces.len() as f64
                    * (1u64 << measured.len()) as f64
                    <= MAX_WORK
            })
}

pub(crate) struct Recorder {
    qubits: usize,
    gates: Vec<Constant>,
    measured: Vec<usize>,
}

impl Recorder {
    pub(crate) fn new(qubits: usize, measured: &[QubitId]) -> Recorder {
        Recorder {
            qubits,
            gates: Vec::new(),
            measured: measured.iter().map(|q| q.index()).collect(),
        }
    }
}

impl Backend for Recorder {
    fn gate(&mut self, gate: &Gate, params: &[f64]) {
        self.gates.push((gate.clone(), params.to_vec()));
    }

    fn pauli(&mut self, qubit: usize, x: bool, z: bool) {
        for (flip, kind) in [(z, GateKind::Z), (x, GateKind::X)] {
            if flip {
                self.gates.push((
                    Gate {
                        kind,
                        controls: Vec::new(),
                        targets: vec![QubitId(qubit as u32)],
                        params: Vec::new(),
                        span: Span::DUMMY,
                    },
                    Vec::new(),
                ));
            }
        }
    }
}

impl Sample for Recorder {
    type Sampler = Sampler;

    fn sampler(&self) -> Sampler {
        let probabilities = Cutting::fitted(&self.gates, self.qubits)
            .map(|cutting| cutting.distribution(&self.measured))
            .unwrap_or_default();
        Sampler::from_probabilities(&probabilities)
    }

    fn sample(
        &self,
        sampler: &Sampler,
        plan: &[(QubitId, ResultId)],
        rng: &mut Rng,
        results: &mut [bool],
    ) {
        let index = sampler.draw(rng);
        for (bit, (_, result)) in plan.iter().enumerate() {
            if let Some(slot) = results.get_mut(result.index()) {
                *slot = (index >> bit) & 1 == 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;
    use crate::simulator::exec;
    use crate::simulator::state::Rng;

    const LOCAL: [&str; 6] = ["h", "t", "sdg", "rx(0.3)", "ry(0.7)", "u3(0.2,0.9,1.4)"];
    const CROSSING: [&str; 8] = [
        "cx",
        "cz",
        "crz(0.4)",
        "swap",
        "ch",
        "cy",
        "cp(0.6)",
        "cu3(0.2,0.3,0.4)",
    ];

    fn pick<'a>(rng: &mut Rng, items: &[&'a str]) -> &'a str {
        items[rng.next_u64() as usize % items.len()]
    }

    #[test]
    fn recombines() {
        let mut rng = Rng::new(5);
        for round in 0..80 {
            let mut text = "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[6];\n".to_string();
            for step in 0..24 {
                let a = rng.next_u64() as usize % 3 + 3 * (step % 2);
                if step % 8 == 7 {
                    let b = rng.next_u64() as usize % 3 + 3 * (1 - step % 2);
                    text += &format!("{} q[{a}], q[{b}];\n", pick(&mut rng, &CROSSING));
                } else if step % 3 == 0 {
                    let b = (a + 1) % 3 + 3 * (step % 2);
                    text += &format!("cx q[{a}], q[{b}];\n");
                } else {
                    text += &format!("{} q[{a}];\n", pick(&mut rng, &LOCAL));
                }
            }
            match round % 4 {
                0 => text += "ccx q[0], q[4], q[2];\n",
                1 => text += "cswap q[5], q[1], q[3];\n",
                _ => {}
            }
            let (program, errors) = qasm::lower(&text);
            assert!(errors.is_empty(), "{errors:?}");
            let state = exec::finish(&program, State::new(6));
            let mut terms = Vec::new();
            let mut exact = 0.0;
            for weight in [1.0, -0.5] {
                let paulis: Vec<(usize, bool, bool)> = (0..6)
                    .filter_map(|q| match rng.next_u64() % 4 {
                        0 => None,
                        1 => Some((q, true, false)),
                        2 => Some((q, true, true)),
                        _ => Some((q, false, true)),
                    })
                    .collect();
                let (mut x, mut z) = (0, 0);
                for &(q, flips, phases) in &paulis {
                    x |= usize::from(flips) << q;
                    z |= usize::from(phases) << q;
                }
                exact += weight * state.pauli_element(&state, x, z).re;
                terms.push((weight, paulis));
            }
            let value = expectation(&program, &terms).unwrap();
            assert!(
                (value - exact).abs() < 1e-9,
                "{value} against {exact}\n{text}"
            );
        }
    }

    #[test]
    fn splits() {
        let pairs: Vec<(usize, usize)> = (0..39)
            .map(|q| (q, q + 1))
            .chain([(0, 39), (5, 25)])
            .collect();
        let side = split(40, &pairs, 10, 30).unwrap();
        let weight = {
            let mut weight = vec![vec![0; 40]; 40];
            for &(a, b) in &pairs {
                weight[a][b] += 1;
                weight[b][a] += 1;
            }
            weight
        };
        assert_eq!(crossing(&weight, &side), 2, "{side:?}");
        assert!(split(70, &pairs, 40, 30).is_none());
    }
    fn chained(rng: &mut Rng) -> String {
        let mut text = "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[9];
"
        .to_string();
        for step in 0..40 {
            let group = step % 3;
            let a = 3 * group + rng.next_u64() as usize % 3;
            if step % 13 == 12 {
                let b = (3 * (group + 1) + rng.next_u64() as usize % 3) % 9;
                text += &format!(
                    "{} q[{a}], q[{b}];
",
                    pick(rng, &CROSSING)
                );
            } else if step % 2 == 0 {
                text += &format!(
                    "cx q[{a}], q[{}];
",
                    3 * group + (a + 1) % 3
                );
            } else {
                text += &format!(
                    "{} q[{a}];
",
                    pick(rng, &LOCAL)
                );
            }
        }
        text
    }

    #[test]
    fn pieces() {
        let mut rng = Rng::new(11);
        for _ in 0..20 {
            let text = chained(&mut rng);
            let (program, errors) = qasm::lower(&text);
            assert!(errors.is_empty(), "{errors:?}");
            let state = exec::finish(&program, State::new(9));
            let (gates, qubits) = gates(&program).unwrap();
            let cutting = Cutting::plan(&gates, qubits, 3).unwrap();
            assert_eq!(cutting.pieces.len(), 3, "{text}");
            let paulis = vec![(0, false, true), (4, true, false), (8, true, true)];
            let exact = state.pauli_element(&state, 1 << 4 | 1 << 8, 1 | 1 << 8).re;
            let value = cutting.expectation(&[(1.0, paulis)]);
            assert!(
                (value - exact).abs() < 1e-9,
                "{value} {exact}
{text}"
            );
            let measured = [2, 5, 7];
            let mut expected = vec![0.0; 8];
            for (j, p) in state.probabilities().into_iter().enumerate() {
                let y = measured
                    .iter()
                    .enumerate()
                    .fold(0, |y, (k, &q)| y | ((j >> q) & 1) << k);
                expected[y] += p;
            }
            for (found, wanted) in cutting.distribution(&measured).iter().zip(&expected) {
                assert!(
                    (found - wanted).abs() < 1e-9,
                    "{found} {wanted}
{text}"
                );
            }
        }
    }

    #[test]
    fn samples_wide() {
        let mut rng = Rng::new(2);
        let mut forward = Vec::new();
        for layer in 0..2 {
            for q in 0..32 {
                forward.push(format!("ry({:.3}) q[{q}];", 3.0 * rng.next_unit()));
            }
            for q in (0..31).filter(|&q| q != 15) {
                forward.push(format!("cx q[{q}], q[{}];", q + 1));
            }
            if layer == 0 {
                forward.push("cx q[15], q[16];".into());
            }
        }
        let backward: Vec<String> = forward
            .iter()
            .rev()
            .map(|line| line.replace("ry(", "ry(-"))
            .collect();
        let text = format!(
            "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[32];
creg c[4];
{}
{}
h q[3];
measure q[0] -> c[0];
measure q[3] -> c[1];
measure q[16] -> c[2];
measure q[31] -> c[3];
",
            forward.join(
                "
"
            ),
            backward.join(
                "
"
            )
        );
        let (program, errors) = qasm::lower(&text);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(exec::kernel(&program), "circuit cutting");
        let config = exec::ExecConfig {
            shots: 2000,
            seed: 4,
            keep_state: false,
        };
        let counts = exec::execute(&program, config).counts;
        let keys: Vec<&String> = counts.keys().collect();
        assert_eq!(keys, ["0000", "0100"], "{counts:?}");
        assert!(
            counts.values().all(|&n| (800..1200).contains(&n)),
            "{counts:?}"
        );
    }
}
