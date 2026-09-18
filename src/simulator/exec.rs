use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

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
            seed: 1,
            keep_state: true,
        }
    }
}

pub struct ExecOutcome {
    pub final_state: Option<State>,
    pub aborted: bool,
    pub counts: BTreeMap<String, u64>,
    pub shots: u64,
    pub messages: Vec<String>,
    pub outputs: Vec<String>,
    pub sampled: bool,
}

pub fn needs_per_shot(program: &Program) -> bool {
    if !program.is_straight_line() {
        return true;
    }

    let mut measured: HashSet<QubitId> = HashSet::new();

    for op in program.ops() {
        match op {
            Op::Reset { .. } => return true,
            Op::Assign {
                expr: Expr::ReadResult(_),
                ..
            } => return true,
            Op::Measure { qubit, .. } => {
                measured.insert(*qubit);
            }
            Op::Gate(gate) if gate.wires().any(|wire| measured.contains(&wire)) => {
                return true;
            }
            _ => {}
        }
    }

    false
}

pub fn execute(program: &Program, config: ExecConfig) -> ExecOutcome {
    if needs_per_shot(program) {
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
                    if let Some(v) = resolve(value, &values)
                        && let Some(cell) = slots.get_mut(slot.index())
                    {
                        *cell = v;
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
        let sampler = state.sampler();
        for _ in 0..config.shots {
            let index = sampler.draw(&mut rng);
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
        aborted: false,
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
    let mut aborted = false;

    for shot in 0..shots {
        let mut state = State::new(program.num_qubits as usize);
        let run = run_once(program, &mut state, &mut rng);
        if run.aborted {
            aborted = true;
            break;
        }

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
        aborted,
        counts,
        shots,
        messages,
        outputs,
        sampled: false,
    }
}

struct ShotRun {
    aborted: bool,
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

    let mut aborted = false;
    let mut current = program.entry;
    let mut previous: Option<BlockId> = None;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > MAX_STEPS {
            aborted = true;
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
                    if let Some(v) = resolve(value, &values)
                        && let Some(cell) = slots.get_mut(slot.index())
                    {
                        *cell = v;
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
        aborted,
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

        other => other.fold(|operand| resolve(operand, values)),
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

    if let Some(label) = label
        && !label.is_empty()
    {
        text.push_str(&format!(" {label:?}"));
    }

    text
}
