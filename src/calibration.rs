use std::collections::{BTreeMap, HashMap};

use crate::ir::{Gate, GateKind, Op, Program};

const MAX_MITIGATED: usize = 20;
use crate::route::Coupling;

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
}

fn average<K>(map: &HashMap<K, f64>) -> f64 {
    map.values().sum::<f64>() / map.len().max(1) as f64
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
                    "line {number}: expected `cx a b error`, `single q error`, `readout q error`, `time cx a b ns`, `time single q ns`, `time readout q ns`, `t1 q us` or `t2 q us`"
                )
            };
            let length = |word: &str| match word.parse::<f64>() {
                Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
                _ => Err(format!(
                    "line {number}: `{word}` must be a time of at least 0"
                )),
            };
            let qubit = |word: &str| word.parse::<usize>().map_err(|_| shape());
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
        self.cx
            .get(&(a.min(b), a.max(b)))
            .copied()
            .unwrap_or_else(|| self.cx.values().sum::<f64>() / self.cx.len().max(1) as f64)
    }

    pub fn timed(&self) -> bool {
        !self.cx_time.is_empty() || !self.single_time.is_empty() || !self.readout_time.is_empty()
    }

    fn cx_time(&self, a: usize, b: usize) -> f64 {
        self.cx_time
            .get(&(a.min(b), a.max(b)))
            .copied()
            .unwrap_or_else(|| average(&self.cx_time))
    }

    pub fn duration(&self, op: &Op) -> f64 {
        match op {
            Op::Gate(gate) => {
                let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
                match wires[..] {
                    [q] => self
                        .single_time
                        .get(&q)
                        .copied()
                        .unwrap_or_else(|| average(&self.single_time)),
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
            Op::Measure { qubit, .. } | Op::Reset { qubit, .. } => self
                .readout_time
                .get(&qubit.index())
                .copied()
                .unwrap_or_else(|| average(&self.readout_time)),
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
        let mut clock: HashMap<usize, f64> = HashMap::new();
        let mut loss = 0.0;
        for op in ops {
            let qubits: Vec<usize> = op.qubits().iter().map(|q| q.index()).collect();
            if qubits.is_empty() {
                continue;
            }
            let start = qubits
                .iter()
                .map(|q| clock.get(q).copied().unwrap_or(0.0))
                .fold(0.0, f64::max);
            for q in &qubits {
                if let Some(&last) = clock.get(q) {
                    let error: f64 = self.idle(*q, start - last).iter().sum();
                    loss += -(-error).ln_1p();
                }
            }
            let end = start + self.duration(op);
            for q in qubits {
                clock.insert(q, end);
            }
        }
        (clock.into_values().fold(0.0, f64::max), loss)
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
