use crate::diag::Span;

#[derive(Clone, PartialEq, Debug)]
pub enum Ty {
    Void,
    Int(u32),
    Half,
    Float,
    Double,
    X86Fp80,
    Fp128,
    Ptr(Option<Box<Ty>>),
    Array(u64, Box<Ty>),
    Vector {
        len: u64,
        scalable: bool,
        elem: Box<Ty>,
    },
    Struct {
        fields: Vec<Ty>,
        packed: bool,
    },
    Named(String),
    Func {
        ret: Box<Ty>,
        params: Vec<Ty>,
        varargs: bool,
    },
    Label,
    Metadata,
    Token,
    Opaque,
}

impl Ty {
    pub fn is_void(&self) -> bool {
        matches!(self, Ty::Void)
    }

    pub fn is_float(&self) -> bool {
        matches!(
            self,
            Ty::Half | Ty::Float | Ty::Double | Ty::X86Fp80 | Ty::Fp128
        )
    }

    pub fn pointee_name(&self) -> Option<&str> {
        match self {
            Ty::Ptr(Some(inner)) => match inner.as_ref() {
                Ty::Named(name) => Some(name),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn render(&self) -> String {
        match self {
            Ty::Void => "void".into(),
            Ty::Int(bits) => format!("i{bits}"),
            Ty::Half => "half".into(),
            Ty::Float => "float".into(),
            Ty::Double => "double".into(),
            Ty::X86Fp80 => "x86_fp80".into(),
            Ty::Fp128 => "fp128".into(),
            Ty::Ptr(None) => "ptr".into(),
            Ty::Ptr(Some(inner)) => format!("{}*", inner.render()),
            Ty::Array(len, elem) => format!("[{len} x {}]", elem.render()),
            Ty::Vector {
                len,
                scalable,
                elem,
            } => {
                if *scalable {
                    format!("<vscale x {len} x {}>", elem.render())
                } else {
                    format!("<{len} x {}>", elem.render())
                }
            }
            Ty::Struct { fields, packed } => {
                let body = fields
                    .iter()
                    .map(|f| f.render())
                    .collect::<Vec<_>>()
                    .join(", ");
                if *packed {
                    format!("<{{ {body} }}>")
                } else {
                    format!("{{ {body} }}")
                }
            }
            Ty::Named(name) => format!("%{name}"),
            Ty::Func {
                ret,
                params,
                varargs,
            } => {
                let mut body = params
                    .iter()
                    .map(|p| p.render())
                    .collect::<Vec<_>>()
                    .join(", ");
                if *varargs {
                    if body.is_empty() {
                        body.push_str("...");
                    } else {
                        body.push_str(", ...");
                    }
                }
                format!("{} ({body})", ret.render())
            }
            Ty::Label => "label".into(),
            Ty::Metadata => "metadata".into(),
            Ty::Token => "token".into(),
            Ty::Opaque => "opaque".into(),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Local(String),
    Global(String),
    Int(i128),
    Float(f64),
    Bool(bool),
    Null,
    NoneValue,
    Undef,
    Poison,
    ZeroInit,
    Bytes(Vec<u8>),
    Aggregate(Vec<TypedValue>),
    ConstExpr(Box<ConstExpr>),
    MetadataRef(String),
    MetadataString(String),
    BlockAddress(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct TypedValue {
    pub ty: Ty,
    pub value: Value,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ConstExpr {
    Cast {
        op: CastOp,
        operand: TypedValue,
        to: Ty,
    },
    GetElementPtr {
        inbounds: bool,
        base_ty: Ty,
        ptr: TypedValue,
        indices: Vec<TypedValue>,
    },
    Binary {
        op: BinOp,
        lhs: TypedValue,
        rhs: TypedValue,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    Shl,
    LShr,
    AShr,
    And,
    Or,
    Xor,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FRem,
}

impl BinOp {
    pub fn from_keyword(keyword: &str) -> Option<BinOp> {
        Some(match keyword {
            "add" => BinOp::Add,
            "sub" => BinOp::Sub,
            "mul" => BinOp::Mul,
            "udiv" => BinOp::UDiv,
            "sdiv" => BinOp::SDiv,
            "urem" => BinOp::URem,
            "srem" => BinOp::SRem,
            "shl" => BinOp::Shl,
            "lshr" => BinOp::LShr,
            "ashr" => BinOp::AShr,
            "and" => BinOp::And,
            "or" => BinOp::Or,
            "xor" => BinOp::Xor,
            "fadd" => BinOp::FAdd,
            "fsub" => BinOp::FSub,
            "fmul" => BinOp::FMul,
            "fdiv" => BinOp::FDiv,
            "frem" => BinOp::FRem,
            _ => return None,
        })
    }

    pub fn keyword(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::UDiv => "udiv",
            BinOp::SDiv => "sdiv",
            BinOp::URem => "urem",
            BinOp::SRem => "srem",
            BinOp::Shl => "shl",
            BinOp::LShr => "lshr",
            BinOp::AShr => "ashr",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::FAdd => "fadd",
            BinOp::FSub => "fsub",
            BinOp::FMul => "fmul",
            BinOp::FDiv => "fdiv",
            BinOp::FRem => "frem",
        }
    }

    pub fn is_float(self) -> bool {
        matches!(
            self,
            BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv | BinOp::FRem
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CastOp {
    Trunc,
    ZExt,
    SExt,
    FPTrunc,
    FPExt,
    FPToUI,
    FPToSI,
    UIToFP,
    SIToFP,
    PtrToInt,
    IntToPtr,
    BitCast,
    AddrSpaceCast,
}

impl CastOp {
    pub fn from_keyword(keyword: &str) -> Option<CastOp> {
        Some(match keyword {
            "trunc" => CastOp::Trunc,
            "zext" => CastOp::ZExt,
            "sext" => CastOp::SExt,
            "fptrunc" => CastOp::FPTrunc,
            "fpext" => CastOp::FPExt,
            "fptoui" => CastOp::FPToUI,
            "fptosi" => CastOp::FPToSI,
            "uitofp" => CastOp::UIToFP,
            "sitofp" => CastOp::SIToFP,
            "ptrtoint" => CastOp::PtrToInt,
            "inttoptr" => CastOp::IntToPtr,
            "bitcast" => CastOp::BitCast,
            "addrspacecast" => CastOp::AddrSpaceCast,
            _ => return None,
        })
    }

    pub fn keyword(self) -> &'static str {
        match self {
            CastOp::Trunc => "trunc",
            CastOp::ZExt => "zext",
            CastOp::SExt => "sext",
            CastOp::FPTrunc => "fptrunc",
            CastOp::FPExt => "fpext",
            CastOp::FPToUI => "fptoui",
            CastOp::FPToSI => "fptosi",
            CastOp::UIToFP => "uitofp",
            CastOp::SIToFP => "sitofp",
            CastOp::PtrToInt => "ptrtoint",
            CastOp::IntToPtr => "inttoptr",
            CastOp::BitCast => "bitcast",
            CastOp::AddrSpaceCast => "addrspacecast",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IntPredicate {
    Eq,
    Ne,
    Ugt,
    Uge,
    Ult,
    Ule,
    Sgt,
    Sge,
    Slt,
    Sle,
}

impl IntPredicate {
    pub fn from_keyword(keyword: &str) -> Option<IntPredicate> {
        Some(match keyword {
            "eq" => IntPredicate::Eq,
            "ne" => IntPredicate::Ne,
            "ugt" => IntPredicate::Ugt,
            "uge" => IntPredicate::Uge,
            "ult" => IntPredicate::Ult,
            "ule" => IntPredicate::Ule,
            "sgt" => IntPredicate::Sgt,
            "sge" => IntPredicate::Sge,
            "slt" => IntPredicate::Slt,
            "sle" => IntPredicate::Sle,
            _ => return None,
        })
    }

    pub fn keyword(self) -> &'static str {
        match self {
            IntPredicate::Eq => "eq",
            IntPredicate::Ne => "ne",
            IntPredicate::Ugt => "ugt",
            IntPredicate::Uge => "uge",
            IntPredicate::Ult => "ult",
            IntPredicate::Ule => "ule",
            IntPredicate::Sgt => "sgt",
            IntPredicate::Sge => "sge",
            IntPredicate::Slt => "slt",
            IntPredicate::Sle => "sle",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloatPredicate {
    False,
    Oeq,
    Ogt,
    Oge,
    Olt,
    Ole,
    One,
    Ord,
    Ueq,
    Ugt,
    Uge,
    Ult,
    Ule,
    Une,
    Uno,
    True,
}

impl FloatPredicate {
    pub fn from_keyword(keyword: &str) -> Option<FloatPredicate> {
        Some(match keyword {
            "false" => FloatPredicate::False,
            "oeq" => FloatPredicate::Oeq,
            "ogt" => FloatPredicate::Ogt,
            "oge" => FloatPredicate::Oge,
            "olt" => FloatPredicate::Olt,
            "ole" => FloatPredicate::Ole,
            "one" => FloatPredicate::One,
            "ord" => FloatPredicate::Ord,
            "ueq" => FloatPredicate::Ueq,
            "ugt" => FloatPredicate::Ugt,
            "uge" => FloatPredicate::Uge,
            "ult" => FloatPredicate::Ult,
            "ule" => FloatPredicate::Ule,
            "une" => FloatPredicate::Une,
            "uno" => FloatPredicate::Uno,
            "true" => FloatPredicate::True,
            _ => return None,
        })
    }

    pub fn keyword(self) -> &'static str {
        match self {
            FloatPredicate::False => "false",
            FloatPredicate::Oeq => "oeq",
            FloatPredicate::Ogt => "ogt",
            FloatPredicate::Oge => "oge",
            FloatPredicate::Olt => "olt",
            FloatPredicate::Ole => "ole",
            FloatPredicate::One => "one",
            FloatPredicate::Ord => "ord",
            FloatPredicate::Ueq => "ueq",
            FloatPredicate::Ugt => "ugt",
            FloatPredicate::Uge => "uge",
            FloatPredicate::Ult => "ult",
            FloatPredicate::Ule => "ule",
            FloatPredicate::Une => "une",
            FloatPredicate::Uno => "uno",
            FloatPredicate::True => "true",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Argument {
    pub ty: Ty,
    pub attrs: Vec<String>,
    pub value: Value,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Call {
    pub tail: bool,
    pub ret_ty: Ty,
    pub explicit_fn_ty: Option<Ty>,
    pub callee: Value,
    pub args: Vec<Argument>,
    pub attr_groups: Vec<String>,
    pub span: Span,
}

impl Call {
    pub fn callee_name(&self) -> Option<&str> {
        match &self.callee {
            Value::Global(name) => Some(name),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum InstKind {
    Call(Call),
    Binary {
        op: BinOp,
        ty: Ty,
        lhs: Value,
        rhs: Value,
    },
    ICmp {
        pred: IntPredicate,
        ty: Ty,
        lhs: Value,
        rhs: Value,
    },
    FCmp {
        pred: FloatPredicate,
        ty: Ty,
        lhs: Value,
        rhs: Value,
    },
    Cast {
        op: CastOp,
        operand: TypedValue,
        to: Ty,
    },
    Select {
        cond: TypedValue,
        if_true: TypedValue,
        if_false: TypedValue,
    },
    Phi {
        ty: Ty,
        incoming: Vec<(Value, String)>,
    },
    Alloca {
        ty: Ty,
        count: Option<TypedValue>,
    },
    Load {
        ty: Ty,
        ptr: TypedValue,
    },
    Store {
        value: TypedValue,
        ptr: TypedValue,
    },
    GetElementPtr {
        inbounds: bool,
        base_ty: Ty,
        ptr: TypedValue,
        indices: Vec<TypedValue>,
    },
    ExtractValue {
        aggregate: TypedValue,
        indices: Vec<u64>,
    },
    InsertValue {
        aggregate: TypedValue,
        value: TypedValue,
        indices: Vec<u64>,
    },
    Freeze(TypedValue),
    Fence,
    Unsupported {
        opcode: String,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Instruction {
    pub result: Option<String>,
    pub kind: InstKind,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Terminator {
    Ret(Option<TypedValue>),
    Br {
        target: String,
    },
    CondBr {
        cond: TypedValue,
        if_true: String,
        if_false: String,
    },
    Switch {
        scrutinee: TypedValue,
        default: String,
        cases: Vec<(TypedValue, String)>,
    },
    Unreachable,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BasicBlock {
    pub label: String,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Param {
    pub ty: Ty,
    pub name: Option<String>,
    pub attrs: Vec<String>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FuncSig {
    pub name: String,
    pub ret_ty: Ty,
    pub params: Vec<Param>,
    pub varargs: bool,
    pub attr_groups: Vec<String>,
    pub attrs: Vec<Attribute>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Function {
    pub sig: FuncSig,
    pub blocks: Vec<BasicBlock>,
    pub span: Span,
}

impl Function {
    pub fn block(&self, label: &str) -> Option<&BasicBlock> {
        self.blocks.iter().find(|b| b.label == label)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Attribute {
    Flag(String),
    KeyValue(String, String),
}

impl Attribute {
    pub fn key(&self) -> &str {
        match self {
            Attribute::Flag(k) => k,
            Attribute::KeyValue(k, _) => k,
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Attribute::Flag(_) => None,
            Attribute::KeyValue(_, v) => Some(v),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AttrGroup {
    pub id: String,
    pub attrs: Vec<Attribute>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct GlobalVar {
    pub name: String,
    pub ty: Ty,
    pub initializer: Option<Value>,
    pub is_constant: bool,
    pub linkage: Vec<String>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TypeDef {
    pub name: String,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub struct NamedMetadata {
    pub name: String,
    pub operands: Vec<String>,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug)]
pub enum MetadataNode {
    Tuple(Vec<MetadataItem>),
    Specialized { kind: String, body: String },
}

#[derive(Clone, PartialEq, Debug)]
pub enum MetadataItem {
    Value(TypedValue),
    Ref(String),
    Str(String),
    Node(Vec<MetadataItem>),
    Null,
}

#[derive(Clone, PartialEq, Debug)]
pub struct MetadataDef {
    pub id: String,
    pub distinct: bool,
    pub node: MetadataNode,
    pub span: Span,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Module {
    pub source_filename: Option<String>,
    pub datalayout: Option<String>,
    pub triple: Option<String>,
    pub type_defs: Vec<TypeDef>,
    pub globals: Vec<GlobalVar>,
    pub declarations: Vec<FuncSig>,
    pub functions: Vec<Function>,
    pub attr_groups: Vec<AttrGroup>,
    pub named_metadata: Vec<NamedMetadata>,
    pub metadata: Vec<MetadataDef>,
}

impl Module {
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.sig.name == name)
    }

    pub fn attr_group(&self, id: &str) -> Option<&AttrGroup> {
        self.attr_groups.iter().find(|g| g.id == id)
    }

    pub fn global(&self, name: &str) -> Option<&GlobalVar> {
        self.globals.iter().find(|g| g.name == name)
    }

    pub fn metadata_def(&self, id: &str) -> Option<&MetadataDef> {
        self.metadata.iter().find(|m| m.id == id)
    }

    pub fn named_metadata_node(&self, name: &str) -> Option<&NamedMetadata> {
        self.named_metadata.iter().find(|n| n.name == name)
    }

    pub fn attributes_of<'a>(&'a self, sig: &'a FuncSig) -> Vec<&'a Attribute> {
        let mut out: Vec<&Attribute> = sig.attrs.iter().collect();
        for id in &sig.attr_groups {
            if let Some(group) = self.attr_group(id) {
                out.extend(group.attrs.iter());
            }
        }
        out
    }

    pub fn entry_point(&self) -> Option<&Function> {
        self.functions
            .iter()
            .find(|f| {
                self.attributes_of(&f.sig)
                    .iter()
                    .any(|a| a.key() == "entry_point")
            })
            .or_else(|| self.function("main"))
    }
}
