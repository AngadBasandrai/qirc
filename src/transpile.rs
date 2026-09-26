use std::fmt;

use crate::codegen::zyz_angles;
use crate::diag::Span;
use crate::ir::*;
use crate::simulator::matrix::{Matrix2, matrix_for};
use crate::synth::Synth;

const NATIVE: [(GateKind, usize); 27] = [
    (GateKind::I, 0),
    (GateKind::X, 0),
    (GateKind::Y, 0),
    (GateKind::Z, 0),
    (GateKind::H, 0),
    (GateKind::S, 0),
    (GateKind::SDag, 0),
    (GateKind::T, 0),
    (GateKind::TDag, 0),
    (GateKind::SX, 0),
    (GateKind::SXDag, 0),
    (GateKind::Rx, 0),
    (GateKind::Ry, 0),
    (GateKind::Rz, 0),
    (GateKind::R1, 0),
    (GateKind::Swap, 0),
    (GateKind::X, 1),
    (GateKind::Y, 1),
    (GateKind::Z, 1),
    (GateKind::H, 1),
    (GateKind::Rx, 1),
    (GateKind::Ry, 1),
    (GateKind::Rz, 1),
    (GateKind::R1, 1),
    (GateKind::Swap, 1),
    (GateKind::X, 2),
    (GateKind::Z, 2),
];

#[derive(Clone, PartialEq, Debug)]
pub struct GateSet {
    name: String,
    gates: Vec<(GateKind, usize)>,
}

impl GateSet {
    pub fn native() -> GateSet {
        GateSet {
            name: "native".into(),
            gates: NATIVE.to_vec(),
        }
    }

    pub fn parse(text: &str) -> Result<GateSet, String> {
        let gates = match text {
            "rz-sx-cx" | "ibm" => vec![
                (GateKind::Rz, 0),
                (GateKind::SX, 0),
                (GateKind::X, 0),
                (GateKind::X, 1),
            ],
            "rz-ry-cz" | "cz" => vec![(GateKind::Rz, 0), (GateKind::Ry, 0), (GateKind::Z, 1)],
            _ => text.split(',').map(named).collect::<Result<_, _>>()?,
        };
        Ok(GateSet {
            name: text.to_string(),
            gates,
        })
    }

    pub fn without(mut self, text: &str) -> Result<GateSet, String> {
        for gate in text.split(',').map(named) {
            let gate = gate?;
            self.gates.retain(|g| *g != gate);
        }
        self.name = format!("{} without {text}", self.name);
        Ok(self)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn has(&self, kind: GateKind, controls: usize) -> bool {
        self.gates.contains(&(kind, controls))
    }

    pub fn allows(&self, gate: &Gate) -> bool {
        let targets = if gate.kind == GateKind::Swap { 2 } else { 1 };
        !matches!(gate.kind, GateKind::Unitary(_))
            && gate.targets.len() == targets
            && self.has(gate.kind, gate.controls.len())
    }
}

fn named(name: &str) -> Result<(GateKind, usize), String> {
    let name = name.trim();
    let base = name.trim_start_matches('c');
    let kind = match base {
        "id" | "i" => GateKind::I,
        "x" => GateKind::X,
        "y" => GateKind::Y,
        "z" => GateKind::Z,
        "h" => GateKind::H,
        "s" => GateKind::S,
        "sdg" => GateKind::SDag,
        "t" => GateKind::T,
        "tdg" => GateKind::TDag,
        "sx" => GateKind::SX,
        "sxdg" => GateKind::SXDag,
        "rx" => GateKind::Rx,
        "ry" => GateKind::Ry,
        "rz" => GateKind::Rz,
        "r1" | "p" => GateKind::R1,
        "swap" => GateKind::Swap,
        _ => return Err(format!("unknown gate `{name}`")),
    };
    Ok((kind, name.len() - base.len()))
}

fn gate_name(gate: &Gate) -> String {
    format!("{}{}", "c".repeat(gate.controls.len()), gate.kind.name())
}

pub struct TranspileStats {
    pub set: String,
    pub gates_before: usize,
    pub gates_after: usize,
    pub entanglers: usize,
    pub leftover: Vec<String>,
}

impl fmt::Display for TranspileStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gate set {}: {} gates became {} ({} two qubit)",
            self.set, self.gates_before, self.gates_after, self.entanglers
        )
    }
}

pub fn transpile(program: &mut Program, set: &GateSet) -> TranspileStats {
    let synth = Synth::new(set);
    let gates_before = program.gate_count();
    let mut leftover = Vec::new();

    for block in &mut program.blocks {
        let mut rewritten = Vec::with_capacity(block.ops.len());

        for op in block.ops.drain(..) {
            let Op::Gate(gate) = op else {
                rewritten.push(op);
                continue;
            };
            match translate(&gate, set, &synth) {
                Some(gates) => rewritten.extend(gates.into_iter().map(Op::Gate)),
                None => {
                    let name = gate_name(&gate);
                    if !leftover.contains(&name) {
                        leftover.push(name);
                    }
                    rewritten.push(Op::Gate(gate));
                }
            }
        }

        block.ops = rewritten;
    }

    TranspileStats {
        set: set.name().to_string(),
        gates_before,
        gates_after: program.gate_count(),
        entanglers: program
            .gates()
            .filter(|g| !g.controls.is_empty() || g.targets.len() > 1)
            .count(),
        leftover,
    }
}

pub(crate) fn translate(gate: &Gate, set: &GateSet, synth: &Synth) -> Option<Vec<Gate>> {
    if set.allows(gate) {
        return Some(vec![gate.clone()]);
    }

    if gate.params.iter().any(|p| p.constant().is_none()) {
        if !gate.controls.is_empty() || gate.targets.len() != 1 {
            return None;
        }
        return synth.turn(gate.kind, gate.params[0], gate.targets[0], gate.span);
    }

    let mut out = Vec::new();
    for part in fuse(canonical(gate)?) {
        let piece = match part.controls.as_slice() {
            [] => synth.one(
                matrix_for(part.kind, &part.constant_params()),
                part.targets[0],
                part.span,
            ),
            [control] => synth.cx(*control, part.targets[0], part.span),
            _ => None,
        };
        out.extend(piece?);
    }
    Some(out)
}

fn unitary(matrix: Matrix2, target: QubitId, span: Span) -> Gate {
    Gate {
        kind: GateKind::Unitary(matrix.to_ir()),
        controls: Vec::new(),
        targets: vec![target],
        params: Vec::new(),
        span,
    }
}

fn cx(control: QubitId, target: QubitId, span: Span) -> Gate {
    Gate {
        kind: GateKind::X,
        controls: vec![control],
        targets: vec![target],
        params: Vec::new(),
        span,
    }
}

fn canonical(gate: &Gate) -> Option<Vec<Gate>> {
    let span = gate.span;
    let h = |q| unitary(Matrix2::h(), q, span);
    let matrix = matrix_for(gate.kind, &gate.constant_params());

    Some(
        match (gate.controls.as_slice(), gate.targets.as_slice(), gate.kind) {
            ([], [target], _) => vec![unitary(matrix, *target, span)],
            ([], [a, b], GateKind::Swap) => {
                vec![cx(*a, *b, span), cx(*b, *a, span), cx(*a, *b, span)]
            }
            ([c], [a, b], GateKind::Swap) => [
                vec![cx(*b, *a, span)],
                toffoli(*c, *a, *b, span),
                vec![cx(*b, *a, span)],
            ]
            .concat(),
            ([c], [t], GateKind::X) => vec![cx(*c, *t, span)],
            ([c], [t], GateKind::Z) => vec![h(*t), cx(*c, *t, span), h(*t)],
            ([c], [t], _) => controlled_unitary(matrix, *c, *t, span),
            ([a, b], [t], GateKind::X) => toffoli(*a, *b, *t, span),
            ([a, b], [t], GateKind::Z) => {
                [vec![h(*t)], toffoli(*a, *b, *t, span), vec![h(*t)]].concat()
            }
            _ => return None,
        },
    )
}

fn fuse(parts: Vec<Gate>) -> Vec<Gate> {
    let mut out: Vec<Gate> = Vec::new();
    for part in parts {
        if let ([], [wire], GateKind::Unitary(m)) =
            (part.controls.as_slice(), part.targets.as_slice(), part.kind)
            && let Some(last) = out.iter_mut().rev().find(|g| g.wires().any(|w| w == *wire))
            && let ([], GateKind::Unitary(earlier)) = (last.controls.as_slice(), last.kind)
        {
            let product = Matrix2::from_ir(m).multiply(Matrix2::from_ir(earlier));
            last.kind = GateKind::Unitary(product.to_ir());
            continue;
        }
        out.push(part);
    }
    out
}

fn global_phase(matrix: &Matrix2, theta: f64, phi: f64, lambda: f64) -> f64 {
    let rebuilt = Matrix2::rz(phi)
        .multiply(Matrix2::ry(theta))
        .multiply(Matrix2::rz(lambda));

    [
        (matrix.a, rebuilt.a),
        (matrix.b, rebuilt.b),
        (matrix.c, rebuilt.c),
        (matrix.d, rebuilt.d),
    ]
    .iter()
    .find(|(_, r)| r.norm() > 1e-9)
    .map_or(0.0, |(l, r)| (l / r).arg())
}

fn controlled_unitary(matrix: Matrix2, control: QubitId, target: QubitId, span: Span) -> Vec<Gate> {
    let (theta, phi, lambda) = zyz_angles(&matrix);
    let alpha = global_phase(&matrix, theta, phi, lambda);

    let a = Matrix2::rz(phi).multiply(Matrix2::ry(theta / 2.0));
    let b = Matrix2::ry(-theta / 2.0).multiply(Matrix2::rz(-(lambda + phi) / 2.0));
    let c = Matrix2::rz((lambda - phi) / 2.0);

    vec![
        unitary(c, target, span),
        cx(control, target, span),
        unitary(b, target, span),
        cx(control, target, span),
        unitary(a, target, span),
        unitary(Matrix2::phase(alpha), control, span),
    ]
}

fn toffoli(a: QubitId, b: QubitId, target: QubitId, span: Span) -> Vec<Gate> {
    let one = |m, q| unitary(m, q, span);
    vec![
        one(Matrix2::h(), target),
        cx(b, target, span),
        one(Matrix2::t_dagger(), target),
        cx(a, target, span),
        one(Matrix2::t(), target),
        cx(b, target, span),
        one(Matrix2::t_dagger(), target),
        cx(a, target, span),
        one(Matrix2::t(), b),
        one(Matrix2::t(), target),
        one(Matrix2::h(), target),
        cx(a, b, span),
        one(Matrix2::t(), a),
        one(Matrix2::t_dagger(), b),
        cx(a, b, span),
    ]
}

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;
    use crate::synth::Unitary;

    const WIRES: [QubitId; 3] = [QubitId(0), QubitId(1), QubitId(2)];

    fn gate(kind: GateKind, controls: &[u32], targets: &[u32], params: &[f64]) -> Gate {
        Gate {
            kind,
            controls: controls.iter().map(|&q| QubitId(q)).collect(),
            targets: targets.iter().map(|&q| QubitId(q)).collect(),
            params: params
                .iter()
                .map(|&p| Operand::Const(Const::Float(p)))
                .collect(),
            span: Span::DUMMY,
        }
    }

    fn check(set: &str, original: Gate) {
        let set = GateSet::parse(set).unwrap();
        let synth = Synth::new(&set);
        let wires = &WIRES[..original.wires().map(|q| q.index() + 1).max().unwrap()];
        let rebuilt = translate(&original, &set, &synth)
            .unwrap_or_else(|| panic!("{} could not build {:?}", set.name(), original.kind));
        for produced in &rebuilt {
            assert!(
                set.allows(produced),
                "{} produced {:?}",
                set.name(),
                produced.kind
            );
        }
        let want = Unitary::of(slice::from_ref(&original), wires).unwrap();
        let got = Unitary::of(&rebuilt, wires).unwrap();
        assert!(
            got.same(&want),
            "{} rebuilt {:?} wrongly",
            set.name(),
            original.kind
        );
    }

    fn samples() -> Vec<Gate> {
        let mut out = Vec::new();
        for kind in [
            GateKind::H,
            GateKind::X,
            GateKind::Y,
            GateKind::S,
            GateKind::T,
            GateKind::SX,
            GateKind::SXDag,
        ] {
            out.push(gate(kind, &[], &[0], &[]));
            out.push(gate(kind, &[1], &[0], &[]));
        }
        for kind in [GateKind::Rx, GateKind::Ry, GateKind::Rz, GateKind::R1] {
            out.push(gate(kind, &[], &[0], &[0.7]));
            out.push(gate(kind, &[0], &[1], &[-1.9]));
        }
        out.push(gate(GateKind::X, &[0, 1], &[2], &[]));
        out.push(gate(GateKind::Z, &[0, 1], &[2], &[]));
        out.push(gate(GateKind::Swap, &[], &[0, 1], &[]));
        out.push(gate(GateKind::Swap, &[2], &[0, 1], &[]));
        out
    }

    #[test]
    fn presets() {
        for set in ["rz-sx-cx", "rz-ry-cz"] {
            for original in samples() {
                check(set, original);
            }
        }
    }

    #[test]
    fn custom_sets() {
        for set in ["rx,ry,cz", "h,rz,cy", "rz,sx,x,ch", "ry,rz,cx"] {
            for original in samples() {
                check(set, original);
            }
        }
    }

    #[test]
    fn clifford_t() {
        let set = "h,s,t,cx";
        for kind in [
            GateKind::X,
            GateKind::Z,
            GateKind::SDag,
            GateKind::TDag,
            GateKind::Y,
        ] {
            check(set, gate(kind, &[], &[0], &[]));
        }
        check(set, gate(GateKind::X, &[0, 1], &[2], &[]));
    }

    #[test]
    fn impossible() {
        let set = GateSet::parse("h,t,cx").unwrap();
        let synth = Synth::new(&set);
        assert!(translate(&gate(GateKind::Rz, &[], &[0], &[0.3]), &set, &synth).is_none());
        let set = GateSet::parse("rz,ry").unwrap();
        let synth = Synth::new(&set);
        assert!(translate(&gate(GateKind::X, &[0], &[1], &[]), &set, &synth).is_none());
    }

    #[test]
    fn names() {
        let set = GateSet::native().without("h,ccx").unwrap();
        assert!(!set.has(GateKind::H, 0));
        assert!(!set.has(GateKind::X, 2));
        assert!(set.has(GateKind::X, 1));
        assert_eq!(set.name(), "native without h,ccx");
        assert!(GateSet::parse("rz,bogus").is_err());
    }
}
