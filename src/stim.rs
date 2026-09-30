use std::fmt::Write;

use crate::calibration::{Calibration, Clock};
use crate::ir::*;
use crate::qasm2::{self, Step};
use crate::simulator::tableau::{self, quarter_turns};

fn name(gate: &Gate) -> Result<&'static str, String> {
    let params = tableau::clifford(gate).ok_or_else(|| match gate.controls.len() {
        0 => format!(
            "Stim only takes Clifford gates, and this `{}` is not one",
            gate.kind.name()
        ),
        controls => format!(
            "Stim only takes Clifford gates, and `{}` with {controls} control(s) is not one",
            gate.kind.name()
        ),
    })?;
    let turns = params.first().copied().and_then(quarter_turns).unwrap_or(0);
    Ok(match (gate.kind, gate.controls.len()) {
        (GateKind::X, 1) => "CX",
        (GateKind::Y, 1) => "CY",
        (GateKind::Z, 1) => "CZ",
        (GateKind::X, _) => "X",
        (GateKind::Y, _) => "Y",
        (GateKind::Z, _) => "Z",
        (GateKind::H, _) => "H",
        (GateKind::S, _) => "S",
        (GateKind::SDag, _) => "S_DAG",
        (GateKind::SX, _) => "SQRT_X",
        (GateKind::SXDag, _) => "SQRT_X_DAG",
        (GateKind::Swap, _) => "SWAP",
        (GateKind::Rz | GateKind::R1, _) => ["I", "S", "Z", "S_DAG"][turns],
        (GateKind::Rx, _) => ["I", "SQRT_X", "X", "SQRT_X_DAG"][turns],
        (GateKind::Ry, _) => ["I", "SQRT_Y", "Y", "SQRT_Y_DAG"][turns],
        (GateKind::I, _) => "I",
        (kind, _) => return Err(format!("Stim has no gate for `{}`", kind.name())),
    })
}

pub fn emit(program: &Program, noise: Option<&Calibration>) -> Result<String, String> {
    let (steps, whole) = qasm2::steps(program);
    if !whole {
        return Err(
            "Stim has no branches, only X, Y or Z gates that depend on one measurement".into(),
        );
    }
    let mut out = String::new();
    let mut clock = Clock::default();
    let mut records = vec![None; program.num_results as usize];
    let mut count = 0;
    for step in steps {
        match step {
            Step::Op(op @ (Op::Gate(_) | Op::Measure { .. } | Op::Reset { .. })) => {
                if let Some(calibration) = noise {
                    for idle in clock.begin(calibration, op) {
                        let ([x, y, z], q) = (idle.paulis, idle.qubit);
                        if x + y + z > 0.0 {
                            writeln!(out, "PAULI_CHANNEL_1({x}, {y}, {z}) {q}").unwrap();
                        }
                    }
                }
                let error = |p: f64| {
                    if p > 0.0 {
                        format!("({p})")
                    } else {
                        String::new()
                    }
                };
                match op {
                    Op::Gate(gate) => {
                        let wires: Vec<String> = gate.wires().map(|q| q.0.to_string()).collect();
                        let wires = wires.join(" ");
                        writeln!(out, "{} {wires}", name(gate)?).unwrap();
                        let p = noise.map_or(0.0, |c| c.gate_error(gate));
                        if p > 0.0 {
                            let arity = gate.wires().count();
                            writeln!(out, "DEPOLARIZE{arity}({p}) {wires}").unwrap();
                        }
                    }
                    Op::Measure { qubit, result, .. } => {
                        let p = noise.map_or(0.0, |c| c.readout(qubit.index()));
                        writeln!(out, "M{} {}", error(p), qubit.0).unwrap();
                        if let Some(record) = records.get_mut(result.index()) {
                            *record = Some(count);
                        }
                        count += 1;
                    }
                    Op::Reset { qubit, .. } => {
                        writeln!(out, "R {}", qubit.0).unwrap();
                        let p = noise.map_or(0.0, |c| c.readout(qubit.index()));
                        if p > 0.0 {
                            writeln!(out, "X_ERROR({p}) {}", qubit.0).unwrap();
                        }
                    }
                    _ => {}
                }
                if let Some(calibration) = noise {
                    clock.end(calibration, op);
                }
            }
            Step::Op(
                Op::RecordOutput { .. }
                | Op::Message { .. }
                | Op::Assign {
                    expr: Expr::ReadResult(_),
                    ..
                },
            ) => {}
            Step::Op(_) => return Err("Stim has no classical variables".into()),
            Step::If {
                result,
                value,
                gate,
            } => {
                if noise.is_some() {
                    return Err(
                        "Stim cannot make noise depend on a measurement, so it cannot write noisy corrections"
                            .into(),
                    );
                }
                let pauli = match (gate.kind, gate.controls.len()) {
                    (GateKind::X, 0) => "X",
                    (GateKind::Y, 0) => "Y",
                    (GateKind::Z, 0) => "Z",
                    _ => {
                        return Err(
                            "Stim can only apply X, Y or Z depending on a measurement".into()
                        );
                    }
                };
                let target = gate.targets[0].0;
                if !value {
                    writeln!(out, "{pauli} {target}").unwrap();
                }
                if let Some(Some(index)) = records.get(result.index()) {
                    writeln!(out, "C{pauli} rec[-{}] {target}", count - index).unwrap();
                }
            }
        }
    }
    Ok(out)
}
