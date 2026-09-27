mod common;

use common::compile;
use qirc::driver::{self, Target};
use qirc::equiv;
use qirc::ir::*;
use qirc::route::Coupling;
use qirc::simulator::matrix::C64;
use qirc::synth::Cost;
use qirc::transpile::GateSet;

fn module(body: &str) -> String {
    format!(
        "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {{
entry:
{body}  ret void
}}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__t__body(%Qubit*)
declare void @__quantum__qis__s__body(%Qubit*)
declare void @__quantum__qis__rz__body(double, %Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__cz__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__swap__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)

attributes #0 = {{ \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"3\" \"required_num_results\"=\"0\" }}
"
    )
}

fn call(name: &str, qubits: &[u32]) -> String {
    let args: Vec<String> = qubits
        .iter()
        .map(|q| format!("%Qubit* inttoptr (i64 {q} to %Qubit*)"))
        .collect();
    format!(
        "  call void @__quantum__qis__{name}__body({})\n",
        args.join(", ")
    )
}

fn rz(angle: f64, qubit: u32) -> String {
    format!(
        "  call void @__quantum__qis__rz__body(double {angle:?}, %Qubit* inttoptr (i64 {qubit} to %Qubit*))\n"
    )
}

fn measure(qubit: u32) -> String {
    format!(
        "  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 {qubit} to %Qubit*), %Result* null)
"
    )
}

fn targeted(source: &str, level: u8, gates: Option<&str>, resynth: Option<usize>) -> Program {
    checked(
        source,
        level,
        &Target {
            gates: gates.map(|g| GateSet::parse(g).unwrap()),
            resynth,
            ..Default::default()
        },
    )
}

fn checked(source: &str, level: u8, target: &Target) -> Program {
    let compilation = driver::compile_for(source, level, false, target);
    let reference = equiv::explore(&compile(source, 0));
    let differences = equiv::compare(&reference, &equiv::explore(&compilation.program), true);
    assert!(differences.is_empty(), "{differences:?}\n{source}");
    compilation.program
}

fn names(program: &Program) -> Vec<String> {
    program
        .gates()
        .map(|g| format!("{}{}", "c".repeat(g.controls.len()), g.kind.name()))
        .collect()
}

#[test]
fn identities() {
    let source = module(
        &[
            call("h", &[0]),
            call("h", &[0]),
            call("t", &[1]),
            call("t", &[1]),
        ]
        .concat(),
    );
    assert_eq!(names(&targeted(&source, 0, None, Some(1))), ["s"]);
}

#[test]
fn swap_from_cx() {
    let source = module(
        &[
            call("cx", &[0, 1]),
            call("cx", &[1, 0]),
            call("cx", &[0, 1]),
        ]
        .concat(),
    );
    assert_eq!(names(&targeted(&source, 0, None, Some(1))), ["swap"]);
}

#[test]
fn longer_rewrites() {
    let source = module(&[call("cx", &[0, 1]), call("x", &[0]), call("cx", &[0, 1])].concat());
    assert_eq!(targeted(&source, 0, None, Some(1)).gate_count(), 3);
    assert_eq!(names(&targeted(&source, 0, None, Some(2))), ["x", "x"]);
}

#[test]
fn fewest_cnots() {
    let source = module(
        &[
            call("cx", &[0, 1]),
            rz(0.3, 1),
            call("cx", &[0, 1]),
            call("h", &[0]),
            call("cx", &[1, 0]),
            call("t", &[1]),
            call("cx", &[0, 1]),
            rz(0.7, 0),
            call("cx", &[1, 0]),
            call("s", &[1]),
            call("cx", &[0, 1]),
        ]
        .concat(),
    );
    let entanglers = |cost| {
        let target = Target {
            gates: Some(GateSet::parse("rz-sx-cx").unwrap()),
            cost,
            ..Default::default()
        };
        checked(&source, 3, &target)
            .gates()
            .filter(|g| g.wires().count() == 2)
            .count()
    };
    assert_eq!(entanglers(Cost::Gates), 6);
    assert!(entanglers(Cost::Cx) <= 3);
    assert!(entanglers(Cost::Ibm) <= 3);
}

#[test]
fn skipped_qubits() {
    let source = module(
        &[
            call("h", &[0]),
            call("x", &[1]),
            call("cx", &[0, 1]),
            call("h", &[0]),
        ]
        .concat(),
    );
    for limit in 1..=6 {
        targeted(&source, 0, None, Some(limit));
    }
}

#[test]
fn commuting_cancel() {
    let source = module(&[rz(0.5, 0), call("cx", &[0, 1]), rz(-0.5, 0)].concat());
    assert_eq!(names(&targeted(&source, 1, None, None)), ["cx"]);

    let blocked = module(&[call("h", &[1]), call("cx", &[0, 1]), call("h", &[1])].concat());
    assert_eq!(targeted(&blocked, 1, None, None).gate_count(), 3);
}

#[test]
fn gate_sets() {
    let source = module(
        &[
            call("h", &[0]),
            call("cx", &[0, 1]),
            call("t", &[1]),
            call("cz", &[1, 2]),
        ]
        .concat(),
    );
    for set in ["rz-sx-cx", "rz-ry-cz", "rx,ry,cy", "h,s,t,cx", "rz,sx,x,ch"] {
        let allowed = GateSet::parse(set).unwrap();
        for resynth in [None, Some(4)] {
            let program = targeted(&source, 2, Some(set), resynth);
            for gate in program.gates() {
                assert!(allowed.allows(gate), "{set} left {:?}", gate.kind);
            }
        }
    }
}

#[test]
fn exclusion() {
    let args: Vec<String> = ["x.ll", "--exclude", "h,cx"].map(String::from).to_vec();
    let options = driver::parse_args(&args).unwrap();
    let set = options.gates.unwrap();
    assert!(!set.has(GateKind::H, 0));
    assert!(!set.has(GateKind::X, 1));
    assert!(set.has(GateKind::Z, 1));

    let source = module(&[call("h", &[0]), call("cx", &[0, 1])].concat());
    let program = targeted(
        &source,
        2,
        Some("x,y,z,s,sdg,t,tdg,sx,sxdg,rx,ry,rz,cz"),
        None,
    );
    assert!(names(&program).iter().all(|n| n != "h" && n != "cx"));
}

#[test]
fn detects_changes() {
    let one = compile(&module(&[call("h", &[0]), measure(0)].concat()), 0);
    let other = compile(&module(&[call("x", &[0]), measure(0)].concat()), 0);
    let differences = equiv::compare(&equiv::explore(&one), &equiv::explore(&other), true);
    assert!(!differences.is_empty());

    let phased = compile(&module(&[call("x", &[0]), call("s", &[0])].concat()), 0);
    let plain = compile(&module(&call("x", &[0])), 0);
    let differences = equiv::compare(&equiv::explore(&phased), &equiv::explore(&plain), true);
    assert!(differences.is_empty(), "{differences:?}");

    let relative = compile(&module(&[call("h", &[0]), call("s", &[0])].concat()), 0);
    let plain = compile(&module(&call("h", &[0])), 0);
    let differences = equiv::compare(&equiv::explore(&relative), &equiv::explore(&plain), true);
    assert_eq!(differences.len(), 1, "{differences:?}");
}

#[test]
fn diff_args() {
    let args: Vec<String> = ["diff", "a.ll", "b.ll", "-O3", "--resynth", "4"]
        .map(String::from)
        .to_vec();
    let options = driver::parse_args(&args).unwrap();
    assert!(options.diff);
    assert_eq!(options.other.unwrap().to_str(), Some("b.ll"));
    assert_eq!(options.resynth, Some(4));

    let args: Vec<String> = ["a.ll", "b.ll"].map(String::from).to_vec();
    assert!(driver::parse_args(&args).is_err());
    let args: Vec<String> = ["a.ll", "--resynth", "9"].map(String::from).to_vec();
    assert!(driver::parse_args(&args).is_err());
}

#[test]
fn endless_loop() {
    let source = "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  br label %loop
loop:
  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  br label %loop
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let outcomes = equiv::explore(&compile(source, 0));
    assert!(outcomes.branches.is_empty());
    assert!(outcomes.unexplored > 0.99);
}

#[test]
fn moved_states() {
    let mut seed = 11u64;
    let mut next = |bound: u32| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) % u64::from(bound)) as u32
    };
    for coupling in [None, Some("line:6"), Some("grid:2x3"), Some("ring:6")] {
        let mut body = String::new();
        for step in 0..30 {
            let a = next(5);
            let b = (a + 1 + next(4)) % 5;
            body += &call("h", &[a]);
            body += &call("t", &[b]);
            body += &call(if step % 3 == 0 { "swap" } else { "cx" }, &[a, b]);
        }
        let source = module(&body).replace(
            "\"required_num_qubits\"=\"3\"",
            "\"required_num_qubits\"=\"5\"",
        );
        let original = common::final_state(&compile(&source, 0));
        let compiled = driver::compile_for(
            &source,
            2,
            false,
            &Target {
                coupling: coupling.and_then(Coupling::parse),
                relabel: true,
                ..Default::default()
            },
        );
        let relabelled = compiled.relabelled.unwrap();
        assert!(relabelled.swaps_removed > 0);
        let layout = match compiled.routed {
            Some(routed) => routed.final_layout,
            None => relabelled.final_layout,
        };
        let state = common::final_state(&compiled.program);
        let mut overlap = C64::new(0.0, 0.0);
        for logical in 0..original.len() {
            let physical: usize = (0..5)
                .filter(|q| logical >> q & 1 == 1)
                .map(|q| 1 << layout[q])
                .sum();
            overlap += original.amplitude(logical).conj() * state.amplitude(physical);
        }
        assert!(
            (overlap.norm() - 1.0).abs() < 1e-9,
            "{coupling:?}: overlap {}",
            overlap.norm()
        );
    }
}
