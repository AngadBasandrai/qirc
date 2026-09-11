use crate::diag::Span;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ValueId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct QubitId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResultId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SlotId(pub u32);

impl SlotId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl QubitId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl ResultId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Profile {
    Base,
    Adaptive,
    #[default]
    Unrestricted,
}

impl Profile {
    pub fn from_attribute(value: &str) -> Profile {
        match value {
            "base_profile" | "base" => Profile::Base,
            "adaptive_profile" | "adaptive" => Profile::Adaptive,
            _ => Profile::Unrestricted,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Base => "base_profile",
            Profile::Adaptive => "adaptive_profile",
            Profile::Unrestricted => "unrestricted",
        }
    }

    pub fn allows_branching(self) -> bool {
        !matches!(self, Profile::Base)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Const {
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl Const {
    pub fn as_f64(self) -> f64 {
        match self {
            Const::Bool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            Const::Int(i) => i as f64,
            Const::Float(f) => f,
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            Const::Bool(b) => b as i64,
            Const::Int(i) => i,
            Const::Float(f) => f as i64,
        }
    }

    pub fn truthy(self) -> bool {
        match self {
            Const::Bool(b) => b,
            Const::Int(i) => i != 0,
            Const::Float(f) => f != 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Operand {
    Const(Const),
    Value(ValueId),
}

impl Operand {
    pub fn constant(self) -> Option<Const> {
        match self {
            Operand::Const(c) => Some(c),
            Operand::Value(_) => None,
        }
    }

    pub fn value(self) -> Option<ValueId> {
        match self {
            Operand::Value(v) => Some(v),
            Operand::Const(_) => None,
        }
    }
}

pub use crate::ast::{BinOp, CastOp, FloatPredicate, IntPredicate};

#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    Const(Const),
    Copy(Operand),
    Binary {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    ICmp {
        pred: IntPredicate,
        lhs: Operand,
        rhs: Operand,
    },
    FCmp {
        pred: FloatPredicate,
        lhs: Operand,
        rhs: Operand,
    },
    Select {
        cond: Operand,
        if_true: Operand,
        if_false: Operand,
    },
    Cast {
        op: CastOp,
        operand: Operand,
    },
    Phi(Vec<(BlockId, Operand)>),
    ReadResult(ResultId),
    Load(SlotId),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Matrix2 {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub c: (f64, f64),
    pub d: (f64, f64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GateKind {
    I,
    X,
    Y,
    Z,
    H,
    S,
    SDag,
    T,
    TDag,
    SX,
    SXDag,
    Rx,
    Ry,
    Rz,
    R1,
    Swap,
    Unitary(Matrix2),
}

impl GateKind {
    pub fn arity(self) -> usize {
        match self {
            GateKind::Swap => 2,
            _ => 1,
        }
    }

    pub fn param_count(self) -> usize {
        match self {
            GateKind::Rx | GateKind::Ry | GateKind::Rz | GateKind::R1 => 1,
            _ => 0,
        }
    }

    pub fn is_self_inverse(self) -> bool {
        matches!(
            self,
            GateKind::I | GateKind::X | GateKind::Y | GateKind::Z | GateKind::H | GateKind::Swap
        )
    }

    pub fn adjoint(self) -> Option<GateKind> {
        Some(match self {
            GateKind::S => GateKind::SDag,
            GateKind::SDag => GateKind::S,
            GateKind::T => GateKind::TDag,
            GateKind::TDag => GateKind::T,
            GateKind::SX => GateKind::SXDag,
            GateKind::SXDag => GateKind::SX,
            other if other.is_self_inverse() => other,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            GateKind::I => "i",
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
            GateKind::R1 => "r1",
            GateKind::Swap => "swap",
            GateKind::Unitary(_) => "unitary",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Gate {
    pub kind: GateKind,
    pub controls: Vec<QubitId>,
    pub targets: Vec<QubitId>,
    pub params: Vec<Operand>,
    pub span: Span,
}

impl Gate {
    pub fn touches(&self, qubit: QubitId) -> bool {
        self.controls.contains(&qubit) || self.targets.contains(&qubit)
    }

    pub fn wires(&self) -> impl Iterator<Item = QubitId> + '_ {
        self.controls.iter().chain(self.targets.iter()).copied()
    }

    pub fn is_parameterised(&self) -> bool {
        self.kind.param_count() > 0
    }

    pub fn constant_angle(&self) -> Option<f64> {
        self.params.first()?.constant().map(|c| c.as_f64())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputKind {
    Result,
    Bool,
    Int,
    Double,
    Tuple,
    Array,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    Gate(Gate),
    Measure {
        qubit: QubitId,
        result: ResultId,
        dest: Option<ValueId>,
        span: Span,
    },
    Reset {
        qubit: QubitId,
        span: Span,
    },
    Assign {
        dest: ValueId,
        expr: Expr,
        span: Span,
    },
    RecordOutput {
        kind: OutputKind,
        result: Option<ResultId>,
        count: Option<i64>,
        label: Option<String>,
        span: Span,
    },
    Message {
        text: String,
        span: Span,
    },
    Store {
        slot: SlotId,
        value: Operand,
        span: Span,
    },
}

impl Op {
    pub fn span(&self) -> Span {
        match self {
            Op::Gate(g) => g.span,
            Op::Measure { span, .. }
            | Op::Reset { span, .. }
            | Op::Assign { span, .. }
            | Op::RecordOutput { span, .. }
            | Op::Store { span, .. }
            | Op::Message { span, .. } => *span,
        }
    }

    pub fn as_gate(&self) -> Option<&Gate> {
        match self {
            Op::Gate(g) => Some(g),
            _ => None,
        }
    }

    pub fn defined_value(&self) -> Option<ValueId> {
        match self {
            Op::Assign { dest, .. } => Some(*dest),
            Op::Measure { dest, .. } => *dest,
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Term {
    Ret(Option<Operand>),
    Br(BlockId),
    CondBr {
        cond: Operand,
        if_true: BlockId,
        if_false: BlockId,
    },
    Switch {
        scrutinee: Operand,
        cases: Vec<(i64, BlockId)>,
        default: BlockId,
    },
    Unreachable,
}

impl Term {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Term::Ret(_) | Term::Unreachable => Vec::new(),
            Term::Br(b) => vec![*b],
            Term::CondBr {
                if_true, if_false, ..
            } => vec![*if_true, *if_false],
            Term::Switch { cases, default, .. } => {
                let mut out: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
                out.push(*default);
                out
            }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub id: BlockId,
    pub label: String,
    pub ops: Vec<Op>,
    pub term: Term,
    pub span: Span,
}

impl Block {
    pub fn gates(&self) -> impl Iterator<Item = &Gate> {
        self.ops.iter().filter_map(Op::as_gate)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Program {
    pub name: String,
    pub profile: Profile,
    pub num_qubits: u32,
    pub num_results: u32,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub next_value: u32,
    pub num_slots: u32,
}

impl Program {
    pub fn new(name: impl Into<String>, profile: Profile) -> Self {
        Self {
            name: name.into(),
            profile,
            num_qubits: 0,
            num_results: 0,
            entry: BlockId(0),
            blocks: Vec::new(),
            next_value: 0,
            num_slots: 0,
        }
    }

    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut Block {
        &mut self.blocks[id.0 as usize]
    }

    pub fn ops(&self) -> impl Iterator<Item = &Op> {
        self.blocks.iter().flat_map(|b| b.ops.iter())
    }

    pub fn gates(&self) -> impl Iterator<Item = &Gate> {
        self.blocks.iter().flat_map(|b| b.gates())
    }

    pub fn gate_count(&self) -> usize {
        self.gates().count()
    }

    pub fn op_count(&self) -> usize {
        self.blocks.iter().map(|b| b.ops.len()).sum()
    }

    pub fn measure_count(&self) -> usize {
        self.ops()
            .filter(|o| matches!(o, Op::Measure { .. }))
            .count()
    }

    pub fn is_straight_line(&self) -> bool {
        self.blocks.len() == 1
    }

    pub fn depth(&self) -> usize {
        let mut per_wire = vec![0usize; self.num_qubits as usize];
        for gate in self.gates() {
            let level = gate
                .wires()
                .map(|q| per_wire.get(q.index()).copied().unwrap_or(0))
                .max()
                .unwrap_or(0)
                + 1;
            for wire in gate.wires() {
                if let Some(slot) = per_wire.get_mut(wire.index()) {
                    *slot = level;
                }
            }
        }
        per_wire.into_iter().max().unwrap_or(0)
    }

    pub fn predecessors(&self, id: BlockId) -> Vec<BlockId> {
        self.blocks
            .iter()
            .filter(|b| b.term.successors().contains(&id))
            .map(|b| b.id)
            .collect()
    }

    pub fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![self.entry];

        while let Some(id) = stack.pop() {
            let index = id.0 as usize;
            if index >= seen.len() || seen[index] {
                continue;
            }
            seen[index] = true;
            for next in self.blocks[index].term.successors() {
                stack.push(next);
            }
        }

        seen
    }

    pub fn fresh_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }
}

impl std::fmt::Display for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "program {} [{}] qubits={} results={}",
            self.name,
            self.profile.name(),
            self.num_qubits,
            self.num_results
        )?;

        for block in &self.blocks {
            writeln!(f, "{}:", block.label)?;
            for op in &block.ops {
                writeln!(f, "  {}", render_op(op))?;
            }
            writeln!(f, "  {}", render_term(self, &block.term))?;
        }

        Ok(())
    }
}

fn render_operand(operand: &Operand) -> String {
    match operand {
        Operand::Const(Const::Bool(b)) => b.to_string(),
        Operand::Const(Const::Int(i)) => i.to_string(),
        Operand::Const(Const::Float(x)) => format!("{x}"),
        Operand::Value(v) => format!("%{}", v.0),
    }
}

fn render_op(op: &Op) -> String {
    match op {
        Op::Gate(gate) => {
            let mut out = String::new();
            if !gate.controls.is_empty() {
                out.push_str(&"c".repeat(gate.controls.len()));
            }
            out.push_str(gate.kind.name());
            if !gate.params.is_empty() {
                let params: Vec<String> = gate.params.iter().map(render_operand).collect();
                out.push_str(&format!("({})", params.join(", ")));
            }
            let wires: Vec<String> = gate
                .controls
                .iter()
                .chain(gate.targets.iter())
                .map(|q| format!("q{}", q.0))
                .collect();
            out.push_str(&format!(" {}", wires.join(", ")));
            out
        }
        Op::Measure {
            qubit,
            result,
            dest,
            ..
        } => match dest {
            Some(d) => format!("%{} = measure q{} -> r{}", d.0, qubit.0, result.0),
            None => format!("measure q{} -> r{}", qubit.0, result.0),
        },
        Op::Reset { qubit, .. } => format!("reset q{}", qubit.0),
        Op::Assign { dest, expr, .. } => format!("%{} = {}", dest.0, render_expr(expr)),
        Op::RecordOutput {
            kind,
            result,
            label,
            ..
        } => {
            let target = result
                .map(|r| format!("r{}", r.0))
                .unwrap_or_else(|| "-".into());
            match label {
                Some(l) => format!("record {kind:?} {target} as {l:?}"),
                None => format!("record {kind:?} {target}"),
            }
        }
        Op::Message { text, .. } => format!("message {text:?}"),
        Op::Store { slot, value, .. } => {
            format!("store s{} = {}", slot.0, render_operand(value))
        }
    }
}

fn render_expr(expr: &Expr) -> String {
    match expr {
        Expr::Const(c) => render_operand(&Operand::Const(*c)),
        Expr::Copy(o) => render_operand(o),
        Expr::Binary { op, lhs, rhs } => format!(
            "{} {} {}",
            op.keyword(),
            render_operand(lhs),
            render_operand(rhs)
        ),
        Expr::ICmp { pred, lhs, rhs } => format!(
            "icmp {} {} {}",
            pred.keyword(),
            render_operand(lhs),
            render_operand(rhs)
        ),
        Expr::FCmp { pred, lhs, rhs } => format!(
            "fcmp {} {} {}",
            pred.keyword(),
            render_operand(lhs),
            render_operand(rhs)
        ),
        Expr::Select {
            cond,
            if_true,
            if_false,
        } => format!(
            "select {} {} {}",
            render_operand(cond),
            render_operand(if_true),
            render_operand(if_false)
        ),
        Expr::Cast { op, operand } => format!("{} {}", op.keyword(), render_operand(operand)),
        Expr::Phi(incoming) => {
            let parts: Vec<String> = incoming
                .iter()
                .map(|(b, o)| format!("[{} {}]", b.0, render_operand(o)))
                .collect();
            format!("phi {}", parts.join(" "))
        }
        Expr::ReadResult(r) => format!("read r{}", r.0),
        Expr::Load(slot) => format!("load s{}", slot.0),
    }
}

fn render_term(program: &Program, term: &Term) -> String {
    let label = |id: &BlockId| program.blocks[id.0 as usize].label.clone();

    match term {
        Term::Ret(None) => "ret".into(),
        Term::Ret(Some(o)) => format!("ret {}", render_operand(o)),
        Term::Br(b) => format!("br {}", label(b)),
        Term::CondBr {
            cond,
            if_true,
            if_false,
        } => format!(
            "br {} ? {} : {}",
            render_operand(cond),
            label(if_true),
            label(if_false)
        ),
        Term::Switch {
            scrutinee,
            cases,
            default,
        } => {
            let parts: Vec<String> = cases
                .iter()
                .map(|(v, b)| format!("{v} => {}", label(b)))
                .collect();
            format!(
                "switch {} [{}] else {}",
                render_operand(scrutinee),
                parts.join(", "),
                label(default)
            )
        }
        Term::Unreachable => "unreachable".into(),
    }
}
