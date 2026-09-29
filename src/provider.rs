use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::process::{self, Command, Stdio};
use std::str;
use std::thread;
use std::time::{Duration, Instant};

use crate::ir::*;

const IONQ: &str = "https://api.ionq.co/v0.3";
const POLL: Duration = Duration::from_secs(2);

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

fn gate(gate: &Gate) -> Result<String, String> {
    let name = match gate.kind {
        GateKind::X => "x",
        GateKind::Y => "y",
        GateKind::Z => "z",
        GateKind::H => "h",
        GateKind::S => "s",
        GateKind::SDag => "si",
        GateKind::T => "t",
        GateKind::TDag => "ti",
        GateKind::SX => "v",
        GateKind::SXDag => "vi",
        GateKind::Rx => "rx",
        GateKind::Ry => "ry",
        GateKind::Rz => "rz",
        GateKind::Swap => "swap",
        GateKind::I => return Ok(String::new()),
        other => {
            return Err(format!(
                "IonQ has no `{}` gate, compile with --gates first",
                other.name()
            ));
        }
    };
    let mut out = format!("{{\"gate\": \"{name}\"");
    match (gate.kind, &gate.targets[..]) {
        (GateKind::Swap, [a, b]) => write!(out, ", \"targets\": [{}, {}]", a.0, b.0).unwrap(),
        (_, [t]) => write!(out, ", \"target\": {}", t.0).unwrap(),
        _ => return Err("a gate needs one target, or two for swap".into()),
    }
    if !gate.controls.is_empty() {
        let controls: Vec<String> = gate.controls.iter().map(|c| c.0.to_string()).collect();
        write!(out, ", \"controls\": [{}]", controls.join(", ")).unwrap();
    }
    if gate.kind.param_count() > 0 {
        let angle = gate
            .constant_angle()
            .ok_or("IonQ needs every angle at compile time")?;
        write!(out, ", \"rotation\": {angle}").unwrap();
    }
    out.push('}');
    Ok(out)
}

pub fn ionq(program: &Program, target: &str, shots: u64) -> Result<String, String> {
    let [block] = &program.blocks[..] else {
        return Err("IonQ runs straight line programs, without branches or loops".into());
    };
    let mut measured = false;
    let mut circuit = Vec::new();
    for op in &block.ops {
        match op {
            Op::Gate(g) if !measured => {
                let line = gate(g)?;
                if !line.is_empty() {
                    circuit.push(line);
                }
            }
            Op::Gate(_) | Op::Reset { .. } => {
                return Err("IonQ measures once at the end, so gates and resets cannot follow a measurement".into());
            }
            Op::Measure { .. } => measured = true,
            _ => {}
        }
    }
    Ok(format!(
        "{{\"target\": \"{target}\", \"shots\": {shots}, \"name\": \"qirc\", \"input\": {{\"format\": \"ionq.circuit.v0\", \"gateset\": \"qis\", \"qubits\": {}, \"circuit\": [{}]}}}}",
        program.num_qubits.max(1),
        circuit.join(", ")
    ))
}

fn request(method: &str, url: &str, key: &str, body: Option<&str>) -> Result<Json, String> {
    let mut config = format!(
        "silent\nshow-error\nfail-with-body\nrequest = \"{method}\"\nurl = \"{url}\"\nheader = \"Authorization: apiKey {key}\"\nheader = \"Content-Type: application/json\"\n"
    );
    let payload = match body {
        Some(body) => {
            let path = env::temp_dir().join(format!("qirc-job-{}.json", process::id()));
            fs::write(&path, body).map_err(|e| format!("cannot write the job: {e}"))?;
            writeln!(
                config,
                "data-binary = \"@{}\"",
                path.display().to_string().replace('\\', "/")
            )
            .unwrap();
            Some(path)
        }
        None => None,
    };
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run curl, which qirc uses to reach the provider: {e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| format!("cannot talk to curl: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if let Some(path) = payload {
        fs::remove_file(path).ok();
    }
    let body = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "the provider refused the request: {} {}",
            reason.trim(),
            body.trim()
        ));
    }
    Json::parse(&body).map_err(|e| format!("the provider sent a reply qirc cannot read: {e}"))
}

pub fn submit(
    program: &Program,
    target: &str,
    shots: u64,
    timeout: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<BTreeMap<String, u64>, String> {
    let key = env::var("IONQ_API_KEY")
        .map_err(|_| "set IONQ_API_KEY to your IonQ API key to submit jobs".to_string())?;
    let base = env::var("QIRC_IONQ_URL").unwrap_or_else(|_| IONQ.to_string());
    let job = request(
        "POST",
        &format!("{base}/jobs"),
        &key,
        Some(&ionq(program, target, shots)?),
    )?;
    let id = job
        .get("id")
        .and_then(Json::text)
        .ok_or("the provider did not return a job id")?
        .to_string();
    progress(&format!("submitted job {id} to IonQ target {target}"));
    let started = Instant::now();
    loop {
        let status = request("GET", &format!("{base}/jobs/{id}"), &key, None)?;
        match status.get("status").and_then(Json::text) {
            Some("completed") => break,
            Some(failed @ ("failed" | "canceled")) => {
                return Err(format!("job {id} {failed}"));
            }
            Some(other) => progress(&format!("job {id} is {other}")),
            None => return Err("the provider sent a job without a status".into()),
        }
        if started.elapsed() > timeout {
            return Err(format!(
                "job {id} did not finish within {} seconds",
                timeout.as_secs()
            ));
        }
        thread::sleep(POLL);
    }
    let results = request("GET", &format!("{base}/jobs/{id}/results"), &key, None)?;
    let Json::Object(histogram) = results else {
        return Err("the provider sent results that are not a histogram".into());
    };
    let plan: Vec<(usize, usize)> = program
        .ops()
        .filter_map(|op| match op {
            Op::Measure { qubit, result, .. } => Some((qubit.index(), result.index())),
            _ => None,
        })
        .collect();
    let mut counts = BTreeMap::new();
    let mut total = 0;
    for (state, share) in &histogram {
        let (Ok(index), Json::Number(share)) = (state.parse::<u128>(), share) else {
            continue;
        };
        let mut bits = vec!['0'; program.num_results as usize];
        for &(qubit, result) in &plan {
            if index >> qubit & 1 == 1 {
                bits[result] = '1';
            }
        }
        let count = (share * shots as f64).round() as u64;
        total += count;
        *counts.entry(bits.into_iter().collect()).or_insert(0) += count;
    }
    if total == 0 {
        return Err("the provider returned no results".into());
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;

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
    fn ionq_jobs() {
        let (program, _) = qasm::lower(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[3];\ncreg c[3];\nh q[0];\ncx q[0], q[1];\nrz(0.5) q[2];\nccx q[0], q[1], q[2];\nmeasure q -> c;\n",
        );
        let job = Json::parse(&ionq(&program, "qpu.aria-1", 100).unwrap()).unwrap();
        assert_eq!(job.get("target").and_then(Json::text), Some("qpu.aria-1"));
        let circuit = job.get("input").and_then(|i| i.get("circuit")).unwrap();
        let Json::List(gates) = circuit else {
            unreachable!()
        };
        assert_eq!(gates.len(), 4);
        assert_eq!(gates[2].get("rotation"), Some(&Json::Number(0.5)));
        assert_eq!(
            gates[3].get("controls"),
            Some(&Json::List(vec![Json::Number(0.0), Json::Number(1.0)]))
        );
        let (late, _) = qasm::lower(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[1];\ncreg c[1];\nmeasure q[0] -> c[0];\nx q[0];\n",
        );
        assert!(ionq(&late, "simulator", 10).is_err());
    }
}
