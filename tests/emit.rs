mod common;

use common::{compile, errors, run};
use qirc::codegen;
use qirc::driver;
use qirc::ir::*;
use qirc::json::Json;
use qirc::transpile::{self, GateSet};

fn module(body: &str) -> String {
    format!(
        "%Qubit = type opaque
%Result = type opaque

define void @main() #0 {{
entry:
{body}  ret void
}}

declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__sx__adj(%Qubit*)
declare void @__quantum__qis__ccz__body(%Qubit*, %Qubit*, %Qubit*)
declare void @__quantum__qis__s__ctl(%Qubit*, %Qubit*)
declare void @__quantum__qis__ccx__ctl(%Qubit*, %Qubit*, %Qubit*, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__rt__read_result(%Result*)
declare void @__quantum__rt__int_record_output(i64, i8*)
declare void @__quantum__rt__bool_record_output(i1, i8*)
declare void @__quantum__rt__double_record_output(double, i8*)
declare void @__quantum__rt__result_record_output(%Result*, i8*)
declare void @__quantum__qis__rx__body(double, %Qubit*)
declare void @__quantum__qis__ry__body(double, %Qubit*)
declare void @__quantum__qis__rzz__body(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__cswap__body(%Qubit*, %Qubit*, %Qubit*)

attributes #0 = {{ \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"4\" \"required_num_results\"=\"1\" }}
"
    )
}

fn returns(source: &str, level: u8) -> Vec<String> {
    run(&compile(source, level), 50, 2)
        .returns
        .into_keys()
        .collect()
}

fn qir(source: &str) -> String {
    codegen::emit_qir(&compile(source, 0)).unwrap()
}

const MEASURED_ONE: &str = "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %r = call i1 @__quantum__rt__read_result(%Result* null)
";

#[test]
fn widths() {
    let source = module(&format!(
        "{MEASURED_ONE}  %s = sext i1 %r to i64
  %big = select i1 %r, i64 4294967301, i64 0
  %t = trunc i64 %big to i32
  %w = sext i32 %t to i64
  %u = add i32 %t, 2147483647
  %z = zext i32 %u to i64
  %b = trunc i64 %big to i1
  call void @__quantum__rt__int_record_output(i64 %s, i8* null)
  call void @__quantum__rt__int_record_output(i64 %w, i8* null)
  call void @__quantum__rt__int_record_output(i64 %z, i8* null)
  call void @__quantum__rt__bool_record_output(i1 %b, i8* null)
"
    ));
    let expected = ["-1, 5, 2147483652, true"];
    assert_eq!(returns(&source, 0), expected);
    assert_eq!(returns(&source, 1), expected);
    assert_eq!(returns(&qir(&source), 0), expected);
}

#[test]
fn bitcast() {
    let source = module(&format!(
        "{MEASURED_ONE}  %d = select i1 %r, double 1.0, double 2.0
  %bits = bitcast double %d to i64
  %e = lshr i64 %bits, 52
  call void @__quantum__rt__int_record_output(i64 %e, i8* null)
"
    ));
    assert_eq!(returns(&source, 1), ["1023"]);
    assert_eq!(returns(&qir(&source), 0), ["1023"]);
}

#[test]
fn phi_types() {
    let source = module(
        "  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %r = call i1 @__quantum__rt__read_result(%Result* null)
  br label %loop
loop:
  %t = phi double [ %t, %loop ], [ 1.5, %entry ]
  %f = phi i1 [ %g, %loop ], [ false, %entry ]
  %g = xor i1 %f, true
  %go = and i1 %r, %g
  br i1 %go, label %loop, label %exit
exit:
  call void @__quantum__rt__double_record_output(double %t, i8* null)
  call void @__quantum__rt__bool_record_output(i1 %g, i8* null)
",
    );
    let emitted = qir(&source);
    assert!(emitted.contains("= phi double [ %v"), "{emitted}");
    assert!(emitted.contains("= phi i1 [ %v"), "{emitted}");
    assert_eq!(returns(&emitted, 0), ["1.5, true"]);
}

#[test]
fn flatten_swap() {
    let source = module(
        "  br label %loop
loop:
  %a = phi double [ %b, %loop ], [ 1.5, %entry ]
  %b = phi double [ %a, %loop ], [ 2.25, %entry ]
  %i = phi i64 [ %n, %loop ], [ 0, %entry ]
  %n = add i64 %i, 1
  %more = icmp slt i64 %n, 3
  br i1 %more, label %loop, label %done
done:
  call void @__quantum__rt__double_record_output(double %a, i8* null)
  call void @__quantum__rt__double_record_output(double %b, i8* null)
",
    );
    assert_eq!(returns(&source, 0), ["1.5, 2.25"]);
    assert_eq!(returns(&source, 1), ["1.5, 2.25"]);
}

#[test]
fn qasm_gate_definitions() {
    let source = module(
        "  call void @__quantum__qis__sx__adj(%Qubit* null)
  call void @__quantum__qis__ccz__body(%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*))
",
    );
    let qasm = codegen::emit_qasm3(&compile(&source, 0)).unwrap();
    assert!(qasm.contains("gate sxdg a { inv @ sx a; }"), "{qasm}");
    assert!(qasm.contains("gate ccz a, b, c {"), "{qasm}");
}

#[test]
fn qasm_read_order() {
    let source = module(
        "  %r = call i1 @__quantum__rt__read_result(%Result* null)
  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  br i1 %r, label %flip, label %done
flip:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  br label %done
done:
",
    );
    let qasm = codegen::emit_qasm3(&compile(&source, 0)).unwrap();
    let read = qasm.find("= bool(c[0]);").expect(&qasm);
    assert!(read < qasm.find("measure").unwrap(), "{qasm}");
    assert!(!qasm.contains("if (c[0])"), "{qasm}");
}

#[test]
fn qasm_loop_record() {
    let source = module(
        "  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %r = call i1 @__quantum__rt__read_result(%Result* null)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %n, %loop ]
  call void @__quantum__rt__int_record_output(i64 %i, i8* null)
  %n = add i64 %i, 1
  %more = icmp slt i64 %n, 3
  %go = or i1 %more, %r
  br i1 %go, label %loop, label %exit
exit:
",
    );
    let error = codegen::emit_qasm3(&compile(&source, 0)).unwrap_err();
    assert!(error.contains("inside a loop"), "{error}");
}

#[test]
fn qasm_early_exits() {
    let mut body = String::from(
        "  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %r = call i1 @__quantum__rt__read_result(%Result* null)
  br label %b0
",
    );
    for i in 0..14 {
        body.push_str(&format!(
            "b{i}:
  br i1 %r, label %l{i}, label %x{i}
l{i}:
  call void @__quantum__qis__x__body(%Qubit* null)
  br label %j{i}
x{i}:
  br i1 %r, label %done, label %j{i}
j{i}:
  br label %b{}
",
            i + 1
        ));
    }
    body.push_str("b14:\n  br label %done\ndone:\n");
    let qasm = codegen::emit_qasm3(&compile(&module(&body), 0)).unwrap();
    assert!(qasm.len() < 20_000, "{} bytes", qasm.len());
}

#[test]
fn qir_controlled_s() {
    let pair = "%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*)";
    let source = module(&format!(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__s__ctl({pair})
"
    ));
    let emitted = qir(&source);
    assert!(!emitted.contains("unitary"), "{emitted}");

    let before = common::final_state(&compile(&source, 0));
    let after = common::final_state(&compile(&emitted, 0));
    let overlap: f64 = (0..before.len())
        .map(|i| before.amplitude(i).conj() * after.amplitude(i))
        .sum::<num_complex::Complex64>()
        .norm();
    assert!((overlap - 1.0).abs() < 1e-9, "overlap {overlap}");

    let wide = module(
        "  call void @__quantum__qis__ccx__ctl(%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*), %Qubit* inttoptr (i64 3 to %Qubit*))
",
    );
    let error = codegen::emit_qir(&compile(&wide, 0)).unwrap_err();
    assert!(error.contains("cccx"), "{error}");
}

#[test]
fn ccz_basis() {
    let source = module(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__ccz__body(%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__h__body(%Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 2 to %Qubit*), %Result* null)
",
    );
    let mut program = compile(&source, 0);
    transpile::transpile(&mut program, &GateSet::parse("rz-sx-cx").unwrap());
    let counts = run(&program, 100, 3).counts;
    assert_eq!(counts.into_keys().collect::<Vec<_>>(), ["1"]);
}

#[test]
fn quoted_names() {
    let source = "%Qubit = type opaque
%Result = type opaque

define void @\"Main Op\"() #0 {
\"my entry\":
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %r = call i1 @__quantum__rt__read_result(%Result* null)
  br i1 %r, label %v0, label %\"then x\"
v0:
  call void @__quantum__qis__x__body(%Qubit* null)
  br label %\"then x\"
\"then x\":
  ret void
}

declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__rt__read_result(%Result*)

attributes #0 = { \"entry_point\" \"qir_profiles\"=\"adaptive_profile\" \"required_num_qubits\"=\"1\" \"required_num_results\"=\"1\" }
";
    let program = compile(source, 0);
    assert_eq!(program.name, "Main Op");
    let emitted = codegen::emit_qir(&program).unwrap();
    assert!(emitted.contains("define void @\"Main Op\"()"), "{emitted}");
    assert!(emitted.contains("\"then x\":"), "{emitted}");
    assert!(!emitted.contains("\nv0:"), "{emitted}");
    compile(&emitted, 0);
}

#[test]
fn label_offset() {
    let source = module(
        "  call void @__quantum__rt__int_record_output(i64 6, i8* getelementptr inbounds ([7 x i8], [7 x i8]* @prefix, i64 0, i64 3))
",
    )
    .replace(
        "define void @main",
        "@prefix = internal constant [7 x i8] c\"prefix\\00\"\n\ndefine void @main",
    );
    let labels: Vec<_> = compile(&source, 0)
        .ops()
        .filter_map(|op| match op {
            Op::RecordOutput { label, .. } => label.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["fix"]);
}

#[test]
fn array_controls() {
    let source = |set: &str| {
        format!(
            "%Result = type opaque
%Qubit = type opaque
%Array = type opaque

define void @main() #0 {{
entry:
  %ctls = call %Array* @__quantum__rt__qubit_allocate_array(i64 2)
  %t = call %Qubit* @__quantum__rt__qubit_allocate()
  %p = call i8* @__quantum__rt__array_get_element_ptr_1d(%Array* %ctls, i64 0)
  %qp = bitcast i8* %p to %Qubit**
  %q = load %Qubit*, %Qubit** %qp
  call void @__quantum__qis__x__body(%Qubit* %q)
{set}  call void @__quantum__qis__x__ctl(%Array* %ctls, %Qubit* %t)
  %r = call %Result* @__quantum__qis__m__body(%Qubit* %t)
  call void @__quantum__rt__result_record_output(%Result* %r, i8* null)
  ret void
}}

declare %Array* @__quantum__rt__qubit_allocate_array(i64)
declare %Qubit* @__quantum__rt__qubit_allocate()
declare i8* @__quantum__rt__array_get_element_ptr_1d(%Array*, i64)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__x__ctl(%Array*, %Qubit*)
declare %Result* @__quantum__qis__m__body(%Qubit*)
declare void @__quantum__rt__result_record_output(%Result*, i8*)

attributes #0 = {{ \"entry_point\" }}
"
        )
    };
    let second = "  %p1 = call i8* @__quantum__rt__array_get_element_ptr_1d(%Array* %ctls, i64 1)
  %qp1 = bitcast i8* %p1 to %Qubit**
  %q1 = load %Qubit*, %Qubit** %qp1
  call void @__quantum__qis__x__body(%Qubit* %q1)
";
    assert_eq!(returns(&source(""), 0), ["0"]);
    assert_eq!(returns(&source(second), 0), ["1"]);
}

#[test]
fn ising_repeat() {
    let source = module(
        "  call void @__quantum__qis__rzz__body(double 0.5, %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 1 to %Qubit*))
",
    );
    let compilation = driver::compile(&source, 0);
    assert_eq!(errors(&compilation), ["`rzz` uses q1 more than once"]);
}

#[test]
fn runtime_basis() {
    let source = module(
        "  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %c = call i1 @__quantum__rt__read_result(%Result* null)
  %a = select i1 %c, double 3.141592653589793, double 3.141592653589793
  call void @__quantum__qis__rx__body(double %a, %Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__ry__body(double %a, %Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 2 to %Qubit*), %Result* inttoptr (i64 2 to %Result*))
  call void @__quantum__rt__result_record_output(%Result* inttoptr (i64 1 to %Result*), i8* null)
  call void @__quantum__rt__result_record_output(%Result* inttoptr (i64 2 to %Result*), i8* null)
",
    );
    for basis in ["rz-sx-cx", "rz-ry-cz"] {
        let mut program = compile(&source, 1);
        let stats = transpile::transpile(&mut program, &GateSet::parse(basis).unwrap());
        assert!(stats.leftover.is_empty());
        let keys: Vec<_> = run(&program, 50, 2).returns.into_keys().collect();
        assert_eq!(keys, ["1, 1"], "{basis}");
    }
}

#[test]
fn cswap_basis() {
    let source = module(
        "  call void @__quantum__qis__x__body(%Qubit* null)
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__cswap__body(%Qubit* null, %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Result* null)
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 2 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
",
    );
    let mut program = compile(&source, 0);
    let stats = transpile::transpile(&mut program, &GateSet::parse("rz-sx-cx").unwrap());
    assert!(stats.leftover.is_empty());
    let counts = run(&program, 50, 2).counts;
    assert_eq!(counts.into_keys().collect::<Vec<_>>(), ["01"]);
}

#[test]
fn unicode_source() {
    let source = module("  ret void \u{e9}\n");
    let found = errors(&driver::compile(&source, 0))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        found.iter().any(|e| e.contains("unexpected character")),
        "{found:?}"
    );
}

#[test]
fn qasm_widths() {
    let source = module(&format!(
        "{MEASURED_ONE}  %z = zext i1 %r to i64
  %a = add i64 %z, 4294967295
  %t = trunc i64 %a to i32
  %w = sext i32 %t to i64
  call void @__quantum__rt__int_record_output(i64 %w, i8* null)
"
    ));
    assert_eq!(returns(&source, 0), ["0"]);
    let qasm = codegen::emit_qasm3(&compile(&source, 0)).unwrap();
    assert!(qasm.contains("int[64](int[32](v2))"), "{qasm}");
    assert!(!qasm.contains("bool v3"), "{qasm}");
}

#[test]
fn qasm_unordered() {
    let source = module(&format!(
        "{MEASURED_ONE}  %d = select i1 %r, double 1.0, double 0.0
  %c = fcmp ueq double %d, 0.0
  %o = fcmp uno double %d, %d
  call void @__quantum__rt__bool_record_output(i1 %c, i8* null)
  call void @__quantum__rt__bool_record_output(i1 %o, i8* null)
"
    ));
    let qasm = codegen::emit_qasm3(&compile(&source, 0)).unwrap();
    assert!(qasm.contains("= !(v1 < 0.0 || v1 > 0.0);"), "{qasm}");
    assert!(qasm.contains("= (v1 != v1 || v1 != v1);"), "{qasm}");
}

#[test]
fn single_precision() {
    let source = module(&format!(
        "{MEASURED_ONE}  %a = select i1 %r, float 0.1, float 0.2
  %b = fadd float %a, 0.2
  %c = fpext float %b to double
  %d = fptrunc double 0.1 to float
  %e = fpext float %d to double
  call void @__quantum__rt__double_record_output(double %c, i8* null)
  call void @__quantum__rt__double_record_output(double %e, i8* null)
"
    ));
    let sum = f64::from(0.1f32 + 0.2f32);
    let expected = format!("{sum:?}, {:?}", f64::from(0.1f32));
    assert_eq!(returns(&source, 0), [expected.as_str()]);
    assert_eq!(returns(&source, 3), [expected.as_str()]);
    assert_eq!(returns(&qir(&source), 0), [expected.as_str()]);
}

#[test]
fn deep_nesting() {
    let mut value = String::from("i64 0");
    for _ in 0..5000 {
        value = format!("i64 add (i64 1, {value})");
    }
    let source = module(&format!("  %x = add {value}, 1\n"));
    let found = errors(&driver::compile(&source, 0))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        found.iter().any(|e| e.contains("nested too deeply")),
        "{found:?}"
    );
}

#[test]
fn escaped_names() {
    let source = r#"%Qubit = type opaque
define void @"m\22a\5Cin\0A\07"() #0 {
entry:
  call void @__quantum__qis__rx__body(double 0x7FF8000000000000, %Qubit* null)
  ret void
}
declare void @__quantum__qis__rx__body(double, %Qubit*)
attributes #0 = { "entry_point" "qir_profiles"="adaptive_profile" "required_num_qubits"="1" }
"#;
    let program = compile(source, 0);
    let json = codegen::emit_json(&program);
    assert!(json.contains(r#""name": "m\"a\\in\u000a\u0007""#), "{json}");
    assert!(json.contains(r#""params": [null]"#), "{json}");
    let qir = codegen::emit_qir(&program).unwrap();
    assert!(qir.lines().next().unwrap().ends_with(r"\0A\07'"), "{qir}");
}

#[test]
fn diagrams() {
    let bell = compile(include_str!("corpus/base_profile_bell.ll"), 1);
    assert_eq!(
        qirc::draw::quantikz(&bell),
        r"\begin{quantikz}
\lstick{$q_{0}$} & \gate{H} & \ctrl{1} & \meter{} & \qw \\
\lstick{$q_{1}$} & \qw & \targ{} & \meter{} & \qw
\end{quantikz}
"
    );
    let teleport = compile(include_str!("corpus/adaptive_teleport.ll"), 2);
    let latex = qirc::draw::quantikz(&teleport);
    assert!(latex.contains(r"\gate{R_y(\frac{\pi}{4})}"), "{latex}");
    assert!(latex.contains(r"\slice{then\_x}"), "{latex}");
    let svg = qirc::draw::svg(&bell);
    assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
    assert_eq!(svg.matches("class=\"wire\"").count(), 2);
    assert_eq!(svg.matches("class=\"plus\"").count(), 1);
    assert_eq!(svg.matches("M r").count(), 2);
}

#[test]
fn fused_matrices() {
    let program = compile("OPENQASM 3.0;\nqubit q;\nh q;\nt q;\nh q;\n", 3);
    let json = Json::parse(&codegen::emit_json(&program)).unwrap();
    let Some(Json::List(blocks)) = json.get("blocks") else {
        panic!("{json:?}");
    };
    let Some(Json::List(ops)) = blocks[0].get("ops") else {
        panic!("{:?}", blocks[0]);
    };
    let Some(Json::List(matrix)) = ops[0].get("matrix") else {
        panic!("{:?}", ops[0]);
    };
    let entries: Vec<f64> = matrix
        .iter()
        .map(|entry| match entry {
            Json::Number(value) => *value,
            other => panic!("{other:?}"),
        })
        .collect();
    let row = entries[..4].iter().map(|x| x * x).sum::<f64>();
    assert!((row - 1.0).abs() < 1e-9, "{entries:?}");
}
