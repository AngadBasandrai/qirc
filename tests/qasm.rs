mod common;

use std::f64::consts::PI;

use common::{compile, errors, final_state, run};
use qirc::calibration::Calibration;
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
            "OPENQASM 3.0;\nqubit q;\nbool x = true;\n",
            "`bool` is not supported yet",
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
        (
            "OPENQASM 2.0;\nqreg q[1];\ngate g a { barrier a".into(),
            "expected `;`",
        ),
        (
            "OPENQASM 3.0;\nqubit q;\nbit c;\nif (!c == 1) x q;\n".into(),
            "cannot be combined",
        ),
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

#[test]
fn stim() {
    let teleport = compile(
        "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[3];
creg c0[1];
creg c1[1];
creg c2[1];
h q[1];
cx q[1], q[2];
cx q[0], q[1];
h q[0];
measure q[0] -> c0[0];
measure q[1] -> c1[0];
if(c1==1) x q[2];
if(c0==0) z q[2];
measure q[2] -> c2[0];
",
        0,
    );
    assert_eq!(
        qirc::stim::emit(&teleport, None).unwrap(),
        "H 1\nCX 1 2\nCX 0 1\nH 0\nM 0\nM 1\nCX rec[-1] 2\nZ 2\nCZ rec[-2] 2\nM 2\n"
    );
    let calibration = Calibration::parse("cx 0 1 0.05\nsingle 0 0.01\nreadout 0 0.02\n").unwrap();
    let flip = compile(
        "OPENQASM 2.0;\nqreg q[1];\ncreg c[1];\nx q[0];\nmeasure q[0] -> c[0];\nreset q[0];\n",
        0,
    );
    assert_eq!(
        qirc::stim::emit(&flip, Some(&calibration)).unwrap(),
        "X 0\nDEPOLARIZE1(0.01) 0\nM(0.02) 0\nR 0\nX_ERROR(0.02) 0\n"
    );
    assert!(qirc::stim::emit(&teleport, Some(&calibration)).is_err());
    let rotation = compile("OPENQASM 2.0;\nqreg q[1];\nrx(0.3) q[0];\n", 0);
    assert!(qirc::stim::emit(&rotation, None).is_err());
}

#[test]
fn loops() {
    let looped = compile(
        "OPENQASM 3.0;
include \"stdgates.inc\";
const int n = 5;
qubit[n] q;
x q[0];
ry(0.3) q[2];
for int i in [0:n - 1] {
  h q[i];
  for int j in [i + 1:n - 1] {
    cp(pi / 2 ** (j - i)) q[j], q[i];
  }
}
for uint k in {0, 1} swap q[k], q[n - 1 - k];
for int dead in [3:2] { x q[0]; }
",
        0,
    );
    let mut flat = String::from(
        "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[5];\nx q[0];\nry(0.3) q[2];\n",
    );
    for i in 0..5 {
        flat += &format!("h q[{i}];\n");
        for j in i + 1..5 {
            flat += &format!("cu1({:?}) q[{j}], q[{i}];\n", PI / 2f64.powi(j - i));
        }
    }
    flat += "swap q[0], q[4];\nswap q[1], q[3];\n";
    let (a, b) = (final_state(&looped), final_state(&compile(&flat, 0)));
    for basis in 0..a.len() {
        assert!((a.amplitude(basis) - b.amplitude(basis)).norm() < 1e-9);
    }

    let repeat = compile(
        "OPENQASM 3.0;
include \"stdgates.inc\";
qubit q;
qubit a;
bit r;
bit out;
h a;
cx a, q;
r = measure a;
while (r) {
  reset a;
  h a;
  cx a, q;
  r = measure a;
}
out = measure q;
",
        1,
    );
    let counts = run(&repeat, 30_000, 2).counts;
    let flipped = counts.get("01").copied().unwrap_or(0) as f64 / 30_000.0;
    assert!((flipped - 1.0 / 3.0).abs() < 0.01, "{counts:?}");

    let halves = compile(
        "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nconst int n = 7 / 2;\nqubit[8] q;\nfor int i in [0:n] x q[i * 5 / 2];\nrx(1 / 2) q[0];\n",
        0,
    );
    let targets: Vec<u32> = halves.gates().map(|g| g.targets[0].0).collect();
    assert_eq!(targets, [0, 2, 5, 7, 0]);
    let angle = halves.gates().last().and_then(|g| g.constant_angle());
    assert_eq!(angle, Some(0.5));

    for (source, message) in [
        (
            "OPENQASM 3.0;\nint[4] big = 100;\n",
            "does not fit in `int[4]`",
        ),
        ("OPENQASM 3.0;\nuint u = -1;\n", "does not fit in `uint`"),
        ("OPENQASM 3.0;\nint k;\n", "needs a value"),
        ("OPENQASM 3.0;\nconst int n = 2;\nn = 3;\n", "cannot change"),
        (
            "OPENQASM 3.0;\nqubit q;\nfor int i in [0:0:3] x q;\n",
            "step other than 0",
        ),
    ] {
        let found = errors(&driver::compile(source, 1)).join("\n");
        assert!(found.contains(message), "{found}");
    }
}
