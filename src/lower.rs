use std::collections::HashMap;
use std::mem;

use crate::ast;
use crate::diag::{Diagnostic, Span};
use crate::ir::*;
use crate::qis::{self, Functor, Intrinsic};

pub struct Lowered {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lower(module: &ast::Module) -> Lowered {
    let expanded = crate::inline::inline_module(module);
    let module = &expanded.module;

    let mut flattener = Lowerer::new(module, Mode::Flatten);
    if let Some(program) = flattener.run_flat() {
        let mut diagnostics = expanded.diagnostics;
        diagnostics.extend(flattener.diagnostics);
        return Lowered {
            program,
            diagnostics,
        };
    }

    let mut lowerer = Lowerer::new(module, Mode::Cfg);
    let program = lowerer.run();

    let mut diagnostics = expanded.diagnostics;
    diagnostics.extend(lowerer.diagnostics);

    Lowered {
        program,
        diagnostics,
    }
}

pub const MAX_WIRES: u32 = 1 << 16;

const MAX_INLINE_DEPTH: usize = 32;
const MAX_FLATTEN_STEPS: usize = 200_000;
const MAX_FLATTEN_OPS: usize = 2_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Cfg,
    Flatten,
}

#[derive(Clone, Debug)]
enum Binding {
    Value(Operand),
    Qubit(QubitId),
    QubitArray { base: QubitId, len: u64 },
    Result(ResultId),
    ResultConst(bool),
    Bytes(Vec<u8>),
    Slot(SlotId),
    Cell(usize),
    GlobalElement { name: String, index: u64 },
    Id(u32),
}

#[derive(Clone, Copy)]
struct GateShape {
    kind: GateKind,
    controls: usize,
    targets: usize,
    params: usize,
}

struct Lowerer<'a> {
    module: &'a ast::Module,
    diagnostics: Vec<Diagnostic>,
    env: HashMap<String, Binding>,
    block_ids: HashMap<String, BlockId>,
    next_value: u32,
    next_qubit: u32,
    next_result: u32,
    max_qubit: u32,
    max_result: u32,
    inline_depth: usize,
    too_many_wires: bool,
    forward: HashMap<String, (ValueId, Span)>,
    ops: Vec<Op>,
    next_slot: u32,
    mode: Mode,
    cells: Vec<Option<Const>>,
    previous_label: Option<String>,
    steps: usize,
    bailed: bool,
}

impl<'a> Lowerer<'a> {
    fn new(module: &'a ast::Module, mode: Mode) -> Self {
        Self {
            module,
            diagnostics: Vec::new(),
            env: HashMap::new(),
            block_ids: HashMap::new(),
            next_value: 0,
            next_qubit: 0,
            next_result: 0,
            max_qubit: 0,
            max_result: 0,
            inline_depth: 0,
            too_many_wires: false,
            forward: HashMap::new(),
            ops: Vec::new(),
            next_slot: 0,
            mode,
            cells: Vec::new(),
            previous_label: None,
            steps: 0,
            bailed: false,
        }
    }

    fn flattening(&self) -> bool {
        self.mode == Mode::Flatten
    }

    fn error(&mut self, message: impl Into<String>, span: Span, label: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(message)
                .with_code("QIR0200")
                .primary(span, label),
        );
    }

    fn warn(&mut self, message: impl Into<String>, span: Span, label: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::warning(message).primary(span, label));
    }

    fn fresh_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn alloc_qubits(&mut self, count: u64) -> QubitId {
        let base = QubitId(self.next_qubit);
        let end = u64::from(self.next_qubit) + count;
        if end > u64::from(MAX_WIRES) {
            self.too_many_wires = true;
            return base;
        }
        self.next_qubit = end as u32;
        self.max_qubit = self.max_qubit.max(self.next_qubit);
        base
    }

    fn note_qubit(&mut self, qubit: QubitId) {
        self.max_qubit = self.max_qubit.max(qubit.0 + 1);
    }

    fn check_wire_limit(&mut self, program: &mut Program) {
        if self.too_many_wires || self.max_qubit > MAX_WIRES || self.max_result > MAX_WIRES {
            self.diagnostics.push(
                Diagnostic::error(format!(
                    "the program uses more than {MAX_WIRES} qubits or results"
                ))
                .with_code("QIR0202"),
            );
            program.num_qubits = 0;
            program.num_results = 0;
        }
    }

    fn note_result(&mut self, result: ResultId) {
        self.max_result = self.max_result.max(result.0 + 1);
    }

    fn entry_setup(&mut self) -> Option<(&'a ast::Function, Profile)> {
        let entry = self.module.entry_point()?;
        let attrs = self.module.attributes_of(&entry.sig);

        let profile = attrs
            .iter()
            .find(|a| a.key() == "qir_profiles")
            .and_then(|a| a.value())
            .map(Profile::from_attribute)
            .unwrap_or(Profile::Unrestricted);

        let declared_qubits =
            attribute_count(&attrs, &["required_num_qubits", "num_required_qubits"]);
        let declared_results =
            attribute_count(&attrs, &["required_num_results", "num_required_results"]);

        if declared_qubits > MAX_WIRES || declared_results > MAX_WIRES {
            self.too_many_wires = true;
            return Some((entry, profile));
        }

        self.next_qubit = declared_qubits;
        self.next_result = declared_results;
        self.max_qubit = declared_qubits;
        self.max_result = declared_results;

        Some((entry, profile))
    }

    fn bind_params(&mut self, entry: &ast::Function) {
        for param in &entry.sig.params {
            let Some(name) = &param.name else { continue };
            match param.ty.pointee_name() {
                Some("Qubit") => {
                    let qubit = self.alloc_qubits(1);
                    self.env.insert(name.clone(), Binding::Qubit(qubit));
                }
                Some("Result") => {
                    let result = ResultId(self.next_result);
                    self.next_result += 1;
                    self.max_result = self.max_result.max(self.next_result);
                    self.env.insert(name.clone(), Binding::Result(result));
                }
                _ => {}
            }
        }
    }

    fn run_flat(&mut self) -> Option<Program> {
        let (entry, profile) = self.entry_setup()?;

        self.bind_params(entry);
        self.execute_function(entry);

        if self.bailed {
            return None;
        }

        let mut program = Program::new(entry.sig.name.clone(), profile);
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: mem::take(&mut self.ops),
            term: Term::Ret(None),
            span: entry.span,
        });
        program.entry = BlockId(0);
        program.num_qubits = self.max_qubit;
        program.num_results = self.max_result;
        program.next_value = self.next_value;
        program.num_slots = 0;
        self.check_wire_limit(&mut program);

        Some(program)
    }

    fn execute_function(&mut self, function: &'a ast::Function) -> Option<Binding> {
        let mut current = 0usize;
        let mut previous: Option<String> = None;

        loop {
            self.steps += 1;
            if self.steps > MAX_FLATTEN_STEPS || self.ops.len() > MAX_FLATTEN_OPS {
                self.bailed = true;
                return None;
            }
            if self.bailed {
                return None;
            }

            let Some(block) = function.blocks.get(current) else {
                self.bailed = true;
                return None;
            };
            self.previous_label = previous.clone();

            for inst in &block.instructions {
                self.lower_instruction(inst);
                if self.bailed {
                    return None;
                }
            }

            let next_label = match &block.terminator {
                ast::Terminator::Ret(value) => {
                    return value.as_ref().and_then(|tv| self.binding_for(&tv.value));
                }
                ast::Terminator::Unreachable => return None,
                ast::Terminator::Br { target } => target.clone(),
                ast::Terminator::CondBr {
                    cond,
                    if_true,
                    if_false,
                } => {
                    let Some(taken) = self.const_of(&cond.value) else {
                        self.bailed = true;
                        return None;
                    };
                    if taken.truthy() {
                        if_true.clone()
                    } else {
                        if_false.clone()
                    }
                }
                ast::Terminator::Switch {
                    scrutinee,
                    default,
                    cases,
                } => {
                    let Some(value) = self.const_of(&scrutinee.value) else {
                        self.bailed = true;
                        return None;
                    };
                    let key = value.as_i64();
                    cases
                        .iter()
                        .find(|(candidate, _)| match &candidate.value {
                            ast::Value::Int(i) => *i as i64 == key,
                            ast::Value::Bool(b) => i64::from(*b) == key,
                            _ => false,
                        })
                        .map(|(_, label)| label.clone())
                        .unwrap_or_else(|| default.clone())
                }
            };

            previous = Some(block.label.clone());
            match function.blocks.iter().position(|b| b.label == next_label) {
                Some(index) => current = index,
                None => {
                    self.bailed = true;
                    return None;
                }
            }
        }
    }

    fn const_of(&mut self, value: &ast::Value) -> Option<Const> {
        match value {
            ast::Value::Int(i) => Some(Const::Int(*i as i64)),
            ast::Value::Float(f) => Some(Const::Float(*f)),
            ast::Value::Bool(b) => Some(Const::Bool(*b)),
            ast::Value::Null | ast::Value::ZeroInit => Some(Const::Int(0)),
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Value(Operand::Const(c))) => Some(*c),
                Some(Binding::Qubit(q)) => Some(Const::Int(i64::from(q.0))),
                Some(Binding::Id(id)) => Some(Const::Int(i64::from(*id))),
                Some(Binding::ResultConst(b)) => Some(Const::Bool(*b)),
                Some(Binding::Cell(index)) => {
                    let slot = *index;
                    self.cells.get(slot).copied().flatten()
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn evaluate(&self, expr: &Expr) -> Option<Const> {
        expr.fold(|operand| operand.constant())
    }

    fn global_element(&self, name: &str, index: u64) -> Option<ast::Value> {
        match self.module.global(name)?.initializer.as_ref()? {
            ast::Value::Aggregate(items) => items.get(index as usize).map(|tv| tv.value.clone()),
            _ => None,
        }
    }

    fn run(&mut self) -> Program {
        let Some(entry) = self.module.entry_point() else {
            self.diagnostics.push(
                Diagnostic::error("no entry point found")
                    .with_code("QIR0201")
                    .note("expected a function with the \"entry_point\" attribute, or one named @main"),
            );
            return Program::new("empty", Profile::Unrestricted);
        };

        let attrs = self.module.attributes_of(&entry.sig);

        let profile = attrs
            .iter()
            .find(|a| a.key() == "qir_profiles")
            .and_then(|a| a.value())
            .map(Profile::from_attribute)
            .unwrap_or(Profile::Unrestricted);

        let declared_qubits =
            attribute_count(&attrs, &["required_num_qubits", "num_required_qubits"]);
        let declared_results =
            attribute_count(&attrs, &["required_num_results", "num_required_results"]);

        self.next_qubit = declared_qubits;
        self.next_result = declared_results;
        self.max_qubit = declared_qubits;
        self.max_result = declared_results;

        let mut program = Program::new(entry.sig.name.clone(), profile);

        for (index, block) in entry.blocks.iter().enumerate() {
            self.block_ids
                .insert(block.label.clone(), BlockId(index as u32));
        }

        for param in &entry.sig.params {
            let Some(name) = &param.name else { continue };
            match param.ty.pointee_name() {
                Some("Qubit") => {
                    let qubit = self.alloc_qubits(1);
                    self.env.insert(name.clone(), Binding::Qubit(qubit));
                }
                Some("Result") => {
                    let result = ResultId(self.next_result);
                    self.next_result += 1;
                    self.max_result = self.max_result.max(self.next_result);
                    self.env.insert(name.clone(), Binding::Result(result));
                }
                _ => {}
            }
        }

        for block in &entry.blocks {
            self.ops = Vec::new();

            for inst in &block.instructions {
                self.lower_instruction(inst);
            }

            let term = self.lower_terminator(&block.terminator, block.span);
            let ops = mem::take(&mut self.ops);

            program.blocks.push(Block {
                id: self.block_ids[&block.label],
                label: block.label.clone(),
                ops,
                term,
                span: block.span,
            });
        }

        if program.blocks.is_empty() {
            program.blocks.push(Block {
                id: BlockId(0),
                label: "entry".into(),
                ops: Vec::new(),
                term: Term::Ret(None),
                span: entry.span,
            });
        }

        program.entry = BlockId(0);
        program.num_slots = self.next_slot;
        program.num_qubits = self.max_qubit;
        program.num_results = self.max_result;
        program.next_value = self.next_value;
        self.check_wire_limit(&mut program);

        let mut unresolved: Vec<(String, Span)> = self
            .forward
            .drain()
            .map(|(name, (_, span))| (name, span))
            .collect();
        unresolved.sort_by_key(|(_, span)| span.start);
        for (name, span) in unresolved {
            self.error(
                format!("`%{name}` is not defined"),
                span,
                "a phi refers to a value that is never assigned",
            );
        }

        program
    }

    fn lower_terminator(&mut self, term: &ast::Terminator, span: Span) -> Term {
        match term {
            ast::Terminator::Ret(None) => Term::Ret(None),
            ast::Terminator::Ret(Some(tv)) => Term::Ret(self.operand(&tv.value, tv.span)),
            ast::Terminator::Unreachable => Term::Unreachable,
            ast::Terminator::Br { target } => match self.block_ids.get(target) {
                Some(id) => Term::Br(*id),
                None => {
                    self.error(
                        format!("branch to unknown block `{target}`"),
                        span,
                        "no such block",
                    );
                    Term::Unreachable
                }
            },
            ast::Terminator::CondBr {
                cond,
                if_true,
                if_false,
            } => {
                let cond_operand = self
                    .operand(&cond.value, cond.span)
                    .unwrap_or(Operand::Const(Const::Bool(false)));
                let then_id = self.block_ids.get(if_true).copied();
                let else_id = self.block_ids.get(if_false).copied();
                match (then_id, else_id) {
                    (Some(t), Some(e)) => Term::CondBr {
                        cond: cond_operand,
                        if_true: t,
                        if_false: e,
                    },
                    _ => {
                        self.error("conditional branch to unknown block", span, "no such block");
                        Term::Unreachable
                    }
                }
            }
            ast::Terminator::Switch {
                scrutinee,
                default,
                cases,
            } => {
                let on = self
                    .operand(&scrutinee.value, scrutinee.span)
                    .unwrap_or(Operand::Const(Const::Int(0)));
                let Some(default_id) = self.block_ids.get(default).copied() else {
                    self.error(
                        "switch default targets an unknown block",
                        span,
                        "no such block",
                    );
                    return Term::Unreachable;
                };
                let mut lowered = Vec::new();
                for (value, label) in cases {
                    let Some(target) = self.block_ids.get(label).copied() else {
                        self.error(
                            format!("switch case targets unknown block `{label}`"),
                            span,
                            "no such block",
                        );
                        continue;
                    };
                    let key = match &value.value {
                        ast::Value::Int(i) => *i as i64,
                        ast::Value::Bool(b) => *b as i64,
                        _ => continue,
                    };
                    lowered.push((key, target));
                }
                Term::Switch {
                    scrutinee: on,
                    cases: lowered,
                    default: default_id,
                }
            }
        }
    }

    fn lower_instruction(&mut self, inst: &ast::Instruction) {
        let span = inst.span;

        match &inst.kind {
            ast::InstKind::Call(call) => self.lower_call(inst.result.as_deref(), call, span),

            ast::InstKind::Binary { op, lhs, rhs, .. } => {
                let (Some(l), Some(r)) = (self.operand(lhs, span), self.operand(rhs, span)) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    Expr::Binary {
                        op: *op,
                        lhs: l,
                        rhs: r,
                    },
                    span,
                );
            }

            ast::InstKind::ICmp { pred, lhs, rhs, .. } => {
                let (Some(l), Some(r)) = (self.operand(lhs, span), self.operand(rhs, span)) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    Expr::ICmp {
                        pred: *pred,
                        lhs: l,
                        rhs: r,
                    },
                    span,
                );
            }

            ast::InstKind::FCmp { pred, lhs, rhs, .. } => {
                let (Some(l), Some(r)) = (self.operand(lhs, span), self.operand(rhs, span)) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    Expr::FCmp {
                        pred: *pred,
                        lhs: l,
                        rhs: r,
                    },
                    span,
                );
            }

            ast::InstKind::Select {
                cond,
                if_true,
                if_false,
            } => {
                let (Some(c), Some(t), Some(f)) = (
                    self.operand(&cond.value, span),
                    self.operand(&if_true.value, span),
                    self.operand(&if_false.value, span),
                ) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    Expr::Select {
                        cond: c,
                        if_true: t,
                        if_false: f,
                    },
                    span,
                );
            }

            ast::InstKind::Cast { op, operand, to } => {
                if let Some(qubit) = self.static_qubit(&operand.value)
                    && to.pointee_name() == Some("Qubit")
                {
                    if let Some(name) = inst.result.as_deref() {
                        self.env.insert(name.to_string(), Binding::Qubit(qubit));
                    }
                    return;
                }

                if *op == ast::CastOp::IntToPtr
                    && let Some(kind) = to.pointee_name().or(Some("ptr"))
                    && matches!(kind, "Qubit" | "Result" | "ptr")
                    && let Some(index) = self.const_of(&operand.value)
                    && let Some(name) = inst.result.as_deref()
                {
                    let Some(id) = wire(i128::from(index.as_i64())) else {
                        self.error(
                            format!("index {} is out of range", index.as_i64()),
                            span,
                            format!("ids must be below {MAX_WIRES}"),
                        );
                        return;
                    };
                    let binding = match kind {
                        "Qubit" => Binding::Qubit(QubitId(id)),
                        "Result" => Binding::Result(ResultId(id)),
                        _ => Binding::Id(id),
                    };
                    self.env.insert(name.to_string(), binding);
                    return;
                }

                if let Some(name) = inst.result.as_deref()
                    && let ast::Value::Local(src) = &operand.value
                    && let Some(binding) = self.env.get(src).cloned()
                    && !matches!(binding, Binding::Value(_))
                {
                    self.env.insert(name.to_string(), binding);
                    return;
                }

                let Some(value) = self.operand(&operand.value, span) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    Expr::Cast {
                        op: *op,
                        operand: value,
                    },
                    span,
                );
            }

            ast::InstKind::Phi { incoming, .. } => {
                if self.flattening() {
                    let from = self.previous_label.clone();
                    let chosen = from.and_then(|label| {
                        incoming
                            .iter()
                            .find(|(_, block)| *block == label)
                            .map(|(value, _)| value.clone())
                    });
                    match chosen.and_then(|value| self.binding_for(&value)) {
                        Some(binding) => {
                            if let Some(name) = inst.result.as_deref() {
                                self.env.insert(name.to_string(), binding);
                            }
                        }
                        None => self.bailed = true,
                    }
                    return;
                }

                let mut lowered = Vec::new();
                for (value, label) in incoming {
                    let Some(block) = self.block_ids.get(label).copied() else {
                        self.error(
                            format!("phi refers to unknown block `{label}`"),
                            span,
                            "no such block",
                        );
                        continue;
                    };
                    let operand = match value {
                        ast::Value::Local(name) if !self.env.contains_key(name) => {
                            let next = ValueId(self.next_value);
                            let id = self.forward.entry(name.clone()).or_insert((next, span)).0;
                            if id == next {
                                self.next_value += 1;
                            }
                            Operand::Value(id)
                        }
                        _ => match self.operand(value, span) {
                            Some(operand) => operand,
                            None => continue,
                        },
                    };
                    lowered.push((block, operand));
                }
                self.assign(inst.result.as_deref(), Expr::Phi(lowered), span);
            }

            ast::InstKind::Alloca { .. } => {
                if self.flattening() {
                    let cell = self.cells.len();
                    self.cells.push(None);
                    if let Some(name) = inst.result.as_deref() {
                        self.env.insert(name.to_string(), Binding::Cell(cell));
                    }
                    return;
                }

                let slot = SlotId(self.next_slot);
                self.next_slot += 1;
                if let Some(name) = inst.result.as_deref() {
                    self.env.insert(name.to_string(), Binding::Slot(slot));
                }
            }

            ast::InstKind::Store { value, ptr } => {
                if self.flattening() {
                    let cell = match &ptr.value {
                        ast::Value::Local(name) => match self.env.get(name) {
                            Some(Binding::Cell(index)) => Some(*index),
                            _ => None,
                        },
                        _ => None,
                    };

                    match (cell, self.binding_for(&value.value)) {
                        (Some(index), Some(Binding::Value(Operand::Const(c)))) => {
                            self.cells[index] = Some(c);
                        }
                        (Some(index), Some(Binding::Qubit(q))) => {
                            self.cells[index] = Some(Const::Int(i64::from(q.0)));
                            self.env.insert(format!("cell#{index}"), Binding::Qubit(q));
                        }
                        (Some(_), _) => self.bailed = true,
                        (None, _) => {}
                    }
                    return;
                }

                let Some(slot) = self.resolve_slot(&ptr.value) else {
                    return;
                };
                let Some(operand) = self.operand(&value.value, span) else {
                    return;
                };
                self.ops.push(Op::Store {
                    slot,
                    value: operand,
                    span,
                });
            }

            ast::InstKind::Load { ptr, .. } => {
                if self.flattening() {
                    let binding = match &ptr.value {
                        ast::Value::Local(name) => self.env.get(name).cloned(),
                        _ => None,
                    };

                    match binding {
                        Some(Binding::Cell(index)) => {
                            if let Some(qubit) = self.env.get(&format!("cell#{index}")).cloned() {
                                if let Some(name) = inst.result.as_deref() {
                                    self.env.insert(name.to_string(), qubit);
                                }
                                return;
                            }
                            match self.cells.get(index).copied().flatten() {
                                Some(value) => {
                                    if let Some(name) = inst.result.as_deref() {
                                        self.env.insert(
                                            name.to_string(),
                                            Binding::Value(Operand::Const(value)),
                                        );
                                    }
                                }
                                None => self.bailed = true,
                            }
                            return;
                        }
                        Some(Binding::GlobalElement {
                            name: global,
                            index,
                        }) => {
                            match self.global_element(&global, index) {
                                Some(element) => match self.binding_for(&element) {
                                    Some(found) => {
                                        if let Some(name) = inst.result.as_deref() {
                                            self.env.insert(name.to_string(), found);
                                        }
                                    }
                                    None => self.bailed = true,
                                },
                                None => self.bailed = true,
                            }
                            return;
                        }
                        _ => {}
                    }
                }

                if let Some(slot) = self.resolve_slot(&ptr.value) {
                    self.assign(inst.result.as_deref(), Expr::Load(slot), span);
                    return;
                }
                if let ast::Value::Global(global) = &ptr.value
                    && let Some(bytes) = self.global_bytes(global)
                    && let Some(name) = inst.result.as_deref()
                {
                    self.env.insert(name.to_string(), Binding::Bytes(bytes));
                    return;
                }
                self.propagate_binding(inst.result.as_deref(), &ptr.value);
            }

            ast::InstKind::GetElementPtr { ptr, indices, .. } => {
                if self.flattening()
                    && let Some(binding) = self.array_element(&ptr.value, indices)
                    && let Some(name) = inst.result.as_deref()
                {
                    self.env.insert(name.to_string(), binding);
                    return;
                }

                self.propagate_binding(inst.result.as_deref(), &ptr.value);
            }

            ast::InstKind::Freeze(tv) => {
                self.propagate_binding(inst.result.as_deref(), &tv.value);
            }

            ast::InstKind::ExtractValue { .. }
            | ast::InstKind::InsertValue { .. }
            | ast::InstKind::Fence => {}

            ast::InstKind::Unsupported { opcode } => {
                self.warn(
                    format!("ignoring unsupported instruction `{opcode}`"),
                    span,
                    "this opcode has no quantum meaning",
                );
            }
        }
    }

    fn array_element(&mut self, base: &ast::Value, indices: &[ast::TypedValue]) -> Option<Binding> {
        let ast::Value::Global(name) = base else {
            return None;
        };
        self.module.global(name)?;

        let last = indices.last()?;
        let index = match self.const_of(&last.value) {
            Some(value) => value.as_i64(),
            None => return None,
        };
        if index < 0 {
            return None;
        }

        Some(Binding::GlobalElement {
            name: name.clone(),
            index: index as u64,
        })
    }

    fn resolve_slot(&self, value: &ast::Value) -> Option<SlotId> {
        match value {
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Slot(slot)) => Some(*slot),
                _ => None,
            },
            _ => None,
        }
    }

    fn propagate_binding(&mut self, result: Option<&str>, source: &ast::Value) {
        let Some(name) = result else { return };
        if let ast::Value::Local(src) = source
            && let Some(binding) = self.env.get(src).cloned()
        {
            self.env.insert(name.to_string(), binding);
        }
    }

    fn assign(&mut self, result: Option<&str>, expr: Expr, span: Span) {
        if self.flattening()
            && let Some(value) = self.evaluate(&expr)
        {
            if let Some(name) = result {
                self.env
                    .insert(name.to_string(), Binding::Value(Operand::Const(value)));
            }
            return;
        }

        let dest = match result.and_then(|name| self.forward.remove(name)) {
            Some((id, _)) => id,
            None => self.fresh_value(),
        };
        self.ops.push(Op::Assign { dest, expr, span });
        if let Some(name) = result {
            self.env
                .insert(name.to_string(), Binding::Value(Operand::Value(dest)));
        }
    }

    fn lower_call(&mut self, result: Option<&str>, call: &ast::Call, span: Span) {
        let Some(callee) = call.callee_name() else {
            self.error(
                "indirect calls are not supported",
                span,
                "callee is not a symbol",
            );
            return;
        };

        if let Some(resolved) = qis::resolve(callee) {
            self.lower_intrinsic(result, call, resolved.intrinsic, resolved.functor, span);
            return;
        }

        if let Some(function) = self.module.function(callee) {
            self.inline(result, call, function, span);
            return;
        }

        if self.module.declarations.iter().any(|d| d.name == callee) {
            self.warn(
                format!("ignoring call to unknown external function `{callee}`"),
                span,
                "not a QIR intrinsic",
            );
            return;
        }

        self.error(
            format!("call to undefined function `{callee}`"),
            span,
            "no definition or declaration in this module",
        );
    }

    fn inline(
        &mut self,
        result: Option<&str>,
        call: &ast::Call,
        function: &'a ast::Function,
        span: Span,
    ) {
        if self.inline_depth >= MAX_INLINE_DEPTH {
            self.error(
                format!("inlining `{}` exceeded the depth limit", function.sig.name),
                span,
                "possible recursion",
            );
            return;
        }

        if self.flattening() {
            let mut scope: HashMap<String, Binding> = HashMap::new();
            for (param, arg) in function.sig.params.iter().zip(&call.args) {
                let Some(name) = &param.name else { continue };
                if let Some(binding) = self.binding_for(&arg.value) {
                    scope.insert(name.clone(), binding);
                }
            }

            let saved_env = mem::replace(&mut self.env, scope);
            let saved_previous = self.previous_label.take();
            self.inline_depth += 1;

            let returned = self.execute_function(function);

            self.inline_depth -= 1;
            self.env = saved_env;
            self.previous_label = saved_previous;

            if let (Some(name), Some(binding)) = (result, returned) {
                self.env.insert(name.to_string(), binding);
            }
            return;
        }

        if function.blocks.len() != 1 {
            self.error(
                format!("cannot inline recursive call to `{}`", function.sig.name),
                span,
                "recursion is not supported",
            );
            return;
        }

        let mut scope: HashMap<String, Binding> = HashMap::new();
        for (param, arg) in function.sig.params.iter().zip(&call.args) {
            let Some(name) = &param.name else { continue };
            if let Some(binding) = self.binding_for(&arg.value) {
                scope.insert(name.clone(), binding);
            }
        }

        let saved = mem::replace(&mut self.env, scope);
        self.inline_depth += 1;

        let body = &function.blocks[0];
        for inst in &body.instructions {
            self.lower_instruction(inst);
        }

        let returned = match &body.terminator {
            ast::Terminator::Ret(Some(tv)) => self.binding_for(&tv.value),
            _ => None,
        };

        self.inline_depth -= 1;
        self.env = saved;

        if let (Some(name), Some(binding)) = (result, returned) {
            self.env.insert(name.to_string(), binding);
        }
    }

    fn binding_for(&mut self, value: &ast::Value) -> Option<Binding> {
        if let Some(qubit) = self.static_qubit(value) {
            return Some(Binding::Qubit(qubit));
        }

        if let ast::Value::Local(name) = value {
            return self.env.get(name).cloned();
        }

        let span = Span::DUMMY;
        self.operand(value, span).map(Binding::Value)
    }

    fn lower_intrinsic(
        &mut self,
        result: Option<&str>,
        call: &ast::Call,
        intrinsic: Intrinsic,
        functor: Functor,
        span: Span,
    ) {
        match intrinsic {
            Intrinsic::Gate {
                kind,
                controls,
                targets,
                params,
            } => self.lower_gate(
                call,
                GateShape {
                    kind,
                    controls,
                    targets,
                    params,
                },
                functor,
                span,
            ),

            Intrinsic::Measure { with_result_arg } => {
                let Some(qubit) = self.qubit_arg(call, 0, span) else {
                    return;
                };

                let result_id = if with_result_arg {
                    match self.result_arg(call, 1, span) {
                        Some(id) => id,
                        None => return,
                    }
                } else {
                    let id = ResultId(self.next_result);
                    self.next_result += 1;
                    id
                };

                self.note_qubit(qubit);
                self.note_result(result_id);

                if let Some(name) = result {
                    self.env
                        .insert(name.to_string(), Binding::Result(result_id));
                }

                self.ops.push(Op::Measure {
                    qubit,
                    result: result_id,
                    dest: None,
                    span,
                });
            }

            Intrinsic::Reset => {
                if let Some(qubit) = self.qubit_arg(call, 0, span) {
                    self.note_qubit(qubit);
                    self.ops.push(Op::Reset { qubit, span });
                }
            }

            Intrinsic::ReadResult => {
                let Some(result_id) = self.result_arg(call, 0, span) else {
                    return;
                };
                self.note_result(result_id);
                self.assign(result, Expr::ReadResult(result_id), span);
            }

            Intrinsic::ResultGetZero => {
                if let Some(name) = result {
                    self.env
                        .insert(name.to_string(), Binding::ResultConst(false));
                }
            }

            Intrinsic::ResultGetOne => {
                if let Some(name) = result {
                    self.env
                        .insert(name.to_string(), Binding::ResultConst(true));
                }
            }

            Intrinsic::ResultEqual => self.lower_result_equal(result, call, span),

            Intrinsic::RecordOutput(kind) => {
                let (result_id, count) = match kind {
                    OutputKind::Tuple | OutputKind::Array => {
                        let count = call.args.first().and_then(|a| match a.value {
                            ast::Value::Int(i) => Some(i as i64),
                            _ => None,
                        });
                        (None, count)
                    }
                    _ => (self.result_arg(call, 0, span), None),
                };

                if let Some(id) = result_id {
                    self.note_result(id);
                }

                let label = call.args.last().and_then(|a| self.resolve_label(&a.value));

                self.ops.push(Op::RecordOutput {
                    kind,
                    result: result_id,
                    count,
                    label,
                    span,
                });
            }

            Intrinsic::QubitAllocate => {
                let qubit = self.alloc_qubits(1);
                if let Some(name) = result {
                    self.env.insert(name.to_string(), Binding::Qubit(qubit));
                }
            }

            Intrinsic::QubitAllocateArray => {
                let count = match call
                    .args
                    .first()
                    .map(|a| a.value.clone())
                    .and_then(|value| self.const_of(&value))
                {
                    Some(n) if n.as_i64() >= 0 => n.as_i64() as u64,
                    _ => {
                        self.error(
                            "qubit array length must be a compile time constant",
                            span,
                            "this length is not known at compile time",
                        );
                        return;
                    }
                };
                let base = self.alloc_qubits(count);
                if let Some(name) = result {
                    self.env
                        .insert(name.to_string(), Binding::QubitArray { base, len: count });
                }
            }

            Intrinsic::ArrayGetElementPtr => {
                let Some(array) = call.args.first().map(|a| a.value.clone()) else {
                    return;
                };
                let ast::Value::Local(array_name) = &array else {
                    return;
                };
                let Some(Binding::QubitArray { base, len }) = self.env.get(array_name).cloned()
                else {
                    return;
                };

                let index = match call
                    .args
                    .get(1)
                    .map(|a| a.value.clone())
                    .and_then(|value| self.const_of(&value))
                {
                    Some(i) if i.as_i64() >= 0 => i.as_i64() as u64,
                    _ => {
                        self.error(
                            "qubit array index must be a compile time constant",
                            span,
                            "this index depends on runtime state",
                        );
                        return;
                    }
                };

                if index >= len {
                    self.error(
                        format!("qubit index {index} is out of bounds for an array of {len}"),
                        span,
                        "out of range",
                    );
                    return;
                }

                let qubit = QubitId(base.0 + index as u32);
                self.note_qubit(qubit);
                if let Some(name) = result {
                    self.env.insert(name.to_string(), Binding::Qubit(qubit));
                }
            }

            Intrinsic::QubitRelease | Intrinsic::QubitReleaseArray | Intrinsic::Initialize => {}

            Intrinsic::Message => {
                let text = call
                    .args
                    .first()
                    .and_then(|a| self.resolve_label(&a.value))
                    .unwrap_or_default();
                self.ops.push(Op::Message { text, span });
            }

            Intrinsic::Ignored => {
                if let (Some(name), Some(arg)) = (result, call.args.first())
                    && let Some(binding) = self.binding_for(&arg.value)
                {
                    self.env.insert(name.to_string(), binding);
                }
            }
        }
    }

    fn lower_result_equal(&mut self, result: Option<&str>, call: &ast::Call, span: Span) {
        let lhs = call.args.first().map(|a| a.value.clone());
        let rhs = call.args.get(1).map(|a| a.value.clone());

        let resolve = |this: &mut Self, value: Option<ast::Value>| -> Option<Binding> {
            let value = value?;
            if let Some(id) = this.static_result(&value) {
                return Some(Binding::Result(id));
            }
            if let ast::Value::Local(name) = &value {
                return this.env.get(name).cloned();
            }
            None
        };

        let left = resolve(self, lhs);
        let right = resolve(self, rhs);

        let expr = match (left, right) {
            (Some(Binding::Result(id)), Some(Binding::ResultConst(expected)))
            | (Some(Binding::ResultConst(expected)), Some(Binding::Result(id))) => {
                self.note_result(id);
                let read = self.fresh_value();
                self.ops.push(Op::Assign {
                    dest: read,
                    expr: Expr::ReadResult(id),
                    span,
                });
                Expr::ICmp {
                    pred: IntPredicate::Eq,
                    lhs: Operand::Value(read),
                    rhs: Operand::Const(Const::Bool(expected)),
                }
            }
            (Some(Binding::Result(a)), Some(Binding::Result(b))) => {
                self.note_result(a);
                self.note_result(b);
                let left_value = self.fresh_value();
                self.ops.push(Op::Assign {
                    dest: left_value,
                    expr: Expr::ReadResult(a),
                    span,
                });
                let right_value = self.fresh_value();
                self.ops.push(Op::Assign {
                    dest: right_value,
                    expr: Expr::ReadResult(b),
                    span,
                });
                Expr::ICmp {
                    pred: IntPredicate::Eq,
                    lhs: Operand::Value(left_value),
                    rhs: Operand::Value(right_value),
                }
            }
            (Some(Binding::ResultConst(a)), Some(Binding::ResultConst(b))) => {
                Expr::Const(Const::Bool(a == b))
            }
            _ => {
                self.error(
                    "cannot compare these results",
                    span,
                    "operands do not resolve to measurement results",
                );
                return;
            }
        };

        self.assign(result, expr, span);
    }

    fn lower_gate(&mut self, call: &ast::Call, shape: GateShape, functor: Functor, span: Span) {
        let GateShape {
            mut kind,
            controls,
            targets,
            params,
        } = shape;

        if functor.is_adjoint() {
            match kind.adjoint() {
                Some(adjoint) => kind = adjoint,
                None if kind.param_count() == 1 => {}
                None => {
                    self.error(
                        format!("gate `{}` has no adjoint", kind.name()),
                        span,
                        "cannot invert this gate",
                    );
                    return;
                }
            }
        }

        let extra_controls = usize::from(functor.is_controlled());
        let total_controls = controls + extra_controls;

        let mut angles = Vec::new();
        for index in 0..params {
            let Some(arg) = call.args.get(index) else {
                self.error("missing rotation angle", span, "expected a double argument");
                return;
            };
            let Some(mut operand) = self.operand(&arg.value, arg.span) else {
                self.error(
                    "rotation angle is not a value",
                    arg.span,
                    "expected a number",
                );
                return;
            };
            if functor.is_adjoint() && kind.param_count() == 1 {
                operand = self.negate(operand, span);
            }
            angles.push(operand);
        }

        let mut wires = Vec::new();
        for index in 0..(total_controls + targets) {
            let Some(qubit) = self.qubit_arg(call, params + index, span) else {
                return;
            };
            wires.push(qubit);
        }

        let target_wires = wires.split_off(total_controls);
        for qubit in wires.iter().chain(target_wires.iter()) {
            self.note_qubit(*qubit);
        }

        self.ops.push(Op::Gate(Gate {
            kind,
            controls: wires,
            targets: target_wires,
            params: angles,
            span,
        }));
    }

    fn negate(&mut self, operand: Operand, span: Span) -> Operand {
        if let Operand::Const(c) = operand {
            return Operand::Const(Const::Float(-c.as_f64()));
        }

        let dest = self.fresh_value();
        self.ops.push(Op::Assign {
            dest,
            expr: Expr::Binary {
                op: BinOp::FSub,
                lhs: Operand::Const(Const::Float(0.0)),
                rhs: operand,
            },
            span,
        });
        Operand::Value(dest)
    }

    fn qubit_arg(&mut self, call: &ast::Call, index: usize, span: Span) -> Option<QubitId> {
        let Some(arg) = call.args.get(index) else {
            self.error(
                format!("missing qubit argument {}", index + 1),
                span,
                "not enough arguments",
            );
            return None;
        };

        if let Some(qubit) = self.static_qubit(&arg.value) {
            self.note_qubit(qubit);
            return Some(qubit);
        }

        if let Some(index) = literal_index(&arg.value)
            && wire(index).is_none()
        {
            self.error(
                format!("qubit index {index} is out of range"),
                arg.span,
                format!("qubit ids must be below {MAX_WIRES}"),
            );
            return None;
        }

        self.error(
            "cannot resolve this operand to a qubit",
            arg.span,
            "expected a static qubit reference",
        );

        if let Some(last) = self.diagnostics.last_mut() {
            last.notes
                .push("use inttoptr, null, or a constant array index".into());
        }

        None
    }

    fn result_arg(&mut self, call: &ast::Call, index: usize, span: Span) -> Option<ResultId> {
        let Some(arg) = call.args.get(index) else {
            self.error(
                format!("missing result argument {}", index + 1),
                span,
                "not enough arguments",
            );
            return None;
        };

        if let Some(id) = self.static_result(&arg.value) {
            self.note_result(id);
            return Some(id);
        }

        if let Some(index) = literal_index(&arg.value)
            && wire(index).is_none()
        {
            self.error(
                format!("result index {index} is out of range"),
                arg.span,
                format!("result ids must be below {MAX_WIRES}"),
            );
            return None;
        }

        self.error(
            "cannot resolve this operand to a measurement result",
            arg.span,
            "expected a static result reference",
        );
        None
    }

    fn static_qubit(&self, value: &ast::Value) -> Option<QubitId> {
        match value {
            ast::Value::Null => Some(QubitId(0)),
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::Cast {
                    op: ast::CastOp::IntToPtr,
                    operand,
                    to,
                } => {
                    if !matches!(to.pointee_name(), Some("Qubit") | None) {
                        return None;
                    }
                    match operand.value {
                        ast::Value::Int(i) => wire(i).map(QubitId),
                        _ => None,
                    }
                }
                _ => None,
            },
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Qubit(q)) => Some(*q),
                Some(Binding::QubitArray { base, .. }) => Some(*base),
                Some(Binding::Id(id)) => Some(QubitId(*id)),
                _ => None,
            },
            _ => None,
        }
    }

    fn static_result(&self, value: &ast::Value) -> Option<ResultId> {
        match value {
            ast::Value::Null => Some(ResultId(0)),
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::Cast {
                    op: ast::CastOp::IntToPtr,
                    operand,
                    to,
                } => {
                    if !matches!(to.pointee_name(), Some("Result") | None) {
                        return None;
                    }
                    match operand.value {
                        ast::Value::Int(i) => wire(i).map(ResultId),
                        _ => None,
                    }
                }
                _ => None,
            },
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Result(r)) => Some(*r),
                Some(Binding::Id(id)) => Some(ResultId(*id)),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_label(&mut self, value: &ast::Value) -> Option<String> {
        match value {
            ast::Value::Null => None,
            ast::Value::Bytes(bytes) => Some(decode_label(bytes)),
            ast::Value::Global(name) => self.global_bytes(name).map(|b| decode_label(&b)),
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::GetElementPtr { ptr, .. } => self.resolve_label(&ptr.value),
                ast::ConstExpr::Cast { operand, .. } => self.resolve_label(&operand.value),
                _ => None,
            },
            ast::Value::Local(name) => {
                let bytes = match self.env.get(name) {
                    Some(Binding::Bytes(bytes)) => Some(bytes.clone()),
                    _ => None,
                };
                bytes.map(|b| decode_label(&b))
            }
            _ => None,
        }
    }

    fn global_bytes(&self, name: &str) -> Option<Vec<u8>> {
        match self.module.global(name)?.initializer.as_ref()? {
            ast::Value::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        }
    }

    fn operand(&mut self, value: &ast::Value, span: Span) -> Option<Operand> {
        match value {
            ast::Value::Int(i) => Some(Operand::Const(Const::Int(*i as i64))),
            ast::Value::Float(f) => Some(Operand::Const(Const::Float(*f))),
            ast::Value::Bool(b) => Some(Operand::Const(Const::Bool(*b))),
            ast::Value::Null | ast::Value::ZeroInit | ast::Value::NoneValue => {
                Some(Operand::Const(Const::Int(0)))
            }
            ast::Value::Undef | ast::Value::Poison => Some(Operand::Const(Const::Int(0))),
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Value(operand)) => Some(*operand),
                Some(Binding::ResultConst(b)) => Some(Operand::Const(Const::Bool(*b))),
                Some(Binding::Qubit(q)) if self.flattening() => {
                    Some(Operand::Const(Const::Int(i64::from(q.0))))
                }
                Some(Binding::Result(id)) => {
                    let id = *id;
                    self.note_result(id);
                    let dest = self.fresh_value();
                    self.ops.push(Op::Assign {
                        dest,
                        expr: Expr::ReadResult(id),
                        span,
                    });
                    Some(Operand::Value(dest))
                }
                _ => {
                    self.error(format!("`%{name}` is not defined"), span, "unknown value");
                    None
                }
            },
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::Cast { operand, .. } => self.operand(&operand.value, span),
                _ => Some(Operand::Const(Const::Int(0))),
            },
            _ => None,
        }
    }
}

fn literal_index(value: &ast::Value) -> Option<i128> {
    match value {
        ast::Value::ConstExpr(expr) => match expr.as_ref() {
            ast::ConstExpr::Cast {
                op: ast::CastOp::IntToPtr,
                operand,
                ..
            } => match operand.value {
                ast::Value::Int(i) => Some(i),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn wire(index: i128) -> Option<u32> {
    u32::try_from(index).ok().filter(|&i| i < MAX_WIRES)
}

fn decode_label(bytes: &[u8]) -> String {
    let trimmed = bytes
        .iter()
        .copied()
        .take_while(|b| *b != 0)
        .collect::<Vec<u8>>();
    String::from_utf8_lossy(&trimmed).into_owned()
}

fn attribute_count(attrs: &[&ast::Attribute], keys: &[&str]) -> u32 {
    for key in keys {
        if let Some(value) = attrs
            .iter()
            .find(|a| a.key() == *key)
            .and_then(|a| a.value())
            && let Ok(parsed) = value.parse::<u32>()
        {
            return parsed;
        }
    }
    0
}
