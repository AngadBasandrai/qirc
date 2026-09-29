use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::bits::{self, Bits};
use super::matrix::{Matrix2, matrix_for};
use super::mps::{self, Mps};
use super::rank::{self, Rank};
use super::simd;
use super::state::{self, Rng, Sampler, State};
use super::tableau::{self, Tableau};
use crate::calibration::{Calibration, Clock};
use crate::ir::*;
use crate::transpile::{self, GateSet};

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

impl Backend for Bits {
    fn gate(&mut self, gate: &Gate, _: &[f64]) {
        self.apply(gate);
    }

    fn pauli(&mut self, qubit: usize, x: bool, _: bool) {
        if x {
            Bits::flip(self, qubit);
        }
    }
}

impl Backend for Mps {
    fn gate(&mut self, gate: &Gate, params: &[f64]) {
        self.apply(gate, params);
    }

    fn pauli(&mut self, qubit: usize, x: bool, z: bool) {
        if x {
            self.one(qubit, &Matrix2::x());
        }
        if z {
            self.one(qubit, &Matrix2::z());
        }
    }
}

impl Backend for Rank {
    fn gate(&mut self, gate: &Gate, params: &[f64]) {
        self.apply(gate, params);
    }

    fn pauli(&mut self, qubit: usize, x: bool, z: bool) {
        Rank::pauli(self, qubit, x, z);
    }
}

pub(crate) trait Sample: Backend {
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

impl Sample for Bits {
    type Sampler = ();

    fn sampler(&self) {}

    fn sample(&self, _: &(), plan: &[(QubitId, ResultId)], _: &mut Rng, results: &mut [bool]) {
        for (qubit, result) in plan {
            if let Some(slot) = results.get_mut(result.index()) {
                *slot = self.get(qubit.index());
            }
        }
    }
}

impl Sample for Mps {
    type Sampler = Mps;

    fn sampler(&self) -> Mps {
        self.prepared()
    }

    fn sample(
        &self,
        sampler: &Mps,
        plan: &[(QubitId, ResultId)],
        rng: &mut Rng,
        results: &mut [bool],
    ) {
        let mut measured = vec![None; self.qubits()];
        for (qubit, result) in plan {
            if result.index() < results.len() {
                measured[qubit.index()] = Some(result.index());
            }
        }
        sampler.draw(&measured, rng, results);
    }
}

impl Sample for Rank {
    type Sampler = Cell<usize>;

    fn sampler(&self) -> Cell<usize> {
        Cell::new(0)
    }

    fn sample(
        &self,
        next: &Cell<usize>,
        plan: &[(QubitId, ResultId)],
        _: &mut Rng,
        results: &mut [bool],
    ) {
        let shot = next.get();
        next.set(shot + 1);
        for (qubit, result) in plan {
            if let Some(slot) = results.get_mut(result.index()) {
                *slot = self.shot(shot, qubit.index());
            }
        }
    }
}

fn ranked(program: &Program) -> Option<Program> {
    if !wide(program) || needs_per_shot(program) {
        return None;
    }
    let mut lowered = program.clone();
    transpile::transpile(&mut lowered, &GateSet::parse("rz-sx-cx").ok()?);
    let rotations = lowered.gates().filter(|g| rank::rotation(g)).count();
    let supported = lowered.gates().all(rank::supports);
    (supported && rotations <= rank::MAX_ROTATIONS).then_some(lowered)
}

fn classical(program: &Program) -> bool {
    let qubits = program.num_qubits as usize;
    qubits > STABILIZER_ABOVE && qubits <= bits::MAX_QUBITS && program.gates().all(bits::supports)
}

fn wide(program: &Program) -> bool {
    program.num_qubits as usize > state::MAX_QUBITS && !classical(program) && !stabilizer(program)
}

fn local(program: &Program) -> Option<Program> {
    let mut lowered = program.clone();
    transpile::transpile(&mut lowered, &GateSet::native().local());
    let supported = lowered.gates().all(mps::supports);
    supported.then_some(lowered)
}

pub fn scalable(program: &Program) -> bool {
    classical(program) || stabilizer(program) || wide(program) && local(program).is_some()
}

pub fn stabilizer(program: &Program) -> bool {
    let qubits = program.num_qubits as usize;
    qubits > STABILIZER_ABOVE
        && qubits <= tableau::MAX_QUBITS
        && program
            .gates()
            .all(|gate| tableau::clifford(gate).is_some())
}

pub fn kernel(program: &Program) -> &'static str {
    if classical(program) {
        "classical bits"
    } else if stabilizer(program) {
        "stabilizer tableau"
    } else if ranked(program).is_some() {
        "stabilizer rank"
    } else if wide(program) {
        "matrix product state"
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
    pub discarded: f64,
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

pub fn execute_bonded(program: &Program, config: ExecConfig, bond: usize) -> Option<ExecOutcome> {
    let lowered = local(program)?;
    let fresh = || Mps::new(lowered.num_qubits as usize, bond);
    let (mut outcome, state) = if needs_per_shot(&lowered) {
        shots(&lowered, config, None, fresh, Mps::measure, &mut |_| {})
    } else {
        let (outcome, state) = execute_sampled(&lowered, config, fresh());
        (outcome, Some(state))
    };
    outcome.discarded = state.map_or(0.0, |s| s.discarded);
    Some(outcome)
}

pub fn execute(program: &Program, config: ExecConfig) -> ExecOutcome {
    let qubits = program.num_qubits as usize;
    if let Some(lowered) = ranked(program) {
        let rank = Rank::new(qubits, config.shots as usize, config.seed);
        return execute_sampled(&lowered, config, rank).0;
    }
    if wide(program)
        && let Some(outcome) = execute_bonded(program, config, mps::DEFAULT_BOND)
    {
        return outcome;
    }
    if classical(program) && needs_per_shot(program) {
        let once = ExecConfig { shots: 1, ..config };
        let mut outcome = shots(
            program,
            once,
            None,
            || Bits::new(qubits),
            Bits::measure,
            &mut |_| {},
        )
        .0;
        for count in outcome
            .counts
            .values_mut()
            .chain(outcome.returns.values_mut())
        {
            *count *= config.shots.max(1);
        }
        outcome
    } else if classical(program) {
        execute_sampled(program, config, Bits::new(qubits)).0
    } else if stabilizer(program) && needs_per_shot(program) {
        shots(
            program,
            config,
            None,
            || Tableau::new(qubits),
            Tableau::measure,
            &mut |_| {},
        )
        .0
    } else if stabilizer(program) {
        execute_sampled(program, config, Tableau::new(qubits)).0
    } else if needs_per_shot(program) {
        let (mut outcome, state) = shots(
            program,
            config,
            None,
            || State::new(qubits),
            State::measure,
            &mut |_| {},
        );
        if config.keep_state {
            outcome.final_state = state;
        }
        outcome
    } else {
        execute_vector(program, config)
    }
}

pub fn execute_vector(program: &Program, config: ExecConfig) -> ExecOutcome {
    let (mut outcome, state) =
        execute_sampled(program, config, State::new(program.num_qubits as usize));
    outcome.final_state = config.keep_state.then_some(state);
    outcome
}

pub(crate) fn finish<S: Sample>(program: &Program, state: S) -> S {
    let config = ExecConfig {
        shots: 0,
        seed: 1,
        keep_state: false,
    };
    execute_sampled(program, config, state).1
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
        discarded: 0.0,
    };
    (outcome, state)
}

pub fn execute_noisy(
    program: &Program,
    config: ExecConfig,
    calibration: &Calibration,
) -> ExecOutcome {
    let qubits = program.num_qubits as usize;
    if classical(program) {
        return shots(
            program,
            config,
            Some(calibration),
            || Bits::new(qubits),
            Bits::measure,
            &mut |_| {},
        )
        .0;
    }
    if stabilizer(program) {
        return shots(
            program,
            config,
            Some(calibration),
            || Tableau::new(qubits),
            Tableau::measure,
            &mut |_| {},
        )
        .0;
    }
    if wide(program)
        && let Some(lowered) = local(program)
    {
        let (mut outcome, state) = shots(
            &lowered,
            config,
            Some(calibration),
            || Mps::new(qubits, mps::DEFAULT_BOND),
            Mps::measure,
            &mut |_| {},
        );
        outcome.discarded = state.map_or(0.0, |s| s.discarded);
        return outcome;
    }
    let (mut outcome, state) = shots(
        program,
        config,
        Some(calibration),
        || State::new(qubits),
        State::measure,
        &mut |_| {},
    );
    if config.keep_state {
        outcome.final_state = state;
    }
    outcome
}

pub fn average_noisy(
    program: &Program,
    config: ExecConfig,
    calibration: &Calibration,
    value: impl Fn(&State) -> f64,
) -> f64 {
    let qubits = program.num_qubits as usize;
    let mut total = 0.0;
    shots(
        program,
        config,
        Some(calibration),
        || State::new(qubits),
        State::measure,
        &mut |state| total += value(state),
    );
    total / config.shots.max(1) as f64
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
    each: &mut dyn FnMut(&S),
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
        let mut clock = Clock::default();
        let run = run_with(
            program,
            &mut state,
            &mut |state, qubit| {
                let outcome = measure(state, qubit, &mut rng);
                let flipped = noise.is_some_and(|c| rng.next_unit() < c.readout(qubit));
                Some(outcome ^ flipped)
            },
            &mut |state, op, done| {
                let Some(calibration) = noise else {
                    return;
                };
                if !done {
                    for (q, [x, y, z]) in clock.begin(calibration, op) {
                        let draw = errors.next_unit();
                        if draw < x + y + z {
                            state.pauli(q, draw < x + y, draw >= x);
                        }
                    }
                    return;
                }
                if let Op::Gate(gate) = op {
                    depolarize(state, gate, calibration, &mut errors);
                }
                clock.end(calibration, op);
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

        each(&state);
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
        discarded: 0.0,
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
    hook: &mut dyn FnMut(&mut S, &Op, bool),
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
                    hook(state, op, false);
                    apply_gate(state, gate, &values);
                    hook(state, op, true);
                }

                Op::Measure { qubit, result, .. } => {
                    hook(state, op, false);
                    let Some(outcome) = measure(state, qubit.index()) else {
                        aborted = true;
                        break 'run;
                    };
                    hook(state, op, true);
                    if result.index() < results.len() {
                        results[result.index()] = outcome;
                    }
                }

                Op::Reset { qubit, .. } => {
                    hook(state, op, false);
                    match measure(state, qubit.index()) {
                        Some(true) => state.flip(qubit.index()),
                        Some(false) => {}
                        None => {
                            aborted = true;
                            break 'run;
                        }
                    }
                    hook(state, op, true);
                }

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

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;

    use super::*;
    use crate::diag::Span;
    use crate::qasm;

    fn random_circuit(qubits: usize, gates: usize, seed: u64) -> Program {
        let mut rng = Rng::new(seed);
        let mut pick = |n: usize| rng.next_u64() as usize % n;
        let mut source = format!("OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[{qubits}];\n");
        let one = [
            "h",
            "t",
            "sx",
            "rx(0.7)",
            "ry(1.3)",
            "rz(-0.4)",
            "u3(0.3, 1.1, -0.6)",
        ];
        for _ in 0..gates {
            let a = pick(qubits);
            let b = (a + 1 + pick(qubits - 1)) % qubits;
            match pick(4) {
                0 => source += &format!("cx q[{a}], q[{b}];\n"),
                1 => source += &format!("crz(0.9) q[{a}], q[{b}];\n"),
                _ => source += &format!("{} q[{a}];\n", one[pick(one.len())]),
            }
        }
        let (program, diagnostics) = qasm::lower(&source);
        assert!(diagnostics.is_empty());
        program
    }

    #[test]
    fn matrix_product() {
        for seed in 0..6 {
            let program = random_circuit(10, 80, seed);
            let exact = execute_vector(&program, ExecConfig::default())
                .final_state
                .unwrap();
            let lowered = local(&program).unwrap();
            let chain = finish(&lowered, Mps::new(10, 1 << 10));
            assert_eq!(chain.discarded, 0.0);
            for basis in 0..exact.len() {
                let difference = (exact.amplitude(basis) - chain.amplitude(basis)).norm();
                assert!(difference < 1e-9, "seed {seed} basis {basis}");
            }
        }
    }

    #[test]
    fn matrix_product_samples() {
        let mut program = random_circuit(6, 40, 11);
        let measures: Vec<Op> = (0..6)
            .map(|q| Op::Measure {
                qubit: QubitId(q),
                result: ResultId(q),
                span: Span::DUMMY,
            })
            .collect();
        program.blocks[0].ops.extend(measures);
        program.num_results = 6;
        let config = ExecConfig {
            shots: 40_000,
            seed: 5,
            keep_state: false,
        };
        let vector = execute_vector(&program, config).counts;
        let chain = execute_bonded(&program, config, 64).unwrap().counts;
        let keys: HashSet<&String> = vector.keys().chain(chain.keys()).collect();
        let distance: u64 = keys
            .into_iter()
            .map(|key| {
                vector
                    .get(key)
                    .unwrap_or(&0)
                    .abs_diff(*chain.get(key).unwrap_or(&0))
            })
            .sum();
        assert!(distance < 2400, "{distance}");
        let truncated = execute_bonded(&random_circuit(8, 120, 3), config, 1).unwrap();
        assert!(truncated.discarded > 0.01);
    }

    #[test]
    fn stabilizer_rank_samples() {
        for seed in 0..4 {
            let source = random_circuit(7, 60, 20 + seed);
            let mut lowered = source.clone();
            transpile::transpile(&mut lowered, &GateSet::parse("rz-sx-cx").unwrap());
            let mut rotations = 0;
            for op in &mut lowered.blocks[0].ops {
                if let Op::Gate(gate) = op
                    && rank::rotation(gate)
                {
                    rotations += 1;
                    if rotations > 6 {
                        gate.params = vec![Operand::Const(Const::Float(FRAC_PI_2))];
                    }
                }
            }
            let measures: Vec<Op> = (0..7)
                .map(|q| Op::Measure {
                    qubit: QubitId(q),
                    result: ResultId(q),
                    span: Span::DUMMY,
                })
                .collect();
            lowered.blocks[0].ops.extend(measures);
            lowered.num_results = 7;
            let config = ExecConfig {
                shots: 20_000,
                seed,
                keep_state: false,
            };
            let vector = execute_vector(&lowered, config).counts;
            let sampled = execute_sampled(&lowered, config, Rank::new(7, 20_000, seed))
                .0
                .counts;
            let keys: HashSet<&String> = vector.keys().chain(sampled.keys()).collect();
            let distance: u64 = keys
                .into_iter()
                .map(|key| {
                    vector
                        .get(key)
                        .unwrap_or(&0)
                        .abs_diff(*sampled.get(key).unwrap_or(&0))
                })
                .sum();
            assert!(distance < 2400, "seed {seed}: {distance}");
        }
    }

    #[test]
    fn noisy_bits() {
        let (program, diagnostics) = qasm::lower(
            "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[5];
creg c[5];
x q[0];
x q[1];
ccx q[0], q[1], q[2];
t q[2];
swap q[2], q[3];
cz q[3], q[4];
cx q[3], q[4];
y q[0];
measure q -> c;
reset q[1];
cx q[4], q[1];
",
        );
        assert!(diagnostics.is_empty());
        let calibration = Calibration::parse(
            "cx 0 1 0.03\ncx 1 2 0.02\ncx 2 3 0.04\ncx 3 4 0.03\nsingle 2 0.01\nreadout 3 0.05\n\
             time cx 2 3 300\ntime cx 3 4 300\ntime readout 3 1000\nt1 4 20\nt2 4 15\n",
        )
        .unwrap();
        let config = ExecConfig {
            shots: 40_000,
            seed: 9,
            keep_state: false,
        };
        let run = |bits: bool| {
            let noise = Some(&calibration);
            if bits {
                shots(
                    &program,
                    config,
                    noise,
                    || Bits::new(5),
                    Bits::measure,
                    &mut |_| {},
                )
                .0
            } else {
                shots(
                    &program,
                    config,
                    noise,
                    || State::new(5),
                    State::measure,
                    &mut |_| {},
                )
                .0
            }
        };
        let (bits, vector) = (run(true).counts, run(false).counts);
        let keys: HashSet<&String> = bits.keys().chain(vector.keys()).collect();
        let distance: u64 = keys
            .into_iter()
            .map(|key| {
                bits.get(key)
                    .unwrap_or(&0)
                    .abs_diff(*vector.get(key).unwrap_or(&0))
            })
            .sum();
        assert!(bits.len() > 4);
        assert!(distance < 1600, "{distance}");
    }
}
