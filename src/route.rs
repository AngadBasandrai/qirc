use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::mem;

use crate::diag::Span;
use crate::ir::*;

#[derive(Clone)]
pub struct Coupling {
    pub qubits: usize,
    pub edges: Vec<(usize, usize)>,
}

impl Coupling {
    pub fn line(qubits: usize) -> Coupling {
        Coupling {
            qubits,
            edges: (0..qubits.saturating_sub(1)).map(|i| (i, i + 1)).collect(),
        }
    }

    pub fn ring(qubits: usize) -> Coupling {
        let mut coupling = Coupling::line(qubits);
        if qubits > 2 {
            coupling.edges.push((qubits - 1, 0));
        }
        coupling
    }

    pub fn grid(rows: usize, columns: usize) -> Coupling {
        let mut edges = Vec::new();
        for row in 0..rows {
            for column in 0..columns {
                let here = row * columns + column;
                if column + 1 < columns {
                    edges.push((here, here + 1));
                }
                if row + 1 < rows {
                    edges.push((here, here + columns));
                }
            }
        }
        Coupling {
            qubits: rows * columns,
            edges,
        }
    }

    pub fn full(qubits: usize) -> Coupling {
        let mut edges = Vec::new();
        for a in 0..qubits {
            for b in (a + 1)..qubits {
                edges.push((a, b));
            }
        }
        Coupling { qubits, edges }
    }

    pub fn parse(spec: &str) -> Option<Coupling> {
        if let Some(rest) = spec.strip_prefix("line:") {
            return Some(Coupling::line(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("ring:") {
            return Some(Coupling::ring(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("full:") {
            return Some(Coupling::full(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("grid:") {
            let (rows, columns) = rest.split_once('x')?;
            return Some(Coupling::grid(rows.parse().ok()?, columns.parse().ok()?));
        }

        let mut edges = Vec::new();
        let mut highest = 0usize;
        for pair in spec.split(',') {
            let (a, b) = pair.trim().split_once('-')?;
            let a: usize = a.trim().parse().ok()?;
            let b: usize = b.trim().parse().ok()?;
            highest = highest.max(a).max(b);
            edges.push((a, b));
        }

        if edges.is_empty() {
            return None;
        }

        Some(Coupling {
            qubits: highest + 1,
            edges,
        })
    }

    pub fn connected(&self, a: usize, b: usize) -> bool {
        self.edges
            .iter()
            .any(|&(x, y)| (x == a && y == b) || (x == b && y == a))
    }

    pub fn neighbours(&self, of: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for &(a, b) in &self.edges {
            if a == of {
                out.push(b);
            } else if b == of {
                out.push(a);
            }
        }
        out
    }

    pub fn distances(&self) -> Vec<Vec<usize>> {
        (0..self.qubits)
            .map(|from| {
                let mut distance = vec![usize::MAX; self.qubits];
                distance[from] = 0;
                let mut queue = VecDeque::from([from]);
                while let Some(current) = queue.pop_front() {
                    for next in self.neighbours(current) {
                        if next < self.qubits && distance[next] == usize::MAX {
                            distance[next] = distance[current] + 1;
                            queue.push_back(next);
                        }
                    }
                }
                distance
            })
            .collect()
    }

    fn spanning_forest(&self) -> (Vec<usize>, Vec<Option<usize>>) {
        let mut order = Vec::with_capacity(self.qubits);
        let mut parent = vec![None; self.qubits];
        let mut seen = vec![false; self.qubits];
        for root in 0..self.qubits {
            if seen[root] {
                continue;
            }
            seen[root] = true;
            let mut queue = VecDeque::from([root]);
            while let Some(current) = queue.pop_front() {
                order.push(current);
                for next in self.neighbours(current) {
                    if next < self.qubits && !seen[next] {
                        seen[next] = true;
                        parent[next] = Some(current);
                        queue.push_back(next);
                    }
                }
            }
        }
        (order, parent)
    }

    pub fn shortest_path(&self, from: usize, to: usize) -> Option<Vec<usize>> {
        if from == to {
            return Some(vec![from]);
        }

        let mut came_from = vec![usize::MAX; self.qubits];
        let mut seen = vec![false; self.qubits];
        let mut queue = VecDeque::new();

        seen[from] = true;
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            for next in self.neighbours(current) {
                if next >= self.qubits || seen[next] {
                    continue;
                }
                seen[next] = true;
                came_from[next] = current;

                if next == to {
                    let mut path = vec![to];
                    let mut walk = to;
                    while walk != from {
                        walk = came_from[walk];
                        path.push(walk);
                    }
                    path.reverse();
                    return Some(path);
                }

                queue.push_back(next);
            }
        }

        None
    }
}

#[derive(Debug)]
pub struct RouteStats {
    pub swaps_inserted: usize,
    pub final_layout: Vec<usize>,
}

impl fmt::Display for RouteStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "routing inserted {} swap(s), final layout {:?}",
            self.swaps_inserted, self.final_layout
        )
    }
}

const LOOKAHEAD: usize = 20;
const LOOKAHEAD_WEIGHT: f64 = 0.5;
const LOOKAHEAD_FADE: f64 = 0.9;
const ABSORB: f64 = 0.25;
const DECAY: f64 = 0.001;
const TRIALS: usize = 8;
const TRIAL_OPS: usize = 20_000;

struct Layout {
    physical: Vec<usize>,
    logical: Vec<usize>,
}

impl Layout {
    fn new(physical: Vec<usize>) -> Layout {
        let mut logical = vec![0; physical.len()];
        for (q, &p) in physical.iter().enumerate() {
            logical[p] = q;
        }
        Layout { physical, logical }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.logical.swap(a, b);
        self.physical[self.logical[a]] = a;
        self.physical[self.logical[b]] = b;
    }

    fn at(&self, q: QubitId) -> usize {
        self.physical[q.index()]
    }
}

struct Router<'a> {
    coupling: &'a Coupling,
    distance: Vec<Vec<usize>>,
    swaps: usize,
}

impl Router<'_> {
    fn swap(&mut self, layout: &mut Layout, a: usize, b: usize, span: Span, out: &mut Vec<Op>) {
        layout.swap(a, b);
        self.swaps += 1;
        out.push(Op::Gate(Gate {
            kind: GateKind::Swap,
            controls: Vec::new(),
            targets: vec![QubitId(a as u32), QubitId(b as u32)],
            params: Vec::new(),
            span,
        }));
    }

    fn segment(
        &mut self,
        ops: Vec<Op>,
        layout: &mut Layout,
        out: &mut Vec<Op>,
    ) -> Result<(), String> {
        let mut waiting = vec![0usize; ops.len()];
        let mut next: Vec<Vec<usize>> = vec![Vec::new(); ops.len()];
        let mut last: HashMap<QubitId, usize> = HashMap::new();
        for (index, op) in ops.iter().enumerate() {
            let mut before: Vec<usize> = op
                .qubits()
                .iter()
                .filter_map(|q| last.insert(*q, index))
                .collect();
            before.sort_unstable();
            before.dedup();
            for earlier in before {
                next[earlier].push(index);
                waiting[index] += 1;
            }
        }

        let mut ops: Vec<Option<Op>> = ops.into_iter().map(Some).collect();
        let mut front: Vec<usize> = (0..ops.len()).filter(|&i| waiting[i] == 0).collect();
        let mut decay = vec![1.0f64; self.coupling.qubits];
        let mut partner: Vec<Option<usize>> = vec![None; self.coupling.qubits];
        let mut stalled = 0;

        while !front.is_empty() {
            let mut progressed = false;
            let mut index = 0;
            while index < front.len() {
                let id = front[index];
                let pair = ops[id].as_ref().and_then(|op| pair(op, layout));
                if pair.is_some_and(|(a, b)| self.distance[a][b] > 1) {
                    index += 1;
                    continue;
                }
                let Some(op) = ops[id].take() else {
                    index += 1;
                    continue;
                };
                match (pair, &op) {
                    (Some((a, b)), _) => {
                        partner[a] = Some(b);
                        partner[b] = Some(a);
                    }
                    (None, Op::Measure { qubit, .. } | Op::Reset { qubit, .. }) => {
                        partner[layout.at(*qubit)] = None;
                    }
                    _ => {}
                }
                out.push(place(op, layout));
                front.swap_remove(index);
                for &after in &next[id] {
                    waiting[after] -= 1;
                    if waiting[after] == 0 {
                        front.push(after);
                    }
                }
                progressed = true;
            }
            if progressed {
                stalled = 0;
                decay.iter_mut().for_each(|d| *d = 1.0);
                continue;
            }

            let blocked: Vec<(usize, usize)> = front
                .iter()
                .filter_map(|&i| ops[i].as_ref().and_then(|op| pair(op, layout)))
                .collect();
            let span = front
                .iter()
                .find_map(|&i| ops[i].as_ref().and_then(Op::as_gate).map(|g| g.span))
                .unwrap_or(Span::DUMMY);
            if blocked
                .iter()
                .any(|&(a, b)| self.distance[a][b] == usize::MAX)
            {
                return Err("the coupling map has no path between two interacting qubits".into());
            }

            stalled += 1;
            if stalled > 2 * self.coupling.qubits + 8 {
                let (a, b) = blocked[0];
                let path = self.coupling.shortest_path(a, b).unwrap_or_default();
                for step in 0..path.len().saturating_sub(2) {
                    self.swap(layout, path[step], path[step + 1], span, out);
                    partner[path[step]] = None;
                    partner[path[step + 1]] = None;
                }
                stalled = 0;
                continue;
            }

            let upcoming = self.upcoming(&front, &next, &ops, layout);
            let mut best: Option<((usize, usize), f64)> = None;
            for &(a, b) in &blocked {
                for from in [a, b] {
                    for to in self.coupling.neighbours(from) {
                        let absorbed = if partner[from] == Some(to) {
                            ABSORB
                        } else {
                            0.0
                        };
                        let score = (self.score((from, to), &blocked, &upcoming) - absorbed)
                            * decay[from].max(decay[to]);
                        if best.is_none_or(|(_, s)| score < s) {
                            best = Some(((from, to), score));
                        }
                    }
                }
            }
            let Some(((a, b), _)) = best else {
                return Err("the coupling map has no edge to move a qubit along".into());
            };
            self.swap(layout, a, b, span, out);
            partner[a] = None;
            partner[b] = None;
            decay[a] += DECAY;
            decay[b] += DECAY;
        }
        Ok(())
    }

    fn upcoming(
        &self,
        front: &[usize],
        next: &[Vec<usize>],
        ops: &[Option<Op>],
        layout: &Layout,
    ) -> Vec<(usize, usize)> {
        let mut seen: Vec<usize> = front.to_vec();
        let mut queue: VecDeque<usize> = front
            .iter()
            .flat_map(|&i| next[i].iter().copied())
            .collect();
        let mut found = Vec::new();
        while let Some(id) = queue.pop_front() {
            if found.len() == LOOKAHEAD {
                break;
            }
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(p) = ops[id].as_ref().and_then(|op| pair(op, layout)) {
                found.push(p);
            }
            queue.extend(next[id].iter().copied());
        }
        found
    }

    fn score(
        &self,
        (a, b): (usize, usize),
        blocked: &[(usize, usize)],
        upcoming: &[(usize, usize)],
    ) -> f64 {
        let moved = |p: usize| {
            if p == a {
                b
            } else if p == b {
                a
            } else {
                p
            }
        };
        let total = |pairs: &[(usize, usize)], fade: f64| {
            let (mut sum, mut norm, mut weight) = (0.0, 0.0, 1.0);
            for &(x, y) in pairs {
                sum += weight * self.distance[moved(x)][moved(y)] as f64;
                norm += weight;
                weight *= fade;
            }
            if norm > 0.0 { sum / norm } else { 0.0 }
        };
        total(blocked, 1.0) + LOOKAHEAD_WEIGHT * total(upcoming, LOOKAHEAD_FADE)
    }

    fn restore(
        &mut self,
        layout: &mut Layout,
        home: &Layout,
        span: Span,
        out: &mut Vec<Op>,
    ) -> Result<(), String> {
        let (order, parent) = self.coupling.spanning_forest();
        for &target in order.iter().rev() {
            let token = home.logical[target];
            let start = layout.physical[token];
            let Some(path) = tree_path(&parent, start, target) else {
                return Err(
                    "the coupling map is not connected enough to restore the layout".into(),
                );
            };
            for step in path.windows(2) {
                self.swap(layout, step[0], step[1], span, out);
            }
        }
        Ok(())
    }
}

fn pair(op: &Op, layout: &Layout) -> Option<(usize, usize)> {
    let gate = op.as_gate()?;
    let wires: Vec<QubitId> = gate.wires().collect();
    (wires.len() == 2).then(|| (layout.at(wires[0]), layout.at(wires[1])))
}

fn place(op: Op, layout: &Layout) -> Op {
    match op {
        Op::Gate(mut gate) => {
            for q in gate.controls.iter_mut().chain(gate.targets.iter_mut()) {
                *q = QubitId(layout.at(*q) as u32);
            }
            Op::Gate(gate)
        }
        Op::Measure {
            qubit,
            result,
            span,
        } => Op::Measure {
            qubit: QubitId(layout.at(qubit) as u32),
            result,
            span,
        },
        Op::Reset { qubit, span } => Op::Reset {
            qubit: QubitId(layout.at(qubit) as u32),
            span,
        },
        other => other,
    }
}

fn tree_path(parent: &[Option<usize>], from: usize, to: usize) -> Option<Vec<usize>> {
    let ancestors = |mut v: usize| {
        let mut chain = vec![v];
        while let Some(p) = parent[v] {
            chain.push(p);
            v = p;
        }
        chain
    };
    let up = ancestors(from);
    let down = ancestors(to);
    let meet = up.iter().position(|v| down.contains(v))?;
    let turn = down.iter().position(|v| *v == up[meet])?;
    let mut path = up[..=meet].to_vec();
    path.extend(down[..turn].iter().rev());
    Some(path)
}

fn layout_pass(router: &mut Router, gates: &[Op], start: Vec<usize>) -> Result<Vec<usize>, String> {
    let mut layout = Layout::new(start);
    router.segment(gates.to_vec(), &mut layout, &mut Vec::new())?;
    Ok(layout.physical)
}

struct Routed {
    blocks: Vec<Vec<Op>>,
    swaps: usize,
    final_layout: Vec<usize>,
}

fn attempt(router: &mut Router, program: &Program, start: Vec<usize>) -> Result<Routed, String> {
    router.swaps = 0;
    let home = Layout::new(start);
    let mut final_layout = home.physical.clone();
    let straight = program.blocks.len() == 1;
    let mut blocks = Vec::with_capacity(program.blocks.len());

    for block in &program.blocks {
        let mut layout = Layout::new(home.physical.clone());
        let mut out = Vec::with_capacity(block.ops.len());
        let mut segment = Vec::new();
        for op in &block.ops {
            if matches!(op, Op::Gate(_) | Op::Measure { .. } | Op::Reset { .. }) {
                segment.push(op.clone());
                continue;
            }
            router.segment(mem::take(&mut segment), &mut layout, &mut out)?;
            out.push(op.clone());
        }
        router.segment(segment, &mut layout, &mut out)?;
        if !straight && !matches!(block.term, Term::Ret(_) | Term::Unreachable) {
            router.restore(&mut layout, &home, block.span, &mut out)?;
        }
        if straight {
            final_layout = layout.physical.clone();
        }
        blocks.push(out);
    }

    Ok(Routed {
        blocks,
        swaps: router.swaps,
        final_layout,
    })
}

fn starts(qubits: usize, trials: usize) -> Vec<Vec<usize>> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |bound: usize| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize % bound
    };
    (0..trials)
        .map(|trial| {
            let mut order: Vec<usize> = (0..qubits).collect();
            match trial {
                0 => {}
                1 => order.reverse(),
                _ => {
                    for i in (1..qubits).rev() {
                        order.swap(i, next(i + 1));
                    }
                }
            }
            order
        })
        .collect()
}

pub fn route(program: &mut Program, coupling: &Coupling) -> Result<RouteStats, String> {
    if (program.num_qubits as usize) > coupling.qubits {
        return Err(format!(
            "the program needs {} qubits but the coupling map has {}",
            program.num_qubits, coupling.qubits
        ));
    }

    for gate in program.gates() {
        let wires = gate.wires().count();
        if wires > 2 {
            return Err(format!(
                "`{}{}` acts on {wires} qubits, the router only handles one and two qubit gates",
                "c".repeat(gate.controls.len()),
                gate.kind.name()
            ));
        }
    }

    let mut router = Router {
        coupling,
        distance: coupling.distances(),
        swaps: 0,
    };

    let quantum: Vec<Op> = program
        .ops()
        .filter(|op| op.as_gate().is_some_and(|g| g.wires().count() == 2))
        .cloned()
        .collect();
    let reversed: Vec<Op> = quantum.iter().rev().cloned().collect();
    let trials = if program.op_count() <= TRIAL_OPS {
        TRIALS
    } else {
        1
    };

    let mut best: Option<Routed> = None;
    for start in starts(coupling.qubits, trials) {
        let forward = layout_pass(&mut router, &quantum, start)?;
        let backward = layout_pass(&mut router, &reversed, forward)?;
        let routed = attempt(&mut router, program, backward)?;
        if best.as_ref().is_none_or(|b| routed.swaps < b.swaps) {
            best = Some(routed);
        }
    }
    let Some(best) = best else {
        return Err("the router found no layout".into());
    };

    for (block, ops) in program.blocks.iter_mut().zip(best.blocks) {
        block.ops = ops;
    }
    program.num_qubits = coupling.qubits as u32;

    Ok(RouteStats {
        swaps_inserted: best.swaps,
        final_layout: best.final_layout,
    })
}

pub fn respects(program: &Program, coupling: &Coupling) -> bool {
    program.gates().all(|gate| {
        let wires: Vec<QubitId> = gate.wires().collect();
        match wires.len() {
            0 | 1 => true,
            2 => coupling.connected(wires[0].index(), wires[1].index()),
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    fn line_program(pairs: &[(u32, u32)], qubits: u32) -> Program {
        let mut program = Program::new("r", Profile::Unrestricted);
        program.num_qubits = qubits;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: pairs
                .iter()
                .map(|(c, t)| {
                    Op::Gate(Gate {
                        kind: GateKind::X,
                        controls: vec![QubitId(*c)],
                        targets: vec![QubitId(*t)],
                        params: Vec::new(),
                        span: Span::DUMMY,
                    })
                })
                .collect(),
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        program
    }

    #[test]
    fn parse_specs() {
        assert_eq!(
            Coupling::parse("line:3").unwrap().edges,
            vec![(0, 1), (1, 2)]
        );
        assert_eq!(Coupling::parse("ring:3").unwrap().edges.len(), 3);
        assert_eq!(Coupling::parse("grid:2x2").unwrap().edges.len(), 4);
        assert_eq!(Coupling::parse("full:4").unwrap().edges.len(), 6);

        let explicit = Coupling::parse("0-1, 2-3").unwrap();
        assert_eq!(explicit.qubits, 4);
        assert!(explicit.connected(2, 3));
        assert!(!explicit.connected(1, 2));

        assert!(Coupling::parse("nonsense").is_none());
    }

    #[test]
    fn shortest_line_path() {
        let line = Coupling::line(5);
        assert_eq!(line.shortest_path(0, 4).unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(line.shortest_path(2, 2).unwrap(), vec![2]);
        assert_eq!(
            Coupling::parse("0-1").unwrap().shortest_path(0, 1),
            Some(vec![0, 1])
        );
    }

    #[test]
    fn adjacent() {
        let mut program = line_program(&[(0, 1), (1, 2)], 3);
        let stats = route(&mut program, &Coupling::line(3)).unwrap();
        assert_eq!(stats.swaps_inserted, 0);
        assert!(respects(&program, &Coupling::line(3)));
    }

    #[test]
    fn star() {
        let coupling = Coupling::line(5);
        let mut program = line_program(&[(0, 1), (0, 2), (0, 3), (0, 4)], 5);

        let stats = route(&mut program, &coupling).unwrap();
        assert!((1..=4).contains(&stats.swaps_inserted), "{stats}");
        assert!(respects(&program, &coupling));
        assert_eq!(program.gate_count(), 4 + stats.swaps_inserted);
    }

    #[test]
    fn branching() {
        let coupling = Coupling::line(3);
        let mut program = line_program(&[(0, 2)], 3);
        program.blocks.push(Block {
            id: BlockId(1),
            label: "second".into(),
            ops: line_program(&[(2, 0), (1, 2)], 3).blocks.remove(0).ops,
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        program.blocks[0].term = Term::Br(BlockId(1));

        route(&mut program, &coupling).unwrap();
        assert!(respects(&program, &coupling));
    }

    #[test]
    fn three_qubit_gate() {
        let mut program = Program::new("t", Profile::Unrestricted);
        program.num_qubits = 3;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: vec![Op::Gate(Gate {
                kind: GateKind::X,
                controls: vec![QubitId(0), QubitId(1)],
                targets: vec![QubitId(2)],
                params: Vec::new(),
                span: Span::DUMMY,
            })],
            term: Term::Ret(None),
            span: Span::DUMMY,
        });

        let message = route(&mut program, &Coupling::line(3)).unwrap_err();
        assert!(message.contains("acts on 3 qubits"), "{message}");
    }

    #[test]
    fn small_device() {
        let mut program = line_program(&[(0, 1)], 4);
        assert!(
            route(&mut program, &Coupling::line(2))
                .unwrap_err()
                .contains("needs 4 qubits")
        );
    }
}
