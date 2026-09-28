use std::f64::consts::FRAC_PI_2;

use super::state::Rng;
use crate::ir::{Const, Gate, GateKind};

pub const MAX_QUBITS: usize = 5000;

#[derive(Clone)]
pub struct Tableau {
    qubits: usize,
    words: usize,
    x: Vec<u64>,
    z: Vec<u64>,
    r: Vec<bool>,
}

fn bit(words: &[u64], q: usize) -> bool {
    words[q / 64] >> (q % 64) & 1 == 1
}

fn toggle(words: &mut [u64], q: usize) {
    words[q / 64] ^= 1u64 << (q % 64);
}

pub(crate) fn quarter_turns(angle: f64) -> Option<usize> {
    let turns = angle / FRAC_PI_2;
    ((turns - turns.round()).abs() < 1e-9).then(|| (turns.round() as i64).rem_euclid(4) as usize)
}

pub(crate) fn clifford(gate: &Gate) -> Option<Vec<f64>> {
    let params = gate
        .params
        .iter()
        .map(|p| p.constant().map(Const::as_f64))
        .collect::<Option<Vec<f64>>>()?;
    supports(gate, &params).then_some(params)
}

pub fn supports(gate: &Gate, params: &[f64]) -> bool {
    let turn = || params.first().copied().and_then(quarter_turns).is_some();
    match (gate.kind, gate.controls.len()) {
        (
            GateKind::I
            | GateKind::X
            | GateKind::Y
            | GateKind::Z
            | GateKind::H
            | GateKind::S
            | GateKind::SDag
            | GateKind::SX
            | GateKind::SXDag
            | GateKind::Swap,
            0,
        ) => true,
        (GateKind::Rz | GateKind::R1 | GateKind::Rx | GateKind::Ry, 0) => turn(),
        (GateKind::X | GateKind::Y | GateKind::Z, 1) => true,
        _ => false,
    }
}

impl Tableau {
    pub fn new(qubits: usize) -> Tableau {
        let words = qubits.div_ceil(64).max(1);
        let rows = 2 * qubits + 1;
        let mut tableau = Tableau {
            qubits,
            words,
            x: vec![0; rows * words],
            z: vec![0; rows * words],
            r: vec![false; rows],
        };
        for q in 0..qubits {
            toggle(tableau.row_x(q), q);
            toggle(tableau.row_z(qubits + q), q);
        }
        tableau
    }

    fn row_x(&mut self, row: usize) -> &mut [u64] {
        &mut self.x[row * self.words..(row + 1) * self.words]
    }

    fn row_z(&mut self, row: usize) -> &mut [u64] {
        &mut self.z[row * self.words..(row + 1) * self.words]
    }

    fn rows(&self) -> usize {
        2 * self.qubits
    }

    fn get(&self, row: usize, q: usize) -> (bool, bool) {
        let at = row * self.words;
        (bit(&self.x[at..], q), bit(&self.z[at..], q))
    }

    pub fn h(&mut self, a: usize) {
        for row in 0..self.rows() {
            let (x, z) = self.get(row, a);
            self.r[row] ^= x & z;
            if x != z {
                toggle(self.row_x(row), a);
                toggle(self.row_z(row), a);
            }
        }
    }

    pub fn s(&mut self, a: usize) {
        for row in 0..self.rows() {
            let (x, z) = self.get(row, a);
            self.r[row] ^= x & z;
            if x {
                toggle(self.row_z(row), a);
            }
        }
    }

    pub fn cx(&mut self, a: usize, b: usize) {
        for row in 0..self.rows() {
            let (xa, za) = self.get(row, a);
            let (xb, zb) = self.get(row, b);
            self.r[row] ^= xa & zb & !(xb ^ za);
            if xa {
                toggle(self.row_x(row), b);
            }
            if zb {
                toggle(self.row_z(row), a);
            }
        }
    }

    pub(crate) fn pauli(&mut self, a: usize, flip_x: bool, flip_z: bool) {
        for row in 0..self.rows() {
            let (x, z) = self.get(row, a);
            self.r[row] ^= (flip_x & z) ^ (flip_z & x);
        }
    }

    fn turns(&mut self, a: usize, count: usize) {
        for _ in 0..count {
            self.s(a);
        }
    }

    pub fn apply(&mut self, gate: &Gate, params: &[f64]) {
        let target = gate.targets[0].index();
        let turns = params.first().copied().and_then(quarter_turns).unwrap_or(0);
        match (gate.kind, gate.controls.first().map(|c| c.index())) {
            (GateKind::X, None) => self.pauli(target, true, false),
            (GateKind::Y, None) => self.pauli(target, true, true),
            (GateKind::Z, None) => self.pauli(target, false, true),
            (GateKind::H, None) => self.h(target),
            (GateKind::S, None) => self.s(target),
            (GateKind::SDag, None) => self.turns(target, 3),
            (GateKind::SX, None) => {
                self.h(target);
                self.s(target);
                self.h(target);
            }
            (GateKind::SXDag, None) => {
                self.h(target);
                self.turns(target, 3);
                self.h(target);
            }
            (GateKind::Rz | GateKind::R1, None) => self.turns(target, turns),
            (GateKind::Rx, None) => {
                self.h(target);
                self.turns(target, turns);
                self.h(target);
            }
            (GateKind::Ry, None) => {
                self.turns(target, 3);
                self.h(target);
                self.turns(target, turns);
                self.h(target);
                self.s(target);
            }
            (GateKind::Swap, None) => {
                let other = gate.targets[1].index();
                self.cx(target, other);
                self.cx(other, target);
                self.cx(target, other);
            }
            (GateKind::X, Some(control)) => self.cx(control, target),
            (GateKind::Z, Some(control)) => {
                self.h(target);
                self.cx(control, target);
                self.h(target);
            }
            (GateKind::Y, Some(control)) => {
                self.turns(target, 3);
                self.cx(control, target);
                self.s(target);
            }
            _ => {}
        }
    }

    fn rowsum(&mut self, h: usize, i: usize) {
        let words = self.words;
        let mut sum = 2 * (i64::from(self.r[h]) + i64::from(self.r[i]));
        for w in 0..words {
            let (x1, z1) = (self.x[i * words + w], self.z[i * words + w]);
            let (x2, z2) = (self.x[h * words + w], self.z[h * words + w]);
            let plus = (x1 & z1 & z2 & !x2) | (x1 & !z1 & z2 & x2) | (!x1 & z1 & x2 & !z2);
            let minus = (x1 & z1 & x2 & !z2) | (x1 & !z1 & z2 & !x2) | (!x1 & z1 & x2 & z2);
            sum += i64::from(plus.count_ones()) - i64::from(minus.count_ones());
            self.x[h * words + w] = x2 ^ x1;
            self.z[h * words + w] = z2 ^ z1;
        }
        self.r[h] = sum.rem_euclid(4) == 2;
    }

    fn copy_row(&mut self, from: usize, to: usize) {
        let words = self.words;
        self.x
            .copy_within(from * words..(from + 1) * words, to * words);
        self.z
            .copy_within(from * words..(from + 1) * words, to * words);
        self.r[to] = self.r[from];
    }

    fn swap_rows(&mut self, a: usize, b: usize) {
        let scratch = 2 * self.qubits;
        self.copy_row(a, scratch);
        self.copy_row(b, a);
        self.copy_row(scratch, b);
    }

    pub fn canonical(&self) -> (Vec<u64>, Vec<u64>, Vec<bool>) {
        let n = self.qubits;
        let mut t = self.clone();
        let mut next = n;
        for column in 0..2 * n {
            let bit = |t: &Tableau, row: usize| {
                let (x, z) = t.get(row, column % n);
                if column < n { x } else { z }
            };
            let Some(found) = (next..2 * n).find(|&row| bit(&t, row)) else {
                continue;
            };
            t.swap_rows(found, next);
            for row in n..2 * n {
                if row != next && bit(&t, row) {
                    t.rowsum(row, next);
                }
            }
            next += 1;
        }
        let words = t.words;
        (
            t.x[n * words..2 * n * words].to_vec(),
            t.z[n * words..2 * n * words].to_vec(),
            t.r[n..2 * n].to_vec(),
        )
    }

    fn clear_row(&mut self, row: usize) {
        self.row_x(row).fill(0);
        self.row_z(row).fill(0);
        self.r[row] = false;
    }

    pub fn measure(&mut self, a: usize, rng: &mut Rng) -> bool {
        self.measure_with(a, || rng.next_u64() & 1 == 1)
    }

    pub fn random(&self, a: usize) -> bool {
        (self.qubits..2 * self.qubits).any(|p| self.get(p, a).0)
    }

    pub fn measure_with(&mut self, a: usize, pick: impl FnOnce() -> bool) -> bool {
        let n = self.qubits;
        match (n..2 * n).find(|&p| self.get(p, a).0) {
            Some(p) => {
                for i in 0..2 * n {
                    if i != p && self.get(i, a).0 {
                        self.rowsum(i, p);
                    }
                }
                self.copy_row(p, p - n);
                self.clear_row(p);
                toggle(self.row_z(p), a);
                let outcome = pick();
                self.r[p] = outcome;
                outcome
            }
            None => {
                let scratch = 2 * n;
                self.clear_row(scratch);
                for i in 0..n {
                    if self.get(i, a).0 {
                        self.rowsum(scratch, i + n);
                    }
                }
                self.r[scratch]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::ir::QubitId;
    use crate::simulator::exec::Backend;
    use crate::simulator::state::State;

    #[test]
    fn ghz() {
        let n = 300;
        let mut rng = Rng::new(9);
        for _ in 0..20 {
            let mut t = Tableau::new(n);
            t.h(0);
            for q in 1..n {
                t.cx(q - 1, q);
            }
            let first = t.measure(0, &mut rng);
            assert!((1..n).all(|q| t.measure(q, &mut rng) == first));
        }
    }

    #[test]
    fn matches_state_vector() {
        let n = 5;
        let mut rng = Rng::new(17);
        let kinds = [
            (GateKind::H, 0),
            (GateKind::S, 0),
            (GateKind::SDag, 0),
            (GateKind::X, 0),
            (GateKind::Y, 0),
            (GateKind::SX, 0),
            (GateKind::X, 1),
            (GateKind::Z, 1),
            (GateKind::Y, 1),
            (GateKind::Swap, 0),
            (GateKind::Ry, 0),
        ];
        for _ in 0..300 {
            let mut state = State::new(n);
            let mut tableau = Tableau::new(n);
            for _ in 0..30 {
                let (kind, controls) = kinds[rng.next_u64() as usize % kinds.len()];
                let a = rng.next_u64() as usize % n;
                let b = (a + 1 + rng.next_u64() as usize % (n - 1)) % n;
                let gate = Gate {
                    kind,
                    controls: if controls == 1 {
                        vec![QubitId(b as u32)]
                    } else {
                        Vec::new()
                    },
                    targets: if kind == GateKind::Swap {
                        vec![QubitId(a as u32), QubitId(b as u32)]
                    } else {
                        vec![QubitId(a as u32)]
                    },
                    params: Vec::new(),
                    span: Span::DUMMY,
                };
                let params = if kind == GateKind::Ry {
                    vec![FRAC_PI_2 * 3.0]
                } else {
                    Vec::new()
                };
                state.gate(&gate, &params);
                tableau.gate(&gate, &params);
            }
            for q in 0..n {
                let p = state.qubit_probability(q);
                let before = tableau.clone();
                let outcome = tableau.measure(q, &mut rng);
                let certain = (p - 0.5).abs() > 0.25;
                if certain {
                    assert_eq!(outcome, p > 0.5);
                } else {
                    assert!((p - 0.5).abs() < 1e-9, "{p}");
                    let forced = (0..64).any(|_| before.clone().measure(q, &mut rng) != outcome);
                    assert!(forced);
                }
                state.collapse(q, outcome);
            }
        }
    }

    #[test]
    fn canonical() {
        let mut a = Tableau::new(3);
        a.h(0);
        a.cx(0, 1);
        a.cx(1, 2);
        let mut b = Tableau::new(3);
        b.h(1);
        b.cx(1, 2);
        b.cx(1, 0);
        assert!(a.canonical() == b.canonical());
        b.pauli(2, false, true);
        assert!(a.canonical() != b.canonical());
        let mut c = a.clone();
        for q in 0..3 {
            c.pauli(q, true, false);
        }
        assert!(a.canonical() == c.canonical());
        c.pauli(2, false, true);
        assert!(a.canonical() != c.canonical());
    }

    #[test]
    fn phases() {
        let mut rng = Rng::new(3);
        let mut t = Tableau::new(2);
        t.h(0);
        t.s(0);
        t.s(0);
        t.h(0);
        assert!(t.measure(0, &mut rng));
        t.pauli(1, true, false);
        t.h(1);
        t.pauli(1, false, true);
        t.h(1);
        assert!(!t.measure(1, &mut rng));
    }
}
