use std::fmt::Write;
use std::str;

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
        let value = value(bytes, &mut at)?;
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
    while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
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
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let hex = bytes.get(*at..*at + 4).ok_or("short unicode escape")?;
                        let code = u32::from_str_radix(str::from_utf8(hex).unwrap_or(""), 16)
                            .map_err(|_| "bad unicode escape")?;
                        *at += 4;
                        let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                        out.extend(ch.to_string().bytes());
                    }
                    other => out.push(other),
                }
            }
            Some(&byte) => {
                out.push(byte);
                *at += 1;
            }
        }
    }
}

fn value(bytes: &[u8], at: &mut usize) -> Result<Json, String> {
    skip(bytes, at);
    match bytes.get(*at) {
        Some(b'{') => {
            *at += 1;
            let mut fields = Vec::new();
            skip(bytes, at);
            if bytes.get(*at) == Some(&b'}') {
                *at += 1;
                return Ok(Json::Object(fields));
            }
            loop {
                let key = string(bytes, at)?;
                expect(bytes, at, b':')?;
                fields.push((key, value(bytes, at)?));
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
            *at += 1;
            let mut items = Vec::new();
            skip(bytes, at);
            if bytes.get(*at) == Some(&b']') {
                *at += 1;
                return Ok(Json::List(items));
            }
            loop {
                items.push(value(bytes, at)?);
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
        Some(_) => {
            let start = *at;
            while bytes
                .get(*at)
                .is_some_and(|b| b.is_ascii_digit() || b"+-.eE".contains(b))
            {
                *at += 1;
            }
            str::from_utf8(&bytes[start..*at])
                .ok()
                .and_then(|text| text.parse().ok())
                .map(Json::Number)
                .ok_or_else(|| format!("unexpected value at byte {start}"))
        }
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
    fn quotes() {
        let text = "a\"b\\c\nd";
        assert_eq!(quoted(text), r#""a\"b\\c\u000ad""#);
        assert_eq!(Json::parse(&quoted(text)).unwrap(), Json::Text(text.into()));
    }
}
