mod common;

use std::f64::consts::FRAC_PI_8;

use common::{compile, errors, final_state, run};
use qirc::codegen;
use qirc::diag::Severity;
use qirc::driver::{self, Emit};
use qirc::ir::*;
use qirc::route::{self, Coupling};
use qirc::simulator::exec::{self, ExecConfig};
use qirc::simulator::state::{self, State};
use qirc::transpile::GateSet;

const BELL: &str = include_str!("corpus/base_profile_bell.ll");
const TELEPORT: &str = include_str!("corpus/adaptive_teleport.ll");
const PYQIR: &str = include_str!("corpus/pyqir_simple.ll");

const REDUNDANT: &str = "\
%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__ry__body(double 0.9, %Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__t__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__t__adj(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__rz__body(double 0.3, %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__rz__body(double 0.4, %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__z__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__z__body(%Qubit*)
declare void @__quantum__qis__t__body(%Qubit*)
declare void @__quantum__qis__t__adj(%Qubit*)
declare void @__quantum__qis__ry__body(double, %Qubit*)
declare void @__quantum__qis__rz__body(double, %Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"unrestricted\" \"required_num_qubits\"=\"3\" \"required_num_results\"=\"0\" }
";

const DYNAMIC_ROTATION: &str = "\
%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  %bit = call i1 @__quantum__qis__read_result__body(%Result* inttoptr (i64 0 to %Result*))
  %theta = select i1 %bit, double 1.25, double -0.75
  call void @__quantum__qis__ry__body(double %theta, %Qubit* inttoptr (i64 0 to %Qubit*))
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)
declare void @__quantum__qis__ry__body(double, %Qubit*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";

fn final_probabilities(program: &Program) -> Vec<f64> {
    final_state(program).probabilities()
}

fn codes(source: &str, level: u8) -> Vec<&'static str> {
    driver::compile(source, level)
        .diagnostics
        .iter()
        .filter_map(|d| d.code)
        .collect()
}

fn assert_same_state(left: &State, right: &State) {
    assert_eq!(left.len(), right.len());

    let pivot = (0..left.len())
        .find(|&index| right.amplitude(index).norm() > 1e-12)
        .expect("a normalized state has a nonzero amplitude");
    let phase = left.amplitude(pivot) / right.amplitude(pivot);

    for index in 0..left.len() {
        let difference = left.amplitude(index) - phase * right.amplitude(index);
        assert!(
            difference.norm() < 1e-9,
            "states differ at basis state {index}: {} vs {}",
            left.amplitude(index),
            right.amplitude(index)
        );
    }
}

#[test]
fn bell_amplitudes() {
    let program = compile(BELL, 1);
    let probabilities = final_probabilities(&program);

    assert!((probabilities[0b00] - 0.5).abs() < 1e-12);
    assert!((probabilities[0b11] - 0.5).abs() < 1e-12);
    assert!(probabilities[0b01].abs() < 1e-12);
    assert!(probabilities[0b10].abs() < 1e-12);
}

#[test]
fn bell_shots() {
    let outcome = run(&compile(BELL, 1), 4000, 24);

    assert_eq!(outcome.counts.len(), 2);
    assert!(outcome.counts.contains_key("00"));
    assert!(outcome.counts.contains_key("11"));

    let zeros = outcome.counts["00"] as f64 / 4000.0;
    assert!((zeros - 0.5).abs() < 0.05, "observed {zeros}");
}

#[test]
fn opt_state() {
    let baseline = final_state(&compile(REDUNDANT, 0));

    for level in 1..=3 {
        let optimised = final_state(&compile(REDUNDANT, level));
        assert_same_state(&baseline, &optimised);
    }
}

#[test]
fn opt_gates() {
    let unoptimised = driver::compile(REDUNDANT, 0);
    let optimised = driver::compile(REDUNDANT, 2);

    assert_eq!(unoptimised.program.gate_count(), 13);
    assert!(optimised.program.gate_count() < unoptimised.program.gate_count());
    assert!(optimised.stats.gates_removed() >= 5);
}

#[test]
fn opt_levels_teleport() {
    let expected = FRAC_PI_8.sin().powi(2);

    for level in 0..=3 {
        let outcome = run(&compile(TELEPORT, level), 3000, 555);

        let teleported = outcome
            .counts
            .iter()
            .filter(|(bits, _)| bits.chars().nth(2) == Some('1'))
            .map(|(_, count)| *count)
            .sum::<u64>() as f64
            / 3000.0;

        assert!(
            (teleported - expected).abs() < 0.04,
            "-O{level} teleported {teleported}, expected about {expected}"
        );
    }
}

#[test]
fn qir_roundtrip() {
    for source in [BELL, PYQIR] {
        let original = compile(source, 0);
        let emitted = codegen::emit_qir(&original).unwrap();
        let reparsed = compile(&emitted, 0);

        assert_eq!(original.num_qubits, reparsed.num_qubits);
        assert_eq!(original.num_results, reparsed.num_results);
        assert_eq!(original.gate_count(), reparsed.gate_count());
        assert_eq!(original.measure_count(), reparsed.measure_count());
        assert_eq!(original.profile, reparsed.profile);

        let shape = |p: &Program| {
            p.gates()
                .map(|g| (g.kind, g.controls.clone(), g.targets.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&original), shape(&reparsed));
    }
}

#[test]
fn qir_roundtrip_fused() {
    let optimised = compile(PYQIR, 3);
    assert!(
        optimised
            .gates()
            .any(|gate| matches!(gate.kind, GateKind::Unitary(_)))
    );

    let emitted = codegen::emit_qir(&optimised).unwrap();
    assert!(!emitted.contains("__quantum__qis__unitary__body"));
    assert!(emitted.contains("__quantum__qis__ry__body"));
    assert!(emitted.contains("__quantum__qis__rz__body"));

    let reparsed = compile(&emitted, 0);
    assert_same_state(&final_state(&optimised), &final_state(&reparsed));
}

#[test]
fn qir_roundtrip_dynamic_angle() {
    for level in 0..=3 {
        let original = compile(DYNAMIC_ROTATION, level);
        let emitted = codegen::emit_qir(&original).unwrap();

        assert!(
            emitted.contains("@__quantum__qis__ry__body(double %v"),
            "-O{level}\n{emitted}"
        );

        let reparsed = compile(&emitted, 0);
        let before = exec::execute(
            &original,
            ExecConfig {
                shots: 40,
                seed: 91,
                keep_state: true,
            },
        );
        let after = exec::execute(
            &reparsed,
            ExecConfig {
                shots: 40,
                seed: 91,
                keep_state: true,
            },
        );

        assert_eq!(before.counts, after.counts, "-O{level} changed outcomes");
        assert_same_state(
            before.final_state.as_ref().expect("the original state"),
            after.final_state.as_ref().expect("the round-tripped state"),
        );
    }
}

#[test]
fn qir_roundtrip_simulates() {
    let original = compile(PYQIR, 0);
    let emitted = codegen::emit_qir(&original).unwrap();
    let reparsed = compile(&emitted, 0);

    let before = final_probabilities(&original);
    let after = final_probabilities(&reparsed);

    for (index, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(
            (a - b).abs() < 1e-12,
            "round trip changed basis state {index}: {a} vs {b}"
        );
    }
}

#[test]
fn qasm3_output() {
    let program = compile(PYQIR, 0);
    let qasm = codegen::emit_qasm3(&program).unwrap();

    assert!(qasm.starts_with("OPENQASM 3.0;"));
    assert!(qasm.contains("include \"stdgates.inc\";"));
    assert!(qasm.contains("qubit[3] q;"));
    assert!(qasm.contains("bit[2] c;"));
    assert!(qasm.contains("ccx q[0], q[1], q[2];"));
    assert!(qasm.contains("tdg q[1];"));
    assert!(qasm.contains("swap q[0], q[1];"));
    assert!(qasm.contains("c[0] = measure q[0];"));
}

#[test]
fn json_output() {
    let program = compile(BELL, 0);
    let json = codegen::emit_json(&program);

    assert!(json.contains("\"qubits\": 2"));
    assert!(json.contains("\"results\": 2"));
    assert!(json.contains("\"gateCount\": 2"));
    assert!(json.contains("\"op\": \"gate\""));
    assert!(json.contains("\"name\": \"h\""));
    assert!(json.contains("\"op\": \"measure\""));
}

#[test]
fn circuit_diagram() {
    let program = compile(BELL, 0);
    let diagram = codegen::emit_circuit(&program);

    let lines: Vec<&str> = diagram.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("q0:"));
    assert!(lines[1].starts_with("q1:"));
    assert!(lines[0].contains('H'));
    assert!(lines[0].contains('*'));
    assert!(lines[1].contains('+'));
    assert!(lines[0].contains('M'));
}

#[test]
fn diagram_crossing() {
    let source = "\
%Qubit = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__cx__body(%Qubit* null, %Qubit* inttoptr (i64 2 to %Qubit*))
  ret void
}

declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"3\" \"required_num_results\"=\"0\" }
";
    let diagram = codegen::emit_circuit(&compile(source, 0));
    let middle = diagram.lines().nth(1).unwrap();
    assert_eq!(middle.matches('|').count(), 1, "{diagram}");
}

#[test]
fn base_profile_feedback() {
    let source = TELEPORT.replace("adaptive_profile", "base_profile");
    let compilation = driver::compile(&source, 0);

    let errors: Vec<&str> = compilation
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .filter_map(|d| d.code)
        .collect();

    assert!(
        errors.contains(&"QIR0300") || errors.contains(&"QIR0301"),
        "expected a Base Profile violation, got {errors:?}"
    );
}

#[test]
fn read_before_measure() {
    let source = "\
%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  %0 = call i1 @__quantum__qis__read_result__body(%Result* inttoptr (i64 0 to %Result*))
  br i1 %0, label %a, label %b
a:
  ret void
b:
  ret void
}
declare i1 @__quantum__qis__read_result__body(%Result*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";

    let codes = codes(source, 0);
    assert!(codes.contains(&"QIR0308"), "{codes:?}");
}

#[test]
fn repeated_qubit() {
    let source = "\
%Qubit = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 0 to %Qubit*))
  ret void
}
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
attributes #0 = { \"entry_point\" \"required_num_qubits\"=\"1\" }
";

    let codes = codes(source, 0);
    assert!(codes.contains(&"QIR0303"), "{codes:?}");
}

#[test]
fn cli_args() {
    let args: Vec<String> = [
        "in.ll", "--emit", "qasm3", "-O2", "--shots", "500", "--seed", "9",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let options = driver::parse_args(&args).expect("valid arguments");
    assert_eq!(options.input.to_str(), Some("in.ll"));
    assert_eq!(options.emit, Emit::Qasm3);
    assert_eq!(options.opt_level, 2);
    assert_eq!(options.shots, 500);
    assert_eq!(options.seed, Some(9));
}

#[test]
fn cli_bad_args() {
    assert!(driver::parse_args(&["--emit".into(), "wat".into()]).is_err());
    assert!(driver::parse_args(&["a.ll".into(), "b.ll".into()]).is_err());
    assert!(driver::parse_args(&["-O9".into(), "a.ll".into()]).is_err());
    assert!(driver::parse_args(&[]).is_err());
    assert!(driver::parse_args(&["--nope".into()]).is_err());
}

#[test]
fn seeded_runs() {
    let program = compile(TELEPORT, 1);

    let first = run(&program, 200, 1234);
    let second = run(&program, 200, 1234);

    assert_eq!(first.counts, second.counts);
}

#[test]
fn fast_path() {
    let bell = compile(BELL, 1);
    assert!(!exec::needs_per_shot(&bell));

    let teleport = compile(TELEPORT, 1);
    assert!(exec::needs_per_shot(&teleport));
}

#[test]
fn qir_roundtrip_branches() {
    let original = compile(TELEPORT, 0);
    let emitted = codegen::emit_qir(&original).unwrap();

    assert!(emitted.contains("read_result"));
    assert!(emitted.contains("br i1 %v"));

    let reparsed = compile(&emitted, 0);
    assert_eq!(reparsed.blocks.len(), original.blocks.len());
    assert_eq!(reparsed.gate_count(), original.gate_count());
    assert_eq!(reparsed.profile, Profile::Adaptive);
    assert!(!reparsed.is_straight_line());

    let before = run(&original, 2000, 31);
    let after = run(&reparsed, 2000, 31);
    assert_eq!(before.counts, after.counts);
}

#[test]
fn exact_doubles() {
    let original = compile(TELEPORT, 0);
    let emitted = codegen::emit_qir(&original).unwrap();
    let reparsed = compile(&emitted, 0);

    let before = original.gates().find(|g| g.kind == GateKind::Ry).unwrap();
    let after = reparsed.gates().find(|g| g.kind == GateKind::Ry).unwrap();

    assert_eq!(
        before.constant_angle().unwrap().to_bits(),
        after.constant_angle().unwrap().to_bits()
    );
}

#[test]
fn qir_roundtrip_switch() {
    let source = "%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  %bit = call i1 @__quantum__qis__read_result__body(%Result* inttoptr (i64 0 to %Result*))
  %0 = zext i1 %bit to i64
  switch i64 %0, label %other [
    i64 0, label %zero
    i64 1, label %one
  ]
zero:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  ret void
one:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  ret void
other:
  ret void
}
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";

    let program = compile(source, 0);
    let emitted = codegen::emit_qir(&program).unwrap();
    assert!(emitted.contains("switch i64"), "{emitted}");

    let reparsed = compile(&emitted, 0);
    let has_switch = reparsed
        .blocks
        .iter()
        .any(|b| matches!(b.term, Term::Switch { .. }));
    assert!(has_switch);
}

const MUTABLE_CLASSICAL: &str = "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  %flag = alloca i1
  %count = alloca i64
  store i1 true, ptr %flag
  store i64 3, ptr %count
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  %loaded = load i1, ptr %flag
  br i1 %loaded, label %yes, label %no
yes:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  store i1 false, ptr %flag
  br label %join
no:
  call void @__quantum__qis__z__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  br label %join
join:
  %again = load i1, ptr %flag
  br i1 %again, label %no, label %done
done:
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__z__body(%Qubit*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"unrestricted\" \"required_num_qubits\"=\"2\" }
";

#[test]
fn classical_control() {
    let program = compile(MUTABLE_CLASSICAL, 0);

    assert!(program.is_straight_line());
    assert_eq!(program.num_slots, 0);
}

#[test]
fn memory_slots() {
    let program = compile(STORED_FEEDBACK, 0);

    assert_eq!(program.num_slots, 1);

    let stores = program
        .ops()
        .filter(|op| matches!(op, Op::Store { .. }))
        .count();
    assert_eq!(stores, 1);

    let loads = program
        .ops()
        .filter(|op| {
            matches!(
                op,
                Op::Assign {
                    expr: Expr::Load(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(loads, 1);
}

#[test]
fn store_then_load() {
    let program = compile(MUTABLE_CLASSICAL, 0);
    let outcome = exec::execute(
        &program,
        ExecConfig {
            shots: 1,
            seed: 5,
            keep_state: true,
        },
    );

    let state = outcome.final_state.expect("a final state");
    assert!(state.qubit_probability(1) > 0.99);
}

#[test]
fn qir_roundtrip_slots() {
    let original = compile(STORED_FEEDBACK, 0);
    let emitted = codegen::emit_qir(&original).unwrap();

    assert!(emitted.contains("alloca"), "{emitted}");
    assert!(emitted.contains("store i64"));
    assert!(emitted.contains("load i64"));

    let reparsed = compile(&emitted, 0);
    assert_eq!(reparsed.num_slots, original.num_slots);
    assert_eq!(reparsed.gate_count(), original.gate_count());
}

const MID_CIRCUIT: &str = "\
%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"2\" }
";

const STORED_FEEDBACK: &str = "\
%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  %slot = alloca i64
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  %bit = call i1 @__quantum__qis__read_result__body(%Result* inttoptr (i64 0 to %Result*))
  %val = select i1 %bit, i64 1, i64 0
  store i64 %val, ptr %slot
  %back = load i64, ptr %slot
  %c = icmp eq i64 %back, 1
  br i1 %c, label %flip, label %done
flip:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  br label %done
done:
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"2\" \"required_num_results\"=\"2\" }
";

#[test]
fn gate_after_measure() {
    let program = compile(MID_CIRCUIT, 1);
    assert!(exec::needs_per_shot(&program));
}

#[test]
fn mid_circuit_distribution() {
    let outcome = run(&compile(MID_CIRCUIT, 1), 8000, 11);

    assert_eq!(outcome.counts.len(), 4);
    for bits in ["00", "01", "10", "11"] {
        let share = outcome.counts[bits] as f64 / 8000.0;
        assert!(
            (share - 0.25).abs() < 0.03,
            "{bits} occurred {share} of the time, expected about 0.25"
        );
    }
}

#[test]
fn final_measure_fast_path() {
    let program = compile(BELL, 1);
    assert!(!exec::needs_per_shot(&program));
}

#[test]
fn opt_stored_values() {
    let baseline = run(&compile(STORED_FEEDBACK, 0), 2000, 3);

    assert!(
        baseline
            .counts
            .keys()
            .all(|bits| bits == "00" || bits == "11"),
        "{:?}",
        baseline.counts
    );

    for level in 1..=3 {
        let optimised = run(&compile(STORED_FEEDBACK, level), 2000, 3);
        assert_eq!(
            baseline.counts, optimised.counts,
            "-O{level} changed the observable outcome"
        );
    }
}

#[test]
fn qasm3_runtime_angle() {
    let program = compile(DYNAMIC_ROTATION, 0);
    let qasm = codegen::emit_qasm3(&program).unwrap();

    assert!(qasm.contains("float[64] v"), "{qasm}");
    assert!(qasm.contains("= 1.25; } else {"), "{qasm}");
    assert!(qasm.contains("ry(v"), "{qasm}");
    assert!(
        !qasm.contains("ry(1.25") && !qasm.contains("ry(-0.75"),
        "{qasm}"
    );
}

#[test]
fn qasm3_outputs() {
    let program = compile(include_str!("corpus/qsharp_count.ll"), 1);
    let qasm = codegen::emit_qasm3(&program).unwrap();

    assert!(qasm.contains("output int[64] out0;"), "{qasm}");
    assert!(qasm.contains("out0 = v"), "{qasm}");
    assert!(qasm.contains(" + 1;"), "{qasm}");
}

#[test]
fn qasm3_empty_program() {
    let program = Program::new("empty", Profile::Unrestricted);
    assert!(
        codegen::emit_qasm3(&program)
            .unwrap()
            .starts_with("OPENQASM 3.0;")
    );
}

#[test]
fn qasm3_float_remainder() {
    let source = "\
%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %bit = call i1 @__quantum__rt__read_result(%Result* null)
  %x = select i1 %bit, double 2.5, double 1.5
  %r = frem double %x, 1.0
  call void @__quantum__qis__ry__body(double %r, %Qubit* null)
  ret void
}

declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__rt__read_result(%Result*)
declare void @__quantum__qis__ry__body(double, %Qubit*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let error = codegen::emit_qasm3(&compile(source, 0)).unwrap_err();
    assert!(error.contains("remainder"), "{error}");
}

#[test]
fn qasm3_branches() {
    let program = compile(TELEPORT, 1);
    let qasm = codegen::emit_qasm3(&program).unwrap();

    assert!(qasm.contains("if (c[1]) {\n    x q[2];\n}"), "{qasm}");
    assert!(qasm.contains("if (c[0]) {\n    z q[2];\n}"), "{qasm}");
    assert_eq!(qasm.matches("x q[2];").count(), 1, "{qasm}");
}

#[test]
fn qir_roundtrip_loop() {
    let source = "\
%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %bit = call i1 @__quantum__rt__read_result(%Result* null)
  %next = add i64 %i, 1
  %enough = icmp sge i64 %next, 3
  %stop = and i1 %bit, %enough
  br i1 %stop, label %done, label %loop
done:
  call void @__quantum__rt__int_record_output(i64 %next, i8* null)
  ret void
}

declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__rt__read_result(%Result*)
declare void @__quantum__rt__int_record_output(i64, i8*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let program = compile(source, 1);
    let again = compile(&codegen::emit_qir(&program).unwrap(), 1);

    for program in [&program, &again] {
        let outcome = run(program, 20, 3);
        assert!(!outcome.aborted);
        assert_eq!(outcome.returns.keys().collect::<Vec<_>>(), ["3"]);
    }
}

#[test]
fn qasm3_loop() {
    let source = "\
%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  br label %again
again:
  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %hit = call i1 @__quantum__qis__read_result__body(%Result* null)
  br i1 %hit, label %done, label %again
done:
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let qasm = codegen::emit_qasm3(&compile(source, 1)).unwrap();

    assert!(qasm.contains("uint[32] block = 1;"), "{qasm}");
    assert!(qasm.contains("while (block != 0) {"), "{qasm}");
    assert!(qasm.contains("block = 0;"), "{qasm}");
    assert_eq!(qasm.matches("h q[0];").count(), 1, "{qasm}");
}

#[test]
fn oversized_register() {
    let source = "\
%Qubit = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
attributes #0 = { \"entry_point\" \"required_num_qubits\"=\"70\" }
";

    let program = compile(source, 0);
    assert_eq!(program.num_qubits, 70);
    assert!(program.num_qubits as usize > state::MAX_QUBITS);
    assert_eq!(state::memory_required(70), None);
    assert!(state::memory_required(20).is_some());
}

const TARGET_SOURCE: &str = "%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__ry__body(double 0.7, %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__ccx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__swap__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__t__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__t__body(%Qubit*)
declare void @__quantum__qis__ry__body(double, %Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__ccx__body(%Qubit*, %Qubit*, %Qubit*)
declare void @__quantum__qis__swap__body(%Qubit*, %Qubit*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"3\" \"required_num_results\"=\"0\" }
";

fn compile_for_target(source: &str, level: u8, target: driver::Target) -> Program {
    let compilation = driver::compile_for(source, level, true, &target);
    let errors = errors(&compilation);
    assert!(errors.is_empty(), "compilation failed: {errors:?}");
    assert!(
        compilation.stats.violations.is_empty(),
        "the verifier rejected the targeted program: {:?}",
        compilation.stats.violations
    );
    compilation.program
}

#[test]
fn transpile_state() {
    let baseline = final_state(&compile(TARGET_SOURCE, 0));
    for set in ["rz-sx-cx", "rz-ry-cz"].map(|s| GateSet::parse(s).unwrap()) {
        let targeted = final_state(&compile_for_target(
            TARGET_SOURCE,
            0,
            driver::Target {
                gates: Some(set.clone()),
                coupling: None,
                resynth: None,
            },
        ));
        assert_same_state(&baseline, &targeted);
    }
}

#[test]
fn transpile_basis_only() {
    for set in ["rz-sx-cx", "rz-ry-cz"].map(|s| GateSet::parse(s).unwrap()) {
        let program = compile_for_target(
            TARGET_SOURCE,
            0,
            driver::Target {
                gates: Some(set.clone()),
                coupling: None,
                resynth: None,
            },
        );

        for gate in program.gates() {
            assert!(
                set.allows(gate),
                "{} left {:?} with {} controls behind",
                set.name(),
                gate.kind,
                gate.controls.len()
            );
        }
    }
}

#[test]
fn route_counts() {
    let source = "%Qubit = type opaque
%Result = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 3 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 3 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  ret void
}
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"4\" \"required_num_results\"=\"2\" }
";

    let plain = run(&compile(source, 0), 2000, 77);
    let routed = run(
        &compile_for_target(
            source,
            0,
            driver::Target {
                coupling: Some(Coupling::line(4)),
                ..Default::default()
            },
        ),
        2000,
        77,
    );

    assert_eq!(plain.counts, routed.counts);
    assert!(plain.counts.contains_key("00") && plain.counts.contains_key("11"));
}

#[test]
fn route_legal() {
    let source = "%Qubit = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 4 to %Qubit*))
  call void @__quantum__qis__cx__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 3 to %Qubit*))
  ret void
}
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"5\" }
";

    let coupling = Coupling::line(5);
    let program = compile_for_target(
        source,
        0,
        driver::Target {
            coupling: Some(coupling.clone()),
            ..Default::default()
        },
    );

    assert!(route::respects(&program, &coupling));
    assert!(program.gate_count() > 2);
}

#[test]
fn transpile_then_route() {
    let coupling = Coupling::line(3);
    let program = compile_for_target(
        TARGET_SOURCE,
        1,
        driver::Target {
            gates: Some(GateSet::parse("rz-sx-cx").unwrap()),
            coupling: Some(coupling.clone()),
            resynth: None,
        },
    );

    assert!(route::respects(&program, &coupling));
    let set = GateSet::parse("rz-sx-cx").unwrap();
    for gate in program.gates() {
        assert!(set.allows(gate), "{:?} survived both passes", gate.kind);
    }
}

#[test]
fn qsharp_loop() {
    let source = include_str!("corpus/qsharp_loop.ll");
    let program = compile(source, 0);

    assert!(program.is_straight_line());
    assert_eq!(program.num_qubits, 4);
    assert_eq!(program.gate_count(), 4);
    assert_eq!(program.measure_count(), 4);

    let kinds: Vec<GateKind> = program.gates().map(|g| g.kind).collect();
    assert_eq!(kinds[0], GateKind::H);
    assert!(kinds[1..].iter().all(|k| *k == GateKind::X));

    let probabilities = final_probabilities(&program);
    assert!((probabilities[0b0000] - 0.5).abs() < 1e-12);
    assert!((probabilities[0b1111] - 0.5).abs() < 1e-12);
}

#[test]
fn qubit_id_overflow() {
    let source = "%Qubit = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 4294967297 to %Qubit*))
  ret void
}
declare void @__quantum__qis__x__body(%Qubit*)
attributes #0 = { \"entry_point\" \"required_num_qubits\"=\"2\" }
";
    let compilation = driver::compile(source, 0);
    let errors = errors(&compilation);
    assert!(
        errors.iter().any(|e| e.contains("4294967297")),
        "{errors:?}"
    );
}

#[test]
fn huge_declared_register() {
    let source = "%Qubit = type opaque
define void @main() #0 {
entry:
  ret void
}
attributes #0 = { \"entry_point\" \"required_num_qubits\"=\"4000000000\" }
";
    let compilation = driver::compile(source, 3);
    let codes: Vec<_> = compilation
        .diagnostics
        .iter()
        .filter_map(|d| d.code)
        .collect();
    assert!(codes.contains(&"QIR0202"), "{codes:?}");
    assert_eq!(compilation.program.num_qubits, 0);
}

#[test]
fn self_loop() {
    let source = "%Qubit = type opaque
define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 0 to %Qubit*))
  br label %entry
}
declare void @__quantum__qis__h__body(%Qubit*)
attributes #0 = { \"entry_point\" \"required_num_qubits\"=\"1\" }
";
    let program = compile(source, 0);
    assert!(!program.is_straight_line());

    assert!(run(&program, 1, 1).aborted);
}
