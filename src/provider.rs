use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::ir::*;
use crate::json::{self, Json};

const IONQ: &str = "https://api.ionq.co/v0.3";
const POLL: Duration = Duration::from_secs(2);
const MAX_IONQ_CONTROLS: usize = 7;
const MAX_PROVIDER_REPLY: usize = 8 << 20;
const PROBABILITY_TOLERANCE: f64 = 1e-6;
const MAX_PRECISE_SHOTS: u64 = 1 << 53;
static NEXT_PAYLOAD: AtomicU64 = AtomicU64::new(0);

struct TempPayload {
    path: PathBuf,
}

impl TempPayload {
    fn create(body: &str) -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..32 {
            let sequence = NEXT_PAYLOAD.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "qirc-job-{}-{stamp}-{sequence}.json",
                process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    let payload = Self { path };
                    file.write_all(body.as_bytes())
                        .map_err(|error| format!("cannot write the job: {error}"))?;
                    return Ok(payload);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("cannot create the job payload: {error}")),
            }
        }
        Err("cannot create a unique job payload".into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPayload {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

fn curl_value(value: &str, name: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err(format!("the {name} contains a control character"));
    }
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn curl_config(
    method: &str,
    url: &str,
    key: &str,
    payload: Option<&Path>,
) -> Result<String, String> {
    let mut config = format!(
        "silent\nshow-error\nfail-with-body\ngloboff\nproto = \"=http,https\"\nmax-filesize = {MAX_PROVIDER_REPLY}\nconnect-timeout = 10\nmax-time = 60\n"
    );
    writeln!(config, "request = {}", curl_value(method, "HTTP method")?).unwrap();
    writeln!(config, "url = {}", curl_value(url, "provider URL")?).unwrap();
    writeln!(
        config,
        "header = {}",
        curl_value(&format!("Authorization: apiKey {key}"), "API key")?
    )
    .unwrap();
    writeln!(
        config,
        "header = {}",
        curl_value("Content-Type: application/json", "HTTP header")?
    )
    .unwrap();
    if let Some(path) = payload {
        let path = path
            .to_str()
            .ok_or("the temporary payload path is not valid Unicode")?
            .replace('\\', "/");
        writeln!(
            config,
            "data-binary = {}",
            curl_value(&format!("@{path}"), "temporary payload path")?
        )
        .unwrap();
    }
    Ok(config)
}

fn gate(gate: &Gate) -> Result<String, String> {
    if !gate.has_valid_shape() {
        return Err(format!(
            "IonQ gate `{}` has an invalid target, parameter, or wire shape",
            gate.kind.name()
        ));
    }
    if gate.controls.len() > MAX_IONQ_CONTROLS {
        return Err(format!(
            "IonQ gates support at most {MAX_IONQ_CONTROLS} controls"
        ));
    }
    if gate.kind == GateKind::Swap && !gate.controls.is_empty() {
        return Err("IonQ does not support a controlled swap".into());
    }
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
        if !angle.is_finite() {
            return Err("IonQ needs every angle to be finite".into());
        }
        write!(out, ", \"rotation\": {angle}").unwrap();
    }
    out.push('}');
    Ok(out)
}

pub fn ionq(program: &Program, target: &str, shots: u64) -> Result<String, String> {
    if shots == 0 {
        return Err("IonQ needs at least one shot".into());
    }
    if shots > MAX_PRECISE_SHOTS {
        return Err(format!(
            "IonQ shot counts cannot exceed {MAX_PRECISE_SHOTS}"
        ));
    }
    let [block] = &program.blocks[..] else {
        return Err("IonQ runs straight line programs, without branches or loops".into());
    };
    if !matches!(block.term, Term::Ret(_)) {
        return Err("IonQ runs straight line programs, without branches or loops".into());
    }
    let mut measured = false;
    let mut circuit = Vec::new();
    for op in &block.ops {
        match op {
            Op::Gate(g) if !measured => {
                if g.wires().any(|wire| wire.0 >= program.num_qubits) {
                    return Err("an IonQ gate uses an out-of-range qubit".into());
                }
                let line = gate(g)?;
                if !line.is_empty() {
                    circuit.push(line);
                }
            }
            Op::Gate(_) | Op::Reset { .. } => {
                return Err("IonQ measures once at the end, so gates and resets cannot follow a measurement".into());
            }
            Op::Measure { qubit, result, .. } => {
                if qubit.0 >= program.num_qubits || result.0 >= program.num_results {
                    return Err("an IonQ measurement uses an out-of-range wire".into());
                }
                measured = true;
            }
            _ => {}
        }
    }
    Ok(format!(
        "{{\"target\": {}, \"shots\": {shots}, \"name\": \"qirc\", \"input\": {{\"format\": \"ionq.circuit.v0\", \"gateset\": \"qis\", \"qubits\": {}, \"circuit\": [{}]}}}}",
        json::quoted(target),
        program.num_qubits.max(1),
        circuit.join(", ")
    ))
}

fn request(method: &str, url: &str, key: &str, body: Option<&str>) -> Result<Json, String> {
    let payload = body.map(TempPayload::create).transpose()?;
    let config = curl_config(method, url, key, payload.as_ref().map(TempPayload::path))?;
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run curl, which qirc uses to reach the provider: {e}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "curl did not open its standard input".to_string())?
        .write_all(config.as_bytes());
    if let Err(error) = write_result {
        child.kill().ok();
        child.wait().ok();
        return Err(format!("cannot talk to curl: {error}"));
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let body = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "the provider refused the request: {} {}",
            reason.trim(),
            body.trim()
        ));
    }
    if output.stdout.len() > MAX_PROVIDER_REPLY {
        return Err(format!(
            "the provider reply exceeds the {MAX_PROVIDER_REPLY} byte limit"
        ));
    }
    let body = String::from_utf8(output.stdout)
        .map_err(|_| "the provider sent a reply that is not UTF-8".to_string())?;
    Json::parse(&body).map_err(|e| format!("the provider sent a reply qirc cannot read: {e}"))
}

fn url_segment(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}

fn decimal_state(state: &str, width: usize) -> Result<Vec<u64>, String> {
    if state.is_empty() || !state.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid state `{state}`"));
    }
    let significant = state.trim_start_matches('0');
    if significant.is_empty() {
        return Ok(vec![0; width.div_ceil(64)]);
    }
    if significant.len() > width / 3 + 2 {
        return Err(format!("state `{state}` does not fit in {width} qubits"));
    }
    let mut words = vec![0u64; width.div_ceil(64)];
    for digit in significant.bytes().map(|byte| u64::from(byte - b'0')) {
        let mut carry = u128::from(digit);
        for word in &mut words {
            let value = u128::from(*word) * 10 + carry;
            *word = value as u64;
            carry = value >> 64;
        }
        if carry != 0 {
            return Err(format!("state `{state}` does not fit in {width} qubits"));
        }
    }
    let remainder = width % 64;
    if remainder != 0 && words.last().is_some_and(|word| word >> remainder != 0) {
        return Err(format!("state `{state}` does not fit in {width} qubits"));
    }
    Ok(words)
}

fn histogram_counts(
    program: &Program,
    histogram: &[(String, Json)],
    shots: u64,
) -> Result<BTreeMap<String, u64>, String> {
    if shots == 0 {
        return Err("the shot count must be positive".into());
    }
    if shots > MAX_PRECISE_SHOTS {
        return Err(format!("the shot count cannot exceed {MAX_PRECISE_SHOTS}"));
    }
    let plan: Vec<(usize, usize)> = program
        .ops()
        .filter_map(|op| match op {
            Op::Measure { qubit, result, .. } => Some((qubit.index(), result.index())),
            _ => None,
        })
        .collect();
    if plan
        .iter()
        .any(|&(qubit, _)| qubit >= program.num_qubits as usize)
    {
        return Err("the measurement plan contains an out-of-range qubit".into());
    }
    if plan
        .iter()
        .any(|&(_, result)| result >= program.num_results as usize)
    {
        return Err("the measurement plan contains an out-of-range result".into());
    }

    let width = program.num_qubits.max(1) as usize;
    let mut probabilities = BTreeMap::<String, f64>::new();
    let mut total = 0.0;
    for (state, share) in histogram {
        let Json::Number(share) = share else {
            return Err(format!(
                "the provider returned a nonnumeric probability for state {state}"
            ));
        };
        if !share.is_finite() || !(0.0..=1.0).contains(share) {
            return Err(format!(
                "the provider returned invalid probability {share} for state {state}"
            ));
        }
        let state_words = decimal_state(state, width)
            .map_err(|reason| format!("the provider returned {reason}"))?;
        let mut bits = vec!['0'; program.num_results as usize];
        for &(qubit, result) in &plan {
            bits[result] = if state_words[qubit / 64] >> (qubit % 64) & 1 == 1 {
                '1'
            } else {
                '0'
            };
        }
        total += share;
        *probabilities
            .entry(bits.into_iter().collect())
            .or_insert(0.0) += share;
    }
    if !total.is_finite() || total == 0.0 {
        return Err("the provider returned no results".into());
    }
    if (total - 1.0).abs() > PROBABILITY_TOLERANCE {
        return Err(format!(
            "the provider probabilities sum to {total}, instead of 1"
        ));
    }

    let mut apportioned = Vec::new();
    let mut assigned = 0u64;
    for (bits, share) in probabilities {
        if share == 0.0 {
            continue;
        }
        let exact = share / total * shots as f64;
        let count = exact.floor() as u64;
        assigned = assigned
            .checked_add(count)
            .ok_or("the provider result counts overflowed")?;
        apportioned.push((bits, count, exact - count as f64));
    }
    let remaining = shots
        .checked_sub(assigned)
        .ok_or("the provider result counts exceed the shot count")?;
    let remaining = usize::try_from(remaining)
        .map_err(|_| "the provider result counts cannot be rounded safely")?;
    if remaining > apportioned.len() {
        return Err("the provider result counts cannot be rounded safely".into());
    }
    apportioned.sort_by(|left, right| {
        right
            .2
            .total_cmp(&left.2)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (_, count, _) in apportioned.iter_mut().take(remaining) {
        *count += 1;
    }
    let mut counts = BTreeMap::new();
    for (bits, count, _) in apportioned {
        if count != 0 {
            counts.insert(bits, count);
        }
    }
    Ok(counts)
}

pub fn submit(
    program: &Program,
    target: &str,
    shots: u64,
    timeout: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<BTreeMap<String, u64>, String> {
    if timeout.is_zero() {
        return Err("the provider timeout must be above zero".into());
    }
    if target.chars().any(char::is_control) {
        return Err("the provider target contains a control character".into());
    }
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
    if id.chars().any(char::is_control) {
        return Err("the provider returned a job id containing a control character".into());
    }
    let encoded_id = url_segment(&id);
    progress(&format!("submitted job {id} to IonQ target {target}"));
    let started = Instant::now();
    loop {
        if started.elapsed() >= timeout {
            return Err(format!(
                "job {id} did not finish within {} seconds",
                timeout.as_secs()
            ));
        }
        let status = request("GET", &format!("{base}/jobs/{encoded_id}"), &key, None)?;
        match status.get("status").and_then(Json::text) {
            Some("completed") => break,
            Some(failed @ ("failed" | "canceled")) => {
                return Err(format!("job {id} {failed}"));
            }
            Some(other) if other.chars().any(char::is_control) => {
                return Err("the provider returned an invalid job status".into());
            }
            Some(other) => progress(&format!("job {id} is {other}")),
            None => return Err("the provider sent a job without a status".into()),
        }
        thread::sleep(POLL.min(timeout.saturating_sub(started.elapsed())));
    }
    let results = request(
        "GET",
        &format!("{base}/jobs/{encoded_id}/results"),
        &key,
        None,
    )?;
    let Json::Object(histogram) = results else {
        return Err("the provider sent results that are not a histogram".into());
    };
    histogram_counts(program, &histogram, shots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::qasm;

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
        assert!(ionq(&program, "simulator", 0).is_err());
        assert!(ionq(&program, "simulator", MAX_PRECISE_SHOTS + 1).is_err());

        let (mut looped, diagnostics) = qasm::lower("OPENQASM 3.0; qubit q;");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        looped.blocks[0].term = Term::Br(BlockId(0));
        assert!(ionq(&looped, "simulator", 10).is_err());

        let mut progress = |_: &str| {};
        assert!(
            submit(&program, "simulator", 10, Duration::ZERO, &mut progress)
                .unwrap_err()
                .contains("timeout")
        );

        let unusual = "simulator\"\\\nnext";
        let job = Json::parse(&ionq(&program, unusual, 10).unwrap()).unwrap();
        assert_eq!(job.get("target").and_then(Json::text), Some(unusual));
    }

    #[test]
    fn ionq_rejects_malformed_gates() {
        let run = |gate: Gate| {
            let mut program = Program::new("malformed", Profile::Base);
            program.num_qubits = 9;
            program.blocks.push(Block {
                id: BlockId(0),
                label: "entry".into(),
                ops: vec![Op::Gate(gate)],
                term: Term::Ret(None),
                span: Span::DUMMY,
            });
            ionq(&program, "simulator", 10)
        };
        let gate = |kind, controls, targets, params| Gate {
            kind,
            controls,
            targets,
            params,
            span: Span::DUMMY,
        };
        let malformed = [
            gate(GateKind::I, vec![], vec![], vec![]),
            gate(
                GateKind::I,
                vec![],
                vec![QubitId(0)],
                vec![Operand::Const(Const::Float(0.5))],
            ),
            gate(GateKind::X, vec![], vec![QubitId(0), QubitId(1)], vec![]),
            gate(GateKind::Rx, vec![], vec![QubitId(0)], vec![]),
            gate(
                GateKind::Rx,
                vec![],
                vec![QubitId(0)],
                vec![
                    Operand::Const(Const::Float(0.5)),
                    Operand::Const(Const::Float(0.25)),
                ],
            ),
            gate(
                GateKind::X,
                vec![QubitId(0), QubitId(0)],
                vec![QubitId(1)],
                vec![],
            ),
            gate(GateKind::X, vec![QubitId(0)], vec![QubitId(0)], vec![]),
            gate(
                GateKind::Swap,
                vec![QubitId(0)],
                vec![QubitId(1), QubitId(2)],
                vec![],
            ),
            gate(
                GateKind::X,
                (0..=MAX_IONQ_CONTROLS as u32).map(QubitId).collect(),
                vec![QubitId(8)],
                vec![],
            ),
        ];
        for gate in malformed {
            assert!(run(gate).is_err());
        }
    }

    #[test]
    fn protects_curl_configuration_and_payloads() {
        assert_eq!(curl_value("a\"b\\c", "value").unwrap(), r#""a\"b\\c""#);
        assert!(curl_value("safe\noutput = bad", "value").is_err());
        let config = curl_config(
            "GET",
            "http://localhost/a\"b",
            "key\\part",
            Some(Path::new("a b/job.json")),
        )
        .unwrap();
        assert!(config.contains(r#"url = "http://localhost/a\"b""#));
        assert!(config.contains(r#"header = "Authorization: apiKey key\\part""#));
        assert!(config.contains(r#"data-binary = "@a b/job.json""#));
        assert!(curl_config("GET", "http://localhost\noutput=x", "key", None).is_err());

        let first = TempPayload::create("first").unwrap();
        let second = TempPayload::create("second").unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(fs::read_to_string(first.path()).unwrap(), "first");
        let path = first.path().to_path_buf();
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn reads_wide_decimal_states() {
        let state = decimal_state("340282366920938463463374607431768211456", 129).unwrap();
        assert_eq!(state.len(), 3);
        assert_eq!(state[2], 1);
        assert!(decimal_state("340282366920938463463374607431768211456", 128).is_err());
        assert!(decimal_state("12x", 8).is_err());
    }

    #[test]
    fn validates_and_apportions_provider_results() {
        let (program, diagnostics) = qasm::lower(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[3];\ncreg c[3];\nmeasure q -> c;\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let histogram = vec![
            ("0".into(), Json::Number(0.25)),
            ("1".into(), Json::Number(0.25)),
            ("4".into(), Json::Number(0.5)),
        ];
        let counts = histogram_counts(&program, &histogram, 7).unwrap();
        assert_eq!(counts.values().sum::<u64>(), 7);
        assert_eq!(counts.get("000"), Some(&2));
        assert_eq!(counts.get("100"), Some(&2));
        assert_eq!(counts.get("001"), Some(&3));

        for bad in [
            vec![("x".into(), Json::Number(1.0))],
            vec![("8".into(), Json::Number(1.0))],
            vec![("0".into(), Json::Number(-0.1))],
            vec![("0".into(), Json::Number(0.5))],
            vec![("0".into(), Json::Text("one".into()))],
        ] {
            assert!(histogram_counts(&program, &bad, 10).is_err(), "{bad:?}");
        }
        assert!(histogram_counts(&program, &histogram, 0).is_err());
        assert!(histogram_counts(&program, &histogram, MAX_PRECISE_SHOTS + 1).is_err());

        let (overwritten, diagnostics) = qasm::lower(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[2];\ncreg c[1];\nmeasure q[0] -> c[0];\nmeasure q[1] -> c[0];\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let counts =
            histogram_counts(&overwritten, &[("1".into(), Json::Number(1.0))], 10).unwrap();
        assert_eq!(counts, BTreeMap::from([("0".into(), 10)]));
    }

    #[test]
    fn escapes_job_ids_as_url_segments() {
        assert_eq!(url_segment("job/a?b=1"), "job%2Fa%3Fb%3D1");
        assert_eq!(url_segment("safe-._~AZ09"), "safe-._~AZ09");
    }
}
