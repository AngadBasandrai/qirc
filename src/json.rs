use std::fmt::Write;
use std::str;

const MAX_NESTING: usize = 128;

#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(text: &str) -> Result<Json, String> {
        let bytes = text.as_bytes();
        let mut at = 0;
        let value = value(bytes, &mut at, 0)?;
        skip(bytes, &mut at);
        if at != bytes.len() {
            return Err(format!("unexpected text after the JSON value at byte {at}"));
        }
        Ok(value)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Json::Text(text) => Some(text),
            _ => None,
        }
    }
}

fn skip(bytes: &[u8], at: &mut usize) {
    while bytes
        .get(*at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        *at += 1;
    }
}

fn expect(bytes: &[u8], at: &mut usize, byte: u8) -> Result<(), String> {
    skip(bytes, at);
    if bytes.get(*at) == Some(&byte) {
        *at += 1;
        Ok(())
    } else {
        Err(format!("expected `{}` at byte {at}", byte as char))
    }
}

fn hex_quad(bytes: &[u8], at: &mut usize) -> Result<u16, String> {
    let start = *at;
    let end = start
        .checked_add(4)
        .ok_or_else(|| format!("unicode escape at byte {start} is too long"))?;
    let hex = bytes
        .get(start..end)
        .ok_or_else(|| format!("short unicode escape at byte {start}"))?;
    let code = u16::from_str_radix(
        str::from_utf8(hex).map_err(|_| format!("bad unicode escape at byte {start}"))?,
        16,
    )
    .map_err(|_| format!("bad unicode escape at byte {start}"))?;
    *at = end;
    Ok(code)
}

fn unicode_escape(bytes: &[u8], at: &mut usize) -> Result<char, String> {
    let start = *at;
    let first = hex_quad(bytes, at)?;
    let code = if (0xd800..=0xdbff).contains(&first) {
        if bytes.get(*at..(*at).saturating_add(2)) != Some(b"\\u") {
            return Err(format!(
                "high surrogate at byte {start} is not followed by a low surrogate"
            ));
        }
        *at += 2;
        let low_at = *at;
        let second = hex_quad(bytes, at)?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return Err(format!("expected a low surrogate at byte {low_at}"));
        }
        0x10000 + (u32::from(first - 0xd800) << 10) + u32::from(second - 0xdc00)
    } else if (0xdc00..=0xdfff).contains(&first) {
        return Err(format!("unexpected low surrogate at byte {start}"));
    } else {
        u32::from(first)
    };
    char::from_u32(code).ok_or_else(|| format!("invalid unicode escape at byte {start}"))
}

fn string(bytes: &[u8], at: &mut usize) -> Result<String, String> {
    expect(bytes, at, b'"')?;
    let mut out = Vec::new();
    loop {
        match bytes.get(*at) {
            None => return Err("unterminated string".into()),
            Some(b'"') => {
                *at += 1;
                return String::from_utf8(out).map_err(|_| "invalid text".into());
            }
            Some(b'\\') => {
                let escaped = bytes.get(*at + 1).copied().ok_or("unterminated escape")?;
                *at += 2;
                match escaped {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let ch = unicode_escape(bytes, at)?;
                        out.extend(ch.to_string().bytes());
                    }
                    other => {
                        return Err(format!(
                            "invalid escape `\\{}` at byte {}",
                            char::from(other),
                            *at - 2
                        ));
                    }
                }
            }
            Some(&byte) if byte < 0x20 => {
                return Err(format!("unescaped control character at byte {}", *at));
            }
            Some(&byte) => {
                out.push(byte);
                *at += 1;
            }
        }
    }
}

fn number(bytes: &[u8], at: &mut usize) -> Result<Json, String> {
    let start = *at;
    if bytes.get(*at) == Some(&b'-') {
        *at += 1;
    }
    match bytes.get(*at) {
        Some(b'0') => {
            *at += 1;
            if bytes.get(*at).is_some_and(u8::is_ascii_digit) {
                return Err(format!("a number has a leading zero at byte {start}"));
            }
        }
        Some(b'1'..=b'9') => {
            *at += 1;
            while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
        }
        _ => return Err(format!("invalid number at byte {start}")),
    }
    if bytes.get(*at) == Some(&b'.') {
        *at += 1;
        let fraction = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        if *at == fraction {
            return Err(format!(
                "a number needs digits after `.` at byte {fraction}"
            ));
        }
    }
    if matches!(bytes.get(*at), Some(b'e' | b'E')) {
        *at += 1;
        if matches!(bytes.get(*at), Some(b'+' | b'-')) {
            *at += 1;
        }
        let exponent = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        if *at == exponent {
            return Err(format!(
                "a number needs digits in its exponent at byte {exponent}"
            ));
        }
    }
    let text = str::from_utf8(&bytes[start..*at]).expect("a JSON number is ASCII");
    let number: f64 = text
        .parse()
        .map_err(|_| format!("invalid number at byte {start}"))?;
    if !number.is_finite() {
        return Err(format!("number at byte {start} is not finite"));
    }
    Ok(Json::Number(number))
}

fn value(bytes: &[u8], at: &mut usize, depth: usize) -> Result<Json, String> {
    skip(bytes, at);
    match bytes.get(*at) {
        Some(b'{') => {
            if depth >= MAX_NESTING {
                return Err(format!("JSON is nested more than {MAX_NESTING} levels"));
            }
            *at += 1;
            let mut fields = Vec::new();
            skip(bytes, at);
            if bytes.get(*at) == Some(&b'}') {
                *at += 1;
                return Ok(Json::Object(fields));
            }
            loop {
                skip(bytes, at);
                let key = string(bytes, at)?;
                expect(bytes, at, b':')?;
                fields.push((key, value(bytes, at, depth + 1)?));
                skip(bytes, at);
                match bytes.get(*at) {
                    Some(b',') => *at += 1,
                    Some(b'}') => {
                        *at += 1;
                        return Ok(Json::Object(fields));
                    }
                    _ => return Err(format!("expected `,` or `}}` at byte {at}")),
                }
            }
        }
        Some(b'[') => {
            if depth >= MAX_NESTING {
                return Err(format!("JSON is nested more than {MAX_NESTING} levels"));
            }
            *at += 1;
            let mut items = Vec::new();
            skip(bytes, at);
            if bytes.get(*at) == Some(&b']') {
                *at += 1;
                return Ok(Json::List(items));
            }
            loop {
                items.push(value(bytes, at, depth + 1)?);
                skip(bytes, at);
                match bytes.get(*at) {
                    Some(b',') => *at += 1,
                    Some(b']') => {
                        *at += 1;
                        return Ok(Json::List(items));
                    }
                    _ => return Err(format!("expected `,` or `]` at byte {at}")),
                }
            }
        }
        Some(b'"') => string(bytes, at).map(Json::Text),
        Some(b't') if bytes[*at..].starts_with(b"true") => {
            *at += 4;
            Ok(Json::Bool(true))
        }
        Some(b'f') if bytes[*at..].starts_with(b"false") => {
            *at += 5;
            Ok(Json::Bool(false))
        }
        Some(b'n') if bytes[*at..].starts_with(b"null") => {
            *at += 4;
            Ok(Json::Null)
        }
        Some(b'-' | b'0'..=b'9') => number(bytes, at),
        Some(_) => Err(format!("unexpected value at byte {}", *at)),
        None => Err("the JSON ended early".into()),
    }
}

pub fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => write!(out, "\\u{:04x}", u32::from(c)).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_json() {
        let parsed =
            Json::parse(r#" {"id": "a\"bA", "list": [1, -2.5e1, true, null], "empty": {}} "#)
                .unwrap();
        assert_eq!(parsed.get("id").and_then(Json::text), Some("a\"bA"));
        assert_eq!(
            parsed.get("list"),
            Some(&Json::List(vec![
                Json::Number(1.0),
                Json::Number(-25.0),
                Json::Bool(true),
                Json::Null
            ]))
        );
        assert_eq!(parsed.get("empty"), Some(&Json::Object(Vec::new())));
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "1 2", "\"open"] {
            assert!(Json::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn enforces_json_numbers_and_whitespace() {
        for good in ["0", "-0", "10", "-12.5", "1e2", "1E-2", "1e+2", "1e308"] {
            assert!(Json::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "+1",
            "01",
            "-01",
            ".1",
            "1.",
            "1e",
            "1e+",
            "--1",
            "NaN",
            "Infinity",
            "1e309",
            "\u{000c}null",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad}");
        }

        assert_eq!(
            Json::parse("{\n  \"a\": 1,\n  \"b\": 2\n}").unwrap(),
            Json::Object(vec![
                ("a".into(), Json::Number(1.0)),
                ("b".into(), Json::Number(2.0))
            ])
        );
    }

    #[test]
    fn validates_escapes_and_surrogates() {
        let parsed = Json::parse(r#""\"\\\/\b\f\n\r\t\u0041\ud83d\ude00""#).unwrap();
        assert_eq!(parsed, Json::Text("\"\\/\u{8}\u{c}\n\r\tA\u{1f600}".into()));
        for bad in [
            r#""\x""#,
            "\"line\nfeed\"",
            r#""\ud800""#,
            r#""\ud800\u0041""#,
            r#""\udc00""#,
            r#""\u12""#,
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn caps_nesting() {
        let deepest = format!("{}0{}", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
        assert!(Json::parse(&deepest).is_ok());
        let too_deep = format!(
            "{}0{}",
            "[".repeat(MAX_NESTING + 1),
            "]".repeat(MAX_NESTING + 1)
        );
        assert!(Json::parse(&too_deep).is_err());
    }

    #[test]
    fn quotes() {
        let text = "a\"b\\c\nd\u{1f600}";
        assert_eq!(quoted(text), "\"a\\\"b\\\\c\\u000ad\u{1f600}\"");
        assert_eq!(Json::parse(&quoted(text)).unwrap(), Json::Text(text.into()));
    }
}
