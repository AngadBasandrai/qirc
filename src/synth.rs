use std::collections::HashMap;
use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI};

use crate::codegen::zyz_angles;
use crate::diag::Span;
use crate::ir::*;
use crate::kak::{self, Piece};
use crate::qsd;
use crate::simulator::matrix::{C64, Matrix2, matrix_for};
use crate::transpile::GateSet;

const EPSILON: f64 = 1e-9;
const MAX_WINDOW: usize = 12;
const MAX_REACH: usize = 64;
const MAX_WIDE: usize = 96;
const MAX_WIDE_REACH: usize = 160;
const MAX_WORDS: usize = 50_000;
const SHORT_WORDS: usize = 2_000;
const WORD_DEPTH: usize = 8;

const FIXED: [GateKind; 10] = [
    GateKind::X,
    GateKind::Y,
    GateKind::Z,
    GateKind::H,
    GateKind::S,
    GateKind::SDag,
    GateKind::T,
    GateKind::TDag,
    GateKind::SX,
    GateKind::SXDag,
];

const ZERO: C64 = C64::new(0.0, 0.0);
const ONE: C64 = C64::new(1.0, 0.0);

#[derive(Clone)]
pub struct Unitary {
    pub(crate) dim: usize,
    pub(crate) cells: Vec<C64>,
}

impl Unitary {
    pub(crate) fn identity(qubits: usize) -> Unitary {
        let dim = 1 << qubits;
        let mut cells = vec![ZERO; dim * dim];
        for i in 0..dim {
            cells[i * dim + i] = ONE;
        }
        Unitary { dim, cells }
    }

    pub fn of(gates: &[Gate], wires: &[QubitId]) -> Option<Unitary> {
        gates
            .iter()
            .try_fold(Unitary::identity(wires.len()), |product, gate| {
                Some(Unitary::gate(gate, wires)?.after(&product))
            })
    }

    fn gate(gate: &Gate, wires: &[QubitId]) -> Option<Unitary> {
        let bit = |q: &QubitId| wires.iter().position(|w| w == q);
        let controls = gate
            .controls
            .iter()
            .map(|q| bit(q).map(|b| 1usize << b))
            .sum::<Option<usize>>()?;
        let targets = gate.targets.iter().map(bit).collect::<Option<Vec<_>>>()?;
        let params = gate
            .params
            .iter()
            .map(|p| p.constant().map(Const::as_f64))
            .collect::<Option<Vec<_>>>()?;

        let dim = 1 << wires.len();
        let matrix = matrix_for(gate.kind, &params);
        let mut cells = vec![ZERO; dim * dim];

        for column in 0..dim {
            let mut buffer = [ZERO; 8];
            let v = &mut buffer[..dim];
            v[column] = ONE;
            match (gate.kind, targets.as_slice()) {
                (GateKind::Swap, &[a, b]) => {
                    for i in 0..dim {
                        if i & controls == controls && (i >> a) & 1 == 1 && (i >> b) & 1 == 0 {
                            v.swap(i, i ^ (1 << a) ^ (1 << b));
                        }
                    }
                }
                _ => {
                    for &t in &targets {
                        for i in 0..dim {
                            if i & controls == controls && (i >> t) & 1 == 0 {
                                let j = i | 1 << t;
                                let (x, y) = (v[i], v[j]);
                                v[i] = matrix.a * x + matrix.b * y;
                                v[j] = matrix.c * x + matrix.d * y;
                            }
                        }
                    }
                }
            }
            for (row, value) in v.iter().enumerate() {
                cells[row * dim + column] = *value;
            }
        }

        Some(Unitary { dim, cells })
    }

    fn one(m: Matrix2) -> Unitary {
        Unitary {
            dim: 2,
            cells: vec![m.a, m.b, m.c, m.d],
        }
    }

    pub(crate) fn kron(low: Matrix2, high: Matrix2) -> Unitary {
        let mut cells = vec![ZERO; 16];
        for row in 0..4 {
            for column in 0..4 {
                cells[row * 4 + column] =
                    cell(high, row >> 1, column >> 1) * cell(low, row & 1, column & 1);
            }
        }
        Unitary { dim: 4, cells }
    }

    fn widen(&self, qubits: usize) -> Unitary {
        let dim = 1 << qubits;
        let mut cells = vec![ZERO; dim * dim];
        for row in 0..dim {
            for column in 0..dim {
                if row / self.dim == column / self.dim {
                    cells[row * dim + column] =
                        self.cells[(row % self.dim) * self.dim + column % self.dim];
                }
            }
        }
        Unitary { dim, cells }
    }

    fn matrix2(&self) -> Matrix2 {
        Matrix2::new(self.cells[0], self.cells[1], self.cells[2], self.cells[3])
    }

    pub(crate) fn after(&self, earlier: &Unitary) -> Unitary {
        let dim = self.dim;
        let mut cells = vec![ZERO; dim * dim];
        for row in 0..dim {
            for k in 0..dim {
                let x = self.cells[row * dim + k];
                if x == ZERO {
                    continue;
                }
                for column in 0..dim {
                    cells[row * dim + column] += x * earlier.cells[k * dim + column];
                }
            }
        }
        Unitary { dim, cells }
    }

    pub(crate) fn adjoint(&self) -> Unitary {
        let dim = self.dim;
        let mut cells = vec![ZERO; dim * dim];
        for row in 0..dim {
            for column in 0..dim {
                cells[column * dim + row] = self.cells[row * dim + column].conj();
            }
        }
        Unitary { dim, cells }
    }

    pub fn same(&self, other: &Unitary) -> bool {
        if self.dim != other.dim {
            return false;
        }
        let Some(pivot) = (0..self.cells.len())
            .max_by(|&a, &b| self.cells[a].norm().total_cmp(&self.cells[b].norm()))
        else {
            return false;
        };
        if other.cells[pivot].norm() < EPSILON {
            return false;
        }
        let phase = other.cells[pivot] / self.cells[pivot];
        (phase.norm() - 1.0).abs() < EPSILON
            && self
                .cells
                .iter()
                .zip(&other.cells)
                .all(|(a, b)| (a * phase - b).norm() < EPSILON)
    }

    fn equals(&self, other: &Unitary) -> bool {
        self.cells
            .iter()
            .zip(&other.cells)
            .all(|(a, b)| (a - b).norm() < EPSILON)
    }

    fn key(&self) -> Vec<i64> {
        let pivot = self
            .cells
            .iter()
            .find(|c| c.norm() > 1e-6)
            .copied()
            .unwrap_or(ONE);
        let phase = pivot.conj() / pivot.norm();
        self.cells
            .iter()
            .flat_map(|c| {
                let v = c * phase;
                [(v.re * 1e6).round() as i64, (v.im * 1e6).round() as i64]
            })
            .collect()
    }

    pub(crate) fn split(&self) -> Option<(Matrix2, Matrix2)> {
        if self.dim != 4 {
            return None;
        }
        let at = |row: usize, column: usize| self.cells[row * 4 + column];
        let pivot =
            (0..16).max_by(|&a, &b| self.cells[a].norm().total_cmp(&self.cells[b].norm()))?;
        let (high_row, low_row) = ((pivot / 4) >> 1, (pivot / 4) & 1);
        let (high_column, low_column) = ((pivot % 4) >> 1, (pivot % 4) & 1);

        let low = unitarise(Matrix2::new(
            at(high_row << 1, high_column << 1),
            at(high_row << 1, high_column << 1 | 1),
            at(high_row << 1 | 1, high_column << 1),
            at(high_row << 1 | 1, high_column << 1 | 1),
        ))?;
        let high = unitarise(Matrix2::new(
            at(low_row, low_column),
            at(low_row, 2 | low_column),
            at(2 | low_row, low_column),
            at(2 | low_row, 2 | low_column),
        ))?;
        Unitary::kron(low, high).same(self).then_some((low, high))
    }
}

fn cell(m: Matrix2, row: usize, column: usize) -> C64 {
    match (row, column) {
        (0, 0) => m.a,
        (0, 1) => m.b,
        (1, 0) => m.c,
        _ => m.d,
    }
}

fn unitarise(m: Matrix2) -> Option<Matrix2> {
    let scale = (m.a * m.d - m.b * m.c).norm().sqrt();
    (scale > EPSILON).then(|| Matrix2::new(m.a / scale, m.b / scale, m.c / scale, m.d / scale))
}

fn negate(m: Matrix2) -> Matrix2 {
    Matrix2::new(-m.a, -m.b, -m.c, -m.d)
}

fn conjugate(w: Matrix2, m: Matrix2) -> Matrix2 {
    w.multiply(m).multiply(w.adjoint())
}

fn wrap(angle: f64) -> f64 {
    let turn = 2.0 * PI;
    let wrapped = angle.rem_euclid(turn);
    if wrapped > PI {
        wrapped - turn
    } else {
        wrapped
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    fn of(kind: GateKind) -> Option<Axis> {
        match kind {
            GateKind::Rx => Some(Axis::X),
            GateKind::Ry => Some(Axis::Y),
            GateKind::Rz | GateKind::R1 => Some(Axis::Z),
            _ => None,
        }
    }

    fn pauli(self) -> Matrix2 {
        match self {
            Axis::X => Matrix2::x(),
            Axis::Y => Matrix2::y(),
            Axis::Z => Matrix2::z(),
        }
    }
}

fn sign(m: Matrix2, axis: Axis) -> Option<f64> {
    let p = axis.pauli();
    if m.approx_eq(&p, EPSILON) {
        Some(1.0)
    } else if m.approx_eq(&negate(p), EPSILON) {
        Some(-1.0)
    } else {
        None
    }
}

fn cliffords() -> Vec<Matrix2> {
    let mut found = vec![Matrix2::identity()];
    let mut keys = vec![Unitary::one(Matrix2::identity()).key()];
    let mut index = 0;
    while index < found.len() {
        for step in [Matrix2::h(), Matrix2::s()] {
            let next = step.multiply(found[index]);
            let key = Unitary::one(next).key();
            if !keys.contains(&key) {
                keys.push(key);
                found.push(next);
            }
        }
        index += 1;
    }
    found
}

enum Turn {
    Direct(GateKind),
    Conjugated {
        kind: GateKind,
        before: Vec<Gate>,
        after: Vec<Gate>,
    },
}

pub(crate) struct Synth {
    fixed: Vec<GateKind>,
    direct: Vec<(Axis, GateKind)>,
    turns: Vec<(Axis, Turn)>,
    words: HashMap<Vec<i64>, (Matrix2, Vec<GateKind>)>,
    frames: Vec<Matrix2>,
    sx: bool,
    entanglers: Vec<GateKind>,
}

impl Synth {
    pub fn new(set: &GateSet) -> Synth {
        let fixed: Vec<GateKind> = FIXED.into_iter().filter(|&k| set.has(k, 0)).collect();
        let mut direct = Vec::new();
        for kind in [GateKind::Rz, GateKind::R1, GateKind::Rx, GateKind::Ry] {
            if let Some(axis) = Axis::of(kind)
                && set.has(kind, 0)
                && !direct.iter().any(|&(a, _)| a == axis)
            {
                direct.push((axis, kind));
            }
        }

        let limit = if direct.is_empty() {
            MAX_WORDS
        } else {
            SHORT_WORDS
        };
        let mut synth = Synth {
            words: words(&fixed, limit),
            fixed,
            direct,
            turns: Vec::new(),
            frames: cliffords(),
            sx: set.has(GateKind::SX, 0),
            entanglers: [GateKind::X, GateKind::Z, GateKind::Y, GateKind::H]
                .into_iter()
                .filter(|&k| set.has(k, 1))
                .collect(),
        };
        synth.turns = synth
            .direct
            .iter()
            .map(|&(axis, kind)| (axis, Turn::Direct(kind)))
            .collect();
        let conjugated: Vec<(Axis, Turn)> = Axis::ALL
            .into_iter()
            .filter(|axis| !synth.direct.iter().any(|(a, _)| a == axis))
            .filter_map(|axis| Some((axis, synth.conjugated(axis)?)))
            .collect();
        synth.turns.extend(conjugated);
        synth
    }

    fn conjugated(&self, axis: Axis) -> Option<Turn> {
        let mut best: Option<(usize, Turn)> = None;
        for &(from, kind) in &self.direct {
            for w in &self.frames {
                if sign(conjugate(*w, from.pauli()), axis) != Some(1.0) {
                    continue;
                }
                let (Some(before), Some(after)) = (
                    self.one(w.adjoint(), PAIR[0], Span::DUMMY),
                    self.one(*w, PAIR[0], Span::DUMMY),
                ) else {
                    continue;
                };
                let len = before.len() + after.len();
                if best.as_ref().is_none_or(|(shortest, _)| len < *shortest) {
                    best = Some((
                        len,
                        Turn::Conjugated {
                            kind,
                            before,
                            after,
                        },
                    ));
                }
            }
        }
        best.map(|(_, turn)| turn)
    }

    fn rotate(&self, axis: Axis, angle: Operand, target: QubitId, span: Span) -> Option<Vec<Gate>> {
        let (_, turn) = self.turns.iter().find(|(a, _)| *a == axis)?;
        let rotation = |kind, angle| Gate {
            kind,
            controls: Vec::new(),
            targets: vec![target],
            params: vec![angle],
            span,
        };
        Some(match turn {
            Turn::Direct(kind) => vec![rotation(*kind, angle)],
            Turn::Conjugated {
                kind,
                before,
                after,
            } => {
                let place = |gates: &[Gate]| {
                    gates
                        .iter()
                        .map(|g| relabel(g.clone(), &[target], span))
                        .collect::<Vec<_>>()
                };
                [place(before), vec![rotation(*kind, angle)], place(after)].concat()
            }
        })
    }

    pub fn turn(
        &self,
        kind: GateKind,
        angle: Operand,
        target: QubitId,
        span: Span,
    ) -> Option<Vec<Gate>> {
        self.rotate(Axis::of(kind)?, angle, target, span)
    }

    fn angle(&self, axis: Axis, theta: f64, target: QubitId, span: Span) -> Option<Vec<Gate>> {
        let theta = wrap(theta);
        if theta.abs() < EPSILON {
            return Some(Vec::new());
        }
        self.rotate(axis, Operand::Const(Const::Float(theta)), target, span)
    }

    fn solve(&self, u: Matrix2, axis: Axis) -> Option<f64> {
        let w = self
            .frames
            .iter()
            .find(|w| sign(conjugate(**w, Matrix2::z()), axis) == Some(1.0))?;
        let v = w.adjoint().multiply(u).multiply(*w);
        (v.b.norm() < EPSILON && v.c.norm() < EPSILON && v.a.norm() > EPSILON)
            .then(|| (v.d / v.a).arg())
    }

    fn euler(
        &self,
        u: Matrix2,
        outer: Axis,
        inner: Axis,
        target: QubitId,
        span: Span,
    ) -> Option<Vec<Gate>> {
        let (w, s_outer, s_inner) = self.frames.iter().find_map(|w| {
            Some((
                *w,
                sign(conjugate(*w, Matrix2::z()), outer)?,
                sign(conjugate(*w, Matrix2::y()), inner)?,
            ))
        })?;
        let (theta, phi, lambda) = zyz_angles(&w.adjoint().multiply(u).multiply(w));
        Some(
            [
                self.angle(outer, s_outer * lambda, target, span)?,
                self.angle(inner, s_inner * theta, target, span)?,
                self.angle(outer, s_outer * phi, target, span)?,
            ]
            .concat(),
        )
    }

    pub fn one(&self, u: Matrix2, target: QubitId, span: Span) -> Option<Vec<Gate>> {
        let goal = Unitary::one(u);
        let mut best: Option<Vec<Gate>> = None;
        let mut consider = |candidate: Option<Vec<Gate>>| {
            if let Some(gates) = candidate
                && best.as_ref().is_none_or(|b| gates.len() < b.len())
                && Unitary::of(&gates, &[target]).is_some_and(|m| m.same(&goal))
            {
                best = Some(gates);
            }
        };

        if let Some((_, word)) = self.words.get(&goal.key()) {
            consider(Some(word.iter().map(|&k| fixed(k, target, span)).collect()));
        }
        for &(axis, _) in &self.direct {
            consider(
                self.solve(u, axis)
                    .and_then(|t| self.angle(axis, t, target, span)),
            );
            for &kind in &self.fixed {
                let f = matrix_for(kind, &[]);
                let after = self.solve(u.multiply(f.adjoint()), axis);
                consider(after.and_then(|t| {
                    Some(
                        [
                            vec![fixed(kind, target, span)],
                            self.angle(axis, t, target, span)?,
                        ]
                        .concat(),
                    )
                }));
                let before = self.solve(f.adjoint().multiply(u), axis);
                consider(before.and_then(|t| {
                    Some(
                        [
                            self.angle(axis, t, target, span)?,
                            vec![fixed(kind, target, span)],
                        ]
                        .concat(),
                    )
                }));
            }
        }
        consider(self.root_x(u, target, span));
        for (outer, _) in &self.turns {
            for (inner, _) in &self.turns {
                if outer != inner {
                    consider(self.euler(u, *outer, *inner, target, span));
                }
            }
        }
        best
    }

    fn root_x(&self, u: Matrix2, target: QubitId, span: Span) -> Option<Vec<Gate>> {
        if !self.sx || !self.direct.iter().any(|&(a, _)| a == Axis::Z) {
            return None;
        }
        let (theta, phi, lambda) = zyz_angles(&u);
        let sx = fixed(GateKind::SX, target, span);
        if (theta - FRAC_PI_2).abs() < EPSILON {
            return Some(
                [
                    self.angle(Axis::Z, lambda - FRAC_PI_2, target, span)?,
                    vec![sx],
                    self.angle(Axis::Z, phi + FRAC_PI_2, target, span)?,
                ]
                .concat(),
            );
        }
        Some(
            [
                self.angle(Axis::Z, lambda, target, span)?,
                vec![sx.clone()],
                self.angle(Axis::Z, theta + PI, target, span)?,
                vec![sx],
                self.angle(Axis::Z, phi + PI, target, span)?,
            ]
            .concat(),
        )
    }

    pub fn cx(&self, control: QubitId, target: QubitId, span: Span) -> Option<Vec<Gate>> {
        for &kind in &self.entanglers {
            let entangler = Gate {
                kind,
                controls: vec![control],
                targets: vec![target],
                params: Vec::new(),
                span,
            };
            if kind == GateKind::X {
                return Some(vec![entangler]);
            }
            let Some(a) = involution_frame(matrix_for(kind, &[])) else {
                continue;
            };
            if let (Some(first), Some(last)) = (
                self.one(a.adjoint(), target, span),
                self.one(a, target, span),
            ) {
                return Some([first, vec![entangler], last].concat());
            }
        }
        None
    }
}

fn involution_frame(k: Matrix2) -> Option<Matrix2> {
    let column = |m: Matrix2| {
        let (a, b) = if m.a.norm() + m.c.norm() > m.b.norm() + m.d.norm() {
            (m.a, m.c)
        } else {
            (m.b, m.d)
        };
        let n = (a.norm_sqr() + b.norm_sqr()).sqrt();
        (n > EPSILON).then(|| (a / n, b / n))
    };
    let half = C64::new(0.5, 0.0);
    let plus = Matrix2::new(half + half * k.a, half * k.b, half * k.c, half + half * k.d);
    let minus = Matrix2::new(
        half - half * k.a,
        -half * k.b,
        -half * k.c,
        half - half * k.d,
    );
    let (p0, p1) = column(plus)?;
    let (m0, m1) = column(minus)?;
    let r = C64::new(FRAC_1_SQRT_2, 0.0);
    Some(Matrix2::new(
        r * (p0.conj() + m0.conj()),
        r * (p1.conj() + m1.conj()),
        r * (p0.conj() - m0.conj()),
        r * (p1.conj() - m1.conj()),
    ))
}

fn words(fixed: &[GateKind], limit: usize) -> HashMap<Vec<i64>, (Matrix2, Vec<GateKind>)> {
    let mut table = HashMap::new();
    table.insert(
        Unitary::one(Matrix2::identity()).key(),
        (Matrix2::identity(), Vec::new()),
    );
    let mut frontier = vec![(Matrix2::identity(), Vec::new())];
    for _ in 0..WORD_DEPTH {
        let mut next = Vec::new();
        for (m, word) in &frontier {
            for &kind in fixed {
                let product = matrix_for(kind, &[]).multiply(*m);
                let key = Unitary::one(product).key();
                if table.len() < limit && !table.contains_key(&key) {
                    let mut longer: Vec<GateKind> = word.clone();
                    longer.push(kind);
                    table.insert(key, (product, longer.clone()));
                    next.push((product, longer));
                }
            }
        }
        frontier = next;
    }
    table
}

fn fixed(kind: GateKind, target: QubitId, span: Span) -> Gate {
    Gate {
        kind,
        controls: Vec::new(),
        targets: vec![target],
        params: Vec::new(),
        span,
    }
}

struct Pairs {
    alphabet: Vec<Gate>,
    table: Vec<(Unitary, Vec<usize>)>,
    index: HashMap<Vec<i64>, usize>,
    controlled: Vec<GateKind>,
}

const PAIR: [QubitId; 2] = [QubitId(0), QubitId(1)];
const TRIPLE: [QubitId; 3] = [QubitId(0), QubitId(1), QubitId(2)];

impl Pairs {
    fn new(set: &GateSet, fixed_kinds: &[GateKind], depth: usize) -> Pairs {
        let mut alphabet = Vec::new();
        for &kind in fixed_kinds {
            for q in PAIR {
                alphabet.push(fixed(kind, q, Span::DUMMY));
            }
        }
        for kind in [GateKind::X, GateKind::Y, GateKind::Z, GateKind::H] {
            if set.has(kind, 1) {
                for (c, t) in [(PAIR[0], PAIR[1]), (PAIR[1], PAIR[0])] {
                    alphabet.push(Gate {
                        kind,
                        controls: vec![c],
                        targets: vec![t],
                        params: Vec::new(),
                        span: Span::DUMMY,
                    });
                }
            }
        }
        if set.has(GateKind::Swap, 0) {
            alphabet.push(Gate {
                kind: GateKind::Swap,
                controls: Vec::new(),
                targets: PAIR.to_vec(),
                params: Vec::new(),
                span: Span::DUMMY,
            });
        }

        let mut table = vec![(Unitary::identity(2), Vec::new())];
        let mut index = HashMap::from([(Unitary::identity(2).key(), 0)]);
        let mut frontier = vec![0];
        for _ in 0..depth {
            let mut next = Vec::new();
            for &entry in &frontier {
                for (letter, gate) in alphabet.iter().enumerate() {
                    if table.len() >= MAX_WORDS {
                        break;
                    }
                    let Some(g) = Unitary::gate(gate, &PAIR) else {
                        continue;
                    };
                    let product = g.after(&table[entry].0);
                    let key = product.key();
                    if index.contains_key(&key) {
                        continue;
                    }
                    let mut word = table[entry].1.clone();
                    word.push(letter);
                    index.insert(key, table.len());
                    next.push(table.len());
                    table.push((product, word));
                }
            }
            frontier = next;
        }

        Pairs {
            alphabet,
            table,
            index,
            controlled: [GateKind::Rz, GateKind::R1, GateKind::Rx, GateKind::Ry]
                .into_iter()
                .filter(|&k| set.has(k, 1))
                .collect(),
        }
    }

    fn word(&self, letters: &[usize]) -> Vec<Gate> {
        letters.iter().map(|&l| self.alphabet[l].clone()).collect()
    }

    fn find(&self, u: &Unitary, synth: &Synth, limit: usize, cost: Cost) -> Option<Vec<Gate>> {
        let mut best: Option<Vec<Gate>> = None;
        let mut consider = |candidate: Option<Vec<Gate>>| {
            if let Some(gates) = candidate
                && gates.len() <= limit
                && best.as_ref().is_none_or(|b| cost.of(&gates) < cost.of(b))
                && Unitary::of(&gates, &PAIR).is_some_and(|m| m.same(u))
            {
                best = Some(gates);
            }
        };

        let split = |m: &Unitary| {
            let (low, high) = m.split()?;
            Some(
                [
                    synth.one(low, PAIR[0], Span::DUMMY)?,
                    synth.one(high, PAIR[1], Span::DUMMY)?,
                ]
                .concat(),
            )
        };

        consider(split(u));
        for (m, first) in &self.table {
            if let Some(&found) = self.index.get(&u.after(&m.adjoint()).key()) {
                consider(Some(
                    [self.word(first), self.word(&self.table[found].1)].concat(),
                ));
            }
        }
        for (letter, gate) in self.alphabet.iter().enumerate() {
            if gate.wires().count() < 2 {
                continue;
            }
            let Some(g) = Unitary::gate(gate, &PAIR) else {
                continue;
            };
            consider(
                split(&g.adjoint().after(u)).map(|rest| [rest, self.word(&[letter])].concat()),
            );
            consider(
                split(&u.after(&g.adjoint())).map(|rest| [self.word(&[letter]), rest].concat()),
            );
        }
        for &kind in &self.controlled {
            for (c, t) in [(PAIR[0], PAIR[1]), (PAIR[1], PAIR[0])] {
                consider(controlled_rotation(u, kind, c, t));
            }
        }
        best
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Cost {
    #[default]
    Gates,
    Cx,
    Ibm,
}

impl Cost {
    pub fn parse(name: &str) -> Option<Cost> {
        match name {
            "gates" => Some(Cost::Gates),
            "cx" => Some(Cost::Cx),
            "ibm" => Some(Cost::Ibm),
            _ => None,
        }
    }

    fn weight(self, gate: &Gate) -> (usize, usize) {
        let entangler = gate.wires().count() > 1;
        match self {
            Cost::Gates => (1, usize::from(entangler)),
            Cost::Cx => (usize::from(entangler), 1),
            Cost::Ibm if entangler => (10, 1),
            Cost::Ibm => match gate.kind {
                GateKind::Rz
                | GateKind::R1
                | GateKind::Z
                | GateKind::S
                | GateKind::SDag
                | GateKind::T
                | GateKind::TDag => (0, 1),
                _ => (1, 1),
            },
        }
    }

    pub fn of<'a>(self, gates: impl IntoIterator<Item = &'a Gate>) -> (usize, usize) {
        gates.into_iter().fold((0, 0), |(a, b), gate| {
            let (c, d) = self.weight(gate);
            (a + c, b + d)
        })
    }
}

fn controlled_rotation(
    u: &Unitary,
    kind: GateKind,
    control: QubitId,
    target: QubitId,
) -> Option<Vec<Gate>> {
    let (c, t) = (control.index(), target.index());
    let at = |ctl: usize, row: usize, column: usize| {
        u.cells[(ctl << c | row << t) * 4 + (ctl << c | column << t)]
    };
    let block = |ctl| Matrix2::new(at(ctl, 0, 0), at(ctl, 0, 1), at(ctl, 1, 0), at(ctl, 1, 1));
    let idle = block(0);
    if idle.b.norm() > EPSILON || idle.c.norm() > EPSILON || idle.a.norm() < EPSILON {
        return None;
    }
    let phase = idle.a;
    let active = block(1);
    let r = Matrix2::new(
        active.a / phase,
        active.b / phase,
        active.c / phase,
        active.d / phase,
    );
    let theta = match kind {
        GateKind::R1 => r.d.arg(),
        GateKind::Rz => 2.0 * r.d.arg(),
        GateKind::Rx => 2.0 * (-r.b.im).atan2(r.a.re),
        GateKind::Ry => 2.0 * r.c.re.atan2(r.a.re),
        _ => return None,
    };
    Some(vec![Gate {
        kind,
        controls: vec![control],
        targets: vec![target],
        params: vec![Operand::Const(Const::Float(theta))],
        span: Span::DUMMY,
    }])
}

struct Search {
    synth: Synth,
    pairs: Pairs,
    limit: usize,
    cost: Cost,
    cache: HashMap<(usize, Vec<i64>), Option<Vec<Gate>>>,
}

impl Search {
    fn new(set: &GateSet, limit: usize, cost: Cost) -> Search {
        let synth = Synth::new(set);
        let pairs = Pairs::new(set, &synth.fixed, limit.div_ceil(2).min(3));
        Search {
            synth,
            pairs,
            limit,
            cost,
            cache: HashMap::new(),
        }
    }

    fn best(&mut self, u: &Unitary, qubits: usize) -> Option<Vec<Gate>> {
        let key = (qubits, u.key());
        if let Some(found) = self.cache.get(&key) {
            return found.clone();
        }
        let found = match qubits {
            1 => self
                .synth
                .one(u.matrix2(), PAIR[0], Span::DUMMY)
                .filter(|gates| gates.len() <= self.limit),
            2 => [
                self.pairs.find(u, &self.synth, self.limit, self.cost),
                kak::synthesize(u).and_then(|pieces| self.build(pieces, &PAIR, u)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|gates| self.cost.of(gates)),
            _ => qsd::synthesize(u).and_then(|pieces| self.build(pieces, &TRIPLE, u)),
        };
        self.cache.insert(key, found.clone());
        found
    }

    fn open(&self) -> bool {
        self.limit >= 2 || self.cost != Cost::Gates
    }

    fn build(&self, pieces: Vec<Piece>, wires: &[QubitId], u: &Unitary) -> Option<Vec<Gate>> {
        if !self.open() {
            return None;
        }
        let parts = pieces
            .into_iter()
            .map(|piece| match piece {
                Piece::Local(q, m) => self.synth.one(m, wires[q], Span::DUMMY),
                Piece::Cx(c, t) => self.synth.cx(wires[c], wires[t], Span::DUMMY),
            })
            .collect::<Option<Vec<_>>>()?;
        let gates = parts.concat();
        Unitary::of(&gates, wires)
            .is_some_and(|m| m.same(u))
            .then_some(gates)
    }

    fn window(&mut self, ops: &[Op]) -> Option<(Vec<usize>, Vec<Gate>)> {
        let last = ops.len().checked_sub(1)?;
        let Op::Gate(newest) = &ops[last] else {
            return None;
        };
        let mut wires = distinct(newest.wires());
        if wires.len() > 2 || newest.params.iter().any(|p| p.constant().is_none()) {
            return None;
        }

        let (cap, reach, width) = if self.open() {
            (MAX_WIDE, MAX_WIDE_REACH, 3)
        } else {
            (MAX_WINDOW, MAX_REACH, 2)
        };
        let mut suffixes = vec![(
            last,
            wires.clone(),
            Unitary::gate(newest, &wires)?,
            self.cost.of([newest]),
            usize::from(wires.len() > 1),
        )];
        let mut skipped = Vec::new();
        for index in (last.saturating_sub(reach)..last).rev() {
            if suffixes.len() == cap {
                break;
            }
            let qubits = ops[index].qubits();
            if !qubits.iter().any(|q| wires.contains(q)) {
                skipped.extend(qubits);
                continue;
            }
            let Op::Gate(gate) = &ops[index] else {
                break;
            };
            let grown = distinct(wires.iter().copied().chain(gate.wires()));
            if grown.len() > width
                || grown
                    .iter()
                    .any(|q| !wires.contains(q) && skipped.contains(q))
                || gate.params.iter().any(|p| p.constant().is_none())
            {
                break;
            }
            let (_, _, later, spent, entangled) = &suffixes[suffixes.len() - 1];
            let (a, b) = self.cost.of([gate]);
            let spent = (spent.0 + a, spent.1 + b);
            let entangled = entangled + usize::from(gate.wires().count() > 1);
            let step = Unitary::gate(gate, &grown)?;
            let product = if grown.len() > wires.len() {
                later.widen(grown.len()).after(&step)
            } else {
                later.after(&step)
            };
            wires = grown;
            suffixes.push((index, wires.clone(), product, spent, entangled));
        }

        for n in (2..=suffixes.len()).rev() {
            let (_, local, u, spent, entangled) = &suffixes[n - 1];
            let widest = n == suffixes.len() || suffixes[n].1.len() > local.len();
            if (n > MAX_WINDOW && !widest)
                || (local.len() == 3 && (!widest || *entangled <= qsd::MAX_CX))
            {
                continue;
            }
            let spent = *spent;
            let Some(found) = self.best(u, local.len()) else {
                continue;
            };
            if self.cost.of(&found) < spent {
                let placed = found
                    .into_iter()
                    .map(|g| relabel(g, local, newest.span))
                    .collect();
                let mut picked: Vec<usize> = suffixes[..n].iter().map(|(i, ..)| *i).collect();
                picked.reverse();
                return Some((picked, placed));
            }
        }
        None
    }
}

fn distinct(wires: impl Iterator<Item = QubitId>) -> Vec<QubitId> {
    let mut out = Vec::new();
    for w in wires {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

fn relabel(mut gate: Gate, local: &[QubitId], span: Span) -> Gate {
    for q in gate.controls.iter_mut().chain(gate.targets.iter_mut()) {
        *q = local[q.index()];
    }
    gate.span = span;
    gate
}

pub fn resynthesize(program: &mut Program, set: &GateSet, limit: usize, cost: Cost) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut search = Search::new(set, limit, cost);
    let mut replaced = 0;

    for block in &mut program.blocks {
        let mut pending: Vec<Op> = block.ops.drain(..).rev().collect();
        let mut out = Vec::with_capacity(pending.len());
        while let Some(op) = pending.pop() {
            out.push(op);
            if let Some((picked, replacement)) = search.window(&out) {
                replaced += 1;
                for &i in picked.iter().rev() {
                    out.remove(i);
                }
                pending.extend(replacement.into_iter().rev().map(Op::Gate));
            }
        }
        block.ops = out;
    }

    replaced
}

pub fn commute(a: &Gate, b: &Gate) -> bool {
    let wires = distinct(a.wires().chain(b.wires()));
    if wires.len() > 3 {
        return false;
    }
    match (Unitary::gate(a, &wires), Unitary::gate(b, &wires)) {
        (Some(x), Some(y)) => x.after(&y).equals(&y.after(&x)),
        _ => false,
    }
}
