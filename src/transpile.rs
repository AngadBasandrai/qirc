use std::f64::consts::PI;
use std::fmt;

use crate::codegen::zyz_angles;
use crate::diag::Span;
use crate::ir::*;
use crate::simulator::matrix::{Matrix2, matrix_for};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Basis {
    RzSxCx,
    RzRyCz,
}

impl Basis {
    pub fn parse(text: &str) -> Option<Basis> {
        Some(match text {
            "rz-sx-cx" | "ibm" => Basis::RzSxCx,
            "rz-ry-cz" | "cz" => Basis::RzRyCz,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Basis::RzSxCx => "rz-sx-cx",
            Basis::RzRyCz => "rz-ry-cz",
        }
    }

    pub fn entangler(self) -> GateKind {
        match self {
            Basis::RzSxCx => GateKind::X,
            Basis::RzRyCz => GateKind::Z,
        }
    }

    pub fn allows(self, gate: &Gate) -> bool {
        if gate.controls.len() > 1 {
            return false;
        }

        if gate.controls.len() == 1 {
            return gate.kind == self.entangler() && gate.targets.len() == 1;
        }

        match self {
            Basis::RzSxCx => matches!(gate.kind, GateKind::Rz | GateKind::SX | GateKind::X),
            Basis::RzRyCz => matches!(gate.kind, GateKind::Rz | GateKind::Ry),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TranspileStats {
    pub basis: Option<&'static str>,
    pub gates_before: usize,
    pub gates_after: usize,
    pub entanglers: usize,
}

impl fmt::Display for TranspileStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "basis {}: {} gates became {} ({} two qubit)",
            self.basis.unwrap_or("none"),
            self.gates_before,
            self.gates_after,
            self.entanglers
        )
    }
}

pub fn transpile(program: &mut Program, basis: Basis) -> TranspileStats {
    let mut stats = TranspileStats {
        basis: Some(basis.name()),
        gates_before: program.gate_count(),
        ..Default::default()
    };

    for block in &mut program.blocks {
        let mut rewritten: Vec<Op> = Vec::with_capacity(block.ops.len());

        for op in block.ops.drain(..) {
            match op {
                Op::Gate(gate) => {
                    for replacement in decompose(&gate, basis) {
                        rewritten.push(Op::Gate(replacement));
                    }
                }
                other => rewritten.push(other),
            }
        }

        block.ops = rewritten;
    }

    stats.gates_after = program.gate_count();
    stats.entanglers = program
        .gates()
        .filter(|g| !g.controls.is_empty() || g.targets.len() > 1)
        .count();
    stats
}

fn gate(kind: GateKind, targets: Vec<QubitId>, params: Vec<f64>, span: Span) -> Gate {
    Gate {
        kind,
        controls: Vec::new(),
        targets,
        params: params
            .into_iter()
            .map(|p| Operand::Const(Const::Float(p)))
            .collect(),
        span,
    }
}

fn controlled(kind: GateKind, control: QubitId, target: QubitId, span: Span) -> Gate {
    Gate {
        kind,
        controls: vec![control],
        targets: vec![target],
        params: Vec::new(),
        span,
    }
}

fn decompose(gate_in: &Gate, basis: Basis) -> Vec<Gate> {
    if basis.allows(gate_in) {
        return vec![gate_in.clone()];
    }

    let span = gate_in.span;

    if gate_in.kind == GateKind::Swap && gate_in.controls.is_empty() {
        let (a, b) = (gate_in.targets[0], gate_in.targets[1]);
        return [
            cx(a, b, basis, span),
            cx(b, a, basis, span),
            cx(a, b, basis, span),
        ]
        .concat();
    }

    if gate_in.controls.len() == 2 && gate_in.targets.len() == 1 {
        return toffoli(
            gate_in.controls[0],
            gate_in.controls[1],
            gate_in.targets[0],
            basis,
            span,
        );
    }

    if gate_in.controls.len() == 1 && gate_in.targets.len() == 1 {
        return controlled_single(gate_in, basis, span);
    }

    if gate_in.controls.is_empty() && gate_in.targets.len() == 1 {
        let matrix = matrix_for(gate_in.kind, &constant_params(gate_in));
        return single(matrix, gate_in.targets[0], basis, span);
    }

    vec![gate_in.clone()]
}

fn constant_params(gate: &Gate) -> Vec<f64> {
    gate.params
        .iter()
        .filter_map(|p| p.constant().map(|c| c.as_f64()))
        .collect()
}

fn cx(control: QubitId, target: QubitId, basis: Basis, span: Span) -> Vec<Gate> {
    match basis {
        Basis::RzSxCx => vec![controlled(GateKind::X, control, target, span)],
        Basis::RzRyCz => {
            let mut out = single(Matrix2::h(), target, basis, span);
            out.push(controlled(GateKind::Z, control, target, span));
            out.extend(single(Matrix2::h(), target, basis, span));
            out
        }
    }
}

fn cz(control: QubitId, target: QubitId, basis: Basis, span: Span) -> Vec<Gate> {
    match basis {
        Basis::RzRyCz => vec![controlled(GateKind::Z, control, target, span)],
        Basis::RzSxCx => {
            let mut out = single(Matrix2::h(), target, basis, span);
            out.push(controlled(GateKind::X, control, target, span));
            out.extend(single(Matrix2::h(), target, basis, span));
            out
        }
    }
}

fn controlled_single(gate_in: &Gate, basis: Basis, span: Span) -> Vec<Gate> {
    let control = gate_in.controls[0];
    let target = gate_in.targets[0];

    match gate_in.kind {
        GateKind::X => cx(control, target, basis, span),
        GateKind::Z => cz(control, target, basis, span),
        _ => {
            let matrix = matrix_for(gate_in.kind, &constant_params(gate_in));
            controlled_unitary(matrix, control, target, basis, span)
        }
    }
}

fn global_phase(matrix: &Matrix2, theta: f64, phi: f64, lambda: f64) -> f64 {
    let rebuilt = Matrix2::rz(phi)
        .multiply(Matrix2::ry(theta))
        .multiply(Matrix2::rz(lambda));

    let entries = [
        (matrix.a, rebuilt.a),
        (matrix.b, rebuilt.b),
        (matrix.c, rebuilt.c),
        (matrix.d, rebuilt.d),
    ];

    entries
        .iter()
        .filter(|(_, r)| r.norm() > 1e-9)
        .map(|(l, r)| (l / r).arg())
        .next()
        .unwrap_or(0.0)
}

fn controlled_unitary(
    matrix: Matrix2,
    control: QubitId,
    target: QubitId,
    basis: Basis,
    span: Span,
) -> Vec<Gate> {
    let (theta, phi, lambda) = zyz_angles(&matrix);
    let alpha = global_phase(&matrix, theta, phi, lambda);

    let a = Matrix2::rz(phi).multiply(Matrix2::ry(theta / 2.0));
    let b = Matrix2::ry(-theta / 2.0).multiply(Matrix2::rz(-(lambda + phi) / 2.0));
    let c = Matrix2::rz((lambda - phi) / 2.0);

    let mut out = Vec::new();

    out.extend(single(c, target, basis, span));
    out.extend(cx(control, target, basis, span));
    out.extend(single(b, target, basis, span));
    out.extend(cx(control, target, basis, span));
    out.extend(single(a, target, basis, span));
    out.extend(single(Matrix2::phase(alpha), control, basis, span));

    out
}

fn toffoli(a: QubitId, b: QubitId, target: QubitId, basis: Basis, span: Span) -> Vec<Gate> {
    let mut out = Vec::new();

    out.extend(single(Matrix2::h(), target, basis, span));
    out.extend(cx(b, target, basis, span));
    out.extend(single(Matrix2::t_dagger(), target, basis, span));
    out.extend(cx(a, target, basis, span));
    out.extend(single(Matrix2::t(), target, basis, span));
    out.extend(cx(b, target, basis, span));
    out.extend(single(Matrix2::t_dagger(), target, basis, span));
    out.extend(cx(a, target, basis, span));
    out.extend(single(Matrix2::t(), b, basis, span));
    out.extend(single(Matrix2::t(), target, basis, span));
    out.extend(single(Matrix2::h(), target, basis, span));
    out.extend(cx(a, b, basis, span));
    out.extend(single(Matrix2::t(), a, basis, span));
    out.extend(single(Matrix2::t_dagger(), b, basis, span));
    out.extend(cx(a, b, basis, span));

    out
}

pub fn single(matrix: Matrix2, target: QubitId, basis: Basis, span: Span) -> Vec<Gate> {
    if matrix.is_identity(1e-12) {
        return Vec::new();
    }

    let (theta, phi, lambda) = zyz_angles(&matrix);

    match basis {
        Basis::RzRyCz => [
            (GateKind::Rz, lambda),
            (GateKind::Ry, theta),
            (GateKind::Rz, phi),
        ]
        .into_iter()
        .filter(|(_, angle)| angle.abs() > 1e-12)
        .map(|(kind, angle)| gate(kind, vec![target], vec![angle], span))
        .collect(),

        Basis::RzSxCx => {
            let mut out = Vec::new();
            let push_rz = |angle: f64, out: &mut Vec<Gate>| {
                let wrapped = wrap(angle);
                if wrapped.abs() > 1e-12 {
                    out.push(gate(GateKind::Rz, vec![target], vec![wrapped], span));
                }
            };

            push_rz(lambda, &mut out);
            out.push(gate(GateKind::SX, vec![target], Vec::new(), span));
            push_rz(theta + PI, &mut out);
            out.push(gate(GateKind::SX, vec![target], Vec::new(), span));
            push_rz(phi + PI, &mut out);
            out
        }
    }
}

fn wrap(angle: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let mut wrapped = angle % two_pi;
    if wrapped > PI {
        wrapped -= two_pi;
    }
    if wrapped < -PI {
        wrapped += two_pi;
    }
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(gates: &[Gate]) -> Matrix2 {
        let mut acc = Matrix2::identity();
        for g in gates {
            let matrix = matrix_for(g.kind, &constant_params(g));
            acc = matrix.multiply(acc);
        }
        acc
    }

    fn equal_up_to_phase(left: &Matrix2, right: &Matrix2) -> bool {
        let entries = [
            (left.a, right.a),
            (left.b, right.b),
            (left.c, right.c),
            (left.d, right.d),
        ];

        let Some((reference, target)) = entries.iter().find(|(_, r)| r.norm() > 1e-9) else {
            return false;
        };
        let phase = reference / target;

        if (phase.norm() - 1.0).abs() > 1e-9 {
            return false;
        }

        entries.iter().all(|(l, r)| (l - phase * r).norm() < 1e-9)
    }

    fn sample_matrices() -> Vec<Matrix2> {
        vec![
            Matrix2::h(),
            Matrix2::x(),
            Matrix2::y(),
            Matrix2::z(),
            Matrix2::s(),
            Matrix2::t(),
            Matrix2::s_dagger(),
            Matrix2::sx(),
            Matrix2::rx(0.7),
            Matrix2::ry(-1.9),
            Matrix2::rz(2.4),
            Matrix2::phase(0.3),
            Matrix2::rx(0.4)
                .multiply(Matrix2::ry(1.1))
                .multiply(Matrix2::rz(-0.8)),
        ]
    }

    use crate::simulator::state::State;

    fn run_gates(gates: &[Gate], qubits: usize, start: usize) -> Vec<C64Pair> {
        run_from(gates, qubits, start, false)
    }

    fn run_superposed(gates: &[Gate], qubits: usize, start: usize) -> Vec<C64Pair> {
        run_from(gates, qubits, start, true)
    }

    fn run_from(gates: &[Gate], qubits: usize, start: usize, spread: bool) -> Vec<C64Pair> {
        let mut state = State::new(qubits);
        for bit in 0..qubits {
            if (start >> bit) & 1 == 1 {
                state.apply(&Matrix2::x(), bit, 0);
            }
        }
        if spread {
            for bit in 0..qubits {
                state.apply(&Matrix2::h(), bit, 0);
                state.apply(&Matrix2::t(), bit, 0);
            }
        }
        for g in gates {
            let controls = g.controls.iter().fold(0u64, |mask, q| mask | (1u64 << q.0));
            if g.kind == GateKind::Swap {
                state.swap(g.targets[0].index(), g.targets[1].index(), controls);
                continue;
            }
            let matrix = matrix_for(g.kind, &constant_params(g));
            for t in &g.targets {
                state.apply(&matrix, t.index(), controls);
            }
        }
        (0..state.len())
            .map(|i| {
                let amp = state.amplitude(i);
                (amp.re, amp.im)
            })
            .collect()
    }

    type C64Pair = (f64, f64);

    fn same_up_to_phase(left: &[C64Pair], right: &[C64Pair]) -> bool {
        let Some(pivot) = (0..right.len()).find(|&i| right[i].0.hypot(right[i].1) > 1e-9) else {
            return false;
        };
        let (rr, ri) = right[pivot];
        let (lr, li) = left[pivot];
        let denominator = rr * rr + ri * ri;
        let phase = (
            (lr * rr + li * ri) / denominator,
            (li * rr - lr * ri) / denominator,
        );

        left.iter().zip(right).all(|((ar, ai), (br, bi))| {
            let pr = phase.0 * br - phase.1 * bi;
            let pi = phase.0 * bi + phase.1 * br;
            (ar - pr).hypot(ai - pi) < 1e-9
        })
    }

    #[test]
    fn ccx() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            let decomposed = toffoli(QubitId(0), QubitId(1), QubitId(2), basis, Span::DUMMY);
            let direct = vec![Gate {
                kind: GateKind::X,
                controls: vec![QubitId(0), QubitId(1)],
                targets: vec![QubitId(2)],
                params: Vec::new(),
                span: Span::DUMMY,
            }];

            for start in 0..8usize {
                let got = run_gates(&decomposed, 3, start);
                let want = run_gates(&direct, 3, start);
                assert!(
                    same_up_to_phase(&got, &want),
                    "{} toffoli wrong from |{start:03b}>",
                    basis.name()
                );

                let got = run_superposed(&decomposed, 3, start);
                let want = run_superposed(&direct, 3, start);
                assert!(
                    same_up_to_phase(&got, &want),
                    "{} toffoli wrong on superposed controls from |{start:03b}>",
                    basis.name()
                );
            }
        }
    }

    #[test]
    fn controlled_u() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            for matrix in sample_matrices() {
                let decomposed =
                    controlled_unitary(matrix, QubitId(0), QubitId(1), basis, Span::DUMMY);
                let direct = vec![Gate {
                    kind: GateKind::Unitary(matrix.to_ir()),
                    controls: vec![QubitId(0)],
                    targets: vec![QubitId(1)],
                    params: Vec::new(),
                    span: Span::DUMMY,
                }];

                for start in 0..4usize {
                    let got = run_gates(&decomposed, 2, start);
                    let want = run_gates(&direct, 2, start);
                    assert!(
                        same_up_to_phase(&got, &want),
                        "{} controlled decomposition wrong from |{start:02b}> for {matrix:?}",
                        basis.name()
                    );

                    let got = run_superposed(&decomposed, 2, start);
                    let want = run_superposed(&direct, 2, start);
                    assert!(
                        same_up_to_phase(&got, &want),
                        "{} controlled decomposition wrong on a superposed control from |{start:02b}> for {matrix:?}",
                        basis.name()
                    );
                }
            }
        }
    }

    #[test]
    fn swap_gate() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            let gate_in = Gate {
                kind: GateKind::Swap,
                controls: Vec::new(),
                targets: vec![QubitId(0), QubitId(1)],
                params: Vec::new(),
                span: Span::DUMMY,
            };
            let decomposed = decompose(&gate_in, basis);

            for start in 0..4usize {
                let got = run_superposed(&decomposed, 2, start);
                let want = run_superposed(std::slice::from_ref(&gate_in), 2, start);
                assert!(
                    same_up_to_phase(&got, &want),
                    "{} swap wrong from |{start:02b}>",
                    basis.name()
                );
            }
        }
    }

    #[test]
    fn one_qubit() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            for matrix in sample_matrices() {
                let gates = single(matrix, QubitId(0), basis, Span::DUMMY);
                let rebuilt = product(&gates);
                assert!(
                    equal_up_to_phase(&matrix, &rebuilt),
                    "{} failed to reproduce {matrix:?}, got {rebuilt:?}",
                    basis.name()
                );
            }
        }
    }

    #[test]
    fn in_basis() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            for matrix in sample_matrices() {
                for produced in single(matrix, QubitId(0), basis, Span::DUMMY) {
                    assert!(
                        basis.allows(&produced),
                        "{} produced {:?} which is outside the basis",
                        basis.name(),
                        produced.kind
                    );
                }
            }
        }
    }

    #[test]
    fn identity_is_empty() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            assert!(single(Matrix2::identity(), QubitId(0), basis, Span::DUMMY).is_empty());
        }
    }

    #[test]
    fn membership() {
        let rz = gate(GateKind::Rz, vec![QubitId(0)], vec![0.5], Span::DUMMY);
        assert!(Basis::RzSxCx.allows(&rz));
        assert!(Basis::RzRyCz.allows(&rz));

        let ry = gate(GateKind::Ry, vec![QubitId(0)], vec![0.5], Span::DUMMY);
        assert!(!Basis::RzSxCx.allows(&ry));
        assert!(Basis::RzRyCz.allows(&ry));

        let cx_gate = controlled(GateKind::X, QubitId(0), QubitId(1), Span::DUMMY);
        assert!(Basis::RzSxCx.allows(&cx_gate));
        assert!(!Basis::RzRyCz.allows(&cx_gate));

        let toffoli_gate = Gate {
            kind: GateKind::X,
            controls: vec![QubitId(0), QubitId(1)],
            targets: vec![QubitId(2)],
            params: Vec::new(),
            span: Span::DUMMY,
        };
        assert!(!Basis::RzSxCx.allows(&toffoli_gate));
    }
}
