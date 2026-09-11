use std::collections::HashSet;

use crate::diag::Diagnostic;
use crate::ir::*;

pub fn validate(program: &Program) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    check_profile(program, &mut diagnostics);
    check_wires(program, &mut diagnostics);
    check_control_flow(program, &mut diagnostics);
    check_measurement_use(program, &mut diagnostics);

    diagnostics
}

fn check_profile(program: &Program, out: &mut Vec<Diagnostic>) {
    if program.profile.allows_branching() {
        return;
    }

    if !program.is_straight_line() {
        let block = program
            .blocks
            .iter()
            .find(|b| !matches!(b.term, Term::Ret(_) | Term::Unreachable));

        let span = block.map(|b| b.span).unwrap_or_default();

        out.push(
            Diagnostic::error("the Base Profile forbids branching")
                .with_code("QIR0300")
                .primary(span, "this program has more than one basic block")
                .note("recompile for the Adaptive Profile, or remove the measurement feedback"),
        );
    }

    for op in program.ops() {
        if let Op::Assign {
            expr: Expr::ReadResult(result),
            span,
            ..
        } = op
        {
            out.push(
                Diagnostic::error("the Base Profile forbids reading a measurement result")
                    .with_code("QIR0301")
                    .primary(
                        *span,
                        format!("r{} is read back into the program", result.0),
                    )
                    .note("only the Adaptive Profile can branch on a measurement"),
            );
        }

        if let Op::Reset { span, .. } = op {
            out.push(
                Diagnostic::warning("the Base Profile does not guarantee qubit reset")
                    .primary(*span, "this reset may not be supported by the target"),
            );
        }
    }
}

fn check_wires(program: &Program, out: &mut Vec<Diagnostic>) {
    for gate in program.gates() {
        for wire in gate.wires() {
            if wire.0 >= program.num_qubits {
                out.push(
                    Diagnostic::error(format!(
                        "qubit q{} is outside the declared register of {}",
                        wire.0, program.num_qubits
                    ))
                    .with_code("QIR0302")
                    .primary(gate.span, "out of range"),
                );
            }
        }

        let mut seen = HashSet::new();
        for wire in gate.wires() {
            if !seen.insert(wire) {
                out.push(
                    Diagnostic::error(format!(
                        "gate `{}` uses q{} more than once",
                        gate.kind.name(),
                        wire.0
                    ))
                    .with_code("QIR0303")
                    .primary(gate.span, "a gate cannot act twice on the same qubit"),
                );
                break;
            }
        }

        let expected = gate.kind.param_count();
        if gate.params.len() != expected {
            out.push(
                Diagnostic::error(format!(
                    "gate `{}` expects {expected} parameter(s) but has {}",
                    gate.kind.name(),
                    gate.params.len()
                ))
                .with_code("QIR0304")
                .primary(gate.span, "wrong number of parameters"),
            );
        }

        if gate.targets.len() != gate.kind.arity() {
            out.push(
                Diagnostic::error(format!(
                    "gate `{}` expects {} target(s) but has {}",
                    gate.kind.name(),
                    gate.kind.arity(),
                    gate.targets.len()
                ))
                .with_code("QIR0305")
                .primary(gate.span, "wrong number of targets"),
            );
        }
    }

    for op in program.ops() {
        match op {
            Op::Measure {
                qubit,
                result,
                span,
                ..
            } => {
                if qubit.0 >= program.num_qubits {
                    out.push(
                        Diagnostic::error(format!("measuring undeclared qubit q{}", qubit.0))
                            .with_code("QIR0302")
                            .primary(*span, "out of range"),
                    );
                }
                if result.0 >= program.num_results {
                    out.push(
                        Diagnostic::error(format!(
                            "result r{} is outside the declared {} results",
                            result.0, program.num_results
                        ))
                        .with_code("QIR0306")
                        .primary(*span, "out of range"),
                    );
                }
            }
            Op::Reset { qubit, span } if qubit.0 >= program.num_qubits => out.push(
                Diagnostic::error(format!("resetting undeclared qubit q{}", qubit.0))
                    .with_code("QIR0302")
                    .primary(*span, "out of range"),
            ),
            _ => {}
        }
    }
}

fn check_control_flow(program: &Program, out: &mut Vec<Diagnostic>) {
    for block in &program.blocks {
        for successor in block.term.successors() {
            if successor.0 as usize >= program.blocks.len() {
                out.push(
                    Diagnostic::error("branch target does not exist")
                        .with_code("QIR0307")
                        .primary(block.span, "dangling edge"),
                );
            }
        }
    }

    let reachable = program.reachable();
    for (index, live) in reachable.iter().enumerate() {
        if !live && !program.blocks[index].ops.is_empty() {
            out.push(
                Diagnostic::warning(format!(
                    "block `{}` is unreachable",
                    program.blocks[index].label
                ))
                .primary(program.blocks[index].span, "no path reaches this block"),
            );
        }
    }
}

fn check_measurement_use(program: &Program, out: &mut Vec<Diagnostic>) {
    let mut written: HashSet<ResultId> = HashSet::new();

    for op in program.ops() {
        if let Op::Measure { result, .. } = op {
            written.insert(*result);
        }
    }

    for op in program.ops() {
        if let Op::Assign {
            expr: Expr::ReadResult(result),
            span,
            ..
        } = op
            && !written.contains(result)
        {
            out.push(
                Diagnostic::error(format!("r{} is read before it is measured", result.0))
                    .with_code("QIR0308")
                    .primary(*span, "no measurement writes this result"),
            );
        }

        if let Op::RecordOutput {
            result: Some(result),
            span,
            ..
        } = op
            && !written.contains(result)
        {
            out.push(
                Diagnostic::warning(format!(
                    "r{} is recorded as output but never measured",
                    result.0
                ))
                .primary(*span, "this will always record zero"),
            );
        }
    }
}
