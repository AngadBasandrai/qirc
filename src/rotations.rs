use std::f64::consts::{FRAC_PI_4, FRAC_PI_8};
use std::fmt;
use std::mem;

use crate::ir::*;
use crate::resources;

const CLOSE: f64 = 1e-12;

#[derive(Clone, PartialEq, Debug)]
struct Pauli {
    x: Vec<u64>,
    z: Vec<u64>,
    phase: u8,
}

impl Pauli {
    fn single(words: usize, q: usize, x: bool, z: bool) -> Pauli {
        let mut pauli = Pauli {
            x: vec![0; words],
            z: vec![0; words],
            phase: u8::from(x && z),
        };
        let mask = 1u64 << (q % 64);
        if x {
            pauli.x[q / 64] |= mask;
        }
        if z {
            pauli.z[q / 64] |= mask;
        }
        pauli
    }

    fn times(&self, other: &Pauli) -> Pauli {
        let crossed: u32 = self
            .z
            .iter()
            .zip(&other.x)
            .map(|(z, x)| (z & x).count_ones())
            .sum();
        Pauli {
            x: self.x.iter().zip(&other.x).map(|(a, b)| a ^ b).collect(),
            z: self.z.iter().zip(&other.z).map(|(a, b)| a ^ b).collect(),
            phase: ((self.phase as u32 + other.phase as u32 + 2 * crossed) % 4) as u8,
        }
    }

    fn scaled(mut self, quarter: u8) -> Pauli {
        self.phase = (self.phase + quarter) % 4;
        self
    }

    fn anticommutes(&self, other: &Pauli) -> bool {
        let count: u32 = (0..self.x.len())
            .map(|w| (self.x[w] & other.z[w]).count_ones() + (self.z[w] & other.x[w]).count_ones())
            .sum();
        count % 2 == 1
    }

    fn negative(&self) -> bool {
        let ys: u32 = self
            .x
            .iter()
            .zip(&self.z)
            .map(|(x, z)| (x & z).count_ones())
            .sum();
        (self.phase as u32 + 4 - ys % 4) % 4 == 2
    }

    fn same(&self, other: &Pauli) -> bool {
        self.x == other.x && self.z == other.z
    }

    fn sign(&self) -> char {
        if self.negative() { '-' } else { '+' }
    }
}

impl fmt::Display for Pauli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut letters = Vec::new();
        for q in 0..64 * self.x.len() {
            let (x, z) = (
                self.x[q / 64] >> (q % 64) & 1,
                self.z[q / 64] >> (q % 64) & 1,
            );
            match (x, z) {
                (1, 0) => letters.push(format!("X{q}")),
                (1, 1) => letters.push(format!("Y{q}")),
                (0, 1) => letters.push(format!("Z{q}")),
                _ => {}
            }
        }
        write!(f, "{}", letters.join(" "))
    }
}

#[derive(Clone, Debug)]
enum Step {
    Rotate { pauli: Pauli, angle: f64 },
    Measure { pauli: Pauli, result: u32 },
}

impl Step {
    fn pauli(&self) -> &Pauli {
        match self {
            Step::Rotate { pauli, .. } | Step::Measure { pauli, .. } => pauli,
        }
    }

    fn eighth(&self) -> bool {
        matches!(self, Step::Rotate { angle, .. } if (angle.abs() - FRAC_PI_8).abs() < CLOSE)
    }
}

pub struct Rotations {
    name: String,
    qubits: usize,
    steps: Vec<Step>,
    layers: Vec<usize>,
    pub t_depth: usize,
    t: usize,
    saved: usize,
}

struct Frame {
    x: Vec<Pauli>,
    z: Vec<Pauli>,
    used: Vec<bool>,
    steps: Vec<Step>,
    merged: usize,
}

impl Frame {
    fn new(qubits: usize) -> Frame {
        let words = qubits.div_ceil(64).max(1);
        Frame {
            x: (0..qubits)
                .map(|q| Pauli::single(words, q, true, false))
                .collect(),
            z: (0..qubits)
                .map(|q| Pauli::single(words, q, false, true))
                .collect(),
            used: vec![false; qubits],
            steps: Vec::new(),
            merged: 0,
        }
    }

    fn y(&self, q: usize) -> Pauli {
        self.x[q].times(&self.z[q]).scaled(1)
    }

    fn absorb(&mut self, pauli: &Pauli, quarter: i64) {
        let quarter = quarter.rem_euclid(4) as u8;
        if quarter == 0 {
            return;
        }
        for image in self.x.iter_mut().chain(self.z.iter_mut()) {
            if image.anticommutes(pauli) {
                let turned = if quarter % 2 == 1 {
                    pauli.times(image)
                } else {
                    image.clone()
                };
                *image = turned.scaled(quarter);
            }
        }
    }

    fn rotate(&mut self, pauli: Pauli, angle: f64) {
        let (pauli, angle) = if pauli.negative() {
            (pauli.scaled(2), -angle)
        } else {
            (pauli, angle)
        };
        let mut found = None;
        for (i, step) in self.steps.iter().enumerate().rev() {
            match step {
                Step::Rotate {
                    pauli: other,
                    angle: earlier,
                } if other.same(&pauli) => {
                    found = Some((i, *earlier));
                    break;
                }
                step if step.pauli().anticommutes(&pauli) => break,
                _ => {}
            }
        }
        let total = found.map_or(angle, |(_, earlier)| earlier + angle);
        let quarters = (total / FRAC_PI_4).round();
        if (total - quarters * FRAC_PI_4).abs() < CLOSE {
            if let Some((i, _)) = found {
                self.merged += 1;
                self.steps.remove(i);
            }
            self.absorb(&pauli, quarters as i64);
            return;
        }
        match found {
            Some((i, _)) => {
                self.merged += 1;
                self.steps[i] = Step::Rotate {
                    pauli,
                    angle: total,
                };
            }
            None => self.steps.push(Step::Rotate { pauli, angle }),
        }
    }

    fn gate(&mut self, gate: &Gate) -> Result<(), String> {
        let wires: Vec<usize> = gate.wires().map(|q| q.0 as usize).collect();
        for &q in &wires {
            self.used[q] = true;
        }
        let angle = || {
            gate.constant_angle()
                .ok_or_else(|| format!("the angle of {} is not a constant", gate.kind.name()))
        };
        match (gate.kind, wires.as_slice()) {
            (GateKind::I, _) => {}
            (GateKind::H, &[q]) => mem::swap(&mut self.x[q], &mut self.z[q]),
            (GateKind::S, &[q]) => self.x[q] = self.x[q].times(&self.z[q]).scaled(3),
            (GateKind::SDag, &[q]) => self.x[q] = self.x[q].times(&self.z[q]).scaled(1),
            (GateKind::X, &[q]) => self.z[q] = self.z[q].clone().scaled(2),
            (GateKind::Z, &[q]) => self.x[q] = self.x[q].clone().scaled(2),
            (GateKind::Y, &[q]) => {
                self.x[q] = self.x[q].clone().scaled(2);
                self.z[q] = self.z[q].clone().scaled(2);
            }
            (GateKind::X, &[c, t]) => {
                self.z[t] = self.z[c].times(&self.z[t]);
                self.x[c] = self.x[c].times(&self.x[t]);
            }
            (GateKind::Z, &[a, b]) => {
                let (xa, xb) = (self.x[a].times(&self.z[b]), self.z[a].times(&self.x[b]));
                (self.x[a], self.x[b]) = (xa, xb);
            }
            (GateKind::Swap, &[a, b]) => {
                self.x.swap(a, b);
                self.z.swap(a, b);
            }
            (GateKind::T, &[q]) => self.rotate(self.z[q].clone(), FRAC_PI_8),
            (GateKind::TDag, &[q]) => self.rotate(self.z[q].clone(), -FRAC_PI_8),
            (GateKind::Rz | GateKind::R1, &[q]) => self.rotate(self.z[q].clone(), angle()? / 2.0),
            (GateKind::Rx, &[q]) => self.rotate(self.x[q].clone(), angle()? / 2.0),
            (GateKind::Ry, &[q]) => self.rotate(self.y(q), angle()? / 2.0),
            _ => {
                return Err(format!(
                    "{} on {} qubits is not a Clifford+T gate or a rotation",
                    gate.kind.name(),
                    wires.len()
                ));
            }
        }
        Ok(())
    }

    fn op(&mut self, op: &Op) -> Result<(), String> {
        match op {
            Op::Gate(gate) => self.gate(gate)?,
            Op::Measure { qubit, result, .. } => {
                let q = qubit.0 as usize;
                self.used[q] = true;
                self.steps.push(Step::Measure {
                    pauli: self.z[q].clone(),
                    result: result.0,
                });
            }
            Op::Reset { qubit, .. } if self.used[qubit.0 as usize] => {
                return Err(format!(
                    "q{} is reset after it is used, which needs a correction that depends on a measurement",
                    qubit.0
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

fn layered(steps: &[Step], weight: impl Fn(&Step) -> usize) -> Vec<usize> {
    let mut layers: Vec<usize> = Vec::with_capacity(steps.len());
    for (j, step) in steps.iter().enumerate() {
        let before = steps[..j]
            .iter()
            .zip(&layers)
            .filter(|(earlier, _)| earlier.pauli().anticommutes(step.pauli()))
            .map(|(_, &layer)| layer)
            .max()
            .unwrap_or(0);
        layers.push(before + weight(step));
    }
    layers
}

pub(crate) fn build(lowered: &Program) -> Result<Rotations, String> {
    let qubits = lowered.num_qubits as usize;
    let mut frame = Frame::new(qubits);
    let mut block = lowered.block(lowered.entry);
    for _ in 0..lowered.blocks.len() {
        for op in &block.ops {
            frame.op(op)?;
        }
        match &block.term {
            Term::Br(next) => block = lowered.block(*next),
            Term::Ret(_) | Term::Unreachable => break,
            Term::CondBr { .. } | Term::Switch { .. } => {
                return Err("Pauli product rotations need a program without branches".into());
            }
        }
    }
    let steps = frame.steps;
    let t = steps.iter().filter(|step| step.eighth()).count();
    let t_depth = layered(&steps, |step| usize::from(step.eighth()))
        .into_iter()
        .max()
        .unwrap_or(0);
    Ok(Rotations {
        name: lowered.name.clone(),
        qubits,
        layers: layered(&steps, |_| 1),
        steps,
        t_depth,
        t,
        saved: frame.merged,
    })
}

pub fn compile(program: &Program) -> Result<Rotations, String> {
    build(&resources::lowered(program)?)
}

impl fmt::Display for Rotations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        writeln!(
            f,
            "Pauli product rotations for {}, {} qubit{}\n",
            self.name,
            self.qubits,
            plural(self.qubits)
        )?;
        writeln!(f, "layer  step")?;
        let mut order: Vec<usize> = (0..self.steps.len()).collect();
        order.sort_by_key(|&i| self.layers[i]);
        for i in order {
            let layer = self.layers[i];
            let step = &self.steps[i];
            match step {
                Step::Rotate { pauli, angle } if step.eighth() => {
                    let sign = if *angle < 0.0 { '-' } else { '+' };
                    writeln!(f, "{layer:>5}  {sign}pi/8     {pauli}")?;
                }
                Step::Rotate { pauli, angle } => writeln!(f, "{layer:>5}  {angle:<+10.6}{pauli}")?,
                Step::Measure { pauli, result } => writeln!(
                    f,
                    "{layer:>5}  measure   {}{pauli} -> r{result}",
                    pauli.sign()
                )?,
            }
        }
        let other = self
            .steps
            .iter()
            .filter(|step| matches!(step, Step::Rotate { .. }) && !step.eighth())
            .count();
        let measured = self.steps.len() - self.t - other;
        let depth = self.layers.iter().max().copied().unwrap_or(0);
        writeln!(
            f,
            "\n{} pi/8 rotation{} in T depth {}, {other} other rotation{}, {measured} measurement{}, {} layer{} in all",
            self.t,
            plural(self.t),
            self.t_depth,
            plural(other),
            plural(measured),
            depth,
            plural(depth)
        )?;
        if self.saved > 0 {
            writeln!(
                f,
                "{} rotation{} merged with an earlier one about the same Pauli product",
                self.saved,
                plural(self.saved)
            )?;
        }
        writeln!(
            f,
            "a rotation by a about P applies exp(-i a P), and the Clifford gates are folded into the rotations and measurements"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;
    use crate::simulator::matrix::{C64, matrix_for};
    use crate::simulator::state::{Rng, State};

    fn compiled(body: &str) -> Rotations {
        let (program, errors) = qasm::lower(&format!(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[3];\ncreg c[3];\n{body}"
        ));
        assert!(errors.is_empty(), "{errors:?}");
        compile(&program).unwrap()
    }

    fn paulis(rotations: &Rotations) -> Vec<String> {
        rotations
            .steps
            .iter()
            .map(|step| step.pauli())
            .map(|pauli| format!("{}{pauli}", pauli.sign()))
            .collect()
    }

    #[test]
    fn folds() {
        let entangled = compiled("t q[0];\ncx q[0], q[1];\nt q[1];\nmeasure q[1] -> c[1];\n");
        assert_eq!(paulis(&entangled), ["+Z0", "+Z0 Z1", "+Z0 Z1"]);
        assert_eq!((entangled.t, entangled.t_depth), (2, 1));
        let turned = compiled("t q[0];\nh q[0];\nt q[0];\n");
        assert_eq!((turned.t, turned.t_depth), (2, 2));
        let doubled = compiled("t q[0];\nt q[0];\nh q[0];\nmeasure q[0] -> c[0];\n");
        assert_eq!((doubled.t, doubled.saved), (0, 1));
        assert_eq!(paulis(&doubled), ["-Y0"]);
        let undone = compiled("t q[0];\ncx q[1], q[2];\ntdg q[0];\n");
        assert!(undone.steps.is_empty(), "{:?}", undone.steps);
        let text = entangled.to_string();
        assert!(text.contains("2 pi/8 rotations in T depth 1"), "{text}");
    }

    fn apply(state: &[C64], pauli: &Pauli) -> Vec<C64> {
        let (x, z) = (pauli.x[0] as usize, pauli.z[0] as usize);
        let phase = C64::i().powu(pauli.phase as u32);
        let mut out = vec![C64::new(0.0, 0.0); state.len()];
        for (b, amplitude) in state.iter().enumerate() {
            let sign = if (z & b).count_ones() % 2 == 1 {
                -1.0
            } else {
                1.0
            };
            out[b ^ x] = phase * sign * amplitude;
        }
        out
    }

    #[test]
    fn matches_circuits() {
        let gates = [
            "h", "s", "sdg", "t", "tdg", "x", "cx", "cz", "rz(0.3)", "ry(0.7)",
        ];
        let n = 5;
        let mut rng = Rng::new(23);
        for round in 0..30 {
            let mut body = String::new();
            for _ in 0..40 {
                let gate = gates[rng.next_u64() as usize % gates.len()];
                let a = rng.next_u64() as usize % n;
                if gate.starts_with('c') {
                    let b = (a + 1 + rng.next_u64() as usize % (n - 1)) % n;
                    body += &format!("{gate} q[{a}], q[{b}];\n");
                } else {
                    body += &format!("{gate} q[{a}];\n");
                }
            }
            let (program, errors) = qasm::lower(&format!(
                "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{n}];\ncreg c[{n}];\n{body}measure q -> c;\n"
            ));
            assert!(errors.is_empty(), "{errors:?}");
            let lowered = resources::lowered(&program).unwrap();
            let mut state = State::new(n);
            for op in &lowered.block(lowered.entry).ops {
                if let Op::Gate(gate) = op {
                    let controls = gate.controls.iter().fold(0, |m, q| m | 1u64 << q.0);
                    let matrix = matrix_for(gate.kind, &gate.constant_params());
                    state.apply(&matrix, gate.targets[0].0 as usize, controls);
                }
            }
            let rotations = build(&lowered).unwrap();
            let mut psi = vec![C64::new(0.0, 0.0); 1 << n];
            psi[0] = C64::new(1.0, 0.0);
            let mut measured = Vec::new();
            for step in &rotations.steps {
                match step {
                    Step::Rotate { pauli, angle } => {
                        let turned = apply(&psi, pauli);
                        for (a, b) in psi.iter_mut().zip(turned) {
                            *a = *a * angle.cos() - C64::i() * angle.sin() * b;
                        }
                    }
                    Step::Measure { pauli, result } => measured.push((*result, pauli.clone())),
                }
            }
            for outcome in 0..1 << n {
                let mut projected = psi.clone();
                for (result, pauli) in &measured {
                    let sign = if outcome >> result & 1 == 1 {
                        -1.0
                    } else {
                        1.0
                    };
                    let flipped = apply(&projected, pauli);
                    for (a, b) in projected.iter_mut().zip(flipped) {
                        *a = (*a + sign * b) * 0.5;
                    }
                }
                let chance: f64 = psi
                    .iter()
                    .zip(&projected)
                    .map(|(a, b)| (a.conj() * b).re)
                    .sum();
                let expected = state.amplitude(outcome).norm_sqr();
                assert!(
                    (chance - expected).abs() < 1e-9,
                    "{round} {outcome} {chance} {expected}\n{body}"
                );
            }
        }
    }
}
