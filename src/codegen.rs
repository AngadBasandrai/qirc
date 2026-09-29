use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::fmt::{self, Write as _};
use std::mem;

use crate::ir::*;
use crate::json;
use crate::simulator::matrix::Matrix2;
use crate::synth::Synth;
use crate::transpile::{self, GateSet};
use crate::verify;

pub fn emit_qasm3(program: &Program) -> Result<String, String> {
    QasmWriter::new(program)?.emit()
}

struct QasmWriter<'a> {
    program: &'a Program,
    out: String,
    types: Types,
    reads: HashMap<ValueId, ResultId>,
    incoming: HashMap<BlockId, Vec<(ValueId, Operand)>>,
    post: Option<HashMap<BlockId, HashSet<BlockId>>>,
    outputs: HashMap<(BlockId, usize), usize>,
    written: HashSet<BlockId>,
    repeated: bool,
}

impl<'a> QasmWriter<'a> {
    fn new(program: &'a Program) -> Result<Self, String> {
        let mut operands = Vec::new();
        let mut incoming: HashMap<BlockId, Vec<(ValueId, Operand)>> = HashMap::new();
        let mut measured: HashMap<ResultId, usize> = HashMap::new();

        for op in program.ops() {
            operands.extend(op.operands());
            match op {
                Op::Assign {
                    dest,
                    expr: Expr::Phi(edges),
                    ..
                } => {
                    for (block, operand) in edges {
                        incoming.entry(*block).or_default().push((*dest, *operand));
                    }
                }
                Op::Assign {
                    expr: Expr::Const(c),
                    ..
                } => operands.push(Operand::Const(*c)),
                Op::Measure { result, .. } => *measured.entry(*result).or_default() += 1,
                _ => {}
            }
        }

        if operands
            .iter()
            .any(|o| matches!(o, Operand::Const(Const::Float(f)) if !f.is_finite()))
        {
            return Err("an infinite or NaN constant cannot be written as OpenQASM 3".into());
        }

        let has_switch = program
            .blocks
            .iter()
            .any(|b| matches!(b.term, Term::Switch { .. }));
        let post = if has_switch {
            None
        } else {
            post_dominators(program)
        };

        let looping = looping_blocks(program);
        if program.blocks.iter().any(|block| {
            looping.contains(&block.id)
                && block.ops.iter().any(|op| {
                    matches!(
                        op,
                        Op::RecordOutput {
                            value: Some(_),
                            kind: OutputKind::Bool | OutputKind::Int | OutputKind::Double,
                            ..
                        }
                    )
                })
        }) {
            return Err("a value recorded inside a loop cannot be written as OpenQASM 3".into());
        }

        let idom = verify::immediate_dominators(program);
        let mut sites = HashMap::new();
        let mut reads = HashMap::new();
        for block in &program.blocks {
            for (index, op) in block.ops.iter().enumerate() {
                match op {
                    Op::Measure { result, .. } => {
                        sites.insert(*result, (block.id, index));
                    }
                    Op::Assign {
                        dest,
                        expr: Expr::ReadResult(result),
                        ..
                    } if measured.get(result) == Some(&1) => {
                        let before = match sites.get(result) {
                            Some(&(site, _)) if site != block.id => {
                                looping.is_empty() && verify::dominates(&idom, site, block.id)
                            }
                            Some(&(_, at)) => at < index,
                            None => false,
                        };
                        if before {
                            reads.insert(*dest, *result);
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(Self {
            program,
            out: String::new(),
            types: Types::new(program),
            reads,
            incoming,
            post,
            outputs: HashMap::new(),
            written: HashSet::new(),
            repeated: false,
        })
    }

    fn emit(mut self) -> Result<String, String> {
        let program = self.program;
        self.out
            .push_str("OPENQASM 3.0;\ninclude \"stdgates.inc\";\n");
        if program.gates().any(|g| g.kind == GateKind::SXDag) {
            self.out.push_str("gate sxdg a { inv @ sx a; }\n");
        }
        if program
            .gates()
            .any(|g| g.kind == GateKind::Z && g.controls.len() == 2)
        {
            self.out
                .push_str("gate ccz a, b, c { h c; ccx a, b, c; h c; }\n");
        }
        self.out.push('\n');
        if program.num_qubits > 0 {
            writeln!(self.out, "qubit[{}] q;", program.num_qubits).unwrap();
        }
        if program.num_results > 0 {
            writeln!(self.out, "bit[{}] c;", program.num_results).unwrap();
        }
        self.declare();
        self.out.push('\n');

        if program.blocks.is_empty() {
            return Ok(self.out);
        }
        let start = self.out.len();
        if self.post.is_some() {
            self.region(program.entry, None, 0)?;
        }
        if self.post.is_none() || self.repeated {
            self.out.truncate(start);
            self.dispatch()?;
        }
        Ok(self.out)
    }

    fn declare(&mut self) {
        for op in self.program.ops() {
            let Op::Assign { dest, ty, expr, .. } = op else {
                continue;
            };
            if self.reads.contains_key(dest) {
                continue;
            }
            declare_var(&mut self.out, *ty, format_args!("v{}", dest.0));
            if matches!(expr, Expr::Phi(_)) {
                declare_var(&mut self.out, *ty, format_args!("v{}_in", dest.0));
            }
        }

        let mut slots: Vec<_> = self.types.slots.iter().collect();
        slots.sort_by_key(|(slot, _)| slot.0);
        for (slot, ty) in slots {
            declare_var(&mut self.out, *ty, format_args!("s{}", slot.0));
        }

        for block in &self.program.blocks {
            for (index, op) in block.ops.iter().enumerate() {
                if let Op::RecordOutput {
                    kind: kind @ (OutputKind::Bool | OutputKind::Int | OutputKind::Double),
                    value: Some(_),
                    ..
                } = op
                {
                    let number = self.outputs.len();
                    self.outputs.insert((block.id, index), number);
                    let ty = match kind {
                        OutputKind::Bool => "bool",
                        OutputKind::Double => "float[64]",
                        _ => "int[64]",
                    };
                    writeln!(self.out, "output {ty} out{number};").unwrap();
                }
            }
        }
    }

    fn region(
        &mut self,
        start: BlockId,
        stop: Option<BlockId>,
        depth: usize,
    ) -> Result<(), String> {
        let program = self.program;
        let indent = "    ".repeat(depth);
        let mut current = start;

        while Some(current) != stop {
            if !self.written.insert(current) {
                self.repeated = true;
                return Ok(());
            }
            self.body(current, &indent)?;

            match &program.block(current).term {
                Term::Ret(_) | Term::Unreachable | Term::Switch { .. } => return Ok(()),
                Term::Br(next) => current = *next,
                Term::CondBr {
                    cond,
                    if_true,
                    if_false,
                } => {
                    let join = self.join(current);
                    let condition = self.condition(cond);
                    let taken = self.nested(*if_true, join, depth + 1)?;
                    let skipped = self.nested(*if_false, join, depth + 1)?;
                    match (taken.is_empty(), skipped.is_empty()) {
                        (true, true) => Ok(()),
                        (false, true) => {
                            write!(self.out, "{indent}if ({condition}) {{\n{taken}{indent}}}\n")
                        }
                        (true, false) => write!(
                            self.out,
                            "{indent}if (!({condition})) {{\n{skipped}{indent}}}\n"
                        ),
                        (false, false) => write!(
                            self.out,
                            "{indent}if ({condition}) {{\n{taken}{indent}}} else {{\n{skipped}{indent}}}\n"
                        ),
                    }
                    .unwrap();
                    match join {
                        Some(next) => current = next,
                        None => return Ok(()),
                    }
                }
            }
        }

        Ok(())
    }

    fn nested(
        &mut self,
        start: BlockId,
        stop: Option<BlockId>,
        depth: usize,
    ) -> Result<String, String> {
        let outer = mem::take(&mut self.out);
        let done = self.region(start, stop, depth);
        let inner = mem::replace(&mut self.out, outer);
        done.map(|()| inner)
    }

    fn dispatch(&mut self) -> Result<(), String> {
        let program = self.program;
        let number = |id: &BlockId| id.0 + 1;

        writeln!(self.out, "uint[32] block = {};", number(&program.entry)).unwrap();
        writeln!(self.out, "while (block != 0) {{").unwrap();
        for block in &program.blocks {
            writeln!(self.out, "    if (block == {}) {{", number(&block.id)).unwrap();
            self.body(block.id, "        ")?;
            let jump = match &block.term {
                Term::Ret(_) | Term::Unreachable => "block = 0;".to_string(),
                Term::Br(next) => format!("block = {};", number(next)),
                Term::CondBr {
                    cond,
                    if_true,
                    if_false,
                } => format!(
                    "if ({}) {{ block = {}; }} else {{ block = {}; }}",
                    self.condition(cond),
                    number(if_true),
                    number(if_false)
                ),
                Term::Switch {
                    scrutinee,
                    cases,
                    default,
                } => {
                    let key = self.value(scrutinee);
                    let mut text = String::new();
                    for (case, target) in cases {
                        write!(
                            text,
                            "if ({key} == {case}) {{ block = {}; }} else ",
                            number(target)
                        )
                        .unwrap();
                    }
                    format!("{text}{{ block = {}; }}", number(default))
                }
            };
            writeln!(self.out, "        {jump}\n    }}").unwrap();
        }
        writeln!(self.out, "}}").unwrap();
        Ok(())
    }

    fn body(&mut self, id: BlockId, indent: &str) -> Result<(), String> {
        let block = self.program.block(id);
        for op in &block.ops {
            if let Op::Assign {
                dest,
                expr: Expr::Phi(_),
                ..
            } = op
            {
                writeln!(self.out, "{indent}v{0} = v{0}_in;", dest.0).unwrap();
            }
        }
        for (index, op) in block.ops.iter().enumerate() {
            self.op(op, indent)?;
            if let (Some(number), Op::RecordOutput { value: Some(v), .. }) =
                (self.outputs.get(&(id, index)), op)
            {
                let value = self.value(v);
                writeln!(self.out, "{indent}out{number} = {value};").unwrap();
            }
        }
        if let Some(edges) = self.incoming.get(&id) {
            for (dest, operand) in edges {
                let value = self.value(operand);
                writeln!(self.out, "{indent}v{}_in = {value};", dest.0).unwrap();
            }
        }
        Ok(())
    }

    fn op(&mut self, op: &Op, indent: &str) -> Result<(), String> {
        match op {
            Op::Gate(gate) => {
                let params: Vec<String> = gate.params.iter().map(|p| self.angle(p)).collect();
                writeln!(self.out, "{indent}{}", qasm_gate(gate, &params)).unwrap();
            }
            Op::Measure { qubit, result, .. } => {
                writeln!(
                    self.out,
                    "{indent}c[{}] = measure q[{}];",
                    result.0, qubit.0
                )
                .unwrap();
            }
            Op::Reset { qubit, .. } => {
                writeln!(self.out, "{indent}reset q[{}];", qubit.0).unwrap();
            }
            Op::Assign { dest, ty, expr, .. } => self.assign(*dest, *ty, expr, indent)?,
            Op::Store { slot, value, .. } => {
                let value = self.value(value);
                writeln!(self.out, "{indent}s{} = {value};", slot.0).unwrap();
            }
            Op::RecordOutput { .. } | Op::Message { .. } => {}
        }
        Ok(())
    }

    fn assign(
        &mut self,
        dest: ValueId,
        ty: Scalar,
        expr: &Expr,
        indent: &str,
    ) -> Result<(), String> {
        if self.reads.contains_key(&dest) {
            return Ok(());
        }
        let name = format!("v{}", dest.0);
        let rhs = match expr {
            Expr::Phi(_) => return Ok(()),
            Expr::Const(c) => self.value(&Operand::Const(*c)),
            Expr::ReadResult(result) => format!("bool(c[{}])", result.0),
            Expr::Load(slot) => format!("s{}", slot.0),
            Expr::Binary { op, lhs, rhs } => self.binary(*op, ty, lhs, rhs)?,
            Expr::ICmp { pred, ty, lhs, rhs } => self.icmp(*pred, *ty, lhs, rhs),
            Expr::FCmp { pred, lhs, rhs, .. } => self.fcmp(*pred, lhs, rhs),
            Expr::Cast { op, from, operand } => self.cast(*op, *from, ty, operand)?,
            Expr::Select {
                cond,
                if_true,
                if_false,
            } => {
                let condition = self.condition(cond);
                let (a, b) = (self.value(if_true), self.value(if_false));
                writeln!(
                    self.out,
                    "{indent}if ({condition}) {{ {name} = {a}; }} else {{ {name} = {b}; }}"
                )
                .unwrap();
                return Ok(());
            }
        };
        writeln!(self.out, "{indent}{name} = {rhs};").unwrap();
        Ok(())
    }

    fn binary(
        &self,
        op: BinOp,
        ty: Scalar,
        lhs: &Operand,
        rhs: &Operand,
    ) -> Result<String, String> {
        let (a, b) = (self.value(lhs), self.value(rhs));

        if ty == Scalar::Bool {
            return match op {
                BinOp::And | BinOp::Mul => Ok(format!("{a} && {b}")),
                BinOp::Or => Ok(format!("{a} || {b}")),
                BinOp::Xor | BinOp::Add | BinOp::Sub => Ok(format!("{a} != {b}")),
                _ => Err(format!(
                    "`{}` on booleans cannot be written as OpenQASM 3",
                    op.keyword()
                )),
            };
        }

        let unsigned = |symbol: &str| {
            let (a, b) = (unsigned(&a, ty), unsigned(&b, ty));
            wrapped(&format!("int[64]({a} {symbol} {b})"), ty)
        };
        Ok(match op {
            BinOp::FAdd => format!("{a} + {b}"),
            BinOp::FSub => format!("{a} - {b}"),
            BinOp::FMul => format!("{a} * {b}"),
            BinOp::FDiv => format!("{a} / {b}"),
            BinOp::Add => wrapped(&format!("{a} + {b}"), ty),
            BinOp::Sub => wrapped(&format!("{a} - {b}"), ty),
            BinOp::Mul => wrapped(&format!("{a} * {b}"), ty),
            BinOp::SDiv => wrapped(&format!("{a} / {b}"), ty),
            BinOp::SRem => format!("{a} % {b}"),
            BinOp::Shl => wrapped(&format!("{a} << {b}"), ty),
            BinOp::AShr => format!("{a} >> {b}"),
            BinOp::And => format!("{a} & {b}"),
            BinOp::Or => format!("{a} | {b}"),
            BinOp::Xor => format!("{a} ^ {b}"),
            BinOp::UDiv => unsigned("/"),
            BinOp::URem => unsigned("%"),
            BinOp::LShr => unsigned(">>"),
            BinOp::FRem => {
                return Err("a floating point remainder cannot be written as OpenQASM 3".into());
            }
        })
    }

    fn icmp(&self, pred: IntPredicate, ty: Scalar, lhs: &Operand, rhs: &Operand) -> String {
        let (a, b) = (self.value(lhs), self.value(rhs));
        let (symbol, signed) = match pred {
            IntPredicate::Eq => return format!("{a} == {b}"),
            IntPredicate::Ne => return format!("{a} != {b}"),
            IntPredicate::Slt => ("<", true),
            IntPredicate::Sle => ("<=", true),
            IntPredicate::Sgt => (">", true),
            IntPredicate::Sge => (">=", true),
            IntPredicate::Ult => ("<", false),
            IntPredicate::Ule => ("<=", false),
            IntPredicate::Ugt => (">", false),
            IntPredicate::Uge => (">=", false),
        };
        let side = |x: &str| match (ty, signed) {
            (Scalar::Bool, true) => format!("-int[64]({x})"),
            (Scalar::Bool, false) => format!("int[64]({x})"),
            (_, true) => x.to_string(),
            (_, false) => unsigned(x, ty),
        };
        format!("{} {symbol} {}", side(&a), side(&b))
    }

    fn fcmp(&self, pred: FloatPredicate, lhs: &Operand, rhs: &Operand) -> String {
        let (a, b) = (self.value(lhs), self.value(rhs));
        match pred {
            FloatPredicate::True => "true".into(),
            FloatPredicate::False => "false".into(),
            FloatPredicate::Oeq => format!("{a} == {b}"),
            FloatPredicate::Une => format!("{a} != {b}"),
            FloatPredicate::Ogt => format!("{a} > {b}"),
            FloatPredicate::Oge => format!("{a} >= {b}"),
            FloatPredicate::Olt => format!("{a} < {b}"),
            FloatPredicate::Ole => format!("{a} <= {b}"),
            FloatPredicate::One => format!("({a} < {b} || {a} > {b})"),
            FloatPredicate::Ueq => format!("!({a} < {b} || {a} > {b})"),
            FloatPredicate::Ugt => format!("!({a} <= {b})"),
            FloatPredicate::Uge => format!("!({a} < {b})"),
            FloatPredicate::Ult => format!("!({a} >= {b})"),
            FloatPredicate::Ule => format!("!({a} > {b})"),
            FloatPredicate::Ord => format!("({a} == {a} && {b} == {b})"),
            FloatPredicate::Uno => format!("({a} != {a} || {b} != {b})"),
        }
    }

    fn cast(
        &self,
        op: CastOp,
        from: Scalar,
        to: Scalar,
        operand: &Operand,
    ) -> Result<String, String> {
        let x = self.value(operand);
        let integer = |x: String| match to {
            Scalar::Bool => format!("({x} & 1) == 1"),
            _ => wrapped(&x, to),
        };
        let float = |x: String| format!("{}({x})", qasm_type(to));
        Ok(match (op, from) {
            _ if from == to => x,
            (CastOp::ZExt | CastOp::UIToFP, Scalar::Bool) if to.is_float() => {
                float(format!("int[64]({x})"))
            }
            (CastOp::SExt | CastOp::SIToFP, Scalar::Bool) if to.is_float() => {
                float(format!("-int[64]({x})"))
            }
            (CastOp::ZExt | CastOp::UIToFP, Scalar::Bool) => format!("int[64]({x})"),
            (CastOp::SExt | CastOp::SIToFP, Scalar::Bool) => format!("-int[64]({x})"),
            (CastOp::Trunc | CastOp::SExt, _) => integer(x),
            (CastOp::ZExt, _) => integer(format!("int[64]({})", unsigned(&x, from))),
            (CastOp::SIToFP | CastOp::FPExt | CastOp::FPTrunc, _) => float(x),
            (CastOp::UIToFP, _) => float(unsigned(&x, from)),
            (CastOp::FPToSI | CastOp::FPToUI, _) => integer(format!("int[64]({x})")),
            (CastOp::PtrToInt | CastOp::IntToPtr | CastOp::BitCast | CastOp::AddrSpaceCast, _) => {
                return Err(format!(
                    "`{}` cannot be written as OpenQASM 3",
                    op.keyword()
                ));
            }
        })
    }

    fn condition(&self, cond: &Operand) -> String {
        let value = self.value(cond);
        if self.type_of(cond) == Scalar::Bool {
            value
        } else {
            format!("{value} != 0")
        }
    }

    fn angle(&self, operand: &Operand) -> String {
        match operand {
            Operand::Const(c) => c.as_f64().to_string(),
            Operand::Value(_) if self.type_of(operand) == Scalar::Double => self.value(operand),
            Operand::Value(_) => format!("float[64]({})", self.value(operand)),
        }
    }

    fn value(&self, operand: &Operand) -> String {
        match operand {
            Operand::Const(Const::Bool(b)) => b.to_string(),
            Operand::Const(Const::Int(i)) if *i < 0 => format!("({i})"),
            Operand::Const(Const::Int(i)) => i.to_string(),
            Operand::Const(Const::Float(f)) if *f < 0.0 => format!("({f:?})"),
            Operand::Const(Const::Float(f)) => format!("{f:?}"),
            Operand::Value(id) => match self.reads.get(id) {
                Some(result) => format!("c[{}]", result.0),
                None => format!("v{}", id.0),
            },
        }
    }

    fn type_of(&self, operand: &Operand) -> Scalar {
        self.types.of(operand)
    }

    fn join(&self, block: BlockId) -> Option<BlockId> {
        let post = self.post.as_ref()?;
        let own = &post[&block];
        own.iter()
            .copied()
            .filter(|&other| other != block)
            .find(|other| post[other].len() == own.len() - 1)
    }
}

fn qasm_type(ty: Scalar) -> &'static str {
    match ty {
        Scalar::Bool => "bool",
        Scalar::Int(_) => "int[64]",
        Scalar::Float => "float[32]",
        Scalar::Double => "float[64]",
    }
}

fn declare_var(out: &mut String, ty: Scalar, name: fmt::Arguments) {
    let zero = match ty {
        Scalar::Bool => "false",
        Scalar::Int(_) => "0",
        _ => "0.0",
    };
    writeln!(out, "{} {name} = {zero};", qasm_type(ty)).unwrap();
}

fn wrapped(x: &str, ty: Scalar) -> String {
    match ty {
        Scalar::Int(bits) if bits < 64 => format!("int[64](int[{bits}]({x}))"),
        _ => x.to_string(),
    }
}

fn unsigned(x: &str, ty: Scalar) -> String {
    match ty {
        Scalar::Int(bits) if bits < 64 => format!("uint[{bits}]({x})"),
        _ => format!("uint[64]({x})"),
    }
}

fn looping_blocks(program: &Program) -> HashSet<BlockId> {
    program
        .blocks
        .iter()
        .filter(|block| {
            let mut stack = block.term.successors();
            let mut seen = HashSet::new();
            while let Some(next) = stack.pop() {
                if next == block.id {
                    return true;
                }
                if seen.insert(next) {
                    stack.extend(program.block(next).term.successors());
                }
            }
            false
        })
        .map(|block| block.id)
        .collect()
}

struct Types {
    values: HashMap<ValueId, Scalar>,
    slots: HashMap<SlotId, Scalar>,
}

impl Types {
    fn new(program: &Program) -> Types {
        let mut values = HashMap::new();
        let mut slots = HashMap::new();
        for op in program.ops() {
            if let Op::Assign { dest, ty, expr, .. } = op {
                values.insert(*dest, *ty);
                if let Expr::Load(slot) = expr {
                    slots.insert(*slot, *ty);
                }
            }
        }
        let mut types = Types { values, slots };
        for op in program.ops() {
            if let Op::Store { slot, value, .. } = op
                && !types.slots.contains_key(slot)
            {
                let ty = types.of(value);
                types.slots.insert(*slot, ty);
            }
        }
        types
    }

    fn of(&self, operand: &Operand) -> Scalar {
        match operand {
            Operand::Const(Const::Bool(_)) => Scalar::Bool,
            Operand::Const(Const::Int(_)) => Scalar::Int(64),
            Operand::Const(Const::Float(_)) => Scalar::Double,
            Operand::Value(id) => self.values.get(id).copied().unwrap_or(Scalar::Int(64)),
        }
    }
}

fn post_dominators(program: &Program) -> Option<HashMap<BlockId, HashSet<BlockId>>> {
    if program.blocks.is_empty() {
        return Some(HashMap::new());
    }
    let mut order = Vec::new();
    let mut open = HashSet::from([program.entry]);
    let mut seen = HashSet::from([program.entry]);
    let mut stack = vec![(program.entry, 0)];

    while let Some(top) = stack.last_mut() {
        let (block, index) = *top;
        match program.block(block).term.successors().get(index) {
            Some(&next) => {
                top.1 += 1;
                if open.contains(&next) {
                    return None;
                }
                if seen.insert(next) {
                    open.insert(next);
                    stack.push((next, 0));
                }
            }
            None => {
                stack.pop();
                open.remove(&block);
                order.push(block);
            }
        }
    }

    let mut post: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();
    for block in order {
        let mut shared: Option<HashSet<BlockId>> = None;
        for next in program.block(block).term.successors() {
            let theirs = &post[&next];
            shared = Some(match shared {
                Some(set) => set.intersection(theirs).copied().collect(),
                None => theirs.clone(),
            });
        }
        let mut set = shared.unwrap_or_default();
        set.insert(block);
        post.insert(block, set);
    }

    Some(post)
}

fn qasm_name(kind: GateKind, controls: usize) -> String {
    let base = match kind {
        GateKind::I => "id",
        GateKind::R1 => "p",
        other => other.name(),
    };

    match (controls, base) {
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
    }
}

fn qasm_gate(gate: &Gate, params: &[String]) -> String {
    let wires = qasm_wires(gate);
    if let GateKind::Unitary(m) = gate.kind {
        let (theta, phi, lambda) = zyz_angles(&Matrix2::from_ir(m));
        return format!("U({theta}, {phi}, {lambda}) {wires};");
    }
    let name = qasm_name(gate.kind, gate.controls.len());

    if params.is_empty() {
        format!("{name} {wires};")
    } else {
        format!("{name}({}) {wires};", params.join(", "))
    }
}

fn qasm_wires(gate: &Gate) -> String {
    gate.wires()
        .map(|q| format!("q[{}]", q.0))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn zyz_angles(m: &Matrix2) -> (f64, f64, f64) {
    let Matrix2 { a, b, c, d } = *m;

    let theta = 2.0 * a.norm().clamp(-1.0, 1.0).acos();

    if b.norm() < 1e-12 && c.norm() < 1e-12 {
        let phase = (d / a).arg();
        return (0.0, phase, 0.0);
    }

    if a.norm() < 1e-12 {
        return (PI, (c / -b).arg(), 0.0);
    }

    let phi = (c / a).arg();
    let lambda = ((-b) / a).arg();

    (theta, phi, lambda)
}

pub fn emit_qir(program: &Program) -> Result<String, String> {
    QirEmitter::new(program).emit()
}

struct QirEmitter<'a> {
    program: &'a Program,
    out: String,
    declarations: Vec<(String, String)>,
    types: Types,
    constants: HashMap<ValueId, Const>,
    blocks: Vec<String>,
    labels: Vec<String>,
    prefix: &'static str,
    legacy: bool,
    native: Option<(GateSet, Synth)>,
}

impl<'a> QirEmitter<'a> {
    fn new(program: &'a Program) -> Self {
        let constants = program
            .ops()
            .filter_map(|op| match op {
                Op::Assign {
                    dest,
                    expr: Expr::Const(c),
                    ..
                } => Some((*dest, *c)),
                _ => None,
            })
            .collect();
        let legacy = program.ops().any(|op| {
            matches!(
                op,
                Op::RecordOutput {
                    kind: OutputKind::TupleEnd | OutputKind::ArrayEnd,
                    ..
                } | Op::RecordOutput {
                    kind: OutputKind::Tuple | OutputKind::Array,
                    count: None,
                    value: None,
                    ..
                }
            )
        });

        Self {
            program,
            out: String::new(),
            declarations: Vec::new(),
            types: Types::new(program),
            constants,
            blocks: block_names(program),
            labels: Vec::new(),
            prefix: if program.name.starts_with("label") {
                "output"
            } else {
                "label"
            },
            legacy,
            native: None,
        }
    }

    fn label_ref(&mut self, label: Option<&str>) -> String {
        let Some(label) = label.filter(|l| !l.is_empty()) else {
            return "i8* null".into();
        };
        let index = match self.labels.iter().position(|l| l == label) {
            Some(index) => index,
            None => {
                self.labels.push(label.to_string());
                self.labels.len() - 1
            }
        };
        let size = label.len() + 1;
        format!(
            "i8* getelementptr inbounds ([{size} x i8], [{size} x i8]* @{}{index}, i64 0, i64 0)",
            self.prefix
        )
    }

    fn declare(&mut self, name: &str, params: &str) {
        if !self.declarations.iter().any(|(n, _)| n == name) {
            self.declarations
                .push((name.to_string(), params.to_string()));
        }
    }

    fn operand(&self, operand: &Operand, ty: Scalar) -> String {
        let constant = match operand {
            Operand::Const(c) => *c,
            Operand::Value(id) => match self.constants.get(id) {
                Some(c) => *c,
                None if self.types.values.contains_key(id) => return format!("%v{}", id.0),
                None => return "undef".into(),
            },
        };
        match ty.normalize(constant) {
            Const::Bool(b) => b.to_string(),
            Const::Int(i) => i.to_string(),
            Const::Float(f) => format_double(f),
        }
    }

    fn block(&self, id: BlockId) -> &str {
        &self.blocks[id.index()]
    }

    fn emit(mut self) -> Result<String, String> {
        let program = self.program;

        writeln!(self.out, "; ModuleID = '{}'", c_string(&program.name)).unwrap();
        writeln!(
            self.out,
            "source_filename = \"{}\"",
            c_string(&program.name)
        )
        .unwrap();
        self.out.push('\n');
        self.out
            .push_str("%Result = type opaque\n%Qubit = type opaque\n\n");

        let mut body = String::new();
        for (index, block) in program.blocks.iter().enumerate() {
            writeln!(body, "{}:", self.block(block.id)).unwrap();
            if index == 0 {
                for slot in 0..program.num_slots {
                    let ty = self
                        .types
                        .slots
                        .get(&SlotId(slot))
                        .copied()
                        .unwrap_or(Scalar::Int(64));
                    writeln!(body, "  %slot{slot} = alloca {ty}").unwrap();
                }
            }
            for op in &block.ops {
                self.emit_op(op, &mut body)?;
            }
            self.emit_terminator(&block.term, &mut body);
        }

        for (index, label) in self.labels.iter().enumerate() {
            writeln!(
                self.out,
                "@{}{index} = internal constant [{} x i8] c\"{}\\00\"",
                self.prefix,
                label.len() + 1,
                c_string(label)
            )
            .unwrap();
        }
        if !self.labels.is_empty() {
            self.out.push('\n');
        }

        writeln!(
            self.out,
            "define void @{}() #0 {{",
            llvm_name(&program.name)
        )
        .unwrap();
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

        Ok(self.out)
    }

    fn emit_op(&mut self, op: &Op, body: &mut String) -> Result<(), String> {
        match op {
            Op::Gate(gate) => self.emit_gate(gate, body)?,

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
                value,
                count,
                label,
                ..
            } => {
                let open = count.is_none() && value.is_none();
                let name = match kind {
                    OutputKind::Result => "__quantum__rt__result_record_output",
                    OutputKind::Tuple if open => "__quantum__rt__tuple_start_record_output",
                    OutputKind::Array if open => "__quantum__rt__array_start_record_output",
                    OutputKind::Tuple => "__quantum__rt__tuple_record_output",
                    OutputKind::Array => "__quantum__rt__array_record_output",
                    OutputKind::TupleEnd => "__quantum__rt__tuple_end_record_output",
                    OutputKind::ArrayEnd => "__quantum__rt__array_end_record_output",
                    OutputKind::Bool => "__quantum__rt__bool_record_output",
                    OutputKind::Int => "__quantum__rt__int_record_output",
                    OutputKind::Double => "__quantum__rt__double_record_output",
                };

                let mut args = Vec::new();
                let mut params = Vec::new();
                match (result, value, count) {
                    (Some(r), _, _) => {
                        args.push(result_ref(*r));
                        params.push("%Result*".to_string());
                    }
                    (None, Some(v), _) => {
                        let ty = match kind {
                            OutputKind::Bool => Scalar::Bool,
                            OutputKind::Double => Scalar::Double,
                            _ => Scalar::Int(64),
                        };
                        args.push(format!("{ty} {}", self.operand(v, ty)));
                        params.push(ty.to_string());
                    }
                    (None, None, Some(n)) => {
                        args.push(format!("i64 {n}"));
                        params.push("i64".into());
                    }
                    (None, None, None) => {}
                }
                if !self.legacy {
                    args.push(self.label_ref(label.as_deref()));
                    params.push("i8*".into());
                }
                writeln!(body, "  call void @{name}({})", args.join(", ")).unwrap();
                self.declare(&format!("void @{name}"), &params.join(", "));
            }

            Op::Assign { dest, ty, expr, .. } => self.emit_assign(*dest, *ty, expr, body),

            Op::Store { slot, value, .. } => {
                let ty = self
                    .types
                    .slots
                    .get(slot)
                    .copied()
                    .unwrap_or(self.types.of(value));
                let value = self.operand(value, ty);
                writeln!(body, "  store {ty} {value}, ptr %slot{}", slot.0).unwrap();
            }

            Op::Message { .. } => {}
        }
        Ok(())
    }

    fn emit_gate(&mut self, gate: &Gate, body: &mut String) -> Result<(), String> {
        if let GateKind::Unitary(matrix) = gate.kind
            && gate.controls.is_empty()
        {
            let (theta, phi, lambda) = zyz_angles(&Matrix2::from_ir(matrix));
            for (kind, angle) in [
                (GateKind::Rz, lambda),
                (GateKind::Ry, theta),
                (GateKind::Rz, phi),
            ] {
                let rotation = Gate {
                    kind,
                    controls: Vec::new(),
                    targets: gate.targets.clone(),
                    params: vec![Operand::Const(Const::Float(angle))],
                    span: gate.span,
                };
                self.emit_gate(&rotation, body)?;
            }
            return Ok(());
        }

        if let Some(name) = intrinsic(gate) {
            let mut args = Vec::new();
            let mut params = Vec::new();
            for param in &gate.params {
                args.push(format!("double {}", self.operand(param, Scalar::Double)));
                params.push("double");
            }
            for qubit in gate.wires() {
                args.push(qubit_ref(qubit));
                params.push("%Qubit*");
            }
            writeln!(body, "  call void @{name}({})", args.join(", ")).unwrap();
            self.declare(&format!("void @{name}"), &params.join(", "));
            return Ok(());
        }

        let name = format!("{}{}", "c".repeat(gate.controls.len()), gate.kind.name());
        if gate.params.iter().any(|p| p.constant().is_none()) {
            return Err(format!(
                "`{name}` with a runtime angle has no QIR intrinsic"
            ));
        }
        let (set, synth) = self.native.get_or_insert_with(|| {
            let set = GateSet::native();
            let synth = Synth::new(&set);
            (set, synth)
        });
        let parts = transpile::translate(gate, set, synth)
            .filter(|parts| parts.iter().all(|part| intrinsic(part).is_some()))
            .ok_or_else(|| {
                format!("`{name}` has no QIR intrinsic and cannot be decomposed into one")
            })?;
        for part in &parts {
            self.emit_gate(part, body)?;
        }
        Ok(())
    }

    fn emit_assign(&mut self, dest: ValueId, ty: Scalar, expr: &Expr, body: &mut String) {
        let name = format!("%v{}", dest.0);

        match expr {
            Expr::Const(_) => {}

            Expr::Load(slot) => {
                writeln!(body, "  {name} = load {ty}, ptr %slot{}", slot.0).unwrap();
            }

            Expr::ReadResult(result) => {
                writeln!(
                    body,
                    "  {name} = call i1 @__quantum__qis__read_result__body({})",
                    result_ref(*result)
                )
                .unwrap();
                self.declare("i1 @__quantum__qis__read_result__body", "%Result*");
            }

            Expr::Binary { op, lhs, rhs } => {
                let (a, b) = (self.operand(lhs, ty), self.operand(rhs, ty));
                writeln!(body, "  {name} = {} {ty} {a}, {b}", op.keyword()).unwrap();
            }

            Expr::ICmp {
                pred,
                ty: operands,
                lhs,
                rhs,
            } => {
                let (a, b) = (self.operand(lhs, *operands), self.operand(rhs, *operands));
                writeln!(
                    body,
                    "  {name} = icmp {} {operands} {a}, {b}",
                    pred.keyword()
                )
                .unwrap();
            }

            Expr::FCmp {
                pred,
                ty: operands,
                lhs,
                rhs,
            } => {
                let (a, b) = (self.operand(lhs, *operands), self.operand(rhs, *operands));
                writeln!(
                    body,
                    "  {name} = fcmp {} {operands} {a}, {b}",
                    pred.keyword()
                )
                .unwrap();
            }

            Expr::Select {
                cond,
                if_true,
                if_false,
            } => {
                let c = self.operand(cond, Scalar::Bool);
                let (a, b) = (self.operand(if_true, ty), self.operand(if_false, ty));
                writeln!(body, "  {name} = select i1 {c}, {ty} {a}, {ty} {b}").unwrap();
            }

            Expr::Cast { op, from, operand } => {
                let value = self.operand(operand, *from);
                match cast_instruction(*op, *from, ty) {
                    Some(keyword) => {
                        writeln!(body, "  {name} = {keyword} {from} {value} to {ty}").unwrap();
                    }
                    None => {
                        writeln!(body, "  {name} = bitcast {ty} {value} to {ty}").unwrap();
                    }
                }
            }

            Expr::Phi(incoming) => {
                let parts: Vec<String> = incoming
                    .iter()
                    .map(|(block, operand)| {
                        format!("[ {}, %{} ]", self.operand(operand, ty), self.block(*block))
                    })
                    .collect();
                writeln!(body, "  {name} = phi {ty} {}", parts.join(", ")).unwrap();
            }
        }
    }

    fn emit_terminator(&self, term: &Term, body: &mut String) {
        match term {
            Term::Ret(_) => {
                writeln!(body, "  ret void").unwrap();
            }
            Term::Unreachable => {
                writeln!(body, "  unreachable").unwrap();
            }
            Term::Br(target) => {
                writeln!(body, "  br label %{}", self.block(*target)).unwrap();
            }
            Term::CondBr {
                cond,
                if_true,
                if_false,
            } => {
                writeln!(
                    body,
                    "  br i1 {}, label %{}, label %{}",
                    self.operand(cond, Scalar::Bool),
                    self.block(*if_true),
                    self.block(*if_false)
                )
                .unwrap();
            }
            Term::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let ty = self.types.of(scrutinee);
                writeln!(
                    body,
                    "  switch {ty} {}, label %{} [",
                    self.operand(scrutinee, ty),
                    self.block(*default)
                )
                .unwrap();
                for (key, target) in cases {
                    let key = self.operand(&Operand::Const(Const::Int(*key)), ty);
                    writeln!(body, "    {ty} {key}, label %{}", self.block(*target)).unwrap();
                }
                writeln!(body, "  ]").unwrap();
            }
        }
    }
}

fn intrinsic(gate: &Gate) -> Option<&'static str> {
    Some(match (gate.kind, gate.controls.len()) {
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
        _ => return None,
    })
}

fn cast_instruction(op: CastOp, from: Scalar, to: Scalar) -> Option<&'static str> {
    if from == to {
        return None;
    }
    let same_size = from.bits() == to.bits();
    Some(match (from.is_float(), to.is_float()) {
        (true, true) if to.bits() < from.bits() => "fptrunc",
        (true, true) => "fpext",
        (true, false) if op == CastOp::BitCast && same_size => "bitcast",
        (true, false) if op == CastOp::FPToUI => "fptoui",
        (true, false) => "fptosi",
        (false, true) if op == CastOp::BitCast && same_size => "bitcast",
        (false, true) if op == CastOp::UIToFP => "uitofp",
        (false, true) => "sitofp",
        _ if to.bits() < from.bits() => "trunc",
        _ if op == CastOp::SExt => "sext",
        _ => "zext",
    })
}

fn block_names(program: &Program) -> Vec<String> {
    let mut used = HashSet::new();
    program
        .blocks
        .iter()
        .map(|block| {
            let mut name = block.label.clone();
            if name.is_empty() || generated(&name) || used.contains(&name) {
                name = format!("{}.{}", block.label, block.id.0);
            }
            used.insert(name.clone());
            llvm_name(&name)
        })
        .collect()
}

fn generated(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = name.strip_prefix("slot") {
        return digits(rest);
    }
    name.strip_prefix('v')
        .is_some_and(|rest| digits(rest.strip_suffix(".f").unwrap_or(rest)))
}

fn llvm_name(name: &str) -> String {
    let plain = name.bytes().next().is_some_and(|b| !b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-$._".contains(&b));
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", c_string(name))
    }
}

fn c_string(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b' '..=b'~' if byte != b'"' && byte != b'\\' => out.push(byte as char),
            _ => write!(out, "\\{byte:02X}").unwrap(),
        }
    }
    out
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
    writeln!(out, "  \"name\": {},", json::quoted(&program.name)).unwrap();
    writeln!(out, "  \"profile\": {:?},", program.profile.name()).unwrap();
    writeln!(out, "  \"qubits\": {},", program.num_qubits).unwrap();
    writeln!(out, "  \"results\": {},", program.num_results).unwrap();
    writeln!(out, "  \"depth\": {},", program.depth()).unwrap();
    writeln!(out, "  \"gateCount\": {},", program.gate_count()).unwrap();
    out.push_str("  \"blocks\": [\n");

    for (index, block) in program.blocks.iter().enumerate() {
        out.push_str("    {\n");
        writeln!(out, "      \"label\": {},", json::quoted(&block.label)).unwrap();
        out.push_str("      \"ops\": [\n");

        let lines: Vec<String> = block.ops.iter().filter_map(json_op).collect();
        out.push_str(&lines.join(",\n"));
        if !lines.is_empty() {
            out.push('\n');
        }

        out.push_str("      ],\n");
        writeln!(
            out,
            "      \"terminator\": {}",
            json::quoted(&json_term(program, &block.term))
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
                .map(|p| match p.constant().map(|c| c.as_f64()) {
                    Some(angle) if angle.is_finite() => angle.to_string(),
                    _ => "null".into(),
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
            wires[wire_index].push_str(&center(cell, cell_width));
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
        if column.get(index).is_some_and(|c| c == "-") {
            column[index] = "|".into();
        }
    }
}

fn gate_symbol(gate: &Gate) -> String {
    match gate.kind {
        GateKind::X if !gate.controls.is_empty() => "+".into(),
        GateKind::Swap => "x".into(),
        GateKind::Unitary(_) => "U".into(),
        other => match gate.constant_angle() {
            Some(angle) if other.param_count() > 0 => {
                format!("{}({angle:.2})", other.name().to_uppercase())
            }
            _ => other.name().to_uppercase(),
        },
    }
}

fn center(text: &str, width: usize) -> String {
    let total = width.saturating_sub(text.len());
    let left = total / 2;
    format!("{}{text}{}", "-".repeat(left), "-".repeat(total - left))
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
