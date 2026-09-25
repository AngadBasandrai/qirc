mod common;

use std::collections::BTreeMap;
use std::f64::consts::PI;

use common::{compile, final_state, run};
use num_complex::Complex64;
use qirc::codegen;
use qirc::diag::Severity;
use qirc::driver;
use qirc::ir::*;

const BELL: &str = include_str!("corpus/qsharp_bell.ll");
const ISING: &str = include_str!("corpus/qsharp_ising.ll");
const TELEPORT: &str = include_str!("corpus/qsharp_teleport.ll");
const COUNT: &str = include_str!("corpus/qsharp_count.ll");

fn messages(source: &str, severity: Severity) -> Vec<String> {
    driver::compile(source, 1)
        .diagnostics
        .into_iter()
        .filter(|d| d.severity == severity)
        .map(|d| d.message)
        .collect()
}

fn keys(tally: &BTreeMap<String, u64>) -> Vec<&str> {
    tally.keys().map(String::as_str).collect()
}

fn module(qubits: u32, results: u32, body: &str) -> String {
    format!(
        "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {{
entry:
{body}  ret void
}}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__rxx__body(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__ryy__body(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__rzz__body(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__rzz__adj(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__mresetz__body(%Qubit*, %Result*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare void @__quantum__qis__frobnicate__body(%Qubit*)
declare i1 @__quantum__rt__read_result(%Result*)
declare void @__quantum__rt__result_record_output(%Result*, i8*)
declare void @__quantum__rt__int_record_output(i64, i8*)
declare void @__quantum__rt__array_record_output(i64, i8*)
declare void @__quantum__rt__tuple_start_record_output(i8*)
declare void @__quantum__rt__tuple_end_record_output(i8*)

attributes #0 = {{ \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"{qubits}\" \"required_num_results\"=\"{results}\" }}
"
    )
}

fn amplitudes(body: &str) -> Vec<Complex64> {
    let state = final_state(&compile(&module(2, 0, body), 0));
    (0..state.len()).map(|i| state.amplitude(i)).collect()
}

fn assert_amplitudes(actual: &[Complex64], expected: &[Complex64]) {
    for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((a - e).norm() < 1e-12, "basis state {index}: {a} vs {e}");
    }
}

const PAIR: &str = "%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*)";
const PLUS_PLUS: &str = "  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 1 to %Qubit*))
";

#[test]
fn bell() {
    let program = compile(BELL, 1);
    assert_eq!(program.num_results, 2);
    assert!(messages(BELL, Severity::Warning).is_empty());

    let outcome = run(&program, 2000, 3);
    assert_eq!(keys(&outcome.counts), ["00", "11"]);
    assert_eq!(keys(&outcome.returns), ["[0, 0]", "[1, 1]"]);
}

#[test]
fn ising() {
    assert!(messages(ISING, Severity::Warning).is_empty());
    let program = compile(ISING, 0);
    assert_eq!(program.num_results, 3);
    assert_eq!(program.gate_count(), 20);
}

#[test]
fn teleport() {
    let program = compile(TELEPORT, 1);
    assert!(messages(TELEPORT, Severity::Warning).is_empty());
    assert!(program.blocks.len() > 1);

    let returns = run(&program, 20000, 5).returns;
    assert_eq!(keys(&returns), ["0", "1"]);
    let share = returns["1"] as f64 / 20000.0;
    let expected = (PI / 8.0).sin().powi(2);
    assert!((share - expected).abs() < 0.015, "{share} vs {expected}");
}

#[test]
fn int_output() {
    let program = compile(COUNT, 1);
    assert!(messages(COUNT, Severity::Warning).is_empty());

    let recorded = program
        .ops()
        .filter(|op| {
            matches!(
                op,
                Op::RecordOutput {
                    kind: OutputKind::Int,
                    value: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(recorded, 1);
    assert_eq!(
        keys(&run(&program, 500, 2).returns),
        ["0", "1", "2", "3", "4"]
    );

    let qir = codegen::emit_qir(&program).unwrap();
    assert!(qir.contains("@__quantum__rt__int_record_output(i64 %"));
    compile(&qir, 1);
}

#[test]
fn labels_roundtrip() {
    let labels = |program: &Program| {
        program
            .ops()
            .filter_map(|op| match op {
                Op::RecordOutput { label, .. } => label.clone(),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let program = compile(BELL, 1);
    let again = compile(&codegen::emit_qir(&program).unwrap(), 1);
    assert_eq!(labels(&again), ["0_a", "1_a0r", "2_a1r"]);
}

#[test]
fn mresetz() {
    let body = "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mresetz__body(%Qubit* null, %Result* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* inttoptr (i64 1 to %Result*))
";
    let program = compile(&module(1, 2, body), 1);
    assert_eq!(
        run(&program, 500, 9).counts,
        BTreeMap::from([("10".to_string(), 500)])
    );
}

#[test]
fn mixed_outputs() {
    let source = "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %bit = call i1 @__quantum__rt__read_result(%Result* null)
  %n = select i1 %bit, i64 7, i64 0
  %d = select i1 %bit, double 0.5, double 1.5
  call void @__quantum__rt__tuple_record_output(i64 3, i8* null)
  call void @__quantum__rt__bool_record_output(i1 %bit, i8* null)
  call void @__quantum__rt__int_record_output(i64 %n, i8* null)
  call void @__quantum__rt__double_record_output(double %d, i8* null)
  ret void
}

declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__rt__read_result(%Result*)
declare void @__quantum__rt__tuple_record_output(i64, i8*)
declare void @__quantum__rt__bool_record_output(i1, i8*)
declare void @__quantum__rt__int_record_output(i64, i8*)
declare void @__quantum__rt__double_record_output(double, i8*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let program = compile(source, 1);
    assert_eq!(keys(&run(&program, 100, 4).returns), ["(true, 7, 0.5)"]);
}

#[test]
fn oversized_array() {
    let source = "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  call void @__quantum__rt__array_record_output(i64 4000000000, i8* null)
  call void @__quantum__rt__result_record_output(%Result* null, i8* null)
  ret void
}

declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare void @__quantum__rt__array_record_output(i64, i8*)
declare void @__quantum__rt__result_record_output(%Result*, i8*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"base_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let program = compile(source, 1);
    assert_eq!(keys(&run(&program, 10, 1).returns), ["[0]"]);
}

#[test]
fn ising_amplitudes() {
    let theta: f64 = 0.8;
    let (c, s) = ((theta / 2.0).cos(), (theta / 2.0).sin());
    let zero = Complex64::new(0.0, 0.0);
    let even = Complex64::from_polar(0.5, -theta / 2.0);
    let odd = Complex64::from_polar(0.5, theta / 2.0);

    let rzz = amplitudes(&format!(
        "{PLUS_PLUS}  call void @__quantum__qis__rzz__body(double {theta}, {PAIR})\n"
    ));
    assert_amplitudes(&rzz, &[even, odd, odd, even]);

    let adjoint = amplitudes(&format!(
        "{PLUS_PLUS}  call void @__quantum__qis__rzz__adj(double {theta}, {PAIR})\n"
    ));
    assert_amplitudes(&adjoint, &[odd, even, even, odd]);

    let rxx = amplitudes(&format!(
        "  call void @__quantum__qis__rxx__body(double {theta}, {PAIR})\n"
    ));
    assert_amplitudes(&rxx, &[c.into(), zero, zero, Complex64::new(0.0, -s)]);

    let ryy = amplitudes(&format!(
        "  call void @__quantum__qis__ryy__body(double {theta}, {PAIR})\n"
    ));
    assert_amplitudes(&ryy, &[c.into(), zero, zero, Complex64::new(0.0, s)]);
}

#[test]
fn unknown_gate() {
    let source = module(
        1,
        0,
        "  call void @__quantum__qis__frobnicate__body(%Qubit* null)\n",
    );
    let errors = messages(&source, Severity::Error);
    assert!(
        errors.iter().any(|m| m.contains("frobnicate")),
        "{errors:?}"
    );
}

const R0: &str = "%Result* null";
const R1: &str = "%Result* inttoptr (i64 1 to %Result*)";
const Q1: &str = "%Qubit* inttoptr (i64 1 to %Qubit*)";

fn returns(qubits: u32, results: u32, body: &str) -> BTreeMap<String, u64> {
    run(&compile(&module(qubits, results, body), 1), 400, 6).returns
}

#[test]
fn record_then_remeasure() {
    let body = format!(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, {R0})
  call void @__quantum__rt__result_record_output({R0}, i8* null)
  call void @__quantum__qis__mz__body({Q1}, {R0})
  call void @__quantum__rt__result_record_output({R0}, i8* null)
"
    );
    assert_eq!(keys(&returns(2, 1, &body)), ["1, 0"]);
}

#[test]
fn tuple_start_end() {
    let body = format!(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, {R0})
  call void @__quantum__qis__mz__body({Q1}, {R1})
  call void @__quantum__rt__tuple_start_record_output(i8* null)
  call void @__quantum__rt__result_record_output({R0}, i8* null)
  call void @__quantum__rt__result_record_output({R1}, i8* null)
  call void @__quantum__rt__tuple_end_record_output(i8* null)
  call void @__quantum__rt__result_record_output({R1}, i8* null)
"
    );
    let source = module(2, 2, &body);
    assert_eq!(keys(&returns(2, 2, &body)), ["(1, 0), 0"]);

    let qir = codegen::emit_qir(&compile(&source, 1)).unwrap();
    assert!(qir.contains("call void @__quantum__rt__tuple_start_record_output()"));
    assert!(qir.contains("call void @__quantum__rt__tuple_end_record_output()"));
    assert!(qir.contains("result_record_output(%Result* inttoptr (i64 1 to %Result*))"));
}

#[test]
fn runtime_count() {
    let body = format!(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, {R0})
  %bit = call i1 @__quantum__rt__read_result({R0})
  %n = select i1 %bit, i64 2, i64 0
  call void @__quantum__rt__array_record_output(i64 %n, i8* null)
  call void @__quantum__rt__result_record_output({R0}, i8* null)
  call void @__quantum__rt__result_record_output({R0}, i8* null)
"
    );
    assert_eq!(keys(&returns(1, 1, &body)), ["[1, 1]"]);
}

#[test]
fn phi_swap() {
    let body = format!(
        "  call void @__quantum__qis__mz__body(%Qubit* null, {R0})
  %zero = call i1 @__quantum__rt__read_result({R0})
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %j, %loop ]
  %a = phi i64 [ 10, %entry ], [ %b, %loop ]
  %b = phi i64 [ 20, %entry ], [ %a, %loop ]
  call void @__quantum__rt__int_record_output(i64 %a, i8* null)
  %j = add i64 %i, 1
  %more = icmp slt i64 %j, 4
  %again = or i1 %more, %zero
  br i1 %again, label %loop, label %exit
exit:
"
    );
    assert_eq!(keys(&returns(1, 1, &body)), ["10, 20, 10, 20"]);
}

#[test]
fn silent_shots() {
    let body = format!(
        "  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, {R0})
  %bit = call i1 @__quantum__rt__read_result({R0})
  br i1 %bit, label %yes, label %exit
yes:
  call void @__quantum__rt__int_record_output(i64 7, i8* null)
  br label %exit
exit:
"
    );
    let tally = returns(1, 1, &body);
    assert_eq!(keys(&tally), ["(nothing)", "7"]);
    assert_eq!(tally.values().sum::<u64>(), 400);
}
