use std::collections::BTreeMap;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, SQRT_2};

use super::matrix::C64;
use super::state::Rng;
use crate::ir::{Gate, GateKind};

pub const MAX_ROTATIONS: usize = 10;

fn get(bits: &[u64], j: usize) -> bool {
    bits[j / 64] >> (j % 64) & 1 == 1
}

fn set(bits: &mut [u64], j: usize, value: bool) {
    let mask = 1u64 << (j % 64);
    if value {
        bits[j / 64] |= mask;
    } else {
        bits[j / 64] &= !mask;
    }
}

fn ones(bits: &[u64]) -> impl Iterator<Item = usize> + '_ {
    bits.iter().enumerate().flat_map(|(w, &word)| {
        (0..64)
            .filter(move |b| word >> b & 1 == 1)
            .map(move |b| w * 64 + b)
    })
}

fn power(k: u32) -> C64 {
    [
        C64::new(1.0, 0.0),
        C64::new(0.0, 1.0),
        C64::new(-1.0, 0.0),
        C64::new(0.0, -1.0),
    ][(k % 4) as usize]
}

#[derive(Clone)]
struct Ch {
    words: usize,
    f: Vec<u64>,
    g: Vec<u64>,
    m: Vec<u64>,
    gamma: Vec<u32>,
    v: Vec<u64>,
    s: Vec<u64>,
    omega: C64,
}

impl Ch {
    fn new(qubits: usize) -> Ch {
        let words = qubits.div_ceil(64).max(1);
        let mut identity = vec![0; qubits * words];
        for q in 0..qubits {
            set(&mut identity[q * words..(q + 1) * words], q, true);
        }
        Ch {
            words,
            f: identity.clone(),
            g: identity,
            m: vec![0; qubits * words],
            gamma: vec![0; qubits],
            v: vec![0; words],
            s: vec![0; words],
            omega: C64::new(1.0, 0.0),
        }
    }

    fn row(data: &[u64], words: usize, p: usize) -> &[u64] {
        &data[p * words..(p + 1) * words]
    }

    fn rows(&self) -> usize {
        self.gamma.len()
    }

    fn s(&mut self, q: usize) {
        let w = self.words;
        for k in 0..w {
            self.m[q * w + k] ^= self.g[q * w + k];
        }
        self.gamma[q] = (self.gamma[q] + 3) % 4;
    }

    fn cx(&mut self, q: usize, r: usize) {
        let w = self.words;
        let sign: u32 = (0..w)
            .map(|k| (self.m[q * w + k] & self.f[r * w + k]).count_ones())
            .sum();
        self.gamma[q] = (self.gamma[q] + self.gamma[r] + 2 * (sign & 1)) % 4;
        for k in 0..w {
            self.g[r * w + k] ^= self.g[q * w + k];
            self.f[q * w + k] ^= self.f[r * w + k];
            self.m[q * w + k] ^= self.m[r * w + k];
        }
    }

    fn x_image(&self, q: usize) -> (Vec<u64>, C64) {
        let w = self.words;
        let (f, m) = (Ch::row(&self.f, w, q), Ch::row(&self.m, w, q));
        let mut t = self.s.clone();
        let mut minus = 0;
        for k in 0..w {
            let (fk, mk, vk, sk) = (f[k], m[k], self.v[k], self.s[k]);
            t[k] = sk ^ (fk & !vk) ^ (mk & vk);
            minus += (mk & !vk & sk).count_ones() + (fk & vk & (sk ^ mk)).count_ones();
        }
        (t, power(self.gamma[q] + 2 * (minus & 1)))
    }

    fn z_image(&self, q: usize) -> (Vec<u64>, C64) {
        let g = Ch::row(&self.g, self.words, q);
        let mut u = self.s.clone();
        let mut minus = 0;
        for k in 0..self.words {
            minus += (g[k] & !self.v[k] & self.s[k]).count_ones();
            u[k] ^= g[k] & self.v[k];
        }
        (u, power(2 * (minus & 1)))
    }

    fn x(&mut self, q: usize) {
        let (t, phase) = self.x_image(q);
        self.s = t;
        self.omega *= phase;
    }

    fn z(&mut self, q: usize) {
        let (u, phase) = self.z_image(q);
        self.s = u;
        self.omega *= phase;
    }

    fn right_s(&mut self, r: usize) {
        let w = self.words;
        for p in 0..self.rows() {
            if get(&self.f[p * w..(p + 1) * w], r) {
                let row = &mut self.m[p * w..(p + 1) * w];
                set(row, r, !get(row, r));
                self.gamma[p] = (self.gamma[p] + 3) % 4;
            }
        }
    }

    fn right_cz(&mut self, r: usize, k: usize) {
        let w = self.words;
        for p in 0..self.rows() {
            let f = &self.f[p * w..(p + 1) * w];
            let (fr, fk) = (get(f, r), get(f, k));
            let row = &mut self.m[p * w..(p + 1) * w];
            if fr {
                set(row, k, !get(row, k));
            }
            if fk {
                set(row, r, !get(row, r));
            }
            if fr && fk {
                self.gamma[p] = (self.gamma[p] + 2) % 4;
            }
        }
    }

    fn right_cx(&mut self, c: usize, t: usize) {
        let w = self.words;
        for p in 0..self.rows() {
            let row = &mut self.f[p * w..(p + 1) * w];
            let fc = get(row, c);
            set(row, t, get(row, t) ^ fc);
            let row = &mut self.m[p * w..(p + 1) * w];
            let mt = get(row, t);
            set(row, c, get(row, c) ^ mt);
            let row = &mut self.g[p * w..(p + 1) * w];
            let gt = get(row, t);
            set(row, c, get(row, c) ^ gt);
        }
    }

    fn h(&mut self, q: usize) {
        let (t, a) = self.x_image(q);
        let (u, b) = self.z_image(q);
        if t == u {
            self.s = t;
            self.omega *= (a + b) * FRAC_1_SQRT_2;
            return;
        }
        let ratio = b / a;
        let delta = (0..4u32)
            .find(|&k| (power(k) - ratio).norm() < 1e-9)
            .unwrap_or(0);
        let factor = self.combine(t, u, delta);
        self.omega *= a * FRAC_1_SQRT_2 * factor;
    }

    fn combine(&mut self, t: Vec<u64>, u: Vec<u64>, delta: u32) -> C64 {
        let words = self.words;
        let d: Vec<u64> = t.iter().zip(&u).map(|(x, y)| x ^ y).collect();
        let hidden: Vec<u64> = d.iter().zip(&self.v).map(|(x, v)| x & !v).collect();
        let plain = hidden.iter().any(|&x| x != 0);
        let q = ones(if plain { &hidden } else { &d }).next().unwrap_or(0);
        let (t, delta, mut factor) = if get(&t, q) {
            (u, (4 - delta) % 4, power(delta))
        } else {
            (t, delta, C64::new(1.0, 0.0))
        };
        self.s = t;
        if plain {
            for j in ones(&hidden).filter(|&j| j != q).collect::<Vec<_>>() {
                self.right_cx(q, j);
            }
            let shown: Vec<u64> = (0..words).map(|k| d[k] & self.v[k]).collect();
            for j in ones(&shown).collect::<Vec<_>>() {
                self.right_cz(q, j);
            }
            set(&mut self.v, q, true);
            set(&mut self.s, q, delta >= 2);
            if delta % 2 == 1 {
                self.right_s(q);
            }
            factor *= SQRT_2;
        } else {
            for j in ones(&d).filter(|&j| j != q).collect::<Vec<_>>() {
                self.right_cx(j, q);
            }
            let (hadamard, bit, scale) = match delta {
                0 => (false, false, C64::new(SQRT_2, 0.0)),
                2 => (false, true, C64::new(SQRT_2, 0.0)),
                1 => (true, true, C64::new(1.0, 1.0)),
                _ => (true, false, C64::new(1.0, -1.0)),
            };
            set(&mut self.v, q, hadamard);
            set(&mut self.s, q, bit);
            if hadamard {
                self.right_s(q);
            }
            factor *= scale;
        }
        factor
    }

    fn amplitude(&self, x: &[u64]) -> C64 {
        let w = self.words;
        let mut g = 0u32;
        let mut f = vec![0u64; w];
        let mut m = vec![0u64; w];
        for p in ones(x).filter(|&p| p < self.rows()) {
            let (fp, mp) = (Ch::row(&self.f, w, p), Ch::row(&self.m, w, p));
            let sign: u32 = (0..w).map(|k| (m[k] & fp[k]).count_ones()).sum();
            g += self.gamma[p] + 2 * (sign & 1);
            for k in 0..w {
                f[k] ^= fp[k];
                m[k] ^= mp[k];
            }
        }
        if (0..w).any(|k| (f[k] ^ self.s[k]) & !self.v[k] != 0) {
            return C64::new(0.0, 0.0);
        }
        let minus: u32 = (0..w)
            .map(|k| (f[k] & self.s[k] & self.v[k]).count_ones())
            .sum();
        let hadamards: u32 = self.v.iter().map(|x| x.count_ones()).sum();
        self.omega
            * power(4 - g % 4)
            * power(2 * (minus & 1))
            * FRAC_1_SQRT_2.powi(hadamards as i32)
    }
}

fn quarter(angle: f64) -> Option<u32> {
    let turns = angle / FRAC_PI_2;
    ((turns - turns.round()).abs() < 1e-9).then(|| (turns.round() as i64).rem_euclid(4) as u32)
}

fn angle(gate: &Gate) -> Option<f64> {
    gate.constant_angle()
}

pub fn supports(gate: &Gate) -> bool {
    match (gate.kind, gate.controls.len(), gate.targets.len()) {
        (GateKind::X, 0 | 1, 1) | (GateKind::SX, 0, 1) => true,
        (GateKind::Rz, 0, 1) => angle(gate).is_some(),
        _ => false,
    }
}

pub fn rotation(gate: &Gate) -> bool {
    gate.kind == GateKind::Rz && angle(gate).is_some_and(|a| quarter(a).is_none())
}

pub struct Rank {
    terms: Vec<(C64, Ch)>,
    samples: Vec<Vec<u64>>,
    rng: Rng,
}

impl Rank {
    pub fn new(qubits: usize, shots: usize, seed: u64) -> Rank {
        let words = qubits.div_ceil(64).max(1);
        Rank {
            terms: vec![(C64::new(1.0, 0.0), Ch::new(qubits))],
            samples: vec![vec![0; words]; shots],
            rng: Rng::new(seed),
        }
    }

    fn amplitude(&self, x: &[u64]) -> C64 {
        self.terms.iter().map(|(c, ch)| c * ch.amplitude(x)).sum()
    }

    fn resample(&mut self, q: usize) {
        let mut groups: BTreeMap<Vec<u64>, Vec<usize>> = BTreeMap::new();
        for (index, sample) in self.samples.iter().enumerate() {
            let mut key = sample.clone();
            set(&mut key, q, false);
            groups.entry(key).or_default().push(index);
        }
        for (mut x, members) in groups {
            let zero = self.amplitude(&x).norm_sqr();
            set(&mut x, q, true);
            let one = self.amplitude(&x).norm_sqr();
            for index in members {
                let outcome = self.rng.next_unit() * (zero + one) < one;
                set(&mut self.samples[index], q, outcome);
            }
        }
    }

    fn each(&mut self, action: impl Fn(&mut Ch)) {
        for (_, ch) in &mut self.terms {
            action(ch);
        }
    }

    fn rotate(&mut self, q: usize, theta: f64) {
        let phase = C64::from_polar(1.0, -theta / 2.0);
        if let Some(k) = quarter(theta) {
            for (c, ch) in &mut self.terms {
                for _ in 0..k {
                    ch.s(q);
                }
                *c *= phase;
            }
            return;
        }
        let (sin, cos) = (theta / 2.0).sin_cos();
        let mut next = Vec::with_capacity(2 * self.terms.len());
        for (c, ch) in self.terms.drain(..) {
            let mut flipped = ch.clone();
            flipped.z(q);
            next.push((c * cos, ch));
            next.push((c * C64::new(0.0, -sin), flipped));
        }
        self.terms = next;
    }

    pub fn apply(&mut self, gate: &Gate, params: &[f64]) {
        match (gate.kind, &gate.controls[..], &gate.targets[..]) {
            (GateKind::X, [], [q]) => self.pauli(q.index(), true, false),
            (GateKind::X, [c], [t]) => {
                let (c, t) = (c.index(), t.index());
                self.each(|ch| ch.cx(c, t));
                for x in &mut self.samples {
                    let flipped = get(x, t) ^ get(x, c);
                    set(x, t, flipped);
                }
            }
            (GateKind::SX, [], [q]) => {
                let q = q.index();
                self.each(|ch| {
                    ch.h(q);
                    ch.s(q);
                    ch.h(q);
                });
                self.resample(q);
            }
            (GateKind::Rz, [], [q]) => {
                if let Some(&theta) = params.first() {
                    self.rotate(q.index(), theta);
                }
            }
            _ => {}
        }
    }

    pub fn pauli(&mut self, q: usize, x: bool, z: bool) {
        if z {
            self.each(|ch| ch.z(q));
        }
        if x {
            self.each(|ch| ch.x(q));
            for sample in &mut self.samples {
                let flipped = !get(sample, q);
                set(sample, q, flipped);
            }
        }
    }

    pub fn shot(&self, index: usize, qubit: usize) -> bool {
        self.samples
            .get(index)
            .is_some_and(|sample| get(sample, qubit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::ir::QubitId;
    use crate::simulator::matrix::Matrix2;
    use crate::simulator::state::State;

    fn basis(index: usize, words: usize) -> Vec<u64> {
        let mut x = vec![0; words];
        x[0] = index as u64;
        x
    }

    #[test]
    fn clifford_amplitudes() {
        let n = 6;
        let mut rng = Rng::new(7);
        for _ in 0..200 {
            let mut ch = Ch::new(n);
            let mut state = State::new(n);
            for _ in 0..40 {
                let a = rng.next_u64() as usize % n;
                let b = (a + 1 + rng.next_u64() as usize % (n - 1)) % n;
                match rng.next_u64() % 7 {
                    0 => {
                        ch.h(a);
                        state.apply(&Matrix2::h(), a, 0);
                    }
                    1 => {
                        ch.s(a);
                        state.apply(&Matrix2::s(), a, 0);
                    }
                    2 => {
                        ch.cx(a, b);
                        state.apply(&Matrix2::x(), b, 1 << a);
                    }
                    4 => {
                        ch.x(a);
                        state.apply(&Matrix2::x(), a, 0);
                    }
                    5 => {
                        ch.z(a);
                        state.apply(&Matrix2::z(), a, 0);
                    }
                    _ => {
                        ch.h(a);
                        ch.s(a);
                        ch.h(a);
                        state.apply(&Matrix2::sx(), a, 0);
                    }
                }
            }
            for index in 0..1 << n {
                let difference = (ch.amplitude(&basis(index, 1)) - state.amplitude(index)).norm();
                assert!(difference < 1e-9, "{index}");
            }
        }
    }

    #[test]
    fn rotated_amplitudes() {
        let n = 5;
        let mut rng = Rng::new(3);
        for _ in 0..40 {
            let mut rank = Rank::new(n, 0, 1);
            let mut state = State::new(n);
            let mut rotations = 0;
            for _ in 0..30 {
                let a = rng.next_u64() as usize % n;
                let b = (a + 1 + rng.next_u64() as usize % (n - 1)) % n;
                let gate = |kind: GateKind, controls: Vec<usize>, targets: Vec<usize>| Gate {
                    kind,
                    controls: controls.into_iter().map(|q| QubitId(q as u32)).collect(),
                    targets: targets.into_iter().map(|q| QubitId(q as u32)).collect(),
                    params: Vec::new(),
                    span: Span::DUMMY,
                };
                match rng.next_u64() % 4 {
                    0 => {
                        rank.apply(&gate(GateKind::SX, vec![], vec![a]), &[]);
                        state.apply(&Matrix2::sx(), a, 0);
                    }
                    1 => {
                        rank.apply(&gate(GateKind::X, vec![a], vec![b]), &[]);
                        state.apply(&Matrix2::x(), b, 1 << a);
                    }
                    2 if rotations < 5 => {
                        rotations += 1;
                        let theta = 0.3 + rng.next_unit();
                        rank.apply(&gate(GateKind::Rz, vec![], vec![a]), &[theta]);
                        state.apply(&Matrix2::rz(theta), a, 0);
                    }
                    _ => {
                        let theta = FRAC_PI_2 * (rng.next_u64() % 4) as f64;
                        rank.apply(&gate(GateKind::Rz, vec![], vec![a]), &[theta]);
                        state.apply(&Matrix2::rz(theta), a, 0);
                    }
                }
            }
            for index in 0..1 << n {
                let difference = (rank.amplitude(&basis(index, 1)) - state.amplitude(index)).norm();
                assert!(difference < 1e-9, "{index}");
            }
        }
    }
}
