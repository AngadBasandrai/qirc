use std::f64::consts::{FRAC_PI_2, PI};
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

pub struct TranspileStats {
    pub basis: Basis,
    pub gates_before: usize,
    pub gates_after: usize,
    pub entanglers: usize,
    pub leftover: usize,
}

impl fmt::Display for TranspileStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "basis {}: {} gates became {} ({} two qubit)",
            self.basis.name(),
            self.gates_before,
            self.gates_after,
            self.entanglers
        )
    }
}

pub fn transpile(program: &mut Program, basis: Basis) -> TranspileStats {
    let gates_before = program.gate_count();

    for block in &mut program.blocks {
        let mut rewritten = Vec::with_capacity(block.ops.len());

        for op in block.ops.drain(..) {
            match op {
                Op::Gate(gate) => {
                    rewritten.extend(decompose(&gate, basis).into_iter().map(Op::Gate))
                }
                other => rewritten.push(other),
            }
        }

        block.ops = rewritten;
    }

    TranspileStats {
        basis,
        gates_before,
        gates_after: program.gate_count(),
        entanglers: program
            .gates()
            .filter(|g| !g.controls.is_empty() || g.targets.len() > 1)
            .count(),
        leftover: program.gates().filter(|g| !basis.allows(g)).count(),
    }
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

pub(crate) fn decompose(gate: &Gate, basis: Basis) -> Vec<Gate> {
    if basis.allows(gate) {
        return vec![gate.clone()];
    }

    let span = gate.span;

    if gate.kind == GateKind::Swap && gate.controls.is_empty() {
        let (a, b) = (gate.targets[0], gate.targets[1]);
        return [
            entangle(GateKind::X, a, b, basis, span),
            entangle(GateKind::X, b, a, basis, span),
            entangle(GateKind::X, a, b, basis, span),
        ]
        .concat();
    }

    if gate.kind == GateKind::Swap && gate.controls.len() == 1 {
        let (control, a, b) = (gate.controls[0], gate.targets[0], gate.targets[1]);
        return [
            entangle(GateKind::X, b, a, basis, span),
            toffoli(control, a, b, basis, span),
            entangle(GateKind::X, b, a, basis, span),
        ]
        .concat();
    }

    if gate.params.iter().any(|p| p.constant().is_none()) {
        return runtime_rotation(gate, basis).unwrap_or_else(|| vec![gate.clone()]);
    }

    if gate.controls.len() == 2 && gate.targets.len() == 1 {
        let (a, b, target) = (gate.controls[0], gate.controls[1], gate.targets[0]);
        return match gate.kind {
            GateKind::X => toffoli(a, b, target, basis, span),
            GateKind::Z => [
                single(Matrix2::h(), target, basis, span),
                toffoli(a, b, target, basis, span),
                single(Matrix2::h(), target, basis, span),
            ]
            .concat(),
            _ => vec![gate.clone()],
        };
    }

    if gate.controls.len() == 1 && gate.targets.len() == 1 {
        return controlled_single(gate, basis);
    }

    if gate.controls.is_empty() && gate.targets.len() == 1 {
        let matrix = matrix_for(gate.kind, &gate.constant_params());
        return single(matrix, gate.targets[0], basis, span);
    }

    vec![gate.clone()]
}

fn runtime_rotation(gate: &Gate, basis: Basis) -> Option<Vec<Gate>> {
    if !gate.controls.is_empty() || gate.targets.len() != 1 {
        return None;
    }
    let (target, span) = (gate.targets[0], gate.span);
    let turn = |angle: f64| self::gate(GateKind::Rz, vec![target], vec![angle], span);
    let rotate = |kind| Gate {
        kind,
        ..gate.clone()
    };
    let hadamard = || single(Matrix2::h(), target, basis, span);

    Some(match (gate.kind, basis) {
        (GateKind::R1, _) => vec![rotate(GateKind::Rz)],
        (GateKind::Rx, Basis::RzSxCx) => {
            [hadamard(), vec![rotate(GateKind::Rz)], hadamard()].concat()
        }
        (GateKind::Ry, Basis::RzSxCx) => [
            vec![turn(-FRAC_PI_2)],
            hadamard(),
            vec![rotate(GateKind::Rz)],
            hadamard(),
            vec![turn(FRAC_PI_2)],
        ]
        .concat(),
        (GateKind::Rx, Basis::RzRyCz) => {
            vec![turn(FRAC_PI_2), rotate(GateKind::Ry), turn(-FRAC_PI_2)]
        }
        _ => return None,
    })
}

fn entangle(
    kind: GateKind,
    control: QubitId,
    target: QubitId,
    basis: Basis,
    span: Span,
) -> Vec<Gate> {
    if kind == basis.entangler() {
        return vec![controlled(kind, control, target, span)];
    }
    let mut out = single(Matrix2::h(), target, basis, span);
    out.push(controlled(basis.entangler(), control, target, span));
    out.extend(single(Matrix2::h(), target, basis, span));
    out
}

fn controlled_single(gate: &Gate, basis: Basis) -> Vec<Gate> {
    let control = gate.controls[0];
    let target = gate.targets[0];

    match gate.kind {
        GateKind::X | GateKind::Z => entangle(gate.kind, control, target, basis, gate.span),
        _ => {
            let matrix = matrix_for(gate.kind, &gate.constant_params());
            controlled_unitary(matrix, control, target, basis, gate.span)
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
        .find(|(_, r)| r.norm() > 1e-9)
        .map_or(0.0, |(l, r)| (l / r).arg())
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
    out.extend(entangle(GateKind::X, control, target, basis, span));
    out.extend(single(b, target, basis, span));
    out.extend(entangle(GateKind::X, control, target, basis, span));
    out.extend(single(a, target, basis, span));
    out.extend(single(Matrix2::phase(alpha), control, basis, span));

    out
}

fn toffoli(a: QubitId, b: QubitId, target: QubitId, basis: Basis, span: Span) -> Vec<Gate> {
    let mut out = Vec::new();

    out.extend(single(Matrix2::h(), target, basis, span));
    out.extend(entangle(GateKind::X, b, target, basis, span));
    out.extend(single(Matrix2::t_dagger(), target, basis, span));
    out.extend(entangle(GateKind::X, a, target, basis, span));
    out.extend(single(Matrix2::t(), target, basis, span));
    out.extend(entangle(GateKind::X, b, target, basis, span));
    out.extend(single(Matrix2::t_dagger(), target, basis, span));
    out.extend(entangle(GateKind::X, a, target, basis, span));
    out.extend(single(Matrix2::t(), b, basis, span));
    out.extend(single(Matrix2::t(), target, basis, span));
    out.extend(single(Matrix2::h(), target, basis, span));
    out.extend(entangle(GateKind::X, a, b, basis, span));
    out.extend(single(Matrix2::t(), a, basis, span));
    out.extend(single(Matrix2::t_dagger(), b, basis, span));
    out.extend(entangle(GateKind::X, a, b, basis, span));

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
    use crate::simulator::matrix::C64;
    use crate::simulator::state::State;

    fn product(gates: &[Gate]) -> Matrix2 {
        let mut acc = Matrix2::identity();
        for g in gates {
            let matrix = matrix_for(g.kind, &g.constant_params());
            acc = matrix.multiply(acc);
        }
        acc
    }

    fn same_up_to_phase(left: &[C64], right: &[C64]) -> bool {
        let Some(p) = right.iter().position(|r| r.norm() > 1e-9) else {
            return false;
        };
        let phase = left[p] / right[p];

        if (phase.norm() - 1.0).abs() > 1e-9 {
            return false;
        }

        left.iter()
            .zip(right)
            .all(|(l, r)| (l - phase * r).norm() < 1e-9)
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

    fn run_from(gates: &[Gate], qubits: usize, start: usize, spread: bool) -> Vec<C64> {
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
            let matrix = matrix_for(g.kind, &g.constant_params());
            for t in &g.targets {
                state.apply(&matrix, t.index(), controls);
            }
        }
        (0..state.len()).map(|i| state.amplitude(i)).collect()
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
                for spread in [false, true] {
                    let got = run_from(&decomposed, 3, start, spread);
                    let want = run_from(&direct, 3, start, spread);
                    assert!(
                        same_up_to_phase(&got, &want),
                        "{} toffoli wrong from |{start:03b}> (spread {spread})",
                        basis.name()
                    );
                }
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
                    for spread in [false, true] {
                        let got = run_from(&decomposed, 2, start, spread);
                        let want = run_from(&direct, 2, start, spread);
                        assert!(
                            same_up_to_phase(&got, &want),
                            "{} controlled decomposition wrong from |{start:02b}> for {matrix:?} (spread {spread})",
                            basis.name()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn swap_gate() {
        for basis in [Basis::RzSxCx, Basis::RzRyCz] {
            let direct = [Gate {
                kind: GateKind::Swap,
                controls: Vec::new(),
                targets: vec![QubitId(0), QubitId(1)],
                params: Vec::new(),
                span: Span::DUMMY,
            }];
            let decomposed = decompose(&direct[0], basis);

            for start in 0..4usize {
                for spread in [false, true] {
                    let got = run_from(&decomposed, 2, start, spread);
                    let want = run_from(&direct, 2, start, spread);
                    assert!(
                        same_up_to_phase(&got, &want),
                        "{} swap wrong from |{start:02b}> (spread {spread})",
                        basis.name()
                    );
                }
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
                    same_up_to_phase(
                        &[matrix.a, matrix.b, matrix.c, matrix.d],
                        &[rebuilt.a, rebuilt.b, rebuilt.c, rebuilt.d]
                    ),
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
    fn identity() {
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
