use std::collections::HashSet;
use std::fmt::Write;

use crate::codegen::zyz_angles;
use crate::ir::*;
use crate::simulator::matrix::Matrix2;

fn gate_line(gate: &Gate) -> Result<String, String> {
    let params: Vec<f64> = gate
        .params
        .iter()
        .map(|p| p.constant().map(Const::as_f64))
        .collect::<Option<_>>()
        .ok_or("OpenQASM 2 cannot write a rotation whose angle is only known at run time")?;
    let base = match gate.kind {
        GateKind::I => "id",
        GateKind::X => "x",
        GateKind::Y => "y",
        GateKind::Z => "z",
        GateKind::H => "h",
        GateKind::S => "s",
        GateKind::SDag => "sdg",
        GateKind::T => "t",
        GateKind::TDag => "tdg",
        GateKind::SX => "sx",
        GateKind::SXDag => "sxdg",
        GateKind::Rx => "rx",
        GateKind::Ry => "ry",
        GateKind::Rz => "rz",
        GateKind::R1 => "u1",
        GateKind::Swap => "swap",
        GateKind::Unitary(_) => "u3",
    };
    let (name, params) = match (gate.kind, gate.controls.len()) {
        (GateKind::Unitary(m), 0) => {
            let (theta, phi, lambda) = zyz_angles(&Matrix2::from_ir(m));
            ("u3".to_string(), vec![theta, phi, lambda])
        }
        (GateKind::R1, 1) => ("cu1".to_string(), params),
        (
            GateKind::X
            | GateKind::Y
            | GateKind::Z
            | GateKind::H
            | GateKind::SX
            | GateKind::Rx
            | GateKind::Ry
            | GateKind::Rz
            | GateKind::Swap,
            1,
        ) => (format!("c{base}"), params),
        (GateKind::X, 2) => ("ccx".to_string(), params),
        (_, 0) => (base.to_string(), params),
        (kind, controls) => {
            return Err(format!(
                "OpenQASM 2 has no gate for `{}` with {controls} control(s), compile with --gates first",
                kind.name()
            ));
        }
    };
    let angles = if params.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            params
                .iter()
                .map(f64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let wires: Vec<String> = gate.wires().map(|q| format!("q[{}]", q.0)).collect();
    Ok(format!("{name}{angles} {};", wires.join(", ")))
}

fn quantum(op: &Op, registers: &dyn Fn(ResultId) -> String) -> Result<Option<String>, String> {
    Ok(match op {
        Op::Gate(gate) => Some(gate_line(gate)?),
        Op::Measure { qubit, result, .. } => {
            Some(format!("measure q[{}] -> {};", qubit.0, registers(*result)))
        }
        Op::Reset { qubit, .. } => Some(format!("reset q[{}];", qubit.0)),
        Op::RecordOutput { .. } | Op::Message { .. } => None,
        Op::Assign {
            expr: Expr::ReadResult(_),
            ..
        } => None,
        _ => return Err("OpenQASM 2 has no classical variables, use --emit qasm3".into()),
    })
}

pub(crate) enum Step<'a> {
    Op(&'a Op),
    If {
        result: ResultId,
        value: bool,
        gate: &'a Gate,
    },
}

pub(crate) fn steps(program: &Program) -> (Vec<Step<'_>>, bool) {
    let mut steps = Vec::new();
    let mut visited = HashSet::new();
    let mut current = program.entry;
    loop {
        if !visited.insert(current) {
            return (steps, false);
        }
        let block = program.block(current);
        steps.extend(block.ops.iter().map(Step::Op));
        match &block.term {
            Term::Ret(_) | Term::Unreachable => return (steps, true),
            Term::Br(next) => current = *next,
            Term::CondBr {
                cond: Operand::Value(value),
                if_true,
                if_false,
            } => {
                let Some(result) = block.ops.iter().find_map(|op| match op {
                    Op::Assign {
                        dest,
                        expr: Expr::ReadResult(result),
                        ..
                    } if dest == value => Some(*result),
                    _ => None,
                }) else {
                    return (steps, false);
                };
                let guarded = |arm: BlockId, join: BlockId| {
                    let body = program.block(arm);
                    (matches!(body.term, Term::Br(target) if target == join)
                        && body.ops.iter().all(|op| matches!(op, Op::Gate(_))))
                    .then_some(body)
                };
                let (body, value, join) =
                    match (guarded(*if_true, *if_false), guarded(*if_false, *if_true)) {
                        (Some(body), _) => (body, true, *if_false),
                        (None, Some(body)) => (body, false, *if_true),
                        (None, None) => return (steps, false),
                    };
                steps.extend(body.ops.iter().filter_map(|op| match op {
                    Op::Gate(gate) => Some(Step::If {
                        result,
                        value,
                        gate,
                    }),
                    _ => None,
                }));
                visited.insert(body.id);
                current = join;
            }
            _ => return (steps, false),
        }
    }
}

pub fn emit(program: &Program) -> Result<String, String> {
    let branching = program.blocks.len() > 1;
    let registers = |result: ResultId| {
        if branching {
            format!("c{}[0]", result.0)
        } else {
            format!("c[{}]", result.0)
        }
    };
    let mut out = String::from("OPENQASM 2.0;\ninclude \"qelib1.inc\";\n\n");
    writeln!(out, "qreg q[{}];", program.num_qubits.max(1)).unwrap();
    if branching {
        for bit in 0..program.num_results {
            writeln!(out, "creg c{bit}[1];").unwrap();
        }
    } else if program.num_results > 0 {
        writeln!(out, "creg c[{}];", program.num_results).unwrap();
    }
    out.push('\n');

    let (steps, whole) = steps(program);
    for step in steps {
        match step {
            Step::Op(op) => {
                if let Some(line) = quantum(op, &registers)? {
                    writeln!(out, "{line}").unwrap();
                }
            }
            Step::If {
                result,
                value,
                gate,
            } => writeln!(
                out,
                "if(c{}=={}) {}",
                result.0,
                u8::from(value),
                gate_line(gate)?
            )
            .unwrap(),
        }
    }
    if !whole {
        return Err(
            "OpenQASM 2 can only branch on one measurement to a block of gates, use --emit qasm3"
                .into(),
        );
    }
    Ok(out)
}
