use super::matrix::{C64, Matrix2, matrix_for};
use super::state::Rng;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::iter;

use crate::ir::{Gate, GateKind, Program};

pub const DEFAULT_BOND: usize = 32;
const NEGLIGIBLE: f64 = 1e-14;
const SWEEPS: usize = 60;

type Square = [[C64; 4]; 4];

#[derive(Clone)]
struct Site {
    left: usize,
    right: usize,
    data: Vec<C64>,
}

impl Site {
    fn at(&self, l: usize, s: usize, r: usize) -> C64 {
        self.data[(l * 2 + s) * self.right + r]
    }
}

#[derive(Clone)]
pub struct Mps {
    sites: Vec<Site>,
    site_of: Vec<usize>,
    qubit_at: Vec<usize>,
    center: usize,
    bond: usize,
    pub discarded: f64,
}

type Interactions = Vec<HashMap<usize, usize>>;

fn spread(weight: &Interactions, position: &[usize]) -> usize {
    let mut total = 0usize;
    for (a, row) in weight.iter().enumerate() {
        for (&b, &w) in row.iter().filter(|(b, _)| **b > a) {
            total = total.saturating_add(w.saturating_mul(position[a].abs_diff(position[b])));
        }
    }
    total
}

fn degree(weight: &Interactions, q: usize) -> usize {
    weight[q].len()
}

fn cuthill_mckee(weight: &Interactions, start: usize) -> Vec<usize> {
    let n = weight.len();
    let mut placed = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut queue = VecDeque::new();
    let mut seeds = iter::once(start).chain(0..n);
    while order.len() < n {
        if queue.is_empty() {
            let Some(seed) = seeds.find(|&q| !placed[q]) else {
                break;
            };
            placed[seed] = true;
            queue.push_back(seed);
        }
        while let Some(q) = queue.pop_front() {
            order.push(q);
            let mut next: Vec<usize> = weight[q].keys().copied().filter(|&r| !placed[r]).collect();
            next.sort_by_key(|&r| (degree(weight, r), r));
            for r in next {
                placed[r] = true;
                queue.push_back(r);
            }
        }
    }
    order.reverse();
    let mut position = vec![0; n];
    for (at, &q) in order.iter().enumerate() {
        position[q] = at;
    }
    position
}

pub(crate) fn arrangement(program: &Program) -> Option<Vec<usize>> {
    let n = program.num_qubits as usize;
    let mut weight: Interactions = (0..n).map(|_| HashMap::new()).collect();
    for gate in program.gates() {
        let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
        if let [a, b] = wires[..] {
            if a >= n || b >= n || a == b {
                return None;
            }
            let ab = weight[a].entry(b).or_default();
            *ab = ab.saturating_add(1);
            let ba = weight[b].entry(a).or_default();
            *ba = ba.saturating_add(1);
        }
    }
    let identity: Vec<usize> = (0..n).collect();
    let current = spread(&weight, &identity);
    let mut starts: Vec<usize> = (0..n).collect();
    starts.sort_by_key(|&q| (degree(&weight, q), q));
    starts.truncate(8);
    starts
        .into_iter()
        .map(|start| cuthill_mckee(&weight, start))
        .map(|position| (spread(&weight, &position), position))
        .filter(|(cost, _)| *cost < current)
        .min_by_key(|(cost, _)| *cost)
        .map(|(_, position)| position)
}

pub fn supports(gate: &Gate) -> bool {
    match (gate.kind, gate.controls.len(), gate.targets.len()) {
        (GateKind::Swap, 0, 2) => true,
        (GateKind::Swap, _, _) => false,
        (_, controls, 1) => controls <= 1,
        _ => false,
    }
}

struct Svd {
    u: Vec<C64>,
    s: Vec<f64>,
    vt: Vec<C64>,
}

fn svd(a: &[C64], rows: usize, cols: usize) -> Svd {
    if rows < cols {
        let adjoint: Vec<C64> = (0..cols * rows)
            .map(|i| a[(i % rows) * cols + i / rows].conj())
            .collect();
        let Svd { u, s, vt } = svd(&adjoint, cols, rows);
        let k = s.len();
        let u2 = (0..rows * k)
            .map(|i| vt[(i % k) * rows + i / k].conj())
            .collect();
        let vt2 = (0..k * cols)
            .map(|i| u[(i % cols) * k + i / cols].conj())
            .collect();
        return Svd { u: u2, s, vt: vt2 };
    }
    let mut u = a.to_vec();
    let mut v = vec![C64::new(0.0, 0.0); cols * cols];
    for j in 0..cols {
        v[j * cols + j] = C64::new(1.0, 0.0);
    }
    for _ in 0..SWEEPS {
        let mut rotated = false;
        for p in 0..cols {
            for q in p + 1..cols {
                let (mut alpha, mut beta, mut gamma) = (0.0, 0.0, C64::new(0.0, 0.0));
                for k in 0..rows {
                    let (x, y) = (u[k * cols + p], u[k * cols + q]);
                    alpha += x.norm_sqr();
                    beta += y.norm_sqr();
                    gamma += x.conj() * y;
                }
                let size = gamma.norm();
                if size <= 1e-15 * (alpha * beta).sqrt() || size == 0.0 {
                    continue;
                }
                rotated = true;
                let phase = (gamma / size).conj();
                let zeta = (beta - alpha) / (2.0 * size);
                let t = zeta.signum() / (zeta.abs() + (1.0 + zeta * zeta).sqrt());
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = c * t;
                for (matrix, height) in [(&mut u, rows), (&mut v, cols)] {
                    for k in 0..height {
                        let x = matrix[k * cols + p];
                        let y = matrix[k * cols + q] * phase;
                        matrix[k * cols + p] = x * c - y * s;
                        matrix[k * cols + q] = x * s + y * c;
                    }
                }
            }
        }
        if !rotated {
            break;
        }
    }
    let norms: Vec<f64> = (0..cols)
        .map(|j| {
            (0..rows)
                .map(|k| u[k * cols + j].norm_sqr())
                .sum::<f64>()
                .sqrt()
        })
        .collect();
    let mut order: Vec<usize> = (0..cols).collect();
    order.sort_by(|&x, &y| norms[y].total_cmp(&norms[x]));
    let mut left = vec![C64::new(0.0, 0.0); rows * cols];
    let mut right = vec![C64::new(0.0, 0.0); cols * cols];
    let mut values = Vec::with_capacity(cols);
    for (new, &old) in order.iter().enumerate() {
        let sigma = norms[old];
        values.push(sigma);
        for k in 0..rows {
            left[k * cols + new] = if sigma > 0.0 {
                u[k * cols + old] / sigma
            } else {
                C64::new(0.0, 0.0)
            };
        }
        for k in 0..cols {
            right[new * cols + k] = v[k * cols + old].conj();
        }
    }
    Svd {
        u: left,
        s: values,
        vt: right,
    }
}

fn controlled(m: &Matrix2) -> Square {
    let zero = C64::new(0.0, 0.0);
    let one = C64::new(1.0, 0.0);
    [
        [one, zero, zero, zero],
        [zero, one, zero, zero],
        [zero, zero, m.a, m.b],
        [zero, zero, m.c, m.d],
    ]
}

fn swapped(m: &Square) -> Square {
    let flip = |i: usize| (i >> 1) | ((i & 1) << 1);
    let mut out = *m;
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = m[flip(i)][flip(j)];
        }
    }
    out
}

fn swap_matrix() -> Square {
    let zero = C64::new(0.0, 0.0);
    let one = C64::new(1.0, 0.0);
    [
        [one, zero, zero, zero],
        [zero, zero, one, zero],
        [zero, one, zero, zero],
        [zero, zero, zero, one],
    ]
}

impl Mps {
    pub fn new(qubits: usize, bond: usize) -> Mps {
        let site = Site {
            left: 1,
            right: 1,
            data: vec![C64::new(1.0, 0.0), C64::new(0.0, 0.0)],
        };
        Mps {
            sites: vec![site; qubits],
            site_of: (0..qubits).collect(),
            qubit_at: (0..qubits).collect(),
            center: 0,
            bond: bond.max(1),
            discarded: 0.0,
        }
    }

    pub fn qubits(&self) -> usize {
        self.sites.len()
    }

    pub fn one(&mut self, qubit: usize, m: &Matrix2) {
        let site = &mut self.sites[self.site_of[qubit]];
        for l in 0..site.left {
            for r in 0..site.right {
                let zero = (l * 2) * site.right + r;
                let one = (l * 2 + 1) * site.right + r;
                let (x, y) = (site.data[zero], site.data[one]);
                site.data[zero] = m.a * x + m.b * y;
                site.data[one] = m.c * x + m.d * y;
            }
        }
    }

    fn step_right(&mut self) {
        let k = self.center;
        let site = &self.sites[k];
        let (left, right) = (site.left, site.right);
        let Svd { u, s, vt } = svd(&site.data, left * 2, right);
        let keep = s
            .iter()
            .take_while(|&&x| x > NEGLIGIBLE * s[0].max(NEGLIGIBLE))
            .count()
            .max(1);
        let width = s.len();
        let mut data = vec![C64::new(0.0, 0.0); left * 2 * keep];
        for row in 0..left * 2 {
            for j in 0..keep {
                data[row * keep + j] = u[row * width + j];
            }
        }
        self.sites[k] = Site {
            left,
            right: keep,
            data,
        };
        let next = &self.sites[k + 1];
        let columns = 2 * next.right;
        let mut merged = vec![C64::new(0.0, 0.0); keep * columns];
        for j in 0..keep {
            for m in 0..right {
                let weight = vt[j * right + m] * s[j];
                if weight == C64::new(0.0, 0.0) {
                    continue;
                }
                for c in 0..columns {
                    merged[j * columns + c] += weight * next.data[m * columns + c];
                }
            }
        }
        self.sites[k + 1] = Site {
            left: keep,
            right: next.right,
            data: merged,
        };
        self.center = k + 1;
    }

    fn step_left(&mut self) {
        let k = self.center;
        let site = &self.sites[k];
        let (left, right) = (site.left, site.right);
        let Svd { u, s, vt } = svd(&site.data, left, 2 * right);
        let keep = s
            .iter()
            .take_while(|&&x| x > NEGLIGIBLE * s[0].max(NEGLIGIBLE))
            .count()
            .max(1);
        let width = s.len();
        let data = vt[..keep * 2 * right].to_vec();
        self.sites[k] = Site {
            left: keep,
            right,
            data,
        };
        let previous = &self.sites[k - 1];
        let rows = previous.left * 2;
        let mut merged = vec![C64::new(0.0, 0.0); rows * keep];
        for row in 0..rows {
            for m in 0..left {
                let x = previous.data[row * left + m];
                if x == C64::new(0.0, 0.0) {
                    continue;
                }
                for j in 0..keep {
                    merged[row * keep + j] += x * u[m * width + j] * s[j];
                }
            }
        }
        self.sites[k - 1] = Site {
            left: previous.left,
            right: keep,
            data: merged,
        };
        self.center = k - 1;
    }

    fn move_center(&mut self, target: usize) {
        while self.center < target {
            self.step_right();
        }
        while self.center > target {
            self.step_left();
        }
    }

    fn adjacent(&mut self, k: usize, m: &Square) {
        self.move_center(k);
        let (a, b) = (&self.sites[k], &self.sites[k + 1]);
        let (left, middle, right) = (a.left, a.right, b.right);
        let mut theta = vec![C64::new(0.0, 0.0); left * 4 * right];
        for l in 0..left {
            for s1 in 0..2 {
                for x in 0..middle {
                    let first = a.at(l, s1, x);
                    if first == C64::new(0.0, 0.0) {
                        continue;
                    }
                    for s2 in 0..2 {
                        for r in 0..right {
                            theta[((l * 2 + s1) * 2 + s2) * right + r] += first * b.at(x, s2, r);
                        }
                    }
                }
            }
        }
        let mut gated = vec![C64::new(0.0, 0.0); theta.len()];
        for l in 0..left {
            for r in 0..right {
                for (out, row) in m.iter().enumerate() {
                    let mut sum = C64::new(0.0, 0.0);
                    for (input, &entry) in row.iter().enumerate() {
                        sum +=
                            entry * theta[((l * 2 + (input >> 1)) * 2 + (input & 1)) * right + r];
                    }
                    gated[((l * 2 + (out >> 1)) * 2 + (out & 1)) * right + r] = sum;
                }
            }
        }
        let Svd { u, s, vt } = svd(&gated, left * 2, 2 * right);
        let total: f64 = s.iter().map(|x| x * x).sum();
        let mut keep = s
            .iter()
            .take_while(|&&x| x > NEGLIGIBLE * s[0].max(NEGLIGIBLE))
            .count()
            .clamp(1, self.bond);
        keep = keep.min(s.len());
        let kept: f64 = s[..keep].iter().map(|x| x * x).sum();
        if total > 0.0 {
            self.discarded = 1.0 - (1.0 - self.discarded) * (kept / total);
        }
        let scale = if kept > 0.0 {
            (total / kept).sqrt()
        } else {
            1.0
        };
        let width = s.len();
        let mut first = vec![C64::new(0.0, 0.0); left * 2 * keep];
        for row in 0..left * 2 {
            for j in 0..keep {
                first[row * keep + j] = u[row * width + j];
            }
        }
        let columns = 2 * right;
        let mut second = vec![C64::new(0.0, 0.0); keep * columns];
        for j in 0..keep {
            for c in 0..columns {
                second[j * columns + c] = vt[j * columns + c] * s[j] * scale;
            }
        }
        self.sites[k] = Site {
            left,
            right: keep,
            data: first,
        };
        self.sites[k + 1] = Site {
            left: keep,
            right,
            data: second,
        };
        self.center = k + 1;
    }

    fn swap_sites(&mut self, k: usize) {
        self.adjacent(k, &swap_matrix());
        let (a, b) = (self.qubit_at[k], self.qubit_at[k + 1]);
        self.qubit_at.swap(k, k + 1);
        self.site_of[a] = k + 1;
        self.site_of[b] = k;
    }

    pub fn two(&mut self, first: usize, second: usize, m: &Square) {
        let home = self.site_of[second];
        self.meet(first, second, m);
        while self.site_of[second] < home {
            let k = self.site_of[second];
            self.swap_sites(k);
        }
        while self.site_of[second] > home {
            let k = self.site_of[second] - 1;
            self.swap_sites(k);
        }
    }

    fn meet(&mut self, first: usize, second: usize, m: &Square) {
        let anchor = self.site_of[first];
        while self.site_of[second] > anchor + 1 {
            let k = self.site_of[second] - 1;
            self.swap_sites(k);
        }
        while self.site_of[second] + 1 < anchor {
            let k = self.site_of[second];
            self.swap_sites(k);
        }
        let (a, b) = (self.site_of[first], self.site_of[second]);
        if a < b {
            self.adjacent(a, m);
        } else {
            self.adjacent(b, &swapped(m));
        }
    }

    pub fn apply(&mut self, gate: &Gate, params: &[f64]) {
        assert!(supports(gate), "gate is not supported by the MPS simulator");
        assert_eq!(
            params.len(),
            gate.kind.param_count(),
            "gate has the wrong number of parameters"
        );
        let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
        assert!(
            wires.iter().all(|&q| q < self.site_of.len()),
            "gate qubit is outside the MPS simulator"
        );
        for (i, &wire) in wires.iter().enumerate() {
            assert!(
                !wires[..i].contains(&wire),
                "a gate cannot use the same qubit more than once"
            );
        }
        match (gate.kind, &gate.controls[..], &gate.targets[..]) {
            (GateKind::Swap, [], [a, b]) => self.two(a.index(), b.index(), &swap_matrix()),
            (kind, [], [t]) => self.one(t.index(), &matrix_for(kind, params)),
            (kind, [c], [t]) => {
                self.two(c.index(), t.index(), &controlled(&matrix_for(kind, params)))
            }
            _ => unreachable!("supported gate shape was checked above"),
        }
    }

    pub fn measure(&mut self, qubit: usize, rng: &mut Rng) -> bool {
        let k = self.site_of[qubit];
        self.move_center(k);
        let site = &self.sites[k];
        let mut weights = [0.0; 2];
        for l in 0..site.left {
            for (s, weight) in weights.iter_mut().enumerate() {
                for r in 0..site.right {
                    *weight += site.at(l, s, r).norm_sqr();
                }
            }
        }
        let total = weights[0] + weights[1];
        let outcome = rng.next_unit() * total < weights[1];
        let chosen = weights[usize::from(outcome)];
        let scale = if chosen > 0.0 {
            1.0 / chosen.sqrt() * total.sqrt()
        } else {
            0.0
        };
        let site = &mut self.sites[k];
        for l in 0..site.left {
            for s in 0..2 {
                for r in 0..site.right {
                    let index = (l * 2 + s) * site.right + r;
                    site.data[index] = if s == usize::from(outcome) {
                        site.data[index] * scale
                    } else {
                        C64::new(0.0, 0.0)
                    };
                }
            }
        }
        outcome
    }

    pub fn prepared(&self) -> Mps {
        let mut copy = self.clone();
        copy.move_center(0);
        copy
    }

    pub fn draw(&self, measured: &[Option<usize>], rng: &mut Rng, results: &mut [bool]) {
        let last = (0..self.sites.len())
            .rev()
            .find(|&k| measured[self.qubit_at[k]].is_some());
        let Some(last) = last else {
            return;
        };
        let mut pure: Option<Vec<C64>> = Some(vec![C64::new(1.0, 0.0)]);
        let mut mixed: Vec<C64> = Vec::new();
        for k in 0..=last {
            let site = &self.sites[k];
            let (left, right) = (site.left, site.right);
            let slot = measured[self.qubit_at[k]];
            if let Some(vector) = &pure {
                let branches: Vec<Vec<C64>> = (0..2)
                    .map(|s| {
                        (0..right)
                            .map(|r| (0..left).map(|l| vector[l] * site.at(l, s, r)).sum())
                            .collect()
                    })
                    .collect();
                match slot {
                    Some(result) => {
                        let weights: Vec<f64> = branches
                            .iter()
                            .map(|w| w.iter().map(C64::norm_sqr).sum())
                            .collect();
                        let outcome = rng.next_unit() * (weights[0] + weights[1]) < weights[1];
                        let chosen = usize::from(outcome);
                        let norm = weights[chosen].sqrt().max(f64::MIN_POSITIVE);
                        results[result] = outcome;
                        pure = Some(branches[chosen].iter().map(|x| x / norm).collect());
                    }
                    None => {
                        mixed = vec![C64::new(0.0, 0.0); right * right];
                        for w in &branches {
                            for (i, x) in w.iter().enumerate() {
                                for (j, y) in w.iter().enumerate() {
                                    mixed[i * right + j] += x * y.conj();
                                }
                            }
                        }
                        pure = None;
                    }
                }
                continue;
            }
            let project = |s: usize| {
                let mut next = vec![C64::new(0.0, 0.0); right * right];
                for l in 0..left {
                    for m in 0..left {
                        let e = mixed[l * left + m];
                        if e == C64::new(0.0, 0.0) {
                            continue;
                        }
                        for r in 0..right {
                            let x = site.at(l, s, r) * e;
                            for q in 0..right {
                                next[r * right + q] += x * site.at(m, s, q).conj();
                            }
                        }
                    }
                }
                next
            };
            let trace = |e: &[C64]| (0..right).map(|i| e[i * right + i].re).sum::<f64>();
            let (zero, one) = (project(0), project(1));
            match slot {
                Some(result) => {
                    let (a, b) = (trace(&zero), trace(&one));
                    let outcome = rng.next_unit() * (a + b) < b;
                    results[result] = outcome;
                    let (chosen, weight) = if outcome { (one, b) } else { (zero, a) };
                    let weight = weight.max(f64::MIN_POSITIVE);
                    mixed = chosen.into_iter().map(|x| x / weight).collect();
                }
                None => {
                    mixed = zero.iter().zip(&one).map(|(x, y)| x + y).collect();
                }
            }
        }
    }

    pub fn expectation(&self, paulis: &[(usize, Matrix2)]) -> C64 {
        let mut env = vec![C64::new(1.0, 0.0)];
        for (k, site) in self.sites.iter().enumerate() {
            let m = paulis
                .iter()
                .find(|(q, _)| self.site_of[*q] == k)
                .map_or(Matrix2::identity(), |(_, m)| *m);
            let op = [[m.a, m.b], [m.c, m.d]];
            let (left, right) = (site.left, site.right);
            let mut next = vec![C64::new(0.0, 0.0); right * right];
            for l in 0..left {
                for (s, row) in op.iter().enumerate() {
                    for r in 0..right {
                        let bra = site.at(l, s, r).conj();
                        if bra == C64::new(0.0, 0.0) {
                            continue;
                        }
                        for m in 0..left {
                            let e = env[l * left + m];
                            if e == C64::new(0.0, 0.0) {
                                continue;
                            }
                            for (t, &o) in row.iter().enumerate() {
                                if o == C64::new(0.0, 0.0) {
                                    continue;
                                }
                                let weight = bra * e * o;
                                for q in 0..right {
                                    next[r * right + q] += weight * site.at(m, t, q);
                                }
                            }
                        }
                    }
                }
            }
            env = next;
        }
        env[0]
    }

    pub fn amplitude(&self, basis: usize) -> C64 {
        let mut vector = vec![C64::new(1.0, 0.0)];
        for (k, site) in self.sites.iter().enumerate() {
            let s = (basis >> self.qubit_at[k]) & 1;
            vector = (0..site.right)
                .map(|r| (0..site.left).map(|l| vector[l] * site.at(l, s, r)).sum())
                .collect();
        }
        vector[0]
    }
}

#[cfg(test)]
mod tests {
    use super::{Mps, arrangement};
    use crate::diag::Span;
    use crate::ir::{Gate, GateKind, Profile, Program, QubitId};

    #[test]
    fn sparse_arrangement_does_not_allocate_a_dense_matrix() {
        let mut program = Program::new("wide", Profile::Unrestricted);
        program.num_qubits = 1 << 16;
        assert_eq!(arrangement(&program), None);
    }

    #[test]
    #[should_panic(expected = "not supported")]
    fn apply_rejects_unsupported_gate() {
        let gate = Gate {
            kind: GateKind::Swap,
            controls: Vec::new(),
            targets: vec![QubitId(0)],
            params: Vec::new(),
            span: Span::DUMMY,
        };
        Mps::new(1, 2).apply(&gate, &[]);
    }
}
