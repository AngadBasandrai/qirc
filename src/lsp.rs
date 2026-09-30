use std::collections::VecDeque;
use std::io::{self, BufRead, ErrorKind, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;

use crate::diag::{Diagnostic, Severity, SourceFile, Span};
use crate::driver;
use crate::json::{self, Json};

const MAX_TEXT: usize = 1 << 20;

fn receive(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidData, "a message without Content-Length")
    })?;
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

fn send(output: &mut impl Write, body: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}

fn raw(value: &Json) -> String {
    match value {
        Json::Number(number) => number.to_string(),
        Json::Text(text) => json::quoted(text),
        _ => "null".into(),
    }
}

fn position(file: &SourceFile, offset: u32) -> String {
    let offset = offset.min(file.text.len() as u32);
    let line = file.line_index(offset);
    let start = file.line_start(line) as usize;
    let character = file.text[start..offset as usize].encode_utf16().count();
    format!(r#"{{"line":{line},"character":{character}}}"#)
}

fn entry(file: &SourceFile, diagnostic: &Diagnostic) -> String {
    let span = diagnostic.primary_span().unwrap_or(Span::DUMMY);
    let mut message = diagnostic.message.clone();
    for label in diagnostic
        .labels
        .iter()
        .filter(|label| !label.message.is_empty())
    {
        message.push('\n');
        message.push_str(&label.message);
    }
    for note in &diagnostic.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    let severity = match diagnostic.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
    };
    let code = diagnostic
        .code
        .map(|code| format!(r#","code":{}"#, json::quoted(code)))
        .unwrap_or_default();
    format!(
        r#"{{"range":{{"start":{},"end":{}}},"severity":{severity},"source":"qirc"{code},"message":{}}}"#,
        position(file, span.start),
        position(file, span.end),
        json::quoted(&message)
    )
}

pub fn check(uri: &str, text: &str) -> String {
    let file = SourceFile::new(uri, text);
    let entries: Vec<String> = if text.len() > MAX_TEXT {
        vec![entry(
            &file,
            &Diagnostic::warning(format!(
                "this file is {} KB, too large to check while typing, so run qirc on it instead",
                text.len() / 1024
            )),
        )]
    } else {
        match panic::catch_unwind(AssertUnwindSafe(|| driver::compile(text, 0))) {
            Ok(compilation) => compilation
                .diagnostics
                .iter()
                .map(|diagnostic| entry(&file, diagnostic))
                .collect(),
            Err(_) => vec![entry(
                &file,
                &Diagnostic::error("internal error: qirc panicked on this file"),
            )],
        }
    };
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":{},"diagnostics":[{}]}}}}"#,
        json::quoted(uri),
        entries.join(",")
    )
}

fn method(message: &Json) -> Option<&str> {
    message.get("method").and_then(Json::text)
}

fn uri(message: &Json) -> Option<&str> {
    message
        .get("params")?
        .get("textDocument")?
        .get("uri")?
        .text()
}

fn document(message: &Json) -> Option<(&str, &str)> {
    let params = message.get("params")?;
    let text = match params.get("contentChanges") {
        Some(Json::List(changes)) => changes.last()?.get("text")?.text()?,
        _ => params.get("textDocument")?.get("text")?.text()?,
    };
    Some((uri(message)?, text))
}

fn superseded(message: &Json, later: &VecDeque<Json>) -> bool {
    matches!(
        method(message),
        Some("textDocument/didOpen" | "textDocument/didChange")
    ) && later.iter().any(|next| {
        matches!(
            method(next),
            Some("textDocument/didChange" | "textDocument/didClose")
        ) && uri(next) == uri(message)
    })
}

fn handle(message: &Json, output: &mut impl Write) -> io::Result<bool> {
    let id = message.get("id").map(raw);
    let reply = |result: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
            id.as_deref().unwrap_or("null")
        )
    };
    match method(message) {
        Some("initialize") => send(
            output,
            &reply(&format!(
                r#"{{"capabilities":{{"textDocumentSync":1}},"serverInfo":{{"name":"qirc","version":"{}"}}}}"#,
                env!("CARGO_PKG_VERSION")
            )),
        )?,
        Some("shutdown") => send(output, &reply("null"))?,
        Some("exit") => return Ok(false),
        Some("textDocument/didOpen" | "textDocument/didChange") => {
            if let Some((uri, text)) = document(message) {
                send(output, &check(uri, text))?;
            }
        }
        Some("textDocument/didClose") => {
            if let Some(uri) = uri(message) {
                send(output, &check(uri, ""))?;
            }
        }
        Some(method) if id.is_some() => send(
            output,
            &format!(
                r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32601,"message":{}}}}}"#,
                id.as_deref().unwrap_or("null"),
                json::quoted(&format!("qirc does not handle {method}"))
            ),
        )?,
        _ => {}
    }
    Ok(true)
}

fn enqueue(body: io::Result<String>, pending: &mut VecDeque<Json>) -> io::Result<()> {
    pending.extend(Json::parse(&body?).ok());
    Ok(())
}

pub fn serve(input: impl BufRead + Send + 'static, mut output: impl Write) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut input = input;
        loop {
            let message = receive(&mut input).transpose();
            let end = !matches!(message, Some(Ok(_)));
            if let Some(message) = message
                && sender.send(message).is_err()
            {
                return;
            }
            if end {
                return;
            }
        }
    });
    let mut pending = VecDeque::new();
    loop {
        if pending.is_empty() {
            match receiver.recv() {
                Ok(body) => enqueue(body, &mut pending)?,
                Err(_) => return Ok(()),
            }
        }
        for body in receiver.try_iter() {
            enqueue(body, &mut pending)?;
        }
        let Some(message) = pending.pop_front() else {
            continue;
        };
        if !superseded(&message, &pending) && !handle(&message, &mut output)? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    fn replies(output: &[u8]) -> Vec<Json> {
        let mut reader = output;
        let mut out = Vec::new();
        while let Some(body) = receive(&mut reader).unwrap() {
            out.push(Json::parse(&body).unwrap());
        }
        out
    }

    #[test]
    fn serves() {
        let broken = "OPENQASM 3.0;\nqubit[2] q;\nh q[5];\n";
        let session = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_string(),
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string(),
            format!(
                r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///a.qasm","languageId":"openqasm","version":1,"text":{}}}}}}}"#,
                json::quoted(broken)
            ),
            format!(
                r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"file:///a.qasm","version":2}},"contentChanges":[{{"text":{}}}]}}}}"#,
                json::quoted(&broken.replace("q[5]", "q[1]"))
            ),
            r#"{"jsonrpc":"2.0","id":"x","method":"textDocument/hover","params":{}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#.to_string(),
            r#"{"jsonrpc":"2.0","method":"exit"}"#.to_string(),
        ]
        .map(|body| frame(&body))
        .concat();
        let mut output = Vec::new();
        serve(Cursor::new(session.into_bytes()), &mut output).unwrap();
        let replies = replies(&output);
        let sync = replies[0]
            .get("result")
            .and_then(|r| r.get("capabilities"))
            .and_then(|c| c.get("textDocumentSync"));
        assert_eq!(sync, Some(&Json::Number(1.0)));
        let published: Vec<&Json> = replies
            .iter()
            .filter_map(|reply| reply.get("params")?.get("diagnostics"))
            .collect();
        assert_eq!(
            published.last(),
            Some(&&Json::List(Vec::new())),
            "{replies:?}"
        );
        let tail = &replies[replies.len() - 2..];
        assert_eq!(tail[0].get("id"), Some(&Json::Text("x".into())));
        assert!(tail[0].get("error").is_some(), "{:?}", tail[0]);
        assert_eq!(tail[1].get("result"), Some(&Json::Null));

        let checked = Json::parse(&check("file:///a.qasm", broken)).unwrap();
        let Some(Json::List(found)) = checked.get("params").and_then(|p| p.get("diagnostics"))
        else {
            panic!("no diagnostics for {broken}");
        };
        let [problem] = &found[..] else {
            panic!("{found:?}");
        };
        let start = problem.get("range").and_then(|r| r.get("start"));
        assert_eq!(
            start.and_then(|s| s.get("line")),
            Some(&Json::Number(2.0)),
            "{problem:?}"
        );
        assert_eq!(problem.get("severity"), Some(&Json::Number(1.0)));
    }

    #[test]
    fn skips_stale() {
        let message = |method: &str, uri: &str| {
            Json::parse(&format!(
                r#"{{"method":"{method}","params":{{"textDocument":{{"uri":"{uri}"}}}}}}"#
            ))
            .unwrap()
        };
        let open = message("textDocument/didOpen", "file:///a.ll");
        let later = VecDeque::from([message("textDocument/didChange", "file:///b.ll")]);
        assert!(!superseded(&open, &later));
        let later = VecDeque::from([message("textDocument/didChange", "file:///a.ll")]);
        assert!(superseded(&open, &later));
        let close = VecDeque::from([message("textDocument/didClose", "file:///a.ll")]);
        assert!(superseded(
            &message("textDocument/didChange", "file:///a.ll"),
            &close
        ));
        assert!(!superseded(&message("shutdown", "file:///a.ll"), &close));
    }

    #[test]
    fn caps_size() {
        let huge = format!("OPENQASM 3.0;\n{}", "// padding\n".repeat(MAX_TEXT / 8));
        let reply = check("file:///huge.qasm", &huge);
        assert!(
            reply.contains("too large to check while typing"),
            "{}",
            &reply[..200]
        );
    }

    #[test]
    fn counts_utf16() {
        let file = SourceFile::new("a", "; \u{1f600}x\nb");
        assert_eq!(position(&file, 6), r#"{"line":0,"character":4}"#);
        assert_eq!(position(&file, 8), r#"{"line":1,"character":0}"#);
    }
}
