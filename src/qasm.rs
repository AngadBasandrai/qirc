use std::collections::HashMap;
use std::f64::consts::{E, PI};

use crate::diag::{Diagnostic, Span};
use crate::ir::*;

const MAX_DEPTH: usize = 64;
const MAX_NESTING: usize = 256;
const MAX_OPS: usize = 5_000_000;

const PRELUDE: &str = "
gate u0(gamma) q { }
gate rzz(theta) a, b { cx a, b; rz(theta) b; cx a, b; }
gate rxx(theta) a, b { h a; h b; cx a, b; rz(theta) b; cx a, b; h a; h b; }
gate ryy(theta) a, b { rx(pi / 2) a; rx(pi / 2) b; cx a, b; rz(theta) b; cx a, b; rx(-pi / 2) a; rx(-pi / 2) b; }
gate cu3(theta, phi, lambda) c, t { p((lambda + phi) / 2) c; p((lambda - phi) / 2) t; cx c, t; u3(-theta / 2, 0, -(phi + lambda) / 2) t; cx c, t; u3(theta / 2, phi, 0) t; }
gate cu(theta, phi, lambda, gamma) c, t { p(gamma) c; cu3(theta, phi, lambda) c, t; }
gate rccx a, b, c { u2(0, pi) c; p(pi / 4) c; cx b, c; p(-pi / 4) c; cx a, c; p(pi / 4) c; cx b, c; p(-pi / 4) c; u2(0, pi) c; }
";

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Name(String),
    Number(f64),
    Text(String),
    Symbol(&'static str),
}

const SYMBOLS: [&str; 30] = [
    "->", "==", "!=", "&&", "||", ">=", "<=", ">", "<", "%", "|", "&", "^", "~", ";", ",", "(",
    ")", "[", "]", "{", "}", "=", "+", "-", "*", "/", "!", "@", ":",
];

fn lex(source: &str) -> Result<Vec<(Token, Span)>, Diagnostic> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        let start = at;
        if c.is_ascii_whitespace() {
            at += 1;
        } else if source[at..].starts_with("//") {
            at = source[at..].find('\n').map_or(bytes.len(), |n| at + n);
        } else if source[at..].starts_with("/*") {
            at = source[at + 2..]
                .find("*/")
                .map_or(bytes.len(), |n| at + n + 4);
        } else if c.is_ascii_alphabetic() || c == b'_' || source[at..].starts_with('π') {
            if source[at..].starts_with('π') {
                at += 'π'.len_utf8();
                tokens.push((Token::Name("pi".into()), Span::new(start, at)));
                continue;
            }
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            tokens.push((
                Token::Name(source[start..at].to_string()),
                Span::new(start, at),
            ));
        } else if c.is_ascii_digit()
            || (c == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
                at += 1;
            }
            if at < bytes.len() && (bytes[at] == b'e' || bytes[at] == b'E') {
                at += 1;
                if at < bytes.len() && (bytes[at] == b'+' || bytes[at] == b'-') {
                    at += 1;
                }
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let text = &source[start..at];
            let value = text.parse::<f64>().map_err(|_| {
                Diagnostic::error(format!("`{text}` is not a number"))
                    .primary(Span::new(start, at), "here")
            })?;
            tokens.push((Token::Number(value), Span::new(start, at)));
        } else if c == b'"' {
            let end = source[at + 1..].find('"').ok_or_else(|| {
                Diagnostic::error("unterminated string")
                    .primary(Span::new(start, start + 1), "starts here")
            })?;
            tokens.push((
                Token::Text(source[at + 1..at + 1 + end].to_string()),
                Span::new(start, at + end + 2),
            ));
            at += end + 2;
        } else if let Some(symbol) = SYMBOLS.iter().find(|s| source[at..].starts_with(**s)) {
            at += symbol.len();
            tokens.push((Token::Symbol(symbol), Span::new(start, at)));
        } else {
            let ch = source[at..].chars().next().unwrap_or('?');
            return Err(
                Diagnostic::error(format!("unexpected character `{ch}`")).primary(
                    Span::new(start, start + ch.len_utf8()),
                    "not part of OpenQASM",
                ),
            );
        }
    }
    Ok(tokens)
}

#[derive(Clone, Debug)]
enum Angle {
    Number(f64),
    Name(String, Span),
    Neg(Box<Angle>),
    Binary(char, Box<Angle>, Box<Angle>),
    Call(String, Box<Angle>, Span),
}

impl Angle {
    fn eval(&self, env: &HashMap<String, f64>) -> Result<f64, Diagnostic> {
        Ok(match self {
            Angle::Number(n) => *n,
            Angle::Name(name, span) => match name.as_str() {
                "pi" => PI,
                "tau" => 2.0 * PI,
                "euler" => E,
                _ => *env.get(name).ok_or_else(|| {
                    Diagnostic::error(format!("unknown parameter `{name}`"))
                        .primary(*span, "not defined here")
                })?,
            },
            Angle::Neg(e) => -e.eval(env)?,
            Angle::Binary(op, a, b) => {
                let (a, b) = (a.eval(env)?, b.eval(env)?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ => a / b,
                }
            }
            Angle::Call(name, arg, span) => {
                let x = arg.eval(env)?;
                match name.as_str() {
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    "tan" => x.tan(),
                    "sqrt" => x.sqrt(),
                    "exp" => x.exp(),
                    "ln" => x.ln(),
                    "arcsin" | "asin" => x.asin(),
                    "arccos" | "acos" => x.acos(),
                    "arctan" | "atan" => x.atan(),
                    _ => {
                        return Err(Diagnostic::error(format!("unknown function `{name}`"))
                            .primary(*span, "here"));
                    }
                }
            }
        })
    }
}

#[derive(Clone, Debug)]
struct Wire {
    name: String,
    index: Option<usize>,
    span: Span,
}

#[derive(Clone, Debug)]
struct Call {
    name: String,
    params: Vec<Angle>,
    args: Vec<Wire>,
    span: Span,
}

#[derive(Clone)]
struct Definition {
    params: Vec<String>,
    qubits: Vec<String>,
    body: Vec<Call>,
    size: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Qubit,
    Bit,
}

struct Register {
    kind: Kind,
    offset: usize,
    size: usize,
    array: bool,
}

struct Parser<'a> {
    tokens: &'a [(Token, Span)],
    at: usize,
    end: usize,
    nesting: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(t, _)| t)
    }

    fn span(&self) -> Span {
        self.tokens
            .get(self.at)
            .or(self.tokens.last())
            .map_or(Span::new(self.end, self.end), |(_, s)| *s)
    }

    fn next(&mut self) -> Option<(Token, Span)> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn symbol(&self, symbol: &str) -> bool {
        matches!(self.peek(), Some(Token::Symbol(s)) if *s == symbol)
    }

    fn word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Name(n)) if n == word)
    }

    fn expect(&mut self, symbol: &str) -> Result<Span, Diagnostic> {
        if self.symbol(symbol) {
            Ok(self.next().map(|(_, s)| s).unwrap_or(Span::DUMMY))
        } else {
            Err(Diagnostic::error(format!("expected `{symbol}`")).primary(self.span(), "here"))
        }
    }

    fn name(&mut self) -> Result<(String, Span), Diagnostic> {
        match self.next() {
            Some((Token::Name(n), span)) => Ok((n, span)),
            _ => {
                self.at -= 1;
                Err(Diagnostic::error("expected a name").primary(self.span(), "here"))
            }
        }
    }

    fn integer(&mut self) -> Result<usize, Diagnostic> {
        match self.next() {
            Some((Token::Number(n), _)) if n >= 0.0 && n.fract() == 0.0 => Ok(n as usize),
            _ => {
                self.at -= 1;
                Err(Diagnostic::error("expected a whole number").primary(self.span(), "here"))
            }
        }
    }

    fn expr(&mut self) -> Result<Angle, Diagnostic> {
        let mut left = self.term()?;
        while self.symbol("+") || self.symbol("-") {
            let op = if self.symbol("+") { '+' } else { '-' };
            self.at += 1;
            left = Angle::Binary(op, Box::new(left), Box::new(self.term()?));
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Angle, Diagnostic> {
        let mut left = self.unary()?;
        while self.symbol("*") || self.symbol("/") {
            let op = if self.symbol("*") { '*' } else { '/' };
            self.at += 1;
            left = Angle::Binary(op, Box::new(left), Box::new(self.unary()?));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Angle, Diagnostic> {
        if self.nesting == MAX_NESTING {
            return Err(
                Diagnostic::error("the expression nests too deeply").primary(self.span(), "here")
            );
        }
        self.nesting += 1;
        let angle = self.operand_angle();
        self.nesting -= 1;
        angle
    }

    fn operand_angle(&mut self) -> Result<Angle, Diagnostic> {
        if self.symbol("-") {
            self.at += 1;
            return Ok(Angle::Neg(Box::new(self.unary()?)));
        }
        if self.symbol("+") {
            self.at += 1;
            return self.unary();
        }
        match self.next() {
            Some((Token::Number(n), _)) => Ok(Angle::Number(n)),
            Some((Token::Name(n), span)) => {
                if self.symbol("(") {
                    self.at += 1;
                    let arg = self.expr()?;
                    self.expect(")")?;
                    Ok(Angle::Call(n, Box::new(arg), span))
                } else {
                    Ok(Angle::Name(n, span))
                }
            }
            Some((Token::Symbol("("), _)) => {
                let inner = self.expr()?;
                self.expect(")")?;
                Ok(inner)
            }
            _ => {
                self.at -= 1;
                Err(Diagnostic::error("expected an expression").primary(self.span(), "here"))
            }
        }
    }

    fn operand(&mut self) -> Result<Wire, Diagnostic> {
        let (name, span) = self.name()?;
        let index = if self.symbol("[") {
            self.at += 1;
            let index = self.integer()?;
            self.expect("]")?;
            Some(index)
        } else {
            None
        };
        Ok(Wire { name, index, span })
    }

    fn call(&mut self, name: String, span: Span) -> Result<Call, Diagnostic> {
        let mut params = Vec::new();
        if self.symbol("(") {
            self.at += 1;
            if !self.symbol(")") {
                params.push(self.expr()?);
                while self.symbol(",") {
                    self.at += 1;
                    params.push(self.expr()?);
                }
            }
            self.expect(")")?;
        }
        let mut args = vec![self.operand()?];
        while self.symbol(",") {
            self.at += 1;
            args.push(self.operand()?);
        }
        self.expect(";")?;
        Ok(Call {
            name,
            params,
            args,
            span,
        })
    }
}

struct Builder {
    program: Program,
    current: usize,
    registers: HashMap<String, Register>,
    definitions: HashMap<String, Definition>,
    qubits: usize,
    bits: usize,
    adaptive: bool,
    measured: Vec<bool>,
    branches: usize,
    emitted: usize,
}

fn op(kind: GateKind, controls: &[usize], targets: &[usize], params: &[f64], span: Span) -> Op {
    Op::Gate(Gate {
        kind,
        controls: controls.iter().map(|&q| QubitId(q as u32)).collect(),
        targets: targets.iter().map(|&q| QubitId(q as u32)).collect(),
        params: params
            .iter()
            .map(|&p| Operand::Const(Const::Float(p)))
            .collect(),
        span,
    })
}

impl Builder {
    fn push(&mut self, op: Op) {
        for q in op.qubits() {
            if self.measured[q.index()] {
                self.adaptive = true;
            }
        }
        if let Op::Measure { qubit, .. } = &op {
            self.measured[qubit.index()] = true;
        }
        self.emitted += 1;
        self.program.blocks[self.current].ops.push(op);
    }

    fn block(&mut self, label: String) -> usize {
        let id = self.program.blocks.len();
        self.program.blocks.push(Block {
            id: BlockId(id as u32),
            label,
            ops: Vec::new(),
            term: Term::Unreachable,
            span: Span::DUMMY,
        });
        id
    }

    fn declare(
        &mut self,
        name: String,
        kind: Kind,
        size: usize,
        array: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if self.registers.contains_key(&name) {
            return Err(Diagnostic::error(format!("`{name}` is already declared"))
                .primary(span, "declared again here"));
        }
        let offset = match kind {
            Kind::Qubit => {
                self.qubits += size;
                self.measured.resize(self.qubits, false);
                self.qubits - size
            }
            Kind::Bit => {
                self.bits += size;
                self.bits - size
            }
        };
        self.registers.insert(
            name,
            Register {
                kind,
                offset,
                size,
                array,
            },
        );
        Ok(())
    }

    fn resolve(&self, operand: &Wire, kind: Kind) -> Result<Vec<usize>, Diagnostic> {
        let noun = if kind == Kind::Qubit { "qubit" } else { "bit" };
        let register = self
            .registers
            .get(&operand.name)
            .filter(|r| r.kind == kind)
            .ok_or_else(|| {
                Diagnostic::error(format!(
                    "`{}` is not a declared {noun} register",
                    operand.name
                ))
                .primary(operand.span, "unknown here")
            })?;
        match operand.index {
            Some(i) if i < register.size => Ok(vec![register.offset + i]),
            Some(i) => Err(Diagnostic::error(format!(
                "index {i} is out of range for `{}`, which has {} {noun}s",
                operand.name, register.size
            ))
            .primary(operand.span, "out of range")),
            None if register.array || register.size == 1 => {
                Ok((register.offset..register.offset + register.size).collect())
            }
            None => Ok(vec![register.offset]),
        }
    }

    fn apply(
        &mut self,
        call: &Call,
        env: &HashMap<String, f64>,
        bound: &HashMap<String, usize>,
        depth: usize,
    ) -> Result<(), Diagnostic> {
        if depth > MAX_DEPTH {
            return Err(Diagnostic::error("gate definitions nest too deeply")
                .primary(call.span, "while expanding this"));
        }
        if self.emitted > MAX_OPS {
            return Err(Diagnostic::error(format!(
                "the program expands to more than {MAX_OPS} operations"
            ))
            .primary(call.span, "while expanding this"));
        }
        let params = call
            .params
            .iter()
            .map(|p| p.eval(env))
            .collect::<Result<Vec<f64>, _>>()?;
        let lists: Vec<Vec<usize>> = call
            .args
            .iter()
            .map(|a| match bound.get(&a.name) {
                Some(&q) if a.index.is_none() => Ok(vec![q]),
                _ => self.resolve(a, Kind::Qubit),
            })
            .collect::<Result<_, _>>()?;
        let width = lists.iter().map(Vec::len).max().unwrap_or(1);
        if lists.iter().any(|l| l.len() != 1 && l.len() != width) {
            return Err(Diagnostic::error(
                "registers of different sizes cannot be broadcast together",
            )
            .primary(call.span, "here"));
        }
        for i in 0..width {
            let qubits: Vec<usize> = lists
                .iter()
                .map(|l| if l.len() == 1 { l[0] } else { l[i] })
                .collect();
            for (a, q) in qubits.iter().enumerate() {
                if qubits[..a].contains(q) {
                    return Err(Diagnostic::error("a gate cannot use the same qubit twice")
                        .primary(call.span, "here"));
                }
            }
            self.gate(call, &params, &qubits, depth)?;
        }
        Ok(())
    }

    fn gate(
        &mut self,
        call: &Call,
        params: &[f64],
        qubits: &[usize],
        depth: usize,
    ) -> Result<(), Diagnostic> {
        let span = call.span;
        let want = |count: usize, arity: usize| -> Result<(), Diagnostic> {
            if params.len() != count || qubits.len() != arity {
                return Err(Diagnostic::error(format!(
                    "`{}` takes {count} parameter(s) and {arity} qubit(s), not {} and {}",
                    call.name,
                    params.len(),
                    qubits.len()
                ))
                .primary(span, "here"));
            }
            Ok(())
        };
        let simple = [
            ("x", GateKind::X),
            ("y", GateKind::Y),
            ("z", GateKind::Z),
            ("h", GateKind::H),
            ("s", GateKind::S),
            ("sdg", GateKind::SDag),
            ("t", GateKind::T),
            ("tdg", GateKind::TDag),
            ("sx", GateKind::SX),
            ("sxdg", GateKind::SXDag),
            ("id", GateKind::I),
            ("i", GateKind::I),
        ];
        let rotations = [
            ("rx", GateKind::Rx),
            ("ry", GateKind::Ry),
            ("rz", GateKind::Rz),
            ("p", GateKind::R1),
            ("phase", GateKind::R1),
            ("u1", GateKind::R1),
        ];
        let name = call.name.as_str();
        if let Some(&(_, kind)) = simple.iter().find(|(n, _)| *n == name) {
            want(0, 1)?;
            self.push(op(kind, &[], qubits, &[], span));
        } else if let Some(&(_, kind)) = rotations.iter().find(|(n, _)| *n == name) {
            want(1, 1)?;
            self.push(op(kind, &[], qubits, params, span));
        } else if let Some(kind) = name.strip_prefix('c').and_then(|base| {
            simple
                .iter()
                .chain(&rotations)
                .find(|(n, _)| *n == base)
                .map(|&(_, k)| k)
        }) {
            let count = usize::from(kind.param_count() > 0);
            want(count, 2)?;
            self.push(op(kind, &qubits[..1], &qubits[1..], params, span));
        } else {
            match name {
                "CX" | "cnot" => {
                    want(0, 2)?;
                    self.push(op(GateKind::X, &qubits[..1], &qubits[1..], &[], span));
                }
                "swap" => {
                    want(0, 2)?;
                    self.push(op(GateKind::Swap, &[], qubits, &[], span));
                }
                "ccx" | "toffoli" => {
                    want(0, 3)?;
                    self.push(op(GateKind::X, &qubits[..2], &qubits[2..], &[], span));
                }
                "ccz" => {
                    want(0, 3)?;
                    self.push(op(GateKind::Z, &qubits[..2], &qubits[2..], &[], span));
                }
                "cswap" | "fredkin" => {
                    want(0, 3)?;
                    self.push(op(GateKind::Swap, &qubits[..1], &qubits[1..], &[], span));
                }
                "U" | "u" | "u3" | "u2" => {
                    let angles = if name == "u2" {
                        want(2, 1)?;
                        [PI / 2.0, params[0], params[1]]
                    } else {
                        want(3, 1)?;
                        [params[0], params[1], params[2]]
                    };
                    self.push(op(GateKind::Rz, &[], qubits, &[angles[2]], span));
                    self.push(op(GateKind::Ry, &[], qubits, &[angles[0]], span));
                    self.push(op(GateKind::Rz, &[], qubits, &[angles[1]], span));
                }
                _ => {
                    let definition = self.definitions.get(name).cloned().ok_or_else(|| {
                        Diagnostic::error(format!("unknown gate `{name}`"))
                            .primary(span, "not a standard gate or one defined in this file")
                    })?;
                    want(definition.params.len(), definition.qubits.len())?;
                    if self.emitted.saturating_add(definition.size) > MAX_OPS {
                        return Err(Diagnostic::error(format!(
                            "the program expands to more than {MAX_OPS} operations"
                        ))
                        .primary(span, "while expanding this"));
                    }
                    let env: HashMap<String, f64> = definition
                        .params
                        .iter()
                        .cloned()
                        .zip(params.iter().copied())
                        .collect();
                    let bound: HashMap<String, usize> = definition
                        .qubits
                        .iter()
                        .cloned()
                        .zip(qubits.iter().copied())
                        .collect();
                    for inner in &definition.body {
                        self.apply(inner, &env, &bound, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn measure(
        &mut self,
        qubits: Vec<usize>,
        bits: Vec<usize>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if qubits.len() != bits.len() {
            return Err(Diagnostic::error(format!(
                "cannot measure {} qubit(s) into {} bit(s)",
                qubits.len(),
                bits.len()
            ))
            .primary(span, "here"));
        }
        for (q, b) in qubits.into_iter().zip(bits) {
            self.push(Op::Measure {
                qubit: QubitId(q as u32),
                result: ResultId(b as u32),
                span,
            });
        }
        Ok(())
    }

    fn condition(&mut self, parser: &mut Parser) -> Result<Vec<(usize, bool)>, Diagnostic> {
        parser.expect("(")?;
        let negated = parser.symbol("!");
        if negated {
            parser.at += 1;
        }
        let operand = parser.operand()?;
        let bits = self.resolve(&operand, Kind::Bit)?;
        let checks = if parser.symbol("==") || parser.symbol("!=") {
            if parser.symbol("!=") {
                return Err(Diagnostic::error(
                    "`!=` conditions are not supported, write the equal case",
                )
                .primary(parser.span(), "here"));
            }
            parser.at += 1;
            let value = parser.integer()?;
            if bits.len() < usize::BITS as usize && value >> bits.len() != 0 {
                return Err(Diagnostic::error(format!(
                    "{value} does not fit in {} bit(s)",
                    bits.len()
                ))
                .primary(operand.span, "compared here"));
            }
            bits.iter()
                .enumerate()
                .map(|(i, &b)| (b, value >> i & 1 == 1))
                .collect()
        } else if bits.len() == 1 {
            vec![(bits[0], !negated)]
        } else {
            return Err(
                Diagnostic::error("a register condition needs `==` and a value")
                    .primary(operand.span, "here"),
            );
        };
        parser.expect(")")?;
        Ok(checks)
    }

    fn branch(&mut self, parser: &mut Parser, depth: usize) -> Result<(), Diagnostic> {
        self.adaptive = true;
        self.branches += 1;
        let tag = self.branches;
        let checks = self.condition(parser)?;
        let then = self.block(format!("then_{tag}"));
        let join = self.block(format!("join_{tag}"));
        let mut from = self.current;
        let else_block = {
            self.current = then;
            self.body(parser, depth)?;
            self.program.blocks[self.current].term = Term::Br(BlockId(join as u32));
            if parser.word("else") {
                parser.at += 1;
                let other = self.block(format!("else_{tag}"));
                self.current = other;
                self.body(parser, depth)?;
                self.program.blocks[self.current].term = Term::Br(BlockId(join as u32));
                other
            } else {
                join
            }
        };
        for (index, &(bit, expected)) in checks.iter().enumerate() {
            let next = if index + 1 == checks.len() {
                then
            } else {
                self.block(format!("check_{tag}_{}", index + 1))
            };
            let value = ValueId(self.program.next_value);
            self.program.next_value += 1;
            self.program.blocks[from].ops.push(Op::Assign {
                dest: value,
                ty: Scalar::Bool,
                expr: Expr::ReadResult(ResultId(bit as u32)),
                span: Span::DUMMY,
            });
            let (yes, no) = if expected {
                (next, else_block)
            } else {
                (else_block, next)
            };
            self.program.blocks[from].term = Term::CondBr {
                cond: Operand::Value(value),
                if_true: BlockId(yes as u32),
                if_false: BlockId(no as u32),
            };
            from = next;
        }
        self.current = join;
        Ok(())
    }

    fn body(&mut self, parser: &mut Parser, depth: usize) -> Result<(), Diagnostic> {
        if parser.symbol("{") {
            parser.at += 1;
            while !parser.symbol("}") {
                if parser.peek().is_none() {
                    return Err(
                        Diagnostic::error("unclosed `{`").primary(parser.span(), "file ends here")
                    );
                }
                self.statement(parser, depth + 1)?;
            }
            parser.at += 1;
            Ok(())
        } else {
            self.statement(parser, depth + 1)
        }
    }

    fn statement(&mut self, parser: &mut Parser, depth: usize) -> Result<(), Diagnostic> {
        if depth > MAX_DEPTH {
            return Err(Diagnostic::error("blocks nest too deeply").primary(parser.span(), "here"));
        }
        let (word, span) = match parser.next() {
            Some((Token::Name(word), span)) => (word, span),
            Some((_, span)) => {
                return Err(Diagnostic::error("expected a statement").primary(span, "here"));
            }
            None => return Ok(()),
        };
        match word.as_str() {
            "OPENQASM" => {
                parser.next();
                parser.expect(";")?;
            }
            "include" => {
                parser.next();
                parser.expect(";")?;
            }
            "qubit" | "bit" | "creg" | "qreg" => {
                let kind = if word == "qubit" || word == "qreg" {
                    Kind::Qubit
                } else {
                    Kind::Bit
                };
                if word == "qreg" || word == "creg" {
                    let (name, span) = parser.name()?;
                    parser.expect("[")?;
                    let size = parser.integer()?;
                    parser.expect("]")?;
                    parser.expect(";")?;
                    self.declare(name, kind, size, true, span)?;
                } else {
                    let size = if parser.symbol("[") {
                        parser.at += 1;
                        let size = parser.integer()?;
                        parser.expect("]")?;
                        Some(size)
                    } else {
                        None
                    };
                    let (name, span) = parser.name()?;
                    parser.expect(";")?;
                    self.declare(name, kind, size.unwrap_or(1), size.is_some(), span)?;
                }
            }
            "gate" => {
                let (name, _) = parser.name()?;
                let mut params = Vec::new();
                if parser.symbol("(") {
                    parser.at += 1;
                    while !parser.symbol(")") {
                        params.push(parser.name()?.0);
                        if parser.symbol(",") {
                            parser.at += 1;
                        }
                    }
                    parser.at += 1;
                }
                let mut qubits = vec![parser.name()?.0];
                while parser.symbol(",") {
                    parser.at += 1;
                    qubits.push(parser.name()?.0);
                }
                parser.expect("{")?;
                let mut body = Vec::new();
                while !parser.symbol("}") {
                    let (inner, span) = parser.name()?;
                    if inner == "barrier" {
                        while !parser.symbol(";") {
                            parser.next();
                        }
                        parser.at += 1;
                        continue;
                    }
                    body.push(parser.call(inner, span)?);
                }
                parser.at += 1;
                let size = body.iter().fold(0usize, |total, call| {
                    let inner = self.definitions.get(&call.name).map_or(3, |d| d.size);
                    total.saturating_add(inner)
                });
                self.definitions.insert(
                    name,
                    Definition {
                        params,
                        qubits,
                        body,
                        size,
                    },
                );
            }
            "bool" | "int" | "uint" | "float" | "angle" | "complex" | "output" | "input"
            | "const" | "while" | "for" | "def" | "let" | "duration" | "stretch" | "delay"
            | "box" => {
                return Err(Diagnostic::error(format!(
                    "`{word}` is not supported yet: qirc reads OpenQASM gates, registers, measurements, resets and `if` on bits"
                ))
                .primary(span, "unsupported here"));
            }
            "opaque" => {
                return Err(
                    Diagnostic::error("opaque gates have no definition to compile")
                        .primary(span, "here"),
                );
            }
            "barrier" => {
                while !parser.symbol(";") && parser.peek().is_some() {
                    parser.next();
                }
                parser.expect(";")?;
            }
            "reset" => {
                let operand = parser.operand()?;
                parser.expect(";")?;
                self.adaptive = true;
                for q in self.resolve(&operand, Kind::Qubit)? {
                    self.push(Op::Reset {
                        qubit: QubitId(q as u32),
                        span,
                    });
                }
            }
            "measure" => {
                let qubit = parser.operand()?;
                let qubits = self.resolve(&qubit, Kind::Qubit)?;
                if parser.symbol("->") {
                    parser.at += 1;
                    let bit = parser.operand()?;
                    let bits = self.resolve(&bit, Kind::Bit)?;
                    self.measure(qubits, bits, span)?;
                } else {
                    return Err(Diagnostic::error(
                        "a measurement needs somewhere to store its result",
                    )
                    .primary(span, "write `c = measure q;` or `measure q -> c;`"));
                }
                parser.expect(";")?;
            }
            "if" => self.branch(parser, depth)?,
            _ if parser.symbol("=")
                || parser.symbol("[")
                    && self
                        .registers
                        .get(&word)
                        .is_some_and(|r| r.kind == Kind::Bit) =>
            {
                parser.at -= 1;
                let bit = parser.operand()?;
                parser.expect("=")?;
                if !parser.word("measure") {
                    return Err(
                        Diagnostic::error("only measurements can be assigned to bits")
                            .primary(parser.span(), "here"),
                    );
                }
                parser.at += 1;
                let qubit = parser.operand()?;
                parser.expect(";")?;
                let bits = self.resolve(&bit, Kind::Bit)?;
                let qubits = self.resolve(&qubit, Kind::Qubit)?;
                self.measure(qubits, bits, span)?;
            }
            _ => {
                let call = parser.call(word, span)?;
                self.apply(&call, &HashMap::new(), &HashMap::new(), 0)?;
            }
        }
        Ok(())
    }
}

pub fn is_qasm(source: &str) -> bool {
    let mut rest = source.trim_start();
    loop {
        if let Some(line) = rest.strip_prefix("//") {
            rest = line.find('\n').map_or("", |n| &line[n..]).trim_start();
        } else if let Some(block) = rest.strip_prefix("/*") {
            rest = block
                .find("*/")
                .map_or("", |n| &block[n + 2..])
                .trim_start();
        } else {
            return rest.starts_with("OPENQASM");
        }
    }
}

pub fn lower(source: &str) -> (Program, Vec<Diagnostic>) {
    let mut builder = Builder {
        program: Program::new("main", Profile::Base),
        current: 0,
        registers: HashMap::new(),
        definitions: HashMap::new(),
        qubits: 0,
        bits: 0,
        adaptive: false,
        measured: Vec::new(),
        branches: 0,
        emitted: 0,
    };
    builder.block("entry".into());
    let prelude = lex(PRELUDE).unwrap_or_default();
    let mut parser = Parser {
        tokens: &prelude,
        at: 0,
        end: PRELUDE.len(),
        nesting: 0,
    };
    while parser.peek().is_some() {
        if builder.statement(&mut parser, 0).is_err() {
            break;
        }
    }
    let parsed = lex(source).and_then(|tokens| {
        let mut parser = Parser {
            tokens: &tokens,
            at: 0,
            end: source.len(),
            nesting: 0,
        };
        while parser.peek().is_some() {
            builder.statement(&mut parser, 0)?;
        }
        Ok(())
    });
    if let Err(error) = parsed {
        let mut empty = Program::new("main", Profile::Base);
        empty.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: Vec::new(),
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        empty.num_qubits = builder.qubits as u32;
        empty.num_results = builder.bits as u32;
        return (empty, vec![error.with_code("QIR0100")]);
    }

    let end = builder.current;
    let bits = builder.bits;
    let ops = &mut builder.program.blocks[end].ops;
    if bits > 0 {
        ops.push(Op::RecordOutput {
            kind: OutputKind::Array,
            result: None,
            value: None,
            count: Some(bits as i64),
            label: None,
            span: Span::DUMMY,
        });
        for bit in 0..bits {
            ops.push(Op::RecordOutput {
                kind: OutputKind::Result,
                result: Some(ResultId(bit as u32)),
                value: None,
                count: None,
                label: None,
                span: Span::DUMMY,
            });
        }
    }
    builder.program.blocks[end].term = Term::Ret(None);
    builder.program.num_qubits = builder.qubits as u32;
    builder.program.num_results = bits as u32;
    if builder.adaptive {
        builder.program.profile = Profile::Adaptive;
    }
    (builder.program, Vec::new())
}
