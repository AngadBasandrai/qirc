use std::collections::{BTreeMap, HashMap, HashSet};

use super::matrix::{Matrix2, matrix_for};
use super::simd;
use super::state::{Rng, Sampler, State};
use super::tableau::{self, Tableau};
use crate::calibration::Calibration;
use crate::ir::*;

const STABILIZER_ABOVE: usize = 20;

pub(crate) trait Backend {
    fn gate(&mut self, gate: &Gate, params: &[f64]);
    fn pauli(&mut self, qubit: usize, x: bool, z: bool);

    fn flip(&mut self, qubit: usize) {
        self.pauli(qubit, true, false);
    }
}

impl Backend for State {
    fn gate(&mut self, gate: &Gate, params: &[f64]) {
        let controls = control_mask(gate);
        if gate.kind == GateKind::Swap {
            if let [a, b] = gate.targets[..] {
                self.swap(a.index(), b.index(), controls);
            }
            return;
        }
        let matrix = matrix_for(gate.kind, params);
        for target in &gate.targets {
            self.apply(&matrix, target.index(), controls);
        }
    }

    fn pauli(&mut self, qubit: usize, x: bool, z: bool) {
        if x {
            self.apply(&Matrix2::x(), qubit, 0);
        }
        if z {
            self.apply(&Matrix2::z(), qubit, 0);
        }
    }
}

impl Backend for Tableau {
    fn gate(&mut self, gate: &Gate, params: &[f64]) {
        self.apply(gate, params);
    }

    fn pauli(&mut self, qubit: usize, x: bool, z: bool) {
        Tableau::pauli(self, qubit, x, z);
    }
}

trait Sample: Backend {
    type Sampler;
    fn sampler(&self) -> Self::Sampler;
    fn sample(
        &self,
        sampler: &Self::Sampler,
        plan: &[(QubitId, ResultId)],
        rng: &mut Rng,
        results: &mut [bool],
    );
}

impl Sample for State {
    type Sampler = Sampler;

    fn sampler(&self) -> Sampler {
        State::sampler(self)
    }

    fn sample(
        &self,
        sampler: &Sampler,
        plan: &[(QubitId, ResultId)],
        rng: &mut Rng,
        results: &mut [bool],
    ) {
        let index = sampler.draw(rng);
        for (qubit, result) in plan {
            if let Some(slot) = results.get_mut(result.index()) {
                *slot = (index >> qubit.0) & 1 == 1;
            }
        }
    }
}

impl Sample for Tableau {
    type Sampler = ();

    fn sampler(&self) {}

    fn sample(&self, _: &(), plan: &[(QubitId, ResultId)], rng: &mut Rng, results: &mut [bool]) {
        let mut copy = self.clone();
        for (qubit, result) in plan {
            let outcome = copy.measure(qubit.index(), rng);
            if let Some(slot) = results.get_mut(result.index()) {
                *slot = outcome;
            }
        }
    }
}

pub fn stabilizer(program: &Program) -> bool {
    let qubits = program.num_qubits as usize;
    qubits > STABILIZER_ABOVE
        && qubits <= tableau::MAX_QUBITS
        && program.gates().all(|gate| {
            gate.params
                .iter()
                .map(|p| p.constant().map(Const::as_f64))
                .collect::<Option<Vec<f64>>>()
                .is_some_and(|params| tableau::supports(gate, &params))
        })
}

pub fn kernel(program: &Program) -> &'static str {
    if stabilizer(program) {
        "stabilizer tableau"
    } else {
        simd::backend()
    }
}

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
    pub returns: BTreeMap<String, u64>,
    pub messages: Vec<String>,
    pub outputs: Vec<String>,
    pub sampled: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Returned {
    Open(OutputKind, Option<usize>),
    Close,
    Bit(bool),
    Bool(bool),
    Int(i64),
    Double(f64),
}

pub fn needs_per_shot(program: &Program) -> bool {
    if !program.is_straight_line() {
        return true;
    }

    let mut measured = HashSet::new();
    let mut recorded = HashSet::new();

    for op in program.ops() {
        match op {
            Op::Reset { .. } => return true,
            Op::Assign {
                expr: Expr::ReadResult(_),
                ..
            }
            | Op::RecordOutput {
                value: Some(Operand::Value(_)),
                ..
            } => return true,
            Op::Measure { result, .. } if recorded.contains(result) => return true,
            Op::Measure { qubit, .. } => {
                measured.insert(*qubit);
            }
            Op::RecordOutput {
                result: Some(result),
                ..
            } => {
                recorded.insert(*result);
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
    let qubits = program.num_qubits as usize;
    if stabilizer(program) && needs_per_shot(program) {
        shots(
            program,
            config,
            None,
            || Tableau::new(qubits),
            Tableau::measure,
        )
        .0
    } else if stabilizer(program) {
        execute_sampled(program, config, Tableau::new(qubits)).0
    } else if needs_per_shot(program) {
        let (mut outcome, state) =
            shots(program, config, None, || State::new(qubits), State::measure);
        if config.keep_state {
            outcome.final_state = state;
        }
        outcome
    } else {
        let (mut outcome, state) = execute_sampled(program, config, State::new(qubits));
        outcome.final_state = config.keep_state.then_some(state);
        outcome
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

fn execute_sampled<S: Sample>(
    program: &Program,
    config: ExecConfig,
    mut state: S,
) -> (ExecOutcome, S) {
    let mut messages = Vec::new();
    let mut outputs = Vec::new();
    let mut values = HashMap::new();
    let mut slots = vec![Const::Int(0); program.num_slots as usize];

    for block in &program.blocks {
        for op in &block.ops {
            match op {
                Op::Gate(gate) => apply_gate(&mut state, gate, &values),
                Op::Store { slot, value, .. } => {
                    if let Some(v) = value.resolve(&values)
                        && let Some(cell) = slots.get_mut(slot.index())
                    {
                        *cell = v;
                    }
                }
                Op::Assign { dest, ty, expr, .. } => {
                    if let Some(value) = eval(*ty, expr, &values, &[], &slots, None) {
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
    let records: Vec<&Op> = program
        .ops()
        .filter(|op| matches!(op, Op::RecordOutput { .. }))
        .collect();
    let mut counts = BTreeMap::new();
    let mut returns = BTreeMap::new();

    if config.shots > 0 && !(plan.is_empty() && records.is_empty()) {
        let mut rng = Rng::new(config.seed);
        let sampler = state.sampler();
        for _ in 0..config.shots {
            let mut results = vec![false; program.num_results as usize];
            state.sample(&sampler, &plan, &mut rng, &mut results);
            if !plan.is_empty() {
                *counts.entry(format_results(&results)).or_insert(0) += 1;
            }
            if !records.is_empty() {
                let shot: Vec<Returned> = records
                    .iter()
                    .filter_map(|op| recorded(op, &values, &results))
                    .collect();
                *returns.entry(format_returned(&shot)).or_insert(0) += 1;
            }
        }
    }

    let outcome = ExecOutcome {
        final_state: None,
        aborted: false,
        counts,
        returns,
        messages,
        outputs,
        sampled: true,
    };
    (outcome, state)
}

pub fn execute_noisy(
    program: &Program,
    config: ExecConfig,
    calibration: &Calibration,
) -> ExecOutcome {
    let qubits = program.num_qubits as usize;
    if stabilizer(program) {
        return shots(
            program,
            config,
            Some(calibration),
            || Tableau::new(qubits),
            Tableau::measure,
        )
        .0;
    }
    let (mut outcome, state) = shots(
        program,
        config,
        Some(calibration),
        || State::new(qubits),
        State::measure,
    );
    if config.keep_state {
        outcome.final_state = state;
    }
    outcome
}

fn depolarize<S: Backend>(state: &mut S, gate: &Gate, calibration: &Calibration, rng: &mut Rng) {
    if rng.next_unit() >= calibration.gate_error(gate) {
        return;
    }
    let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
    let choices = (1u64 << (2 * wires.len())) - 1;
    let choice = 1 + rng.next_u64() % choices;
    for (i, &q) in wires.iter().enumerate() {
        let pauli = choice >> (2 * i) & 3;
        state.pauli(q, pauli & 1 == 1, pauli & 2 == 2);
    }
}

fn shots<S: Backend>(
    program: &Program,
    config: ExecConfig,
    noise: Option<&Calibration>,
    fresh: impl Fn() -> S,
    measure: fn(&mut S, usize, &mut Rng) -> bool,
) -> (ExecOutcome, Option<S>) {
    let shots = config.shots.max(1);
    let mut rng = Rng::new(config.seed);
    let mut errors = Rng::new(config.seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
    let mut counts = BTreeMap::new();
    let mut returns = BTreeMap::new();
    let mut messages = Vec::new();
    let mut outputs = Vec::new();
    let mut last_state = None;
    let mut aborted = false;
    let records = program
        .ops()
        .any(|op| matches!(op, Op::RecordOutput { .. }));

    for shot in 0..shots {
        let mut state = fresh();
        let run = run_with(
            program,
            &mut state,
            &mut |state, qubit| {
                let outcome = measure(state, qubit, &mut rng);
                let flipped = noise.is_some_and(|c| rng.next_unit() < c.readout(qubit));
                Some(outcome ^ flipped)
            },
            &mut |state, gate| {
                if let Some(calibration) = noise {
                    depolarize(state, gate, calibration, &mut errors);
                }
            },
        );
        if run.aborted {
            aborted = true;
            break;
        }

        if shot == 0 {
            messages = run.messages;
            outputs = run.outputs;
        }

        *counts.entry(format_results(&run.results)).or_insert(0) += 1;
        if records {
            *returns.entry(format_returned(&run.returned)).or_insert(0) += 1;
        }

        if shot + 1 == shots {
            last_state = Some(state);
        }
    }

    let outcome = ExecOutcome {
        final_state: None,
        aborted,
        counts,
        returns,
        messages,
        outputs,
        sampled: false,
    };
    (outcome, last_state)
}

pub(crate) struct ShotRun {
    pub(crate) aborted: bool,
    pub(crate) results: Vec<bool>,
    messages: Vec<String>,
    outputs: Vec<String>,
    pub(crate) returned: Vec<Returned>,
}

pub(crate) fn run_with<S: Backend>(
    program: &Program,
    state: &mut S,
    measure: &mut dyn FnMut(&mut S, usize) -> Option<bool>,
    after: &mut dyn FnMut(&mut S, &Gate),
) -> ShotRun {
    let mut results = vec![false; program.num_results as usize];
    let mut values = HashMap::new();
    let mut slots = vec![Const::Int(0); program.num_slots as usize];
    let mut messages = Vec::new();
    let mut outputs = Vec::new();
    let mut returned = Vec::new();

    let mut aborted = false;
    let mut current = program.entry;
    let mut previous = None;
    let mut steps = 0usize;

    'run: loop {
        steps += 1;
        if steps > MAX_STEPS {
            aborted = true;
            break;
        }

        let index = current.index();
        if index >= program.blocks.len() {
            break;
        }
        let block = &program.blocks[index];

        let phis: Vec<(ValueId, Const)> = block
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Assign {
                    dest,
                    ty,
                    expr: expr @ Expr::Phi(_),
                    ..
                } => Some((*dest, eval(*ty, expr, &values, &results, &slots, previous)?)),
                _ => None,
            })
            .collect();
        values.extend(phis);

        for op in &block.ops {
            match op {
                Op::Gate(gate) => {
                    apply_gate(state, gate, &values);
                    after(state, gate);
                }

                Op::Measure { qubit, result, .. } => {
                    let Some(outcome) = measure(state, qubit.index()) else {
                        aborted = true;
                        break 'run;
                    };
                    if result.index() < results.len() {
                        results[result.index()] = outcome;
                    }
                }

                Op::Reset { qubit, .. } => match measure(state, qubit.index()) {
                    Some(true) => state.flip(qubit.index()),
                    Some(false) => {}
                    None => {
                        aborted = true;
                        break 'run;
                    }
                },

                Op::Assign {
                    expr: Expr::Phi(_), ..
                } => {}

                Op::Assign { dest, ty, expr, .. } => {
                    if let Some(value) = eval(*ty, expr, &values, &results, &slots, previous) {
                        values.insert(*dest, value);
                    }
                }

                Op::Message { text, .. } => messages.push(text.clone()),

                Op::Store { slot, value, .. } => {
                    if let Some(v) = value.resolve(&values)
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
                } => {
                    outputs.push(render_output(*kind, *result, *count, label.as_deref()));
                    returned.extend(recorded(op, &values, &results));
                }
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
                let taken = cond.resolve(&values).is_some_and(|c| c.truthy());
                Some(if taken { *if_true } else { *if_false })
            }
            Term::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let key = scrutinee.resolve(&values).map_or(0, |c| c.as_i64());
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
        returned,
    }
}

fn recorded(op: &Op, values: &HashMap<ValueId, Const>, results: &[bool]) -> Option<Returned> {
    let Op::RecordOutput {
        kind,
        result,
        value,
        count,
        ..
    } = op
    else {
        return None;
    };

    let value = value.and_then(|v| v.resolve(values));
    Some(match kind {
        OutputKind::Tuple | OutputKind::Array => Returned::Open(
            *kind,
            count
                .or(value.map(|v| v.as_i64()))
                .map(|n| n.max(0) as usize),
        ),
        OutputKind::TupleEnd | OutputKind::ArrayEnd => Returned::Close,
        OutputKind::Result => {
            Returned::Bit(result.is_some_and(|r| results.get(r.index()).copied().unwrap_or(false)))
        }
        OutputKind::Bool => Returned::Bool(value?.truthy()),
        OutputKind::Int => Returned::Int(value?.as_i64()),
        OutputKind::Double => Returned::Double(value?.as_f64()),
    })
}

pub(crate) fn format_returned(shot: &[Returned]) -> String {
    let mut rest = shot;
    let mut parts = Vec::new();
    while !rest.is_empty() {
        parts.extend(take_returned(&mut rest));
    }
    if parts.is_empty() {
        return "(nothing)".into();
    }
    parts.join(", ")
}

fn take_returned(rest: &mut &[Returned]) -> Option<String> {
    let (first, tail) = rest.split_first()?;
    *rest = tail;

    Some(match *first {
        Returned::Open(kind, count) => {
            let mut items = Vec::new();
            while count.is_none_or(|n| items.len() < n) {
                match rest.first() {
                    None => break,
                    Some(Returned::Close) if count.is_none() => {
                        *rest = &rest[1..];
                        break;
                    }
                    Some(_) => items.extend(take_returned(rest)),
                }
            }
            match kind {
                OutputKind::Array => format!("[{}]", items.join(", ")),
                _ => format!("({})", items.join(", ")),
            }
        }
        Returned::Close => return None,
        Returned::Bit(bit) => u8::from(bit).to_string(),
        Returned::Bool(flag) => flag.to_string(),
        Returned::Int(number) => number.to_string(),
        Returned::Double(number) => format!("{number:?}"),
    })
}

fn apply_gate<S: Backend>(state: &mut S, gate: &Gate, values: &HashMap<ValueId, Const>) {
    let params: Vec<f64> = gate
        .params
        .iter()
        .map(|operand| operand.resolve(values).map_or(0.0, |c| c.as_f64()))
        .collect();
    state.gate(gate, &params);
}

fn eval(
    ty: Scalar,
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
            operand.resolve(values)
        }

        Expr::ReadResult(result) => Some(Const::Bool(
            results.get(result.index()).copied().unwrap_or(false),
        )),

        Expr::Load(slot) => slots.get(slot.index()).map(|c| ty.normalize(*c)),

        other => other.fold(ty, |operand| operand.resolve(values)),
    }
}

pub(crate) fn format_results(results: &[bool]) -> String {
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
        OutputKind::TupleEnd => "TUPLE_END".into(),
        OutputKind::ArrayEnd => "ARRAY_END".into(),
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
