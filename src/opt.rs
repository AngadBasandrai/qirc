use std::collections::{HashMap, HashSet};

use crate::ir::*;
use crate::simulator::matrix::{Matrix2, matrix_for};

const ANGLE_EPSILON: f64 = 1e-12;
const MATRIX_EPSILON: f64 = 1e-12;
const MAX_FIXPOINT_ROUNDS: usize = 16;

#[derive(Clone, Debug, Default)]
pub struct OptStats {
    pub gates_before: usize,
    pub gates_after: usize,
    pub depth_before: usize,
    pub depth_after: usize,
    pub ops_before: usize,
    pub ops_after: usize,
    pub applied: Vec<(&'static str, usize)>,
    pub rounds: usize,
}

impl OptStats {
    pub fn gates_removed(&self) -> usize {
        self.gates_before.saturating_sub(self.gates_after)
    }

    pub fn changed(&self) -> bool {
        !self.applied.is_empty()
    }
}

impl std::fmt::Display for OptStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

pub fn optimise(program: &mut Program, level: u8) -> OptStats {
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

        for (name, hits) in run_round(program, level) {
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

fn run_round(program: &mut Program, level: u8) -> Vec<(&'static str, usize)> {
    let mut results = Vec::new();

    results.push(("drop-identity", drop_identity_gates(program)));
    results.push(("cancel-inverses", cancel_inverses(program)));
    results.push(("merge-rotations", merge_rotations(program)));
    results.push(("fold-constants", fold_constants(program)));

    if level >= 2 {
        results.push(("peephole", peephole(program)));
        results.push(("simplify-cfg", simplify_cfg(program)));
    }

    if level >= 3 {
        results.push(("fuse-single-qubit", fuse_single_qubit(program)));
    }

    results.push(("dead-code", eliminate_dead_code(program)));
    results
}

fn op_wires(op: &Op) -> Vec<QubitId> {
    match op {
        Op::Gate(gate) => gate.wires().collect(),
        Op::Measure { qubit, .. } | Op::Reset { qubit, .. } => vec![*qubit],
        _ => Vec::new(),
    }
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

fn next_on_shared_wire(ops: &[Op], from: usize, wires: &[QubitId]) -> Option<usize> {
    let set: HashSet<QubitId> = wires.iter().copied().collect();

    for (offset, op) in ops.iter().enumerate().skip(from + 1) {
        if op_wires(op).iter().any(|w| set.contains(w)) {
            return Some(offset);
        }
    }

    None
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

fn cancel_inverses(program: &mut Program) -> usize {
    let mut removed = 0;

    for block in &mut program.blocks {
        let mut changed = true;

        while changed {
            changed = false;

            for index in 0..block.ops.len() {
                let Some(gate) = block.ops[index].as_gate() else {
                    continue;
                };
                let wires: Vec<QubitId> = gate.wires().collect();

                let Some(partner) = next_on_shared_wire(&block.ops, index, &wires) else {
                    continue;
                };

                let Some(other) = block.ops[partner].as_gate() else {
                    continue;
                };

                if are_inverse(gate, other) {
                    block.ops.remove(partner);
                    block.ops.remove(index);
                    removed += 2;
                    changed = true;
                    break;
                }
            }
        }
    }

    removed
}

fn merge_rotations(program: &mut Program) -> usize {
    let mut merged = 0;

    for block in &mut program.blocks {
        let mut changed = true;

        while changed {
            changed = false;

            for index in 0..block.ops.len() {
                let Some(gate) = block.ops[index].as_gate() else {
                    continue;
                };
                if gate.kind.param_count() == 0 {
                    continue;
                }

                let wires: Vec<QubitId> = gate.wires().collect();
                let Some(partner) = next_on_shared_wire(&block.ops, index, &wires) else {
                    continue;
                };

                let Some(other) = block.ops[partner].as_gate() else {
                    continue;
                };

                if gate.kind != other.kind || signature(gate) != signature(other) {
                    continue;
                }

                let (Some(x), Some(y)) = (gate.constant_angle(), other.constant_angle()) else {
                    continue;
                };

                let total = x + y;
                block.ops.remove(partner);

                if let Op::Gate(target) = &mut block.ops[index] {
                    target.params = vec![Operand::Const(Const::Float(total))];
                }

                merged += 1;
                changed = true;
                break;
            }
        }
    }

    merged
}

fn is_identity_gate(gate: &Gate) -> bool {
    match gate.kind {
        GateKind::I => true,
        GateKind::Unitary(m) => Matrix2::from_ir(m).is_identity(MATRIX_EPSILON),
        GateKind::Rx | GateKind::Ry | GateKind::Rz | GateKind::R1 => gate
            .constant_angle()
            .map(|angle| angle.abs() < ANGLE_EPSILON)
            .unwrap_or(false),
        GateKind::Swap => gate.targets.len() == 2 && gate.targets[0] == gate.targets[1],
        _ => false,
    }
}

fn drop_identity_gates(program: &mut Program) -> usize {
    let mut removed = 0;

    for block in &mut program.blocks {
        let before = block.ops.len();
        block.ops.retain(|op| match op.as_gate() {
            Some(gate) => !is_identity_gate(gate),
            None => true,
        });
        removed += before - block.ops.len();
    }

    removed
}

fn peephole(program: &mut Program) -> usize {
    let mut rewrites = 0;

    for block in &mut program.blocks {
        let mut index = 0;

        while index + 2 < block.ops.len() {
            let window = (
                block.ops[index].as_gate().cloned(),
                block.ops[index + 1].as_gate().cloned(),
                block.ops[index + 2].as_gate().cloned(),
            );

            let (Some(first), Some(middle), Some(last)) = window else {
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
                let replacement = if middle.kind == GateKind::X {
                    GateKind::Z
                } else {
                    GateKind::X
                };

                let mut rewritten = middle.clone();
                rewritten.kind = replacement;

                block.ops.drain(index..index + 3);
                block.ops.insert(index, Op::Gate(rewritten));
                rewrites += 1;
                continue;
            }

            index += 1;
        }
    }

    rewrites
}

fn fuse_single_qubit(program: &mut Program) -> usize {
    let mut fused = 0;

    for block in &mut program.blocks {
        let mut changed = true;

        while changed {
            changed = false;

            for index in 0..block.ops.len() {
                let Some(gate) = block.ops[index].as_gate() else {
                    continue;
                };

                if !gate.controls.is_empty() || gate.targets.len() != 1 {
                    continue;
                }
                if gate.is_parameterised() && gate.constant_angle().is_none() {
                    continue;
                }

                let wire = gate.targets[0];
                let Some(partner) = next_on_shared_wire(&block.ops, index, &[wire]) else {
                    continue;
                };

                let Some(other) = block.ops[partner].as_gate() else {
                    continue;
                };

                if !other.controls.is_empty() || other.targets != vec![wire] {
                    continue;
                }
                if other.kind == GateKind::Swap || gate.kind == GateKind::Swap {
                    continue;
                }
                if other.is_parameterised() && other.constant_angle().is_none() {
                    continue;
                }

                let first = matrix_for(gate.kind, &collect_params(gate));
                let second = matrix_for(other.kind, &collect_params(other));
                let combined = second.multiply(first);

                block.ops.remove(partner);
                if let Op::Gate(target) = &mut block.ops[index] {
                    target.kind = GateKind::Unitary(combined.to_ir());
                    target.params.clear();
                }

                fused += 1;
                changed = true;
                break;
            }
        }
    }

    fused
}

fn collect_params(gate: &Gate) -> Vec<f64> {
    gate.params
        .iter()
        .filter_map(|p| p.constant().map(|c| c.as_f64()))
        .collect()
}

fn fold_constants(program: &mut Program) -> usize {
    let mut folded = 0;
    let mut known: HashMap<ValueId, Const> = HashMap::new();

    let single_block = program.blocks.len() == 1;

    for block in &mut program.blocks {
        for op in &mut block.ops {
            let Op::Assign { dest, expr, .. } = op else {
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

            if let Some(value) = try_fold(expr, &known) {
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

fn try_fold(expr: &Expr, known: &HashMap<ValueId, Const>) -> Option<Const> {
    let lookup = |operand: &Operand| -> Option<Const> {
        match operand {
            Operand::Const(c) => Some(*c),
            Operand::Value(id) => known.get(id).copied(),
        }
    };

    match expr {
        Expr::Copy(operand) => lookup(operand),
        Expr::Binary { op, lhs, rhs } => {
            let (a, b) = (lookup(lhs)?, lookup(rhs)?);
            if op.is_float() {
                let (x, y) = (a.as_f64(), b.as_f64());
                Some(Const::Float(match op {
                    BinOp::FAdd => x + y,
                    BinOp::FSub => x - y,
                    BinOp::FMul => x * y,
                    BinOp::FDiv => x / y,
                    _ => return None,
                }))
            } else {
                let (x, y) = (a.as_i64(), b.as_i64());
                Some(Const::Int(match op {
                    BinOp::Add => x.wrapping_add(y),
                    BinOp::Sub => x.wrapping_sub(y),
                    BinOp::Mul => x.wrapping_mul(y),
                    BinOp::And => x & y,
                    BinOp::Or => x | y,
                    BinOp::Xor => x ^ y,
                    _ => return None,
                }))
            }
        }
        Expr::ICmp { pred, lhs, rhs } => {
            let (a, b) = (lookup(lhs)?.as_i64(), lookup(rhs)?.as_i64());
            Some(Const::Bool(match pred {
                IntPredicate::Eq => a == b,
                IntPredicate::Ne => a != b,
                IntPredicate::Slt => a < b,
                IntPredicate::Sle => a <= b,
                IntPredicate::Sgt => a > b,
                IntPredicate::Sge => a >= b,
                _ => return None,
            }))
        }
        Expr::Select {
            cond,
            if_true,
            if_false,
        } => {
            let taken = lookup(cond)?.truthy();
            lookup(if taken { if_true } else { if_false })
        }
        Expr::Cast { op, operand } => {
            let value = lookup(operand)?;
            Some(match op {
                CastOp::SIToFP | CastOp::UIToFP => Const::Float(value.as_f64()),
                CastOp::FPToSI | CastOp::FPToUI => Const::Int(value.as_f64() as i64),
                CastOp::ZExt | CastOp::SExt | CastOp::Trunc => Const::Int(value.as_i64()),
                _ => return None,
            })
        }
        _ => None,
    }
}

fn substitute_known(program: &mut Program, known: &HashMap<ValueId, Const>) {
    let replace = |operand: &mut Operand| {
        if let Operand::Value(id) = operand {
            if let Some(value) = known.get(id) {
                *operand = Operand::Const(*value);
            }
        }
    };

    for block in &mut program.blocks {
        for op in &mut block.ops {
            if let Op::Gate(gate) = op {
                for param in &mut gate.params {
                    replace(param);
                }
            }
        }

        match &mut block.term {
            Term::CondBr { cond, .. } => replace(cond),
            Term::Switch { scrutinee, .. } => replace(scrutinee),
            Term::Ret(Some(operand)) => replace(operand),
            _ => {}
        }
    }
}

fn eliminate_dead_code(program: &mut Program) -> usize {
    let mut live: HashSet<ValueId> = HashSet::new();

    for block in &program.blocks {
        for op in &block.ops {
            match op {
                Op::Gate(gate) => {
                    for param in &gate.params {
                        if let Some(id) = param.value() {
                            live.insert(id);
                        }
                    }
                }
                Op::Assign { expr, .. } => {
                    for id in expr_uses(expr) {
                        live.insert(id);
                    }
                }
                _ => {}
            }
        }

        match &block.term {
            Term::CondBr { cond, .. } => {
                if let Some(id) = cond.value() {
                    live.insert(id);
                }
            }
            Term::Switch { scrutinee, .. } => {
                if let Some(id) = scrutinee.value() {
                    live.insert(id);
                }
            }
            Term::Ret(Some(operand)) => {
                if let Some(id) = operand.value() {
                    live.insert(id);
                }
            }
            _ => {}
        }
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

fn expr_uses(expr: &Expr) -> Vec<ValueId> {
    let mut out = Vec::new();
    let mut push = |operand: &Operand| {
        if let Some(id) = operand.value() {
            out.push(id);
        }
    };

    match expr {
        Expr::Const(_) | Expr::ReadResult(_) | Expr::Load(_) => {}
        Expr::Copy(o) | Expr::Cast { operand: o, .. } => push(o),
        Expr::Binary { lhs, rhs, .. }
        | Expr::ICmp { lhs, rhs, .. }
        | Expr::FCmp { lhs, rhs, .. } => {
            push(lhs);
            push(rhs);
        }
        Expr::Select {
            cond,
            if_true,
            if_false,
        } => {
            push(cond);
            push(if_true);
            push(if_false);
        }
        Expr::Phi(incoming) => {
            for (_, operand) in incoming {
                push(operand);
            }
        }
    }

    out
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
    if reachable.iter().any(|live| !live) {
        for (index, live) in reachable.iter().enumerate() {
            if !live && !program.blocks[index].ops.is_empty() {
                program.blocks[index].ops.clear();
                program.blocks[index].term = Term::Unreachable;
                changes += 1;
            }
        }
    }

    changes
}
