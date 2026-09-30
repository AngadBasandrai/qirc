use std::collections::{BTreeMap, HashMap};
use std::f64::consts::PI;
use std::hash::Hash;

use crate::ir::{Gate, GateKind, Op, Program};
use crate::phase;
use crate::route::Coupling;

const MAX_MITIGATED: usize = 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    pub qubits: usize,
    cx: HashMap<(usize, usize), f64>,
    single: HashMap<usize, f64>,
    readout: HashMap<usize, f64>,
    cx_time: HashMap<(usize, usize), f64>,
    single_time: HashMap<usize, f64>,
    readout_time: HashMap<usize, f64>,
    t1: HashMap<usize, f64>,
    t2: HashMap<usize, f64>,
    frequency: HashMap<usize, f64>,
    drive: HashMap<usize, (f64, f64)>,
    cross: HashMap<(usize, usize), f64>,
    resonator: HashMap<usize, (f64, f64)>,
    detuning: HashMap<usize, f64>,
    pub decouple: bool,
}

fn lookup<K: Eq + Hash>(map: &HashMap<K, f64>, key: K) -> f64 {
    map.get(&key)
        .copied()
        .unwrap_or_else(|| map.values().sum::<f64>() / map.len().max(1) as f64)
}

pub(crate) struct Idle {
    pub(crate) qubit: usize,
    pub(crate) paulis: [f64; 3],
    pub(crate) phase: f64,
    pub(crate) echo: bool,
}

#[derive(Default)]
pub(crate) struct Clock {
    free: Vec<Option<f64>>,
    start: f64,
}

impl Clock {
    pub(crate) fn begin(&mut self, calibration: &Calibration, op: &Op) -> Vec<Idle> {
        let qubits: Vec<usize> = op.qubits().iter().map(|q| q.index()).collect();
        if let Some(&top) = qubits.iter().max()
            && top >= self.free.len()
        {
            self.free.resize(top + 1, None);
        }
        self.start = qubits
            .iter()
            .filter_map(|&q| self.free[q])
            .fold(0.0, f64::max);
        qubits
            .iter()
            .filter_map(|&q| {
                self.free[q].map(|last| {
                    let gap = self.start - last;
                    Idle {
                        qubit: q,
                        paulis: calibration.idle(q, gap),
                        phase: calibration.drift(q, gap),
                        echo: calibration.echoes(q, gap),
                    }
                })
            })
            .collect()
    }

    pub(crate) fn end(&mut self, calibration: &Calibration, op: &Op) {
        let end = self.start + calibration.duration(op);
        for q in op.qubits() {
            self.free[q.index()] = Some(end);
        }
    }
}

impl Calibration {
    pub fn parse(text: &str) -> Result<Calibration, String> {
        let mut calibration = Calibration::default();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let words: Vec<&str> = line.split_whitespace().collect();
            if words.first().is_none_or(|w| w.starts_with('#')) {
                continue;
            }
            let shape = || {
                format!(
                    "line {number}: expected `cx a b error`, `single q error`, `readout q error`, `time cx a b ns`, `time single q ns`, `time readout q ns`, `t1 q us`, `t2 q us`, `frequency q GHz`, `drive q amplitude beta`, `cross a b amplitude`, `resonator q GHz [amplitude]` or `detuning q kHz`"
                )
            };
            let length = |word: &str| match word.parse::<f64>() {
                Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
                _ => Err(format!(
                    "line {number}: `{word}` must be a time of at least 0"
                )),
            };
            let qubit = |word: &str| word.parse::<usize>().map_err(|_| shape());
            let number_in = |word: &str, low: f64, high: f64| match word.parse::<f64>() {
                Ok(v) if v.is_finite() && v > low && v <= high => Ok(v),
                _ => Err(format!(
                    "line {number}: `{word}` must be above {low} and at most {high}"
                )),
            };
            let rate = |word: &str| match word.parse::<f64>() {
                Ok(e) if (0.0..1.0).contains(&e) => Ok(e),
                _ => Err(format!(
                    "line {number}: error rate `{word}` must be at least 0 and below 1"
                )),
            };
            let highest = match words[..] {
                ["cx", a, b, e] => {
                    let (a, b) = (qubit(a)?, qubit(b)?);
                    if a == b {
                        return Err(format!(
                            "line {number}: a cx edge needs two different qubits"
                        ));
                    }
                    calibration.cx.insert((a.min(b), a.max(b)), rate(e)?);
                    a.max(b)
                }
                ["single", q, e] => {
                    let q = qubit(q)?;
                    calibration.single.insert(q, rate(e)?);
                    q
                }
                ["readout", q, e] => {
                    let q = qubit(q)?;
                    calibration.readout.insert(q, rate(e)?);
                    q
                }
                ["time", "cx", a, b, t] => {
                    let (a, b) = (qubit(a)?, qubit(b)?);
                    calibration.cx_time.insert((a.min(b), a.max(b)), length(t)?);
                    a.max(b)
                }
                ["time", "single", q, t] => {
                    let q = qubit(q)?;
                    calibration.single_time.insert(q, length(t)?);
                    q
                }
                ["time", "readout", q, t] => {
                    let q = qubit(q)?;
                    calibration.readout_time.insert(q, length(t)?);
                    q
                }
                ["t1", q, t] => {
                    let q = qubit(q)?;
                    calibration.t1.insert(q, length(t)? * 1000.0);
                    q
                }
                ["t2", q, t] => {
                    let q = qubit(q)?;
                    calibration.t2.insert(q, length(t)? * 1000.0);
                    q
                }
                ["frequency", q, ghz] => {
                    let q = qubit(q)?;
                    calibration
                        .frequency
                        .insert(q, number_in(ghz, 0.0, 1000.0)?);
                    q
                }
                ["drive", q, amplitude, beta] => {
                    let q = qubit(q)?;
                    let beta = number_in(beta, -100.0, 100.0)?;
                    calibration
                        .drive
                        .insert(q, (number_in(amplitude, 0.0, 1.0)?, beta));
                    q
                }
                ["resonator", q, ghz] | ["resonator", q, ghz, _] => {
                    let q = qubit(q)?;
                    let amplitude = match words.get(3) {
                        Some(word) => number_in(word, 0.0, 1.0)?,
                        None => 0.1,
                    };
                    calibration
                        .resonator
                        .insert(q, (number_in(ghz, 0.0, 1000.0)?, amplitude));
                    q
                }
                ["detuning", q, khz] => {
                    let q = qubit(q)?;
                    let value = khz
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && v.abs() <= 1e6)
                        .ok_or_else(|| {
                            format!("line {number}: detuning `{khz}` must be a number of kHz")
                        })?;
                    calibration.detuning.insert(q, value);
                    q
                }
                ["cross", a, b, amplitude] => {
                    let (a, b) = (qubit(a)?, qubit(b)?);
                    calibration
                        .cross
                        .insert((a, b), number_in(amplitude, 0.0, 1.0)?);
                    a.max(b)
                }
                _ => return Err(shape()),
            };
            calibration.qubits = calibration.qubits.max(highest + 1);
        }
        if calibration.cx.is_empty() {
            return Err("the calibration lists no cx edges".into());
        }
        Ok(calibration)
    }

    pub fn scaled(&self, factor: f64) -> Calibration {
        let rate = |e: &f64| (e * factor).min(0.99);
        let mut scaled = self.clone();
        scaled.cx.values_mut().for_each(|e| *e = rate(e));
        scaled.single.values_mut().for_each(|e| *e = rate(e));
        scaled.readout.values_mut().for_each(|e| *e = rate(e));
        scaled.t1.values_mut().for_each(|t| *t /= factor);
        scaled.t2.values_mut().for_each(|t| *t /= factor);
        scaled.detuning.values_mut().for_each(|d| *d *= factor);
        scaled
    }

    pub fn coupling(&self) -> Coupling {
        let mut edges: Vec<(usize, usize)> = self.cx.keys().copied().collect();
        edges.sort_unstable();
        Coupling {
            qubits: self.qubits,
            edges,
        }
    }

    pub fn cx(&self, a: usize, b: usize) -> f64 {
        lookup(&self.cx, (a.min(b), a.max(b)))
    }

    pub fn surface(&self) -> (Option<f64>, Option<f64>) {
        let mean = |values: Vec<f64>| {
            (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
        };
        let error = mean(self.cx.values().copied().collect())
            .or_else(|| mean(self.single.values().copied().collect()));
        let cycle = mean(self.cx_time.values().copied().collect())
            .zip(mean(self.readout_time.values().copied().collect()))
            .map(|(cx, readout)| 4.0 * cx + 2.0 * readout);
        (error, cycle)
    }

    pub fn coherent(&self) -> bool {
        !self.detuning.is_empty()
    }

    pub fn drift(&self, q: usize, ns: f64) -> f64 {
        self.detuning
            .get(&q)
            .map_or(0.0, |khz| 2.0 * PI * khz * ns * 1e-6)
    }

    pub fn pulse(&self, q: usize) -> f64 {
        lookup(&self.single_time, q)
    }

    pub fn echoes(&self, q: usize, ns: f64) -> bool {
        self.decouple && ns >= 2.0 * self.pulse(q) + 1e-9
    }

    pub fn timed(&self) -> bool {
        !self.cx_time.is_empty() || !self.single_time.is_empty() || !self.readout_time.is_empty()
    }

    fn cx_time(&self, a: usize, b: usize) -> f64 {
        lookup(&self.cx_time, (a.min(b), a.max(b)))
    }

    pub fn frequency(&self, q: usize) -> Option<f64> {
        self.frequency.get(&q).copied()
    }

    pub fn drive(&self, q: usize) -> (f64, f64) {
        self.drive.get(&q).copied().unwrap_or((0.2, 0.0))
    }

    pub fn resonator(&self, q: usize) -> Option<(f64, f64)> {
        self.resonator.get(&q).copied()
    }

    pub fn cross(&self, control: usize, target: usize) -> f64 {
        self.cross.get(&(control, target)).copied().unwrap_or(0.1)
    }

    pub fn coupled(&self, a: usize, b: usize) -> bool {
        self.cx.contains_key(&(a.min(b), a.max(b)))
    }

    pub fn timeline<'a>(&self, ops: impl IntoIterator<Item = &'a Op>) -> Vec<(f64, f64)> {
        let mut clock = Clock::default();
        ops.into_iter()
            .map(|op| {
                clock.begin(self, op);
                let start = clock.start;
                clock.end(self, op);
                (start, start + self.duration(op))
            })
            .collect()
    }

    pub fn duration(&self, op: &Op) -> f64 {
        match op {
            Op::Gate(gate) => {
                let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
                match wires[..] {
                    [_] if phase::diagonal(gate.kind) => 0.0,
                    [q] => lookup(&self.single_time, q),
                    [a, b] if gate.kind == GateKind::Swap => 3.0 * self.cx_time(a, b),
                    [a, b] => self.cx_time(a, b),
                    _ => {
                        let mut total = 0.0;
                        for (i, &a) in wires.iter().enumerate() {
                            for &b in &wires[i + 1..] {
                                total += 2.0 * self.cx_time(a, b);
                            }
                        }
                        total
                    }
                }
            }
            Op::Measure { qubit, .. } | Op::Reset { qubit, .. } => {
                lookup(&self.readout_time, qubit.index())
            }
            _ => 0.0,
        }
    }

    pub fn idle(&self, q: usize, ns: f64) -> [f64; 3] {
        let decay = |time: Option<&f64>| time.map_or(0.0, |t| 1.0 - (-ns / t).exp());
        let damping = decay(self.t1.get(&q));
        let dephasing = decay(self.t2.get(&q));
        let flip = damping / 4.0;
        [flip, flip, (dephasing / 2.0 - flip).max(0.0)]
    }

    pub fn schedule<'a>(&self, ops: impl IntoIterator<Item = &'a Op>) -> (f64, f64) {
        let mut clock = Clock::default();
        let mut loss = 0.0;
        for op in ops {
            for idle in clock.begin(self, op) {
                let coherent = if idle.echo {
                    2.0 * lookup(&self.single, idle.qubit)
                } else {
                    (idle.phase / 2.0).sin().powi(2)
                };
                loss += -(-(idle.paulis.iter().sum::<f64>() + coherent).min(0.999_999)).ln_1p();
            }
            clock.end(self, op);
        }
        (clock.free.into_iter().flatten().fold(0.0, f64::max), loss)
    }

    pub fn mitigate(
        &self,
        program: &Program,
        counts: &BTreeMap<String, u64>,
    ) -> Option<Vec<(String, f64)>> {
        let bits = program.num_results as usize;
        if bits == 0 || bits > MAX_MITIGATED {
            return None;
        }
        let mut flips = vec![0.0; bits];
        for op in program.ops() {
            if let Op::Measure { qubit, result, .. } = op
                && let Some(flip) = flips.get_mut(result.index())
            {
                *flip = self.readout(qubit.index());
            }
        }
        let total: u64 = counts.values().sum();
        let mut dense = vec![0.0; 1 << bits];
        for (key, &count) in counts {
            let index = key
                .chars()
                .take(bits)
                .enumerate()
                .filter(|&(_, c)| c == '1')
                .fold(0usize, |index, (bit, _)| index | 1 << bit);
            dense[index] += count as f64 / total.max(1) as f64;
        }
        for (bit, &p) in flips.iter().enumerate() {
            if p <= 0.0 || p >= 0.5 {
                continue;
            }
            let scale = 1.0 / (1.0 - 2.0 * p);
            for low in (0..dense.len()).filter(|j| j >> bit & 1 == 0) {
                let high = low | 1 << bit;
                let (a, b) = (dense[low], dense[high]);
                dense[low] = scale * ((1.0 - p) * a - p * b);
                dense[high] = scale * ((1.0 - p) * b - p * a);
            }
        }
        dense.iter_mut().for_each(|v| *v = v.max(0.0));
        let sum: f64 = dense.iter().sum();
        let mut out: Vec<(String, f64)> = dense
            .iter()
            .enumerate()
            .filter(|&(_, &v)| sum > 0.0 && v / sum > 1e-9)
            .map(|(index, &v)| {
                let key = (0..bits)
                    .map(|bit| if index >> bit & 1 == 1 { '1' } else { '0' })
                    .collect();
                (key, v / sum)
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Some(out)
    }

    pub fn readout(&self, q: usize) -> f64 {
        self.readout.get(&q).copied().unwrap_or(0.0)
    }

    pub fn gate_error(&self, gate: &Gate) -> f64 {
        let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
        match wires[..] {
            [q] => self.single.get(&q).copied().unwrap_or(0.0),
            [a, b] if gate.kind == GateKind::Swap => 1.0 - (1.0 - self.cx(a, b)).powi(3),
            [a, b] => self.cx(a, b),
            _ => {
                let mut kept = 1.0;
                for (i, &a) in wires.iter().enumerate() {
                    for &b in &wires[i + 1..] {
                        kept *= (1.0 - self.cx(a, b)).powi(2);
                    }
                }
                1.0 - kept
            }
        }
    }

    pub fn infidelity<'a>(&self, ops: impl IntoIterator<Item = &'a Op>) -> f64 {
        let loss = |e: f64| -(-e).ln_1p();
        ops.into_iter()
            .map(|op| match op {
                Op::Gate(gate) => loss(self.gate_error(gate)),
                Op::Measure { qubit, .. } => loss(self.readout(qubit.index())),
                _ => 0.0,
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let text = "# two qubits\ncx 0 1 0.01\nsingle 1 0.001\nreadout 0 0.02\n\ncx 2 1 0.03\n";
        let calibration = Calibration::parse(text).unwrap();
        assert_eq!(calibration.qubits, 3);
        assert_eq!(calibration.coupling().edges, [(0, 1), (1, 2)]);
        assert_eq!(calibration.cx(1, 0), 0.01);
        assert!((calibration.cx(0, 2) - 0.02).abs() < 1e-12);
    }

    #[test]
    fn rejects() {
        for (text, message) in [
            ("cx 0 1 1.5", "below 1"),
            ("cx 0 0 0.1", "two different"),
            ("readout 0", "expected"),
            ("single 0 0.1", "no cx"),
        ] {
            let error = Calibration::parse(text).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
    }
}
