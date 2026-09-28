use std::collections::{HashMap, HashSet};
use std::fmt;
use std::mem;

use crate::ir::*;
use crate::phase;
use crate::simulator::matrix::{Matrix2, matrix_for};
use crate::synth;
use crate::verify;

const ANGLE_EPSILON: f64 = 1e-12;
const MATRIX_EPSILON: f64 = 1e-12;
const MAX_FIXPOINT_ROUNDS: usize = 16;
const SCAN: usize = 32;

#[derive(Debug, Default)]
pub struct OptStats {
    pub gates_before: usize,
    pub gates_after: usize,
    pub depth_before: usize,
    pub depth_after: usize,
    pub ops_before: usize,
    pub ops_after: usize,
    pub applied: Vec<(&'static str, usize)>,
    pub rounds: usize,
    pub violations: Vec<String>,
}

impl OptStats {
    pub fn gates_removed(&self) -> usize {
        self.gates_before.saturating_sub(self.gates_after)
    }

    pub fn changed(&self) -> bool {
        !self.applied.is_empty()
    }
}

impl fmt::Display for OptStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "gates {} -> {}, depth {} -> {}, ops {} -> {} ({} rounds)",
            self.gates_before,
            self.gates_after,
            self.depth_before,
            self.depth_after,
            self.ops_before,
            self.ops_after,
            self.rounds
        )?;

        for (name, hits) in &self.applied {
            writeln!(f, "  {name}: {hits}")?;
        }

        Ok(())
    }
}

pub fn optimise(program: &mut Program, level: u8, verify_each: bool) -> OptStats {
    let mut stats = OptStats {
        gates_before: program.gate_count(),
        depth_before: program.depth(),
        ops_before: program.op_count(),
        ..Default::default()
    };

    if level == 0 {
        stats.gates_after = stats.gates_before;
        stats.depth_after = stats.depth_before;
        stats.ops_after = stats.ops_before;
        return stats;
    }

    let mut totals: HashMap<&'static str, usize> = HashMap::new();
    let rounds = if level >= 2 { MAX_FIXPOINT_ROUNDS } else { 1 };

    for round in 0..rounds {
        let mut changed = 0usize;

        for (name, pass) in schedule(level) {
            let hits = pass(program);
            if verify_each {
                for found in verify::verify(program) {
                    stats.violations.push(format!("after {name}: {found}"));
                }
            }
            if hits > 0 {
                *totals.entry(name).or_insert(0) += hits;
                changed += hits;
            }
        }

        stats.rounds = round + 1;

        if changed == 0 {
            break;
        }
    }

    let mut applied: Vec<(&'static str, usize)> = totals.into_iter().collect();
    applied.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    stats.applied = applied;
    stats.gates_after = program.gate_count();
    stats.depth_after = program.depth();
    stats.ops_after = program.op_count();
    stats
}

type Pass = fn(&mut Program) -> usize;

fn schedule(level: u8) -> Vec<(&'static str, Pass)> {
    let mut schedule: Vec<(&'static str, Pass)> = vec![
        ("drop-identity", drop_identity_gates),
        ("drop-before-measure", drop_before_measure),
        ("cancel-inverses", cancel_inverses),
        ("merge-rotations", merge_rotations),
        ("fold-constants", fold_constants),
    ];

    if level >= 2 {
        schedule.push(("peephole", peephole));
        schedule.push(("fold-phases", phase::fold));
        schedule.push(("simplify-cfg", simplify_cfg));
    }

    if level >= 3 {
        schedule.push(("fuse-single-qubit", fuse_single_qubit));
    }

    schedule.push(("dead-code", eliminate_dead_code));
    schedule
}

fn signature(gate: &Gate) -> (Vec<QubitId>, Vec<QubitId>) {
    let mut controls = gate.controls.clone();
    controls.sort();

    let mut targets = gate.targets.clone();
    if gate.kind == GateKind::Swap {
        targets.sort();
    }

    (controls, targets)
}

fn are_inverse(a: &Gate, b: &Gate) -> bool {
    if signature(a) != signature(b) {
        return false;
    }

    if a.kind.param_count() > 0 {
        if a.kind != b.kind {
            return false;
        }
        return match (a.constant_angle(), b.constant_angle()) {
            (Some(x), Some(y)) => (x + y).abs() < ANGLE_EPSILON,
            _ => false,
        };
    }

    if let (GateKind::Unitary(x), GateKind::Unitary(y)) = (a.kind, b.kind) {
        let product = Matrix2::from_ir(x).multiply(Matrix2::from_ir(y));
        return product.is_identity(MATRIX_EPSILON);
    }

    a.kind.adjoint() == Some(b.kind)
}

enum Rewrite {
    Drop,
    Replace(Gate),
}

fn rewrite_pairs(
    program: &mut Program,
    mut decide: impl FnMut(&Gate, &Gate) -> Option<Rewrite>,
) -> usize {
    let mut hits = 0;

    for block in &mut program.blocks {
        let mut ops: Vec<Option<Op>> = mem::take(&mut block.ops).into_iter().map(Some).collect();
        let mut index = 0;

        while index < ops.len() {
            let Some((partner, crossed, rewrite)) = pair_at(&ops, index, &mut decide) else {
                index += 1;
                continue;
            };

            match rewrite {
                Rewrite::Replace(gate) if crossed => {
                    ops[partner] = Some(Op::Gate(gate));
                    ops[index] = None;
                }
                Rewrite::Replace(gate) => {
                    ops[partner] = None;
                    ops[index] = Some(Op::Gate(gate));
                }
                Rewrite::Drop => {
                    ops[partner] = None;
                    ops[index] = None;
                }
            }
            hits += 1;
            let mut back = 0;
            while index > 0 && back < SCAN {
                index -= 1;
                if ops[index].is_some() {
                    back += 1;
                }
            }
        }

        block.ops = ops.into_iter().flatten().collect();
    }

    hits
}

fn pair_at(
    ops: &[Option<Op>],
    index: usize,
    decide: &mut impl FnMut(&Gate, &Gate) -> Option<Rewrite>,
) -> Option<(usize, bool, Rewrite)> {
    let gate = ops[index].as_ref()?.as_gate()?;
    let wires: Vec<QubitId> = gate.wires().collect();
    let mut crossed = false;
    let mut seen = 0;

    for (offset, op) in ops[index + 1..].iter().enumerate() {
        let Some(op) = op else {
            continue;
        };
        if !op.qubits().iter().any(|q| wires.contains(q)) {
            continue;
        }
        seen += 1;
        let other = op.as_gate()?;
        if let Some(rewrite) = decide(gate, other) {
            return Some((index + 1 + offset, crossed, rewrite));
        }
        if seen == SCAN || !synth::commute(gate, other) {
            return None;
        }
        crossed = true;
    }

    None
}

fn cancel_inverses(program: &mut Program) -> usize {
    rewrite_pairs(program, |a, b| are_inverse(a, b).then_some(Rewrite::Drop)) * 2
}

fn merge_rotations(program: &mut Program) -> usize {
    rewrite_pairs(program, |a, b| {
        if a.kind.param_count() == 0 || a.kind != b.kind || signature(a) != signature(b) {
            return None;
        }
        let (x, y) = (a.constant_angle()?, b.constant_angle()?);
        let mut merged = a.clone();
        merged.params = vec![Operand::Const(Const::Float(x + y))];
        Some(Rewrite::Replace(merged))
    })
}

fn is_identity_gate(gate: &Gate) -> bool {
    match gate.kind {
        GateKind::I => true,
        GateKind::Unitary(m) => Matrix2::from_ir(m).is_identity(MATRIX_EPSILON),
        GateKind::Rx | GateKind::Ry | GateKind::Rz | GateKind::R1 => gate
            .constant_angle()
            .is_some_and(|angle| angle.abs() < ANGLE_EPSILON),
        GateKind::Swap => gate.targets.len() == 2 && gate.targets[0] == gate.targets[1],
        _ => false,
    }
}

pub(crate) fn drop_before_measure(program: &mut Program) -> usize {
    let mut removed = 0;
    for block in &mut program.blocks {
        let mut settled: HashSet<QubitId> = HashSet::new();
        let mut keep = vec![true; block.ops.len()];
        for (index, op) in block.ops.iter().enumerate().rev() {
            match op {
                Op::Measure { qubit, .. } | Op::Reset { qubit, .. } => {
                    settled.insert(*qubit);
                }
                Op::Gate(gate)
                    if phase::diagonal(gate.kind) && gate.wires().all(|q| settled.contains(&q)) =>
                {
                    keep[index] = false;
                    removed += 1;
                }
                _ => {
                    for q in op.qubits() {
                        settled.remove(&q);
                    }
                }
            }
        }
        let mut index = 0;
        block.ops.retain(|_| {
            index += 1;
            keep[index - 1]
        });
    }
    removed
}

fn drop_identity_gates(program: &mut Program) -> usize {
    let mut removed = 0;

    for block in &mut program.blocks {
        let before = block.ops.len();
        block
            .ops
            .retain(|op| !op.as_gate().is_some_and(is_identity_gate));
        removed += before - block.ops.len();
    }

    removed
}

fn peephole(program: &mut Program) -> usize {
    let mut rewrites = 0;

    for block in &mut program.blocks {
        let mut index = 0;

        while index + 2 < block.ops.len() {
            let [Op::Gate(first), Op::Gate(middle), Op::Gate(last)] = &block.ops[index..index + 3]
            else {
                index += 1;
                continue;
            };

            let uncontrolled =
                first.controls.is_empty() && middle.controls.is_empty() && last.controls.is_empty();

            let same_wire = first.targets.len() == 1
                && first.targets == middle.targets
                && middle.targets == last.targets;

            if uncontrolled
                && same_wire
                && first.kind == GateKind::H
                && last.kind == GateKind::H
                && matches!(middle.kind, GateKind::X | GateKind::Z)
            {
                let mut rewritten = middle.clone();
                rewritten.kind = if middle.kind == GateKind::X {
                    GateKind::Z
                } else {
                    GateKind::X
                };

                block.ops.splice(index..index + 3, [Op::Gate(rewritten)]);
                rewrites += 1;
                continue;
            }

            index += 1;
        }
    }

    rewrites
}

fn fuse_single_qubit(program: &mut Program) -> usize {
    rewrite_pairs(program, |a, b| {
        if !a.controls.is_empty() || a.targets.len() != 1 {
            return None;
        }
        if a.is_parameterised() && a.constant_angle().is_none() {
            return None;
        }

        let wire = a.targets[0];
        if !b.controls.is_empty() || b.targets != [wire] {
            return None;
        }
        if b.is_parameterised() && b.constant_angle().is_none() {
            return None;
        }

        let first = matrix_for(a.kind, &a.constant_params());
        let second = matrix_for(b.kind, &b.constant_params());
        let mut fused = a.clone();
        fused.kind = GateKind::Unitary(second.multiply(first).to_ir());
        fused.params.clear();
        Some(Rewrite::Replace(fused))
    })
}

fn fold_constants(program: &mut Program) -> usize {
    let mut folded = 0;
    let mut known: HashMap<ValueId, Const> = HashMap::new();

    let single_block = program.blocks.len() == 1;

    for block in &mut program.blocks {
        for op in &mut block.ops {
            let Op::Assign { dest, ty, expr, .. } = op else {
                continue;
            };

            if let Expr::Const(value) = expr {
                known.insert(*dest, *value);
                continue;
            }

            if matches!(expr, Expr::Load(_)) {
                continue;
            }

            if !single_block && matches!(expr, Expr::Phi(_)) {
                continue;
            }

            if let Some(value) = expr.fold(*ty, |o| o.resolve(&known)) {
                *expr = Expr::Const(value);
                known.insert(*dest, value);
                folded += 1;
            }
        }
    }

    if folded > 0 {
        substitute_known(program, &known);
    }

    folded
}

fn substitute_known(program: &mut Program, known: &HashMap<ValueId, Const>) {
    let replace = |operand: &mut Operand| {
        if let Operand::Value(id) = operand
            && let Some(value) = known.get(id)
        {
            *operand = Operand::Const(*value);
        }
    };

    for block in &mut program.blocks {
        for op in &mut block.ops {
            match op {
                Op::Gate(gate) => {
                    for param in &mut gate.params {
                        replace(param);
                    }
                }
                Op::Store { value, .. } => replace(value),
                Op::RecordOutput {
                    value: Some(value), ..
                } => replace(value),
                _ => {}
            }
        }

        if let Some(o) = block.term.operand_mut() {
            replace(o);
        }
    }
}

fn eliminate_dead_code(program: &mut Program) -> usize {
    let mut live: HashSet<ValueId> = HashSet::new();

    for block in &program.blocks {
        live.extend(
            block
                .ops
                .iter()
                .flat_map(Op::operands)
                .chain(block.term.operand())
                .filter_map(|o| o.value()),
        );
    }

    let mut removed = 0;

    for block in &mut program.blocks {
        let before = block.ops.len();
        block.ops.retain(|op| match op {
            Op::Assign { dest, expr, .. } => {
                live.contains(dest) || matches!(expr, Expr::ReadResult(_) | Expr::Load(_))
            }
            _ => true,
        });
        removed += before - block.ops.len();
    }

    removed
}

fn simplify_cfg(program: &mut Program) -> usize {
    let mut changes = 0;

    for index in 0..program.blocks.len() {
        let replacement = match &program.blocks[index].term {
            Term::CondBr {
                cond,
                if_true,
                if_false,
            } => match cond.constant() {
                Some(value) => Some(Term::Br(if value.truthy() { *if_true } else { *if_false })),
                None if if_true == if_false => Some(Term::Br(*if_true)),
                None => None,
            },
            Term::Switch {
                scrutinee,
                cases,
                default,
            } => scrutinee.constant().map(|value| {
                let key = value.as_i64();
                Term::Br(
                    cases
                        .iter()
                        .find(|(candidate, _)| *candidate == key)
                        .map(|(_, block)| *block)
                        .unwrap_or(*default),
                )
            }),
            _ => None,
        };

        if let Some(term) = replacement {
            program.blocks[index].term = term;
            changes += 1;
        }
    }

    let reachable = program.reachable();
    for (index, live) in reachable.iter().enumerate() {
        if !live && !program.blocks[index].ops.is_empty() {
            program.blocks[index].ops.clear();
            program.blocks[index].term = Term::Unreachable;
            changes += 1;
        }
    }

    changes
}
