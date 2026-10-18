use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_4, PI};

use crate::ir::*;
use crate::simulator::matrix::C64;

const SNAP: f64 = 1e-11;
const EXACT: f64 = 1e-10;
const FAINT: f64 = 1e-13;
const REACH: usize = 4000;
const TERMS: usize = 4096;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    H(u32),
    X(u32),
    Rz(u32, f64),
    Cx(u32, u32),
    Wall,
}

impl Kind {
    fn of(op: &Op) -> Kind {
        let Op::Gate(gate) = op else {
            return Kind::Wall;
        };
        if !gate.has_valid_shape() || gate.targets.len() != 1 {
            return Kind::Wall;
        }
        let target = gate.targets[0].0;
        match (gate.kind, gate.controls.as_slice()) {
            (GateKind::H, []) => Kind::H(target),
            (GateKind::X, []) => Kind::X(target),
            (GateKind::X, [control]) if control.0 != target => Kind::Cx(control.0, target),
            (GateKind::Rz, []) => match gate.constant_angle() {
                Some(angle) if angle.is_finite() => Kind::Rz(target, angle),
                _ => Kind::Wall,
            },
            _ => Kind::Wall,
        }
    }

    fn wires(self) -> (u32, Option<u32>) {
        match self {
            Kind::H(q) | Kind::X(q) | Kind::Rz(q, _) => (q, None),
            Kind::Cx(c, t) => (c, Some(t)),
            Kind::Wall => (u32::MAX, None),
        }
    }

    fn partner(self, other: Kind) -> bool {
        match (self, other) {
            (Kind::H(a), Kind::H(b))
            | (Kind::X(a), Kind::X(b))
            | (Kind::Rz(a, _), Kind::Rz(b, _)) => a == b,
            (Kind::Cx(a, b), Kind::Cx(c, d)) => a == c && b == d,
            _ => false,
        }
    }
}

fn horizon(kinds: &[Kind], from: usize) -> Option<usize> {
    let gate = kinds[from];
    let mut last = None;
    for (offset, &other) in kinds[from + 1..].iter().enumerate() {
        if other == Kind::Wall {
            break;
        }
        if gate.partner(other) {
            last = Some(from + 1 + offset);
        }
    }
    last
}

#[derive(Clone, Copy)]
struct Term {
    x: u128,
    z: u128,
    weight: C64,
}

fn expand(gate: Kind) -> Vec<Term> {
    let one = |wire: u32| 1u128 << wire;
    let term = |x: u128, z: u128, re: f64, im: f64| Term {
        x,
        z,
        weight: C64::new(re, im),
    };
    let mut terms = match gate {
        Kind::H(q) => vec![
            term(one(q), 0, FRAC_1_SQRT_2, 0.0),
            term(0, one(q), FRAC_1_SQRT_2, 0.0),
        ],
        Kind::X(q) => vec![term(one(q), 0, 1.0, 0.0)],
        Kind::Rz(q, angle) => vec![
            term(0, 0, (angle / 2.0).cos(), 0.0),
            term(0, one(q), 0.0, -(angle / 2.0).sin()),
        ],
        Kind::Cx(c, t) => vec![
            term(0, 0, 0.5, 0.0),
            term(0, one(c), 0.5, 0.0),
            term(one(t), 0, 0.5, 0.0),
            term(one(t), one(c), -0.5, 0.0),
        ],
        Kind::Wall => Vec::new(),
    };
    tidy(&mut terms);
    terms
}

fn tidy(terms: &mut Vec<Term>) {
    terms.sort_unstable_by_key(|term| (term.x, term.z));
    let mut kept = 0;
    for index in 0..terms.len() {
        if kept > 0 && terms[kept - 1].x == terms[index].x && terms[kept - 1].z == terms[index].z {
            let weight = terms[index].weight;
            terms[kept - 1].weight += weight;
        } else {
            terms[kept] = terms[index];
            kept += 1;
        }
    }
    terms.truncate(kept);
    terms.retain(|term| term.weight.norm_sqr() > FAINT * FAINT);
}

fn alike(held: &[Term], home: &[Term]) -> bool {
    if held.len() != home.len() {
        return false;
    }
    let (mut same, mut flipped) = (true, true);
    for (a, b) in held.iter().zip(home) {
        if a.x != b.x || a.z != b.z {
            return false;
        }
        same &= (a.weight - b.weight).norm_sqr() < EXACT * EXACT;
        flipped &= (a.weight + b.weight).norm_sqr() < EXACT * EXACT;
    }
    same || flipped
}

fn strings(kinds: &[Kind], from: usize, last: usize) -> Option<usize> {
    let gate = kinds[from];
    let wide = |kind: Kind| {
        let (a, b) = kind.wires();
        a >= 128 || b.is_some_and(|b| b >= 128)
    };
    if wide(gate) {
        return None;
    }
    let home = expand(gate);
    let mut held = home.clone();
    let mut sorted = true;
    let mut support = held.iter().fold(0u128, |mask, term| mask | term.x | term.z);
    let mut steps = 0;
    for (index, &other) in kinds.iter().enumerate().take(last + 1).skip(from + 1) {
        if wide(other) {
            return None;
        }
        let (a, b) = other.wires();
        let mask = (1u128 << a) | b.map_or(0, |b| 1u128 << b);
        if support & mask == 0 {
            continue;
        }
        if gate.partner(other) {
            if !sorted {
                tidy(&mut held);
                sorted = true;
            }
            if alike(&held, &home) {
                return Some(index);
            }
        }
        steps += 1;
        if steps > REACH {
            return None;
        }
        match other {
            Kind::H(q) => {
                let bit = 1u128 << q;
                for term in &mut held {
                    let (x, z) = (term.x & bit, term.z & bit);
                    if x != 0 && z != 0 {
                        term.weight = -term.weight;
                    }
                    term.x = (term.x & !bit) | z;
                    term.z = (term.z & !bit) | x;
                }
                sorted = false;
            }
            Kind::X(q) => {
                let bit = 1u128 << q;
                for term in &mut held {
                    if term.z & bit != 0 {
                        term.weight = -term.weight;
                    }
                }
            }
            Kind::Cx(c, t) => {
                let (control, target) = (1u128 << c, 1u128 << t);
                for term in &mut held {
                    let (xc, zc) = (term.x & control != 0, term.z & control != 0);
                    let (xt, zt) = (term.x & target != 0, term.z & target != 0);
                    if xc && zt && (xt == zc) {
                        term.weight = -term.weight;
                    }
                    if xc {
                        term.x ^= target;
                    }
                    if zt {
                        term.z ^= control;
                    }
                }
                sorted = false;
            }
            Kind::Rz(q, angle) => {
                let bit = 1u128 << q;
                let (sin, cos) = angle.sin_cos();
                let count = held.len();
                for index in 0..count {
                    let term = held[index];
                    if term.x & bit == 0 {
                        continue;
                    }
                    let turn = if term.z & bit == 0 { sin } else { -sin };
                    held[index].weight = term.weight * cos;
                    held.push(Term {
                        x: term.x,
                        z: term.z ^ bit,
                        weight: term.weight * turn,
                    });
                }
                if held.len() != count {
                    tidy(&mut held);
                    sorted = true;
                }
            }
            Kind::Wall => return None,
        }
        if held.len() > TERMS {
            return None;
        }
        support = held.iter().fold(0u128, |mask, term| mask | term.x | term.z);
    }
    None
}

fn settle(angle: f64) -> Option<f64> {
    let turned = angle - 2.0 * PI * (angle / (2.0 * PI)).round();
    let steps = (turned / FRAC_PI_4).round();
    let snapped = if (turned - steps * FRAC_PI_4).abs() < SNAP {
        steps * FRAC_PI_4
    } else {
        turned
    };
    (snapped.abs() >= SNAP).then_some(snapped)
}

fn pass(ops: &mut Vec<Op>) -> usize {
    let mut kinds: Vec<Kind> = ops.iter().map(Kind::of).collect();
    let mut saved = 0;
    let mut from = 0;
    while from < kinds.len() {
        let gate = kinds[from];
        let hit = (gate != Kind::Wall)
            .then(|| horizon(&kinds, from))
            .flatten()
            .and_then(|last| strings(&kinds, from, last));
        let Some(to) = hit else {
            from += 1;
            continue;
        };
        let merged = match (gate, kinds[to]) {
            (Kind::Rz(wire, a), Kind::Rz(_, b)) => settle(a + b).map(|sum| (wire, sum)),
            _ => None,
        };
        match merged {
            Some((wire, sum)) => {
                kinds[to] = Kind::Rz(wire, sum);
                if let Op::Gate(held) = &mut ops[to] {
                    held.params = vec![Operand::Const(Const::Float(sum))];
                }
                saved += 1;
            }
            None => {
                kinds.remove(to);
                ops.remove(to);
                saved += 2;
            }
        }
        kinds.remove(from);
        ops.remove(from);
        from = from.saturating_sub(1);
    }
    saved
}

pub fn cancel(program: &mut Program) -> usize {
    let mut saved = 0;
    for block in &mut program.blocks {
        for _ in 0..4 {
            let cut = pass(&mut block.ops);
            saved += cut;
            if cut == 0 {
                break;
            }
        }
    }
    saved
}
