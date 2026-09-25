use crate::ir::{GateKind, OutputKind};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Functor {
    Body,
    Adjoint,
    Controlled,
    ControlledAdjoint,
}

impl Functor {
    pub fn is_adjoint(self) -> bool {
        matches!(self, Functor::Adjoint | Functor::ControlledAdjoint)
    }

    pub fn is_controlled(self) -> bool {
        matches!(self, Functor::Controlled | Functor::ControlledAdjoint)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Intrinsic {
    Gate {
        kind: GateKind,
        controls: usize,
        targets: usize,
        params: usize,
    },
    Ising(GateKind),
    Measure {
        reset: bool,
    },
    Reset,
    ReadResult,
    RecordOutput(OutputKind),
    QubitAllocate,
    QubitAllocateArray,
    QubitRelease,
    QubitReleaseArray,
    ArrayGetElementPtr,
    ResultGetZero,
    ResultGetOne,
    ResultEqual,
    Initialize,
    Message,
    Ignored,
}

pub struct Resolved {
    pub intrinsic: Intrinsic,
    pub functor: Functor,
}

fn split_mangled(name: &str) -> Option<(&str, &str, Functor)> {
    let rest = name.strip_prefix("__quantum__")?;
    let (namespace, tail) = rest.split_once("__")?;

    let (base, functor) = [
        ("__ctladj", Functor::ControlledAdjoint),
        ("__ctl", Functor::Controlled),
        ("__adj", Functor::Adjoint),
        ("__body", Functor::Body),
    ]
    .into_iter()
    .find_map(|(s, f)| Some((tail.strip_suffix(s)?, f)))
    .unwrap_or((tail, Functor::Body));

    Some((namespace, base, functor))
}

pub fn resolve(name: &str) -> Option<Resolved> {
    let (namespace, base, functor) = split_mangled(name)?;

    let intrinsic = match namespace {
        "qis" => resolve_qis(base)?,
        "rt" => resolve_rt(base)?,
        "qir" => Intrinsic::Ignored,
        _ => return None,
    };

    Some(Resolved { intrinsic, functor })
}

fn gate(kind: GateKind, controls: usize, targets: usize, params: usize) -> Intrinsic {
    Intrinsic::Gate {
        kind,
        controls,
        targets,
        params,
    }
}

fn resolve_qis(base: &str) -> Option<Intrinsic> {
    Some(match base {
        "i" | "id" => gate(GateKind::I, 0, 1, 0),
        "x" => gate(GateKind::X, 0, 1, 0),
        "y" => gate(GateKind::Y, 0, 1, 0),
        "z" => gate(GateKind::Z, 0, 1, 0),
        "h" => gate(GateKind::H, 0, 1, 0),
        "s" => gate(GateKind::S, 0, 1, 0),
        "t" => gate(GateKind::T, 0, 1, 0),
        "sx" => gate(GateKind::SX, 0, 1, 0),

        "cnot" | "cx" => gate(GateKind::X, 1, 1, 0),
        "cy" => gate(GateKind::Y, 1, 1, 0),
        "cz" => gate(GateKind::Z, 1, 1, 0),
        "ch" => gate(GateKind::H, 1, 1, 0),
        "ccx" | "toffoli" => gate(GateKind::X, 2, 1, 0),
        "ccz" => gate(GateKind::Z, 2, 1, 0),

        "swap" => gate(GateKind::Swap, 0, 2, 0),
        "cswap" | "fredkin" => gate(GateKind::Swap, 1, 2, 0),

        "rx" => gate(GateKind::Rx, 0, 1, 1),
        "ry" => gate(GateKind::Ry, 0, 1, 1),
        "rz" => gate(GateKind::Rz, 0, 1, 1),
        "r1" | "p" | "phase" => gate(GateKind::R1, 0, 1, 1),
        "crx" => gate(GateKind::Rx, 1, 1, 1),
        "cry" => gate(GateKind::Ry, 1, 1, 1),
        "crz" => gate(GateKind::Rz, 1, 1, 1),
        "cr1" | "cp" => gate(GateKind::R1, 1, 1, 1),

        "rxx" => Intrinsic::Ising(GateKind::Rx),
        "ryy" => Intrinsic::Ising(GateKind::Ry),
        "rzz" => Intrinsic::Ising(GateKind::Rz),

        "m" | "mz" | "measure" => Intrinsic::Measure { reset: false },
        "mresetz" => Intrinsic::Measure { reset: true },
        "reset" => Intrinsic::Reset,
        "read_result" => Intrinsic::ReadResult,
        "barrier" => Intrinsic::Ignored,

        _ => return None,
    })
}

fn resolve_rt(base: &str) -> Option<Intrinsic> {
    Some(match base {
        "result_record_output" => Intrinsic::RecordOutput(OutputKind::Result),
        "bool_record_output" => Intrinsic::RecordOutput(OutputKind::Bool),
        "int_record_output" => Intrinsic::RecordOutput(OutputKind::Int),
        "double_record_output" => Intrinsic::RecordOutput(OutputKind::Double),
        "tuple_record_output" | "tuple_start_record_output" => {
            Intrinsic::RecordOutput(OutputKind::Tuple)
        }
        "array_record_output" | "array_start_record_output" => {
            Intrinsic::RecordOutput(OutputKind::Array)
        }
        "tuple_end_record_output" => Intrinsic::RecordOutput(OutputKind::TupleEnd),
        "array_end_record_output" => Intrinsic::RecordOutput(OutputKind::ArrayEnd),

        "qubit_allocate" => Intrinsic::QubitAllocate,
        "qubit_allocate_array" => Intrinsic::QubitAllocateArray,
        "qubit_release" => Intrinsic::QubitRelease,
        "qubit_release_array" => Intrinsic::QubitReleaseArray,
        "array_get_element_ptr_1d" => Intrinsic::ArrayGetElementPtr,

        "read_result" => Intrinsic::ReadResult,
        "result_get_zero" => Intrinsic::ResultGetZero,
        "result_get_one" => Intrinsic::ResultGetOne,
        "result_equal" => Intrinsic::ResultEqual,

        "initialize" => Intrinsic::Initialize,
        "message" => Intrinsic::Message,

        _ if base.starts_with("string_")
            || base.starts_with("array_")
            || base.starts_with("tuple_")
            || base.starts_with("callable_")
            || base.starts_with("bigint_")
            || base.ends_with("_update_reference_count")
            || base.ends_with("_update_alias_count") =>
        {
            Intrinsic::Ignored
        }

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functor_suffixes() {
        let (ns, base, functor) = split_mangled("__quantum__qis__h__body").unwrap();
        assert_eq!(ns, "qis");
        assert_eq!(base, "h");
        assert_eq!(functor, Functor::Body);

        let (_, base, functor) = split_mangled("__quantum__qis__t__adj").unwrap();
        assert_eq!(base, "t");
        assert_eq!(functor, Functor::Adjoint);

        let (_, base, functor) = split_mangled("__quantum__qis__x__ctl").unwrap();
        assert_eq!(base, "x");
        assert_eq!(functor, Functor::Controlled);

        let (_, base, functor) = split_mangled("__quantum__qis__rz__ctladj").unwrap();
        assert_eq!(base, "rz");
        assert_eq!(functor, Functor::ControlledAdjoint);
    }

    #[test]
    fn bare_names() {
        let (_, base, functor) = split_mangled("__quantum__qis__cnot").unwrap();
        assert_eq!(base, "cnot");
        assert_eq!(functor, Functor::Body);
    }

    #[test]
    fn underscored_names() {
        let (_, base, _) = split_mangled("__quantum__qis__read_result__body").unwrap();
        assert_eq!(base, "read_result");

        let (_, base, _) = split_mangled("__quantum__rt__result_record_output").unwrap();
        assert_eq!(base, "result_record_output");

        let (_, base, _) = split_mangled("__quantum__rt__array_get_element_ptr_1d").unwrap();
        assert_eq!(base, "array_get_element_ptr_1d");
    }

    #[test]
    fn controlled_aliases() {
        let cnot = resolve("__quantum__qis__cnot__body").unwrap();
        assert_eq!(cnot.intrinsic, gate(GateKind::X, 1, 1, 0));

        let ccx = resolve("__quantum__qis__ccx__body").unwrap();
        assert_eq!(ccx.intrinsic, gate(GateKind::X, 2, 1, 0));

        let crz = resolve("__quantum__qis__crz__body").unwrap();
        assert_eq!(crz.intrinsic, gate(GateKind::Rz, 1, 1, 1));
    }

    #[test]
    fn measure_variants() {
        let plain = Intrinsic::Measure { reset: false };
        assert_eq!(
            resolve("__quantum__qis__mz__body").unwrap().intrinsic,
            plain
        );
        assert_eq!(resolve("__quantum__qis__m__body").unwrap().intrinsic, plain);
        assert_eq!(
            resolve("__quantum__qis__mresetz__body").unwrap().intrinsic,
            Intrinsic::Measure { reset: true }
        );
    }

    #[test]
    fn qsharp_names() {
        assert_eq!(
            resolve("__quantum__rt__read_result").unwrap().intrinsic,
            Intrinsic::ReadResult
        );
        assert_eq!(
            resolve("__quantum__qis__rzz__body").unwrap().intrinsic,
            Intrinsic::Ising(GateKind::Rz)
        );
    }

    #[test]
    fn runtime_bookkeeping() {
        for name in [
            "__quantum__rt__string_update_reference_count",
            "__quantum__rt__array_update_alias_count",
            "__quantum__rt__tuple_create",
            "__quantum__rt__string_create",
        ] {
            assert_eq!(
                resolve(name).map(|r| r.intrinsic),
                Some(Intrinsic::Ignored),
                "{name} should resolve as ignorable"
            );
        }
    }

    #[test]
    fn unknown_names() {
        assert!(resolve("__quantum__qis__nope__body").is_none());
        assert!(resolve("@printf").is_none());
        assert!(resolve("Program__Rotate__body").is_none());
    }
}
