use std::collections::HashMap;

use crate::diag::Span;
use crate::ir::*;
use crate::phase;

pub(crate) mod gauge;

const EPSILON: f64 = 1e-9;
const MAX_WIRES: usize = 128;
const GREEDY_WIRES: usize = 40;
const WORK: usize = 400_000;
const MAX_TRIES: usize = 48;
const MAX_ROUNDS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Cx(usize, usize),
    Rz(usize, f64),
    X(usize),
}

impl Step {
    fn wires(self) -> (usize, Option<usize>) {
        match self {
            Step::Cx(c, t) => (c, Some(t)),
            Step::Rz(q, _) | Step::X(q) => (q, None),
        }
    }

    fn map(self, f: impl Fn(usize) -> usize) -> Step {
        match self {
            Step::Cx(c, t) => Step::Cx(f(c), f(t)),
            Step::Rz(q, angle) => Step::Rz(f(q), angle),
            Step::X(q) => Step::X(f(q)),
        }
    }

    fn gate(self, span: Span) -> Gate {
        let (kind, controls, target, params) = match self {
            Step::Cx(c, t) => (GateKind::X, vec![QubitId(c as u32)], t, Vec::new()),
            Step::X(q) => (GateKind::X, Vec::new(), q, Vec::new()),
            Step::Rz(q, angle) => (
                GateKind::Rz,
                Vec::new(),
                q,
                vec![Operand::Const(Const::Float(angle))],
            ),
        };
        Gate {
            kind,
            controls,
            targets: vec![QubitId(target as u32)],
            params,
            span,
        }
    }
}

fn classify(gate: &Gate) -> Option<Step> {
    if !gate.has_valid_shape() {
        return None;
    }
    let target = gate.targets.first()?.index();
    match (gate.kind, &gate.controls[..]) {
        (GateKind::X, []) => Some(Step::X(target)),
        (GateKind::X, [c]) => Some(Step::Cx(c.index(), target)),
        (kind, []) if phase::diagonal(kind) && kind != GateKind::I => {
            phase::slope(gate).map(|angle| Step::Rz(target, angle))
        }
        _ => None,
    }
}

#[derive(Debug)]
struct Spec {
    terms: Vec<(u128, f64)>,
    rows: Vec<u128>,
    flips: u128,
}

impl Spec {
    fn of(steps: &[Step], wires: usize) -> Spec {
        let mut rows: Vec<u128> = (0..wires).map(|q| 1 << q).collect();
        let mut flips = 0u128;
        let mut terms: HashMap<u128, f64> = HashMap::new();
        for &step in steps {
            match step {
                Step::Cx(c, t) => {
                    rows[t] ^= rows[c];
                    flips ^= (flips >> c & 1) << t;
                }
                Step::X(q) => flips ^= 1 << q,
                Step::Rz(q, angle) => {
                    let signed = if flips >> q & 1 == 1 { -angle } else { angle };
                    *terms.entry(rows[q]).or_insert(0.0) += signed;
                }
            }
        }
        let mut terms: Vec<(u128, f64)> = terms
            .into_iter()
            .map(|(parity, angle)| (parity, phase::wrap(angle)))
            .filter(|(_, angle)| angle.abs() > EPSILON)
            .collect();
        terms.sort_by_key(|(parity, _)| *parity);
        Spec { terms, rows, flips }
    }

    fn same(&self, other: &Spec) -> bool {
        self.rows == other.rows
            && self.flips == other.flips
            && self.terms.len() == other.terms.len()
            && self
                .terms
                .iter()
                .zip(&other.terms)
                .all(|(a, b)| a.0 == b.0 && phase::wrap(a.1 - b.1).abs() < 1e-7)
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() >> 11) as usize % bound.max(1)
    }
}

fn weight(rows: &[u128]) -> usize {
    rows.iter().map(|row| row.count_ones() as usize).sum()
}

fn transpose(rows: &[u128]) -> Vec<u128> {
    let n = rows.len();
    (0..n)
        .map(|column| {
            (0..n)
                .filter(|&row| rows[row] >> column & 1 == 1)
                .fold(0u128, |acc, row| acc | 1 << row)
        })
        .collect()
}

fn eliminate(rows: &mut [u128], order: &[usize]) -> Option<Vec<(usize, usize)>> {
    let n = rows.len();
    let mut done = vec![false; n];
    let mut moves = Vec::new();
    for &p in order {
        if rows[p] >> p & 1 == 0 {
            let k = (0..n)
                .filter(|&k| k != p && !done[k] && rows[k] >> p & 1 == 1)
                .min_by_key(|&k| rows[k].count_ones())?;
            rows[p] ^= rows[k];
            moves.push((p, k));
        }
        for k in 0..n {
            if k != p && rows[k] >> p & 1 == 1 {
                rows[k] ^= rows[p];
                moves.push((k, p));
            }
        }
        done[p] = true;
    }
    Some(moves)
}

fn invert(rows: &[u128]) -> Option<Vec<u128>> {
    let n = rows.len();
    let mut work = rows.to_vec();
    let mut inverse: Vec<u128> = (0..n).map(|q| 1 << q).collect();
    let order: Vec<usize> = (0..n).collect();
    for (dst, src) in eliminate(&mut work, &order)? {
        inverse[dst] ^= inverse[src];
    }
    Some(inverse)
}

fn linear(map: &[u128], order: &[usize], greedy: bool) -> Option<Vec<(usize, usize)>> {
    let n = map.len();
    let mut map = map.to_vec();
    let mut inverse = invert(&map)?;
    let mut out = Vec::new();

    if greedy && n <= GREEDY_WIRES {
        loop {
            let now = weight(&map) + weight(&inverse);
            if map.iter().enumerate().all(|(q, row)| *row == 1 << q) {
                return Some(out);
            }
            let mut best: Option<(usize, usize, usize)> = None;
            for c in 0..n {
                for t in 0..n {
                    if c == t {
                        continue;
                    }
                    let column: i64 = map
                        .iter()
                        .filter(|row| *row >> t & 1 == 1)
                        .map(|row| if row >> c & 1 == 1 { -1 } else { 1 })
                        .sum();
                    let row = (inverse[t] ^ inverse[c]).count_ones() as i64
                        - inverse[t].count_ones() as i64;
                    let after = (now as i64 + column + row) as usize;
                    if after < now && best.is_none_or(|(score, ..)| after < score) {
                        best = Some((after, c, t));
                    }
                }
            }
            let Some((_, c, t)) = best else {
                break;
            };
            for row in &mut map {
                *row ^= (*row >> t & 1) << c;
            }
            inverse[t] ^= inverse[c];
            out.push((c, t));
        }
    }

    let mut by_rows = inverse.clone();
    let rows: Vec<(usize, usize)> = eliminate(&mut by_rows, order)?
        .into_iter()
        .map(|(dst, src)| (src, dst))
        .collect();
    let mut by_columns = transpose(&map);
    let columns = eliminate(&mut by_columns, order)?;
    out.extend(if rows.len() <= columns.len() {
        rows
    } else {
        columns
    });
    Some(out)
}

struct Net {
    wires: usize,
    left: Vec<(u128, f64)>,
    map: Vec<u128>,
    steps: Vec<Step>,
    cx: usize,
}

impl Net {
    fn new(spec: &Spec) -> Net {
        let mut net = Net {
            wires: spec.rows.len(),
            left: spec.terms.clone(),
            map: spec.rows.clone(),
            steps: Vec::new(),
            cx: 0,
        };
        net.flush();
        net
    }

    fn flush(&mut self) {
        let steps = &mut self.steps;
        self.left.retain(|&(vector, angle)| {
            if vector.count_ones() == 1 {
                steps.push(Step::Rz(vector.trailing_zeros() as usize, angle));
                false
            } else {
                true
            }
        });
    }

    fn cx(&mut self, c: usize, t: usize) {
        for (vector, _) in &mut self.left {
            *vector ^= (*vector >> t & 1) << c;
        }
        for row in &mut self.map {
            *row ^= (*row >> t & 1) << c;
        }
        self.steps.push(Step::Cx(c, t));
        self.cx += 1;
        self.flush();
    }

    fn gray(&mut self) {
        let all: Vec<u128> = self.left.iter().map(|(vector, _)| *vector).collect();
        let mut live: Vec<bool> = vec![true; all.len()];
        let mut vectors = all;
        let free: Vec<usize> = (0..self.wires).collect();
        let mut stack: Vec<(Vec<usize>, Vec<usize>, Option<usize>)> =
            vec![((0..vectors.len()).collect(), free, None)];
        while let Some((set, free, target)) = stack.pop() {
            let mut set: Vec<usize> = set.into_iter().filter(|&i| live[i]).collect();
            if set.is_empty() {
                continue;
            }
            if let Some(q) = target {
                loop {
                    let common = set.iter().fold(!0u128, |acc, &i| acc & vectors[i]) & !(1 << q);
                    if common == 0 || set.iter().any(|&i| vectors[i] >> q & 1 == 0) {
                        break;
                    }
                    let c = common.trailing_zeros() as usize;
                    for (i, vector) in vectors.iter_mut().enumerate() {
                        if live[i] {
                            *vector ^= (*vector >> q & 1) << c;
                        }
                    }
                    self.cx(c, q);
                    for (i, vector) in vectors.iter().enumerate() {
                        if live[i] && vector.count_ones() == 1 {
                            live[i] = false;
                        }
                    }
                    set.retain(|&i| live[i]);
                    if set.is_empty() {
                        break;
                    }
                }
            }
            if set.is_empty() || free.is_empty() {
                continue;
            }
            let split = free
                .iter()
                .copied()
                .max_by_key(|&j| {
                    let ones = set.iter().filter(|&&i| vectors[i] >> j & 1 == 1).count();
                    ones.max(set.len() - ones)
                })
                .unwrap_or(free[0]);
            let (ones, zeros): (Vec<usize>, Vec<usize>) =
                set.iter().partition(|&&i| vectors[i] >> split & 1 == 1);
            let rest: Vec<usize> = free.into_iter().filter(|&j| j != split).collect();
            stack.push((ones, rest.clone(), target.or(Some(split))));
            stack.push((zeros, rest, target));
        }
    }

    fn greedy(&mut self, rng: &mut Rng, pull: usize, noise: bool) {
        while !self.left.is_empty() {
            let lightest = self
                .left
                .iter()
                .map(|(vector, _)| vector.count_ones())
                .min()
                .unwrap_or(0);
            let mut candidates: Vec<u128> = self
                .left
                .iter()
                .map(|(vector, _)| *vector)
                .filter(|vector| vector.count_ones() == lightest)
                .collect();
            while candidates.len() > 6 {
                candidates.swap_remove(rng.below(candidates.len()));
            }
            let mut best: Option<(usize, u128, usize)> = None;
            for &vector in &candidates {
                for t in (0..self.wires).filter(|&t| vector >> t & 1 == 1) {
                    let others = vector & !(1 << t);
                    let after = |v: u128| {
                        if v >> t & 1 == 1 {
                            (v ^ others).count_ones() as usize
                        } else {
                            v.count_ones() as usize
                        }
                    };
                    let mut score: usize = self.left.iter().map(|(v, _)| after(*v)).sum::<usize>()
                        * 4
                        + self.map.iter().map(|v| after(*v)).sum::<usize>() * pull;
                    if noise {
                        score = score * 4 + rng.below(6);
                    }
                    if best.is_none_or(|(least, ..)| score < least) {
                        best = Some((score, vector, t));
                    }
                }
            }
            let Some((_, vector, t)) = best else {
                break;
            };
            let mut controls: Vec<usize> = (0..self.wires)
                .filter(|&c| c != t && vector >> c & 1 == 1)
                .collect();
            if noise {
                for i in (1..controls.len()).rev() {
                    controls.swap(i, rng.below(i + 1));
                }
            }
            for c in controls {
                self.cx(c, t);
            }
        }
    }

    fn finish(mut self, spec: &Spec, order: &[usize], greedy: bool) -> Option<Vec<Step>> {
        if !self.left.is_empty() {
            return None;
        }
        for (c, t) in linear(&self.map, order, greedy)? {
            self.steps.push(Step::Cx(c, t));
        }
        self.steps.extend(
            (0..self.wires)
                .filter(|&q| spec.flips >> q & 1 == 1)
                .map(Step::X),
        );
        Some(self.steps)
    }
}

fn price(steps: &[Step]) -> (usize, usize) {
    let cx = steps
        .iter()
        .filter(|step| matches!(step, Step::Cx(..)))
        .count();
    (cx, steps.len())
}

fn synthesize(spec: &Spec, rng: &mut Rng) -> Option<Vec<Step>> {
    let wires = spec.rows.len();
    let natural: Vec<usize> = (0..wires).collect();
    let size = (spec.terms.len() + wires) * wires * wires.max(spec.terms.len()) + 1;
    let tries = (WORK / size).clamp(3, MAX_TRIES);
    let mut best: Option<Vec<Step>> = None;
    for attempt in 0..tries {
        let mut net = Net::new(spec);
        match attempt {
            0 => net.gray(),
            1 => net.greedy(rng, 1, false),
            2 => net.greedy(rng, 0, false),
            3 => net.greedy(rng, 4, false),
            _ => net.greedy(rng, attempt % 3, true),
        }
        if !net.left.is_empty() {
            net.greedy(rng, 1, false);
        }
        if best
            .as_ref()
            .is_some_and(|found| price(found).0 <= net.cx && attempt > 0)
        {
            continue;
        }
        let mut order = natural.clone();
        if attempt > 3 {
            for i in (1..wires).rev() {
                order.swap(i, rng.below(i + 1));
            }
        }
        for greedy in [false, true] {
            let net = Net {
                wires,
                left: net.left.clone(),
                map: net.map.clone(),
                steps: net.steps.clone(),
                cx: net.cx,
            };
            let Some(steps) = net.finish(spec, &order, greedy) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|found| price(&steps) < price(found))
            {
                best = Some(steps);
            }
        }
    }
    best.filter(|steps| Spec::of(steps, wires).same(spec))
}

#[derive(Clone, Copy)]
struct Options {
    push: bool,
    pin: bool,
}

const PLANS: [Options; 4] = [
    Options {
        pin: true,
        push: true,
    },
    Options {
        pin: false,
        push: true,
    },
    Options {
        pin: true,
        push: false,
    },
    Options {
        pin: false,
        push: false,
    },
];

struct Region {
    prefix: Vec<Gate>,
    steps: Vec<Step>,
    span: Span,
    suffix: Vec<Gate>,
    soft: Vec<bool>,
}

const CONTROL: u8 = 1;
const TARGET: u8 = 2;
const OPEN: u8 = CONTROL | TARGET;

fn uses(gate: &Gate, step: Option<Step>, pin: bool) -> Vec<(usize, u8)> {
    match step {
        Some(Step::Cx(c, t)) if pin => vec![(c, CONTROL), (t, TARGET)],
        Some(Step::Rz(q, _)) if pin => vec![(q, CONTROL)],
        Some(Step::X(q)) if pin => vec![(q, TARGET)],
        _ => gate.wires().map(|q| (q.index(), 0)).collect(),
    }
}

fn carve(pending: Vec<Gate>, qubits: usize, options: &Options) -> Region {
    let mut touched = vec![false; qubits];
    let mut state = vec![OPEN; qubits];
    let mut met = vec![false; qubits];
    let mut soft = vec![false; qubits];
    let mut open = qubits;
    let mut region = Region {
        prefix: Vec::new(),
        steps: Vec::new(),
        span: Span::DUMMY,
        suffix: Vec::new(),
        soft: Vec::new(),
    };
    for gate in pending {
        if open == 0 {
            region.suffix.push(gate);
            continue;
        }
        let step = classify(&gate);
        let uses = uses(&gate, step, options.pin);
        let fits = match step {
            Some(_) if options.pin => uses.iter().all(|&(w, u)| state[w] & u != 0),
            Some(_) => uses.iter().all(|&(w, _)| state[w] == OPEN),
            None => false,
        };
        if let (Some(step), true) = (step, fits) {
            if region.steps.is_empty() {
                region.span = gate.span;
            }
            for &(w, _) in &uses {
                touched[w] = true;
            }
            region.steps.push(step);
        } else if step.is_none() && uses.iter().all(|&(w, _)| state[w] == OPEN && !touched[w]) {
            region.prefix.push(gate);
        } else {
            for &(w, u) in &uses {
                if !met[w] {
                    met[w] = true;
                    soft[w] = step.is_some();
                }
                if state[w] != 0 && state[w] & u == 0 {
                    open -= 1;
                }
                state[w] &= u;
            }
            region.suffix.push(gate);
        }
    }
    region.soft = soft;
    region
}

fn improve(steps: Vec<Step>, rng: &mut Rng) -> Vec<Step> {
    let before = price(&steps);
    if before.0 < 2 {
        return steps;
    }
    let mut wires: Vec<usize> = Vec::new();
    for step in &steps {
        let (a, b) = step.wires();
        wires.extend([Some(a), b].into_iter().flatten());
    }
    wires.sort_unstable();
    wires.dedup();
    if wires.len() > MAX_WIRES {
        return steps;
    }
    let local: HashMap<usize, usize> = wires.iter().enumerate().map(|(i, &w)| (w, i)).collect();
    let inner: Vec<Step> = steps.iter().map(|step| step.map(|w| local[&w])).collect();
    let spec = Spec::of(&inner, wires.len());
    match synthesize(&spec, rng) {
        Some(found) if price(&found) < before => found
            .into_iter()
            .map(|step| step.map(|w| wires[w]))
            .collect(),
        _ => steps,
    }
}

fn split(steps: &mut Vec<Step>, soft: &[bool]) -> Vec<Step> {
    let mut locked = vec![false; soft.len()];
    let mut keep = vec![true; steps.len()];
    for (index, step) in steps.iter().enumerate().rev() {
        let (a, b) = step.wires();
        let wires = [Some(a), b];
        if wires.iter().flatten().all(|&w| soft[w] && !locked[w]) {
            keep[index] = false;
        } else {
            for &w in wires.iter().flatten() {
                locked[w] = true;
            }
        }
    }
    let mut pushed = Vec::new();
    let mut index = 0;
    steps.retain(|step| {
        index += 1;
        if keep[index - 1] {
            true
        } else {
            pushed.push(*step);
            false
        }
    });
    pushed
}

fn rewrite(
    gates: Vec<Gate>,
    qubits: usize,
    backward: bool,
    options: &Options,
    rng: &mut Rng,
) -> Vec<Gate> {
    let mut out = Vec::with_capacity(gates.len());
    let mut pending = gates;
    if backward {
        pending.reverse();
    }
    while !pending.is_empty() {
        let mut region = carve(pending, qubits, options);
        if backward {
            region.steps.reverse();
        }
        let mut steps = improve(region.steps, rng);
        if backward {
            steps.reverse();
        }
        let mut next = Vec::with_capacity(region.suffix.len());
        if options.push && !region.suffix.is_empty() {
            let mut kept = steps.clone();
            let pushed = split(&mut kept, &region.soft);
            if !kept.is_empty() || !region.prefix.is_empty() {
                steps = kept;
                next.extend(pushed.into_iter().map(|step| step.gate(region.span)));
            }
        }
        out.extend(region.prefix);
        out.extend(steps.into_iter().map(|step| step.gate(region.span)));
        next.extend(region.suffix);
        pending = next;
    }
    if backward {
        out.reverse();
    }
    out
}

fn tally(gates: &[Gate]) -> (usize, usize) {
    let cx = gates.iter().filter(|gate| gate.wires().count() > 1).count();
    (cx, gates.len())
}

fn settle(gates: Vec<Gate>, qubits: usize, rng: &mut Rng) -> Vec<Gate> {
    let mut best = gates;
    let mut idle = 0;
    for round in 0..MAX_ROUNDS * PLANS.len() {
        let plan = PLANS[round / 2 % PLANS.len()];
        let found = rewrite(best.clone(), qubits, round % 2 == 1, &plan, rng);
        if tally(&found) < tally(&best) {
            best = found;
            idle = 0;
        } else {
            idle += 1;
            if idle >= 2 * PLANS.len() {
                break;
            }
        }
    }
    best
}

pub(crate) fn resynthesize(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut saved = 0;
    for block in &mut program.blocks {
        let before = block.ops.len();
        let mut out: Vec<Op> = Vec::with_capacity(before);
        let mut run: Vec<Gate> = Vec::new();
        let mut flush = |run: &mut Vec<Gate>, out: &mut Vec<Op>| {
            let gates = std::mem::take(run);
            let (cx, total) = tally(&gates);
            let found = settle(gates, qubits, &mut rng);
            let (less, fewer) = tally(&found);
            saved += (cx + total).saturating_sub(less + fewer);
            out.extend(found.into_iter().map(Op::Gate));
        };
        for op in block.ops.drain(..) {
            match op {
                Op::Gate(gate) if gate.wires().all(|q| q.index() < qubits) => run.push(gate),
                other => {
                    flush(&mut run, &mut out);
                    out.push(other);
                }
            }
        }
        flush(&mut run, &mut out);
        block.ops = out;
    }
    saved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equiv;
    use crate::qasm;

    const GATES: [&str; 12] = [
        "cx", "cx", "cx", "cx", "t", "tdg", "rz(0.3)", "s", "x", "h", "cx", "z",
    ];

    fn random(rng: &mut Rng, qubits: usize, length: usize, hadamards: bool) -> String {
        let mut text = format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n");
        for q in 0..qubits {
            text += &format!("ry(0.{}) q[{q}];\nrz(1.{}) q[{q}];\n", 3 + q, 1 + q);
        }
        for _ in 0..length {
            let gate = GATES[rng.below(GATES.len())];
            let a = rng.below(qubits);
            if gate == "cx" {
                let b = (a + 1 + rng.below(qubits - 1)) % qubits;
                text += &format!("cx q[{a}], q[{b}];\n");
            } else if gate != "h" || hadamards {
                text += &format!("{gate} q[{a}];\n");
            }
        }
        text
    }

    fn cx(program: &Program) -> usize {
        program
            .gates()
            .filter(|gate| gate.wires().count() > 1)
            .count()
    }

    #[test]
    fn resynthesizes() {
        let mut rng = Rng(77);
        let (mut before, mut after) = (0, 0);
        for round in 0..400 {
            let qubits = 2 + round % 6;
            let source = random(&mut rng, qubits, 8 + round % 60, round % 4 != 0);
            let (original, errors) = qasm::lower(&source);
            assert!(errors.is_empty(), "{errors:?}");
            let mut rewritten = original.clone();
            resynthesize(&mut rewritten);
            let differences = equiv::compare(
                &equiv::explore(&original),
                &equiv::explore(&rewritten),
                true,
            );
            assert!(differences.is_empty(), "{differences:?}\n{source}");
            assert!(cx(&rewritten) <= cx(&original), "{source}");
            before += cx(&original);
            after += cx(&rewritten);
        }
        assert!(after < before, "{after} of {before}");
    }

    #[test]
    fn linear_maps() {
        let mut rng = Rng(5);
        for round in 0..300 {
            let wires = 2 + round % 9;
            let mut steps = Vec::new();
            for _ in 0..3 * wires {
                let c = rng.below(wires);
                let t = (c + 1 + rng.below(wires - 1)) % wires;
                steps.push(Step::Cx(c, t));
            }
            let spec = Spec::of(&steps, wires);
            let natural: Vec<usize> = (0..wires).collect();
            for greedy in [false, true] {
                let moves = linear(&spec.rows, &natural, greedy).expect("linear");
                let steps: Vec<Step> = moves.iter().map(|&(c, t)| Step::Cx(c, t)).collect();
                assert!(
                    Spec::of(&steps, wires).same(&spec),
                    "{greedy} {spec:?} {steps:?}"
                );
            }
            let found = synthesize(&spec, &mut rng).expect("synthesis");
            assert!(Spec::of(&found, wires).same(&spec));
        }
    }
}
