use std::collections::VecDeque;
use std::io::{self, BufRead, ErrorKind, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;

use crate::diag::{Diagnostic, Severity, SourceFile, Span};
use crate::driver;
use crate::json::{self, Json};

const MAX_TEXT: usize = 1 << 20;
const MAX_MESSAGE: usize = 8 << 20;
const MAX_HEADERS: usize = 64 << 10;
const MAX_PENDING: usize = 8;

fn header_line(input: &mut impl BufRead, line: &mut String) -> io::Result<usize> {
    let mut bytes = Vec::new();
    loop {
        let (take, ended) = {
            let available = input.fill_buf()?;
            if available.is_empty() {
                if bytes.is_empty() {
                    return Ok(0);
                }
                break;
            }
            let take = available
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            let next = bytes.len().checked_add(take).ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidData, "LSP headers are too large")
            })?;
            if next > MAX_HEADERS {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("an LSP header line exceeds {MAX_HEADERS} bytes"),
                ));
            }
            bytes.extend_from_slice(&available[..take]);
            (take, available[take - 1] == b'\n')
        };
        input.consume(take);
        if ended {
            break;
        }
    }
    *line = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "an LSP header is not UTF-8"))?;
    Ok(line.len())
}

fn receive(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let read = header_line(input, &mut line)?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "the LSP headers ended before an empty line",
                ))
            };
        }
        header_bytes = header_bytes
            .checked_add(read)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "LSP headers are too large"))?;
        if header_bytes > MAX_HEADERS {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("LSP headers exceed {MAX_HEADERS} bytes"),
            ));
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| io::Error::new(ErrorKind::InvalidData, "an invalid Content-Length"))?;
            if length.replace(parsed).is_some() {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "more than one Content-Length header",
                ));
            }
        }
    }
    let length = length.ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidData, "a message without Content-Length")
    })?;
    if length > MAX_MESSAGE {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("Content-Length {length} exceeds the {MAX_MESSAGE} byte message limit"),
        ));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(length).map_err(|error| {
        io::Error::new(
            ErrorKind::OutOfMemory,
            format!("cannot allocate a {length} byte message: {error}"),
        )
    })?;
    body.resize(length, 0);
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
    if let Ok(message) = Json::parse(&body?) {
        pending.push_back(message);
    }
    Ok(())
}

fn fill_pending(
    receiver: &mpsc::Receiver<io::Result<String>>,
    pending: &mut VecDeque<Json>,
) -> io::Result<()> {
    for _ in 0..MAX_PENDING {
        if pending.len() >= MAX_PENDING {
            break;
        }
        match receiver.try_recv() {
            Ok(body) => enqueue(body, pending)?,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
    Ok(())
}

pub fn serve(input: impl BufRead + Send + 'static, mut output: impl Write) -> io::Result<()> {
    let (sender, receiver) = mpsc::sync_channel(8);
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
        fill_pending(&receiver, &mut pending)?;
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

    #[test]
    fn rejects_invalid_or_oversized_frames() {
        let headers = [
            "Content-Length: no\r\n\r\n".to_string(),
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\n".to_string(),
            format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1),
            format!("X: {}\r\n\r\n", "x".repeat(MAX_HEADERS)),
        ];
        for header in headers {
            let error = receive(&mut Cursor::new(header.as_bytes())).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{header:?}");
        }
        let error = receive(&mut Cursor::new(b"Content-Length: 1"))
            .expect_err("a truncated header block should fail");
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn bounds_pending_messages_during_a_burst() {
        let (sender, receiver) = mpsc::channel();
        for id in 0..(MAX_PENDING * 2) {
            sender.send(Ok(format!(r#"{{"id":{id}}}"#))).unwrap();
        }
        let mut pending = VecDeque::new();
        fill_pending(&receiver, &mut pending).unwrap();
        assert_eq!(pending.len(), MAX_PENDING);
        assert_eq!(receiver.try_iter().count(), MAX_PENDING);

        let (sender, receiver) = mpsc::channel();
        for _ in 0..(MAX_PENDING * 2) {
            sender.send(Ok("not json".into())).unwrap();
        }
        sender.send(Ok(r#"{"id":1}"#.into())).unwrap();
        let mut pending = VecDeque::new();
        fill_pending(&receiver, &mut pending).unwrap();
        assert!(pending.is_empty());
        assert_eq!(receiver.try_iter().count(), MAX_PENDING + 1);
    }
}
