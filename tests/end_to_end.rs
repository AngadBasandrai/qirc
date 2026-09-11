use qirc::codegen;
use qirc::diag::Severity;
use qirc::driver::{self, Emit};
use qirc::ir::*;
use qirc::simulator::exec::{self, ExecConfig};
use qirc::simulator::state::State;

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

fn compile_clean(source: &str, level: u8) -> Program {
    let compilation = driver::compile(source, level);
    let errors: Vec<String> = compilation
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.clone())
        .collect();
    assert!(errors.is_empty(), "compilation failed: {errors:?}");
    compilation.program
}

fn final_state(program: &Program) -> State {
    let outcome = exec::execute(
        program,
        ExecConfig {
            shots: 0,
            seed: 1,
            keep_state: true,
        },
    );
    outcome.final_state.expect("a final state")
}

fn final_probabilities(program: &Program) -> Vec<f64> {
    final_state(program).probabilities()
}

fn assert_states_equivalent(left: &State, right: &State) {
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
fn bell_pair_has_the_right_amplitudes() {
    let program = compile_clean(BELL, 1);
    let probabilities = final_probabilities(&program);

    assert!((probabilities[0b00] - 0.5).abs() < 1e-12);
    assert!((probabilities[0b11] - 0.5).abs() < 1e-12);
    assert!(probabilities[0b01].abs() < 1e-12);
    assert!(probabilities[0b10].abs() < 1e-12);
}

#[test]
fn bell_pair_shots_are_perfectly_correlated() {
    let program = compile_clean(BELL, 1);
    let outcome = exec::execute(
        &program,
        ExecConfig {
            shots: 4000,
            seed: 24,
            keep_state: false,
        },
    );

    assert_eq!(outcome.counts.keys().len(), 2, "only 00 and 11 may appear");
    assert!(outcome.counts.contains_key("00"));
    assert!(outcome.counts.contains_key("11"));

    let zeros = outcome.counts["00"] as f64 / 4000.0;
    assert!((zeros - 0.5).abs() < 0.05, "observed {zeros}");
}

#[test]
fn optimisation_preserves_the_state_vector() {
    let baseline = final_state(&compile_clean(REDUNDANT, 0));

    for level in 1..=3u8 {
        let optimised = final_state(&compile_clean(REDUNDANT, level));
        assert_states_equivalent(&baseline, &optimised);
    }
}

#[test]
fn optimisation_actually_removes_gates() {
    let unoptimised = driver::compile(REDUNDANT, 0);
    let optimised = driver::compile(REDUNDANT, 2);

    assert_eq!(unoptimised.program.gate_count(), 13);
    assert!(
        optimised.program.gate_count() < unoptimised.program.gate_count(),
        "optimisation removed nothing"
    );
    assert!(optimised.stats.gates_removed() >= 5);
}

#[test]
fn every_optimisation_level_keeps_the_measurement_distribution() {
    let mut distributions = Vec::new();

    for level in 0..=3u8 {
        let program = compile_clean(TELEPORT, level);
        let outcome = exec::execute(
            &program,
            ExecConfig {
                shots: 3000,
                seed: 555,
                keep_state: false,
            },
        );

        let teleported = outcome
            .counts
            .iter()
            .filter(|(bits, _)| bits.chars().nth(2) == Some('1'))
            .map(|(_, count)| *count)
            .sum::<u64>() as f64
            / 3000.0;

        distributions.push(teleported);
    }

    let expected = (std::f64::consts::FRAC_PI_8).sin().powi(2);
    for (level, observed) in distributions.iter().enumerate() {
        assert!(
            (observed - expected).abs() < 0.04,
            "-O{level} teleported {observed}, expected about {expected}"
        );
    }
}

#[test]
fn qir_round_trips_through_its_own_frontend() {
    for source in [BELL, PYQIR] {
        let original = compile_clean(source, 0);
        let emitted = codegen::emit_qir(&original);
        let reparsed = compile_clean(&emitted, 0);

        assert_eq!(original.num_qubits, reparsed.num_qubits);
        assert_eq!(original.num_results, reparsed.num_results);
        assert_eq!(original.gate_count(), reparsed.gate_count());
        assert_eq!(original.measure_count(), reparsed.measure_count());
        assert_eq!(original.profile, reparsed.profile);

        let before: Vec<(GateKind, Vec<QubitId>, Vec<QubitId>)> = original
            .gates()
            .map(|g| (g.kind, g.controls.clone(), g.targets.clone()))
            .collect();
        let after: Vec<(GateKind, Vec<QubitId>, Vec<QubitId>)> = reparsed
            .gates()
            .map(|g| (g.kind, g.controls.clone(), g.targets.clone()))
            .collect();

        assert_eq!(before, after, "gate sequence changed across a round trip");
    }
}

#[test]
fn fused_unitaries_are_synthesized_for_qir_round_trips() {
    let optimised = compile_clean(PYQIR, 3);
    assert!(
        optimised
            .gates()
            .any(|gate| matches!(gate.kind, GateKind::Unitary(_))),
        "the fixture should exercise O3 fusion"
    );

    let emitted = codegen::emit_qir(&optimised);
    assert!(!emitted.contains("__quantum__qis__unitary__body"));
    assert!(emitted.contains("__quantum__qis__ry__body"));
    assert!(emitted.contains("__quantum__qis__rz__body"));

    let reparsed = compile_clean(&emitted, 0);
    let before = exec::execute(
        &optimised,
        ExecConfig {
            shots: 0,
            seed: 7,
            keep_state: true,
        },
    );
    let after = exec::execute(
        &reparsed,
        ExecConfig {
            shots: 0,
            seed: 7,
            keep_state: true,
        },
    );

    assert_states_equivalent(
        before.final_state.as_ref().expect("the original state"),
        after.final_state.as_ref().expect("the round-tripped state"),
    );
}

#[test]
fn dynamic_rotation_parameters_survive_qir_round_trips() {
    for level in 0..=3 {
        let original = compile_clean(DYNAMIC_ROTATION, level);
        let emitted = codegen::emit_qir(&original);

        assert!(
            emitted.contains("@__quantum__qis__ry__body(double %v"),
            "-O{level} must preserve the computed SSA angle:\n{emitted}"
        );

        let reparsed = compile_clean(&emitted, 0);
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
        assert_states_equivalent(
            before.final_state.as_ref().expect("the original state"),
            after.final_state.as_ref().expect("the round-tripped state"),
        );
    }
}

#[test]
fn round_tripped_program_simulates_identically() {
    let original = compile_clean(PYQIR, 0);
    let emitted = codegen::emit_qir(&original);
    let reparsed = compile_clean(&emitted, 0);

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
fn qasm3_output_is_well_formed() {
    let program = compile_clean(PYQIR, 0);
    let qasm = codegen::emit_qasm3(&program);

    assert!(qasm.starts_with("OPENQASM 3.0;"));
    assert!(qasm.contains("include \"stdgates.inc\";"));
    assert!(qasm.contains("qubit[3] q;"));
    assert!(qasm.contains("bit[2] c;"));
    assert!(qasm.contains("ccx q[0], q[1], q[2];"));
    assert!(qasm.contains("tdg q[1];"));
    assert!(qasm.contains("swap q[0], q[1];"));
    assert!(qasm.contains("c[0] = measure q[0];"));
    assert!(!qasm.contains("unsupported"));
}

#[test]
fn json_output_reports_the_circuit() {
    let program = compile_clean(BELL, 0);
    let json = codegen::emit_json(&program);

    assert!(json.contains("\"qubits\": 2"));
    assert!(json.contains("\"results\": 2"));
    assert!(json.contains("\"gateCount\": 2"));
    assert!(json.contains("\"op\": \"gate\""));
    assert!(json.contains("\"name\": \"h\""));
    assert!(json.contains("\"op\": \"measure\""));
}

#[test]
fn circuit_diagram_shows_every_wire() {
    let program = compile_clean(BELL, 0);
    let diagram = codegen::emit_circuit(&program);

    let lines: Vec<&str> = diagram.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("q0:"));
    assert!(lines[1].starts_with("q1:"));
    assert!(lines[0].contains('H'));
    assert!(lines[0].contains('*'), "the control should be marked");
    assert!(lines[1].contains('+'), "the target should be marked");
    assert!(lines[0].contains('M'));
}

#[test]
fn base_profile_rejects_measurement_feedback() {
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
fn reading_an_unmeasured_result_is_an_error() {
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

    let compilation = driver::compile(source, 0);
    assert!(
        compilation
            .diagnostics
            .iter()
            .any(|d| d.code == Some("QIR0308")),
        "expected QIR0308 for reading an unmeasured result"
    );
}

#[test]
fn a_gate_may_not_touch_the_same_qubit_twice() {
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

    let compilation = driver::compile(source, 0);
    assert!(
        compilation
            .diagnostics
            .iter()
            .any(|d| d.code == Some("QIR0303")),
        "expected QIR0303 for a repeated wire"
    );
}

#[test]
fn cli_parses_its_options() {
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
fn cli_rejects_nonsense() {
    assert!(driver::parse_args(&["--emit".into(), "wat".into()]).is_err());
    assert!(driver::parse_args(&["a.ll".into(), "b.ll".into()]).is_err());
    assert!(driver::parse_args(&["-O9".into(), "a.ll".into()]).is_err());
    assert!(driver::parse_args(&[]).is_err());
    assert!(driver::parse_args(&["--nope".into()]).is_err());
}

#[test]
fn deterministic_seeds_give_deterministic_runs() {
    let program = compile_clean(TELEPORT, 1);

    let first = exec::execute(
        &program,
        ExecConfig {
            shots: 200,
            seed: 1234,
            keep_state: false,
        },
    );
    let second = exec::execute(
        &program,
        ExecConfig {
            shots: 200,
            seed: 1234,
            keep_state: false,
        },
    );

    assert_eq!(first.counts, second.counts);
}

#[test]
fn straight_line_programs_use_the_sampling_fast_path() {
    let bell = compile_clean(BELL, 1);
    assert!(!exec::needs_per_shot_simulation(&bell));

    let teleport = compile_clean(TELEPORT, 1);
    assert!(exec::needs_per_shot_simulation(&teleport));
}

#[test]
fn branching_programs_round_trip_with_their_control_flow() {
    let original = compile_clean(TELEPORT, 0);
    let emitted = codegen::emit_qir(&original);

    assert!(
        emitted.contains("read_result"),
        "the emitted module must define its branch condition"
    );
    assert!(
        emitted.contains("br i1 %v"),
        "expected a real conditional branch"
    );

    let reparsed = compile_clean(&emitted, 0);
    assert_eq!(reparsed.blocks.len(), original.blocks.len());
    assert_eq!(reparsed.gate_count(), original.gate_count());
    assert_eq!(reparsed.profile, Profile::Adaptive);
    assert!(!reparsed.is_straight_line());

    let before = exec::execute(
        &original,
        ExecConfig {
            shots: 2000,
            seed: 31,
            keep_state: false,
        },
    );
    let after = exec::execute(
        &reparsed,
        ExecConfig {
            shots: 2000,
            seed: 31,
            keep_state: false,
        },
    );
    assert_eq!(
        before.counts, after.counts,
        "round trip changed the statistics"
    );
}

#[test]
fn emitted_doubles_are_bit_exact() {
    let original = compile_clean(TELEPORT, 0);
    let emitted = codegen::emit_qir(&original);
    let reparsed = compile_clean(&emitted, 0);

    let before = original.gates().find(|g| g.kind == GateKind::Ry).unwrap();
    let after = reparsed.gates().find(|g| g.kind == GateKind::Ry).unwrap();

    assert_eq!(
        before.constant_angle().unwrap().to_bits(),
        after.constant_angle().unwrap().to_bits(),
        "the rotation angle lost precision across a round trip"
    );
}

#[test]
fn switch_terminators_survive_emission() {
    let source = "%Qubit = type opaque
define void @main() #0 {
entry:
  %0 = add i64 1, 0
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
attributes #0 = { \"entry_point\" \"qir_profiles\"=\"unrestricted\" \"required_num_qubits\"=\"1\" }
";

    let program = compile_clean(source, 0);
    let emitted = codegen::emit_qir(&program);
    assert!(
        emitted.contains("switch i64"),
        "switch was not emitted:
{emitted}"
    );

    let reparsed = compile_clean(&emitted, 0);
    let has_switch = reparsed
        .blocks
        .iter()
        .any(|b| matches!(b.term, Term::Switch { .. }));
    assert!(has_switch, "switch did not survive the round trip");
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
fn alloca_load_store_become_memory_slots() {
    let program = compile_clean(MUTABLE_CLASSICAL, 0);

    assert_eq!(program.num_slots, 2, "both allocas should get a slot");

    let stores = program
        .ops()
        .filter(|op| matches!(op, Op::Store { .. }))
        .count();
    assert_eq!(stores, 3);

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
    assert_eq!(loads, 2);
}

#[test]
fn stores_are_observed_by_later_loads() {
    let program = compile_clean(MUTABLE_CLASSICAL, 0);
    let outcome = exec::execute(
        &program,
        ExecConfig {
            shots: 1,
            seed: 5,
            keep_state: true,
        },
    );

    assert!(outcome.final_state.is_some());

    let state = outcome.final_state.unwrap();
    assert!(
        state.qubit_probability(1) > 0.99,
        "the true branch should have run X on q1, then the store should stop the loop"
    );
}

#[test]
fn memory_slots_survive_a_qir_round_trip() {
    let original = compile_clean(MUTABLE_CLASSICAL, 0);
    let emitted = codegen::emit_qir(&original);

    assert!(
        emitted.contains("alloca"),
        "slots must be declared:
{emitted}"
    );
    assert!(emitted.contains("store i1"), "stores must be emitted");
    assert!(emitted.contains("load i1"), "loads must be emitted");

    let reparsed = compile_clean(&emitted, 0);
    assert_eq!(reparsed.num_slots, original.num_slots);
    assert_eq!(reparsed.gate_count(), original.gate_count());
}
