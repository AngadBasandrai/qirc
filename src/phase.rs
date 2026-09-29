use std::cmp::Ordering;
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use crate::ir::*;

const EPSILON: f64 = 1e-12;

struct Term {
    at: usize,
    flipped: bool,
    angle: f64,
}

fn slope(gate: &Gate) -> Option<f64> {
    if !gate.controls.is_empty() {
        return None;
    }
    match gate.kind {
        GateKind::Z => Some(PI),
        GateKind::S => Some(FRAC_PI_2),
        GateKind::SDag => Some(-FRAC_PI_2),
        GateKind::T => Some(FRAC_PI_4),
        GateKind::TDag => Some(-FRAC_PI_4),
        GateKind::Rz | GateKind::R1 => gate.params.first()?.constant().map(Const::as_f64),
        _ => None,
    }
}

pub(crate) fn diagonal(kind: GateKind) -> bool {
    matches!(
        kind,
        GateKind::I
            | GateKind::Z
            | GateKind::S
            | GateKind::SDag
            | GateKind::T
            | GateKind::TDag
            | GateKind::Rz
            | GateKind::R1
    )
}

fn xor(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

pub(crate) fn wrap(angle: f64) -> f64 {
    let turn = 2.0 * PI;
    let wrapped = angle.rem_euclid(turn);
    if wrapped > PI {
        wrapped - turn
    } else {
        wrapped
    }
}

pub(crate) fn rewrite(gate: &mut Gate, angle: f64) {
    let named = [
        (PI, GateKind::Z),
        (-PI, GateKind::Z),
        (FRAC_PI_2, GateKind::S),
        (-FRAC_PI_2, GateKind::SDag),
        (FRAC_PI_4, GateKind::T),
        (-FRAC_PI_4, GateKind::TDag),
    ];
    if let Some(&(_, kind)) = named.iter().find(|(a, _)| (a - angle).abs() < 1e-9) {
        gate.kind = kind;
        gate.params.clear();
    } else {
        if !matches!(gate.kind, GateKind::Rz | GateKind::R1) {
            gate.kind = GateKind::Rz;
        }
        gate.params = vec![Operand::Const(Const::Float(angle))];
    }
}

fn fold_block(ops: &mut Vec<Op>, qubits: usize) -> usize {
    let mut fresh = qubits as u32;
    let mut parity: Vec<(Vec<u32>, bool)> = (0..qubits as u32).map(|q| (vec![q], false)).collect();
    let mut terms: HashMap<Vec<u32>, Term> = HashMap::new();
    let mut finished = Vec::new();
    let mut keep = vec![true; ops.len()];
    let mut renew = |parity: &mut Vec<(Vec<u32>, bool)>, q: usize| {
        parity[q] = (vec![fresh], false);
        fresh += 1;
    };

    for (index, op) in ops.iter().enumerate() {
        let wires = op.qubits();
        let tracked = match op {
            Op::Gate(gate) => match (
                slope(gate),
                gate.kind,
                &gate.controls[..],
                &gate.targets[..],
            ) {
                (Some(slope), ..) => {
                    let (p, flipped) = &parity[gate.targets[0].index()];
                    let signed = if *flipped { -slope } else { slope };
                    if p.is_empty() {
                        keep[index] = false;
                    } else if let Some(term) = terms.get_mut(p) {
                        term.angle += signed;
                        keep[index] = false;
                    } else {
                        terms.insert(
                            p.clone(),
                            Term {
                                at: index,
                                flipped: *flipped,
                                angle: signed,
                            },
                        );
                    }
                    true
                }
                (None, kind, ..) if diagonal(kind) => true,
                (None, GateKind::X, [], [t]) => {
                    parity[t.index()].1 ^= true;
                    true
                }
                (None, GateKind::X, [c], [t]) => {
                    let control = parity[c.index()].clone();
                    let target = &mut parity[t.index()];
                    target.0 = xor(&target.0, &control.0);
                    target.1 ^= control.1;
                    true
                }
                (None, GateKind::Swap, [], [a, b]) => {
                    parity.swap(a.index(), b.index());
                    true
                }
                (None, GateKind::H, [], [t]) => {
                    renew(&mut parity, t.index());
                    true
                }
                _ => false,
            },
            _ => wires.is_empty(),
        };
        if !tracked {
            finished.extend(terms.drain().map(|(_, term)| term));
            for q in wires {
                renew(&mut parity, q.index());
            }
        }
    }
    finished.extend(terms.into_values());

    for term in finished {
        let angle = wrap(if term.flipped {
            -term.angle
        } else {
            term.angle
        });
        if angle.abs() < EPSILON {
            keep[term.at] = false;
        } else if let Op::Gate(gate) = &mut ops[term.at]
            && slope(gate).is_none_or(|s| (wrap(s) - angle).abs() > EPSILON)
        {
            rewrite(gate, angle);
        }
    }

    let removed = keep.iter().filter(|k| !**k).count();
    keep_marked(ops, &keep);
    removed
}

pub(crate) fn fold(program: &mut Program) -> usize {
    let qubits = program.num_qubits as usize;
    program
        .blocks
        .iter_mut()
        .map(|block| fold_block(&mut block.ops, qubits))
        .sum()
}
