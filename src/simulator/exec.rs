use std::collections::BTreeMap;
use std::collections::HashMap;

use super::matrix::{Matrix2, matrix_for};
use super::state::{Rng, State};
use crate::ir::*;

const MAX_STEPS: usize = 1_000_000;

#[derive(Clone, Copy, Debug)]
pub struct ExecConfig {
    pub shots: u64,
    pub seed: u64,
    pub keep_state: bool,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            shots: 0,
            seed: 0x5EED,
            keep_state: true,
        }
    }
}

pub struct ExecOutcome {
    pub final_state: Option<State>,
    pub counts: BTreeMap<String, u64>,
    pub shots: u64,
    pub messages: Vec<String>,
    pub outputs: Vec<String>,
    pub sampled: bool,
}

pub fn needs_per_shot_simulation(program: &Program) -> bool {
    if !program.is_straight_line() {
        return true;
    }

    program.ops().any(|op| match op {
        Op::Reset { .. } => true,
        Op::Assign {
            expr: Expr::ReadResult(_),
            ..
        } => true,
        _ => false,
    })
}

pub fn execute(program: &Program, config: ExecConfig) -> ExecOutcome {
    if needs_per_shot_simulation(program) {
        execute_per_shot(program, config)
    } else {
        execute_sampled(program, config)
    }
}

fn control_mask(gate: &Gate) -> u64 {
    gate.controls
        .iter()
        .fold(0u64, |mask, q| mask | (1u64 << q.0))
}

fn measurement_plan(program: &Program) -> Vec<(QubitId, ResultId)> {
    program
        .ops()
        .filter_map(|op| match op {
            Op::Measure { qubit, result, .. } => Some((*qubit, *result)),
            _ => None,
        })
        .collect()
}

fn execute_sampled(program: &Program, config: ExecConfig) -> ExecOutcome {
    let mut state = State::new(program.num_qubits as usize);
    let mut messages = Vec::new();
    let mut outputs = Vec::new();
    let mut values: HashMap<ValueId, Const> = HashMap::new();
    let mut slots: Vec<Const> = vec![Const::Int(0); program.num_slots as usize];

    for block in &program.blocks {
        for op in &block.ops {
            match op {
                Op::Gate(gate) => apply_gate(&mut state, gate, &values),
                Op::Store { slot, value, .. } => {
                    if let Some(v) = resolve(value, &values) {
                        if let Some(cell) = slots.get_mut(slot.index()) {
                            *cell = v;
                        }
                    }
                }
                Op::Assign { dest, expr, .. } => {
                    if let Some(value) = eval(expr, &values, &[], &slots, None) {
                        values.insert(*dest, value);
                    }
                }
                Op::Message { text, .. } => messages.push(text.clone()),
                Op::RecordOutput {
                    kind,
                    result,
                    count,
                    label,
                    ..
                } => outputs.push(render_output(*kind, *result, *count, label.as_deref())),
                Op::Measure { .. } | Op::Reset { .. } => {}
            }
        }
    }

    let plan = measurement_plan(program);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();

    if config.shots > 0 && !plan.is_empty() {
        let mut rng = Rng::new(config.seed);
        for _ in 0..config.shots {
            let index = state.sample_index(&mut rng);
            let mut results = vec![false; program.num_results as usize];
            for (qubit, result) in &plan {
                if (result.index()) < results.len() {
                    results[result.index()] = (index >> qubit.0) & 1 == 1;
                }
            }
            *counts.entry(format_results(&results)).or_insert(0) += 1;
        }
    }

    ExecOutcome {
        final_state: config.keep_state.then_some(state),
        counts,
        shots: config.shots,
        messages,
        outputs,
        sampled: true,
    }
}

fn execute_per_shot(program: &Program, config: ExecConfig) -> ExecOutcome {
    let shots = config.shots.max(1);
    let mut rng = Rng::new(config.seed);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut messages = Vec::new();
    let mut outputs = Vec::new();
    let mut last_state = None;

    for shot in 0..shots {
        let mut state = State::new(program.num_qubits as usize);
        let run = run_once(program, &mut state, &mut rng);

        if shot == 0 {
            messages = run.messages;
            outputs = run.outputs;
        }

        *counts.entry(format_results(&run.results)).or_insert(0) += 1;

        if shot + 1 == shots && config.keep_state {
            last_state = Some(state);
        }
    }

    ExecOutcome {
        final_state: last_state,
        counts,
        shots,
        messages,
        outputs,
        sampled: false,
    }
}

struct ShotRun {
    results: Vec<bool>,
    messages: Vec<String>,
    outputs: Vec<String>,
}

fn run_once(program: &Program, state: &mut State, rng: &mut Rng) -> ShotRun {
    let mut results = vec![false; program.num_results as usize];
    let mut values: HashMap<ValueId, Const> = HashMap::new();
    let mut slots: Vec<Const> = vec![Const::Int(0); program.num_slots as usize];
    let mut messages = Vec::new();
    let mut outputs = Vec::new();

    let mut current = program.entry;
    let mut previous: Option<BlockId> = None;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > MAX_STEPS {
            messages.push("execution aborted: step limit exceeded".into());
            break;
        }

        let index = current.0 as usize;
        if index >= program.blocks.len() {
            break;
        }
        let block = &program.blocks[index];

        for op in &block.ops {
            match op {
                Op::Gate(gate) => apply_gate(state, gate, &values),

                Op::Measure {
                    qubit,
                    result,
                    dest,
                    ..
                } => {
                    let outcome = state.measure(qubit.index(), rng);
                    if result.index() < results.len() {
                        results[result.index()] = outcome;
                    }
                    if let Some(dest) = dest {
                        values.insert(*dest, Const::Bool(outcome));
                    }
                }

                Op::Reset { qubit, .. } => state.reset(qubit.index(), rng),

                Op::Assign { dest, expr, .. } => {
                    if let Some(value) = eval(expr, &values, &results, &slots, previous) {
                        values.insert(*dest, value);
                    }
                }

                Op::Message { text, .. } => messages.push(text.clone()),

                Op::Store { slot, value, .. } => {
                    if let Some(v) = resolve(value, &values) {
                        if let Some(cell) = slots.get_mut(slot.index()) {
                            *cell = v;
                        }
                    }
                }

                Op::RecordOutput {
                    kind,
                    result,
                    count,
                    label,
                    ..
                } => outputs.push(render_output(*kind, *result, *count, label.as_deref())),
            }
        }

        let next = match &block.term {
            Term::Ret(_) | Term::Unreachable => None,
            Term::Br(target) => Some(*target),
            Term::CondBr {
                cond,
                if_true,
                if_false,
            } => {
                let taken = resolve(cond, &values).map(|c| c.truthy()).unwrap_or(false);
                Some(if taken { *if_true } else { *if_false })
            }
            Term::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let key = resolve(scrutinee, &values).map(|c| c.as_i64()).unwrap_or(0);
                Some(
                    cases
                        .iter()
                        .find(|(value, _)| *value == key)
                        .map(|(_, block)| *block)
                        .unwrap_or(*default),
                )
            }
        };

        match next {
            Some(target) => {
                previous = Some(current);
                current = target;
            }
            None => break,
        }
    }

    ShotRun {
        results,
        messages,
        outputs,
    }
}

fn apply_gate(state: &mut State, gate: &Gate, values: &HashMap<ValueId, Const>) {
    let controls = control_mask(gate);

    if gate.kind == GateKind::Swap {
        if gate.targets.len() == 2 {
            state.swap(gate.targets[0].index(), gate.targets[1].index(), controls);
        }
        return;
    }

    let params: Vec<f64> = gate
        .params
        .iter()
        .map(|operand| resolve(operand, values).map(|c| c.as_f64()).unwrap_or(0.0))
        .collect();

    let matrix: Matrix2 = matrix_for(gate.kind, &params);

    for target in &gate.targets {
        state.apply(&matrix, target.index(), controls);
    }
}

fn resolve(operand: &Operand, values: &HashMap<ValueId, Const>) -> Option<Const> {
    match operand {
        Operand::Const(c) => Some(*c),
        Operand::Value(id) => values.get(id).copied(),
    }
}

fn eval(
    expr: &Expr,
    values: &HashMap<ValueId, Const>,
    results: &[bool],
    slots: &[Const],
    previous: Option<BlockId>,
) -> Option<Const> {
    match expr {
        Expr::Const(c) => Some(*c),
        Expr::Copy(operand) => resolve(operand, values),

        Expr::Binary { op, lhs, rhs } => {
            let a = resolve(lhs, values)?;
            let b = resolve(rhs, values)?;
            Some(eval_binary(*op, a, b))
        }

        Expr::ICmp { pred, lhs, rhs } => {
            let a = resolve(lhs, values)?.as_i64();
            let b = resolve(rhs, values)?.as_i64();
            Some(Const::Bool(match pred {
                IntPredicate::Eq => a == b,
                IntPredicate::Ne => a != b,
                IntPredicate::Sgt => a > b,
                IntPredicate::Sge => a >= b,
                IntPredicate::Slt => a < b,
                IntPredicate::Sle => a <= b,
                IntPredicate::Ugt => (a as u64) > (b as u64),
                IntPredicate::Uge => (a as u64) >= (b as u64),
                IntPredicate::Ult => (a as u64) < (b as u64),
                IntPredicate::Ule => (a as u64) <= (b as u64),
            }))
        }

        Expr::FCmp { pred, lhs, rhs } => {
            let a = resolve(lhs, values)?.as_f64();
            let b = resolve(rhs, values)?.as_f64();
            let ordered = !a.is_nan() && !b.is_nan();
            Some(Const::Bool(match pred {
                FloatPredicate::False => false,
                FloatPredicate::True => true,
                FloatPredicate::Oeq => ordered && a == b,
                FloatPredicate::Ogt => ordered && a > b,
                FloatPredicate::Oge => ordered && a >= b,
                FloatPredicate::Olt => ordered && a < b,
                FloatPredicate::Ole => ordered && a <= b,
                FloatPredicate::One => ordered && a != b,
                FloatPredicate::Ord => ordered,
                FloatPredicate::Uno => !ordered,
                FloatPredicate::Ueq => !ordered || a == b,
                FloatPredicate::Ugt => !ordered || a > b,
                FloatPredicate::Uge => !ordered || a >= b,
                FloatPredicate::Ult => !ordered || a < b,
                FloatPredicate::Ule => !ordered || a <= b,
                FloatPredicate::Une => !ordered || a != b,
            }))
        }

        Expr::Select {
            cond,
            if_true,
            if_false,
        } => {
            let taken = resolve(cond, values)?.truthy();
            resolve(if taken { if_true } else { if_false }, values)
        }

        Expr::Cast { op, operand } => {
            let value = resolve(operand, values)?;
            Some(match op {
                CastOp::SIToFP | CastOp::UIToFP => Const::Float(value.as_f64()),
                CastOp::FPToSI | CastOp::FPToUI => Const::Int(value.as_f64() as i64),
                CastOp::ZExt | CastOp::SExt | CastOp::Trunc => Const::Int(value.as_i64()),
                CastOp::FPTrunc | CastOp::FPExt => Const::Float(value.as_f64()),
                _ => value,
            })
        }

        Expr::Phi(incoming) => {
            let from = previous?;
            let operand = incoming
                .iter()
                .find(|(block, _)| *block == from)
                .map(|(_, operand)| operand)?;
            resolve(operand, values)
        }

        Expr::ReadResult(result) => Some(Const::Bool(
            results.get(result.index()).copied().unwrap_or(false),
        )),

        Expr::Load(slot) => slots.get(slot.index()).copied(),
    }
}

fn eval_binary(op: BinOp, a: Const, b: Const) -> Const {
    if op.is_float() {
        let (x, y) = (a.as_f64(), b.as_f64());
        return Const::Float(match op {
            BinOp::FAdd => x + y,
            BinOp::FSub => x - y,
            BinOp::FMul => x * y,
            BinOp::FDiv => x / y,
            BinOp::FRem => x % y,
            _ => 0.0,
        });
    }

    let (x, y) = (a.as_i64(), b.as_i64());

    let value = match op {
        BinOp::Add => x.wrapping_add(y),
        BinOp::Sub => x.wrapping_sub(y),
        BinOp::Mul => x.wrapping_mul(y),
        BinOp::SDiv => {
            if y == 0 {
                0
            } else {
                x.wrapping_div(y)
            }
        }
        BinOp::UDiv => {
            if y == 0 {
                0
            } else {
                ((x as u64) / (y as u64)) as i64
            }
        }
        BinOp::SRem => {
            if y == 0 {
                0
            } else {
                x.wrapping_rem(y)
            }
        }
        BinOp::URem => {
            if y == 0 {
                0
            } else {
                ((x as u64) % (y as u64)) as i64
            }
        }
        BinOp::Shl => x.wrapping_shl(y as u32),
        BinOp::LShr => ((x as u64).wrapping_shr(y as u32)) as i64,
        BinOp::AShr => x.wrapping_shr(y as u32),
        BinOp::And => x & y,
        BinOp::Or => x | y,
        BinOp::Xor => x ^ y,
        _ => 0,
    };

    match (a, b) {
        (Const::Bool(_), Const::Bool(_)) if matches!(op, BinOp::And | BinOp::Or | BinOp::Xor) => {
            Const::Bool(value != 0)
        }
        _ => Const::Int(value),
    }
}

fn format_results(results: &[bool]) -> String {
    if results.is_empty() {
        return "(no measurements)".into();
    }
    results.iter().map(|b| if *b { '1' } else { '0' }).collect()
}

fn render_output(
    kind: OutputKind,
    result: Option<ResultId>,
    count: Option<i64>,
    label: Option<&str>,
) -> String {
    let mut text = match kind {
        OutputKind::Result => match result {
            Some(r) => format!("RESULT r{}", r.0),
            None => "RESULT".into(),
        },
        OutputKind::Tuple => match count {
            Some(n) => format!("TUPLE {n}"),
            None => "TUPLE".into(),
        },
        OutputKind::Array => match count {
            Some(n) => format!("ARRAY {n}"),
            None => "ARRAY".into(),
        },
        OutputKind::Bool => "BOOL".into(),
        OutputKind::Int => "INT".into(),
        OutputKind::Double => "DOUBLE".into(),
    };

    if let Some(label) = label {
        if !label.is_empty() {
            text.push_str(&format!(" {label:?}"));
        }
    }

    text
}
