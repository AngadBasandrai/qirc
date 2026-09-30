use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

use crate::ir::*;
use crate::phase;

const MAX_OPS: usize = 20_000;
const MAX_EDGES: usize = 1 << 21;
const EPSILON: f64 = 1e-12;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    Plain,
    Hadamard,
}

impl Edge {
    fn then(self, other: Edge) -> Edge {
        if self == other {
            Edge::Plain
        } else {
            Edge::Hadamard
        }
    }

    fn flipped(self) -> Edge {
        self.then(Edge::Hadamard)
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Phase {
    quarters: u8,
    term: Option<(bool, usize)>,
}

impl Phase {
    fn constant(quarters: u8) -> Phase {
        Phase {
            quarters: quarters % 4,
            term: None,
        }
    }

    fn pauli(self) -> bool {
        self.term.is_none() && self.quarters.is_multiple_of(2)
    }

    fn clifford(self) -> bool {
        self.term.is_none() && !self.quarters.is_multiple_of(2)
    }

    fn zero(self) -> bool {
        self.term.is_none() && self.quarters == 0
    }

    fn turned(self, quarters: u8) -> Phase {
        Phase {
            quarters: (self.quarters + quarters) % 4,
            ..self
        }
    }

    fn negated(self) -> Phase {
        Phase {
            quarters: (4 - self.quarters) % 4,
            term: self.term.map(|(negative, variable)| (!negative, variable)),
        }
    }
}

#[derive(Default)]
struct Variables {
    parent: Vec<usize>,
    flip: Vec<bool>,
}

impl Variables {
    fn fresh(&mut self) -> usize {
        self.parent.push(self.parent.len());
        self.flip.push(false);
        self.parent.len() - 1
    }

    fn find(&mut self, variable: usize) -> (usize, bool) {
        let mut root = variable;
        let mut parity = false;
        while self.parent[root] != root {
            parity ^= self.flip[root];
            root = self.parent[root];
        }
        let mut node = variable;
        let mut rest = parity;
        while node != root {
            let next = self.parent[node];
            let step = self.flip[node];
            self.parent[node] = root;
            self.flip[node] = rest;
            rest ^= step;
            node = next;
        }
        (root, parity)
    }

    fn join(&mut self, a: (bool, usize), b: (bool, usize)) -> Option<(bool, usize)> {
        let (root_a, parity_a) = self.find(a.1);
        let (root_b, parity_b) = self.find(b.1);
        if root_a == root_b {
            return None;
        }
        let sign_a = a.0 ^ parity_a;
        let sign_b = b.0 ^ parity_b;
        self.parent[root_b] = root_a;
        self.flip[root_b] = sign_a ^ sign_b;
        Some((sign_a, root_a))
    }
}

struct Spider {
    boundary: bool,
    phase: Phase,
    edges: BTreeMap<usize, Edge>,
}

#[derive(Default)]
struct Graph {
    spiders: Vec<Option<Spider>>,
    variables: Variables,
    edges: usize,
    broken: bool,
}

impl Graph {
    fn add(&mut self, boundary: bool, phase: Phase) -> usize {
        self.spiders.push(Some(Spider {
            boundary,
            phase,
            edges: BTreeMap::new(),
        }));
        self.spiders.len() - 1
    }

    fn live(&self, v: usize) -> bool {
        self.spiders[v].is_some()
    }

    fn spider(&self, v: usize) -> &Spider {
        self.spiders[v].as_ref().unwrap()
    }

    fn spider_mut(&mut self, v: usize) -> &mut Spider {
        self.spiders[v].as_mut().unwrap()
    }

    fn neighbors(&self, v: usize) -> Vec<usize> {
        self.spider(v).edges.keys().copied().collect()
    }

    fn degree(&self, v: usize) -> usize {
        self.spider(v).edges.len()
    }

    fn interior(&self, v: usize) -> bool {
        let spider = self.spider(v);
        !spider.boundary && spider.edges.keys().all(|&n| !self.spider(n).boundary)
    }

    fn axle(&self, v: usize) -> bool {
        self.spider(v).edges.keys().any(|&n| self.degree(n) == 1)
    }

    fn sum(&mut self, a: Phase, b: Phase) -> Phase {
        let term = match (a.term, b.term) {
            (Some(x), Some(y)) => {
                let joined = self.variables.join(x, y);
                self.broken |= joined.is_none();
                joined
            }
            (x, y) => x.or(y),
        };
        Phase {
            quarters: (a.quarters + b.quarters) % 4,
            term,
        }
    }

    fn turn(&mut self, v: usize, quarters: u8) {
        let spider = self.spider_mut(v);
        spider.phase = spider.phase.turned(quarters);
    }

    fn toggle(&mut self, a: usize, b: usize) {
        if self.spider_mut(a).edges.remove(&b).is_some() {
            self.spider_mut(b).edges.remove(&a);
            self.edges -= 1;
        } else {
            self.spider_mut(a).edges.insert(b, Edge::Hadamard);
            self.spider_mut(b).edges.insert(a, Edge::Hadamard);
            self.edges += 1;
            self.broken |= self.edges > MAX_EDGES;
        }
    }

    fn link(&mut self, a: usize, b: usize, edge: Edge) {
        let spiders = !self.spider(a).boundary && !self.spider(b).boundary;
        match (edge, self.spider(a).edges.get(&b).copied()) {
            (Edge::Plain, _) if spiders => self.fuse(a, b),
            (Edge::Hadamard, Some(Edge::Hadamard)) | (Edge::Hadamard, None) => self.toggle(a, b),
            (_, None) => {
                self.spider_mut(a).edges.insert(b, edge);
                self.spider_mut(b).edges.insert(a, edge);
                self.edges += 1;
            }
            _ => self.broken = true,
        }
    }

    fn remove(&mut self, v: usize) {
        let spider = self.spiders[v].take().unwrap();
        for n in spider.edges.keys() {
            self.spider_mut(*n).edges.remove(&v);
        }
        self.edges -= spider.edges.len();
    }

    fn fuse(&mut self, a: usize, b: usize) {
        let taken = self.spiders[b].take().unwrap();
        self.edges -= taken.edges.len();
        for (&n, &edge) in &taken.edges {
            if !self.live(n) {
                self.broken = true;
                continue;
            }
            self.spider_mut(n).edges.remove(&b);
            if n == a {
                match edge {
                    Edge::Hadamard => self.turn(a, 2),
                    Edge::Plain => self.broken = true,
                }
            } else {
                self.link(a, n, edge);
            }
        }
        let phase = self.sum(self.spider(a).phase, taken.phase);
        self.spider_mut(a).phase = phase;
    }

    fn pivot(&mut self, u: usize, v: usize) {
        let around_u: BTreeSet<usize> = self.neighbors(u).into_iter().filter(|&n| n != v).collect();
        let around_v: BTreeSet<usize> = self.neighbors(v).into_iter().filter(|&n| n != u).collect();
        let both: Vec<usize> = around_u.intersection(&around_v).copied().collect();
        let only_u: Vec<usize> = around_u.difference(&around_v).copied().collect();
        let only_v: Vec<usize> = around_v.difference(&around_u).copied().collect();
        let (phase_u, phase_v) = (self.spider(u).phase.quarters, self.spider(v).phase.quarters);
        for &a in &only_u {
            for &b in only_v.iter().chain(&both) {
                self.toggle(a, b);
            }
        }
        for &a in &only_v {
            for &b in &both {
                self.toggle(a, b);
            }
        }
        for &a in &only_u {
            self.turn(a, phase_v);
        }
        for &a in &only_v {
            self.turn(a, phase_u);
        }
        for &a in &both {
            self.turn(a, phase_u + phase_v + 2);
        }
        self.remove(u);
        self.remove(v);
    }

    fn complement(&mut self, v: usize) {
        let around = self.neighbors(v);
        let quarters = self.spider(v).phase.quarters;
        for (i, &a) in around.iter().enumerate() {
            for &b in &around[i + 1..] {
                self.toggle(a, b);
            }
            self.turn(a, 4 - quarters);
        }
        self.remove(v);
    }

    fn identities(&mut self) -> usize {
        let mut count = 0;
        for v in 0..self.spiders.len() {
            if !self.live(v)
                || self.spider(v).boundary
                || !self.spider(v).phase.zero()
                || self.degree(v) != 2
            {
                continue;
            }
            let pair: Vec<(usize, Edge)> =
                self.spider(v).edges.iter().map(|(&n, &e)| (n, e)).collect();
            let [(a, first), (b, second)] = pair[..] else {
                continue;
            };
            self.remove(v);
            self.link(a, b, first.then(second));
            count += 1;
        }
        count
    }

    fn complements(&mut self) -> usize {
        let mut count = 0;
        for v in 0..self.spiders.len() {
            if self.live(v) && self.spider(v).phase.clifford() && self.interior(v) {
                self.complement(v);
                count += 1;
            }
        }
        count
    }

    fn pivots(&mut self) -> usize {
        let mut count = 0;
        for u in 0..self.spiders.len() {
            if !self.live(u) || !self.spider(u).phase.pauli() || !self.interior(u) {
                continue;
            }
            let partner = self
                .neighbors(u)
                .into_iter()
                .find(|&v| self.spider(v).phase.pauli() && self.interior(v));
            if let Some(v) = partner {
                self.pivot(u, v);
                count += 1;
            }
        }
        count
    }

    fn gadget_pivots(&mut self) -> usize {
        let mut count = 0;
        for u in 0..self.spiders.len() {
            if !self.live(u)
                || !self.spider(u).phase.pauli()
                || !self.interior(u)
                || self.degree(u) < 2
                || self.axle(u)
            {
                continue;
            }
            let partner = self.neighbors(u).into_iter().find(|&v| {
                !self.spider(v).phase.pauli()
                    && self.interior(v)
                    && self.degree(v) > 1
                    && !self.axle(v)
            });
            let Some(v) = partner else {
                continue;
            };
            let phase = self.spider(v).phase;
            self.spider_mut(v).phase = Phase::default();
            let axle = self.add(false, Phase::default());
            let leaf = self.add(false, phase);
            self.toggle(v, axle);
            self.toggle(axle, leaf);
            self.pivot(u, v);
            count += 1;
        }
        count
    }

    fn gadget(&self, leaf: usize) -> Option<(usize, Vec<usize>)> {
        let spider = self.spider(leaf);
        if spider.boundary || spider.edges.len() != 1 {
            return None;
        }
        let (&axle, &edge) = spider.edges.iter().next()?;
        let hub = self.spider(axle);
        if edge != Edge::Hadamard || hub.boundary || !hub.phase.pauli() || hub.edges.len() < 2 {
            return None;
        }
        Some((
            axle,
            hub.edges.keys().copied().filter(|&n| n != leaf).collect(),
        ))
    }

    fn gadgets(&mut self) -> usize {
        let mut count = 0;
        let mut seen: BTreeMap<Vec<usize>, usize> = BTreeMap::new();
        for leaf in 0..self.spiders.len() {
            if !self.live(leaf) {
                continue;
            }
            let Some((axle, targets)) = self.gadget(leaf) else {
                continue;
            };
            if self.spider(axle).phase.quarters == 2 {
                self.spider_mut(axle).phase = Phase::default();
                let spider = self.spider_mut(leaf);
                spider.phase = spider.phase.negated();
                count += 1;
            }
            let earlier = seen.get(&targets).copied().filter(|&first| {
                self.live(first)
                    && self
                        .gadget(first)
                        .is_some_and(|(hub, found)| hub != axle && found == targets)
            });
            match earlier {
                Some(first) => {
                    let phase = self.sum(self.spider(first).phase, self.spider(leaf).phase);
                    self.spider_mut(first).phase = phase;
                    self.remove(leaf);
                    self.remove(axle);
                    count += 1;
                }
                None => {
                    seen.insert(targets, leaf);
                }
            }
        }
        count
    }

    fn simplify(&mut self) {
        loop {
            let mut changed = 0;
            loop {
                let found = self.identities() + self.complements() + self.pivots();
                if found == 0 || self.broken {
                    break;
                }
                changed += found;
            }
            changed += self.gadgets() + self.gadget_pivots();
            if changed == 0 || self.broken {
                return;
            }
        }
    }
}

struct Builder {
    graph: Graph,
    wires: Vec<Option<(usize, Edge)>>,
    rotations: Vec<(usize, f64)>,
}

fn quarter_turns(angle: f64) -> Option<u8> {
    let turns = angle / FRAC_PI_2;
    let nearest = turns.round();
    ((turns - nearest).abs() < 1e-9).then(|| nearest.rem_euclid(4.0) as u8)
}

impl Builder {
    fn new(qubits: usize) -> Builder {
        Builder {
            graph: Graph::default(),
            wires: vec![None; qubits],
            rotations: Vec::new(),
        }
    }

    fn wire(&mut self, q: usize) -> (usize, Edge) {
        match self.wires[q] {
            Some(wire) => wire,
            None => {
                let input = self.graph.add(true, Phase::default());
                self.wires[q] = Some((input, Edge::Plain));
                (input, Edge::Plain)
            }
        }
    }

    fn z(&mut self, q: usize, phase: Phase) {
        let (at, edge) = self.wire(q);
        if edge == Edge::Plain && !self.graph.spider(at).boundary {
            let sum = self.graph.sum(self.graph.spider(at).phase, phase);
            self.graph.spider_mut(at).phase = sum;
        } else {
            let spider = self.graph.add(false, phase);
            self.graph.link(at, spider, edge);
            self.wires[q] = Some((spider, Edge::Plain));
        }
    }

    fn hadamard(&mut self, q: usize) {
        let (at, edge) = self.wire(q);
        self.wires[q] = Some((at, edge.flipped()));
    }

    fn x(&mut self, q: usize, phase: Phase) {
        self.hadamard(q);
        self.z(q, phase);
        self.hadamard(q);
    }

    fn cz(&mut self, a: usize, b: usize) {
        self.z(a, Phase::default());
        self.z(b, Phase::default());
        let (first, _) = self.wire(a);
        let (second, _) = self.wire(b);
        self.graph.toggle(first, second);
    }

    fn rotation(&mut self, at: usize, angle: f64) -> Phase {
        match quarter_turns(angle) {
            Some(quarters) => Phase::constant(quarters),
            None => {
                let variable = self.graph.variables.fresh();
                self.rotations.push((at, angle));
                Phase {
                    quarters: 0,
                    term: Some((false, variable)),
                }
            }
        }
    }

    fn gate(&mut self, gate: &Gate, at: usize) -> bool {
        let angle = match gate.params.first() {
            Some(param) => match param.constant() {
                Some(value) => value.as_f64(),
                None => return false,
            },
            None => 0.0,
        };
        let constant = Phase::constant;
        match (gate.kind, &gate.controls[..], &gate.targets[..]) {
            (GateKind::X, [c], [t]) => {
                self.hadamard(t.index());
                self.cz(c.index(), t.index());
                self.hadamard(t.index());
            }
            (GateKind::Z, [c], [t]) => self.cz(c.index(), t.index()),
            (GateKind::Swap, [], [a, b]) => {
                self.wire(a.index());
                self.wire(b.index());
                self.wires.swap(a.index(), b.index());
            }
            (kind, [], [q]) => {
                let q = q.index();
                match kind {
                    GateKind::I => {}
                    GateKind::H => self.hadamard(q),
                    GateKind::X => self.x(q, constant(2)),
                    GateKind::Y => {
                        self.z(q, constant(2));
                        self.x(q, constant(2));
                    }
                    GateKind::Z => self.z(q, constant(2)),
                    GateKind::S => self.z(q, constant(1)),
                    GateKind::SDag => self.z(q, constant(3)),
                    GateKind::SX => self.x(q, constant(1)),
                    GateKind::SXDag => self.x(q, constant(3)),
                    GateKind::T | GateKind::TDag | GateKind::Rz | GateKind::R1 => {
                        let angle = match kind {
                            GateKind::T => FRAC_PI_4,
                            GateKind::TDag => -FRAC_PI_4,
                            _ => angle,
                        };
                        let phase = self.rotation(at, angle);
                        self.z(q, phase);
                    }
                    GateKind::Rx => {
                        let phase = self.rotation(at, angle);
                        self.x(q, phase);
                    }
                    GateKind::Ry => {
                        self.z(q, constant(3));
                        let phase = self.rotation(at, angle);
                        self.x(q, phase);
                        self.z(q, constant(1));
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn reduce(mut self, ops: &mut [Op], keep: &mut [bool]) -> usize {
        if self.rotations.len() < 2 {
            return 0;
        }
        for q in 0..self.wires.len() {
            if let Some((at, edge)) = self.wires[q] {
                let output = self.graph.add(true, Phase::default());
                self.graph.link(at, output, edge);
            }
        }
        self.graph.simplify();
        if self.graph.broken {
            return 0;
        }
        let mut groups: BTreeMap<usize, Vec<(usize, bool)>> = BTreeMap::new();
        for variable in 0..self.rotations.len() {
            let (root, parity) = self.graph.variables.find(variable);
            groups.entry(root).or_default().push((variable, parity));
        }
        let mut removed = 0;
        for members in groups.values().filter(|members| members.len() > 1) {
            let total: f64 = members
                .iter()
                .map(|&(variable, parity)| {
                    let angle = self.rotations[variable].1;
                    if parity { -angle } else { angle }
                })
                .sum();
            let (first, parity) = members[0];
            for &(variable, _) in &members[1..] {
                keep[self.rotations[variable].0] = false;
                removed += 1;
            }
            let at = self.rotations[first].0;
            let angle = phase::wrap(if parity { -total } else { total });
            if angle.abs() < EPSILON {
                keep[at] = false;
                removed += 1;
            } else if let Op::Gate(gate) = &mut ops[at] {
                match gate.kind {
                    GateKind::Rx | GateKind::Ry => {
                        gate.params = vec![Operand::Const(Const::Float(angle))]
                    }
                    _ => phase::rewrite(gate, angle),
                }
            }
        }
        removed
    }
}

fn teleport_block(ops: &mut Vec<Op>, qubits: usize) -> usize {
    let mut keep = vec![true; ops.len()];
    let mut removed = 0;
    let mut builder = Builder::new(qubits);
    let mut size = 0;
    for at in 0..ops.len() {
        let taken = match &ops[at] {
            Op::Gate(gate) if size < MAX_OPS => builder.gate(gate, at),
            op => op.qubits().is_empty(),
        };
        if taken {
            size += 1;
        } else {
            removed += builder.reduce(ops, &mut keep);
            builder = Builder::new(qubits);
            size = 0;
        }
    }
    removed += builder.reduce(ops, &mut keep);
    keep_marked(ops, &keep);
    removed
}

pub(crate) fn teleport(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    program
        .blocks
        .iter_mut()
        .map(|block| teleport_block(&mut block.ops, qubits))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equiv;
    use crate::qasm;
    use crate::simulator::state::Rng;

    const GATES: [&str; 16] = [
        "h", "s", "sdg", "t", "tdg", "x", "y", "sx", "rz(0.3)", "rx(0.7)", "ry(0.4)", "t", "cx",
        "cz", "swap", "cx",
    ];
    const ANGLES: [&str; 4] = ["t", "tdg", "rz(0.3)", "rz(-1.1)"];

    fn header(qubits: usize) -> String {
        format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n")
    }

    fn random(rng: &mut Rng, qubits: usize, length: usize) -> String {
        let mut text = header(qubits);
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

    fn gadgets(rng: &mut Rng, qubits: usize, count: usize) -> String {
        let mut text = header(qubits);
        for _ in 0..count {
            let parity: Vec<usize> = (0..qubits)
                .filter(|_| rng.next_u64().is_multiple_of(2))
                .collect();
            let Some((&last, rest)) = parity.split_last() else {
                continue;
            };
            let ladder: String = rest
                .iter()
                .map(|q| format!("cx q[{q}], q[{last}];\n"))
                .collect();
            let angle = ANGLES[rng.next_u64() as usize % ANGLES.len()];
            text += &format!("{ladder}{angle} q[{last}];\n{ladder}");
            if rng.next_u64().is_multiple_of(3) {
                text += &format!("h q[{}];\n", rng.next_u64() as usize % qubits);
            }
        }
        text
    }

    fn rotations(program: &Program) -> usize {
        program
            .gates()
            .filter(|gate| match gate.kind {
                GateKind::T | GateKind::TDag => true,
                GateKind::Rz | GateKind::R1 | GateKind::Rx | GateKind::Ry => gate
                    .constant_angle()
                    .is_none_or(|angle| quarter_turns(angle).is_none()),
                _ => false,
            })
            .count()
    }

    #[test]
    fn teleports() {
        let mut rng = Rng::new(3);
        let (mut folded_total, mut teleported_total) = (0, 0);
        for round in 0..600 {
            let qubits = 2 + round % 5;
            let source = if round % 3 == 2 {
                gadgets(&mut rng, qubits, 4 + round % 12)
            } else {
                random(&mut rng, qubits, 10 + round % 50)
            };
            let (original, errors) = qasm::lower(&source);
            assert!(errors.is_empty(), "{errors:?}");
            let mut folded = original.clone();
            while phase::fold(&mut folded) > 0 {}
            let mut teleported = folded.clone();
            teleport(&mut teleported);
            let differences = equiv::compare(
                &equiv::explore(&original),
                &equiv::explore(&teleported),
                true,
            );
            assert!(differences.is_empty(), "{differences:?}\n{source}");
            assert!(rotations(&teleported) <= rotations(&folded), "{source}");
            folded_total += rotations(&folded);
            teleported_total += rotations(&teleported);
        }
        assert!(
            teleported_total < folded_total,
            "{teleported_total} of {folded_total}"
        );
    }
}
