use std::collections::{HashMap, HashSet};
use std::mem;

use crate::diag::Span;
use crate::ir::*;
use crate::phase;

const WIDEST: usize = 64;
const FULL: usize = 6;
const PATIENCE: usize = 8;
const TOP: usize = 32;
const TRIES: u64 = 16;

type Term = [u64; 3];

enum Kind {
    Cx(QubitId, QubitId),
    Ccz([QubitId; 3]),
    Turn(QubitId),
    Other,
}

fn classify(op: &Op) -> Kind {
    let Some(gate) = op.as_gate() else {
        return Kind::Other;
    };
    match (gate.kind, &gate.controls[..], &gate.targets[..]) {
        (GateKind::X, [c], [t]) if gate.params.is_empty() => Kind::Cx(*c, *t),
        (GateKind::Z, [a, b], [t]) if gate.params.is_empty() => Kind::Ccz([*a, *b, *t]),
        (_, [], [t]) if phase::slope(gate).is_some() => Kind::Turn(*t),
        _ => Kind::Other,
    }
}

fn tidy(mut term: Term) -> Option<Option<Term>> {
    term.sort_unstable();
    let [a, b, c] = term;
    if a == 0 || a ^ b ^ c == 0 {
        return Some(None);
    }
    if a == b || b == c {
        return None;
    }
    Some(Some(term))
}

fn toggle(terms: &mut Vec<Term>, term: Term) {
    match terms.iter().position(|held| *held == term) {
        Some(at) => {
            terms.swap_remove(at);
        }
        None => terms.push(term),
    }
}

fn shared(left: &Term, right: &Term) -> Vec<u64> {
    left.iter()
        .copied()
        .filter(|form| right.contains(form))
        .collect()
}

fn rest(term: &Term, without: &[u64]) -> Vec<u64> {
    term.iter()
        .copied()
        .filter(|form| !without.contains(form))
        .collect()
}

struct Basis {
    rows: Vec<u64>,
    inverse: Vec<u64>,
    moves: Vec<(usize, usize)>,
}

impl Basis {
    fn new(width: usize) -> Self {
        Basis {
            rows: (0..width).map(|k| 1 << k).collect(),
            inverse: (0..width).map(|k| 1 << k).collect(),
            moves: Vec::new(),
        }
    }

    fn solve(&self, form: u64) -> u64 {
        let mut left = form;
        let mut found = 0;
        while left != 0 {
            found ^= self.inverse[left.trailing_zeros() as usize];
            left &= left - 1;
        }
        found
    }

    fn price(&self, form: u64) -> usize {
        self.solve(form).count_ones() as usize - 1
    }

    fn add(&mut self, source: usize, sink: usize) {
        self.rows[sink] ^= self.rows[source];
        for held in &mut self.inverse {
            if *held >> sink & 1 == 1 {
                *held ^= 1 << source;
            }
        }
        self.moves.push((source, sink));
    }

    fn make(&mut self, form: u64, locked: u64, wanted: &HashMap<u64, usize>) -> usize {
        let chosen = self.solve(form);
        let free = chosen & !locked;
        let mut pivot = free.trailing_zeros() as usize;
        let mut least = usize::MAX;
        let mut left = free;
        while left != 0 {
            let wire = left.trailing_zeros() as usize;
            left &= left - 1;
            let need = wanted.get(&self.rows[wire]).copied().unwrap_or(0);
            if need < least {
                least = need;
                pivot = wire;
            }
        }
        let mut left = chosen & !(1 << pivot);
        while left != 0 {
            let wire = left.trailing_zeros() as usize;
            left &= left - 1;
            self.add(wire, pivot);
        }
        pivot
    }

    fn settle(&mut self, goal: &[u64]) {
        let width = self.rows.len();
        let mut matrix: Vec<u64> = goal.iter().map(|row| self.solve(*row)).collect();
        let mut steps = Vec::new();
        for column in 0..width {
            if matrix[column] >> column & 1 == 0 {
                let below = (column + 1..width)
                    .find(|&row| matrix[row] >> column & 1 == 1)
                    .expect("goal is invertible");
                matrix[column] ^= matrix[below];
                steps.push((below, column));
            }
            for row in 0..width {
                if row != column && matrix[row] >> column & 1 == 1 {
                    matrix[row] ^= matrix[column];
                    steps.push((column, row));
                }
            }
        }
        for (source, sink) in steps.into_iter().rev() {
            self.add(source, sink);
        }
    }
}

enum Step {
    Add(usize, usize),
    Triple([usize; 3]),
    Turn(usize, usize),
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn schedule(
    width: usize,
    terms: &[Term],
    turns: &[u64],
    goal: &[u64],
    seed: u64,
) -> (usize, Vec<Step>) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut basis = Basis::new(width);
    let mut steps = Vec::new();
    let mut wanted: HashMap<u64, usize> = HashMap::new();
    for term in terms {
        for form in term {
            *wanted.entry(*form).or_default() += 1;
        }
    }
    let mut waiting: Vec<Option<u64>> = turns.iter().copied().map(Some).collect();
    let flush = |basis: &mut Basis, steps: &mut Vec<Step>, waiting: &mut Vec<Option<u64>>| {
        for (source, sink) in basis.moves.drain(..) {
            steps.push(Step::Add(source, sink));
        }
        for (index, slot) in waiting.iter_mut().enumerate() {
            let Some(form) = *slot else {
                continue;
            };
            if let Some(wire) = basis.rows.iter().position(|row| *row == form) {
                steps.push(Step::Turn(index, wire));
                *slot = None;
            }
        }
    };
    let mut cx = 0;
    flush(&mut basis, &mut steps, &mut waiting);
    let mut left: Vec<Term> = terms.to_vec();
    while !left.is_empty() {
        let mut best = 0;
        let mut least = usize::MAX;
        for (index, term) in left.iter().enumerate() {
            let mut price = term.iter().map(|form| basis.price(*form)).sum::<usize>() * 8;
            if seed != 0 {
                price += (rng.next() % 8) as usize;
            }
            if price < least {
                least = price;
                best = index;
            }
        }
        let term = left.remove(best);
        let mut order = term;
        order.sort_by_key(|form| basis.price(*form));
        let mut locked = 0;
        let mut wires = [0; 3];
        for form in &term {
            *wanted.get_mut(form).expect("counted") -= 1;
        }
        for (slot, form) in order.iter().enumerate() {
            let wire = basis.make(*form, locked, &wanted);
            locked |= 1 << wire;
            wires[slot] = wire;
        }
        cx += basis.moves.len();
        flush(&mut basis, &mut steps, &mut waiting);
        steps.push(Step::Triple(wires));
    }
    for index in 0..waiting.len() {
        if let Some(form) = waiting[index] {
            basis.make(form, 0, &wanted);
            cx += basis.moves.len();
            flush(&mut basis, &mut steps, &mut waiting);
        }
    }
    basis.settle(goal);
    cx += basis.moves.len();
    flush(&mut basis, &mut steps, &mut waiting);
    (cx, steps)
}

fn eighths(terms: &[Term]) -> usize {
    let mut seen: Vec<u64> = Vec::with_capacity(terms.len() * 7);
    for [a, b, c] in terms {
        seen.extend([*a, *b, *c, a ^ b, a ^ c, b ^ c, a ^ b ^ c]);
    }
    seen.sort_unstable();
    let mut odd = 0;
    let mut at = 0;
    while at < seen.len() {
        let mut end = at;
        while end < seen.len() && seen[end] == seen[at] {
            end += 1;
        }
        odd += (end - at) & 1;
        at = end;
    }
    odd
}

fn score(width: usize, terms: &[Term], goal: &[u64]) -> usize {
    let (cx, _) = schedule(width, terms, &[], goal, 0);
    FULL * terms.len() + cx + eighths(terms)
}

fn insertions(terms: &[Term], term: Term) -> Vec<Vec<Term>> {
    let mut found = Vec::new();
    let mut plain = terms.to_vec();
    toggle(&mut plain, term);
    found.push(plain);
    for (index, held) in terms.iter().enumerate() {
        let both = shared(held, &term);
        if both.len() != 2 {
            continue;
        }
        let merged = [
            both[0],
            both[1],
            rest(held, &both)[0] ^ rest(&term, &both)[0],
        ];
        let Some(merged) = tidy(merged) else {
            continue;
        };
        let mut next = terms.to_vec();
        next.swap_remove(index);
        if let Some(merged) = merged {
            toggle(&mut next, merged);
        }
        found.push(next);
    }
    found
}

struct Swap {
    left: Term,
    right: Term,
    shared: u64,
    forms: [u64; 4],
}

fn swaps(terms: &[Term]) -> Vec<Swap> {
    let mut found = Vec::new();
    for i in 0..terms.len() {
        for j in i + 1..terms.len() {
            let both = shared(&terms[i], &terms[j]);
            if both.len() != 1 {
                continue;
            }
            let [p, q] = rest(&terms[i], &both)[..] else {
                continue;
            };
            let [r, s] = rest(&terms[j], &both)[..] else {
                continue;
            };
            for (r, s) in [(r, s), (s, r)] {
                found.push(Swap {
                    left: terms[i],
                    right: terms[j],
                    shared: both[0],
                    forms: [p, q, r, s],
                });
            }
        }
    }
    found
}

fn apply(terms: &[Term], swap: &Swap) -> Vec<Vec<Term>> {
    let g = swap.shared;
    let [p, q, r, s] = swap.forms;
    let (Some(joined), Some(first), Some(second)) =
        (tidy([g, p ^ r, q ^ s]), tidy([g, p, s]), tidy([g, r, q]))
    else {
        return Vec::new();
    };
    if !terms.contains(&swap.left) || !terms.contains(&swap.right) {
        return Vec::new();
    }
    let mut base: Vec<Term> = terms
        .iter()
        .copied()
        .filter(|term| *term != swap.left && *term != swap.right)
        .collect();
    if let Some(joined) = joined {
        toggle(&mut base, joined);
    }
    let mut stage = vec![base];
    for extra in [first, second].into_iter().flatten() {
        stage = stage
            .iter()
            .flat_map(|held| insertions(held, extra))
            .collect();
    }
    stage
}

fn variety(terms: &[Term]) -> usize {
    let mut forms: Vec<u64> = terms.iter().flatten().copied().collect();
    forms.sort_unstable();
    forms.dedup();
    forms.len()
}

fn neighbours(terms: &[Term]) -> Vec<Vec<Term>> {
    let swaps = swaps(terms);
    let mut found: Vec<Vec<Term>> = swaps.iter().flat_map(|swap| apply(terms, swap)).collect();
    let mut groups: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (index, swap) in swaps.iter().enumerate() {
        let [p, q, r, s] = swap.forms;
        for (x, y) in [(p, r), (q, s)] {
            groups.entry((x.min(y), x.max(y))).or_default().push(index);
        }
    }
    let mut keys: Vec<(u64, u64)> = groups.keys().copied().collect();
    keys.sort_unstable();
    let mut grid: HashMap<(u64, u64), HashMap<(u64, u64), usize>> = HashMap::new();
    for (index, swap) in swaps.iter().enumerate() {
        let [p, q, r, s] = swap.forms;
        let first = (p.min(r), p.max(r));
        let second = (q.min(s), q.max(s));
        grid.entry(first).or_default().insert(second, index);
        grid.entry(second).or_default().insert(first, index);
    }
    for (at, one) in keys.iter().enumerate() {
        for two in &keys[at + 1..] {
            let mut common: Vec<(u64, u64)> = grid[one]
                .keys()
                .copied()
                .filter(|key| grid[two].contains_key(key) && key > two)
                .collect();
            common.sort_unstable();
            for (edge, three) in common.iter().enumerate() {
                for four in &common[edge + 1..] {
                    let picked = [
                        grid[one][three],
                        grid[one][four],
                        grid[two][three],
                        grid[two][four],
                    ];
                    let mut held = terms.to_vec();
                    for index in picked {
                        let next = apply(&held, &swaps[index])
                            .into_iter()
                            .min_by_key(|next| (next.len(), variety(next)));
                        match next {
                            Some(next) => held = next,
                            None => break,
                        }
                    }
                    if held.len() + 4 == terms.len() {
                        found.push(held);
                    }
                }
            }
        }
    }
    for key in keys {
        let members = &groups[&key];
        if members.len() < 2 {
            continue;
        }
        let mut held = terms.to_vec();
        let mut used = 0;
        for &index in members {
            let next = apply(&held, &swaps[index])
                .into_iter()
                .min_by_key(|next| (next.len(), variety(next)));
            if let Some(next) = next.filter(|next| next.len() < held.len()) {
                held = next;
                used += 1;
            }
        }
        if used > 1 {
            found.push(held);
        }
    }
    found
}

fn search(width: usize, start: &[Term], goal: &[u64]) -> Vec<Vec<Term>> {
    let key = |terms: &[Term]| {
        let mut sorted = terms.to_vec();
        sorted.sort_unstable();
        sorted
    };
    let mut path = Vec::new();
    let mut held = start.to_vec();
    let mut best = score(width, &held, goal);
    let mut seen: HashSet<Vec<Term>> = HashSet::new();
    seen.insert(key(&held));
    let mut stale = 0;
    loop {
        let mut chosen: Option<(usize, Vec<Term>)> = None;
        let mut around: Vec<Vec<Term>> = neighbours(&held)
            .into_iter()
            .filter(|next| !seen.contains(&key(next)))
            .collect();
        around.sort_by_cached_key(|next| FULL * next.len() + eighths(next) + variety(next));
        around.truncate(TOP);
        for next in around {
            let found = score(width, &next, goal);
            if chosen.as_ref().is_none_or(|(least, _)| found < *least) {
                chosen = Some((found, next));
            }
        }
        let Some((found, next)) = chosen else {
            break;
        };
        held = next;
        seen.insert(key(&held));
        if found < best {
            best = found;
            stale = 0;
            path.push(held.clone());
        } else {
            stale += 1;
            if stale > PATIENCE {
                break;
            }
        }
    }
    path
}

struct Region {
    wires: Vec<QubitId>,
    terms: Vec<Term>,
    turns: Vec<(u64, Op)>,
    goal: Vec<u64>,
}

fn read(run: &[Op]) -> Option<Region> {
    let mut wires: Vec<QubitId> = Vec::new();
    for op in run {
        for wire in op.qubits() {
            if !wires.contains(&wire) {
                wires.push(wire);
            }
        }
    }
    if wires.len() > WIDEST {
        return None;
    }
    wires.sort_unstable();
    let at = |wire: QubitId| wires.iter().position(|held| *held == wire).expect("listed");
    let mut rows: Vec<u64> = (0..wires.len()).map(|k| 1 << k).collect();
    let mut terms = Vec::new();
    let mut turns = Vec::new();
    for op in run {
        match classify(op) {
            Kind::Cx(c, t) => rows[at(t)] ^= rows[at(c)],
            Kind::Ccz([a, b, t]) => {
                let term = tidy([rows[at(a)], rows[at(b)], rows[at(t)]])??;
                toggle(&mut terms, term);
            }
            Kind::Turn(t) => turns.push((rows[at(t)], op.clone())),
            Kind::Other => return None,
        }
    }
    Some(Region {
        wires,
        terms,
        turns,
        goal: rows,
    })
}

fn write(region: &Region, terms: &[Term], tries: u64) -> Vec<Op> {
    let width = region.wires.len();
    let forms: Vec<u64> = region.turns.iter().map(|(form, _)| *form).collect();
    let (_, steps) = (0..tries.max(1))
        .map(|seed| schedule(width, terms, &forms, &region.goal, seed))
        .min_by_key(|(cx, _)| *cx)
        .expect("one try");
    let gate = |kind, controls: Vec<QubitId>, target| {
        Op::Gate(Gate {
            kind,
            controls,
            targets: vec![target],
            params: Vec::new(),
            span: Span::DUMMY,
        })
    };
    let wire = |index: usize| region.wires[index];
    steps
        .into_iter()
        .map(|step| match step {
            Step::Add(source, sink) => gate(GateKind::X, vec![wire(source)], wire(sink)),
            Step::Triple([a, b, t]) => gate(GateKind::Z, vec![wire(a), wire(b)], wire(t)),
            Step::Turn(index, target) => {
                let mut op = region.turns[index].1.clone();
                if let Op::Gate(gate) = &mut op {
                    gate.targets = vec![wire(target)];
                }
                op
            }
        })
        .collect()
}

fn rework(run: Vec<Op>) -> (Vec<Op>, bool) {
    let triples = run
        .iter()
        .filter(|op| matches!(classify(op), Kind::Ccz(_)))
        .count();
    if triples < 2 {
        return (run, false);
    }
    let Some(region) = read(&run) else {
        return (run, false);
    };
    let path = search(region.wires.len(), &region.terms, &region.goal);
    let Some(chosen) = path.last() else {
        return (run, false);
    };
    (write(&region, chosen, TRIES), true)
}

fn plain_h(op: &Op) -> Option<QubitId> {
    let gate = op.as_gate()?;
    match (gate.kind, &gate.controls[..], &gate.targets[..]) {
        (GateKind::H, [], [wire]) => Some(*wire),
        _ => None,
    }
}

#[derive(Default)]
struct Sweep {
    out: Vec<Op>,
    run: Vec<Op>,
    hoisted: Vec<Op>,
    sunk: Vec<Op>,
    flipped: Vec<(QubitId, Op)>,
    touched: Vec<QubitId>,
    blocked: Vec<QubitId>,
    changed: usize,
}

impl Sweep {
    fn close(&mut self) {
        self.out.append(&mut self.hoisted);
        let (after, moved) = rework(mem::take(&mut self.run));
        self.out.extend(after);
        self.out.extend(self.flipped.drain(..).map(|(_, op)| op));
        self.out.append(&mut self.sunk);
        self.touched.clear();
        self.blocked.clear();
        self.changed += usize::from(moved);
    }

    fn turned(&self, wire: QubitId) -> bool {
        self.flipped.iter().any(|(held, _)| *held == wire)
    }

    fn sink(&mut self, op: Op) {
        for wire in op.qubits() {
            if let Some(at) = self.flipped.iter().position(|(held, _)| *held == wire) {
                let (_, turn) = self.flipped.remove(at);
                self.sunk.push(turn);
            }
            self.blocked.push(wire);
        }
        self.sunk.push(op);
    }

    fn take(&mut self, mut op: Op) {
        let wires = op.qubits();
        if !matches!(classify(&op), Kind::Other) {
            let mut turned = wires.iter().filter(|wire| self.turned(**wire)).count();
            let pair = matches!(classify(&op), Kind::Cx(..));
            let fresh = wires
                .iter()
                .copied()
                .find(|wire| !self.touched.contains(wire) && !self.blocked.contains(wire));
            if let (true, 1, Some(wire)) = (pair, turned, fresh) {
                let model = self.flipped.iter().find(|(held, _)| wires.contains(held));
                let mut turn = model.expect("one turned wire").1.clone();
                if let Op::Gate(gate) = &mut turn {
                    gate.targets = vec![wire];
                }
                self.hoisted.push(turn.clone());
                self.flipped.push((wire, turn));
                turned = 2;
            }
            let swaps = turned == 2 && pair;
            if wires.iter().any(|wire| self.blocked.contains(wire)) || (turned > 0 && !swaps) {
                self.close();
            } else if swaps && let Op::Gate(gate) = &mut op {
                mem::swap(&mut gate.controls[0], &mut gate.targets[0]);
            }
            for wire in wires {
                if !self.touched.contains(&wire) {
                    self.touched.push(wire);
                }
            }
            self.run.push(op);
        } else if op.as_gate().is_none() {
            self.close();
            self.out.push(op);
        } else if self.run.is_empty() {
            self.out.push(op);
        } else if wires
            .iter()
            .all(|wire| !self.touched.contains(wire) && !self.blocked.contains(wire))
        {
            self.hoisted.push(op);
        } else if let Some(wire) = plain_h(&op).filter(|wire| !self.blocked.contains(wire)) {
            match self.flipped.iter().position(|(held, _)| *held == wire) {
                Some(at) => {
                    self.flipped.remove(at);
                }
                None => self.flipped.push((wire, op)),
            }
        } else {
            self.sink(op);
        }
    }
}

pub fn refactor(program: &mut Program) -> usize {
    let mut changed = 0;
    for block in &mut program.blocks {
        let mut sweep = Sweep::default();
        for op in mem::take(&mut block.ops) {
            sweep.take(op);
        }
        sweep.close();
        changed += sweep.changed;
        block.ops = sweep.out;
    }
    changed
}
