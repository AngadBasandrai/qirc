mod common;

use common::{compile, errors, run};
use qirc::driver;
use qirc::equiv;
use qirc::ir::Profile;

const BELL: &str = "OPENQASM 3.0;
include \"stdgates.inc\";
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
c = measure q;
";

const TELEPORT_QIR: &str = include_str!("corpus/adaptive_teleport.ll");

fn probabilities(outcomes: &equiv::Outcomes) -> Vec<(String, f64)> {
    let mut totals: Vec<(String, f64)> = Vec::new();
    for (key, branch) in &outcomes.branches {
        let bits = key.split(' ').next().unwrap_or_default().to_string();
        match totals.iter_mut().find(|(k, _)| *k == bits) {
            Some((_, p)) => *p += branch.probability(),
            None => totals.push((bits, branch.probability())),
        }
    }
    totals
}

#[test]
fn bell() {
    let program = compile(BELL, 1);
    assert_eq!(program.num_qubits, 2);
    assert_eq!(program.profile, Profile::Base);
    let counts = run(&program, 400, 3).counts;
    assert_eq!(counts.keys().collect::<Vec<_>>(), ["00", "11"]);
}

#[test]
fn teleport() {
    let source = "OPENQASM 3.0;
qubit[3] q;
bit[3] c;
ry(pi/4) q[0];
h q[1];
cx q[1], q[2];
cx q[0], q[1];
h q[0];
c[0] = measure q[0];
c[1] = measure q[1];
if (c[1]) x q[2];
if (c[0] == 1) { z q[2]; }
c[2] = measure q[2];
";
    let program = compile(source, 1);
    assert_eq!(program.profile, Profile::Adaptive);
    let left = probabilities(&equiv::explore(&program));
    let right = probabilities(&equiv::explore(&compile(TELEPORT_QIR, 1)));
    assert_eq!(left.len(), right.len());
    for ((a, p), (b, q)) in left.iter().zip(&right) {
        assert_eq!(a, b);
        assert!((p - q).abs() < 1e-9, "{a}: {p} vs {q}");
    }
}

#[test]
fn qasm2_features() {
    let source = "OPENQASM 2.0;
include \"qelib1.inc\";
gate majority a, b, c { cx c, b; cx c, a; ccx a, b, c; }
gate spin(theta) a { u3(theta, 0, pi) a; u2(0, pi) a; }
qreg q[3];
qreg r[3];
creg c[3];
creg d[3];
x q;
spin(pi / 2) r[0];
majority q[0], q[1], q[2];
cx q, r;
measure q -> c;
if (c == 4) x r[1];
measure r -> d;
";
    let program = compile(source, 1);
    assert_eq!(program.num_qubits, 6);
    assert_eq!(program.num_results, 6);
    let counts = run(&program, 200, 1).counts;
    assert_eq!(counts.keys().collect::<Vec<_>>(), ["001011"]);
}

#[test]
fn equivalent_to_qir() {
    let qasm = "OPENQASM 3.0;
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
c[0] = measure q[0];
c[1] = measure q[1];
";
    let compiled = driver::compile_for(
        qasm,
        3,
        false,
        &driver::Target {
            gates: Some(qirc::transpile::GateSet::parse("rz-sx-cx").unwrap()),
            ..Default::default()
        },
    );
    let differences = equiv::compare(
        &equiv::explore(&compile(qasm, 0)),
        &equiv::explore(&compiled.program),
        true,
    );
    assert!(differences.is_empty(), "{differences:?}");
}

#[test]
fn errors_point_at_source() {
    for (source, message) in [
        (
            "OPENQASM 3.0;\nqubit[2] q;\nfoo q[0];\n",
            "unknown gate `foo`",
        ),
        ("OPENQASM 3.0;\nqubit[2] q;\nh q[2];\n", "out of range"),
        (
            "OPENQASM 3.0;\nqubit q;\nint x = 1;\n",
            "`int` is not supported yet",
        ),
        (
            "OPENQASM 3.0;\nqubit[2] q;\ncx q[0];\n",
            "takes 0 parameter(s) and 2 qubit(s)",
        ),
        (
            "OPENQASM 3.0;\nqubit[2] q;\ncx q[1], q[1];\n",
            "same qubit twice",
        ),
        (
            "OPENQASM 3.0;\nqubit[2] q;\nqubit[3] r;\ncx q, r;\n",
            "different sizes",
        ),
    ] {
        let compilation = driver::compile(source, 1);
        let found = errors(&compilation);
        assert!(
            found.iter().any(|e| e.contains(message)),
            "{source}: {found:?}"
        );
        assert!(compilation.diagnostics[0].primary_span().is_some());
    }
}

#[test]
fn hostile_inputs() {
    let deep = format!(
        "OPENQASM 3.0;\nqubit q;\nrz({}1{}) q;\n",
        "(".repeat(100_000),
        ")".repeat(100_000)
    );
    let nested = format!(
        "OPENQASM 3.0;\nqubit q;\nbit c;\nc = measure q;\n{}x q;{}\n",
        "if (c) { ".repeat(10_000),
        " }".repeat(10_000)
    );
    let mut doubling = String::from("OPENQASM 2.0;\nqreg q[1];\ngate g0 a { x a; }\n");
    for i in 1..40 {
        doubling += &format!("gate g{i} a {{ g{} a; g{} a; }}\n", i - 1, i - 1);
    }
    doubling += "g39 q[0];\n";
    for (source, message) in [
        (deep, "nests too deeply"),
        (nested, "nest too deeply"),
        (doubling, "more than 5000000 operations"),
    ] {
        let found = errors(&driver::compile(&source, 1)).join("\n");
        assert!(found.contains(message), "{found}");
    }
}

#[test]
fn qasm2_round_trip() {
    for source in [
        include_str!("corpus/base_profile_bell.ll"),
        TELEPORT_QIR,
        include_str!("corpus/qsharp_ising.ll"),
    ] {
        let program = compile(source, 2);
        let text = qirc::qasm2::emit(&program).expect("qasm2");
        assert!(text.starts_with("OPENQASM 2.0;"));
        let back = compile(&text, 0);
        let (a, b) = (
            probabilities(&equiv::explore(&program)),
            probabilities(&equiv::explore(&back)),
        );
        assert_eq!(a.len(), b.len(), "{text}");
        for ((x, p), (y, q)) in a.iter().zip(&b) {
            assert_eq!(x, y);
            assert!((p - q).abs() < 1e-9, "{x}: {p} vs {q}");
        }
    }
    let teleport = qirc::qasm2::emit(&compile(TELEPORT_QIR, 2)).unwrap();
    assert!(teleport.contains("if(c1==1) x q[2];"), "{teleport}");
    let looping = compile(include_str!("../examples/repeat_until_success.ll"), 1);
    assert!(qirc::qasm2::emit(&looping).is_err());
}
