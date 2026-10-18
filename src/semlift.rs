use std::array;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::f64::consts::FRAC_PI_4;
use std::mem;

use crate::diag::Span;
use crate::ir::*;
use crate::phase;

const EPSILON: f64 = 1e-9;

const REACH: usize = 48;
const DEBT: usize = 4;

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[derive(Clone, Copy)]
struct Value {
    form: usize,
    flip: bool,
}

struct Held {
    wire: usize,
    start: usize,
    end: usize,
    flip: bool,
}

struct Turn {
    at: usize,
    form: usize,
    sign: f64,
    angle: f64,
}

struct Forms {
    keys: Vec<Vec<u32>>,
    hashes: Vec<u64>,
    ids: HashMap<Vec<u32>, usize>,
    by_hash: HashMap<u64, usize>,
}

impl Forms {
    fn intern(&mut self, key: Vec<u32>) -> usize {
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let hash = key.iter().fold(0, |h, &v| h ^ mix(u64::from(v)));
        let id = self.keys.len();
        self.keys.push(key.clone());
        self.hashes.push(hash);
        self.ids.insert(key, id);
        self.by_hash.entry(hash).or_insert(id);
        id
    }

    fn sum(&self, a: usize, b: usize) -> Option<usize> {
        let found = *self.by_hash.get(&(self.hashes[a] ^ self.hashes[b]))?;
        (phase::xor(&self.keys[a], &self.keys[b]) == self.keys[found]).then_some(found)
    }
}

struct Sweep {
    forms: Forms,
    held: Vec<Held>,
    holders: HashMap<usize, Vec<usize>>,
    turns: Vec<Turn>,
    by_form: HashMap<usize, Vec<usize>>,
}

struct Track {
    forms: Forms,
    fresh: u32,
    held: Vec<Held>,
    holders: HashMap<usize, Vec<usize>>,
    current: Vec<usize>,
    values: Vec<Value>,
    end: usize,
}

impl Track {
    fn set(&mut self, wire: usize, value: Value, start: usize) {
        if let Some(&last) = self.current.get(wire) {
            self.held[last].end = start;
        }
        let id = self.held.len();
        self.held.push(Held {
            wire,
            start,
            end: self.end,
            flip: value.flip,
        });
        self.holders.entry(value.form).or_default().push(id);
        if wire < self.values.len() {
            self.values[wire] = value;
            self.current[wire] = id;
        } else {
            self.values.push(value);
            self.current.push(id);
        }
    }

    fn renew(&mut self, wire: usize, start: usize) {
        let form = self.forms.intern(vec![self.fresh]);
        self.fresh += 1;
        self.set(wire, Value { form, flip: false }, start);
    }
}

fn sweep(ops: &[Op], qubits: usize) -> Sweep {
    let mut track = Track {
        forms: Forms {
            keys: Vec::new(),
            hashes: Vec::new(),
            ids: HashMap::new(),
            by_hash: HashMap::new(),
        },
        fresh: 0,
        held: Vec::new(),
        holders: HashMap::new(),
        current: Vec::with_capacity(qubits),
        values: Vec::with_capacity(qubits),
        end: ops.len() + 1,
    };
    for wire in 0..qubits {
        track.renew(wire, 0);
    }

    let mut turns = Vec::new();
    let mut by_form: HashMap<usize, Vec<usize>> = HashMap::new();

    for (index, op) in ops.iter().enumerate() {
        let after = index + 1;
        let tracked = match op {
            Op::Gate(gate) => match (
                phase::slope(gate),
                gate.kind,
                &gate.controls[..],
                &gate.targets[..],
            ) {
                (Some(slope), ..) => {
                    let value = track.values[gate.targets[0].index()];
                    if !track.forms.keys[value.form].is_empty() {
                        let sign = if value.flip { -1.0 } else { 1.0 };
                        by_form.entry(value.form).or_default().push(turns.len());
                        turns.push(Turn {
                            at: index,
                            form: value.form,
                            sign,
                            angle: sign * slope,
                        });
                    }
                    true
                }
                (None, kind, ..) if phase::diagonal(kind) => true,
                (None, GateKind::X, [], [t]) => {
                    let mut value = track.values[t.index()];
                    value.flip ^= true;
                    track.set(t.index(), value, after);
                    true
                }
                (None, GateKind::X, [c], [t]) => {
                    let control = track.values[c.index()];
                    let target = track.values[t.index()];
                    let key = phase::xor(
                        &track.forms.keys[target.form],
                        &track.forms.keys[control.form],
                    );
                    let value = Value {
                        form: track.forms.intern(key),
                        flip: target.flip ^ control.flip,
                    };
                    track.set(t.index(), value, after);
                    true
                }
                (None, GateKind::X, [_, _], [t]) | (None, GateKind::H, [], [t]) => {
                    track.renew(t.index(), after);
                    true
                }
                (None, GateKind::Swap, [], [a, b]) => {
                    let (x, y) = (track.values[a.index()], track.values[b.index()]);
                    track.set(a.index(), y, after);
                    track.set(b.index(), x, after);
                    true
                }
                _ => false,
            },
            _ => op.qubits().is_empty(),
        };
        if !tracked {
            for wire in 0..qubits {
                track.renew(wire, after);
            }
        }
    }

    Sweep {
        forms: track.forms,
        held: track.held,
        holders: track.holders,
        turns,
        by_form,
    }
}

fn eighths(angle: f64) -> Option<i64> {
    let steps = phase::wrap(angle) / FRAC_PI_4;
    let near = steps.round();
    ((steps - near).abs() < EPSILON).then_some(near as i64)
}

fn odd(angle: f64) -> bool {
    eighths(angle).is_some_and(|steps| steps.rem_euclid(2) == 1)
}

fn nearest(sweep: &Sweep, form: usize, anchor: usize) -> Option<usize> {
    sweep
        .by_form
        .get(&form)?
        .iter()
        .copied()
        .filter(|&turn| odd(sweep.turns[turn].angle))
        .min_by_key(|&turn| turn.abs_diff(anchor))
}

struct Spot {
    time: usize,
    wires: [usize; 3],
    weights: [f64; 7],
}

fn independent(a: usize, b: usize, c: usize) -> bool {
    a != 0 && b != 0 && a != b && c != 0 && c != a && c != b && c != a ^ b
}

type Trio = [(usize, usize, bool); 3];

fn weigh(trio: &Trio) -> [f64; 7] {
    let mut weights = [0.0; 7];
    for subset in 1..8usize {
        let mut mask = 0;
        let mut flip = false;
        for (slot, item) in trio.iter().enumerate() {
            if subset >> slot & 1 == 1 {
                mask ^= item.1;
                flip ^= item.2;
            }
        }
        let weight = if subset.count_ones() % 2 == 1 {
            1.0
        } else {
            -1.0
        };
        weights[mask - 1] = if flip { -weight } else { weight };
    }
    weights
}

fn cleared(angles: &[f64; 7], weights: &[f64; 7]) -> usize {
    [1.0, -1.0]
        .map(|sign| {
            angles
                .iter()
                .zip(weights)
                .filter(|(angle, weight)| zero(**angle - sign * FRAC_PI_4 * **weight))
                .count()
        })
        .into_iter()
        .max()
        .unwrap_or(0)
}

fn place(
    sweep: &Sweep,
    space: &[Option<usize>; 7],
    sizes: &[usize; 7],
    angles: Option<&[f64; 7]>,
    (lo, hi): (usize, usize),
) -> Option<Spot> {
    let mut items: Vec<(usize, usize, usize, usize, bool)> = Vec::new();
    for (slot, form) in space.iter().enumerate() {
        let Some(form) = form else {
            continue;
        };
        for &id in sweep.holders.get(form).into_iter().flatten() {
            let held = &sweep.held[id];
            if held.start < held.end {
                items.push((held.start, held.end, held.wire, slot + 1, held.flip));
            }
        }
    }
    let mut times: Vec<usize> = items.iter().flat_map(|item| [item.0, item.1]).collect();
    times.sort_unstable();
    times.dedup();

    let mut segments: Vec<(usize, usize, (usize, usize), Trio)> = Vec::new();
    for pair in times.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let active: Vec<(usize, usize, bool)> = items
            .iter()
            .filter(|item| item.0 <= from && from < item.1)
            .map(|item| (item.2, item.3, item.4))
            .collect();
        let mut best: Option<((usize, usize), Trio)> = None;
        for i in 0..active.len() {
            for j in i + 1..active.len() {
                for k in j + 1..active.len() {
                    let trio = [active[i], active[j], active[k]];
                    if !independent(trio[0].1, trio[1].1, trio[2].1) {
                        continue;
                    }
                    let size: usize = trio.iter().map(|t| sizes[t.1 - 1]).sum();
                    let score = (
                        size,
                        angles.map_or(0, |angles| 7 - cleared(angles, &weigh(&trio))),
                    );
                    if best.is_none_or(|(held, _)| score < held) {
                        best = Some((score, trio));
                    }
                }
            }
        }
        if let Some((score, trio)) = best {
            segments.push((from, to, score, trio));
        }
    }

    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if segments[run.1 - 1].1 == segment.0 => run.1 = index + 1,
            _ => runs.push((index, index + 1)),
        }
    }
    let distance = |run: &(usize, usize)| {
        let (from, to) = (segments[run.0].0, segments[run.1 - 1].1);
        if to <= lo {
            lo - to + 1
        } else {
            from.saturating_sub(hi)
        }
    };
    let chosen = runs
        .iter()
        .flat_map(|run| (run.0..run.1).map(move |index| (index, distance(run))))
        .rev()
        .min_by_key(|&(index, distance)| (distance, segments[index].2))
        .map(|(index, _)| &segments[index])?;

    let trio = chosen.3;
    Some(Spot {
        time: chosen.1 - 1,
        wires: trio.map(|item| item.0),
        weights: weigh(&trio),
    })
}

fn spaces(sweep: &Sweep, anchor: usize, reach: usize) -> Vec<[usize; 7]> {
    let u = sweep.turns[anchor].form;
    let low = anchor.saturating_sub(reach);
    let high = (anchor + reach + 1).min(sweep.turns.len());
    let mut near: Vec<usize> = Vec::new();
    for turn in low..high {
        let form = sweep.turns[turn].form;
        if form != u && odd(sweep.turns[turn].angle) && !near.contains(&form) {
            near.push(form);
        }
    }
    let partners: Vec<(usize, usize)> = near
        .iter()
        .filter_map(|&v| Some((v, sweep.forms.sum(u, v)?)))
        .filter(|&(_, uv)| nearest(sweep, uv, anchor).is_some())
        .collect();
    let mut found = Vec::new();
    for (i, &(v1, uv1)) in partners.iter().enumerate() {
        for &(v2, uv2) in &partners[i + 1..] {
            if v2 == uv1 {
                continue;
            }
            let Some(v12) = sweep.forms.sum(v1, v2) else {
                continue;
            };
            let Some(all) = sweep.forms.sum(u, v12) else {
                continue;
            };
            found.push([u, v1, uv1, v2, uv2, v12, all]);
        }
    }
    found
}

fn zero(angle: f64) -> bool {
    phase::wrap(angle).abs() < EPSILON
}

#[derive(Clone, Copy)]
enum Slot {
    Turn(usize),
    Debt(u64),
}

enum Insert {
    Triple([usize; 3]),
    Rotation(usize, f64),
}

struct Lift {
    sweep: Sweep,
    debts: HashMap<u64, (Vec<u32>, f64)>,
    inserted: Vec<(usize, Insert)>,
}

impl Lift {
    fn angle(&self, slot: Slot) -> f64 {
        match slot {
            Slot::Turn(turn) => self.sweep.turns[turn].angle,
            Slot::Debt(hash) => self.debts.get(&hash).map_or(0.0, |debt| debt.1),
        }
    }

    fn span(&self, slots: &[Slot; 7], anchor: usize) -> (usize, usize) {
        slots.iter().fold((usize::MAX, 0), |(lo, hi), slot| {
            let at = match slot {
                Slot::Turn(turn) => self.sweep.turns[*turn].at,
                Slot::Debt(_) => self.sweep.turns[anchor].at,
            };
            (lo.min(at), hi.max(at + 1))
        })
    }

    fn slot(&self, form: Option<usize>, hash: u64, anchor: usize) -> Slot {
        let near = |want: bool| {
            self.sweep
                .by_form
                .get(&form?)?
                .iter()
                .copied()
                .filter(|&turn| !want || odd(self.sweep.turns[turn].angle))
                .min_by_key(|&turn| turn.abs_diff(anchor))
        };
        if let Some(turn) = near(true) {
            return Slot::Turn(turn);
        }
        if self.debts.get(&hash).is_some_and(|debt| odd(debt.1)) {
            return Slot::Debt(hash);
        }
        near(false).map_or(Slot::Debt(hash), Slot::Turn)
    }

    fn extract(&mut self, slots: [Slot; 7], keys: Option<&[Vec<u32>; 7]>, spot: Spot) {
        let residue = |sign: f64| {
            slots
                .iter()
                .zip(&spot.weights)
                .filter(|(slot, weight)| zero(self.angle(**slot) - sign * FRAC_PI_4 * **weight))
                .count()
        };
        let sign = if residue(-1.0) > residue(1.0) {
            -1.0
        } else {
            1.0
        };
        for (index, (slot, weight)) in slots.iter().zip(&spot.weights).enumerate() {
            let delta = sign * FRAC_PI_4 * weight;
            match *slot {
                Slot::Turn(turn) => self.sweep.turns[turn].angle -= delta,
                Slot::Debt(hash) => {
                    let key = keys.map(|keys| keys[index].clone()).unwrap_or_default();
                    self.debts.entry(hash).or_insert((key, 0.0)).1 -= delta;
                }
            }
        }
        self.inserted.push((spot.time, Insert::Triple(spot.wires)));
    }

    fn exact(&mut self, reach: usize) {
        for anchor in 0..self.sweep.turns.len() {
            if !odd(self.sweep.turns[anchor].angle) {
                continue;
            }
            for space in spaces(&self.sweep, anchor, reach) {
                let mut chosen = [Slot::Turn(anchor); 7];
                let mut complete = true;
                for slot in 1..7 {
                    match nearest(&self.sweep, space[slot], anchor) {
                        Some(turn) => chosen[slot] = Slot::Turn(turn),
                        None => complete = false,
                    }
                }
                if !complete {
                    continue;
                }
                let sizes = space.map(|form| self.sweep.forms.keys[form].len());
                let span = self.span(&chosen, anchor);
                let angles = chosen.map(|slot| self.angle(slot));
                let Some(spot) = place(&self.sweep, &space.map(Some), &sizes, Some(&angles), span)
                else {
                    continue;
                };
                self.extract(chosen, None, spot);
                break;
            }
        }
    }

    fn indebted(&mut self, reach: usize, least: usize) {
        for anchor in 0..self.sweep.turns.len() {
            if !odd(self.sweep.turns[anchor].angle) {
                continue;
            }
            let u = self.sweep.turns[anchor].form;
            let low = anchor.saturating_sub(reach);
            let high = (anchor + reach + 1).min(self.sweep.turns.len());
            let mut near: Vec<usize> = Vec::new();
            for turn in low..high {
                let form = self.sweep.turns[turn].form;
                if form != u && odd(self.sweep.turns[turn].angle) && !near.contains(&form) {
                    near.push(form);
                }
            }
            let mut found: Vec<(usize, [usize; 3])> = Vec::new();
            for (i, &v1) in near.iter().enumerate() {
                for &v2 in &near[i + 1..] {
                    let all = combine(&self.sweep.forms.hashes, [u, v1, v2]);
                    if all.contains(&0) {
                        continue;
                    }
                    let count = all
                        .iter()
                        .filter(|&&hash| {
                            let form = self.sweep.forms.by_hash.get(&hash).copied();
                            odd(self.angle(self.slot(form, hash, anchor)))
                        })
                        .count();
                    if count >= least {
                        found.push((count, [u, v1, v2]));
                    }
                }
            }
            found.sort_by_key(|&(count, _)| Reverse(count));
            for (_, basis) in found {
                if !odd(self.sweep.turns[anchor].angle) {
                    break;
                }
                let keys = spanned(&self.sweep.forms.keys, basis);
                if keys.iter().any(Vec::is_empty) {
                    continue;
                }
                let hashes = combine(&self.sweep.forms.hashes, basis);
                let space = keys
                    .clone()
                    .map(|key| self.sweep.forms.ids.get(&key).copied());
                let slots: [Slot; 7] =
                    array::from_fn(|index| self.slot(space[index], hashes[index], anchor));
                let clean =
                    slots
                        .iter()
                        .zip(&keys)
                        .zip(&space)
                        .all(|((slot, key), form)| match slot {
                            Slot::Turn(_) => true,
                            Slot::Debt(hash) => {
                                form.is_some_and(|form| self.sweep.holders.contains_key(&form))
                                    && self.debts.get(hash).is_none_or(|debt| debt.0 == *key)
                            }
                        });
                let count = slots.iter().filter(|slot| odd(self.angle(**slot))).count();
                if !clean || count < least {
                    continue;
                }
                let sizes = keys.each_ref().map(Vec::len);
                let span = self.span(&slots, anchor);
                let Some(spot) = place(&self.sweep, &space, &sizes, None, span) else {
                    continue;
                };
                self.extract(slots, Some(&keys), spot);
                break;
            }
        }
    }

    fn settle(&mut self) -> bool {
        let debts: Vec<(Vec<u32>, f64)> = self
            .debts
            .values()
            .filter(|debt| !zero(debt.1))
            .cloned()
            .collect();
        for (key, angle) in debts {
            let Some(&form) = self.sweep.forms.ids.get(&key) else {
                return false;
            };
            let Some(held) = self
                .sweep
                .holders
                .get(&form)
                .into_iter()
                .flatten()
                .map(|&id| &self.sweep.held[id])
                .find(|held| held.start < held.end)
            else {
                return false;
            };
            let sign = if held.flip { -1.0 } else { 1.0 };
            self.inserted
                .push((held.start, Insert::Rotation(held.wire, sign * angle)));
        }
        self.debts.clear();
        true
    }
}

fn combine(hashes: &[u64], basis: [usize; 3]) -> [u64; 7] {
    array::from_fn(|index| {
        let mask = index + 1;
        (0..3)
            .filter(|bit| mask >> bit & 1 == 1)
            .fold(0, |h, bit| h ^ hashes[basis[bit]])
    })
}

fn spanned(keys: &[Vec<u32>], basis: [usize; 3]) -> [Vec<u32>; 7] {
    array::from_fn(|index| {
        let mask = index + 1;
        (0..3)
            .filter(|bit| mask >> bit & 1 == 1)
            .fold(Vec::new(), |key, bit| phase::xor(&key, &keys[basis[bit]]))
    })
}

fn lift_block(ops: Vec<Op>, qubits: usize) -> (Vec<Op>, usize) {
    let mut lift = Lift {
        sweep: sweep(&ops, qubits),
        debts: HashMap::new(),
        inserted: Vec::new(),
    };
    lift.exact(REACH);
    let angles: Vec<f64> = lift.sweep.turns.iter().map(|turn| turn.angle).collect();
    let count = lift.inserted.len();
    lift.indebted(REACH, DEBT);
    if !lift.settle() {
        for (turn, angle) in lift.sweep.turns.iter_mut().zip(angles) {
            turn.angle = angle;
        }
        lift.inserted.truncate(count);
        lift.debts.clear();
    }

    let lifted = lift
        .inserted
        .iter()
        .filter(|(_, insert)| matches!(insert, Insert::Triple(_)))
        .count();
    if lifted == 0 {
        return (ops, 0);
    }
    let edits: HashMap<usize, f64> = lift
        .sweep
        .turns
        .iter()
        .map(|turn| (turn.at, turn.sign * turn.angle))
        .collect();
    let mut inserted = lift.inserted;
    inserted.sort_by_key(|&(time, _)| time);
    let mut pending = inserted.into_iter().peekable();
    let mut out = Vec::with_capacity(ops.len() + lifted);
    let mut push_until = |out: &mut Vec<Op>, time: usize, span: Span| {
        while let Some((_, insert)) = pending.next_if(|&(at, _)| at <= time) {
            let gate = match insert {
                Insert::Triple(mut wires) => {
                    wires.sort_unstable();
                    Gate {
                        kind: GateKind::Z,
                        controls: vec![QubitId(wires[0] as u32), QubitId(wires[1] as u32)],
                        targets: vec![QubitId(wires[2] as u32)],
                        params: Vec::new(),
                        span,
                    }
                }
                Insert::Rotation(wire, angle) => {
                    let mut gate = Gate {
                        kind: GateKind::Rz,
                        controls: Vec::new(),
                        targets: vec![QubitId(wire as u32)],
                        params: Vec::new(),
                        span,
                    };
                    phase::rewrite(&mut gate, phase::wrap(angle));
                    gate
                }
            };
            out.push(Op::Gate(gate));
        }
    };
    let length = ops.len();
    for (index, mut op) in ops.into_iter().enumerate() {
        let span = op.as_gate().map_or(Span::DUMMY, |g| g.span);
        push_until(&mut out, index, span);
        if let (Some(&angle), Op::Gate(gate)) = (edits.get(&index), &mut op) {
            let before = phase::slope(gate).expect("rotation");
            if (phase::wrap(angle - before)).abs() > EPSILON {
                if zero(angle) {
                    continue;
                }
                phase::rewrite(gate, phase::wrap(angle));
            }
        }
        out.push(op);
    }
    push_until(&mut out, length, Span::DUMMY);
    (out, lifted)
}

pub fn raise(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    let mut lifted = 0;
    for block in &mut program.blocks {
        let (ops, found) = lift_block(mem::take(&mut block.ops), qubits);
        block.ops = ops;
        lifted += found;
    }
    lifted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::exec::{self, ExecConfig};
    use crate::simulator::matrix::C64;
    use crate::synth;

    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % sides
        }
    }

    fn gate(kind: GateKind, controls: &[QubitId], target: QubitId, angle: Option<f64>) -> Op {
        Op::Gate(Gate {
            kind,
            controls: controls.to_vec(),
            targets: vec![target],
            params: angle
                .map(|a| vec![Operand::Const(Const::Float(a))])
                .unwrap_or_default(),
            span: Span::DUMMY,
        })
    }

    fn turn(angle: f64, wire: QubitId) -> Op {
        gate(GateKind::Rz, &[], wire, Some(angle))
    }

    fn cx(c: QubitId, t: QubitId) -> Op {
        gate(GateKind::X, &[c], t, None)
    }

    fn decomposed(dice: &mut Dice, wires: [QubitId; 3]) -> Vec<Op> {
        let mut roles = wires;
        for slot in 0..3 {
            let pick = slot + dice.roll(3 - slot);
            roles.swap(slot, pick);
        }
        let [x, y, z] = roles;
        let q = if dice.roll(2) == 0 {
            FRAC_PI_4
        } else {
            -FRAC_PI_4
        };
        let mut ops = if dice.roll(2) == 0 {
            vec![
                cx(y, z),
                turn(-q, z),
                cx(x, z),
                turn(q, z),
                cx(y, z),
                turn(-q, z),
                cx(x, z),
                cx(x, y),
                turn(-q, y),
                cx(x, y),
                turn(q, x),
                turn(q, y),
                turn(q, z),
            ]
        } else {
            vec![
                turn(q, x),
                turn(q, y),
                turn(q, z),
                cx(x, y),
                turn(-q, y),
                cx(y, z),
                turn(q, z),
                cx(x, z),
                turn(-q, z),
                cx(y, z),
                turn(-q, z),
                cx(x, z),
                cx(x, y),
            ]
        };
        for _ in 0..60 {
            let at = dice.roll(ops.len() - 1);
            let (a, b) = (ops[at].as_gate().unwrap(), ops[at + 1].as_gate().unwrap());
            if synth::commute(a, b) {
                ops.swap(at, at + 1);
            }
        }
        ops
    }

    fn build(ops: Vec<Op>, qubits: u32) -> Program {
        let mut program = Program::new("semlift", Profile::Base);
        program.num_qubits = qubits;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops,
            term: Term::Unreachable,
            span: Span::DUMMY,
        });
        program
    }

    fn state(program: &Program) -> Vec<C64> {
        let state = exec::execute_vector(program, ExecConfig::default())
            .final_state
            .unwrap();
        (0..state.len()).map(|i| state.amplitude(i)).collect()
    }

    fn random(dice: &mut Dice, qubits: u32, length: usize, noise: bool) -> (Program, usize) {
        let mut ops = Vec::new();
        for q in 0..qubits {
            let wire = QubitId(q);
            ops.push(gate(
                GateKind::Ry,
                &[],
                wire,
                Some(0.3 + 0.37 * dice.roll(16) as f64),
            ));
            ops.push(turn(0.2 + 0.41 * dice.roll(16) as f64, wire));
            ops.push(gate(GateKind::H, &[], wire, None));
        }
        let mut triples = 0;
        for _ in 0..length {
            let mut wires: Vec<QubitId> = (0..qubits).map(QubitId).collect();
            for slot in 0..3 {
                let pick = slot + dice.roll(wires.len() - slot);
                wires.swap(slot, pick);
            }
            let [a, b, t] = [wires[0], wires[1], wires[2]];
            match dice.roll(if noise { 6 } else { 2 }) {
                0 => {
                    ops.extend(decomposed(dice, [a, b, t]));
                    ops.push(gate(GateKind::H, &[], t, None));
                    ops.push(gate(GateKind::H, &[], a, None));
                    ops.push(gate(GateKind::H, &[], b, None));
                    triples += 1;
                }
                1 => {
                    ops.push(gate(GateKind::H, &[], t, None));
                    ops.extend(decomposed(dice, [a, b, t]));
                    ops.push(gate(GateKind::H, &[], t, None));
                    ops.push(gate(GateKind::H, &[], a, None));
                    ops.push(gate(GateKind::H, &[], b, None));
                    triples += 1;
                }
                2 => ops.push(cx(a, t)),
                3 => ops.push(gate(GateKind::X, &[], t, None)),
                4 => ops.push(gate(GateKind::H, &[], t, None)),
                _ => ops.push(turn(FRAC_PI_4 * (1 + dice.roll(7)) as f64, t)),
            }
        }
        (build(ops, qubits), triples)
    }

    fn agree(want: &[C64], got: &[C64]) -> bool {
        let overlap: C64 = want.iter().zip(got).map(|(a, b)| a.conj() * b).sum();
        (overlap.norm() - 1.0).abs() < 1e-9
    }

    #[test]
    fn lifts_every_shuffled_decomposition() {
        let mut dice = Dice(0x5e11);
        for round in 0..80 {
            let qubits = 3 + (round % 4) as u32;
            let (program, triples) = random(&mut dice, qubits, 1 + round % 6, false);
            let mut lifted = program.clone();
            assert_eq!(raise(&mut lifted), triples, "round {round}");
            assert!(agree(&state(&program), &state(&lifted)), "round {round}");
            let rotations = lifted
                .gates()
                .filter(|g| phase::slope(g).is_some_and(|s| !zero(s)) && g.kind != GateKind::Z)
                .count();
            assert_eq!(rotations, qubits as usize, "round {round}");
        }
    }

    #[test]
    fn random_circuits_keep_their_state() {
        let mut dice = Dice(0xfeed);
        for round in 0..150 {
            let qubits = 3 + (round % 4) as u32;
            let (program, _) = random(&mut dice, qubits, 4 + round % 16, true);
            let mut lifted = program.clone();
            raise(&mut lifted);
            assert!(agree(&state(&program), &state(&lifted)), "round {round}");
        }
    }
}
