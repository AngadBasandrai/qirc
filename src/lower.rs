use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;
use std::mem;
use std::ops::Range;

use crate::ast;
use crate::diag::{Diagnostic, Span};
use crate::inline::inline_module;
use crate::ir::*;
use crate::qis::{self, Functor, Intrinsic};

pub struct Lowered {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lower(module: &ast::Module) -> Lowered {
    let expanded = inline_module(module);
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

const MAX_INLINE_DEPTH: usize = if cfg!(target_arch = "wasm32") {
    32
} else {
    10_000
};
const MAX_FLATTEN_STEPS: usize = 200_000;
const MAX_FLATTEN_OPS: usize = 2_000_000;

#[derive(PartialEq)]
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

    fn scalar(&mut self, ty: &ast::Ty, span: Span) -> Option<Scalar> {
        let scalar = match ty {
            ast::Ty::Int(1) => Scalar::Bool,
            ast::Ty::Int(bits @ 2..=64) => Scalar::Int(*bits),
            ast::Ty::Float => Scalar::Float,
            ast::Ty::Double => Scalar::Double,
            ast::Ty::Int(bits) => {
                self.error(
                    format!("integer type i{bits} is not supported"),
                    span,
                    "qirc scalar integers must contain between 1 and 64 bits",
                );
                return None;
            }
            ast::Ty::Half => {
                self.error(
                    "half-precision floating point is not supported",
                    span,
                    "qirc supports float and double scalar arithmetic",
                );
                return None;
            }
            ast::Ty::X86Fp80 | ast::Ty::Fp128 => {
                self.error(
                    "extended-precision floating point is not supported",
                    span,
                    "qirc supports float and double scalar arithmetic",
                );
                return None;
            }
            _ => {
                self.error(
                    "a non-scalar type cannot be used in scalar arithmetic",
                    span,
                    "expected an integer, float, or double",
                );
                return None;
            }
        };
        Some(scalar)
    }

    fn cast_scalar(&mut self, ty: &ast::Ty, span: Span) -> Option<Scalar> {
        match ty {
            ast::Ty::Ptr(_) => Some(Scalar::Int(64)),
            _ => self.scalar(ty, span),
        }
    }

    fn validate_constant_cast(
        &mut self,
        op: ast::CastOp,
        from: Scalar,
        to: Scalar,
        value: Const,
        span: Span,
    ) -> Option<()> {
        if !matches!(op, ast::CastOp::FPToSI | ast::CastOp::FPToUI) {
            return Some(());
        }
        let bits = match to {
            Scalar::Bool => 1,
            Scalar::Int(bits) => bits,
            _ => 0,
        };
        let value = from.normalize(value).as_f64();
        let truncated = value.trunc();
        let fits = from.is_float()
            && bits > 0
            && value.is_finite()
            && match op {
                ast::CastOp::FPToSI => {
                    let limit = 2_f64.powi(bits as i32 - 1);
                    truncated >= -limit && truncated < limit
                }
                ast::CastOp::FPToUI => truncated >= 0.0 && truncated < 2_f64.powi(bits as i32),
                _ => unreachable!(),
            };
        if fits {
            Some(())
        } else {
            self.error(
                "floating-point to integer conversion is out of range",
                span,
                "this conversion is poison in LLVM",
            );
            None
        }
    }

    fn validate_binary_type(&mut self, op: ast::BinOp, ty: Scalar, span: Span) -> Option<()> {
        if op.is_float() == ty.is_float() {
            return Some(());
        }
        let (message, label) = if op.is_float() {
            (
                "a floating-point binary opcode needs floating-point operands",
                "this operand type is not floating point",
            )
        } else {
            (
                "an integer binary opcode needs integer operands",
                "this operand type is not an integer",
            )
        };
        self.error(message, span, label);
        None
    }

    fn validate_cast_type(
        &mut self,
        op: ast::CastOp,
        from_ty: &ast::Ty,
        to_ty: &ast::Ty,
        span: Span,
    ) -> Option<(Scalar, Scalar)> {
        let from = self.cast_scalar(from_ty, span)?;
        let to = self.cast_scalar(to_ty, span)?;
        let from_ptr = matches!(from_ty, ast::Ty::Ptr(_));
        let to_ptr = matches!(to_ty, ast::Ty::Ptr(_));
        let from_int = !from_ptr && !from.is_float();
        let to_int = !to_ptr && !to.is_float();

        let valid = match op {
            ast::CastOp::Trunc => from_int && to_int && from.bits() > to.bits(),
            ast::CastOp::ZExt | ast::CastOp::SExt => from_int && to_int && from.bits() < to.bits(),
            ast::CastOp::FPTrunc => from.is_float() && to.is_float() && from.bits() > to.bits(),
            ast::CastOp::FPExt => from.is_float() && to.is_float() && from.bits() < to.bits(),
            ast::CastOp::FPToUI | ast::CastOp::FPToSI => from.is_float() && to_int,
            ast::CastOp::UIToFP | ast::CastOp::SIToFP => from_int && to.is_float(),
            ast::CastOp::PtrToInt => from_ptr && to_int,
            ast::CastOp::IntToPtr => from_int && to_ptr,
            ast::CastOp::BitCast => {
                (from_ptr && to_ptr) || (!from_ptr && !to_ptr && from.bits() == to.bits())
            }
            ast::CastOp::AddrSpaceCast => from_ptr && to_ptr,
        };
        if valid {
            Some((from, to))
        } else {
            self.error(
                format!("invalid `{}` cast for these operand types", op.keyword()),
                span,
                "the source and destination types do not satisfy this cast's LLVM constraints",
            );
            None
        }
    }

    fn validate_constant_binary(
        &mut self,
        op: ast::BinOp,
        ty: Scalar,
        left: Option<Const>,
        right: Option<Const>,
        span: Span,
    ) -> Option<()> {
        if op.is_float() {
            return Some(());
        }
        if matches!(
            op,
            ast::BinOp::UDiv | ast::BinOp::SDiv | ast::BinOp::URem | ast::BinOp::SRem
        ) && right.is_some_and(|value| ty.unsigned(value) == 0)
        {
            self.error(
                "integer division by zero",
                span,
                "this expression is poison in LLVM",
            );
            return None;
        }
        if matches!(op, ast::BinOp::SDiv | ast::BinOp::SRem)
            && left.is_some_and(|value| ty.signed(value) == signed_min(ty.bits()))
            && right.is_some_and(|value| ty.signed(value) == -1)
        {
            self.error(
                "signed integer division or remainder overflow",
                span,
                "this expression is poison in LLVM",
            );
            return None;
        }
        if matches!(op, ast::BinOp::Shl | ast::BinOp::LShr | ast::BinOp::AShr)
            && right.is_some_and(|value| ty.unsigned(value) >= u64::from(ty.bits()))
        {
            self.error("oversized shift", span, "this expression is poison in LLVM");
            return None;
        }
        Some(())
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

    fn alloc_result(&mut self) -> ResultId {
        let id = ResultId(self.next_result);
        self.next_result += 1;
        self.note_result(id);
        id
    }

    fn read_result(&mut self, id: ResultId, span: Span) -> Operand {
        self.note_result(id);
        let dest = self.fresh_value();
        self.ops.push(Op::Assign {
            dest,
            ty: Scalar::Bool,
            expr: Expr::ReadResult(id),
            span,
        });
        Operand::Value(dest)
    }

    fn bind(&mut self, name: Option<&str>, binding: Binding) {
        if let Some(name) = name {
            self.env.insert(name.to_string(), binding);
        }
    }

    fn entry(&mut self) -> Option<(&'a ast::Function, Profile, u32, u32)> {
        let entry = self.module.entry_point()?;
        let attrs = self.module.attributes_of(&entry.sig);

        let profile = attrs
            .iter()
            .find(|a| a.key() == "qir_profiles")
            .and_then(|a| a.value())
            .map(Profile::from_attribute)
            .unwrap_or(Profile::Unrestricted);

        let mut count = |keys: &[&str]| match attribute_count(&attrs, keys) {
            Ok(value) => value,
            Err(message) => {
                self.diagnostics
                    .push(Diagnostic::error(message).with_code("QIR0202"));
                0
            }
        };
        let declared_qubits = count(&["required_num_qubits", "num_required_qubits"]);
        let declared_results = count(&["required_num_results", "num_required_results"]);

        Some((entry, profile, declared_qubits, declared_results))
    }

    fn declare(&mut self, qubits: u32, results: u32) {
        self.next_qubit = qubits;
        self.next_result = results;
        self.max_qubit = qubits;
        self.max_result = results;
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
                    let result = self.alloc_result();
                    self.env.insert(name.clone(), Binding::Result(result));
                }
                _ => {}
            }
        }
    }

    fn run_flat(&mut self) -> Option<Program> {
        let (entry, profile, qubits, results) = self.entry()?;
        if qubits > MAX_WIRES || results > MAX_WIRES {
            self.too_many_wires = true;
        } else {
            self.declare(qubits, results);
        }

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
        program.num_qubits = self.max_qubit;
        program.num_results = self.max_result;
        program.next_value = self.next_value;
        self.check_wire_limit(&mut program);

        Some(program)
    }

    fn execute_function(&mut self, function: &'a ast::Function) -> Option<Binding> {
        let positions: HashMap<&str, usize> = function
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.label.as_str(), index))
            .collect();
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

            let phis = block
                .instructions
                .iter()
                .take_while(|inst| matches!(inst.kind, ast::InstKind::Phi { .. }))
                .count();
            let chosen: Vec<Option<Binding>> = block.instructions[..phis]
                .iter()
                .map(|inst| self.phi_choice(inst))
                .collect();
            for (inst, binding) in block.instructions[..phis].iter().zip(chosen) {
                let Some(binding) = binding else {
                    self.bailed = true;
                    return None;
                };
                self.bind(inst.result.as_deref(), binding);
            }

            for inst in &block.instructions[phis..] {
                self.lower_instruction(inst);
                if self.bailed {
                    return None;
                }
            }

            let next_label = match &block.terminator {
                ast::Terminator::Ret(value) => {
                    if let Some(tv) = value {
                        if !matches!(tv.ty, ast::Ty::Ptr(_))
                            && self.scalar(&tv.ty, tv.span).is_none()
                        {
                            self.bailed = true;
                            return None;
                        }
                        return self.binding_for(&tv.value);
                    }
                    return None;
                }
                ast::Terminator::Unreachable => return None,
                ast::Terminator::Br { target } => target.clone(),
                ast::Terminator::CondBr {
                    cond,
                    if_true,
                    if_false,
                } => {
                    if cond.ty != ast::Ty::Int(1) {
                        self.bailed = true;
                        return None;
                    }
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
                    if let Some((value, _)) =
                        cases.iter().find(|(value, _)| value.ty != scrutinee.ty)
                    {
                        self.error(
                            "a switch case has a different type from its scrutinee",
                            value.span,
                            "case and scrutinee types must match",
                        );
                        self.bailed = true;
                        return None;
                    }
                    let Some(ty) = self.scalar(&scrutinee.ty, scrutinee.span) else {
                        self.bailed = true;
                        return None;
                    };
                    if ty.is_float() {
                        self.bailed = true;
                        return None;
                    }
                    let Some(value) = self.const_of(&scrutinee.value) else {
                        self.bailed = true;
                        return None;
                    };
                    let key = ty.normalize(value).as_i64();
                    cases
                        .iter()
                        .find(|(candidate, _)| {
                            self.const_of(&candidate.value)
                                .map(|value| ty.normalize(value).as_i64())
                                == Some(key)
                        })
                        .map(|(_, label)| label.clone())
                        .unwrap_or_else(|| default.clone())
                }
            };

            previous = Some(block.label.clone());
            match positions.get(next_label.as_str()) {
                Some(&index) => current = index,
                None => {
                    self.bailed = true;
                    return None;
                }
            }
        }
    }

    fn const_of(&self, value: &ast::Value) -> Option<Const> {
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
                Some(Binding::Cell(index)) => self.cells.get(*index).copied().flatten(),
                _ => None,
            },
            _ => None,
        }
    }

    fn global_element(&self, name: &str, index: u64) -> Option<ast::Value> {
        match self.module.global(name)?.initializer.as_ref()? {
            ast::Value::Aggregate(items) => items.get(index as usize).map(|tv| tv.value.clone()),
            _ => None,
        }
    }

    fn run(&mut self) -> Program {
        let Some((entry, profile, qubits, results)) = self.entry() else {
            self.diagnostics.push(
                Diagnostic::error("no entry point found")
                    .with_code("QIR0201")
                    .note("expected a function with the \"entry_point\" attribute, or one named @main"),
            );
            return Program::new("empty", Profile::Unrestricted);
        };
        self.declare(qubits, results);

        let mut program = Program::new(entry.sig.name.clone(), profile);

        for (index, block) in entry.blocks.iter().enumerate() {
            self.block_ids
                .insert(block.label.clone(), BlockId(index as u32));
        }

        self.bind_params(entry);

        for block in &entry.blocks {
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
        unresolved.sort_by(|(a, x), (b, y)| (x.start, a).cmp(&(y.start, b)));
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
            ast::Terminator::Ret(Some(tv)) => {
                let Some(ty) = self.cast_scalar(&tv.ty, tv.span) else {
                    return Term::Unreachable;
                };
                match self.typed(&tv.value, ty, tv.span) {
                    Some(value) => Term::Ret(Some(value)),
                    None => Term::Unreachable,
                }
            }
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
                if cond.ty != ast::Ty::Int(1) {
                    self.error(
                        "a conditional branch needs an i1 condition",
                        cond.span,
                        "this condition is not a boolean",
                    );
                    return Term::Unreachable;
                }
                let Some(cond_operand) = self.typed(&cond.value, Scalar::Bool, cond.span) else {
                    return Term::Unreachable;
                };
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
                if let Some((value, _)) = cases.iter().find(|(value, _)| value.ty != scrutinee.ty) {
                    self.error(
                        "a switch case has a different type from its scrutinee",
                        value.span,
                        "case and scrutinee types must match",
                    );
                    return Term::Unreachable;
                }
                let Some(ty) = self.scalar(&scrutinee.ty, scrutinee.span) else {
                    return Term::Unreachable;
                };
                if ty.is_float() {
                    self.error(
                        "a switch needs an integer scrutinee",
                        scrutinee.span,
                        "floating-point switches are not valid LLVM IR",
                    );
                    return Term::Unreachable;
                }
                let Some(on) = self.typed(&scrutinee.value, ty, scrutinee.span) else {
                    return Term::Unreachable;
                };
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
                    let Some(key) = self
                        .typed(&value.value, ty, value.span)
                        .and_then(Operand::constant)
                        .map(Const::as_i64)
                    else {
                        continue;
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

            ast::InstKind::Binary { op, ty, lhs, rhs } => {
                let Some(ty) = self.scalar(ty, span) else {
                    return;
                };
                if self.validate_binary_type(*op, ty, span).is_none() {
                    return;
                }
                let (Some(lhs), Some(rhs)) = (self.typed(lhs, ty, span), self.typed(rhs, ty, span))
                else {
                    return;
                };
                if self
                    .validate_constant_binary(*op, ty, lhs.constant(), rhs.constant(), span)
                    .is_none()
                {
                    return;
                }
                self.assign(
                    inst.result.as_deref(),
                    ty,
                    Expr::Binary { op: *op, lhs, rhs },
                    span,
                )
            }

            ast::InstKind::ICmp { pred, ty, lhs, rhs } => {
                let Some(ty) = self.cast_scalar(ty, span) else {
                    return;
                };
                if ty.is_float() {
                    self.error(
                        "icmp needs integer or pointer operands",
                        span,
                        "this operand type is floating point",
                    );
                    return;
                }
                self.assign_pair(inst, ty, Scalar::Bool, lhs, rhs, |lhs, rhs| Expr::ICmp {
                    pred: *pred,
                    ty,
                    lhs,
                    rhs,
                })
            }

            ast::InstKind::FCmp { pred, ty, lhs, rhs } => {
                let Some(ty) = self.scalar(ty, span) else {
                    return;
                };
                if !ty.is_float() {
                    self.error(
                        "fcmp needs floating-point operands",
                        span,
                        "this operand type is not floating point",
                    );
                    return;
                }
                self.assign_pair(inst, ty, Scalar::Bool, lhs, rhs, |lhs, rhs| Expr::FCmp {
                    pred: *pred,
                    ty,
                    lhs,
                    rhs,
                })
            }

            ast::InstKind::Select {
                cond,
                if_true,
                if_false,
            } => {
                if cond.ty != ast::Ty::Int(1) {
                    self.error(
                        "select needs an i1 condition",
                        cond.span,
                        "this condition is not a boolean",
                    );
                    return;
                }
                if if_true.ty != if_false.ty {
                    self.error(
                        "select needs matching arm types",
                        span,
                        "the true and false values have different types",
                    );
                    return;
                }
                let Some(ty) = self.scalar(&if_true.ty, span) else {
                    return;
                };
                let (Some(c), Some(t), Some(f)) = (
                    self.typed(&cond.value, Scalar::Bool, span),
                    self.typed(&if_true.value, ty, span),
                    self.typed(&if_false.value, ty, span),
                ) else {
                    return;
                };
                self.assign(
                    inst.result.as_deref(),
                    ty,
                    Expr::Select {
                        cond: c,
                        if_true: t,
                        if_false: f,
                    },
                    span,
                );
            }

            ast::InstKind::Cast { op, operand, to } => {
                let Some((from, to_scalar)) = self.validate_cast_type(*op, &operand.ty, to, span)
                else {
                    return;
                };
                if let Some(qubit) = self.static_qubit(&operand.value)
                    && to.pointee_name() == Some("Qubit")
                {
                    self.bind(inst.result.as_deref(), Binding::Qubit(qubit));
                    return;
                }

                let kind = to.pointee_name().unwrap_or("ptr");
                if *op == ast::CastOp::IntToPtr
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

                let Some(value) = self.typed(&operand.value, from, span) else {
                    return;
                };
                if let Operand::Const(constant) = value
                    && self
                        .validate_constant_cast(*op, from, to_scalar, constant, span)
                        .is_none()
                {
                    return;
                }
                self.assign(
                    inst.result.as_deref(),
                    to_scalar,
                    Expr::Cast {
                        op: *op,
                        from,
                        operand: value,
                    },
                    span,
                );
            }

            ast::InstKind::Phi { ty, incoming } => {
                if self.flattening() {
                    match self.phi_choice(inst) {
                        Some(binding) => self.bind(inst.result.as_deref(), binding),
                        None => self.bailed = true,
                    }
                    return;
                }
                let Some(ty) = self.scalar(ty, span) else {
                    return;
                };

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
                        _ => match self.typed(value, ty, span) {
                            Some(operand) => operand,
                            None => continue,
                        },
                    };
                    lowered.push((block, operand));
                }
                self.assign(inst.result.as_deref(), ty, Expr::Phi(lowered), span);
            }

            ast::InstKind::Alloca { .. } => {
                if self.flattening() {
                    let cell = self.cells.len();
                    self.cells.push(None);
                    self.bind(inst.result.as_deref(), Binding::Cell(cell));
                    return;
                }

                let slot = SlotId(self.next_slot);
                self.next_slot += 1;
                self.bind(inst.result.as_deref(), Binding::Slot(slot));
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
                let Some(ty) = self.scalar(&value.ty, span) else {
                    return;
                };
                let Some(operand) = self.typed(&value.value, ty, span) else {
                    return;
                };
                self.ops.push(Op::Store {
                    slot,
                    value: operand,
                    span,
                });
            }

            ast::InstKind::Load { ty, ptr } => {
                if self.flattening() {
                    let binding = match &ptr.value {
                        ast::Value::Local(name) => self.env.get(name).cloned(),
                        _ => None,
                    };

                    match binding {
                        Some(Binding::Cell(index)) => {
                            if let Some(qubit) = self.env.get(&format!("cell#{index}")).cloned() {
                                self.bind(inst.result.as_deref(), qubit);
                                return;
                            }
                            match self.cells.get(index).copied().flatten() {
                                Some(value) => self.bind(
                                    inst.result.as_deref(),
                                    Binding::Value(Operand::Const(value)),
                                ),
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
                                    Some(found) => self.bind(inst.result.as_deref(), found),
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
                    let Some(ty) = self.scalar(ty, span) else {
                        return;
                    };
                    self.assign(inst.result.as_deref(), ty, Expr::Load(slot), span);
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

            ast::InstKind::ExtractValue { .. } | ast::InstKind::InsertValue { .. } => {
                self.error(
                    "aggregate value instructions are not supported",
                    span,
                    "dropping this value could change what the program computes",
                );
            }

            // Fences only constrain host-memory ordering. The lowered quantum IR has no
            // shared-memory operations to reorder, so a fence has no observable effect.
            ast::InstKind::Fence => {}

            ast::InstKind::Unsupported { opcode } => {
                self.error(
                    format!("unsupported instruction `{opcode}`"),
                    span,
                    "dropping it could change what the program computes",
                );
            }
        }
    }

    fn array_element(&self, base: &ast::Value, indices: &[ast::TypedValue]) -> Option<Binding> {
        let ast::Value::Global(name) = base else {
            return None;
        };
        self.module.global(name)?;

        let last = indices.last()?;
        let index = u64::try_from(self.const_of(&last.value)?.as_i64()).ok()?;

        Some(Binding::GlobalElement {
            name: name.clone(),
            index,
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

    fn phi_choice(&mut self, inst: &ast::Instruction) -> Option<Binding> {
        let ast::InstKind::Phi { ty, incoming } = &inst.kind else {
            return None;
        };
        if !matches!(ty, ast::Ty::Ptr(_)) {
            self.scalar(ty, inst.span)?;
        }
        let label = self.previous_label.as_deref()?;
        let (value, _) = incoming.iter().find(|(_, block)| block == label)?;
        self.binding_for(value)
    }

    fn assign(&mut self, result: Option<&str>, ty: Scalar, expr: Expr, span: Span) {
        if self.flattening()
            && let Some(value) = expr.fold(ty, |o| o.constant())
        {
            self.bind(result, Binding::Value(Operand::Const(value)));
            return;
        }

        let dest = match result.and_then(|name| self.forward.remove(name)) {
            Some((id, _)) => id,
            None => self.fresh_value(),
        };
        self.ops.push(Op::Assign {
            dest,
            ty,
            expr,
            span,
        });
        self.bind(result, Binding::Value(Operand::Value(dest)));
    }

    fn assign_pair(
        &mut self,
        inst: &ast::Instruction,
        operands: Scalar,
        ty: Scalar,
        lhs: &ast::Value,
        rhs: &ast::Value,
        make: impl FnOnce(Operand, Operand) -> Expr,
    ) {
        let span = inst.span;
        let (Some(l), Some(r)) = (
            self.typed(lhs, operands, span),
            self.typed(rhs, operands, span),
        ) else {
            return;
        };
        self.assign(inst.result.as_deref(), ty, make(l, r), span);
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

        if callee.starts_with("__quantum__qis__") {
            self.error(
                format!("unsupported quantum instruction `{callee}`"),
                span,
                "dropping it would change what the program computes",
            );
            return;
        }

        if self.module.declarations.iter().any(|d| d.name == callee) {
            self.error(
                format!("unsupported external function `{callee}`"),
                span,
                "dropping this call could change what the program computes",
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
                format!(
                    "`{}` recurses more than {MAX_INLINE_DEPTH} calls deep",
                    function.sig.name
                ),
                span,
                "while expanding this call",
            );
            return;
        }

        if self.flattening() {
            let scope = self.scope(function, call);
            let saved_env = mem::replace(&mut self.env, scope);
            let saved_previous = self.previous_label.take();
            self.inline_depth += 1;

            let returned = self.execute_function(function);

            self.inline_depth -= 1;
            self.env = saved_env;
            self.previous_label = saved_previous;

            if let Some(binding) = returned {
                self.bind(result, binding);
            }
            return;
        }

        if function.blocks.len() != 1 {
            self.error(
                format!(
                    "`{}` calls itself in a way that depends on a measurement",
                    function.sig.name
                ),
                span,
                "only calls in tail position can become a loop",
            );
            return;
        }

        let scope = self.scope(function, call);
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

        if let Some(binding) = returned {
            self.bind(result, binding);
        }
    }

    fn scope(&mut self, function: &ast::Function, call: &ast::Call) -> HashMap<String, Binding> {
        let mut scope = HashMap::new();
        for (param, arg) in function.sig.params.iter().zip(&call.args) {
            let Some(name) = &param.name else { continue };
            if let Some(binding) = self.binding_for(&arg.value) {
                scope.insert(name.clone(), binding);
            }
        }
        scope
    }

    fn binding_for(&mut self, value: &ast::Value) -> Option<Binding> {
        if let Some(qubit) = self.static_qubit(value) {
            return Some(Binding::Qubit(qubit));
        }

        if let Some(text) = self.resolve_label(value) {
            return Some(Binding::Bytes(text.into_bytes()));
        }

        if let ast::Value::Local(name) = value {
            return self.env.get(name).cloned();
        }

        self.operand(value, Span::DUMMY).map(Binding::Value)
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

            Intrinsic::Ising(axis) => self.lower_ising(call, axis, functor, span),

            Intrinsic::Measure { reset } => {
                let Some(qubit) = self.qubit_arg(call, 0, span) else {
                    return;
                };

                let result_id = if call.args.len() > 1 {
                    let Some(id) = self.result_arg(call, 1, span) else {
                        return;
                    };
                    id
                } else {
                    self.alloc_result()
                };

                self.bind(result, Binding::Result(result_id));

                self.ops.push(Op::Measure {
                    qubit,
                    result: result_id,
                    span,
                });
                if reset {
                    self.ops.push(Op::Reset { qubit, span });
                }
            }

            Intrinsic::Reset => {
                if let Some(qubit) = self.qubit_arg(call, 0, span) {
                    self.ops.push(Op::Reset { qubit, span });
                }
            }

            Intrinsic::ReadResult => {
                let Some(result_id) = self.result_arg(call, 0, span) else {
                    return;
                };
                self.assign(result, Scalar::Bool, Expr::ReadResult(result_id), span);
            }

            Intrinsic::ResultGetZero => self.bind(result, Binding::ResultConst(false)),

            Intrinsic::ResultGetOne => self.bind(result, Binding::ResultConst(true)),

            Intrinsic::ResultEqual => self.lower_result_equal(result, call, span),

            Intrinsic::RecordOutput(kind) => {
                let (result_id, value, count) = match kind {
                    OutputKind::Tuple | OutputKind::Array if call.args.len() > 1 => {
                        match self.operand(&call.args[0].value, call.args[0].span) {
                            Some(Operand::Const(count)) => (None, None, Some(count.as_i64())),
                            count => (None, count, None),
                        }
                    }
                    OutputKind::Tuple
                    | OutputKind::Array
                    | OutputKind::TupleEnd
                    | OutputKind::ArrayEnd => (None, None, None),
                    OutputKind::Result => (self.result_arg(call, 0, span), None, None),
                    OutputKind::Bool | OutputKind::Int | OutputKind::Double => {
                        let ty = match kind {
                            OutputKind::Bool => Scalar::Bool,
                            OutputKind::Double => Scalar::Double,
                            _ => Scalar::Int(64),
                        };
                        let value = call
                            .args
                            .first()
                            .and_then(|a| self.typed(&a.value, ty, a.span));
                        (None, value, None)
                    }
                };

                let label = call.args.last().and_then(|a| self.resolve_label(&a.value));

                self.ops.push(Op::RecordOutput {
                    kind,
                    result: result_id,
                    value,
                    count,
                    label,
                    span,
                });
            }

            Intrinsic::QubitAllocate => {
                let qubit = self.alloc_qubits(1);
                self.bind(result, Binding::Qubit(qubit));
            }

            Intrinsic::QubitAllocateArray => {
                let count = match call.args.first().and_then(|a| self.const_of(&a.value)) {
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
                self.bind(result, Binding::QubitArray { base, len: count });
            }

            Intrinsic::ArrayGetElementPtr => {
                let Some(ast::Value::Local(array_name)) = call.args.first().map(|a| &a.value)
                else {
                    return;
                };
                let Some(Binding::QubitArray { base, len }) = self.env.get(array_name).cloned()
                else {
                    return;
                };

                let index = match call.args.get(1).and_then(|a| self.const_of(&a.value)) {
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
                self.bind(result, Binding::Qubit(qubit));
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
        let resolve = |value: Option<&ast::Value>| {
            let value = value?;
            if let Some(id) = self.static_result(value) {
                return Some(Binding::Result(id));
            }
            if let ast::Value::Local(name) = value {
                return self.env.get(name).cloned();
            }
            None
        };

        let left = resolve(call.args.first().map(|a| &a.value));
        let right = resolve(call.args.get(1).map(|a| &a.value));

        let expr = match (left, right) {
            (Some(Binding::Result(id)), Some(Binding::ResultConst(expected)))
            | (Some(Binding::ResultConst(expected)), Some(Binding::Result(id))) => Expr::ICmp {
                pred: IntPredicate::Eq,
                ty: Scalar::Bool,
                lhs: self.read_result(id, span),
                rhs: Operand::Const(Const::Bool(expected)),
            },
            (Some(Binding::Result(a)), Some(Binding::Result(b))) => Expr::ICmp {
                pred: IntPredicate::Eq,
                ty: Scalar::Bool,
                lhs: self.read_result(a, span),
                rhs: self.read_result(b, span),
            },
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

        self.assign(result, Scalar::Bool, expr, span);
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

        let negate = functor.is_adjoint() && kind.param_count() == 1;
        let mut angles = Vec::new();
        for index in 0..params {
            let Some(angle) = self.angle_arg(call, index, negate, span) else {
                return;
            };
            angles.push(angle);
        }

        let Some(mut wires) =
            self.qubit_args(call, params..params + total_controls + targets, span)
        else {
            return;
        };
        if functor.is_controlled()
            && let Some(array) = call
                .args
                .get(params)
                .and_then(|a| self.control_array(&a.value))
        {
            wires.splice(0..1, array);
        }
        let target_wires = wires.split_off(wires.len() - targets);

        self.ops.push(Op::Gate(Gate {
            kind,
            controls: wires,
            targets: target_wires,
            params: angles,
            span,
        }));
    }

    fn lower_ising(&mut self, call: &ast::Call, axis: GateKind, functor: Functor, span: Span) {
        let Some(angle) = self.angle_arg(call, 0, functor.is_adjoint(), span) else {
            return;
        };

        let controls = usize::from(functor.is_controlled());
        let Some(mut wires) = self.qubit_args(call, 1..controls + 3, span) else {
            return;
        };
        if functor.is_controlled()
            && let Some(array) = call.args.get(1).and_then(|a| self.control_array(&a.value))
        {
            wires.splice(0..1, array);
        }
        if let Some(repeated) = wires
            .iter()
            .enumerate()
            .find_map(|(i, q)| wires[..i].contains(q).then_some(q))
        {
            let name = match axis {
                GateKind::Rx => "rxx",
                GateKind::Ry => "ryy",
                _ => "rzz",
            };
            self.error(
                format!("`{name}` uses q{} more than once", repeated.0),
                span,
                "each qubit may appear once",
            );
            return;
        }
        let pair = wires.split_off(wires.len() - 2);

        let gate = |kind, controls, targets, params| {
            Op::Gate(Gate {
                kind,
                controls,
                targets,
                params,
                span,
            })
        };
        let basis = |sign: f64| match axis {
            GateKind::Rx => Some((GateKind::H, vec![])),
            GateKind::Ry => Some((
                GateKind::Rx,
                vec![Operand::Const(Const::Float(sign * FRAC_PI_2))],
            )),
            _ => None,
        };

        if let Some((kind, params)) = basis(1.0) {
            for &qubit in &pair {
                self.ops
                    .push(gate(kind, vec![], vec![qubit], params.clone()));
            }
        }
        self.ops
            .push(gate(GateKind::X, vec![pair[0]], vec![pair[1]], vec![]));
        self.ops
            .push(gate(GateKind::Rz, wires, vec![pair[1]], vec![angle]));
        self.ops
            .push(gate(GateKind::X, vec![pair[0]], vec![pair[1]], vec![]));
        if let Some((kind, params)) = basis(-1.0) {
            for &qubit in &pair {
                self.ops
                    .push(gate(kind, vec![], vec![qubit], params.clone()));
            }
        }
    }

    fn angle_arg(
        &mut self,
        call: &ast::Call,
        index: usize,
        negate: bool,
        span: Span,
    ) -> Option<Operand> {
        let Some(arg) = call.args.get(index) else {
            self.error("missing rotation angle", span, "expected a double argument");
            return None;
        };
        let Some(operand) = self.operand(&arg.value, arg.span) else {
            self.error(
                "rotation angle is not a value",
                arg.span,
                "expected a number",
            );
            return None;
        };
        Some(if negate {
            self.negate(operand, span)
        } else {
            operand
        })
    }

    fn qubit_args(
        &mut self,
        call: &ast::Call,
        range: Range<usize>,
        span: Span,
    ) -> Option<Vec<QubitId>> {
        range.map(|i| self.qubit_arg(call, i, span)).collect()
    }

    fn negate(&mut self, operand: Operand, span: Span) -> Operand {
        if let Operand::Const(c) = operand {
            return Operand::Const(Const::Float(-c.as_f64()));
        }

        let dest = self.fresh_value();
        self.ops.push(Op::Assign {
            dest,
            ty: Scalar::Double,
            expr: Expr::Binary {
                op: BinOp::FSub,
                lhs: Operand::Const(Const::Float(0.0)),
                rhs: operand,
            },
            span,
        });
        Operand::Value(dest)
    }

    fn control_array(&self, value: &ast::Value) -> Option<Vec<QubitId>> {
        let ast::Value::Local(name) = value else {
            return None;
        };
        let Some(Binding::QubitArray { base, len }) = self.env.get(name) else {
            return None;
        };
        Some((0..*len).map(|i| QubitId(base.0 + i as u32)).collect())
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

        if let Some(index) = inttoptr(&arg.value).map(|(i, _)| i)
            && wire(index).is_none()
        {
            self.error(
                format!("qubit index {index} is out of range"),
                arg.span,
                format!("qubit ids must be below {MAX_WIRES}"),
            );
            return None;
        }

        self.diagnostics.push(
            Diagnostic::error("cannot resolve this operand to a qubit")
                .with_code("QIR0200")
                .primary(arg.span, "expected a static qubit reference")
                .note("use inttoptr, null, or a constant array index"),
        );
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

        if let Some(index) = inttoptr(&arg.value).map(|(i, _)| i)
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
            ast::Value::ConstExpr(_) => inttoptr(value)
                .filter(|(_, p)| matches!(p, Some("Qubit") | None))
                .and_then(|(i, _)| wire(i))
                .map(QubitId),
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
            ast::Value::ConstExpr(_) => inttoptr(value)
                .filter(|(_, p)| matches!(p, Some("Result") | None))
                .and_then(|(i, _)| wire(i))
                .map(ResultId),
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Result(r)) => Some(*r),
                Some(Binding::Id(id)) => Some(ResultId(*id)),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_label(&self, value: &ast::Value) -> Option<String> {
        match value {
            ast::Value::Null => None,
            ast::Value::Bytes(bytes) => Some(decode_label(bytes)),
            ast::Value::Global(name) => self.global_bytes(name).map(|b| decode_label(&b)),
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::GetElementPtr { ptr, indices, .. } => {
                    let label = self.resolve_label(&ptr.value)?;
                    let offset = match indices.last().map(|i| &i.value) {
                        Some(ast::Value::Int(n)) => usize::try_from(*n).unwrap_or(usize::MAX),
                        _ => 0,
                    };
                    Some(label.get(offset..).unwrap_or_default().to_string())
                }
                ast::ConstExpr::Cast { operand, .. } => self.resolve_label(&operand.value),
                _ => None,
            },
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Bytes(bytes)) => Some(decode_label(bytes)),
                _ => None,
            },
            _ => None,
        }
    }

    fn global_bytes(&self, name: &str) -> Option<Vec<u8>> {
        match self.module.global(name)?.initializer.as_ref()? {
            ast::Value::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        }
    }

    fn typed(&mut self, value: &ast::Value, ty: Scalar, span: Span) -> Option<Operand> {
        Some(match self.operand(value, span)? {
            Operand::Const(c) => Operand::Const(ty.normalize(c)),
            operand => operand,
        })
    }

    fn operand(&mut self, value: &ast::Value, span: Span) -> Option<Operand> {
        match value {
            ast::Value::Int(i) => Some(Operand::Const(Const::Int(*i as i64))),
            ast::Value::Float(f) => Some(Operand::Const(Const::Float(*f))),
            ast::Value::Bool(b) => Some(Operand::Const(Const::Bool(*b))),
            ast::Value::Null | ast::Value::ZeroInit => Some(Operand::Const(Const::Int(0))),
            ast::Value::Undef | ast::Value::Poison => {
                self.error(
                    "an undefined or poison value cannot be lowered safely",
                    span,
                    "choosing a concrete value here could change program behavior",
                );
                None
            }
            ast::Value::NoneValue => {
                self.error(
                    "`none` is not a scalar value",
                    span,
                    "expected an integer, floating point, or boolean operand",
                );
                None
            }
            ast::Value::Local(name) => match self.env.get(name) {
                Some(Binding::Value(operand)) => Some(*operand),
                Some(Binding::ResultConst(b)) => Some(Operand::Const(Const::Bool(*b))),
                Some(Binding::Qubit(q)) if self.flattening() => {
                    Some(Operand::Const(Const::Int(i64::from(q.0))))
                }
                Some(Binding::Result(id)) => {
                    let id = *id;
                    Some(self.read_result(id, span))
                }
                _ => {
                    self.error(format!("`%{name}` is not defined"), span, "unknown value");
                    None
                }
            },
            ast::Value::ConstExpr(expr) => match expr.as_ref() {
                ast::ConstExpr::Cast { op, operand, to } => {
                    let (from, to) = self.validate_cast_type(*op, &operand.ty, to, span)?;
                    let value = self
                        .operand(&operand.value, span)?
                        .constant()
                        .ok_or_else(|| {
                            self.error(
                                "a constant expression depends on a runtime value",
                                span,
                                "not a compile-time constant",
                            );
                        })
                        .ok()?;
                    self.validate_constant_cast(*op, from, to, value, span)?;
                    Some(Operand::Const(op.apply(from, to, value)))
                }
                ast::ConstExpr::Binary { op, lhs, rhs } => {
                    let ty = self.scalar(&lhs.ty, span)?;
                    if lhs.ty != rhs.ty {
                        self.error(
                            "a constant binary expression has mismatched operand types",
                            span,
                            "both operands must have the same LLVM type",
                        );
                        return None;
                    }
                    self.validate_binary_type(*op, ty, span)?;
                    let left = self
                        .operand(&lhs.value, span)?
                        .constant()
                        .map(|value| ty.normalize(value));
                    let right = self
                        .operand(&rhs.value, span)?
                        .constant()
                        .map(|value| ty.normalize(value));
                    let (Some(left), Some(right)) = (left, right) else {
                        self.error(
                            "a constant expression depends on a runtime value",
                            span,
                            "not a compile-time constant",
                        );
                        return None;
                    };
                    self.validate_constant_binary(*op, ty, Some(left), Some(right), span)?;
                    Some(Operand::Const(op.apply(ty, left, right)))
                }
                ast::ConstExpr::GetElementPtr { .. } => {
                    self.error(
                        "a pointer constant expression cannot be used as a scalar",
                        span,
                        "this pointer operation is not supported here",
                    );
                    None
                }
            },
            _ => None,
        }
    }
}

fn inttoptr(value: &ast::Value) -> Option<(i128, Option<&str>)> {
    let ast::Value::ConstExpr(expr) = value else {
        return None;
    };
    match expr.as_ref() {
        ast::ConstExpr::Cast {
            op: ast::CastOp::IntToPtr,
            operand,
            to,
        } => match operand.value {
            ast::Value::Int(i) => Some((i, to.pointee_name())),
            _ => None,
        },
        _ => None,
    }
}

fn wire(index: i128) -> Option<u32> {
    u32::try_from(index).ok().filter(|&i| i < MAX_WIRES)
}

fn signed_min(bits: u32) -> i64 {
    if bits >= 64 {
        i64::MIN
    } else {
        -(1_i64 << (bits - 1))
    }
}

fn decode_label(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn attribute_count(attrs: &[&ast::Attribute], keys: &[&str]) -> Result<u32, String> {
    for key in keys {
        if let Some(attribute) = attrs.iter().find(|a| a.key() == *key) {
            let Some(value) = attribute.value() else {
                return Err(format!("the `{key}` attribute needs an integer value"));
            };
            return value.parse::<u32>().map_err(|_| {
                format!("the `{key}` attribute value `{value}` does not fit in 32 bits")
            });
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::lower;
    use crate::diag::Severity;
    use crate::parse::parse_module;

    fn rejects_source(source: &str, expected: &str) {
        let (module, parse_diagnostics) = parse_module(source);
        assert!(
            parse_diagnostics.is_empty(),
            "test source failed to parse: {parse_diagnostics:?}"
        );
        let lowered = lower(&module);
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains(expected)),
            "missing `{expected}` error: {:?}",
            lowered
                .diagnostics
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }

    fn rejects(body: &str, expected: &str) {
        rejects_source(
            &format!("define void @main() {{\nentry:\n{body}\nret void\n}}"),
            expected,
        );
    }

    #[test]
    fn rejects_values_that_were_previously_silently_changed() {
        rejects("%x = add i64 undef, 1", "undefined or poison");
        rejects(
            "%x = add i64 udiv (i64 1, i64 0), 3",
            "integer division by zero",
        );
        rejects(
            "%x = add i8 sdiv (i8 128, i8 -1), 0",
            "signed integer division or remainder overflow",
        );
        rejects(
            "%x = icmp eq i128 18446744073709551616, 0",
            "integer type i128 is not supported",
        );
        rejects(
            "%x = fadd half 0xH3C00, 0xH3C00",
            "half-precision floating point is not supported",
        );
        rejects(
            "%x = fptosi double 0x7FF8000000000000 to i64",
            "conversion is out of range",
        );
        rejects(
            "%x = fptoui double -1.0 to i8",
            "conversion is out of range",
        );
        rejects(
            "%x = extractvalue { i64 } { i64 1 }, 0",
            "aggregate value instructions",
        );
        rejects(
            "%x = atomicrmw add ptr null, i64 1 monotonic",
            "unsupported instruction",
        );
    }

    #[test]
    fn rejects_malformed_scalar_operations() {
        rejects("%x = udiv i64 1, 0", "integer division by zero");
        rejects(
            "%x = sdiv i8 128, -1",
            "signed integer division or remainder overflow",
        );
        rejects(
            "%x = srem i8 128, -1",
            "signed integer division or remainder overflow",
        );
        rejects("%x = shl i8 1, 8", "oversized shift");
        rejects("%x = add double 1.0, 2.0", "integer binary opcode");
        rejects("%x = fadd i64 1, 2", "floating-point binary opcode");
        rejects("%x = icmp eq double 1.0, 2.0", "icmp needs");
        rejects("%x = fcmp oeq i64 1, 2", "fcmp needs");
        rejects("%x = select i8 1, i64 2, i64 3", "select needs an i1");
        rejects(
            "%x = select i1 true, i8 2, i16 3",
            "select needs matching arm types",
        );
    }

    #[test]
    fn rejects_invalid_cast_families_and_directions() {
        rejects("%x = trunc double 1.0 to i32", "invalid `trunc` cast");
        rejects("%x = fpext i32 1 to double", "invalid `fpext` cast");
        rejects("%x = trunc i8 1 to i16", "invalid `trunc` cast");
        rejects("%x = bitcast i32 1 to double", "invalid `bitcast` cast");
    }

    #[test]
    fn rejects_switch_cases_with_a_different_type() {
        rejects_source(
            "define void @main() {\nentry:\nswitch i8 0, label %done [ i16 0, label %done ]\ndone:\nret void\n}",
            "switch case has a different type",
        );
    }

    #[test]
    fn rejects_unsupported_flattened_phi_types() {
        rejects_source(
            "define i128 @main() {\nentry:\nbr label %join\njoin:\n%x = phi i128 [ 0, %entry ]\n%keep = add i64 1, 2\nret i128 %x\n}",
            "integer type i128 is not supported",
        );
    }

    #[test]
    fn rejects_unknown_declared_external_calls() {
        let source = "declare void @mystery()\ndefine void @main() {\nentry:\ncall void @mystery()\nret void\n}";
        let (module, parse_diagnostics) = parse_module(source);
        assert!(parse_diagnostics.is_empty());
        let lowered = lower(&module);
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error
                    && d.message.contains("unsupported external function"))
        );
    }

    #[test]
    fn evaluates_supported_constant_expressions() {
        let source =
            "define void @main() {\nentry:\n%x = add i64 add (i64 1, i64 2), 3\nret void\n}";
        let (module, parse_diagnostics) = parse_module(source);
        assert!(parse_diagnostics.is_empty());
        let lowered = lower(&module);
        assert!(
            lowered
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{:?}",
            lowered
                .diagnostics
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_invalid_declared_wire_counts() {
        for value in ["4294967296", "many"] {
            let source = format!(
                "define void @main() #0 {{\nentry:\nret void\n}}\nattributes #0 = {{ \"entry_point\" \"required_num_qubits\"=\"{value}\" }}"
            );
            let (module, parse_diagnostics) = parse_module(&source);
            assert!(parse_diagnostics.is_empty());
            let lowered = lower(&module);
            assert!(
                lowered
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == Some("QIR0202")),
                "{value}: {:?}",
                lowered.diagnostics
            );
        }
    }

    #[test]
    fn flattens_static_pointer_phis_before_scalar_validation() {
        let source = r#"%Qubit = type opaque
define void @main() #0 {
entry:
  %q0 = inttoptr i64 0 to %Qubit*
  br label %join
join:
  %q = phi %Qubit* [ %q0, %entry ]
  call void @__quantum__qis__h__body(%Qubit* %q)
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
attributes #0 = { "entry_point" "required_num_qubits"="1" }
"#;
        let (module, parse_diagnostics) = parse_module(source);
        assert!(parse_diagnostics.is_empty(), "{parse_diagnostics:?}");
        let lowered = lower(&module);
        assert!(
            lowered
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error),
            "{:?}",
            lowered.diagnostics
        );
        assert_eq!(lowered.program.gate_count(), 1);
    }
}
