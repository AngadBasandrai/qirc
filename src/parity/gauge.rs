use std::collections::HashMap;
use std::env;
use std::f64::consts::PI;
use std::sync::OnceLock;

use super::{CONTROL, EPSILON, OPEN, Rng, Spec, Step, TARGET, classify};
use crate::diag::Span;
use crate::ir::*;
use crate::phase;

const WIDTH: usize = 4;
const REACH: usize = 600;
const SWEEPS: usize = 12;
const NONE: u8 = 255;

struct Table {
    wires: usize,
    bits: usize,
    index: [u8; 16],
    rank: Vec<u16>,
    cost: Vec<u8>,
}

fn pack(rows: &[u8]) -> usize {
    rows.iter()
        .enumerate()
        .fold(0, |acc, (q, row)| acc | (*row as usize) << (4 * q))
}

fn pairs(wires: usize) -> Vec<(usize, usize)> {
    (0..wires)
        .flat_map(|c| (0..wires).filter(move |&t| t != c).map(move |t| (c, t)))
        .collect()
}

impl Table {
    fn build(wires: usize) -> Table {
        let moves = pairs(wires);
        let mut index = [NONE; 16];
        let mut bits = 0;
        for (parity, slot) in index.iter_mut().enumerate().take(1 << wires).skip(1) {
            if parity.count_ones() > 1 {
                *slot = bits as u8;
                bits += 1;
            }
        }
        let mut rank = vec![u16::MAX; 1 << 16];
        let identity: Vec<u8> = (0..wires).map(|q| 1 << q).collect();
        let mut list = vec![pack(&identity)];
        rank[list[0]] = 0;
        let mut head = 0;
        while head < list.len() {
            let key = list[head];
            head += 1;
            for &(c, t) in &moves {
                let next = key ^ ((key >> (4 * c) & 15) << (4 * t));
                if rank[next] == u16::MAX {
                    rank[next] = list.len() as u16;
                    list.push(next);
                }
            }
        }
        let cost = Table::load(wires, list.len() << bits).unwrap_or_else(|| {
            let cost = Table::search(&moves, &index, &rank, &list, bits);
            Table::store(wires, &cost);
            cost
        });
        Table {
            wires,
            bits,
            index,
            rank,
            cost,
        }
    }

    fn search(
        moves: &[(usize, usize)],
        index: &[u8; 16],
        rank: &[u16],
        list: &[usize],
        bits: usize,
    ) -> Vec<u8> {
        let mut cost = vec![NONE; list.len() << bits];
        cost[0] = 0;
        let mut frontier = vec![0u32];
        let mut depth = 0u8;
        while !frontier.is_empty() {
            let mut fresh = Vec::new();
            for &state in &frontier {
                let state = state as usize;
                let key = list[state >> bits];
                let mask = state & ((1 << bits) - 1);
                for &(c, t) in moves {
                    let row = (key >> (4 * c) ^ key >> (4 * t)) & 15;
                    let next = key ^ ((key >> (4 * c) & 15) << (4 * t));
                    let seen = match index[row] {
                        NONE => mask,
                        bit => mask | 1 << bit,
                    };
                    let to = (rank[next] as usize) << bits | seen;
                    if cost[to] == NONE {
                        cost[to] = depth + 1;
                        fresh.push(to as u32);
                    }
                }
            }
            depth += 1;
            frontier = fresh;
        }
        for chunk in cost.chunks_mut(1 << bits) {
            for bit in 0..bits {
                for mask in 0..1usize << bits {
                    if mask >> bit & 1 == 0 {
                        chunk[mask] = chunk[mask].min(chunk[mask | 1 << bit]);
                    }
                }
            }
        }
        cost
    }

    fn path(wires: usize) -> Option<std::path::PathBuf> {
        (wires == WIDTH).then(|| env::temp_dir().join(format!("qirc_gaugenet_{wires}.bin")))
    }

    fn load(wires: usize, size: usize) -> Option<Vec<u8>> {
        let bytes = std::fs::read(Table::path(wires)?).ok()?;
        (bytes.len() == size).then_some(bytes)
    }

    fn store(wires: usize, cost: &[u8]) {
        let Some(path) = Table::path(wires) else {
            return;
        };
        let scratch = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&scratch, cost).is_ok() {
            let _ = std::fs::rename(&scratch, &path);
        }
    }

    fn get(wires: usize) -> &'static Table {
        static TABLES: [OnceLock<Table>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
        TABLES[wires - 2].get_or_init(|| Table::build(wires))
    }

    fn cost(&self, rows: &[u8], terms: impl Iterator<Item = u8>) -> u8 {
        let mask = terms.fold(0usize, |mask, parity| match self.index[parity as usize] {
            NONE => mask,
            bit => mask | 1 << bit,
        });
        self.cost[(self.rank[pack(rows)] as usize) << self.bits | mask]
    }
}

#[derive(Clone)]
struct Form {
    rows: Vec<u8>,
    flips: u128,
    terms: Vec<(u8, f64)>,
}

impl Form {
    fn of(spec: &Spec, extra: &[(u8, f64)]) -> Form {
        let mut terms: HashMap<u8, f64> = spec
            .terms
            .iter()
            .map(|&(parity, angle)| (parity as u8, angle))
            .collect();
        for &(parity, angle) in extra {
            *terms.entry(parity).or_insert(0.0) += angle;
        }
        let wide: Vec<u8> = terms
            .keys()
            .copied()
            .filter(|parity| parity.count_ones() > 1)
            .collect();
        for parity in wide {
            let angle = terms[&parity];
            let turns = (angle / PI).round();
            terms.insert(parity, angle - turns * PI);
            if (turns as i64).rem_euclid(2) == 1 {
                for q in (0..8).filter(|q| parity >> q & 1 == 1) {
                    *terms.entry(1 << q).or_insert(0.0) += PI;
                }
            }
        }
        let mut terms: Vec<(u8, f64)> = terms
            .into_iter()
            .map(|(parity, angle)| (parity, phase::wrap(angle)))
            .filter(|(_, angle)| angle.abs() > EPSILON)
            .collect();
        terms.sort_by_key(|(parity, _)| *parity);
        Form {
            rows: spec.rows.iter().map(|&row| row as u8).collect(),
            flips: spec.flips,
            terms,
        }
    }

    fn price(&self, table: &Table) -> (usize, usize) {
        let cx = table.cost(&self.rows, self.terms.iter().map(|(parity, _)| *parity)) as usize;
        (cx, cx + self.terms.len() + self.flips.count_ones() as usize)
    }

    fn spec(&self) -> Spec {
        Spec {
            terms: self
                .terms
                .iter()
                .map(|&(parity, angle)| (parity as u128, angle))
                .collect(),
            rows: self.rows.iter().map(|&row| row as u128).collect(),
            flips: self.flips,
        }
    }

    fn emit(&self, table: &Table) -> Option<Vec<Step>> {
        let moves = pairs(table.wires);
        let mut rows = self.rows.clone();
        let mut left = self.terms.clone();
        let mut steps = Vec::new();
        let flush = |left: &mut Vec<(u8, f64)>, steps: &mut Vec<Step>| {
            left.retain(|&(parity, angle)| {
                if parity.count_ones() == 1 {
                    steps.push(Step::Rz(parity.trailing_zeros() as usize, angle));
                    false
                } else {
                    true
                }
            });
        };
        flush(&mut left, &mut steps);
        loop {
            let now = table.cost(&rows, left.iter().map(|(parity, _)| *parity));
            if now == 0 {
                break;
            }
            let (c, t) = moves.iter().copied().find(|&(c, t)| {
                let turn = |v: u8| v ^ ((v >> t & 1) << c);
                let next: Vec<u8> = rows.iter().map(|&row| turn(row)).collect();
                table.cost(&next, left.iter().map(|(parity, _)| turn(*parity))) == now - 1
            })?;
            let turn = |v: u8| v ^ ((v >> t & 1) << c);
            for row in &mut rows {
                *row = turn(*row);
            }
            for (parity, _) in &mut left {
                *parity = turn(*parity);
            }
            steps.push(Step::Cx(c, t));
            flush(&mut left, &mut steps);
        }
        if !left.is_empty() {
            return None;
        }
        steps.extend(
            (0..table.wires)
                .filter(|&q| self.flips >> q & 1 == 1)
                .map(Step::X),
        );
        Some(steps)
    }
}

fn pattern(basis: &[u8], unit: f64) -> Vec<(u8, f64)> {
    (1usize..1 << basis.len())
        .map(|combo| {
            let parity = basis
                .iter()
                .enumerate()
                .filter(|(i, _)| combo >> i & 1 == 1)
                .fold(0u8, |acc, (_, vector)| acc ^ vector);
            let sign = if combo.count_ones() % 2 == 1 {
                1.0
            } else {
                -1.0
            };
            (parity, sign * unit)
        })
        .collect()
}

fn gauges(wires: usize) -> Vec<Vec<(u8, f64)>> {
    let mut out = vec![Vec::new()];
    if wires < 3 {
        return out;
    }
    let units: Vec<u8> = (0..wires).map(|q| 1 << q).collect();
    if wires == 3 {
        out.push(pattern(&units, PI / 2.0));
        return out;
    }
    for normal in 1u8..16 {
        let inside: Vec<u8> = (1u8..16)
            .filter(|point| (point & normal).count_ones() % 2 == 0)
            .collect();
        let third = inside
            .iter()
            .copied()
            .find(|&point| {
                point != inside[0] && point != inside[1] && point != inside[0] ^ inside[1]
            })
            .unwrap_or(inside[2]);
        out.push(pattern(&[inside[0], inside[1], third], PI / 2.0));
    }
    let planes = out.clone();
    for scale in [1.0, 2.0, 3.0] {
        let whole = pattern(&units, scale * PI / 4.0);
        for plane in &planes {
            let mut both = plane.clone();
            both.extend(whole.iter().copied());
            out.push(both);
        }
    }
    out
}

fn carve(
    steps: &[Option<Step>],
    gates: &[Gate],
    taken: &[bool],
    start: usize,
    qubits: usize,
    rng: Option<&mut Rng>,
) -> (Vec<usize>, Vec<usize>) {
    let mut state = vec![OPEN; qubits];
    let mut wires: Vec<usize> = Vec::new();
    let mut inside = Vec::new();
    let mut rng = rng;
    for index in start..steps.len().min(start + REACH) {
        if taken[index] {
            continue;
        }
        let uses: Vec<(usize, u8)> = match steps[index] {
            Some(Step::Cx(c, t)) => vec![(c, CONTROL), (t, TARGET)],
            Some(Step::Rz(q, _)) => vec![(q, CONTROL)],
            Some(Step::X(q)) => vec![(q, TARGET)],
            None => gates[index].wires().map(|q| (q.index(), 0)).collect(),
        };
        let fresh = uses.iter().filter(|(w, _)| !wires.contains(w)).count();
        let mut fits = steps[index].is_some()
            && uses.iter().all(|&(w, u)| state[w] & u != 0)
            && wires.len() + fresh <= WIDTH;
        if fits
            && fresh > 0
            && !wires.is_empty()
            && let Some(rng) = rng.as_deref_mut()
        {
            fits = rng.below(4) != 0;
        }
        if fits {
            for &(w, _) in &uses {
                if !wires.contains(&w) {
                    wires.push(w);
                }
            }
            inside.push(index);
        } else {
            for &(w, u) in &uses {
                state[w] &= u;
            }
            if wires.len() == WIDTH && wires.iter().all(|&w| state[w] == 0) {
                break;
            }
        }
    }
    (inside, wires)
}

fn better(found: (usize, usize), held: (usize, usize)) -> bool {
    found.0 <= held.0 && found.1 <= held.1 && found != held
}

fn solve(inner: &[Step], wires: usize) -> Option<Vec<Step>> {
    let held = super::price(inner);
    let spec = Spec::of(inner, wires);
    let table = Table::get(wires);
    let mut best = Form::of(&spec, &[]);
    let mut price = best.price(table);
    for extra in gauges(wires).iter().skip(1) {
        let form = Form::of(&spec, extra);
        let found = form.price(table);
        if found < price {
            price = found;
            best = form;
        }
    }
    if !better(price, held) {
        return None;
    }
    let steps = best.emit(table)?;
    if !Spec::of(&steps, wires).same(&best.spec()) || super::price(&steps) != price {
        return None;
    }
    Some(steps)
}

fn sweep(
    gates: Vec<Gate>,
    qubits: usize,
    backward: bool,
    rng: &mut Rng,
    random: bool,
) -> Vec<Gate> {
    let mut gates = gates;
    if backward {
        gates.reverse();
    }
    let steps: Vec<Option<Step>> = gates.iter().map(classify).collect();
    let mut taken = vec![false; gates.len()];
    let mut out: Vec<Gate> = Vec::with_capacity(gates.len());
    for start in 0..gates.len() {
        if taken[start] {
            continue;
        }
        if steps[start].is_none() {
            out.push(gates[start].clone());
            continue;
        }
        let (inside, wires) = carve(
            &steps,
            &gates,
            &taken,
            start,
            qubits,
            random.then_some(&mut *rng),
        );
        let found = (wires.len() >= 2 && inside.len() >= 3)
            .then(|| {
                let place = |w: usize| wires.iter().position(|&held| held == w).unwrap_or(0);
                let mut inner: Vec<Step> = inside
                    .iter()
                    .filter_map(|&index| steps[index])
                    .map(|step| step.map(place))
                    .collect();
                if backward {
                    inner.reverse();
                }
                solve(&inner, wires.len())
            })
            .flatten();
        match found {
            Some(mut fresh) => {
                if backward {
                    fresh.reverse();
                }
                let span: Span = gates[start].span;
                out.extend(
                    fresh
                        .into_iter()
                        .map(|step| step.map(|w| wires[w]).gate(span)),
                );
                for index in inside {
                    taken[index] = true;
                }
            }
            None => out.push(gates[start].clone()),
        }
    }
    if backward {
        out.reverse();
    }
    out
}

fn settle(gates: Vec<Gate>, qubits: usize, rng: &mut Rng) -> Vec<Gate> {
    let mut best = gates;
    let mut idle = 0;
    for round in 0..SWEEPS {
        let found = sweep(best.clone(), qubits, round % 2 == 1, rng, round >= 2);
        if super::tally(&found) < super::tally(&best) {
            best = found;
            idle = 0;
        } else {
            idle += 1;
            if idle >= 4 {
                break;
            }
        }
    }
    best
}

pub(crate) fn optimal(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut saved = 0;
    for block in &mut program.blocks {
        let mut out: Vec<Op> = Vec::with_capacity(block.ops.len());
        let mut run: Vec<Gate> = Vec::new();
        let mut flush = |run: &mut Vec<Gate>, out: &mut Vec<Op>| {
            let gates = std::mem::take(run);
            let (cx, total) = super::tally(&gates);
            let found = settle(gates, qubits, &mut rng);
            let (less, fewer) = super::tally(&found);
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

    #[test]
    fn small_tables() {
        let two = Table::get(2);
        assert_eq!(two.cost(&[1, 2], [3u8].into_iter()), 2);
        assert_eq!(two.cost(&[3, 2], [3u8].into_iter()), 1);
        let three = Table::get(3);
        assert_eq!(three.cost(&[1, 2, 4], [7u8].into_iter()), 4);
        assert_eq!(three.cost(&[1, 2, 4], [3u8, 5, 6, 7].into_iter()), 6);
    }

    #[test]
    fn gauge_is_sound() {
        let angles = ["t", "tdg", "s", "sdg", "z", "rz(0.3)"];
        let mut rng = Rng(91);
        for round in 0..150 {
            let qubits = 3;
            let mut source = format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n");
            for q in 0..qubits {
                source += &format!("ry(0.{}) q[{q}];\nrz(1.{}) q[{q}];\n", 3 + q, 1 + q);
            }
            for _ in 0..6 + round % 30 {
                let a = rng.below(qubits);
                match rng.below(3) {
                    0 => source += &format!("{} q[{a}];\n", angles[rng.below(angles.len())]),
                    1 if round % 5 == 0 && rng.below(4) == 0 => source += &format!("x q[{a}];\n"),
                    _ => {
                        let b = (a + 1 + rng.below(qubits - 1)) % qubits;
                        source += &format!("cx q[{a}], q[{b}];\n");
                    }
                }
            }
            let (original, errors) = qasm::lower(&source);
            assert!(errors.is_empty(), "{errors:?}");
            let mut rewritten = original.clone();
            let gates: Vec<Gate> = rewritten.blocks[0]
                .ops
                .drain(..)
                .filter_map(|op| match op {
                    Op::Gate(gate) => Some(gate),
                    _ => None,
                })
                .collect();
            let found = settle(gates, qubits, &mut rng);
            rewritten.blocks[0].ops = found.into_iter().map(Op::Gate).collect();
            let differences = equiv::compare(
                &equiv::explore(&original),
                &equiv::explore(&rewritten),
                true,
            );
            assert!(differences.is_empty(), "{differences:?}\n{source}");
        }
    }
}
