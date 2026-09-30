use std::fs;
use std::panic::{self, AssertUnwindSafe};

use qirc::diag::SourceFile;
use qirc::driver::{self, Output};
use qirc::simulator::state::Rng;

const TOKENS: [&str; 16] = [
    "phi",
    "br",
    "%x",
    "(",
    "]",
    "}",
    "i64 99999999999",
    "i64 -7",
    "@__quantum__qis__h__body",
    "inttoptr",
    "label %entry",
    "double 1e308",
    "qubit[40] r;",
    "for int i in [0:1000000] { h q[0]; }",
    "measure",
    "\"",
];

const QASM: &str = "OPENQASM 3.0;
include \"stdgates.inc\";
qubit[3] q;
bit[3] c;
const int n = 3;
for int i in [0:n - 2] {
    cx q[i], q[i + 1];
}
rz(pi / 2 ** 3) q[2];
c[0] = measure q[0];
if (c[0]) { x q[1]; } else { h q[2]; }
while (c[1] == 0) {
    h q[1];
    c[1] = measure q[1];
}
c[2] = measure q[2];
";

fn damage(rng: &mut Rng, text: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    for _ in 0..1 + rng.next_u64() % 3 {
        if lines.is_empty() {
            break;
        }
        let at = rng.next_u64() as usize % lines.len();
        match rng.next_u64() % 6 {
            0 => {
                lines.remove(at);
            }
            1 => {
                let copy = lines[at].clone();
                lines.insert(at, copy);
            }
            2 => {
                let other = rng.next_u64() as usize % lines.len();
                lines.swap(at, other);
            }
            3 => {
                let token = TOKENS[rng.next_u64() as usize % TOKENS.len()];
                let cut = lines[at].len() / 2;
                let cut = (0..=cut)
                    .rev()
                    .find(|&i| lines[at].is_char_boundary(i))
                    .unwrap_or(0);
                lines[at].insert_str(cut, token);
            }
            4 => {
                let line = &mut lines[at];
                let cut = (0..=line.len() / 2)
                    .rev()
                    .find(|&i| line.is_char_boundary(i))
                    .unwrap_or(0);
                line.truncate(cut);
            }
            _ => {
                let digits: String = lines[at]
                    .chars()
                    .map(|c| {
                        if c.is_ascii_digit() {
                            char::from(b'0' + (rng.next_u64() % 10) as u8)
                        } else {
                            c
                        }
                    })
                    .collect();
                lines[at] = digits;
            }
        }
    }
    lines.join("\n")
}

#[test]
fn survives_damage() {
    let mut sources: Vec<(String, String)> = fs::read_dir("tests/corpus")
        .unwrap()
        .chain(fs::read_dir("examples").unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "ll"))
        .map(|path| {
            (
                path.display().to_string(),
                fs::read_to_string(&path).unwrap(),
            )
        })
        .collect();
    sources.push(("input.qasm".into(), QASM.into()));
    sources.sort();

    let mut rng = Rng::new(11);
    let mut crashes = Vec::new();
    for round in 0..600 {
        let (name, text) = &sources[round % sources.len()];
        let damaged = damage(&mut rng, text);
        let emit = ["run", "qasm3", "qir", "cost", "circuit", "stim", "json"][round % 7];
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let compilation = driver::compile(&damaged, 2);
            if compilation.program.num_qubits > 12 {
                return;
            }
            let args: Vec<String> = [
                name.as_str(),
                "--emit",
                emit,
                "-O2",
                "--shots",
                "4",
                "--seed",
                "1",
            ]
            .map(String::from)
            .to_vec();
            if let Ok(options) = driver::parse_args_with(&args, |_| Err("no files".into())) {
                driver::run_source(
                    &options,
                    &SourceFile::new(name.clone(), damaged.clone()),
                    None,
                    &mut Output::default(),
                );
            }
        }));
        if outcome.is_err() {
            crashes.push(format!("{name} --emit {emit}\n{damaged}"));
        }
    }
    assert!(
        crashes.is_empty(),
        "{} crashes, first:\n{}",
        crashes.len(),
        crashes[0]
    );
}
