use std::borrow::Cow;

use crate::diag::{Diagnostic, Span};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Less,
    Greater,
    Comma,
    Star,
    Equal,
    Bang,
    Colon,
    Ellipsis,
    Ident,
    LocalIdent,
    GlobalIdent,
    MetadataIdent,
    AttrGroupId,
    IntLit,
    FloatLit,
    StringLit,
    CStringLit,
    Eof,
}

impl TokenKind {
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Less => "`<`",
            TokenKind::Greater => "`>`",
            TokenKind::Comma => "`,`",
            TokenKind::Star => "`*`",
            TokenKind::Equal => "`=`",
            TokenKind::Bang => "`!`",
            TokenKind::Colon => "`:`",
            TokenKind::Ellipsis => "`...`",
            TokenKind::Ident => "an identifier",
            TokenKind::LocalIdent => "a local value name",
            TokenKind::GlobalIdent => "a global name",
            TokenKind::MetadataIdent => "a metadata name",
            TokenKind::AttrGroupId => "an attribute group id",
            TokenKind::IntLit => "an integer literal",
            TokenKind::FloatLit => "a floating point literal",
            TokenKind::StringLit => "a string literal",
            TokenKind::CStringLit => "a byte string literal",
            TokenKind::Eof => "end of file",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.span.range()]
    }
}

pub fn tokenize(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::with_capacity(src.len() / 4 + 1);
    let mut errors = Vec::new();

    loop {
        match lexer.next_token() {
            Ok(token) => {
                let done = token.kind == TokenKind::Eof;
                tokens.push(token);
                if done {
                    break;
                }
            }
            Err(diagnostic) => errors.push(diagnostic),
        }
    }

    (tokens, errors)
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.pos += 1;
                }
                Some(b';') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.pos += 2;
                    while self.pos < self.bytes.len() {
                        if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn token(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    fn next_token(&mut self) -> Result<Token, Diagnostic> {
        self.skip_trivia();

        let start = self.pos;

        let Some(b) = self.peek() else {
            return Ok(self.token(TokenKind::Eof, start));
        };

        match b {
            b'(' => {
                self.pos += 1;
                Ok(self.token(TokenKind::LParen, start))
            }
            b')' => {
                self.pos += 1;
                Ok(self.token(TokenKind::RParen, start))
            }
            b'{' => {
                self.pos += 1;
                Ok(self.token(TokenKind::LBrace, start))
            }
            b'}' => {
                self.pos += 1;
                Ok(self.token(TokenKind::RBrace, start))
            }
            b'[' => {
                self.pos += 1;
                Ok(self.token(TokenKind::LBracket, start))
            }
            b']' => {
                self.pos += 1;
                Ok(self.token(TokenKind::RBracket, start))
            }
            b'<' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Less, start))
            }
            b'>' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Greater, start))
            }
            b',' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Comma, start))
            }
            b'*' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Star, start))
            }
            b'=' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Equal, start))
            }
            b':' => {
                self.pos += 1;
                Ok(self.token(TokenKind::Colon, start))
            }
            b'.' => {
                if self.peek_at(1) == Some(b'.') && self.peek_at(2) == Some(b'.') {
                    self.pos += 3;
                    return Ok(self.token(TokenKind::Ellipsis, start));
                }
                self.lex_bare_ident(start)
            }
            b'%' | b'@' => {
                self.pos += 1;
                let kind = if b == b'%' {
                    TokenKind::LocalIdent
                } else {
                    TokenKind::GlobalIdent
                };
                self.lex_sigil_name(start, kind)
            }
            b'!' => {
                self.pos += 1;
                match self.peek() {
                    Some(c) if is_name_byte(c) => {
                        while self.peek().is_some_and(is_name_byte) {
                            self.pos += 1;
                        }
                        Ok(self.token(TokenKind::MetadataIdent, start))
                    }
                    Some(b'"') => {
                        self.lex_quoted(start)?;
                        Ok(self.token(TokenKind::MetadataIdent, start))
                    }
                    _ => Ok(self.token(TokenKind::Bang, start)),
                }
            }
            b'#' => {
                self.pos += 1;
                while self.peek().is_some_and(is_name_byte) {
                    self.pos += 1;
                }
                Ok(self.token(TokenKind::AttrGroupId, start))
            }
            b'"' => {
                self.lex_quoted(start)?;
                Ok(self.token(TokenKind::StringLit, start))
            }
            b'c' if self.c_string_follows() => {
                self.pos += 1;
                while matches!(self.peek(), Some(b' ' | b'\t')) {
                    self.pos += 1;
                }
                self.lex_quoted(start)?;
                Ok(self.token(TokenKind::CStringLit, start))
            }
            b'-' | b'+' => {
                if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
                    self.lex_number(start)
                } else {
                    self.lex_bare_ident(start)
                }
            }
            b'0'..=b'9' => self.lex_number(start),
            _ if is_ident_start(b) => self.lex_bare_ident(start),
            _ => {
                self.pos += 1;
                Err(Diagnostic::error(format!(
                    "unexpected character `{}` in LLVM IR",
                    self.src[start..self.pos].escape_debug()
                ))
                .with_code("QIR0001")
                .primary(Span::new(start, self.pos), "not valid here"))
            }
        }
    }

    fn c_string_follows(&self) -> bool {
        let mut i = 1;
        while matches!(self.peek_at(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        self.peek_at(i) == Some(b'"')
    }

    fn lex_bare_ident(&mut self, start: usize) -> Result<Token, Diagnostic> {
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }

        while self.peek().is_some_and(is_ident_continue) {
            self.pos += 1;
        }

        if self.pos == start {
            self.pos += 1;
        }

        Ok(self.token(TokenKind::Ident, start))
    }

    fn lex_sigil_name(&mut self, start: usize, kind: TokenKind) -> Result<Token, Diagnostic> {
        if self.peek() == Some(b'"') {
            self.lex_quoted(start)?;
            return Ok(self.token(kind, start));
        }

        let name_start = self.pos;
        while self.peek().is_some_and(is_name_byte) {
            self.pos += 1;
        }

        if self.pos == name_start {
            return Err(Diagnostic::error("expected a name after the sigil")
                .with_code("QIR0002")
                .primary(
                    Span::new(start, self.pos.max(start + 1)),
                    "a `%` or `@` must be followed by a name",
                ));
        }

        Ok(self.token(kind, start))
    }

    fn lex_quoted(&mut self, start: usize) -> Result<(), Diagnostic> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.pos += 1;

        loop {
            match self.bump() {
                Some(b'"') => return Ok(()),
                Some(b'\\') => {
                    self.bump();
                }
                Some(_) => {}
                None => {
                    return Err(Diagnostic::error("unterminated string literal")
                        .with_code("QIR0003")
                        .primary(Span::new(start, self.pos), "this string is never closed"));
                }
            }
        }
    }

    fn lex_number(&mut self, start: usize) -> Result<Token, Diagnostic> {
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }

        if self.peek() == Some(b'0') && matches!(self.peek_at(1), Some(b'x' | b'X')) {
            self.pos += 2;
            if matches!(self.peek(), Some(b'K' | b'L' | b'M' | b'H' | b'R')) {
                self.pos += 1;
            }
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            return Ok(self.token(TokenKind::FloatLit, start));
        }

        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }

        let mut is_float = false;

        if self.peek() == Some(b'.') && !self.peek_at(1).is_some_and(is_ident_start) {
            is_float = true;
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }

        if is_float && matches!(self.peek(), Some(b'e' | b'E')) {
            let mut probe = self.pos + 1;
            if matches!(self.bytes.get(probe), Some(b'-' | b'+')) {
                probe += 1;
            }
            if self.bytes.get(probe).is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                self.pos = probe;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }

        Ok(self.token(
            if is_float {
                TokenKind::FloatLit
            } else {
                TokenKind::IntLit
            },
            start,
        ))
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || matches!(b, b'$' | b'.' | b'_' | b'-')
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'$' | b'.' | b'_' | b'-')
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'$' | b'.' | b'_' | b'-')
}

pub fn decode_name(raw: &str) -> Cow<'_, str> {
    let body = raw
        .strip_prefix('%')
        .or_else(|| raw.strip_prefix('@'))
        .or_else(|| raw.strip_prefix('!'))
        .or_else(|| raw.strip_prefix('#'))
        .unwrap_or(raw);

    if !body.starts_with('"') {
        return Cow::Borrowed(body);
    }

    let inner = body
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(body);

    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }

    Cow::Owned(String::from_utf8_lossy(&decode_escapes(inner)).into_owned())
}

pub fn decode_cstring(raw: &str) -> Vec<u8> {
    let inner = raw
        .strip_prefix('c')
        .unwrap_or(raw)
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or("");

    decode_escapes(inner).into_owned()
}

fn decode_escapes(s: &str) -> Cow<'_, [u8]> {
    if !s.contains('\\') {
        return Cow::Borrowed(s.as_bytes());
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }

        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            out.push(b'\\');
            i += 2;
            continue;
        }

        out.push(bytes[i]);
        i += 1;
    }

    Cow::Owned(out)
}

pub fn parse_int(raw: &str) -> Option<i128> {
    raw.parse::<i128>().ok()
}

pub fn parse_float(raw: &str) -> Option<f64> {
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        if let Some(rest) = hex.strip_prefix('H') {
            let bits = u16::from_str_radix(rest, 16).ok()?;
            return Some(half_bits_to_f64(bits));
        }

        for prefix in ['K', 'L', 'M', 'R'] {
            if let Some(rest) = hex.strip_prefix(prefix) {
                let truncated = &rest[..rest.len().min(16)];
                let bits = u64::from_str_radix(truncated, 16).ok()?;
                return Some(f64::from_bits(bits));
            }
        }

        let bits = u64::from_str_radix(hex, 16).ok()?;
        return Some(f64::from_bits(bits));
    }

    raw.parse::<f64>().ok()
}

fn half_bits_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let mantissa = (bits & 0x3ff) as f64;

    match exponent {
        0 => sign * mantissa * 2f64.powi(-24),
        0x1f if mantissa == 0.0 => sign * f64::INFINITY,
        0x1f => f64::NAN,
        _ => sign * (1.0 + mantissa / 1024.0) * 2f64.powi(exponent - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, errors) = tokenize(src);
        assert!(errors.is_empty(), "unexpected lex errors: {errors:?}");
        tokens
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != TokenKind::Eof)
            .collect()
    }

    fn texts(src: &str) -> Vec<String> {
        let (tokens, errors) = tokenize(src);
        assert!(errors.is_empty(), "unexpected lex errors: {errors:?}");
        tokens
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| t.text(src).to_string())
            .collect()
    }

    #[test]
    fn comments() {
        assert_eq!(kinds("; a comment\n  ; another\n"), Vec::<TokenKind>::new());
        assert_eq!(
            kinds("ret ; trailing\nvoid"),
            vec![TokenKind::Ident, TokenKind::Ident]
        );
    }

    #[test]
    fn sigils() {
        assert_eq!(
            kinds("%q0 @main !0 #1"),
            vec![
                TokenKind::LocalIdent,
                TokenKind::GlobalIdent,
                TokenKind::MetadataIdent,
                TokenKind::AttrGroupId
            ]
        );
        assert_eq!(texts("%0 %.str @g.str"), vec!["%0", "%.str", "@g.str"]);
    }

    #[test]
    fn quoted_names() {
        let src = r#"%"quoted type" @"quoted fn name""#;
        assert_eq!(
            kinds(src),
            vec![TokenKind::LocalIdent, TokenKind::GlobalIdent]
        );
        assert_eq!(decode_name(r#"%"quoted type""#), "quoted type");
        assert_eq!(decode_name("%q0"), "q0");
    }

    #[test]
    fn bang_vs_metadata() {
        assert_eq!(
            kinds("!llvm.module.flags = !{!0}"),
            vec![
                TokenKind::MetadataIdent,
                TokenKind::Equal,
                TokenKind::Bang,
                TokenKind::LBrace,
                TokenKind::MetadataIdent,
                TokenKind::RBrace
            ]
        );
        assert_eq!(
            kinds(r#"!0 = !{i32 1, !"qir_major_version", i32 1}"#),
            vec![
                TokenKind::MetadataIdent,
                TokenKind::Equal,
                TokenKind::Bang,
                TokenKind::LBrace,
                TokenKind::Ident,
                TokenKind::IntLit,
                TokenKind::Comma,
                TokenKind::MetadataIdent,
                TokenKind::Comma,
                TokenKind::Ident,
                TokenKind::IntLit,
                TokenKind::RBrace
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("0 42 -1 1.5 1.000000e+00 -5.000000e-01 0x400921FB54442D18"),
            vec![
                TokenKind::IntLit,
                TokenKind::IntLit,
                TokenKind::IntLit,
                TokenKind::FloatLit,
                TokenKind::FloatLit,
                TokenKind::FloatLit,
                TokenKind::FloatLit
            ]
        );
    }

    #[test]
    fn hex_float() {
        let pi = parse_float("0x400921FB54442D18").unwrap();
        assert!((pi - std::f64::consts::PI).abs() < 1e-15);
        assert_eq!(parse_float("1.000000e+00"), Some(1.0));
        assert_eq!(parse_float("-5.000000e-01"), Some(-0.5));
        assert_eq!(parse_int("-1"), Some(-1));
    }

    #[test]
    fn array_type_x() {
        assert_eq!(texts("[4 x i8]"), vec!["[", "4", "x", "i8", "]"]);
    }

    #[test]
    fn c_string_escapes() {
        let src = r#"c"ab\0A\00""#;
        assert_eq!(kinds(src), vec![TokenKind::CStringLit]);
        assert_eq!(decode_cstring(src), vec![b'a', b'b', 0x0A, 0x00]);
    }

    #[test]
    fn ellipsis() {
        assert_eq!(
            kinds("(i64, ...)"),
            vec![
                TokenKind::LParen,
                TokenKind::Ident,
                TokenKind::Comma,
                TokenKind::Ellipsis,
                TokenKind::RParen
            ]
        );
    }

    #[test]
    fn unterminated_string() {
        let (_, errors) = tokenize("@g = constant [2 x i8] c\"ab");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Some("QIR0003"));
    }

    #[test]
    fn spans() {
        let src = "call void @__quantum__qis__h__body(%Qubit* null)";
        let (tokens, _) = tokenize(src);
        let global = tokens
            .iter()
            .find(|t| t.kind == TokenKind::GlobalIdent)
            .unwrap();
        assert_eq!(global.text(src), "@__quantum__qis__h__body");
        assert_eq!(global.span.start as usize, src.find('@').unwrap());
    }

    #[test]
    fn block_comments() {
        assert_eq!(
            kinds("ret /* skipped */ void"),
            vec![TokenKind::Ident, TokenKind::Ident]
        );
        assert_eq!(
            kinds(
                "/* multi
line */ ret"
            ),
            vec![TokenKind::Ident]
        );
    }

    #[test]
    fn float_needs_point() {
        assert_eq!(kinds("1e10"), vec![TokenKind::IntLit, TokenKind::Ident]);
        assert_eq!(kinds("1.0e10"), vec![TokenKind::FloatLit]);
        assert_eq!(texts("1e10"), vec!["1", "e10"]);
    }

    #[test]
    fn dashed_idents() {
        assert_eq!(kinds("-foo :"), vec![TokenKind::Ident, TokenKind::Colon]);
        assert_eq!(texts("frame-pointer"), vec!["frame-pointer"]);
        assert_eq!(kinds("i32 -1"), vec![TokenKind::Ident, TokenKind::IntLit]);
        assert_eq!(texts(".LBB0_1 :"), vec![".LBB0_1", ":"]);
    }

    #[test]
    fn c_string_space() {
        assert_eq!(kinds("c \"ab\""), vec![TokenKind::CStringLit]);
    }

    #[test]
    fn semicolon_in_string() {
        let src = "@s = constant [4 x i8] c\";x\\00\"";
        let toks = texts(src);
        assert!(toks.iter().any(|t| t.contains(';')), "got {toks:?}");
    }

    const CORPUS: &[(&str, &str)] = &[
        (
            "base_profile_bell",
            include_str!("../tests/corpus/base_profile_bell.ll"),
        ),
        (
            "adaptive_teleport",
            include_str!("../tests/corpus/adaptive_teleport.ll"),
        ),
        (
            "pyqir_simple",
            include_str!("../tests/corpus/pyqir_simple.ll"),
        ),
        (
            "unrestricted_dynamic",
            include_str!("../tests/corpus/unrestricted_dynamic.ll"),
        ),
        (
            "syntax_stress",
            include_str!("../tests/corpus/syntax_stress.ll"),
        ),
    ];

    #[test]
    fn corpus() {
        for (name, src) in CORPUS {
            let (tokens, errors) = tokenize(src);
            assert!(errors.is_empty(), "{name} produced lex errors: {errors:#?}");
            assert!(tokens.len() > 50, "{name} produced too few tokens");
            assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);

            let covered: usize = tokens
                .iter()
                .filter(|t| t.kind != TokenKind::Eof)
                .map(|t| t.span.len())
                .sum();
            assert!(covered > 0, "{name} covered nothing");
        }
    }

    #[test]
    fn corpus_spans() {
        for (name, src) in CORPUS {
            let (tokens, _) = tokenize(src);
            for token in tokens.iter().filter(|t| t.kind != TokenKind::Eof) {
                let text = token.text(src);
                assert!(!text.is_empty(), "{name} produced an empty token");
                assert!(
                    !text.starts_with(';'),
                    "{name} leaked a comment into a token: {text}"
                );
            }
        }
    }
}
