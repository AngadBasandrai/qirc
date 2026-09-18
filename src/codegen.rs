use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ir::*;
use crate::simulator::matrix::Matrix2;

pub fn emit_qasm3(program: &Program) -> String {
    let mut out = String::new();

    out.push_str("OPENQASM 3.0;\n");
    out.push_str("include \"stdgates.inc\";\n\n");

    if program.num_qubits > 0 {
        writeln!(out, "qubit[{}] q;", program.num_qubits).unwrap();
    }
    if program.num_results > 0 {
        writeln!(out, "bit[{}] c;", program.num_results).unwrap();
    }
    out.push('\n');

    let branching = !program.is_straight_line();

    for block in &program.blocks {
        if branching {
            writeln!(out, "// block {}", block.label).unwrap();
        }

        for op in &block.ops {
            match op {
                Op::Gate(gate) => {
                    if let Some(line) = qasm_gate(gate) {
                        writeln!(out, "{line}").unwrap();
                    } else if gate.is_parameterised() && gate.constant_angle().is_none() {
                        writeln!(
                            out,
                            "// omitted {} on {}: angle is only known at run time",
                            gate.kind.name(),
                            qasm_wires(gate)
                        )
                        .unwrap();
                    } else {
                        writeln!(out, "// unsupported gate {}", gate.kind.name()).unwrap();
                    }
                }
                Op::Measure { qubit, result, .. } => {
                    writeln!(out, "c[{}] = measure q[{}];", result.0, qubit.0).unwrap();
                }
                Op::Reset { qubit, .. } => {
                    writeln!(out, "reset q[{}];", qubit.0).unwrap();
                }
                Op::Assign { .. }
                | Op::RecordOutput { .. }
                | Op::Store { .. }
                | Op::Message { .. } => {}
            }
        }

        if branching && let Term::CondBr { .. } = block.term {
            writeln!(out, "// conditional branch elided").unwrap();
        }
    }

    out
}

fn qasm_name(kind: GateKind, controls: usize) -> Option<String> {
    let base = match kind {
        GateKind::I => "id",
        GateKind::X => "x",
        GateKind::Y => "y",
        GateKind::Z => "z",
        GateKind::H => "h",
        GateKind::S => "s",
        GateKind::SDag => "sdg",
        GateKind::T => "t",
        GateKind::TDag => "tdg",
        GateKind::SX => "sx",
        GateKind::SXDag => "sxdg",
        GateKind::Rx => "rx",
        GateKind::Ry => "ry",
        GateKind::Rz => "rz",
        GateKind::R1 => "p",
        GateKind::Swap => "swap",
        GateKind::Unitary(_) => return None,
    };

    Some(match (controls, base) {
        (0, _) => base.to_string(),
        (1, "x") => "cx".into(),
        (1, "y") => "cy".into(),
        (1, "z") => "cz".into(),
        (1, "h") => "ch".into(),
        (1, "p") => "cp".into(),
        (1, "rx") => "crx".into(),
        (1, "ry") => "cry".into(),
        (1, "rz") => "crz".into(),
        (1, "swap") => "cswap".into(),
        (2, "x") => "ccx".into(),
        (2, "z") => "ccz".into(),
        (n, _) => format!("{}{base}", "ctrl @ ".repeat(n)),
    })
}

fn qasm_gate(gate: &Gate) -> Option<String> {
    if let GateKind::Unitary(m) = gate.kind {
        let matrix = Matrix2::from_ir(m);
        let (theta, phi, lambda) = zyz_angles(&matrix);
        let wires = qasm_wires(gate);
        return Some(format!("U({theta}, {phi}, {lambda}) {wires};"));
    }

    let name = qasm_name(gate.kind, gate.controls.len())?;
    let wires = qasm_wires(gate);

    if gate.params.is_empty() {
        return Some(format!("{name} {wires};"));
    }

    let mut params = Vec::with_capacity(gate.params.len());
    for parameter in &gate.params {
        params.push(format!("{}", parameter.constant()?.as_f64()));
    }

    Some(format!("{name}({}) {wires};", params.join(", ")))
}

fn qasm_wires(gate: &Gate) -> String {
    gate.controls
        .iter()
        .chain(gate.targets.iter())
        .map(|q| format!("q[{}]", q.0))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn zyz_angles(m: &Matrix2) -> (f64, f64, f64) {
    let a = m.a;
    let b = m.b;
    let c = m.c;
    let d = m.d;

    let theta = 2.0 * a.norm().clamp(-1.0, 1.0).acos();

    if b.norm() < 1e-12 && c.norm() < 1e-12 {
        let phase = (d / a).arg();
        return (0.0, phase, 0.0);
    }

    if a.norm() < 1e-12 {
        return (std::f64::consts::PI, (c / (-b)).arg(), 0.0);
    }

    let phi = (c / a).arg();
    let lambda = ((-b) / a).arg();

    (theta, phi, lambda)
}

pub fn emit_qir(program: &Program) -> String {
    QirEmitter::new(program).emit()
}

struct QirEmitter<'a> {
    program: &'a Program,
    out: String,
    declarations: Vec<(String, String)>,
    values: HashMap<ValueId, (String, &'static str)>,
    slot_types: HashMap<SlotId, &'static str>,
}

impl<'a> QirEmitter<'a> {
    fn new(program: &'a Program) -> Self {
        Self {
            program,
            out: String::new(),
            declarations: Vec::new(),
            values: HashMap::new(),
            slot_types: HashMap::new(),
        }
    }

    fn declare(&mut self, name: &str, params: &str) {
        if !self.declarations.iter().any(|(n, _)| n == name) {
            self.declarations
                .push((name.to_string(), params.to_string()));
        }
    }

    fn operand(&self, operand: &Operand) -> (String, &'static str) {
        match operand {
            Operand::Const(Const::Bool(b)) => (b.to_string(), "i1"),
            Operand::Const(Const::Int(i)) => (i.to_string(), "i64"),
            Operand::Const(Const::Float(f)) => (format_double(*f), "double"),
            Operand::Value(id) => self
                .values
                .get(id)
                .cloned()
                .unwrap_or_else(|| ("0".to_string(), "i64")),
        }
    }

    fn emit(mut self) -> String {
        let program = self.program;

        writeln!(self.out, "; ModuleID = '{}'", program.name).unwrap();
        writeln!(self.out, "source_filename = \"{}\"", program.name).unwrap();
        self.out.push('\n');
        self.out
            .push_str("%Result = type opaque\n%Qubit = type opaque\n\n");

        for block in &program.blocks {
            for op in &block.ops {
                if let Op::Store { slot, value, .. } = op {
                    let (_, ty) = self.operand(value);
                    self.slot_types.insert(*slot, ty);
                }
            }
        }

        let mut body = String::new();
        for (index, block) in program.blocks.iter().enumerate() {
            if index == 0 {
                writeln!(body, "{}:", block.label).unwrap();
                for slot in 0..program.num_slots {
                    let ty = self.slot_types.get(&SlotId(slot)).copied().unwrap_or("i64");
                    writeln!(body, "  %slot{slot} = alloca {ty}").unwrap();
                }
                for op in &block.ops {
                    self.emit_op(op, &mut body);
                }
                self.emit_terminator(&block.term, &mut body);
            } else {
                self.emit_block(block, &mut body);
            }
        }

        writeln!(self.out, "define void @{}() #0 {{", program.name).unwrap();
        self.out.push_str(&body);
        self.out.push_str("}\n\n");

        for (name, params) in &self.declarations {
            writeln!(self.out, "declare {name}({params})").unwrap();
        }

        self.out.push('\n');
        writeln!(
            self.out,
            "attributes #0 = {{ \"entry_point\" \"output_labeling_schema\" \"qir_profiles\"=\"{}\" \"required_num_qubits\"=\"{}\" \"required_num_results\"=\"{}\" }}",
            program.profile.name(),
            program.num_qubits,
            program.num_results
        ).unwrap();

        self.out.push('\n');
        self.out
            .push_str("!llvm.module.flags = !{!0, !1, !2, !3}\n\n");
        self.out
            .push_str("!0 = !{i32 1, !\"qir_major_version\", i32 1}\n");
        self.out
            .push_str("!1 = !{i32 7, !\"qir_minor_version\", i32 0}\n");
        self.out
            .push_str("!2 = !{i32 1, !\"dynamic_qubit_management\", i1 false}\n");
        self.out
            .push_str("!3 = !{i32 1, !\"dynamic_result_management\", i1 false}\n");

        self.out
    }

    fn emit_block(&mut self, block: &Block, body: &mut String) {
        writeln!(body, "{}:", block.label).unwrap();

        for op in &block.ops {
            self.emit_op(op, body);
        }

        self.emit_terminator(&block.term, body);
    }

    fn emit_op(&mut self, op: &Op, body: &mut String) {
        match op {
            Op::Gate(gate) => self.emit_gate(gate, body),

            Op::Measure { qubit, result, .. } => {
                writeln!(
                    body,
                    "  call void @__quantum__qis__mz__body({}, {})",
                    qubit_ref(*qubit),
                    result_ref(*result)
                )
                .unwrap();
                self.declare(
                    "void @__quantum__qis__mz__body",
                    "%Qubit*, %Result* writeonly",
                );
            }

            Op::Reset { qubit, .. } => {
                writeln!(
                    body,
                    "  call void @__quantum__qis__reset__body({})",
                    qubit_ref(*qubit)
                )
                .unwrap();
                self.declare("void @__quantum__qis__reset__body", "%Qubit*");
            }

            Op::RecordOutput {
                kind,
                result,
                count,
                ..
            } => {
                let name = match kind {
                    OutputKind::Result => "__quantum__rt__result_record_output",
                    OutputKind::Tuple => "__quantum__rt__tuple_record_output",
                    OutputKind::Array => "__quantum__rt__array_record_output",
                    OutputKind::Bool => "__quantum__rt__bool_record_output",
                    OutputKind::Int => "__quantum__rt__int_record_output",
                    OutputKind::Double => "__quantum__rt__double_record_output",
                };

                match result {
                    Some(r) => {
                        writeln!(body, "  call void @{name}({}, i8* null)", result_ref(*r))
                            .unwrap();
                        self.declare(&format!("void @{name}"), "%Result*, i8*");
                    }
                    None => {
                        writeln!(
                            body,
                            "  call void @{name}(i64 {}, i8* null)",
                            count.unwrap_or(0)
                        )
                        .unwrap();
                        self.declare(&format!("void @{name}"), "i64, i8*");
                    }
                }
            }

            Op::Assign { dest, expr, .. } => self.emit_assign(*dest, expr, body),

            Op::Store { slot, value, .. } => {
                let (rendered, ty) = self.operand(value);
                writeln!(body, "  store {ty} {rendered}, ptr %slot{}", slot.0).unwrap();
            }

            Op::Message { .. } => {}
        }
    }

    fn emit_gate(&mut self, gate: &Gate, body: &mut String) {
        if let GateKind::Unitary(matrix) = gate.kind {
            debug_assert!(gate.controls.is_empty());
            debug_assert_eq!(gate.targets.len(), 1);

            let (theta, phi, lambda) = zyz_angles(&Matrix2::from_ir(matrix));
            for (kind, angle) in [
                (GateKind::Rz, lambda),
                (GateKind::Ry, theta),
                (GateKind::Rz, phi),
            ] {
                let rotation = Gate {
                    kind,
                    controls: gate.controls.clone(),
                    targets: gate.targets.clone(),
                    params: vec![Operand::Const(Const::Float(angle))],
                    span: gate.span,
                };
                self.emit_native_gate(&rotation, body);
            }
            return;
        }

        self.emit_native_gate(gate, body);
    }

    fn emit_native_gate(&mut self, gate: &Gate, body: &mut String) {
        let (name, params, args) = self.gate_call(gate);
        writeln!(body, "  call void @{name}({args})").unwrap();
        self.declare(&format!("void @{name}"), &params);
    }

    fn emit_assign(&mut self, dest: ValueId, expr: &Expr, body: &mut String) {
        let name = format!("%v{}", dest.0);

        match expr {
            Expr::Const(c) => {
                let rendered = self.operand(&Operand::Const(*c));
                self.values.insert(dest, rendered);
            }

            Expr::Copy(operand) => {
                let rendered = self.operand(operand);
                self.values.insert(dest, rendered);
            }

            Expr::Load(slot) => {
                let ty = self.slot_types.get(slot).copied().unwrap_or("i64");
                writeln!(body, "  {name} = load {ty}, ptr %slot{}", slot.0).unwrap();
                self.values.insert(dest, (name, ty));
            }

            Expr::ReadResult(result) => {
                writeln!(
                    body,
                    "  {name} = call i1 @__quantum__qis__read_result__body({})",
                    result_ref(*result)
                )
                .unwrap();
                self.declare("i1 @__quantum__qis__read_result__body", "%Result*");
                self.values.insert(dest, (name, "i1"));
            }

            Expr::Binary { op, lhs, rhs } => {
                let (a, ty) = self.operand(lhs);
                let (b, _) = self.operand(rhs);
                let ty = if op.is_float() { "double" } else { ty };
                writeln!(body, "  {name} = {} {ty} {a}, {b}", op.keyword()).unwrap();
                self.values.insert(dest, (name, ty));
            }

            Expr::ICmp { pred, lhs, rhs } => {
                let (a, ty) = self.operand(lhs);
                let (b, _) = self.operand(rhs);
                writeln!(body, "  {name} = icmp {} {ty} {a}, {b}", pred.keyword()).unwrap();
                self.values.insert(dest, (name, "i1"));
            }

            Expr::FCmp { pred, lhs, rhs } => {
                let (a, _) = self.operand(lhs);
                let (b, _) = self.operand(rhs);
                writeln!(body, "  {name} = fcmp {} double {a}, {b}", pred.keyword()).unwrap();
                self.values.insert(dest, (name, "i1"));
            }

            Expr::Select {
                cond,
                if_true,
                if_false,
            } => {
                let (c, _) = self.operand(cond);
                let (a, ty) = self.operand(if_true);
                let (b, _) = self.operand(if_false);
                writeln!(body, "  {name} = select i1 {c}, {ty} {a}, {ty} {b}").unwrap();
                self.values.insert(dest, (name, ty));
            }

            Expr::Cast { op, operand } => {
                let (value, from) = self.operand(operand);
                let to = match op {
                    CastOp::SIToFP | CastOp::UIToFP | CastOp::FPExt => "double",
                    CastOp::FPToSI | CastOp::FPToUI | CastOp::ZExt | CastOp::SExt => "i64",
                    CastOp::Trunc => "i1",
                    _ => from,
                };
                writeln!(body, "  {name} = {} {from} {value} to {to}", op.keyword()).unwrap();
                self.values.insert(dest, (name, to));
            }

            Expr::Phi(incoming) => {
                let rendered: Vec<(String, String)> = incoming
                    .iter()
                    .map(|(block, operand)| {
                        let (value, ty) = self.operand(operand);
                        (
                            format!("[ {value}, %{} ]", self.program.block(*block).label),
                            ty.to_string(),
                        )
                    })
                    .collect();

                let ty = rendered
                    .first()
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or_else(|| "i64".into());
                let parts: Vec<String> = rendered.into_iter().map(|(text, _)| text).collect();

                writeln!(body, "  {name} = phi {ty} {}", parts.join(", ")).unwrap();
                let leaked: &'static str = if ty == "i1" {
                    "i1"
                } else if ty == "double" {
                    "double"
                } else {
                    "i64"
                };
                self.values.insert(dest, (name, leaked));
            }
        }
    }

    fn emit_terminator(&mut self, term: &Term, body: &mut String) {
        match term {
            Term::Ret(_) => {
                writeln!(body, "  ret void").unwrap();
            }
            Term::Unreachable => {
                writeln!(body, "  unreachable").unwrap();
            }
            Term::Br(target) => {
                writeln!(body, "  br label %{}", self.program.block(*target).label).unwrap();
            }
            Term::CondBr {
                cond,
                if_true,
                if_false,
            } => {
                let (value, ty) = self.operand(cond);
                let condition = if ty == "i1" {
                    value
                } else {
                    format!("icmp ne {ty} {value}, 0")
                };
                writeln!(
                    body,
                    "  br i1 {condition}, label %{}, label %{}",
                    self.program.block(*if_true).label,
                    self.program.block(*if_false).label
                )
                .unwrap();
            }
            Term::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let (value, ty) = self.operand(scrutinee);
                writeln!(
                    body,
                    "  switch {ty} {value}, label %{} [",
                    self.program.block(*default).label
                )
                .unwrap();
                for (key, target) in cases {
                    writeln!(
                        body,
                        "    {ty} {key}, label %{}",
                        self.program.block(*target).label
                    )
                    .unwrap();
                }
                writeln!(body, "  ]").unwrap();
            }
        }
    }

    fn gate_call(&self, gate: &Gate) -> (&'static str, String, String) {
        let name: &'static str = match (gate.kind, gate.controls.len()) {
            (GateKind::I, 0) => "__quantum__qis__i__body",
            (GateKind::X, 0) => "__quantum__qis__x__body",
            (GateKind::Y, 0) => "__quantum__qis__y__body",
            (GateKind::Z, 0) => "__quantum__qis__z__body",
            (GateKind::H, 0) => "__quantum__qis__h__body",
            (GateKind::S, 0) => "__quantum__qis__s__body",
            (GateKind::SDag, 0) => "__quantum__qis__s__adj",
            (GateKind::T, 0) => "__quantum__qis__t__body",
            (GateKind::TDag, 0) => "__quantum__qis__t__adj",
            (GateKind::SX, 0) => "__quantum__qis__sx__body",
            (GateKind::SXDag, 0) => "__quantum__qis__sx__adj",
            (GateKind::Rx, 0) => "__quantum__qis__rx__body",
            (GateKind::Ry, 0) => "__quantum__qis__ry__body",
            (GateKind::Rz, 0) => "__quantum__qis__rz__body",
            (GateKind::R1, 0) => "__quantum__qis__r1__body",
            (GateKind::Swap, 0) => "__quantum__qis__swap__body",
            (GateKind::X, 1) => "__quantum__qis__cx__body",
            (GateKind::Y, 1) => "__quantum__qis__cy__body",
            (GateKind::Z, 1) => "__quantum__qis__cz__body",
            (GateKind::H, 1) => "__quantum__qis__ch__body",
            (GateKind::Rx, 1) => "__quantum__qis__crx__body",
            (GateKind::Ry, 1) => "__quantum__qis__cry__body",
            (GateKind::Rz, 1) => "__quantum__qis__crz__body",
            (GateKind::R1, 1) => "__quantum__qis__cr1__body",
            (GateKind::X, 2) => "__quantum__qis__ccx__body",
            (GateKind::Z, 2) => "__quantum__qis__ccz__body",
            (GateKind::Swap, 1) => "__quantum__qis__cswap__body",
            (GateKind::Unitary(_), _) => {
                unreachable!("fused unitaries must be synthesized before QIR emission")
            }
            _ => "__quantum__qis__unitary__body",
        };

        let mut args = Vec::new();
        let mut types = Vec::new();

        for param in &gate.params {
            let (value, ty) = self.operand(param);
            debug_assert_eq!(ty, "double", "QIS rotation parameters must be doubles");
            args.push(format!("double {value}"));
            types.push("double".to_string());
        }

        for qubit in gate.controls.iter().chain(gate.targets.iter()) {
            args.push(qubit_ref(*qubit));
            types.push("%Qubit*".to_string());
        }

        (name, types.join(", "), args.join(", "))
    }
}

fn format_double(value: f64) -> String {
    format!("0x{:016X}", value.to_bits())
}

fn qubit_ref(qubit: QubitId) -> String {
    format!("%Qubit* inttoptr (i64 {} to %Qubit*)", qubit.0)
}

fn result_ref(result: ResultId) -> String {
    format!("%Result* inttoptr (i64 {} to %Result*)", result.0)
}

pub fn emit_json(program: &Program) -> String {
    let mut out = String::new();

    out.push_str("{\n");
    writeln!(out, "  \"name\": {:?},", program.name).unwrap();
    writeln!(out, "  \"profile\": {:?},", program.profile.name()).unwrap();
    writeln!(out, "  \"qubits\": {},", program.num_qubits).unwrap();
    writeln!(out, "  \"results\": {},", program.num_results).unwrap();
    writeln!(out, "  \"depth\": {},", program.depth()).unwrap();
    writeln!(out, "  \"gateCount\": {},", program.gate_count()).unwrap();
    out.push_str("  \"blocks\": [\n");

    for (index, block) in program.blocks.iter().enumerate() {
        out.push_str("    {\n");
        writeln!(out, "      \"label\": {:?},", block.label).unwrap();
        out.push_str("      \"ops\": [\n");

        let lines: Vec<String> = block.ops.iter().filter_map(json_op).collect();
        out.push_str(&lines.join(",\n"));
        if !lines.is_empty() {
            out.push('\n');
        }

        out.push_str("      ],\n");
        writeln!(
            out,
            "      \"terminator\": {:?}",
            json_term(program, &block.term)
        )
        .unwrap();
        out.push_str("    }");
        if index + 1 < program.blocks.len() {
            out.push(',');
        }
        out.push('\n');
    }

    out.push_str("  ]\n}\n");
    out
}

fn json_op(op: &Op) -> Option<String> {
    match op {
        Op::Gate(gate) => {
            let params: Vec<String> = gate
                .params
                .iter()
                .map(|p| match p.constant() {
                    Some(c) => format!("{}", c.as_f64()),
                    None => "null".into(),
                })
                .collect();

            let controls: Vec<String> = gate.controls.iter().map(|q| q.0.to_string()).collect();
            let targets: Vec<String> = gate.targets.iter().map(|q| q.0.to_string()).collect();

            Some(format!(
                "        {{ \"op\": \"gate\", \"name\": {:?}, \"controls\": [{}], \"targets\": [{}], \"params\": [{}] }}",
                gate.kind.name(),
                controls.join(", "),
                targets.join(", "),
                params.join(", ")
            ))
        }
        Op::Measure { qubit, result, .. } => Some(format!(
            "        {{ \"op\": \"measure\", \"qubit\": {}, \"result\": {} }}",
            qubit.0, result.0
        )),
        Op::Reset { qubit, .. } => Some(format!(
            "        {{ \"op\": \"reset\", \"qubit\": {} }}",
            qubit.0
        )),
        Op::RecordOutput { kind, result, .. } => Some(format!(
            "        {{ \"op\": \"record\", \"kind\": {:?}, \"result\": {} }}",
            format!("{kind:?}").to_lowercase(),
            result.map(|r| r.0.to_string()).unwrap_or("null".into())
        )),
        Op::Assign { .. } | Op::Store { .. } | Op::Message { .. } => None,
    }
}

fn json_term(program: &Program, term: &Term) -> String {
    match term {
        Term::Ret(_) => "ret".into(),
        Term::Unreachable => "unreachable".into(),
        Term::Br(target) => format!("br {}", program.block(*target).label),
        Term::CondBr {
            if_true, if_false, ..
        } => format!(
            "condbr {} {}",
            program.block(*if_true).label,
            program.block(*if_false).label
        ),
        Term::Switch { default, .. } => format!("switch {}", program.block(*default).label),
    }
}

pub fn emit_circuit(program: &Program) -> String {
    if program.num_qubits == 0 {
        return "(no qubits)\n".into();
    }

    let width = program.num_qubits as usize;
    let mut wires: Vec<String> = (0..width).map(|i| format!("q{i}: ")).collect();
    let label_width = wires.iter().map(|w| w.len()).max().unwrap_or(0);

    for wire in &mut wires {
        while wire.len() < label_width {
            wire.insert(wire.len() - 2, ' ');
        }
    }

    let mut columns: Vec<Vec<String>> = Vec::new();

    for block in &program.blocks {
        for op in &block.ops {
            let mut column = vec!["-".to_string(); width];

            match op {
                Op::Gate(gate) => {
                    for control in &gate.controls {
                        if let Some(slot) = column.get_mut(control.index()) {
                            *slot = "*".into();
                        }
                    }
                    let symbol = gate_symbol(gate);
                    for target in &gate.targets {
                        if let Some(slot) = column.get_mut(target.index()) {
                            *slot = symbol.clone();
                        }
                    }
                    fill_vertical(&mut column, gate);
                }
                Op::Measure { qubit, .. } => {
                    if let Some(slot) = column.get_mut(qubit.index()) {
                        *slot = "M".into();
                    }
                }
                Op::Reset { qubit, .. } => {
                    if let Some(slot) = column.get_mut(qubit.index()) {
                        *slot = "0".into();
                    }
                }
                _ => continue,
            }

            columns.push(column);
        }
    }

    let cell_width = columns
        .iter()
        .flatten()
        .map(|c| c.len())
        .max()
        .unwrap_or(1)
        .max(1);

    for column in &columns {
        for (wire_index, cell) in column.iter().enumerate() {
            let filler = if cell == "-" || cell == "|" {
                cell
            } else {
                "-"
            };
            let padded = center(cell, cell_width, filler);
            wires[wire_index].push_str(&padded);
            wires[wire_index].push('-');
        }
    }

    let mut out = String::new();
    for wire in wires {
        out.push_str(&wire);
        out.push('\n');
    }
    out
}

fn fill_vertical(column: &mut [String], gate: &Gate) {
    let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
    let (Some(low), Some(high)) = (wires.iter().min(), wires.iter().max()) else {
        return;
    };

    for index in (*low + 1)..*high {
        if column.get(index).map(|c| c == "-").unwrap_or(false) {
            column[index] = "|".into();
        }
    }
}

fn gate_symbol(gate: &Gate) -> String {
    match gate.kind {
        GateKind::X if !gate.controls.is_empty() => "+".into(),
        GateKind::Swap => "x".into(),
        GateKind::Unitary(_) => "U".into(),
        other => {
            let name = other.name();
            let mut symbol = name.to_uppercase();
            if other.param_count() > 0
                && let Some(angle) = gate.constant_angle()
            {
                symbol = format!("{}({:.2})", name.to_uppercase(), angle);
            }
            symbol
        }
    }
}

fn center(text: &str, width: usize, filler: &str) -> String {
    if text.len() >= width {
        return text.to_string();
    }

    let total = width - text.len();
    let left = total / 2;
    let right = total - left;

    format!("{}{}{}", filler.repeat(left), text, filler.repeat(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_zyz_eq(original: Matrix2) {
        let (theta, phi, lambda) = zyz_angles(&original);
        let reconstructed = Matrix2::rz(phi)
            .multiply(Matrix2::ry(theta))
            .multiply(Matrix2::rz(lambda));
        let pairs = [
            (original.a, reconstructed.a),
            (original.b, reconstructed.b),
            (original.c, reconstructed.c),
            (original.d, reconstructed.d),
        ];
        let (expected, actual) = pairs
            .iter()
            .find(|(_, actual)| actual.norm() > 1e-12)
            .expect("a unitary has a nonzero matrix element");
        let phase = expected / actual;

        for (expected, actual) in pairs {
            assert!(
                (expected - phase * actual).norm() < 1e-12,
                "ZYZ reconstruction differs: {original:?} vs {reconstructed:?}"
            );
        }
    }

    #[test]
    fn zyz_roundtrip() {
        for matrix in [
            Matrix2::identity(),
            Matrix2::x(),
            Matrix2::y(),
            Matrix2::z(),
            Matrix2::h(),
            Matrix2::sx(),
            Matrix2::rz(0.73)
                .multiply(Matrix2::ry(-1.21))
                .multiply(Matrix2::rz(2.44)),
            Matrix2::rx(0.91).multiply(Matrix2::phase(-0.37)),
        ] {
            assert_zyz_eq(matrix);
        }
    }
}
