use std::collections::HashMap;

use crate::ir::{Gate, GateKind, Op};
use crate::route::Coupling;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    pub qubits: usize,
    cx: HashMap<(usize, usize), f64>,
    single: HashMap<usize, f64>,
    readout: HashMap<usize, f64>,
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
                    "line {number}: expected `cx a b error`, `single q error` or `readout q error`"
                )
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
                _ => return Err(shape()),
            };
            calibration.qubits = calibration.qubits.max(highest + 1);
        }
        if calibration.cx.is_empty() {
            return Err("the calibration lists no cx edges".into());
        }
        Ok(calibration)
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
