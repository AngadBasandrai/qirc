use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use crate::ir::*;
use crate::transpile::{self, GateSet};

const MAX_TERMS: usize = 1 << 16;
const NEGLIGIBLE: f64 = 1e-12;

fn bit(key: &[u64], index: usize) -> bool {
    key[index / 64] >> (index % 64) & 1 == 1
}

fn xz(words: usize, key: &[u64], q: usize) -> (bool, bool) {
    (bit(key, q), bit(key, 64 * words + q))
}

fn set(words: usize, key: &mut [u64], q: usize, x: bool, z: bool) {
    for (index, value) in [(q, x), (64 * words + q, z)] {
        let mask = 1u64 << (index % 64);
        if value {
            key[index / 64] |= mask;
        } else {
            key[index / 64] &= !mask;
        }
    }
}

struct Terms {
    words: usize,
    map: HashMap<Vec<u64>, f64>,
}

impl Terms {
    fn rotate(&mut self, q: usize, axis: (bool, bool), angle: f64) {
        let (gx, gz) = axis;
        let words = self.words;
        let flipped: Vec<(Vec<u64>, f64)> = self
            .map
            .iter()
            .filter(|(key, _)| {
                let (x, z) = xz(words, key, q);
                (x & gz) ^ (z & gx)
            })
            .map(|(key, &c)| (key.clone(), c))
            .collect();
        let (sin, cos) = angle.sin_cos();
        for (key, c) in &flipped {
            self.map.insert(key.clone(), c * cos);
        }
        let index = |x: bool, z: bool| match (x, z) {
            (true, false) => 0,
            (true, true) => 1,
            _ => 2,
        };
        for (mut key, c) in flipped {
            let (x, z) = xz(words, &key, q);
            let sign = if (index(gx, gz) + 3 - index(x, z)) % 3 == 1 {
                1.0
            } else {
                -1.0
            };
            set(words, &mut key, q, x ^ gx, z ^ gz);
            *self.map.entry(key).or_insert(0.0) += sign * sin * c;
        }
    }

    fn cx(&mut self, control: usize, target: usize) {
        let words = self.words;
        let moved: Vec<(Vec<u64>, f64)> = self
            .map
            .extract_if(|key, _| xz(words, key, control).0 || xz(words, key, target).1)
            .collect();
        for (mut key, c) in moved {
            let (xc, zc) = xz(words, &key, control);
            let (xt, zt) = xz(words, &key, target);
            set(words, &mut key, control, xc, zc ^ zt);
            set(words, &mut key, target, xt ^ xc, zt);
            self.map
                .insert(key, if xc && zt && xt == zc { -c } else { c });
        }
    }
}

pub(crate) fn propagate(program: &Program, paulis: &[(usize, bool, bool)]) -> Result<f64, String> {
    let words = (program.num_qubits as usize).div_ceil(64).max(1);
    let mut lowered = program.clone();
    transpile::transpile(&mut lowered, &GateSet::parse("rz-sx-cx")?);
    let mut terms = Terms {
        words,
        map: HashMap::new(),
    };
    let mut start = vec![0; 2 * words];
    for &(q, x, z) in paulis {
        set(words, &mut start, q, x, z);
    }
    terms.map.insert(start, 1.0);
    let gates: Vec<&Gate> = lowered.gates().collect();
    for gate in gates.into_iter().rev() {
        let target = gate.targets[0].index();
        if let (GateKind::X, [control]) = (gate.kind, &gate.controls[..]) {
            terms.cx(control.index(), target);
            continue;
        }
        let angle = || {
            gate.constant_angle().ok_or(
                "cannot propagate a Pauli through a rotation whose angle is only known at run time",
            )
        };
        let (x, y, z) = ((true, false), (true, true), (false, true));
        let (axis, turn) = match (gate.kind, gate.controls.len()) {
            (GateKind::I, 0) => continue,
            (GateKind::Rz | GateKind::R1, 0) => (z, angle()?),
            (GateKind::Rx, 0) => (x, angle()?),
            (GateKind::Ry, 0) => (y, angle()?),
            (GateKind::X, 0) => (x, PI),
            (GateKind::Y, 0) => (y, PI),
            (GateKind::Z, 0) => (z, PI),
            (GateKind::SX, 0) => (x, FRAC_PI_2),
            (GateKind::SXDag, 0) => (x, -FRAC_PI_2),
            (GateKind::S, 0) => (z, FRAC_PI_2),
            (GateKind::SDag, 0) => (z, -FRAC_PI_2),
            (GateKind::T, 0) => (z, FRAC_PI_4),
            (GateKind::TDag, 0) => (z, -FRAC_PI_4),
            (kind, _) => {
                return Err(format!(
                    "cannot propagate a Pauli through `{}`",
                    kind.name()
                ));
            }
        };
        terms.rotate(target, axis, turn);
        terms.map.retain(|_, c| c.abs() >= NEGLIGIBLE);
        if terms.map.len() > MAX_TERMS {
            return Err(format!(
                "the light cone of a term is too wide for a state vector and its Pauli expansion grew past {MAX_TERMS} strings"
            ));
        }
    }
    Ok(terms
        .map
        .iter()
        .filter(|(key, _)| key[..words].iter().all(|&w| w == 0))
        .map(|(_, c)| c)
        .sum())
}
