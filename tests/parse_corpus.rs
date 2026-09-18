use qirc::ast::*;
use qirc::diag::SourceFile;
use qirc::parse::parse_module;

const BELL: &str = include_str!("corpus/base_profile_bell.ll");
const TELEPORT: &str = include_str!("corpus/adaptive_teleport.ll");
const PYQIR: &str = include_str!("corpus/pyqir_simple.ll");
const DYNAMIC: &str = include_str!("corpus/unrestricted_dynamic.ll");
const STRESS: &str = include_str!("corpus/syntax_stress.ll");

const CORPUS: &[(&str, &str)] = &[
    ("base_profile_bell", BELL),
    ("adaptive_teleport", TELEPORT),
    ("pyqir_simple", PYQIR),
    ("unrestricted_dynamic", DYNAMIC),
    ("syntax_stress", STRESS),
];

fn parse_clean(name: &str, src: &str) -> Module {
    let (module, diagnostics) = parse_module(src);
    if !diagnostics.is_empty() {
        let file = SourceFile::new(name, src);
        panic!(
            "{name} produced {} diagnostics:\n{}",
            diagnostics.len(),
            diagnostics
                .iter()
                .map(|d| d.render(&file))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    module
}

#[test]
fn corpus_clean() {
    for (name, src) in CORPUS {
        parse_clean(name, src);
    }
}

#[test]
fn corpus_supported() {
    for (name, src) in CORPUS {
        let module = parse_clean(name, src);
        for function in &module.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    if let InstKind::Unsupported { opcode } = &inst.kind {
                        panic!(
                            "{name}: {} has unsupported opcode `{opcode}`",
                            function.sig.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn bell() {
    let module = parse_clean("bell", BELL);

    assert_eq!(module.source_filename.as_deref(), Some("BellPair"));
    assert_eq!(module.type_defs.len(), 2);
    assert!(module.type_defs.iter().any(|t| t.name == "Qubit"));
    assert_eq!(module.declarations.len(), 5);
    assert_eq!(module.functions.len(), 1);

    let main = module.entry_point().expect("an entry point");
    assert_eq!(main.sig.name, "main");
    assert_eq!(main.blocks.len(), 1);
    assert_eq!(main.blocks[0].label, "block_0");
    assert_eq!(main.blocks[0].instructions.len(), 7);
    assert_eq!(main.blocks[0].terminator, Terminator::Ret(None));

    let attrs = module.attributes_of(&main.sig);
    assert!(attrs.iter().any(|a| a.key() == "entry_point"));
    let qubits = attrs
        .iter()
        .find(|a| a.key() == "required_num_qubits")
        .and_then(|a| a.value());
    assert_eq!(qubits, Some("2"));
    let profile = attrs
        .iter()
        .find(|a| a.key() == "qir_profiles")
        .and_then(|a| a.value());
    assert_eq!(profile, Some("base_profile"));
}

#[test]
fn inttoptr_qubit() {
    let module = parse_clean("bell", BELL);
    let main = module.entry_point().unwrap();

    let InstKind::Call(call) = &main.blocks[0].instructions[0].kind else {
        panic!("first instruction should be a call");
    };
    assert_eq!(call.callee_name(), Some("__quantum__qis__h__body"));
    assert_eq!(call.args.len(), 1);

    let Value::ConstExpr(expr) = &call.args[0].value else {
        panic!(
            "expected a constant expression, got {:?}",
            call.args[0].value
        );
    };
    let ConstExpr::Cast { op, operand, to } = expr.as_ref() else {
        panic!("expected a cast");
    };
    assert_eq!(*op, CastOp::IntToPtr);
    assert_eq!(operand.value, Value::Int(0));
    assert_eq!(to.pointee_name(), Some("Qubit"));
}

#[test]
fn module_flags() {
    let module = parse_clean("bell", BELL);

    let flags = module
        .named_metadata_node("llvm.module.flags")
        .expect("llvm.module.flags");
    assert_eq!(flags.operands, vec!["0", "1", "2", "3"]);

    let major = module.metadata_def("0").expect("!0");
    let MetadataNode::Tuple(items) = &major.node else {
        panic!("expected a tuple");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[1], MetadataItem::Str("qir_major_version".into()));
}

#[test]
fn teleport() {
    let module = parse_clean("teleport", TELEPORT);
    let main = module.entry_point().unwrap();

    let labels: Vec<&str> = main.blocks.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["entry", "then_x", "join_x", "then_z", "join_z"]
    );

    let Terminator::CondBr {
        if_true, if_false, ..
    } = &main.blocks[0].terminator
    else {
        panic!("entry should end in a conditional branch");
    };
    assert_eq!(if_true, "then_x");
    assert_eq!(if_false, "join_x");

    assert_eq!(
        main.blocks[1].terminator,
        Terminator::Br {
            target: "join_x".into()
        }
    );

    let read = main.blocks[0]
        .instructions
        .iter()
        .find(|instruction| {
            matches!(
                &instruction.kind,
                InstKind::Call(call)
                    if call.callee_name() == Some("__quantum__qis__read_result__body")
            )
        })
        .expect("a read_result call");
    assert_eq!(read.result.as_deref(), Some("0"));
}

#[test]
fn rotation_angle() {
    let module = parse_clean("teleport", TELEPORT);
    let main = module.entry_point().unwrap();

    let InstKind::Call(call) = &main.blocks[0].instructions[1].kind else {
        panic!("expected the ry call");
    };
    assert_eq!(call.callee_name(), Some("__quantum__qis__ry__body"));
    assert_eq!(call.args[0].ty, Ty::Double);
    let Value::Float(theta) = call.args[0].value else {
        panic!("expected a float angle");
    };
    assert!((theta - std::f64::consts::FRAC_PI_4).abs() < 1e-15);
}

#[test]
fn pyqir() {
    let module = parse_clean("pyqir", PYQIR);
    let main = module.entry_point().unwrap();

    let InstKind::Call(first) = &main.blocks[0].instructions[0].kind else {
        panic!("expected a call");
    };
    assert_eq!(first.args[0].value, Value::Null);

    let rz = main.blocks[0]
        .instructions
        .iter()
        .find_map(|i| match &i.kind {
            InstKind::Call(c) if c.callee_name() == Some("__quantum__qis__rz__body") => Some(c),
            _ => None,
        })
        .expect("an rz call");
    let Value::Float(theta) = rz.args[0].value else {
        panic!("expected a float");
    };
    assert!((theta - std::f64::consts::PI).abs() < 1e-15);

    let ccx = main.blocks[0]
        .instructions
        .iter()
        .find_map(|i| match &i.kind {
            InstKind::Call(c) if c.callee_name() == Some("__quantum__qis__ccx__body") => Some(c),
            _ => None,
        })
        .expect("a ccx call");
    assert_eq!(ccx.args.len(), 3);
}

#[test]
fn param_attrs() {
    let module = parse_clean("bell", BELL);
    let mz = module
        .declarations
        .iter()
        .find(|d| d.name == "__quantum__qis__mz__body")
        .expect("mz declaration");
    assert_eq!(mz.params.len(), 2);
    assert_eq!(mz.params[1].attrs, vec!["writeonly".to_string()]);
    assert_eq!(mz.params[1].ty.pointee_name(), Some("Result"));
}

#[test]
fn dynamic() {
    let module = parse_clean("dynamic", DYNAMIC);

    assert_eq!(module.functions.len(), 2);
    let helper = module.function("Program__Rotate__body").expect("helper");
    assert_eq!(helper.sig.params.len(), 2);
    assert_eq!(helper.sig.params[0].name.as_deref(), Some("q"));
    assert_eq!(helper.sig.params[1].ty, Ty::Double);

    let main = module.function("Program__Main__body").expect("main");
    let header = main.block("header").expect("the loop header");
    let phi = header
        .instructions
        .iter()
        .find_map(|i| match &i.kind {
            InstKind::Phi { incoming, .. } => Some(incoming),
            _ => None,
        })
        .expect("a phi node");
    assert_eq!(phi.len(), 2);
    assert_eq!(phi[0], (Value::Int(0), "entry".to_string()));
    assert_eq!(phi[1], (Value::Local("next".into()), "body".to_string()));

    assert!(matches!(main.sig.ret_ty, Ty::Ptr(_)));
}

#[test]
fn stress() {
    let module = parse_clean("stress", STRESS);
    let stress = module.function("stress").expect("the stress function");

    let entry = stress.block("entry").unwrap();
    let Terminator::Switch { cases, default, .. } = &entry.terminator else {
        panic!("entry should end in a switch, got {:?}", entry.terminator);
    };
    assert_eq!(cases.len(), 2);
    assert_eq!(default, "default");
    assert_eq!(cases[0].1, "case0");
    assert_eq!(cases[1].1, "case1");

    let merge = stress.block("merge").unwrap();
    let phi = merge
        .instructions
        .iter()
        .find_map(|i| match &i.kind {
            InstKind::Phi { incoming, .. } => Some(incoming.len()),
            _ => None,
        })
        .expect("a three way phi");
    assert_eq!(phi, 3);

    let varargs_call = merge
        .instructions
        .iter()
        .find_map(|i| match &i.kind {
            InstKind::Call(c) => Some(c),
            _ => None,
        })
        .expect("the varargs call");
    assert_eq!(varargs_call.callee_name(), Some("quoted fn name"));
    assert!(varargs_call.explicit_fn_ty.is_some());
    assert_eq!(varargs_call.args.len(), 3);
}

#[test]
fn unnamed_block() {
    let module = parse_clean("stress", STRESS);
    let noop = module.function("noop").expect("noop");
    assert_eq!(noop.blocks.len(), 1);
    assert_eq!(noop.blocks[0].label, "0");
    assert_eq!(noop.blocks[0].terminator, Terminator::Ret(None));
}

#[test]
fn packed_and_vector() {
    let module = parse_clean("stress", STRESS);

    let quoted = module
        .type_defs
        .iter()
        .find(|t| t.name == "quoted type")
        .expect("the quoted type name");
    let Ty::Struct { fields, packed } = &quoted.ty else {
        panic!("expected a struct");
    };
    assert!(!packed);
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[1], Ty::Array(4, Box::new(Ty::Double)));
    assert_eq!(
        fields[2],
        Ty::Vector {
            len: 2,
            scalable: false,
            elem: Box::new(Ty::Int(64))
        }
    );

    let packed_ty = module
        .type_defs
        .iter()
        .find(|t| t.name == "Packed")
        .expect("Packed");
    let Ty::Struct { packed, .. } = &packed_ty.ty else {
        panic!("expected a struct");
    };
    assert!(packed);
}

#[test]
fn globals() {
    let module = parse_clean("stress", STRESS);

    let s = module.global("g.str").expect("g.str");
    assert!(s.is_constant);
    assert_eq!(s.ty, Ty::Array(4, Box::new(Ty::Int(8))));
    assert_eq!(
        s.initializer,
        Some(Value::Bytes(vec![b'a', b'b', 0x0A, 0x00]))
    );

    let zero = module.global("g.zero").expect("g.zero");
    assert_eq!(zero.initializer, Some(Value::ZeroInit));

    let arr = module.global("g.arr").expect("g.arr");
    let Some(Value::Aggregate(items)) = &arr.initializer else {
        panic!("expected an array initializer");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[2].value, Value::Int(3));
}

#[test]
fn error_spans() {
    let broken = "define void @main() {\nentry:\n  call void @f(%Nope)\n  ret void\n}\n";
    let (_, diagnostics) = parse_module(broken);
    assert!(!diagnostics.is_empty());

    let file = SourceFile::new("broken.ll", broken);
    for d in &diagnostics {
        let span = d.primary_span().expect("a primary span");
        assert!(span.end as usize <= broken.len());
        let rendered = d.render(&file);
        assert!(rendered.contains("broken.ll:"));
    }
}
