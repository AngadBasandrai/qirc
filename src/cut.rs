use crate::ir::*;
use crate::simulator::exec::Backend;
use crate::simulator::matrix::{C64, Matrix2, matrix_for};
use crate::simulator::state::{self, State};
use crate::transpile::{self, GateSet};

pub(crate) type Weighted = (f64, Vec<(usize, bool, bool)>);

const MAX_BYTES: u64 = 1 << 31;
const MAX_WORK: f64 = 4e9;

enum Step {
    Gate(Gate, Vec<f64>),
    Factor(usize, usize),
}

struct Piece {
    qubits: usize,
    steps: Vec<Step>,
}

struct Tree<'a> {
    piece: &'a Piece,
    cuts: &'a [Vec<(Matrix2, Matrix2)>],
    strides: Vec<usize>,
    right: bool,
    leaves: Vec<(usize, State)>,
}

impl Tree<'_> {
    fn grow(&mut self, from: usize, mut state: State, index: usize) {
        for (at, step) in self.piece.steps.iter().enumerate().skip(from) {
            match step {
                Step::Gate(gate, params) => state.gate(gate, params),
                Step::Factor(cut, wire) => {
                    let last = self.cuts[*cut].len() - 1;
                    for digit in 0..last {
                        self.branch(at, state.clone(), (*cut, *wire, digit), index);
                    }
                    self.branch(at, state, (*cut, *wire, last), index);
                    return;
                }
            }
        }
        self.leaves.push((index, state));
    }

    fn branch(
        &mut self,
        at: usize,
        mut state: State,
        (cut, wire, digit): (usize, usize, usize),
        index: usize,
    ) {
        let (left, right) = self.cuts[cut][digit];
        state.apply(if self.right { &right } else { &left }, wire, 0);
        self.grow(at + 1, state, index + digit * self.strides[cut]);
    }
}

fn crossing(weight: &[Vec<usize>], side: &[bool]) -> usize {
    let mut total = 0;
    for a in 0..side.len() {
        for b in a + 1..side.len() {
            if side[a] != side[b] {
                total += weight[a][b];
            }
        }
    }
    total
}

fn split(qubits: usize, pairs: &[(usize, usize)], limit: usize) -> Option<Vec<bool>> {
    let smallest = qubits.saturating_sub(limit).max(1);
    if smallest > limit.min(qubits - 1) {
        return None;
    }
    let mut weight = vec![vec![0; qubits]; qubits];
    for &(a, b) in pairs {
        weight[a][b] += 1;
        weight[b][a] += 1;
    }
    let fits = |side: &[bool]| {
        let right = side.iter().filter(|&&s| s).count();
        (smallest..=limit).contains(&right) && (smallest..=limit).contains(&(qubits - right))
    };
    let mut side = (smallest..=limit.min(qubits - 1))
        .map(|at| (0..qubits).map(|q| q >= qubits - at).collect::<Vec<bool>>())
        .filter(|side| fits(side))
        .min_by_key(|side| crossing(&weight, side))?;
    let mut current = crossing(&weight, &side);
    loop {
        let mut moves: Vec<Vec<usize>> = (0..qubits).map(|q| vec![q]).collect();
        for a in 0..qubits {
            for b in a + 1..qubits {
                if side[a] != side[b] {
                    moves.push(vec![a, b]);
                }
            }
        }
        let better = moves.into_iter().find_map(|flips| {
            let mut trial = side.clone();
            for q in flips {
                trial[q] = !trial[q];
            }
            let cost = crossing(&weight, &trial);
            (fits(&trial) && cost < current).then_some((trial, cost))
        });
        match better {
            Some((trial, cost)) => {
                side = trial;
                current = cost;
            }
            None => return Some(side),
        }
    }
}

fn factors(gate: &Gate, params: &[f64], side: &[bool]) -> Option<Vec<(Matrix2, Matrix2)>> {
    let half = |m: Matrix2| {
        let s = C64::new(0.5, 0.0);
        Matrix2::new(m.a * s, m.b * s, m.c * s, m.d * s)
    };
    let zero = Matrix2::new(
        C64::new(1.0, 0.0),
        C64::default(),
        C64::default(),
        C64::default(),
    );
    let one = Matrix2::new(
        C64::default(),
        C64::default(),
        C64::default(),
        C64::new(1.0, 0.0),
    );
    let (terms, first) = match (gate.kind, &gate.controls[..], &gate.targets[..]) {
        (GateKind::Swap, [], [a, _]) => (
            [
                Matrix2::identity(),
                Matrix2::x(),
                Matrix2::y(),
                Matrix2::z(),
            ]
            .into_iter()
            .map(|p| (half(p), p))
            .collect(),
            *a,
        ),
        (kind, [c], [_]) if kind != GateKind::Swap => (
            vec![(zero, Matrix2::identity()), (one, matrix_for(kind, params))],
            *c,
        ),
        _ => return None,
    };
    Some(if side[first.index()] {
        terms.into_iter().map(|(a, b)| (b, a)).collect()
    } else {
        terms
    })
}

fn states(piece: &Piece, cuts: &[Vec<(Matrix2, Matrix2)>], right: bool) -> Vec<State> {
    let mut strides = Vec::with_capacity(cuts.len());
    let mut stride = 1;
    for terms in cuts {
        strides.push(stride);
        stride *= terms.len();
    }
    let mut tree = Tree {
        piece,
        cuts,
        strides,
        right,
        leaves: Vec::new(),
    };
    tree.grow(0, State::new(piece.qubits), 0);
    tree.leaves.sort_by_key(|(index, _)| *index);
    tree.leaves.into_iter().map(|(_, state)| state).collect()
}

pub(crate) fn expectation(program: &Program, terms: &[Weighted]) -> Result<f64, String> {
    let mut program = program.clone();
    if program.gates().any(|gate| gate.wires().count() > 2) {
        transpile::transpile(&mut program, &GateSet::native().local());
    }
    let qubits = program.num_qubits as usize;
    let mut gates = Vec::new();
    for gate in program.gates() {
        let params: Option<Vec<f64>> = gate
            .params
            .iter()
            .map(|param| param.constant().map(Const::as_f64))
            .collect();
        let params = params.ok_or("cutting needs every angle known at compile time")?;
        gates.push((gate.clone(), params));
    }
    let pairs: Vec<(usize, usize)> = gates
        .iter()
        .filter_map(
            |(gate, _)| match gate.wires().map(|q| q.index()).collect::<Vec<_>>()[..] {
                [a, b] => Some((a, b)),
                _ => None,
            },
        )
        .collect();
    let side = split(qubits, &pairs, state::MAX_QUBITS)
        .ok_or_else(|| format!("{qubits} qubits cannot be cut into two state vectors"))?;

    let mut local = vec![0; qubits];
    let mut sizes = [0; 2];
    for q in 0..qubits {
        let piece = usize::from(side[q]);
        local[q] = sizes[piece];
        sizes[piece] += 1;
    }
    let mut pieces: Vec<Piece> = sizes
        .iter()
        .map(|&qubits| Piece {
            qubits,
            steps: Vec::new(),
        })
        .collect();
    let mut cuts = Vec::new();
    for (gate, params) in gates {
        let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
        let sides: Vec<bool> = wires.iter().map(|&q| side[q]).collect();
        if sides.iter().all(|&s| s == sides[0]) {
            let mut moved = gate.clone();
            for q in moved.controls.iter_mut().chain(moved.targets.iter_mut()) {
                *q = QubitId(local[q.index()] as u32);
            }
            pieces[usize::from(sides[0])]
                .steps
                .push(Step::Gate(moved, params));
            continue;
        }
        let terms = factors(&gate, &params, &side)
            .ok_or_else(|| format!("cannot cut through `{}`", gate.kind.name()))?;
        for &q in &wires {
            pieces[usize::from(side[q])]
                .steps
                .push(Step::Factor(cuts.len(), local[q]));
        }
        cuts.push(terms);
    }
    let count = cuts
        .iter()
        .map(Vec::len)
        .try_fold(1usize, |total, n| total.checked_mul(n));
    let amplitudes = (1u64 << sizes[0]) + (1u64 << sizes[1]);
    let too_big = || {
        format!(
            "cutting it into {} and {} qubits takes {} cuts, too many to recombine",
            sizes[0],
            sizes[1],
            cuts.len()
        )
    };
    let count = count.ok_or_else(too_big)?;
    let work = (count as f64).powi(2) / 2.0 * amplitudes as f64;
    if (count as u64).saturating_mul(amplitudes).saturating_mul(16) > MAX_BYTES || work > MAX_WORK {
        return Err(too_big());
    }

    let [left, right] =
        [false, true].map(|right| states(&pieces[usize::from(right)], &cuts, right));
    let mut total = 0.0;
    for (weight, paulis) in terms {
        let mut masks = [(0, 0); 2];
        for &(q, flips, phases) in paulis {
            let (x, z) = &mut masks[usize::from(side[q])];
            *x |= usize::from(flips) << local[q];
            *z |= usize::from(phases) << local[q];
        }
        let [(left_x, left_z), (right_x, right_z)] = masks;
        let mut sum = 0.0;
        for ket in 0..count {
            for bra in 0..=ket {
                let product = left[bra].pauli_element(&left[ket], left_x, left_z)
                    * right[bra].pauli_element(&right[ket], right_x, right_z);
                sum += if bra == ket {
                    product.re
                } else {
                    2.0 * product.re
                };
            }
        }
        total += weight * sum;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;
    use crate::simulator::exec;
    use crate::simulator::state::Rng;

    const LOCAL: [&str; 6] = ["h", "t", "sdg", "rx(0.3)", "ry(0.7)", "u3(0.2,0.9,1.4)"];
    const CROSSING: [&str; 5] = ["cx", "cz", "crz(0.4)", "swap", "ch"];

    fn pick<'a>(rng: &mut Rng, items: &[&'a str]) -> &'a str {
        items[rng.next_u64() as usize % items.len()]
    }

    #[test]
    fn recombines() {
        let mut rng = Rng::new(5);
        for round in 0..40 {
            let mut text = "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[6];\n".to_string();
            for step in 0..24 {
                let a = rng.next_u64() as usize % 3 + 3 * (step % 2);
                if step % 8 == 7 {
                    let b = rng.next_u64() as usize % 3 + 3 * (1 - step % 2);
                    text += &format!("{} q[{a}], q[{b}];\n", pick(&mut rng, &CROSSING));
                } else if step % 3 == 0 {
                    let b = (a + 1) % 3 + 3 * (step % 2);
                    text += &format!("cx q[{a}], q[{b}];\n");
                } else {
                    text += &format!("{} q[{a}];\n", pick(&mut rng, &LOCAL));
                }
            }
            if round % 4 == 0 {
                text += "ccx q[0], q[4], q[2];\n";
            }
            let (program, errors) = qasm::lower(&text);
            assert!(errors.is_empty(), "{errors:?}");
            let state = exec::finish(&program, State::new(6));
            let mut terms = Vec::new();
            let mut exact = 0.0;
            for weight in [1.0, -0.5] {
                let paulis: Vec<(usize, bool, bool)> = (0..6)
                    .filter_map(|q| match rng.next_u64() % 4 {
                        0 => None,
                        1 => Some((q, true, false)),
                        2 => Some((q, true, true)),
                        _ => Some((q, false, true)),
                    })
                    .collect();
                let (mut x, mut z) = (0, 0);
                for &(q, flips, phases) in &paulis {
                    x |= usize::from(flips) << q;
                    z |= usize::from(phases) << q;
                }
                exact += weight * state.pauli_element(&state, x, z).re;
                terms.push((weight, paulis));
            }
            let value = expectation(&program, &terms).unwrap();
            assert!(
                (value - exact).abs() < 1e-9,
                "{value} against {exact}\n{text}"
            );
        }
    }

    #[test]
    fn splits() {
        let pairs: Vec<(usize, usize)> = (0..39)
            .map(|q| (q, q + 1))
            .chain([(0, 39), (5, 25)])
            .collect();
        let side = split(40, &pairs, 30).unwrap();
        let weight = {
            let mut weight = vec![vec![0; 40]; 40];
            for &(a, b) in &pairs {
                weight[a][b] += 1;
                weight[b][a] += 1;
            }
            weight
        };
        assert_eq!(crossing(&weight, &side), 2, "{side:?}");
        assert!(split(70, &pairs, 30).is_none());
    }
}
