use crate::ast::*;
use crate::diag::{Diagnostic, Span};
use crate::lex::{self, Token, TokenKind};

pub fn parse_module(src: &str) -> (Module, Vec<Diagnostic>) {
    let (tokens, lex_errors) = lex::tokenize(src);
    let mut parser = Parser::new(src, &tokens);
    parser.diagnostics.extend(lex_errors);
    let module = parser.parse_module();
    (module, parser.diagnostics)
}

const TYPE_KEYWORDS: &[&str] = &[
    "void",
    "half",
    "bfloat",
    "float",
    "double",
    "x86_fp80",
    "fp128",
    "ppc_fp128",
    "ptr",
    "label",
    "metadata",
    "token",
    "opaque",
];

const VALUE_KEYWORDS: &[&str] = &[
    "null",
    "none",
    "undef",
    "poison",
    "zeroinitializer",
    "true",
    "false",
];

const TERMINATOR_KEYWORDS: &[&str] = &[
    "ret",
    "br",
    "switch",
    "unreachable",
    "invoke",
    "resume",
    "callbr",
    "catchret",
    "cleanupret",
    "catchswitch",
];

fn is_int_type_keyword(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('i') else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_type_keyword(text: &str) -> bool {
    TYPE_KEYWORDS.contains(&text) || is_int_type_keyword(text)
}

fn is_value_keyword(text: &str) -> bool {
    VALUE_KEYWORDS.contains(&text)
        || CastOp::from_keyword(text).is_some()
        || BinOp::from_keyword(text).is_some()
        || matches!(
            text,
            "getelementptr" | "select" | "icmp" | "fcmp" | "blockaddress"
        )
}

struct Parser<'a> {
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    next_unnamed: u32,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: &'a [Token]) -> Self {
        Self {
            src,
            tokens,
            pos: 0,
            diagnostics: Vec::new(),
            next_unnamed: 0,
        }
    }

    fn peek(&self) -> Token {
        self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_at(&self, offset: usize) -> Token {
        self.tokens[(self.pos + offset).min(self.tokens.len() - 1)]
    }

    fn text(&self, token: Token) -> &'a str {
        token.text(self.src)
    }

    fn cur_text(&self) -> &'a str {
        self.text(self.peek())
    }

    fn at_eof(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn bump(&mut self) -> Token {
        let token = self.peek();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        token
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        self.peek().kind == TokenKind::Ident && self.cur_text() == keyword
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, keyword: &str) -> bool {
        if self.at_keyword(keyword) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Option<Token> {
        if self.at(kind) {
            return Some(self.bump());
        }

        let token = self.peek();
        self.error(
            format!(
                "expected {}, found {}",
                kind.describe(),
                token.kind.describe()
            ),
            token.span,
            "unexpected here",
        );
        None
    }

    fn error(&mut self, message: impl Into<String>, span: Span, label: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(message)
                .with_code("QIR0100")
                .primary(span, label),
        );
    }

    fn starts_new_line(&self, index: usize) -> bool {
        if index == 0 {
            return true;
        }
        let prev_end = self.tokens[index - 1].span.end as usize;
        let cur_start = self.tokens[index.min(self.tokens.len() - 1)].span.start as usize;
        self.src[prev_end..cur_start.max(prev_end)].contains('\n')
    }

    fn skip_to_next_line(&mut self) {
        let line = self.pos;
        while !self.at_eof() && (self.pos == line || !self.starts_new_line(self.pos)) {
            self.pos += 1;
        }
    }

    fn skip_balanced(&mut self, open: TokenKind, close: TokenKind) {
        if !self.eat(open) {
            return;
        }
        let mut depth = 1usize;
        while depth > 0 && !self.at_eof() {
            let token = self.bump();
            if token.kind == open {
                depth += 1;
            } else if token.kind == close {
                depth -= 1;
            }
        }
    }

    fn parse_module(&mut self) -> Module {
        let mut module = Module::default();

        while !self.at_eof() {
            let before = self.pos;
            self.parse_top_level(&mut module);
            if self.pos == before {
                self.pos += 1;
            }
        }

        module
    }

    fn parse_top_level(&mut self, module: &mut Module) {
        let token = self.peek();

        match token.kind {
            TokenKind::Ident => match self.cur_text() {
                "source_filename" => {
                    self.bump();
                    self.eat(TokenKind::Equal);
                    if let Some(t) = self.expect(TokenKind::StringLit) {
                        module.source_filename = Some(lex::decode_name(self.text(t)).into_owned());
                    }
                }
                "target" => {
                    self.bump();
                    let which = self.cur_text().to_string();
                    self.bump();
                    self.eat(TokenKind::Equal);
                    if let Some(t) = self.expect(TokenKind::StringLit) {
                        let value = lex::decode_name(self.text(t)).into_owned();
                        if which == "triple" {
                            module.triple = Some(value);
                        } else {
                            module.datalayout = Some(value);
                        }
                    }
                }
                "declare" => {
                    if let Some(sig) = self.parse_declare() {
                        module.declarations.push(sig);
                    }
                }
                "define" => {
                    if let Some(function) = self.parse_define() {
                        module.functions.push(function);
                    }
                }
                "attributes" => {
                    if let Some(group) = self.parse_attr_group() {
                        module.attr_groups.push(group);
                    }
                }
                _ => self.skip_to_next_line(),
            },
            TokenKind::LocalIdent => {
                if self.peek_at(1).kind == TokenKind::Equal && self.text(self.peek_at(2)) == "type"
                {
                    if let Some(def) = self.parse_type_def() {
                        module.type_defs.push(def);
                    }
                } else {
                    self.skip_to_next_line();
                }
            }
            TokenKind::GlobalIdent => {
                if let Some(global) = self.parse_global() {
                    module.globals.push(global);
                }
            }
            TokenKind::MetadataIdent => {
                let name_token = self.peek();
                let name = lex::decode_name(self.text(name_token)).into_owned();

                if name.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty() {
                    if let Some(def) = self.parse_metadata_def() {
                        module.metadata.push(def);
                    }
                } else if let Some(named) = self.parse_named_metadata() {
                    module.named_metadata.push(named);
                }
            }
            _ => self.skip_to_next_line(),
        }
    }

    fn parse_type_def(&mut self) -> Option<TypeDef> {
        let start = self.peek().span;
        let name_token = self.expect(TokenKind::LocalIdent)?;
        let name = lex::decode_name(self.text(name_token)).into_owned();
        self.expect(TokenKind::Equal)?;
        self.eat_keyword("type");
        let ty = self.parse_type()?;

        Some(TypeDef {
            name,
            ty,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_global(&mut self) -> Option<GlobalVar> {
        let start = self.peek().span;
        let name_token = self.expect(TokenKind::GlobalIdent)?;
        let name = lex::decode_name(self.text(name_token)).into_owned();
        self.expect(TokenKind::Equal)?;

        let mut linkage = Vec::new();
        let mut is_constant = false;

        loop {
            if self.at(TokenKind::Ident) {
                let text = self.cur_text();
                if text == "constant" {
                    is_constant = true;
                    self.bump();
                    break;
                }
                if text == "global" {
                    self.bump();
                    break;
                }
                if is_type_keyword(text) {
                    break;
                }
                linkage.push(text.to_string());
                self.bump();
                if self.at(TokenKind::LParen) {
                    self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                }
                continue;
            }
            break;
        }

        let ty = self.parse_type()?;

        let initializer = if self.at(TokenKind::Comma) || self.starts_new_line(self.pos) {
            None
        } else {
            self.parse_value()
        };

        while self.eat(TokenKind::Comma) {
            self.skip_to_next_line();
            break;
        }

        Some(GlobalVar {
            name,
            ty,
            initializer,
            is_constant,
            linkage,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_attr_group(&mut self) -> Option<AttrGroup> {
        let start = self.peek().span;
        self.eat_keyword("attributes");
        let id_token = self.expect(TokenKind::AttrGroupId)?;
        let id = lex::decode_name(self.text(id_token)).into_owned();
        self.expect(TokenKind::Equal)?;
        self.expect(TokenKind::LBrace)?;

        let mut attrs = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            attrs.push(self.parse_attribute());
        }
        self.eat(TokenKind::RBrace);

        Some(AttrGroup {
            id,
            attrs,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_attribute(&mut self) -> Attribute {
        let token = self.bump();
        let raw = self.text(token);

        let key = if token.kind == TokenKind::StringLit {
            lex::decode_name(raw).into_owned()
        } else {
            raw.to_string()
        };

        if self.at(TokenKind::LParen) {
            let open = self.pos;
            self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
            let close = self.tokens[self.pos.saturating_sub(1)].span;
            let inner = &self.src[self.tokens[open].span.end as usize..close.start as usize];
            return Attribute::KeyValue(key, inner.trim().to_string());
        }

        if self.eat(TokenKind::Equal) {
            let value_token = self.bump();
            let raw_value = self.text(value_token);
            let value = if value_token.kind == TokenKind::StringLit {
                lex::decode_name(raw_value).into_owned()
            } else {
                raw_value.to_string()
            };
            return Attribute::KeyValue(key, value);
        }

        Attribute::Flag(key)
    }

    fn parse_named_metadata(&mut self) -> Option<NamedMetadata> {
        let start = self.peek().span;
        let name_token = self.expect(TokenKind::MetadataIdent)?;
        let name = lex::decode_name(self.text(name_token)).into_owned();
        self.expect(TokenKind::Equal)?;
        self.expect(TokenKind::Bang)?;
        self.expect(TokenKind::LBrace)?;

        let mut operands = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            if self.at(TokenKind::MetadataIdent) {
                let token = self.bump();
                operands.push(lex::decode_name(self.text(token)).into_owned());
            } else {
                self.bump();
            }
            self.eat(TokenKind::Comma);
        }
        self.eat(TokenKind::RBrace);

        Some(NamedMetadata {
            name,
            operands,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_metadata_def(&mut self) -> Option<MetadataDef> {
        let start = self.peek().span;
        let id_token = self.expect(TokenKind::MetadataIdent)?;
        let id = lex::decode_name(self.text(id_token)).into_owned();
        self.expect(TokenKind::Equal)?;

        let distinct = self.eat_keyword("distinct");

        if self.at(TokenKind::Ident) {
            let kind = self.cur_text().to_string();
            self.bump();
            let open = self.pos;
            self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
            let body = if self.pos > open {
                let close = self.tokens[self.pos - 1].span;
                self.src[self.tokens[open].span.end as usize..close.start as usize].to_string()
            } else {
                String::new()
            };
            return Some(MetadataDef {
                id,
                distinct,
                node: MetadataNode::Specialized { kind, body },
                span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
            });
        }

        self.expect(TokenKind::Bang)?;
        self.expect(TokenKind::LBrace)?;

        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            items.push(self.parse_metadata_item());
            self.eat(TokenKind::Comma);
        }
        self.eat(TokenKind::RBrace);

        Some(MetadataDef {
            id,
            distinct,
            node: MetadataNode::Tuple(items),
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_metadata_item(&mut self) -> MetadataItem {
        if self.at(TokenKind::Bang) && self.peek_at(1).kind == TokenKind::LBrace {
            self.bump();
            self.bump();
            let mut items = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at_eof() {
                let before = self.pos;
                items.push(self.parse_metadata_item());
                if self.pos == before {
                    self.bump();
                }
                self.eat(TokenKind::Comma);
            }
            self.eat(TokenKind::RBrace);
            return MetadataItem::Node(items);
        }

        if self.at(TokenKind::MetadataIdent) {
            let token = self.bump();
            let raw = self.text(token);
            let decoded = lex::decode_name(raw).into_owned();
            if raw.starts_with("!\"") {
                return MetadataItem::Str(decoded);
            }
            return MetadataItem::Ref(decoded);
        }

        if self.at_keyword("null") {
            self.bump();
            return MetadataItem::Null;
        }

        match self.parse_typed_value() {
            Some(tv) => MetadataItem::Value(tv),
            None => {
                self.bump();
                MetadataItem::Null
            }
        }
    }

    fn skip_leading_modifiers(&mut self) {
        loop {
            match self.peek().kind {
                TokenKind::Ident => {
                    let text = self.cur_text();
                    if is_type_keyword(text) {
                        return;
                    }
                    self.bump();
                    if self.at(TokenKind::LParen) {
                        self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                    }
                }
                TokenKind::StringLit => {
                    self.bump();
                    if self.eat(TokenKind::Equal) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn parse_signature(&mut self) -> Option<FuncSig> {
        let start = self.peek().span;
        self.skip_leading_modifiers();

        let ret_ty = self.parse_type()?;
        let name_token = self.expect(TokenKind::GlobalIdent)?;
        let name = lex::decode_name(self.text(name_token)).into_owned();

        self.expect(TokenKind::LParen)?;

        let mut params = Vec::new();
        let mut varargs = false;

        while !self.at(TokenKind::RParen) && !self.at_eof() {
            if self.eat(TokenKind::Ellipsis) {
                varargs = true;
                break;
            }

            let param_start = self.peek().span;
            let Some(ty) = self.parse_type() else {
                break;
            };
            let attrs = self.parse_param_attrs();

            let param_name = if self.at(TokenKind::LocalIdent) {
                let token = self.bump();
                Some(lex::decode_name(self.text(token)).into_owned())
            } else {
                None
            };

            params.push(Param {
                ty,
                name: param_name,
                attrs,
                span: param_start.to(self.tokens[self.pos.saturating_sub(1)].span),
            });

            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;

        let mut attr_groups = Vec::new();
        let mut attrs = Vec::new();

        while !self.at_eof() && !self.at(TokenKind::LBrace) && !self.starts_new_line(self.pos) {
            if self.at(TokenKind::AttrGroupId) {
                let token = self.bump();
                attr_groups.push(lex::decode_name(self.text(token)).into_owned());
                continue;
            }
            if self.at(TokenKind::Ident) || self.at(TokenKind::StringLit) {
                attrs.push(self.parse_attribute());
                continue;
            }
            self.bump();
        }

        Some(FuncSig {
            name,
            ret_ty,
            params,
            varargs,
            attr_groups,
            attrs,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_param_attrs(&mut self) -> Vec<String> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::Ident) {
            let text = self.cur_text();
            if is_value_keyword(text) {
                break;
            }
            attrs.push(text.to_string());
            self.bump();
            if self.at(TokenKind::LParen) {
                self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
            }
        }
        attrs
    }

    fn parse_declare(&mut self) -> Option<FuncSig> {
        self.eat_keyword("declare");
        let sig = self.parse_signature();
        if sig.is_none() {
            self.skip_to_next_line();
        }
        sig
    }

    fn parse_define(&mut self) -> Option<Function> {
        let start = self.peek().span;
        self.eat_keyword("define");

        self.next_unnamed = 0;

        let sig = match self.parse_signature() {
            Some(sig) => sig,
            None => {
                self.skip_to_next_line();
                return None;
            }
        };

        for param in &sig.params {
            if let Some(name) = &param.name {
                self.observe_numbered_name(name);
            } else {
                self.next_unnamed += 1;
            }
        }

        self.expect(TokenKind::LBrace)?;

        let mut blocks = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            if let Some(block) = self.parse_block() {
                blocks.push(block);
            }
            if self.pos == before {
                self.pos += 1;
            }
        }
        self.eat(TokenKind::RBrace);

        Some(Function {
            sig,
            blocks,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn observe_numbered_name(&mut self, name: &str) {
        if let Ok(n) = name.parse::<u32>() {
            self.next_unnamed = self.next_unnamed.max(n + 1);
        }
    }

    fn at_label(&self) -> bool {
        if !self.starts_new_line(self.pos) {
            return false;
        }
        matches!(self.peek().kind, TokenKind::Ident | TokenKind::IntLit)
            && self.peek_at(1).kind == TokenKind::Colon
    }

    fn at_terminator(&self) -> bool {
        self.peek().kind == TokenKind::Ident && TERMINATOR_KEYWORDS.contains(&self.cur_text())
    }

    fn parse_block(&mut self) -> Option<BasicBlock> {
        let start = self.peek().span;

        let label = if self.at_label() {
            let token = self.bump();
            self.bump();
            let name = self.text(token).to_string();
            self.observe_numbered_name(&name);
            name
        } else {
            let name = self.next_unnamed.to_string();
            self.next_unnamed += 1;
            name
        };

        let mut instructions = Vec::new();
        let mut terminator = None;

        while !self.at(TokenKind::RBrace) && !self.at_eof() && !self.at_label() {
            if self.at_terminator() {
                terminator = self.parse_terminator();
                break;
            }

            let before = self.pos;
            if let Some(inst) = self.parse_instruction() {
                instructions.push(inst);
            }
            if self.pos == before {
                self.skip_to_next_line();
            }
        }

        Some(BasicBlock {
            label,
            instructions,
            terminator: terminator.unwrap_or(Terminator::Unreachable),
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_terminator(&mut self) -> Option<Terminator> {
        let keyword = self.cur_text().to_string();
        self.bump();

        let term = match keyword.as_str() {
            "unreachable" => Terminator::Unreachable,
            "ret" => {
                if self.at_keyword("void") {
                    self.bump();
                    Terminator::Ret(None)
                } else {
                    Terminator::Ret(self.parse_typed_value())
                }
            }
            "br" => {
                if self.at_keyword("label") {
                    self.bump();
                    let token = self.expect(TokenKind::LocalIdent)?;
                    Terminator::Br {
                        target: lex::decode_name(self.text(token)).into_owned(),
                    }
                } else {
                    let cond = self.parse_typed_value()?;
                    self.expect(TokenKind::Comma)?;
                    self.eat_keyword("label");
                    let then_token = self.expect(TokenKind::LocalIdent)?;
                    self.expect(TokenKind::Comma)?;
                    self.eat_keyword("label");
                    let else_token = self.expect(TokenKind::LocalIdent)?;
                    Terminator::CondBr {
                        cond,
                        if_true: lex::decode_name(self.text(then_token)).into_owned(),
                        if_false: lex::decode_name(self.text(else_token)).into_owned(),
                    }
                }
            }
            "switch" => {
                let scrutinee = self.parse_typed_value()?;
                self.expect(TokenKind::Comma)?;
                self.eat_keyword("label");
                let default_token = self.expect(TokenKind::LocalIdent)?;
                let default = lex::decode_name(self.text(default_token)).into_owned();

                self.expect(TokenKind::LBracket)?;
                let mut cases = Vec::new();
                while !self.at(TokenKind::RBracket) && !self.at_eof() {
                    let Some(value) = self.parse_typed_value() else {
                        break;
                    };
                    self.expect(TokenKind::Comma)?;
                    self.eat_keyword("label");
                    let target = self.expect(TokenKind::LocalIdent)?;
                    cases.push((value, lex::decode_name(self.text(target)).into_owned()));
                    self.eat(TokenKind::Comma);
                }
                self.eat(TokenKind::RBracket);

                Terminator::Switch {
                    scrutinee,
                    default,
                    cases,
                }
            }
            _ => {
                self.skip_to_next_line();
                Terminator::Unreachable
            }
        };

        self.consume_trailing_metadata();
        Some(term)
    }

    fn consume_trailing_metadata(&mut self) {
        while self.at(TokenKind::Comma) && self.peek_at(1).kind == TokenKind::MetadataIdent {
            self.bump();
            self.bump();
            if self.at(TokenKind::MetadataIdent) || self.at(TokenKind::Bang) {
                self.bump();
                if self.at(TokenKind::LBrace) {
                    self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                }
            }
        }
    }

    fn consume_trailing_suffixes(&mut self) {
        loop {
            if self.at(TokenKind::Comma) && self.text(self.peek_at(1)) == "align" {
                self.bump();
                self.bump();
                self.eat(TokenKind::IntLit);
                continue;
            }
            if self.at(TokenKind::Comma) && self.peek_at(1).kind == TokenKind::MetadataIdent {
                self.consume_trailing_metadata();
                continue;
            }
            break;
        }
    }

    fn parse_instruction(&mut self) -> Option<Instruction> {
        let start = self.peek().span;

        let result = if self.at(TokenKind::LocalIdent) && self.peek_at(1).kind == TokenKind::Equal {
            let token = self.bump();
            self.bump();
            let name = lex::decode_name(self.text(token)).into_owned();
            self.observe_numbered_name(&name);
            Some(name)
        } else {
            None
        };

        let kind = self.parse_inst_kind()?;
        self.consume_trailing_suffixes();

        Some(Instruction {
            result,
            kind,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_inst_kind(&mut self) -> Option<InstKind> {
        if !self.at(TokenKind::Ident) {
            let token = self.peek();
            self.error(
                format!("expected an instruction, found {}", token.kind.describe()),
                token.span,
                "not an opcode",
            );
            self.skip_to_next_line();
            return None;
        }

        let opcode = self.cur_text().to_string();

        if matches!(opcode.as_str(), "tail" | "musttail" | "notail") {
            self.bump();
            return self.parse_inst_kind_tail(true);
        }

        self.parse_inst_kind_tail(false)
    }

    fn parse_inst_kind_tail(&mut self, tail: bool) -> Option<InstKind> {
        let opcode = self.cur_text().to_string();
        let opcode_span = self.peek().span;

        if opcode == "call" {
            self.bump();
            return self.parse_call(tail, opcode_span).map(InstKind::Call);
        }

        if let Some(op) = BinOp::from_keyword(&opcode) {
            self.bump();
            while self.at(TokenKind::Ident) && !is_type_keyword(self.cur_text()) {
                self.bump();
            }
            let ty = self.parse_type()?;
            let lhs = self.parse_value()?;
            self.expect(TokenKind::Comma)?;
            let rhs = self.parse_value()?;
            return Some(InstKind::Binary { op, ty, lhs, rhs });
        }

        if let Some(op) = CastOp::from_keyword(&opcode) {
            self.bump();
            let operand = self.parse_typed_value()?;
            self.eat_keyword("to");
            let to = self.parse_type()?;
            return Some(InstKind::Cast { op, operand, to });
        }

        match opcode.as_str() {
            "icmp" => {
                self.bump();
                let pred_token = self.bump();
                let pred = IntPredicate::from_keyword(self.text(pred_token))?;
                let ty = self.parse_type()?;
                let lhs = self.parse_value()?;
                self.expect(TokenKind::Comma)?;
                let rhs = self.parse_value()?;
                Some(InstKind::ICmp { pred, ty, lhs, rhs })
            }
            "fcmp" => {
                self.bump();
                while self.at(TokenKind::Ident)
                    && FloatPredicate::from_keyword(self.cur_text()).is_none()
                {
                    self.bump();
                }
                let pred_token = self.bump();
                let pred = FloatPredicate::from_keyword(self.text(pred_token))?;
                let ty = self.parse_type()?;
                let lhs = self.parse_value()?;
                self.expect(TokenKind::Comma)?;
                let rhs = self.parse_value()?;
                Some(InstKind::FCmp { pred, ty, lhs, rhs })
            }
            "select" => {
                self.bump();
                let cond = self.parse_typed_value()?;
                self.expect(TokenKind::Comma)?;
                let if_true = self.parse_typed_value()?;
                self.expect(TokenKind::Comma)?;
                let if_false = self.parse_typed_value()?;
                Some(InstKind::Select {
                    cond,
                    if_true,
                    if_false,
                })
            }
            "phi" => {
                self.bump();
                while self.at(TokenKind::Ident) && !is_type_keyword(self.cur_text()) {
                    self.bump();
                }
                let ty = self.parse_type()?;
                let mut incoming = Vec::new();
                loop {
                    self.expect(TokenKind::LBracket)?;
                    let value = self.parse_value()?;
                    self.expect(TokenKind::Comma)?;
                    let label_token = self.expect(TokenKind::LocalIdent)?;
                    incoming.push((value, lex::decode_name(self.text(label_token)).into_owned()));
                    self.expect(TokenKind::RBracket)?;
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                Some(InstKind::Phi { ty, incoming })
            }
            "alloca" => {
                self.bump();
                self.eat_keyword("inalloca");
                let ty = self.parse_type()?;
                let count = if self.at(TokenKind::Comma)
                    && self.text(self.peek_at(1)) != "align"
                    && self.peek_at(1).kind != TokenKind::MetadataIdent
                {
                    self.bump();
                    self.parse_typed_value()
                } else {
                    None
                };
                Some(InstKind::Alloca { ty, count })
            }
            "load" => {
                self.bump();
                self.eat_keyword("volatile");
                let ty = self.parse_type()?;
                self.expect(TokenKind::Comma)?;
                let ptr = self.parse_typed_value()?;
                Some(InstKind::Load { ty, ptr })
            }
            "store" => {
                self.bump();
                self.eat_keyword("volatile");
                let value = self.parse_typed_value()?;
                self.expect(TokenKind::Comma)?;
                let ptr = self.parse_typed_value()?;
                Some(InstKind::Store { value, ptr })
            }
            "getelementptr" => {
                self.bump();
                let inbounds = self.eat_keyword("inbounds");
                let base_ty = self.parse_type()?;
                self.expect(TokenKind::Comma)?;
                let ptr = self.parse_typed_value()?;
                let mut indices = Vec::new();
                while self.eat(TokenKind::Comma) {
                    if self.peek().kind == TokenKind::MetadataIdent {
                        self.pos -= 1;
                        break;
                    }
                    let Some(index) = self.parse_typed_value() else {
                        break;
                    };
                    indices.push(index);
                }
                Some(InstKind::GetElementPtr {
                    inbounds,
                    base_ty,
                    ptr,
                    indices,
                })
            }
            "extractvalue" => {
                self.bump();
                let aggregate = self.parse_typed_value()?;
                let mut indices = Vec::new();
                while self.eat(TokenKind::Comma) {
                    let token = self.expect(TokenKind::IntLit)?;
                    indices.push(self.text(token).parse::<u64>().unwrap_or(0));
                }
                Some(InstKind::ExtractValue { aggregate, indices })
            }
            "insertvalue" => {
                self.bump();
                let aggregate = self.parse_typed_value()?;
                self.expect(TokenKind::Comma)?;
                let value = self.parse_typed_value()?;
                let mut indices = Vec::new();
                while self.eat(TokenKind::Comma) {
                    let token = self.expect(TokenKind::IntLit)?;
                    indices.push(self.text(token).parse::<u64>().unwrap_or(0));
                }
                Some(InstKind::InsertValue {
                    aggregate,
                    value,
                    indices,
                })
            }
            "freeze" => {
                self.bump();
                Some(InstKind::Freeze(self.parse_typed_value()?))
            }
            "fence" => {
                self.bump();
                self.skip_to_next_line();
                Some(InstKind::Fence)
            }
            _ => {
                self.bump();
                self.skip_to_next_line();
                Some(InstKind::Unsupported { opcode })
            }
        }
    }

    fn parse_call(&mut self, tail: bool, start: Span) -> Option<Call> {
        while self.at(TokenKind::Ident) && !is_type_keyword(self.cur_text()) {
            self.bump();
            if self.at(TokenKind::LParen) && self.text(self.peek_at(1)) == "addrspace" {
                self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
            }
        }

        let first_ty = self.parse_type()?;

        let (ret_ty, explicit_fn_ty) = match &first_ty {
            Ty::Func { ret, .. } => ((**ret).clone(), Some(first_ty.clone())),
            _ => (first_ty, None),
        };

        let callee = self.parse_value()?;

        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let arg_start = self.peek().span;
            let Some(ty) = self.parse_type() else {
                break;
            };
            let attrs = self.parse_param_attrs();
            let Some(value) = self.parse_value() else {
                break;
            };
            args.push(Argument {
                ty,
                attrs,
                value,
                span: arg_start.to(self.tokens[self.pos.saturating_sub(1)].span),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;

        let mut attr_groups = Vec::new();
        while self.at(TokenKind::AttrGroupId) {
            let token = self.bump();
            attr_groups.push(lex::decode_name(self.text(token)).into_owned());
        }

        Some(Call {
            tail,
            ret_ty,
            explicit_fn_ty,
            callee,
            args,
            attr_groups,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_type(&mut self) -> Option<Ty> {
        let token = self.peek();

        let mut ty = match token.kind {
            TokenKind::Ident => {
                let text = self.cur_text();
                if let Some(rest) = text.strip_prefix('i') {
                    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                        self.bump();
                        Ty::Int(rest.parse().unwrap_or(32))
                    } else {
                        return self.type_error(token);
                    }
                } else {
                    let ty = match text {
                        "void" => Ty::Void,
                        "half" | "bfloat" => Ty::Half,
                        "float" => Ty::Float,
                        "double" => Ty::Double,
                        "x86_fp80" => Ty::X86Fp80,
                        "fp128" | "ppc_fp128" => Ty::Fp128,
                        "ptr" => Ty::Ptr(None),
                        "label" => Ty::Label,
                        "metadata" => Ty::Metadata,
                        "token" => Ty::Token,
                        "opaque" => Ty::Opaque,
                        _ => return self.type_error(token),
                    };
                    self.bump();
                    ty
                }
            }
            TokenKind::LocalIdent => {
                self.bump();
                Ty::Named(lex::decode_name(self.text(token)).into_owned())
            }
            TokenKind::LBracket => {
                self.bump();
                let len_token = self.expect(TokenKind::IntLit)?;
                let len = self.text(len_token).parse::<u64>().unwrap_or(0);
                self.eat_keyword("x");
                let elem = self.parse_type()?;
                self.expect(TokenKind::RBracket)?;
                Ty::Array(len, Box::new(elem))
            }
            TokenKind::LBrace => {
                self.bump();
                let fields = self.parse_type_list(TokenKind::RBrace)?;
                Ty::Struct {
                    fields,
                    packed: false,
                }
            }
            TokenKind::Less => {
                self.bump();
                if self.at(TokenKind::LBrace) {
                    self.bump();
                    let fields = self.parse_type_list(TokenKind::RBrace)?;
                    self.expect(TokenKind::Greater)?;
                    Ty::Struct {
                        fields,
                        packed: true,
                    }
                } else {
                    let scalable = self.eat_keyword("vscale");
                    if scalable {
                        self.eat_keyword("x");
                    }
                    let len_token = self.expect(TokenKind::IntLit)?;
                    let len = self.text(len_token).parse::<u64>().unwrap_or(0);
                    self.eat_keyword("x");
                    let elem = self.parse_type()?;
                    self.expect(TokenKind::Greater)?;
                    Ty::Vector {
                        len,
                        scalable,
                        elem: Box::new(elem),
                    }
                }
            }
            _ => return self.type_error(token),
        };

        loop {
            if self.at(TokenKind::Star) {
                self.bump();
                ty = Ty::Ptr(Some(Box::new(ty)));
                continue;
            }

            if self.at(TokenKind::LParen) {
                self.bump();
                let mut params = Vec::new();
                let mut varargs = false;
                while !self.at(TokenKind::RParen) && !self.at_eof() {
                    if self.eat(TokenKind::Ellipsis) {
                        varargs = true;
                        break;
                    }
                    let Some(param) = self.parse_type() else {
                        break;
                    };
                    self.parse_param_attrs();
                    if self.at(TokenKind::LocalIdent) {
                        self.bump();
                    }
                    params.push(param);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
                ty = Ty::Func {
                    ret: Box::new(ty),
                    params,
                    varargs,
                };
                continue;
            }

            break;
        }

        Some(ty)
    }

    fn parse_type_list(&mut self, close: TokenKind) -> Option<Vec<Ty>> {
        let mut fields = Vec::new();
        while !self.at(close) && !self.at_eof() {
            let field = self.parse_type()?;
            fields.push(field);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(close)?;
        Some(fields)
    }

    fn type_error(&mut self, token: Token) -> Option<Ty> {
        self.error(
            format!("expected a type, found {}", token.kind.describe()),
            token.span,
            "not a type",
        );
        None
    }

    fn parse_typed_value(&mut self) -> Option<TypedValue> {
        let start = self.peek().span;
        let ty = self.parse_type()?;
        self.parse_param_attrs();
        let value = self.parse_value()?;
        Some(TypedValue {
            ty,
            value,
            span: start.to(self.tokens[self.pos.saturating_sub(1)].span),
        })
    }

    fn parse_value(&mut self) -> Option<Value> {
        let token = self.peek();

        match token.kind {
            TokenKind::LocalIdent => {
                self.bump();
                Some(Value::Local(
                    lex::decode_name(self.text(token)).into_owned(),
                ))
            }
            TokenKind::GlobalIdent => {
                self.bump();
                Some(Value::Global(
                    lex::decode_name(self.text(token)).into_owned(),
                ))
            }
            TokenKind::IntLit => {
                self.bump();
                Some(Value::Int(lex::parse_int(self.text(token)).unwrap_or(0)))
            }
            TokenKind::FloatLit => {
                self.bump();
                Some(Value::Float(
                    lex::parse_float(self.text(token)).unwrap_or(0.0),
                ))
            }
            TokenKind::CStringLit => {
                self.bump();
                Some(Value::Bytes(lex::decode_cstring(self.text(token))))
            }
            TokenKind::StringLit => {
                self.bump();
                Some(Value::MetadataString(
                    lex::decode_name(self.text(token)).into_owned(),
                ))
            }
            TokenKind::MetadataIdent => {
                self.bump();
                let raw = self.text(token);
                let decoded = lex::decode_name(raw).into_owned();
                if raw.starts_with("!\"") {
                    Some(Value::MetadataString(decoded))
                } else {
                    Some(Value::MetadataRef(decoded))
                }
            }
            TokenKind::LBracket => {
                self.bump();
                let items = self.parse_value_list(TokenKind::RBracket)?;
                Some(Value::Aggregate(items))
            }
            TokenKind::LBrace => {
                self.bump();
                let items = self.parse_value_list(TokenKind::RBrace)?;
                Some(Value::Aggregate(items))
            }
            TokenKind::Less => {
                self.bump();
                let items = self.parse_value_list(TokenKind::Greater)?;
                Some(Value::Aggregate(items))
            }
            TokenKind::Ident => {
                let text = self.cur_text();

                match text {
                    "null" => {
                        self.bump();
                        return Some(Value::Null);
                    }
                    "none" => {
                        self.bump();
                        return Some(Value::NoneValue);
                    }
                    "undef" => {
                        self.bump();
                        return Some(Value::Undef);
                    }
                    "poison" => {
                        self.bump();
                        return Some(Value::Poison);
                    }
                    "zeroinitializer" => {
                        self.bump();
                        return Some(Value::ZeroInit);
                    }
                    "true" => {
                        self.bump();
                        return Some(Value::Bool(true));
                    }
                    "false" => {
                        self.bump();
                        return Some(Value::Bool(false));
                    }
                    _ => {}
                }

                if let Some(op) = CastOp::from_keyword(text) {
                    self.bump();
                    self.expect(TokenKind::LParen)?;
                    let operand = self.parse_typed_value()?;
                    self.eat_keyword("to");
                    let to = self.parse_type()?;
                    self.expect(TokenKind::RParen)?;
                    return Some(Value::ConstExpr(Box::new(ConstExpr::Cast {
                        op,
                        operand,
                        to,
                    })));
                }

                if text == "getelementptr" {
                    self.bump();
                    let inbounds = self.eat_keyword("inbounds");
                    self.expect(TokenKind::LParen)?;
                    let base_ty = self.parse_type()?;
                    self.expect(TokenKind::Comma)?;
                    let ptr = self.parse_typed_value()?;
                    let mut indices = Vec::new();
                    while self.eat(TokenKind::Comma) {
                        let Some(index) = self.parse_typed_value() else {
                            break;
                        };
                        indices.push(index);
                    }
                    self.expect(TokenKind::RParen)?;
                    return Some(Value::ConstExpr(Box::new(ConstExpr::GetElementPtr {
                        inbounds,
                        base_ty,
                        ptr,
                        indices,
                    })));
                }

                if let Some(op) = BinOp::from_keyword(text) {
                    self.bump();
                    self.expect(TokenKind::LParen)?;
                    let lhs = self.parse_typed_value()?;
                    self.expect(TokenKind::Comma)?;
                    let rhs = self.parse_typed_value()?;
                    self.expect(TokenKind::RParen)?;
                    return Some(Value::ConstExpr(Box::new(ConstExpr::Binary {
                        op,
                        lhs,
                        rhs,
                    })));
                }

                if text == "blockaddress" {
                    self.bump();
                    self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                    return Some(Value::BlockAddress(String::new()));
                }

                self.error(
                    format!("expected a value, found `{text}`"),
                    token.span,
                    "not a value",
                );
                None
            }
            _ => {
                self.error(
                    format!("expected a value, found {}", token.kind.describe()),
                    token.span,
                    "not a value",
                );
                None
            }
        }
    }

    fn parse_value_list(&mut self, close: TokenKind) -> Option<Vec<TypedValue>> {
        let mut items = Vec::new();
        while !self.at(close) && !self.at_eof() {
            let item = self.parse_typed_value()?;
            items.push(item);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(close)?;
        Some(items)
    }
}
