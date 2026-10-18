use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI};

use crate::diag::Span;
use crate::ir::*;
use crate::phase;

const EPSILON: f64 = 1e-9;
const REACH: usize = 4096;
const SPAN: usize = 40_000;
const MAX_ROUNDS: usize = 64;
const MAX_FLOAT: usize = 4000;
const MAX_TERMS: usize = 800;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Letter {
    H(u32),
    X(u32),
    P(u32, f64),
    Cx(u32, u32),
    Cz(u32, u32),
    Xx(u32, u32),
    Other(usize),
}

use Letter::*;

#[derive(Clone, Debug)]
struct Item {
    letter: Letter,
    wires: Vec<u32>,
}

impl Item {
    fn new(letter: Letter) -> Item {
        let wires = match letter {
            H(q) | X(q) | P(q, _) => vec![q],
            Cx(a, b) | Cz(a, b) | Xx(a, b) => vec![a, b],
            Other(_) => Vec::new(),
        };
        Item { letter, wires }
    }

    fn shares(&self, other: &Item) -> bool {
        self.wires.iter().any(|w| other.wires.contains(w))
    }
}

fn commute(a: Letter, b: Letter) -> bool {
    match (a, b) {
        (Other(_), _) | (_, Other(_)) | (H(_), _) | (_, H(_)) => false,
        (P(..), P(..)) | (P(..), Cz(..)) | (Cz(..), P(..)) | (Cz(..), Cz(..)) => true,
        (X(_), X(_)) | (X(_), Xx(..)) | (Xx(..), X(_)) | (Xx(..), Xx(..)) => true,
        (P(..), X(_)) | (X(_), P(..)) | (X(_), Cz(..)) | (Cz(..), X(_)) => false,
        (P(..), Xx(..)) | (Xx(..), P(..)) | (Cz(..), Xx(..)) | (Xx(..), Cz(..)) => false,
        (Cx(c, _), Xx(a, b)) | (Xx(a, b), Cx(c, _)) => c != a && c != b,
        (P(q, _), Cx(_, t)) | (Cx(_, t), P(q, _)) => q != t,
        (X(q), Cx(c, _)) | (Cx(c, _), X(q)) => q != c,
        (Cx(c, t), Cx(d, u)) => c != u && d != t,
        (Cx(_, t), Cz(a, b)) | (Cz(a, b), Cx(_, t)) => t != a && t != b,
    }
}

fn merge(a: Letter, b: Letter) -> Option<Option<Letter>> {
    match (a, b) {
        (H(p), H(q)) | (X(p), X(q)) if p == q => Some(None),
        (P(p, x), P(q, y)) if p == q => {
            let sum = phase::wrap(x + y);
            Some((sum.abs() > EPSILON).then_some(P(p, sum)))
        }
        (Cx(..), Cx(..)) | (Cz(..), Cz(..)) | (Xx(..), Xx(..)) if a == b => Some(None),
        _ => None,
    }
}

fn reduce(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Option<Item>> = Vec::with_capacity(items.len());
    for item in items {
        let mut seen = 0;
        let mut absorbed = false;
        for slot in out.iter_mut().rev().take(SPAN) {
            let Some(earlier) = slot else {
                continue;
            };
            if !earlier.shares(&item) {
                continue;
            }
            if let Some(merged) = merge(earlier.letter, item.letter) {
                match merged {
                    Some(letter) => earlier.letter = letter,
                    None => *slot = None,
                }
                absorbed = true;
                break;
            }
            seen += 1;
            if seen == REACH || !commute(earlier.letter, item.letter) {
                break;
            }
        }
        if !absorbed {
            out.push(Some(item));
        }
    }
    out.into_iter().flatten().collect()
}

fn threads(items: &[Option<Item>], qubits: usize) -> Vec<Vec<usize>> {
    let mut on = vec![Vec::new(); qubits];
    for (index, item) in items.iter().enumerate() {
        for &w in item.iter().flat_map(|item| &item.wires) {
            on[w as usize].push(index);
        }
    }
    on
}

fn rewrite(items: Vec<Item>, qubits: usize) -> (Vec<Item>, bool) {
    let mut slots: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    let on = threads(&slots, qubits);
    let mut frozen = vec![false; slots.len()];
    let mut changed = false;

    for (wire, list) in on.iter().enumerate() {
        let wire = wire as u32;
        for window in list.windows(3) {
            let [i, j, k] = [window[0], window[1], window[2]];
            if frozen[i] || frozen[j] || frozen[k] {
                continue;
            }
            let letter = |slot: &Option<Item>| slot.as_ref().map(|item| item.letter);
            if letter(&slots[i]) != Some(H(wire)) || letter(&slots[k]) != Some(H(wire)) {
                continue;
            }
            let Some(middle) = letter(&slots[j]) else {
                continue;
            };
            let (outer, inner) = match middle {
                P(_, angle) if (angle.abs() - FRAC_PI_2).abs() < EPSILON => {
                    (Some(P(wire, -angle)), H(wire))
                }
                P(_, angle) if (angle.abs() - PI).abs() < EPSILON => (None, X(wire)),
                X(_) => (None, P(wire, PI)),
                Cx(c, t) if t == wire => (None, Cz(c.min(t), c.max(t))),
                Cx(c, t) if c == wire => (None, Xx(c.min(t), c.max(t))),
                Cz(a, b) => (None, Cx(if a == wire { b } else { a }, wire)),
                Xx(a, b) => (None, Cx(wire, if a == wire { b } else { a })),
                _ => continue,
            };
            slots[i] = outer.map(Item::new);
            slots[k] = outer.map(Item::new);
            slots[j] = Some(Item::new(inner));
            frozen[i] = true;
            frozen[j] = true;
            frozen[k] = true;
            changed = true;
        }
        for window in list.windows(5) {
            if window.iter().any(|&n| frozen[n]) {
                continue;
            }
            let letters: Vec<Letter> = window
                .iter()
                .filter_map(|&n| slots[n].as_ref().map(|item| item.letter))
                .collect();
            let [H(a), P(_, before), middle, P(_, after), H(b)] = letters[..] else {
                continue;
            };
            let quarter = (before.abs() - FRAC_PI_2).abs() < EPSILON;
            let pivot = match middle {
                Cx(_, t) => t == wire,
                Xx(..) => true,
                _ => false,
            };
            if a != wire || b != wire || !quarter || !pivot || (before + after).abs() > EPSILON {
                continue;
            }
            slots[window[0]] = None;
            slots[window[4]] = None;
            slots[window[1]] = Some(Item::new(P(wire, after)));
            slots[window[3]] = Some(Item::new(P(wire, before)));
            for &n in window {
                frozen[n] = true;
            }
            changed = true;
        }
    }

    (slots.into_iter().flatten().collect(), changed)
}

fn expand(items: Vec<Item>, qubits: usize) -> Vec<Item> {
    let on = {
        let slots: Vec<Option<Item>> = items.iter().cloned().map(Some).collect();
        threads(&slots, qubits)
    };
    let mut place = vec![[0usize; 2]; items.len()];
    for (wire, list) in on.iter().enumerate() {
        for (rank, &index) in list.iter().enumerate() {
            for (side, &w) in items[index].wires.iter().enumerate().take(2) {
                if w as usize == wire {
                    place[index][side] = rank;
                }
            }
        }
    }
    let score = |wire: u32, rank: usize| {
        let list = &on[wire as usize];
        let weigh = |at: Option<&usize>| match at.map(|&n| items[n].letter) {
            Some(letter) if letter == H(wire) => 2,
            Some(Cz(..) | Xx(..)) => 1,
            _ => 0,
        };
        let before = rank.checked_sub(1).and_then(|r| list.get(r));
        weigh(before) + weigh(list.get(rank + 1))
    };
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let (a, b, diagonal) = match item.letter {
            Cz(a, b) => (a, b, true),
            Xx(a, b) => (a, b, false),
            _ => {
                out.push(item.clone());
                continue;
            }
        };
        let (other, turned) = if score(a, place[index][0]) > score(b, place[index][1]) {
            (b, a)
        } else {
            (a, b)
        };
        let inner = if diagonal {
            Cx(other, turned)
        } else {
            Cx(turned, other)
        };
        out.push(Item::new(H(turned)));
        out.push(Item::new(inner));
        out.push(Item::new(H(turned)));
    }
    out
}

fn lift(gate: &Gate, at: usize) -> Item {
    let wire = |q: &QubitId| q.0;
    let letter = match (gate.kind, &gate.controls[..], &gate.targets[..]) {
        (GateKind::H, [], [t]) => H(wire(t)),
        (GateKind::X, [], [t]) => X(wire(t)),
        (GateKind::X, [c], [t]) if c != t => Cx(wire(c), wire(t)),
        (GateKind::Z, [c], [t]) if c != t => Cz(wire(c).min(wire(t)), wire(c).max(wire(t))),
        (_, [], [t]) => match phase::slope(gate) {
            Some(angle) if angle.is_finite() => P(wire(t), phase::wrap(angle)),
            _ => Other(at),
        },
        _ => Other(at),
    };
    let mut item = Item::new(letter);
    if let Other(_) = letter {
        item.wires = gate.wires().map(|q| q.0).collect();
    }
    item
}

fn lower(item: &Item, source: &[Gate], span: Span, named: bool) -> Gate {
    let plain = |kind, controls: &[u32], target: u32| Gate {
        kind,
        controls: controls.iter().map(|&q| QubitId(q)).collect(),
        targets: vec![QubitId(target)],
        params: Vec::new(),
        span,
    };
    match item.letter {
        H(q) => plain(GateKind::H, &[], q),
        X(q) => plain(GateKind::X, &[], q),
        Cx(c, t) => plain(GateKind::X, &[c], t),
        Cz(a, b) | Xx(a, b) => plain(GateKind::Z, &[a], b),
        P(q, angle) => {
            let mut gate = plain(GateKind::Rz, &[], q);
            gate.params = vec![Operand::Const(Const::Float(angle))];
            if named {
                phase::rewrite(&mut gate, angle);
            }
            gate
        }
        Other(at) => source[at].clone(),
    }
}

fn cost(items: &[Item]) -> (usize, usize) {
    let hadamards = items.iter().filter(|i| matches!(i.letter, H(_))).count();
    (items.len(), hadamards)
}

fn simplify(items: Vec<Item>, qubits: usize) -> Vec<Item> {
    let mut items = reduce(items);
    for _ in 0..MAX_ROUNDS {
        let (next, changed) = rewrite(items, qubits);
        items = reduce(next);
        if !changed {
            break;
        }
    }
    reduce(expand(items, qubits))
}

fn frame(items: Vec<Item>, qubits: usize) -> Vec<Item> {
    let mut flips = vec![(false, false); qubits];
    let mut out = Vec::with_capacity(items.len());
    let flush = |flips: &mut Vec<(bool, bool)>, out: &mut Vec<Item>, w: u32| {
        let (x, z) = std::mem::take(&mut flips[w as usize]);
        if x {
            out.push(Item::new(X(w)));
        }
        if z {
            out.push(Item::new(P(w, PI)));
        }
    };
    for item in items {
        let at = |w: u32| w as usize;
        match item.letter {
            X(q) => {
                flips[at(q)].0 ^= true;
                continue;
            }
            P(q, angle) if (angle.abs() - PI).abs() < EPSILON => {
                flips[at(q)].1 ^= true;
                continue;
            }
            P(q, angle) => {
                let signed = if flips[at(q)].0 { -angle } else { angle };
                out.push(Item::new(P(q, signed)));
                continue;
            }
            H(q) => {
                let (x, z) = flips[at(q)];
                flips[at(q)] = (z, x);
            }
            Cx(c, t) => {
                flips[at(t)].0 ^= flips[at(c)].0;
                flips[at(c)].1 ^= flips[at(t)].1;
            }
            Cz(a, b) => {
                flips[at(b)].1 ^= flips[at(a)].0;
                flips[at(a)].1 ^= flips[at(b)].0;
            }
            Xx(a, b) => {
                flips[at(b)].0 ^= flips[at(a)].1;
                flips[at(a)].0 ^= flips[at(b)].1;
            }
            Other(_) => {
                for &w in &item.wires {
                    flush(&mut flips, &mut out, w);
                }
            }
        }
        out.push(item);
    }
    for w in 0..qubits as u32 {
        flush(&mut flips, &mut out, w);
    }
    out
}

type Label = (Vec<u32>, bool);

struct Tracker {
    labels: Vec<Label>,
}

impl Tracker {
    fn new(qubits: usize) -> Tracker {
        Tracker {
            labels: (0..qubits as u32).map(|q| (vec![q], false)).collect(),
        }
    }

    fn step<'a>(&mut self, item: &'a Item, base: u32) -> &'a [u32] {
        match item.letter {
            X(q) => {
                self.labels[q as usize].1 ^= true;
                &[]
            }
            Cx(c, t) => {
                let control = self.labels[c as usize].clone();
                let target = &mut self.labels[t as usize];
                target.0 = phase::xor(&target.0, &control.0);
                target.1 ^= control.1;
                &item.wires[1..]
            }
            P(..) | Cz(..) => &[],
            H(_) | Xx(..) | Other(_) => {
                for (side, &w) in item.wires.iter().enumerate() {
                    self.labels[w as usize] = (vec![base + side as u32], false);
                }
                &item.wires
            }
        }
    }
}

fn available(
    skeleton: &[Option<(u32, Item)>],
    skip: [usize; 2],
    needed: &HashMap<Vec<u32>, f64>,
    qubits: usize,
) -> bool {
    let mut tracker = Tracker::new(qubits);
    let mut found: HashSet<Vec<u32>> = HashSet::new();
    for (parity, _) in &tracker.labels {
        if needed.contains_key(parity) {
            found.insert(parity.clone());
        }
    }
    for (index, slot) in skeleton.iter().enumerate() {
        let Some((base, item)) = slot else {
            continue;
        };
        if skip.contains(&index) {
            continue;
        }
        let changed = tracker.step(item, *base).to_vec();
        for w in changed {
            let parity = &tracker.labels[w as usize].0;
            if needed.contains_key(parity) && !found.contains(parity) {
                found.insert(parity.clone());
            }
        }
    }
    found.len() == needed.len()
}

#[derive(Clone, Copy, PartialEq)]
enum Greed {
    First,
    Each,
    Best,
}

fn polarise(needed: &mut HashMap<Vec<u32>, f64>, greed: Greed) -> bool {
    let mut keys: Vec<Vec<u32>> = needed.keys().cloned().collect();
    keys.sort();
    let near = |angle: f64, goal: f64| (phase::wrap(angle).abs() - goal).abs() < EPSILON;
    let mut applied = false;
    let mut best: (usize, Vec<(Vec<u32>, f64)>) = (0, Vec::new());
    for x in &keys {
        let Some(&angle) = needed.get(x) else {
            continue;
        };
        let (half, quarter) = (near(angle, PI), near(angle, FRAC_PI_2));
        if !half && !quarter {
            continue;
        }
        let mut consider = |needed: &HashMap<Vec<u32>, f64>, members: &[(&Vec<u32>, f64)]| {
            let cleared = members
                .iter()
                .filter(|(key, turn)| phase::wrap(needed[*key] + turn).abs() < EPSILON)
                .count();
            if cleared > best.0 && (best.0 == 0 || greed != Greed::First) {
                best = (
                    cleared,
                    members.iter().map(|(k, t)| ((*k).clone(), *t)).collect(),
                );
            }
        };
        for (first, y) in keys.iter().enumerate() {
            let xy = phase::xor(x, y);
            if y == x || !needed.contains_key(y) || !needed.contains_key(&xy) {
                continue;
            }
            if half {
                consider(needed, &[(x, PI), (y, PI), (&xy, PI)]);
                continue;
            }
            for z in &keys[first + 1..] {
                if z == x || *z == xy || !needed.contains_key(z) {
                    continue;
                }
                let (xz, yz) = (phase::xor(x, z), phase::xor(y, z));
                let xyz = phase::xor(&xy, z);
                if [&xz, &yz, &xyz]
                    .iter()
                    .any(|key| !needed.contains_key(*key))
                {
                    continue;
                }
                for turn in [FRAC_PI_2, -FRAC_PI_2] {
                    consider(
                        needed,
                        &[
                            (x, turn),
                            (y, turn),
                            (z, turn),
                            (&xy, -turn),
                            (&xz, -turn),
                            (&yz, -turn),
                            (&xyz, turn),
                        ],
                    );
                }
            }
        }
        if greed != Greed::Best {
            applied |= turn_all(needed, std::mem::take(&mut best));
        }
    }
    applied | turn_all(needed, best)
}

fn turn_all(needed: &mut HashMap<Vec<u32>, f64>, best: (usize, Vec<(Vec<u32>, f64)>)) -> bool {
    for (key, turn) in &best.1 {
        let angle = phase::wrap(needed[key] + turn);
        if angle.abs() < EPSILON {
            needed.remove(key);
        } else {
            needed.insert(key.clone(), angle);
        }
    }
    best.0 > 0
}

fn float(items: Vec<Item>, qubits: usize, greed: Option<Greed>) -> Vec<Item> {
    if items.len() > MAX_FLOAT {
        return items;
    }
    let mut tracker = Tracker::new(qubits);
    let mut needed: HashMap<Vec<u32>, f64> = HashMap::new();
    let mut skeleton: Vec<Option<(u32, Item)>> = Vec::with_capacity(items.len());
    let mut base = qubits as u32;
    for item in items {
        if let P(q, angle) = item.letter {
            let (parity, flipped) = &tracker.labels[q as usize];
            if !parity.is_empty() {
                *needed.entry(parity.clone()).or_insert(0.0) +=
                    if *flipped { -angle } else { angle };
            }
            continue;
        }
        tracker.step(&item, base);
        let width = item.wires.len() as u32;
        skeleton.push(Some((base, item)));
        base += width;
    }
    needed.retain(|_, angle| phase::wrap(*angle).abs() > EPSILON);
    if let Some(greed) = greed
        && needed.len() <= MAX_TERMS
    {
        while polarise(&mut needed, greed) {}
    }

    for later in 0..skeleton.len() {
        let Some((_, item)) = &skeleton[later] else {
            continue;
        };
        if !matches!(item.letter, Cx(..)) {
            continue;
        }
        let item = item.clone();
        let mut partner = None;
        let mut seen = 0;
        for earlier in (0..later).rev() {
            let Some((_, other)) = &skeleton[earlier] else {
                continue;
            };
            if !other.shares(&item) {
                continue;
            }
            if other.letter == item.letter {
                partner = Some(earlier);
                break;
            }
            seen += 1;
            if seen == REACH || !commute(other.letter, item.letter) {
                break;
            }
        }
        if let Some(earlier) = partner
            && available(&skeleton, [earlier, later], &needed, qubits)
        {
            skeleton[earlier] = None;
            skeleton[later] = None;
        }
    }

    let mut tracker = Tracker::new(qubits);
    let mut out = Vec::with_capacity(skeleton.len() + needed.len());
    let mut settle = |tracker: &Tracker, out: &mut Vec<Item>, w: u32| {
        let (parity, flipped) = &tracker.labels[w as usize];
        if let Some(angle) = needed.remove(parity) {
            let angle = phase::wrap(if *flipped { -angle } else { angle });
            out.push(Item::new(P(w, angle)));
        }
    };
    for w in 0..qubits as u32 {
        settle(&tracker, &mut out, w);
    }
    for (base, item) in skeleton.into_iter().flatten() {
        let changed = tracker.step(&item, base).to_vec();
        out.push(item);
        for w in changed {
            settle(&tracker, &mut out, w);
        }
    }
    out
}

fn settle(gates: &[Gate], qubits: usize, named: bool) -> Option<(Vec<Gate>, usize)> {
    let original: Vec<Item> = gates
        .iter()
        .enumerate()
        .map(|(at, gate)| lift(gate, at))
        .collect();
    if original
        .iter()
        .any(|item| item.wires.iter().any(|&w| w as usize >= qubits))
    {
        return None;
    }
    let before = cost(&original);
    let mut items = simplify(original.clone(), qubits);
    let framed = reduce(frame(original.clone(), qubits));
    let mut rivals: Vec<Vec<Item>> = vec![framed.clone()];
    let plain = reduce(original);
    for greed in [
        None,
        Some(Greed::First),
        Some(Greed::Each),
        Some(Greed::Best),
    ] {
        rivals.push(float(plain.clone(), qubits, greed));
        rivals.push(float(framed.clone(), qubits, greed));
    }
    for rival in rivals {
        let rival = simplify(rival, qubits);
        if cost(&rival) < cost(&items) {
            items = rival;
        }
    }
    let after = cost(&items);
    if after >= before {
        return None;
    }
    let span = gates.first().map_or(Span::DUMMY, |gate| gate.span);
    let lowered = items
        .iter()
        .map(|item| lower(item, gates, span, named))
        .collect();
    let gained = (before.0 - after.0) + usize::from(after.0 == before.0 && after.1 < before.1);
    Some((lowered, gained))
}

fn settle_block(ops: &mut Vec<Op>, qubits: usize, named: bool) -> usize {
    let mut out: Vec<Op> = Vec::with_capacity(ops.len());
    let mut run: Vec<Gate> = Vec::new();
    let mut hits = 0;
    let mut flush = |run: &mut Vec<Gate>, out: &mut Vec<Op>| {
        if let Some((gates, gained)) = settle(run, qubits, named) {
            hits += gained;
            out.extend(gates.into_iter().map(Op::Gate));
            run.clear();
        } else {
            out.extend(run.drain(..).map(Op::Gate));
        }
    };
    for op in ops.drain(..) {
        match op {
            Op::Gate(gate) => run.push(gate),
            other => {
                flush(&mut run, &mut out);
                out.push(other);
            }
        }
    }
    flush(&mut run, &mut out);
    *ops = out;
    hits
}

fn settle_program(program: &mut Program, named: bool) -> usize {
    let qubits = program.num_qubits as usize;
    program
        .blocks
        .iter_mut()
        .map(|block| settle_block(&mut block.ops, qubits, named))
        .sum()
}

pub(crate) fn normalise(program: &mut Program) -> usize {
    settle_program(program, true)
}

fn unname(program: &mut Program) {
    for block in &mut program.blocks {
        for op in &mut block.ops {
            if let Op::Gate(gate) = op
                && gate.kind != GateKind::Rz
                && let Some(angle) = phase::slope(gate)
            {
                gate.kind = GateKind::Rz;
                gate.params = vec![Operand::Const(Const::Float(angle))];
            }
        }
    }
}

pub(crate) fn polish(program: &mut Program) -> usize {
    let mut total = 0;
    for _ in 0..MAX_ROUNDS {
        let hits = settle_program(program, false) + phase::fold(program);
        unname(program);
        if hits == 0 {
            break;
        }
        total += hits;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equiv;
    use crate::qasm;
    use crate::simulator::state::Rng;

    const GATES: [&str; 16] = [
        "h", "h", "s", "sdg", "t", "tdg", "x", "z", "rz(0.3)", "cx", "cx", "cx", "cz", "ry(0.4)",
        "swap", "rz(-1.1)",
    ];

    fn random(rng: &mut Rng, qubits: usize, length: usize) -> String {
        let mut text = format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n");
        for _ in 0..length {
            let gate = GATES[rng.next_u64() as usize % GATES.len()];
            let a = rng.next_u64() as usize % qubits;
            if matches!(gate, "cx" | "cz" | "swap") {
                let b = (a + 1 + rng.next_u64() as usize % (qubits - 1)) % qubits;
                text += &format!("{gate} q[{a}], q[{b}];\n");
            } else {
                text += &format!("{gate} q[{a}];\n");
            }
        }
        text
    }

    fn check(source: &str) -> (usize, usize) {
        let (original, errors) = qasm::lower(source);
        assert!(errors.is_empty(), "{errors:?}");
        let mut settled = original.clone();
        while normalise(&mut settled) > 0 {}
        let differences =
            equiv::compare(&equiv::explore(&original), &equiv::explore(&settled), true);
        assert!(differences.is_empty(), "{differences:?}\n{source}");
        assert!(settled.gate_count() <= original.gate_count(), "{source}");
        (original.gate_count(), settled.gate_count())
    }

    #[test]
    fn settles_random_circuits() {
        let mut rng = Rng::new(7);
        let (mut before, mut after) = (0, 0);
        for round in 0..1500 {
            let qubits = 2 + round % 4;
            let (b, a) = check(&random(&mut rng, qubits, 6 + round % 60));
            before += b;
            after += a;
        }
        assert!(after < before, "{after} of {before}");
    }

    fn toffoli(a: usize, b: usize, c: usize) -> String {
        format!(
            "h q[{c}]; cx q[{b}], q[{c}]; tdg q[{c}]; cx q[{a}], q[{c}]; t q[{c}]; \
             cx q[{b}], q[{c}]; tdg q[{c}]; cx q[{a}], q[{c}]; cx q[{a}], q[{b}]; tdg q[{b}]; \
             cx q[{a}], q[{b}]; t q[{a}]; t q[{b}]; t q[{c}]; h q[{c}];\n"
        )
    }

    fn arithmetic(rng: &mut Rng, qubits: usize, length: usize) -> String {
        let mut text = format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n");
        let mut pick = |bound: usize| rng.next_u64() as usize % bound;
        for _ in 0..length {
            let a = pick(qubits);
            let b = (a + 1 + pick(qubits - 1)) % qubits;
            let c = (0..qubits)
                .filter(|q| *q != a && *q != b)
                .nth(pick(qubits - 2));
            match (pick(8), c) {
                (0..=3, Some(c)) => text += &toffoli(a, b, c),
                (4 | 5, _) => text += &format!("cx q[{a}], q[{b}];\n"),
                (6, _) => text += &format!("x q[{a}];\n"),
                _ => text += &format!("s q[{a}];\n"),
            }
        }
        text
    }

    #[test]
    fn floats_rotations() {
        let mut rng = Rng::new(11);
        let (mut before, mut after) = (0, 0);
        for round in 0..300 {
            let qubits = 3 + round % 3;
            let (b, a) = check(&arithmetic(&mut rng, qubits, 2 + round % 9));
            before += b;
            after += a;
        }
        assert!(after < before, "{after} of {before}");
    }

    #[test]
    fn hadamard_identities() {
        let header = "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[3];\n";
        let cases = [
            ("h q[0]; h q[1]; cx q[0], q[1]; h q[0]; h q[1];", 1),
            ("h q[1]; s q[1]; cx q[0], q[1]; sdg q[1]; h q[1];", 3),
            ("h q[1]; sdg q[1]; cx q[0], q[1]; s q[1]; h q[1];", 3),
            (
                "cx q[0], q[1]; h q[1]; cx q[1], q[2]; h q[1]; cx q[0], q[1];",
                3,
            ),
            (
                "cx q[0], q[1]; t q[0]; cx q[0], q[2]; x q[1]; cx q[0], q[1];",
                3,
            ),
        ];
        for (body, expected) in cases {
            let (_, after) = check(&format!("{header}{body}\n"));
            assert_eq!(after, expected, "{body}");
        }
    }
}
