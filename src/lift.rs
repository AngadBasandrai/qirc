use std::collections::HashMap;
use std::f64::consts::FRAC_PI_4;
use std::mem;

use crate::diag::Span;
use crate::ir::*;
use crate::phase;
use crate::transpile;

const SWEEPS: usize = 64;
const EPSILON: f64 = 1e-9;

fn triple(op: &Op) -> Option<&Gate> {
    op.as_gate().filter(|g| {
        matches!(g.kind, GateKind::Z | GateKind::X)
            && g.controls.len() == 2
            && g.targets.len() == 1
            && g.params.is_empty()
    })
}

pub fn holds_triples(program: &Program) -> bool {
    program.ops().any(|op| triple(op).is_some())
}

fn plain_h(op: &Op, wire: QubitId) -> bool {
    op.as_gate()
        .is_some_and(|g| g.kind == GateKind::H && g.controls.is_empty() && g.targets == [wire])
}

fn wrap_targets(ops: Vec<Op>) -> Vec<Op> {
    let mut before: Vec<[Option<usize>; 3]> = vec![[None; 3]; ops.len()];
    let mut after = before.clone();
    let mut seen: HashMap<QubitId, usize> = HashMap::new();
    for (index, op) in ops.iter().enumerate() {
        if let Some(gate) = triple(op) {
            for (slot, wire) in gate.wires().enumerate() {
                before[index][slot] = seen.get(&wire).copied();
            }
        }
        for wire in op.qubits() {
            seen.insert(wire, index);
        }
    }
    seen.clear();
    for (index, op) in ops.iter().enumerate().rev() {
        if let Some(gate) = triple(op) {
            for (slot, wire) in gate.wires().enumerate() {
                after[index][slot] = seen.get(&wire).copied();
            }
        }
        for wire in op.qubits() {
            seen.insert(wire, index);
        }
    }

    let mut ops: Vec<Option<Op>> = ops.into_iter().map(Some).collect();
    for index in 0..ops.len() {
        let Some(gate) = ops[index].as_ref().and_then(triple) else {
            continue;
        };
        if gate.kind != GateKind::Z {
            continue;
        }
        let wires: Vec<QubitId> = gate.wires().collect();
        for slot in [2, 1, 0] {
            let wire = wires[slot];
            let (Some(left), Some(right)) = (before[index][slot], after[index][slot]) else {
                continue;
            };
            let both = [left, right]
                .iter()
                .all(|&at| ops[at].as_ref().is_some_and(|op| plain_h(op, wire)));
            if !both {
                continue;
            }
            ops[left] = None;
            ops[right] = None;
            if let Some(Op::Gate(gate)) = &mut ops[index] {
                gate.kind = GateKind::X;
                gate.controls = wires.iter().copied().filter(|w| *w != wire).collect();
                gate.targets = vec![wire];
            }
            break;
        }
    }

    ops.into_iter().flatten().collect()
}

#[derive(Default)]
struct Class {
    fixed: f64,
    members: Vec<(usize, f64)>,
}

struct Site {
    classes: [Option<usize>; 7],
}

struct Survey {
    classes: Vec<Class>,
    sites: Vec<Site>,
    signs: Vec<f64>,
}

impl Survey {
    fn total(&self, class: usize) -> f64 {
        let class = &self.classes[class];
        let turns: f64 = class
            .members
            .iter()
            .map(|&(site, weight)| weight * self.signs[site])
            .sum();
        phase::wrap(class.fixed + FRAC_PI_4 * turns)
    }

    fn silent(&self, class: usize) -> bool {
        self.total(class).abs() < EPSILON
    }

    fn quiet(&self, site: usize) -> usize {
        self.sites[site]
            .classes
            .iter()
            .flatten()
            .filter(|&&class| self.silent(class))
            .count()
    }

    fn settle(&mut self) {
        for _ in 0..SWEEPS {
            let mut improved = false;
            for site in 0..self.sites.len() {
                let before = self.quiet(site);
                self.signs[site] = -self.signs[site];
                if self.quiet(site) > before {
                    improved = true;
                } else {
                    self.signs[site] = -self.signs[site];
                }
            }
            if !improved {
                break;
            }
        }
    }

    fn pair(&self, site: usize) -> usize {
        let score = |mask: usize| match self.sites[site].classes[mask - 1] {
            Some(class) if self.silent(class) => 3,
            Some(class) => {
                let shared = self.classes[class].members.len()
                    + usize::from(self.classes[class].fixed.abs() > EPSILON);
                usize::from(shared > 1)
            }
            None => 0,
        };
        [5, 6].into_iter().fold(3, |best, mask| {
            if score(mask) > score(best) {
                mask
            } else {
                best
            }
        })
    }
}

fn survey(ops: &[Op], paired: &[Option<[QubitId; 2]>], qubits: usize) -> Survey {
    let mut fresh = qubits as u32;
    let mut parity: Vec<(Vec<u32>, bool)> = (0..qubits as u32).map(|q| (vec![q], false)).collect();
    let mut epoch = 0u32;
    let mut ids: HashMap<(u32, Vec<u32>), usize> = HashMap::new();
    let mut found = Survey {
        classes: Vec::new(),
        sites: Vec::new(),
        signs: Vec::new(),
    };
    let mut renew = |parity: &mut Vec<(Vec<u32>, bool)>, q: usize| {
        parity[q] = (vec![fresh], false);
        fresh += 1;
    };

    for (index, op) in ops.iter().enumerate() {
        if let Some(gate) = triple(op) {
            let target = gate.targets[0].index();
            let wrapped = gate.kind == GateKind::X;
            if wrapped {
                renew(&mut parity, target);
            }
            if paired[index].is_none() {
                let wires: Vec<usize> = gate.wires().map(QubitId::index).collect();
                let site = found.sites.len();
                let mut classes = [None; 7];
                for mask in 1..8usize {
                    let mut key = Vec::new();
                    let mut flipped = false;
                    for (slot, wire) in wires.iter().enumerate() {
                        if mask >> slot & 1 == 1 {
                            key = phase::xor(&key, &parity[*wire].0);
                            flipped ^= parity[*wire].1;
                        }
                    }
                    if key.is_empty() {
                        continue;
                    }
                    let weight = if mask.count_ones() == 2 { -1.0 } else { 1.0 };
                    let weight = if flipped { -weight } else { weight };
                    let class = *ids.entry((epoch, key)).or_insert_with(|| {
                        found.classes.push(Class::default());
                        found.classes.len() - 1
                    });
                    found.classes[class].members.push((site, weight));
                    classes[mask - 1] = Some(class);
                }
                found.sites.push(Site { classes });
                found.signs.push(1.0);
            }
            if wrapped {
                renew(&mut parity, target);
            }
            continue;
        }

        let wires = op.qubits();
        let tracked = match op {
            Op::Gate(gate) => match (
                phase::slope(gate),
                gate.kind,
                &gate.controls[..],
                &gate.targets[..],
            ) {
                (Some(slope), ..) => {
                    let (key, flipped) = &parity[gate.targets[0].index()];
                    if !key.is_empty() {
                        let class = *ids.entry((epoch, key.clone())).or_insert_with(|| {
                            found.classes.push(Class::default());
                            found.classes.len() - 1
                        });
                        found.classes[class].fixed += if *flipped { -slope } else { slope };
                    }
                    true
                }
                (None, kind, ..) if phase::diagonal(kind) => true,
                (None, GateKind::X, [], [t]) => {
                    parity[t.index()].1 ^= true;
                    true
                }
                (None, GateKind::X, [c], [t]) => {
                    let control = parity[c.index()].clone();
                    let target = &mut parity[t.index()];
                    target.0 = phase::xor(&target.0, &control.0);
                    target.1 ^= control.1;
                    true
                }
                (None, GateKind::Swap, [], [a, b]) => {
                    parity.swap(a.index(), b.index());
                    true
                }
                (None, GateKind::H, [], [t]) => {
                    renew(&mut parity, t.index());
                    true
                }
                _ => false,
            },
            _ => wires.is_empty(),
        };
        if !tracked {
            epoch += 1;
            for q in wires {
                renew(&mut parity, q.index());
            }
        }
    }

    found
}

fn simple(kind: GateKind, controls: &[QubitId], target: QubitId, span: Span) -> Op {
    Op::Gate(Gate {
        kind,
        controls: controls.to_vec(),
        targets: vec![target],
        params: Vec::new(),
        span,
    })
}

fn full(out: &mut Vec<Op>, [x, y, z]: [QubitId; 3], sign: f64, span: Span) {
    let turn = |q, positive: bool| {
        let kind = if positive == (sign > 0.0) {
            GateKind::T
        } else {
            GateKind::TDag
        };
        simple(kind, &[], q, span)
    };
    let cx = |c, t| simple(GateKind::X, &[c], t, span);
    out.extend([
        cx(y, z),
        turn(z, false),
        cx(x, z),
        turn(z, true),
        cx(y, z),
        turn(z, false),
        cx(x, z),
        cx(x, y),
        turn(y, false),
        cx(x, y),
        turn(x, true),
        turn(y, true),
        turn(z, true),
    ]);
}

fn relative(out: &mut Vec<Op>, [a, b]: [QubitId; 2], target: QubitId, span: Span) {
    let one = |kind| simple(kind, &[], target, span);
    let cx = |c| simple(GateKind::X, &[c], target, span);
    out.extend([
        one(GateKind::H),
        one(GateKind::T),
        cx(b),
        one(GateKind::TDag),
        cx(a),
        one(GateKind::T),
        cx(b),
        one(GateKind::TDag),
        one(GateKind::H),
    ]);
}

struct Choice {
    sign: f64,
    roles: [QubitId; 3],
}

fn emit(ops: &[Op], paired: &[Option<[QubitId; 2]>], choices: &[Choice]) -> Vec<Op> {
    let mut out = Vec::with_capacity(ops.len() * 4);
    let mut choices = choices.iter();
    for (op, paired) in ops.iter().zip(paired) {
        let Some(gate) = triple(op) else {
            out.push(op.clone());
            continue;
        };
        let span = gate.span;
        let target = gate.targets[0];
        if let Some(controls) = paired {
            relative(&mut out, *controls, target, span);
            continue;
        }
        let Some(choice) = choices.next() else {
            out.push(op.clone());
            continue;
        };
        let wrapped = gate.kind == GateKind::X;
        if wrapped {
            out.push(simple(GateKind::H, &[], target, span));
        }
        full(&mut out, choice.roles, choice.sign, span);
        if wrapped {
            out.push(simple(GateKind::H, &[], target, span));
        }
    }
    out
}

pub fn lower(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    let mut lowered = 0;

    for block in 0..program.blocks.len() {
        if !program.blocks[block]
            .ops
            .iter()
            .any(|op| triple(op).is_some())
        {
            continue;
        }
        let ops = wrap_targets(mem::take(&mut program.blocks[block].ops));
        let paired = transpile::relative_pairs(&ops);

        let mut choices: Vec<Choice> = ops
            .iter()
            .zip(&paired)
            .filter(|(_, paired)| paired.is_none())
            .filter_map(|(op, _)| triple(op))
            .map(|gate| Choice {
                sign: 1.0,
                roles: [gate.controls[0], gate.controls[1], gate.targets[0]],
            })
            .collect();
        lowered += ops.iter().filter(|op| triple(op).is_some()).count();

        let mut found = survey(&ops, &paired, qubits);
        found.settle();
        for (site, choice) in choices.iter_mut().enumerate() {
            let [a, b, t] = choice.roles;
            choice.sign = found.signs[site];
            choice.roles = match found.pair(site) {
                5 => [a, t, b],
                6 => [b, t, a],
                _ => [a, b, t],
            };
            if choice.roles[0] > choice.roles[1] {
                choice.roles.swap(0, 1);
            }
        }

        program.blocks[block].ops = emit(&ops, &paired, &choices);
    }

    lowered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::exec::{self, ExecConfig};
    use crate::simulator::matrix::C64;
    use crate::synth::Unitary;

    const WIRES: [QubitId; 3] = [QubitId(0), QubitId(1), QubitId(2)];

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

    fn gates(ops: &[Op]) -> Vec<Gate> {
        ops.iter().filter_map(Op::as_gate).cloned().collect()
    }

    fn turn(angle: f64, wire: QubitId) -> Op {
        Op::Gate(Gate {
            kind: GateKind::Rz,
            controls: Vec::new(),
            targets: vec![wire],
            params: vec![Operand::Const(Const::Float(angle))],
            span: Span::DUMMY,
        })
    }

    #[test]
    fn every_lowering_is_a_ccz() {
        let want = Unitary::of(
            &gates(&[simple(GateKind::Z, &WIRES[..2], WIRES[2], Span::DUMMY)]),
            &WIRES,
        )
        .unwrap();
        for roles in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 2, 0],
            [1, 0, 2],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            for sign in [1.0, -1.0] {
                let mut out = Vec::new();
                full(&mut out, roles.map(|q| WIRES[q]), sign, Span::DUMMY);
                let got = Unitary::of(&gates(&out), &WIRES).unwrap();
                assert!(got.same(&want), "roles {roles:?} sign {sign}");
            }
        }
    }

    fn state(program: &Program) -> Vec<C64> {
        let state = exec::execute_vector(program, ExecConfig::default())
            .final_state
            .unwrap();
        (0..state.len()).map(|i| state.amplitude(i)).collect()
    }

    fn random(dice: &mut Dice, qubits: u32, length: usize) -> Program {
        let mut ops = Vec::new();
        for q in 0..qubits {
            for kind in [GateKind::Ry, GateKind::Rz] {
                let mut op = turn(0.3 + 0.37 * dice.roll(16) as f64, QubitId(q));
                if let Op::Gate(gate) = &mut op {
                    gate.kind = kind;
                }
                ops.push(op);
            }
        }
        for _ in 0..length {
            let mut wires: Vec<QubitId> = (0..qubits).map(QubitId).collect();
            for slot in 0..3 {
                let pick = slot + dice.roll(wires.len() - slot);
                wires.swap(slot, pick);
            }
            let [a, b, t] = [wires[0], wires[1], wires[2]];
            match dice.roll(9) {
                0 => ops.push(simple(GateKind::H, &[], t, Span::DUMMY)),
                1 => ops.push(simple(GateKind::X, &[], t, Span::DUMMY)),
                2 => ops.push(simple(GateKind::X, &[a], t, Span::DUMMY)),
                3 => ops.push(turn(FRAC_PI_4 * (1 + dice.roll(7)) as f64, t)),
                4 | 5 => ops.push(simple(GateKind::Z, &[a, b], t, Span::DUMMY)),
                6 => ops.push(simple(GateKind::X, &[a, b], t, Span::DUMMY)),
                _ => ops.extend([
                    simple(GateKind::H, &[], t, Span::DUMMY),
                    simple(GateKind::Z, &[a, b], t, Span::DUMMY),
                    simple(GateKind::H, &[], t, Span::DUMMY),
                ]),
            }
        }
        let mut program = Program::new("lift", Profile::Base);
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

    #[test]
    fn random_circuits_keep_their_state() {
        let mut dice = Dice(0x5eed);
        for round in 0..120 {
            let qubits = 3 + (round % 4) as u32;
            let program = random(&mut dice, qubits, 6 + round % 20);
            let want = state(&program);
            let triples = program.ops().filter(|op| triple(op).is_some()).count();
            let mut lowered = program.clone();
            assert_eq!(lower(&mut lowered), triples, "round {round}");
            assert!(!holds_triples(&lowered));
            let got = state(&lowered);
            let overlap: C64 = want.iter().zip(&got).map(|(a, b)| a.conj() * b).sum();
            assert!((overlap.norm() - 1.0).abs() < 1e-9, "round {round}");
        }
    }
}
