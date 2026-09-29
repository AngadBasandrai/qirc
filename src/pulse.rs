use std::collections::BTreeSet;
use std::fmt::Write;

use crate::calibration::Calibration;
use crate::diag::Span;
use crate::ir::*;
use crate::transpile::{self, GateSet};

fn lowered(program: &Program, calibration: &Calibration) -> Result<Program, String> {
    let [block] = &program.blocks[..] else {
        return Err("a pulse schedule needs a straight line program".into());
    };
    let mut lowered = program.clone();
    if block.ops.iter().any(|op| matches!(op, Op::Gate(_))) {
        transpile::transpile(&mut lowered, &GateSet::parse("rz-sx-cx")?);
    }
    for gate in lowered.gates() {
        let supported = match (gate.kind, &gate.controls[..], &gate.targets[..]) {
            (GateKind::Rz, [], [_]) => gate.constant_angle().is_some(),
            (GateKind::SX | GateKind::X, [], [_]) => true,
            (GateKind::X, [c], [t]) => {
                if !calibration.coupled(c.index(), t.index()) {
                    return Err(format!(
                        "qubits {} and {} are not coupled in the calibration, compile with --calibration so the program is routed",
                        c.index(),
                        t.index()
                    ));
                }
                true
            }
            _ => false,
        };
        if !supported {
            return Err(format!(
                "`{}` has no pulse, since angles must be known at compile time",
                gate.kind.name()
            ));
        }
    }
    for q in used(&lowered) {
        if calibration.frequency(q).is_none() {
            return Err(format!(
                "the calibration gives no frequency for qubit {q}, add a line `frequency {q} GHz`"
            ));
        }
    }
    Ok(lowered)
}

fn used(program: &Program) -> BTreeSet<usize> {
    program
        .ops()
        .flat_map(|op| op.qubits())
        .map(|q| q.index())
        .collect()
}

fn measured(program: &Program) -> BTreeSet<usize> {
    program
        .ops()
        .filter_map(|op| match op {
            Op::Measure { qubit, .. } => Some(qubit.index()),
            _ => None,
        })
        .collect()
}

fn pairs(program: &Program) -> BTreeSet<(usize, usize)> {
    program
        .gates()
        .filter_map(|gate| match (&gate.controls[..], &gate.targets[..]) {
            ([c], [t]) => Some((c.index(), t.index())),
            _ => None,
        })
        .collect()
}

fn single(calibration: &Calibration, q: usize) -> f64 {
    calibration.duration(&Op::Gate(Gate {
        kind: GateKind::SX,
        controls: Vec::new(),
        targets: vec![QubitId(q as u32)],
        params: Vec::new(),
        span: Span::DUMMY,
    }))
}

fn statement(op: &Op) -> Option<String> {
    Some(match op {
        Op::Gate(gate) => match (gate.kind, &gate.controls[..], &gate.targets[..]) {
            (GateKind::Rz, [], [q]) => format!("rz({}) ${};", gate.constant_angle()?, q.0),
            (GateKind::SX, [], [q]) => format!("sx ${};", q.0),
            (GateKind::X, [], [q]) => format!("x ${};", q.0),
            (GateKind::X, [c], [t]) => format!("cx ${}, ${};", c.0, t.0),
            _ => return None,
        },
        Op::Measure { qubit, result, .. } => format!("c[{}] = measure ${};", result.0, qubit.0),
        Op::Reset { qubit, .. } => format!("reset ${};", qubit.0),
        _ => return None,
    })
}

pub fn emit(program: &Program, calibration: &Calibration) -> Result<String, String> {
    let program = lowered(program, calibration)?;
    let qubits = used(&program);
    let mut out = String::from("OPENQASM 3.0;\ndefcalgrammar \"openpulse\";\n\ncal {\n");
    for &q in &qubits {
        let hertz = calibration.frequency(q).unwrap_or_default() * 1e9;
        writeln!(out, "  port d{q};").unwrap();
        writeln!(out, "  frame q{q}_drive = newframe(d{q}, {hertz}, 0);").unwrap();
    }
    for &(c, t) in &pairs(&program) {
        let hertz = calibration.frequency(t).unwrap_or_default() * 1e9;
        writeln!(out, "  frame q{c}_q{t}_cross = newframe(d{c}, {hertz}, 0);").unwrap();
    }
    let readouts: Vec<(usize, f64, f64)> = measured(&program)
        .into_iter()
        .filter_map(|q| {
            calibration
                .resonator(q)
                .map(|(ghz, amplitude)| (q, ghz, amplitude))
        })
        .collect();
    for &(q, ghz, _) in &readouts {
        let hertz = ghz * 1e9;
        writeln!(out, "  port m{q};").unwrap();
        writeln!(out, "  port a{q};").unwrap();
        writeln!(out, "  frame q{q}_measure = newframe(m{q}, {hertz}, 0);").unwrap();
        writeln!(out, "  frame q{q}_acquire = newframe(a{q}, {hertz}, 0);").unwrap();
    }
    out.push_str("}\n\n");
    for &q in &qubits {
        let length = single(calibration, q);
        let (amplitude, beta) = calibration.drive(q);
        let sigma = length / 4.0;
        writeln!(
            out,
            "defcal rz(angle[20] theta) ${q} {{\n  shift_phase(q{q}_drive, -theta);\n}}"
        )
        .unwrap();
        writeln!(
            out,
            "defcal sx ${q} {{\n  play(q{q}_drive, drag({amplitude}, {length}ns, {sigma}ns, {beta}));\n}}"
        )
        .unwrap();
        writeln!(
            out,
            "defcal x ${q} {{\n  play(q{q}_drive, drag({}, {length}ns, {sigma}ns, {beta}));\n}}",
            2.0 * amplitude
        )
        .unwrap();
    }
    for &(c, t) in &pairs(&program) {
        let op = Op::Gate(Gate {
            kind: GateKind::X,
            controls: vec![QubitId(c as u32)],
            targets: vec![QubitId(t as u32)],
            params: Vec::new(),
            span: Span::DUMMY,
        });
        let length = calibration.duration(&op);
        let rise = (length / 8.0).min(10.0);
        writeln!(
            out,
            "defcal cx ${c}, ${t} {{\n  play(q{c}_q{t}_cross, gaussian_square({}, {length}ns, {}ns, {rise}ns));\n}}",
            calibration.cross(c, t),
            length - 4.0 * rise
        )
        .unwrap();
    }
    for &(q, _, amplitude) in &readouts {
        let length = calibration.duration(&Op::Measure {
            qubit: QubitId(q as u32),
            result: ResultId(0),
            span: Span::DUMMY,
        });
        writeln!(
            out,
            "defcal measure ${q} -> bit {{\n  play(q{q}_measure, constant({amplitude}, {length}ns));\n  return capture_v2(q{q}_acquire, {length}ns);\n}}"
        )
        .unwrap();
    }
    out.push('\n');
    if program.num_results > 0 {
        writeln!(out, "bit[{}] c;", program.num_results).unwrap();
    }
    let ops = &program.blocks[0].ops;
    let times = calibration.timeline(ops);
    let mut free = vec![0.0; program.num_qubits as usize];
    for (op, &(start, end)) in ops.iter().zip(&times) {
        let Some(line) = statement(op) else {
            continue;
        };
        for q in op.qubits() {
            let gap = start - free[q.index()];
            if gap > 1e-9 {
                writeln!(out, "delay[{gap}ns] ${};", q.0).unwrap();
            }
            free[q.index()] = end;
        }
        writeln!(out, "{line}").unwrap();
    }
    Ok(out)
}

pub fn schedule(program: &Program, calibration: &Calibration) -> Result<String, String> {
    let program = lowered(program, calibration)?;
    let ops = &program.blocks[0].ops;
    let times = calibration.timeline(ops);
    let total = times.iter().map(|&(_, end)| end).fold(0.0, f64::max);
    let mut out = format!("{:>10} {:>10}  operation\n", "start ns", "end ns");
    for (op, &(start, end)) in ops.iter().zip(&times) {
        if let Some(line) = statement(op) {
            writeln!(
                out,
                "{start:>10.1} {end:>10.1}  {}",
                line.trim_end_matches(';')
            )
            .unwrap();
        }
    }
    writeln!(
        out,
        "\ntotal {total:.1} ns on {} qubits",
        used(&program).len()
    )
    .unwrap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;

    #[test]
    fn schedules() {
        let calibration = Calibration::parse(
            "cx 0 1 0.01\ntime single 0 40\ntime single 1 40\ntime cx 0 1 300\nfrequency 0 5.1\nfrequency 1 4.9\ndrive 1 0.25 0.3\ntime readout 0 800\nresonator 0 7.2 0.15\n",
        )
        .unwrap();
        let (program, _) = qasm::lower(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[2];\ncreg c[2];\nh q[0];\ncx q[0], q[1];\nmeasure q -> c;\n",
        );
        let pulses = emit(&program, &calibration).unwrap();
        assert!(pulses.contains("frame q0_q1_cross = newframe(d0, 4900000000, 0);"));
        assert!(pulses.contains("play(q1_drive, drag(0.25, 40ns, 10ns, 0.3));"));
        assert!(pulses.contains("delay[40ns] $1;\ncx $0, $1;"));
        assert!(pulses.contains(
            "defcal measure $0 -> bit {\n  play(q0_measure, constant(0.15, 800ns));\n  return capture_v2(q0_acquire, 800ns);\n}"
        ));
        assert!(!pulses.contains("defcal measure $1"));
        let timeline = schedule(&program, &calibration).unwrap();
        assert!(
            timeline.contains("total 1140.0 ns on 2 qubits"),
            "{timeline}"
        );
        let silent = Calibration::parse("cx 0 1 0.01\n").unwrap();
        assert!(
            emit(&program, &silent)
                .unwrap_err()
                .contains("frequency 0 GHz")
        );
    }
}
