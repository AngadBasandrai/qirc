use qirc::diag::SourceFile;
use qirc::ir::*;
use qirc::lower::lower;
use qirc::parse::parse_module;

const BELL: &str = include_str!("corpus/base_profile_bell.ll");
const TELEPORT: &str = include_str!("corpus/adaptive_teleport.ll");
const PYQIR: &str = include_str!("corpus/pyqir_simple.ll");
const DYNAMIC: &str = include_str!("corpus/unrestricted_dynamic.ll");

fn compile(name: &str, src: &str) -> Program {
    let (module, parse_errors) = parse_module(src);
    assert!(parse_errors.is_empty(), "{name} failed to parse");

    let lowered = lower(&module);
    let hard_errors: Vec<_> = lowered
        .diagnostics
        .iter()
        .filter(|d| d.severity == qirc::diag::Severity::Error)
        .collect();

    if !hard_errors.is_empty() {
        let file = SourceFile::new(name, src);
        panic!(
            "{name} failed to lower:\n{}",
            hard_errors
                .iter()
                .map(|d| d.render(&file))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    lowered.program
}

#[test]
fn bell_lowers_to_two_gates_and_two_measurements() {
    let program = compile("bell", BELL);

    assert_eq!(program.profile, Profile::Base);
    assert_eq!(program.num_qubits, 2);
    assert_eq!(program.num_results, 2);
    assert_eq!(program.blocks.len(), 1);
    assert_eq!(program.gate_count(), 2);
    assert_eq!(program.measure_count(), 2);

    let gates: Vec<&Gate> = program.gates().collect();

    assert_eq!(gates[0].kind, GateKind::H);
    assert!(gates[0].controls.is_empty());
    assert_eq!(gates[0].targets, vec![QubitId(0)]);

    assert_eq!(gates[1].kind, GateKind::X);
    assert_eq!(gates[1].controls, vec![QubitId(0)]);
    assert_eq!(gates[1].targets, vec![QubitId(1)]);
}

#[test]
fn bell_records_its_outputs() {
    let program = compile("bell", BELL);

    let records: Vec<(&OutputKind, Option<ResultId>)> = program
        .ops()
        .filter_map(|op| match op {
            Op::RecordOutput { kind, result, .. } => Some((kind, *result)),
            _ => None,
        })
        .collect();

    assert_eq!(records.len(), 3);
    assert_eq!(*records[0].0, OutputKind::Tuple);
    assert_eq!(*records[1].0, OutputKind::Result);
    assert_eq!(records[1].1, Some(ResultId(0)));
    assert_eq!(records[2].1, Some(ResultId(1)));
}

#[test]
fn teleport_keeps_its_control_flow() {
    let program = compile("teleport", TELEPORT);

    assert_eq!(program.profile, Profile::Adaptive);
    assert_eq!(program.num_qubits, 3);
    assert_eq!(program.blocks.len(), 5);
    assert!(!program.is_straight_line());

    let entry = program.block(program.entry);
    let Term::CondBr { cond, .. } = &entry.term else {
        panic!(
            "entry should end in a conditional branch, got {:?}",
            entry.term
        );
    };
    assert!(matches!(cond, Operand::Value(_)));

    let reads: Vec<ResultId> = program
        .ops()
        .filter_map(|op| match op {
            Op::Assign {
                expr: Expr::ReadResult(r),
                ..
            } => Some(*r),
            _ => None,
        })
        .collect();
    assert_eq!(reads, vec![ResultId(1), ResultId(0)]);
}

#[test]
fn teleport_carries_its_rotation_angle() {
    let program = compile("teleport", TELEPORT);

    let ry = program
        .gates()
        .find(|g| g.kind == GateKind::Ry)
        .expect("an ry gate");
    let angle = ry.constant_angle().expect("a constant angle");
    assert!((angle - std::f64::consts::FRAC_PI_4).abs() < 1e-15);
}

#[test]
fn pyqir_null_becomes_qubit_zero_and_ccx_gets_two_controls() {
    let program = compile("pyqir", PYQIR);

    assert_eq!(program.num_qubits, 3);

    let first = program.gates().next().unwrap();
    assert_eq!(first.kind, GateKind::H);
    assert_eq!(first.targets, vec![QubitId(0)]);

    let ccx = program
        .gates()
        .find(|g| g.controls.len() == 2)
        .expect("a doubly controlled gate");
    assert_eq!(ccx.kind, GateKind::X);
    assert_eq!(ccx.controls, vec![QubitId(0), QubitId(1)]);
    assert_eq!(ccx.targets, vec![QubitId(2)]);
}

#[test]
fn adjoint_suffix_selects_the_dagger_gate() {
    let program = compile("pyqir", PYQIR);

    assert!(program.gates().any(|g| g.kind == GateKind::S));
    assert!(
        program.gates().any(|g| g.kind == GateKind::TDag),
        "t__adj should lower to tdg"
    );
}

#[test]
fn swap_lowers_to_two_targets() {
    let program = compile("pyqir", PYQIR);

    let swap = program
        .gates()
        .find(|g| g.kind == GateKind::Swap)
        .expect("a swap");
    assert!(swap.controls.is_empty());
    assert_eq!(swap.targets, vec![QubitId(0), QubitId(1)]);
}

#[test]
fn helper_functions_are_inlined() {
    let (module, errors) = parse_module(DYNAMIC);
    assert!(errors.is_empty());
    let lowered = lower(&module);

    let rz_gates: Vec<&Gate> = lowered
        .program
        .gates()
        .filter(|g| g.kind == GateKind::Rz)
        .collect();

    assert!(
        !rz_gates.is_empty(),
        "the Rotate helper should have been inlined into an rz"
    );
}

#[test]
fn loop_dependent_qubit_index_is_rejected_clearly() {
    let (module, errors) = parse_module(DYNAMIC);
    assert!(errors.is_empty());
    let lowered = lower(&module);

    let message = lowered
        .diagnostics
        .iter()
        .find(|d| d.message.contains("compile time constant"))
        .expect("a diagnostic about non constant qubit indexing");

    assert_eq!(message.severity, qirc::diag::Severity::Error);
    assert!(message.primary_span().is_some());
}

#[test]
fn qubit_parameters_are_allocated_in_order() {
    let src = "\
define void @main(%Qubit* %q0, %Qubit* %q1) {
entry:
  call void @__quantum__qis__h(%Qubit* %q0)
  call void @__quantum__qis__cnot(%Qubit* %q0, %Qubit* %q1)
  ret void
}
declare void @__quantum__qis__h(%Qubit*)
declare void @__quantum__qis__cnot(%Qubit*, %Qubit*)
";
    let program = compile("params", src);

    assert_eq!(program.num_qubits, 2);
    let gates: Vec<&Gate> = program.gates().collect();
    assert_eq!(gates[0].targets, vec![QubitId(0)]);
    assert_eq!(gates[1].controls, vec![QubitId(0)]);
    assert_eq!(gates[1].targets, vec![QubitId(1)]);
}

#[test]
fn program_depth_accounts_for_shared_wires() {
    let program = compile("bell", BELL);
    assert_eq!(program.depth(), 2);

    let program = compile("pyqir", PYQIR);
    assert!(program.depth() >= 4);
}

#[test]
fn ir_display_is_readable() {
    let program = compile("bell", BELL);
    let text = format!("{program}");

    assert!(text.contains("program main [base_profile] qubits=2 results=2"));
    assert!(text.contains("h q0"));
    assert!(text.contains("cx q0, q1"));
    assert!(text.contains("measure q0 -> r0"));
}
