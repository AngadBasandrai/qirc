use std::f64::consts::{FRAC_PI_4, PI};

use crate::ir::*;

const GRID: f64 = 1e-13;
const REACH: usize = 4000;
const TERMS: usize = 4096;
const BULK: usize = 16384;

const OCTANTS: [(i8, i8); 8] = [
    (1, 0),
    (2, 2),
    (0, 1),
    (-2, 2),
    (-1, 0),
    (-2, -2),
    (0, -1),
    (2, -2),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Turn {
    Octant(u8),
    Free(u64, bool),
}

fn octant(angle: f64) -> Option<f64> {
    let steps = (angle / FRAC_PI_4).round();
    (steps.abs() < 1e15 && (angle - steps * FRAC_PI_4).abs() <= GRID * angle.abs().max(1.0))
        .then_some(steps)
}

fn turn(angle: f64) -> Turn {
    match octant(angle) {
        Some(steps) => Turn::Octant((steps as i64).rem_euclid(8) as u8),
        None => Turn::Free(angle.abs().to_bits(), angle < 0.0),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    H(u32),
    X(u32),
    Rz(u32, f64, Turn),
    Cx(u32, u32),
    Wall,
}

impl Kind {
    fn rz(wire: u32, angle: f64) -> Kind {
        Kind::Rz(wire, angle, turn(angle))
    }

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
                Some(angle) if angle.is_finite() => Kind::rz(target, angle),
                _ => Kind::Wall,
            },
            _ => Kind::Wall,
        }
    }

    fn wires(self) -> (u32, Option<u32>) {
        match self {
            Kind::H(q) | Kind::X(q) | Kind::Rz(q, _, _) => (q, None),
            Kind::Cx(c, t) => (c, Some(t)),
            Kind::Wall => (u32::MAX, None),
        }
    }

    fn wide(self) -> bool {
        let (a, b) = self.wires();
        a >= 128 || b.is_some_and(|b| b >= 128)
    }

    fn partner(self, other: Kind) -> bool {
        match (self, other) {
            (Kind::H(a), Kind::H(b))
            | (Kind::X(a), Kind::X(b))
            | (Kind::Rz(a, _, _), Kind::Rz(b, _, _)) => a == b,
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Root {
    whole: i64,
    surd: i64,
    shift: u32,
}

impl Root {
    const ZERO: Root = Root {
        whole: 0,
        surd: 0,
        shift: 0,
    };

    fn int(value: i64) -> Root {
        Root {
            whole: value,
            surd: 0,
            shift: 0,
        }
    }

    fn reduce(mut self) -> Root {
        if self.whole == 0 && self.surd == 0 {
            return Root::ZERO;
        }
        let drop = (self.whole | self.surd).trailing_zeros().min(self.shift);
        self.whole >>= drop;
        self.surd >>= drop;
        self.shift -= drop;
        self
    }

    fn lift(self, shift: u32) -> Option<(i64, i64)> {
        let gap = shift - self.shift;
        if gap == 0 {
            return Some((self.whole, self.surd));
        }
        if gap > 60 {
            return None;
        }
        let factor = 1i64 << gap;
        Some((
            self.whole.checked_mul(factor)?,
            self.surd.checked_mul(factor)?,
        ))
    }

    fn plus(self, other: Root) -> Option<Root> {
        let shift = self.shift.max(other.shift);
        let (a, b) = self.lift(shift)?;
        let (c, d) = other.lift(shift)?;
        Some(
            Root {
                whole: a.checked_add(c)?,
                surd: b.checked_add(d)?,
                shift,
            }
            .reduce(),
        )
    }

    fn negated(self) -> Option<Root> {
        Some(Root {
            whole: self.whole.checked_neg()?,
            surd: self.surd.checked_neg()?,
            shift: self.shift,
        })
    }

    fn halved_root(self) -> Option<Root> {
        Some(
            Root {
                whole: self.surd.checked_mul(2)?,
                surd: self.whole,
                shift: self.shift.checked_add(1)?,
            }
            .reduce(),
        )
    }

    fn scaled(self, factor: i8) -> Option<Root> {
        match factor {
            0 => Some(Root::ZERO),
            1 => Some(self),
            -1 => self.negated(),
            2 => self.halved_root(),
            -2 => self.halved_root()?.negated(),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Power {
    var: u64,
    cos: u32,
    sin: bool,
}

type Mono = Vec<Power>;

fn adjust(mono: &[Power], var: u64, change: impl FnOnce(Power) -> Power) -> Mono {
    let mut out = mono.to_vec();
    match out.binary_search_by_key(&var, |power| power.var) {
        Ok(at) => {
            out[at] = change(out[at]);
            if out[at].cos == 0 && !out[at].sin {
                out.remove(at);
            }
        }
        Err(at) => {
            let power = change(Power {
                var,
                cos: 0,
                sin: false,
            });
            if power.cos != 0 || power.sin {
                out.insert(at, power);
            }
        }
    }
    out
}

#[derive(Clone, PartialEq, Debug)]
struct Poly(Vec<(Mono, Root)>);

impl Poly {
    fn gather(mut items: Vec<(Mono, Root)>) -> Option<Poly> {
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out: Vec<(Mono, Root)> = Vec::with_capacity(items.len());
        for (mono, coefficient) in items {
            match out.last_mut() {
                Some(last) if last.0 == mono => last.1 = last.1.plus(coefficient)?,
                _ => out.push((mono, coefficient)),
            }
        }
        out.retain(|(_, coefficient)| *coefficient != Root::ZERO);
        Some(Poly(out))
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stop {
    Reject,
    Escalate,
}

trait Weight: Clone + PartialEq {
    fn int(value: i64) -> Self;
    fn is_zero(&self) -> bool;
    fn plus(&self, other: &Self) -> Option<Self>;
    fn scaled(&self, factor: i8) -> Option<Self>;
    fn spin(&self, var: u64) -> Result<(Self, Self), Stop>;
    fn bulk(&self) -> usize;
    const SCALAR: bool;
}

impl Weight for Root {
    const SCALAR: bool = true;

    fn int(value: i64) -> Root {
        Root::int(value)
    }

    fn is_zero(&self) -> bool {
        *self == Root::ZERO
    }

    fn plus(&self, other: &Root) -> Option<Root> {
        Root::plus(*self, *other)
    }

    fn scaled(&self, factor: i8) -> Option<Root> {
        Root::scaled(*self, factor)
    }

    fn spin(&self, _: u64) -> Result<(Root, Root), Stop> {
        Err(Stop::Escalate)
    }

    fn bulk(&self) -> usize {
        1
    }
}

impl Weight for Poly {
    const SCALAR: bool = false;

    fn int(value: i64) -> Poly {
        Poly::gather(vec![(Vec::new(), Root::int(value))]).unwrap_or(Poly(Vec::new()))
    }

    fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    fn plus(&self, other: &Poly) -> Option<Poly> {
        Poly::gather(self.0.iter().chain(&other.0).cloned().collect())
    }

    fn scaled(&self, factor: i8) -> Option<Poly> {
        let items = self
            .0
            .iter()
            .map(|(mono, coefficient)| Some((mono.clone(), coefficient.scaled(factor)?)))
            .collect::<Option<Vec<_>>>()?;
        Poly::gather(items)
    }

    fn spin(&self, var: u64) -> Result<(Poly, Poly), Stop> {
        let mut cos = Vec::with_capacity(self.0.len());
        let mut sin = Vec::with_capacity(2 * self.0.len());
        for (mono, coefficient) in &self.0 {
            cos.push((
                adjust(mono, var, |power| Power {
                    cos: power.cos + 1,
                    ..power
                }),
                *coefficient,
            ));
            if mono.iter().any(|power| power.var == var && power.sin) {
                let plain = adjust(mono, var, |power| Power {
                    sin: false,
                    ..power
                });
                let raised = adjust(&plain, var, |power| Power {
                    cos: power.cos + 2,
                    ..power
                });
                sin.push((plain, *coefficient));
                sin.push((raised, coefficient.negated().ok_or(Stop::Reject)?));
            } else {
                sin.push((
                    adjust(mono, var, |power| Power { sin: true, ..power }),
                    *coefficient,
                ));
            }
        }
        let cos = Poly::gather(cos).ok_or(Stop::Reject)?;
        let sin = Poly::gather(sin).ok_or(Stop::Reject)?;
        Ok((cos, sin))
    }

    fn bulk(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, PartialEq, Debug)]
struct Term<W> {
    x: u128,
    z: u128,
    weight: W,
}

fn home<W: Weight>(gate: Kind) -> Option<(Vec<Term<W>>, bool)> {
    let one = |wire: u32| 1u128 << wire;
    let term = |x: u128, z: u128, value: i64| Term {
        x,
        z,
        weight: W::int(value),
    };
    let (mut terms, flip) = match gate {
        Kind::H(q) => (vec![term(one(q), 0, 1), term(0, one(q), 1)], true),
        Kind::X(q) => (vec![term(one(q), 0, 1)], true),
        Kind::Cx(c, t) => (
            vec![
                term(0, 0, 1),
                term(0, one(c), 1),
                term(one(t), 0, 1),
                term(one(t), one(c), -1),
            ],
            true,
        ),
        Kind::Rz(_, _, Turn::Octant(0)) | Kind::Wall => return None,
        Kind::Rz(q, _, turn) => (vec![term(0, one(q), 1)], turn == Turn::Octant(4)),
    };
    tidy(&mut terms).ok()?;
    Some((terms, flip))
}

fn tidy<W: Weight>(terms: &mut Vec<Term<W>>) -> Result<(), Stop> {
    terms.sort_by_key(|term| (term.x, term.z));
    let mut kept = 0;
    for index in 0..terms.len() {
        if kept > 0 && terms[kept - 1].x == terms[index].x && terms[kept - 1].z == terms[index].z {
            let sum = terms[kept - 1]
                .weight
                .plus(&terms[index].weight)
                .ok_or(Stop::Reject)?;
            terms[kept - 1].weight = sum;
        } else {
            terms.swap(kept, index);
            kept += 1;
        }
    }
    terms.truncate(kept);
    terms.retain(|term| !term.weight.is_zero());
    Ok(())
}

fn alike<W: Weight>(held: &[Term<W>], home: &[Term<W>], flip: bool) -> bool {
    if held.len() != home.len() || held.iter().zip(home).any(|(a, b)| a.x != b.x || a.z != b.z) {
        return false;
    }
    held.iter().zip(home).all(|(a, b)| a.weight == b.weight)
        || flip
            && held
                .iter()
                .zip(home)
                .all(|(a, b)| a.weight.plus(&b.weight).is_some_and(|sum| sum.is_zero()))
}

fn sweep<W: Weight>(kinds: &[Kind], from: usize, last: usize) -> Result<Option<usize>, Stop> {
    let gate = kinds[from];
    let Some((home, flip)) = home::<W>(gate) else {
        return Ok(None);
    };
    let mut held = home.clone();
    let mut sorted = true;
    let mut support = held.iter().fold(0u128, |mask, term| mask | term.x | term.z);
    let mut steps = 0;
    for (index, &other) in kinds.iter().enumerate().take(last + 1).skip(from + 1) {
        if other.wide() {
            return Err(Stop::Reject);
        }
        let (a, b) = other.wires();
        let mask = (1u128 << a) | b.map_or(0, |b| 1u128 << b);
        if support & mask == 0 {
            continue;
        }
        if gate.partner(other) {
            if !sorted {
                tidy(&mut held)?;
                sorted = true;
            }
            if alike(&held, &home, flip) {
                return Ok(Some(index));
            }
        }
        steps += 1;
        if steps > REACH {
            return Err(Stop::Reject);
        }
        match other {
            Kind::H(q) => {
                let bit = 1u128 << q;
                for term in &mut held {
                    let (x, z) = (term.x & bit, term.z & bit);
                    if x != 0 && z != 0 {
                        term.weight = term.weight.scaled(-1).ok_or(Stop::Reject)?;
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
                        term.weight = term.weight.scaled(-1).ok_or(Stop::Reject)?;
                    }
                }
            }
            Kind::Cx(c, t) => {
                let (control, target) = (1u128 << c, 1u128 << t);
                for term in &mut held {
                    let (xc, zc) = (term.x & control != 0, term.z & control != 0);
                    let (xt, zt) = (term.x & target != 0, term.z & target != 0);
                    if xc && zt && (xt == zc) {
                        term.weight = term.weight.scaled(-1).ok_or(Stop::Reject)?;
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
            Kind::Rz(q, _, turn) => {
                let bit = 1u128 << q;
                let count = held.len();
                for index in 0..count {
                    let term = &held[index];
                    if term.x & bit == 0 {
                        continue;
                    }
                    let (x, z) = (term.x, term.z);
                    let sign: i8 = if z & bit == 0 { 1 } else { -1 };
                    let (stay, moved) = match turn {
                        Turn::Octant(step) => {
                            let (cos, sin) = OCTANTS[step as usize];
                            let stay = term.weight.scaled(cos).ok_or(Stop::Reject)?;
                            let moved = term.weight.scaled(sin * sign).ok_or(Stop::Reject)?;
                            (stay, moved)
                        }
                        Turn::Free(var, negative) => {
                            let (stay, moved) = term.weight.spin(var)?;
                            let factor = if negative == (sign == 1) { -1 } else { 1 };
                            (stay, moved.scaled(factor).ok_or(Stop::Reject)?)
                        }
                    };
                    match (stay.is_zero(), moved.is_zero()) {
                        (_, true) => held[index].weight = stay,
                        (true, false) => {
                            held[index].weight = moved;
                            held[index].z = z ^ bit;
                            sorted = false;
                        }
                        (false, false) => {
                            held[index].weight = stay;
                            held.push(Term {
                                x,
                                z: z ^ bit,
                                weight: moved,
                            });
                        }
                    }
                }
                if held.len() != count {
                    tidy(&mut held)?;
                    sorted = true;
                }
            }
            Kind::Wall => return Err(Stop::Reject),
        }
        if held.len() > TERMS
            || !W::SCALAR && held.iter().map(|term| term.weight.bulk()).sum::<usize>() > BULK
        {
            return Err(Stop::Reject);
        }
        support = held.iter().fold(0u128, |mask, term| mask | term.x | term.z);
    }
    Ok(None)
}

fn strings(kinds: &[Kind], from: usize, last: usize) -> Option<usize> {
    if kinds[from].wide() {
        return None;
    }
    match sweep::<Root>(kinds, from, last) {
        Ok(hit) => hit,
        Err(Stop::Escalate) => sweep::<Poly>(kinds, from, last).ok().flatten(),
        Err(Stop::Reject) => None,
    }
}

fn settle(angle: f64) -> Option<f64> {
    let turned = angle - 2.0 * PI * (angle / (2.0 * PI)).round();
    let snapped = octant(turned).map_or(turned, |steps| steps * FRAC_PI_4);
    (snapped != 0.0).then_some(snapped)
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
            (Kind::Rz(wire, a, _), Kind::Rz(_, b, _)) => settle(a + b).map(|sum| (wire, sum)),
            _ => None,
        };
        match merged {
            Some((wire, sum)) => {
                kinds[to] = Kind::rz(wire, sum);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::matrix::C64;
    use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, SQRT_2};

    const WIDTH: usize = 4;
    const T: f64 = FRAC_PI_4;

    fn value(root: Root) -> f64 {
        (root.whole as f64 + root.surd as f64 * SQRT_2) / 2f64.powi(root.shift as i32)
    }

    fn root(whole: i64, surd: i64, shift: u32) -> Root {
        Root { whole, surd, shift }
    }

    fn apply(state: &mut [C64], kind: Kind) {
        let dim = state.len();
        match kind {
            Kind::H(q) => {
                for i in 0..dim {
                    if i >> q & 1 == 0 {
                        let (a, b) = (state[i], state[i | 1 << q]);
                        state[i] = (a + b) * FRAC_1_SQRT_2;
                        state[i | 1 << q] = (a - b) * FRAC_1_SQRT_2;
                    }
                }
            }
            Kind::X(q) => {
                for i in 0..dim {
                    if i >> q & 1 == 0 {
                        state.swap(i, i | 1 << q);
                    }
                }
            }
            Kind::Rz(q, angle, _) => {
                for (i, amplitude) in state.iter_mut().enumerate() {
                    let half = if i >> q & 1 == 0 {
                        -angle / 2.0
                    } else {
                        angle / 2.0
                    };
                    *amplitude *= C64::new(half.cos(), half.sin());
                }
            }
            Kind::Cx(c, t) => {
                for i in 0..dim {
                    if i >> c & 1 == 1 && i >> t & 1 == 0 {
                        state.swap(i, i | 1 << t);
                    }
                }
            }
            Kind::Wall => unreachable!(),
        }
    }

    fn unitary(kinds: &[Kind]) -> Vec<C64> {
        let dim = 1 << WIDTH;
        let mut columns = Vec::with_capacity(dim * dim);
        for basis in 0..dim {
            let mut state = vec![C64::new(0.0, 0.0); dim];
            state[basis] = C64::new(1.0, 0.0);
            for &kind in kinds {
                apply(&mut state, kind);
            }
            columns.extend(state);
        }
        columns
    }

    fn rewritten(kinds: &[Kind], to: usize) -> Vec<Kind> {
        let mut out = kinds[1..to].to_vec();
        if let (Kind::Rz(q, a, _), Kind::Rz(_, b, _)) = (kinds[0], kinds[to]) {
            out.push(Kind::rz(q, a + b));
        }
        out
    }

    fn equal_up_to_sign(a: &[C64], b: &[C64]) -> bool {
        let gap = |sign: f64| {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y * sign).norm())
                .fold(0.0, f64::max)
        };
        gap(1.0) < 1e-9 || gap(-1.0) < 1e-9
    }

    fn verdict(kinds: &[Kind]) -> Option<usize> {
        let to = strings(kinds, 0, kinds.len() - 1)?;
        assert!(
            equal_up_to_sign(&unitary(&kinds[..=to]), &unitary(&rewritten(kinds, to))),
            "oracle accepted a pair that does not commute: {kinds:?}"
        );
        Some(to)
    }

    fn commutes(kinds: &[Kind]) -> bool {
        let to = kinds.len() - 1;
        equal_up_to_sign(&unitary(kinds), &unitary(&rewritten(kinds, to)))
    }

    fn inverse(kinds: &[Kind]) -> Vec<Kind> {
        kinds
            .iter()
            .rev()
            .map(|&kind| match kind {
                Kind::Rz(q, angle, _) => Kind::rz(q, -angle),
                other => other,
            })
            .collect()
    }

    fn toffoli(a: u32, b: u32, c: u32) -> Vec<Kind> {
        vec![
            Kind::H(c),
            Kind::Cx(b, c),
            Kind::rz(c, -T),
            Kind::Cx(a, c),
            Kind::rz(c, T),
            Kind::Cx(b, c),
            Kind::rz(c, -T),
            Kind::Cx(a, c),
            Kind::rz(b, T),
            Kind::rz(c, T),
            Kind::H(c),
            Kind::Cx(a, b),
            Kind::rz(a, T),
            Kind::rz(b, -T),
            Kind::Cx(a, b),
        ]
    }

    fn sandwich(gate: Kind, middle: &[Kind]) -> Vec<Kind> {
        let mut kinds = vec![gate];
        kinds.extend_from_slice(middle);
        kinds.push(gate);
        kinds
    }

    #[test]
    fn root_arithmetic_is_exact() {
        let one = Root::int(1);
        let r = one.halved_root().unwrap();
        assert_eq!(r, root(0, 1, 1));
        assert_eq!(r.halved_root().unwrap(), root(1, 0, 1));
        assert_eq!(r.plus(r).unwrap(), root(0, 1, 0));
        assert_eq!(r.plus(r.negated().unwrap()).unwrap(), Root::ZERO);
        let mut power = one;
        for _ in 0..8 {
            power = power.halved_root().unwrap();
        }
        assert_eq!(power, root(1, 0, 4));
        let mut sum = Root::ZERO;
        for _ in 0..16 {
            sum = sum.plus(power).unwrap();
        }
        assert_eq!(sum, one);
        assert_eq!(root(6, -4, 3).reduce(), root(3, -2, 2));
        assert_eq!(root(0, 0, 9).reduce(), Root::ZERO);
        assert_eq!(root(2, 2, 1).reduce(), root(1, 1, 0));
        let cos8 = one
            .plus(r)
            .unwrap()
            .halved_root()
            .unwrap()
            .halved_root()
            .unwrap();
        assert!((value(cos8) - (std::f64::consts::PI / 8.0).cos().powi(2)).abs() < 1e-15);
        for factor in [0i8, 1, -1, 2, -2] {
            let scaled = root(3, -5, 2).scaled(factor).unwrap();
            let expected = value(root(3, -5, 2))
                * match factor {
                    0 => 0.0,
                    1 => 1.0,
                    -1 => -1.0,
                    2 => FRAC_1_SQRT_2,
                    _ => -FRAC_1_SQRT_2,
                };
            assert!((value(scaled) - expected).abs() < 1e-12);
        }
        assert!(Root::int(i64::MAX).plus(Root::int(1)).is_none());
        assert!(Root::int(1).plus(root(1, 0, 65)).is_none());
        assert!(Root::int(i64::MIN).negated().is_none());
        assert_eq!(Root::int(1).spin(3), Err(Stop::Escalate));
    }

    #[test]
    fn trig_polynomials_obey_the_circle() {
        let one = Poly::int(1);
        let (c, s) = one.spin(7).unwrap();
        let (cc, cs) = c.spin(7).unwrap();
        let (sc, ss) = s.spin(7).unwrap();
        assert_eq!(cc.plus(&ss).unwrap(), one);
        assert_eq!(cs, sc);
        assert_ne!(cc, ss);
        assert!(cs.plus(&sc.scaled(-1).unwrap()).unwrap().is_zero());
        let (_, other) = one.spin(8).unwrap();
        assert_ne!(other, s);
        assert_eq!(Poly::int(0), Poly(Vec::new()));
    }

    #[test]
    fn angles_snap_only_to_exact_octants() {
        assert_eq!(turn(T), Turn::Octant(1));
        assert_eq!(turn(-T), Turn::Octant(7));
        assert_eq!(turn(5.0 * std::f64::consts::PI / 2.0), Turn::Octant(2));
        assert_eq!(turn(0.0), Turn::Octant(0));
        assert!(matches!(turn(1e-12), Turn::Free(_, false)));
        assert!(matches!(turn(T + 1e-12), Turn::Free(_, false)));
        assert!(matches!(turn(-0.3), Turn::Free(_, true)));
        assert_eq!(turn(0.3), Turn::Free(0.3f64.to_bits(), false));
        assert_eq!(settle(T + T), Some(FRAC_PI_2));
        assert_eq!(settle(0.3 - 0.3), None);
        assert_eq!(settle(1e-12), Some(1e-12));
    }

    #[test]
    fn accepts_toffoli_ladder_commutation() {
        let ladder = toffoli(0, 1, 2);
        let last = ladder.len() + 1;
        for gate in [Kind::Cx(0, 2), Kind::Cx(1, 2), Kind::X(2), Kind::Cx(0, 3)] {
            assert_eq!(verdict(&sandwich(gate, &ladder)), Some(last), "{gate:?}");
        }
        assert_eq!(verdict(&sandwich(Kind::rz(0, 0.3), &ladder)), Some(13));
        assert_eq!(verdict(&sandwich(Kind::rz(1, T), &ladder)), Some(9));
        for gate in [
            Kind::H(0),
            Kind::Cx(2, 0),
            Kind::X(0),
            Kind::H(1),
            Kind::Cx(2, 3),
        ] {
            let kinds = sandwich(gate, &ladder);
            assert_eq!(verdict(&kinds), None, "{gate:?}");
            assert!(!commutes(&kinds), "{gate:?}");
        }
    }

    #[test]
    fn accepts_compute_uncompute_sandwich() {
        let compute = vec![
            Kind::rz(0, T),
            Kind::Cx(0, 1),
            Kind::H(1),
            Kind::rz(1, 3.0 * T),
            Kind::Cx(1, 2),
            Kind::rz(2, -T),
            Kind::H(2),
            Kind::rz(2, T),
        ];
        let work = vec![Kind::H(3), Kind::rz(3, 0.7), Kind::H(3)];
        let mut middle = compute.clone();
        middle.extend(&work);
        middle.extend(inverse(&compute));
        assert_eq!(
            verdict(&sandwich(Kind::H(0), &middle)),
            Some(middle.len() + 1)
        );
        let mut generic = compute.clone();
        generic[0] = Kind::rz(0, 0.37);
        generic[3] = Kind::rz(1, -1.1);
        let mut middle = generic.clone();
        middle.extend(&work);
        middle.extend(inverse(&generic));
        assert_eq!(
            verdict(&sandwich(Kind::H(0), &middle)),
            Some(middle.len() + 1)
        );
        let mut middle = generic.clone();
        middle.extend(&work);
        middle.extend(&inverse(&generic)[..generic.len() - 1]);
        let kinds = sandwich(Kind::H(0), &middle);
        assert_eq!(verdict(&kinds), None);
        assert!(!commutes(&kinds));
    }

    #[test]
    fn exact_t_cycles_return_home() {
        let mut kinds = vec![Kind::X(0)];
        kinds.extend(std::iter::repeat_n(Kind::rz(0, T), 8));
        kinds.push(Kind::X(0));
        assert_eq!(verdict(&kinds), Some(9));
        kinds[4] = Kind::rz(0, T + 1e-12);
        assert_eq!(verdict(&kinds), None);
    }

    #[test]
    fn rejects_near_miss_blocks() {
        let block = |eps: f64| {
            vec![
                Kind::H(0),
                Kind::rz(0, FRAC_PI_2),
                Kind::H(0),
                Kind::rz(0, eps),
                Kind::H(0),
                Kind::rz(0, -FRAC_PI_2),
                Kind::H(0),
            ]
        };
        assert_eq!(verdict(&block(0.0)), Some(6));
        for eps in [1e-12, -1e-12, 1e-11, 1e-9, 0.25] {
            assert_eq!(verdict(&block(eps)), None, "{eps}");
        }
        let near = vec![Kind::X(0), Kind::rz(0, 1e-12), Kind::X(0)];
        assert_eq!(verdict(&near), None);
        let pair = |eps: f64| {
            vec![
                Kind::X(1),
                Kind::rz(1, 0.4),
                Kind::Cx(0, 1),
                Kind::rz(1, -0.4 + eps),
                Kind::Cx(0, 1),
                Kind::X(1),
            ]
        };
        assert_eq!(verdict(&pair(1e-12)), None);
    }

    #[test]
    fn random_blocks_are_sound() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let angles = [T, -T, 2.0 * T, 3.0 * T, -3.0 * T, 4.0 * T, 0.3, -0.3, 1.1];
        let mut accepted = 0;
        let mut generic = 0;
        for round in 0..3000 {
            let gate = |next: &mut dyn FnMut() -> u64| {
                let q = (next() % WIDTH as u64) as u32;
                let other = (q + 1 + (next() % (WIDTH as u64 - 1)) as u32) % WIDTH as u32;
                match next() % 7 {
                    0 | 1 => Kind::H(q),
                    2 => Kind::X(q),
                    3 | 4 => Kind::rz(q, angles[(next() % angles.len() as u64) as usize]),
                    _ => Kind::Cx(q, other),
                }
            };
            let head = gate(&mut next);
            let length = 2 + (next() % 6) as usize;
            let outer: Vec<Kind> = (0..length).map(|_| gate(&mut next)).collect();
            let inner: Vec<Kind> = (0..1 + next() % 3).map(|_| gate(&mut next)).collect();
            let mut middle = outer.clone();
            middle.extend(&inner);
            if round % 2 == 0 {
                middle.extend(inverse(&outer));
            }
            let tail = match head {
                Kind::Rz(q, _, _) => Kind::rz(q, angles[(next() % angles.len() as u64) as usize]),
                other => other,
            };
            let mut kinds = vec![head];
            kinds.extend(middle);
            kinds.push(tail);
            if verdict(&kinds).is_some() {
                accepted += 1;
                if kinds
                    .iter()
                    .any(|kind| matches!(kind, Kind::Rz(_, _, Turn::Free(..))))
                {
                    generic += 1;
                }
            }
        }
        assert!(accepted > 300, "{accepted}");
        assert!(generic > 50, "{generic}");
    }
}
