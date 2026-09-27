use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, FRAC_PI_4, PI};

use crate::simulator::matrix::{C64, Matrix2};
use crate::synth::Unitary;

const EPSILON: f64 = 1e-9;
const ZERO: C64 = C64::new(0.0, 0.0);
const ONE: C64 = C64::new(1.0, 0.0);
const TURNS: [f64; 8] = [0.3, 1.1, 2.0, 0.7, 1.7, 2.6, 0.05, 1.45];

#[derive(Clone, Copy, Debug)]
pub enum Piece {
    Local(usize, Matrix2),
    Cx(usize, usize),
}

impl Piece {
    fn unitary(self) -> Unitary {
        match self {
            Piece::Local(0, m) => Unitary::kron(m, Matrix2::identity()),
            Piece::Local(_, m) => Unitary::kron(Matrix2::identity(), m),
            Piece::Cx(control, target) => build(|row, column| {
                let moved = if column >> control & 1 == 1 {
                    column ^ 1 << target
                } else {
                    column
                };
                if row == moved { ONE } else { ZERO }
            }),
        }
    }
}

fn build(f: impl Fn(usize, usize) -> C64) -> Unitary {
    Unitary {
        dim: 4,
        cells: (0..16).map(|i| f(i / 4, i % 4)).collect(),
    }
}

fn transpose(u: &Unitary) -> Unitary {
    build(|row, column| u.cells[column * 4 + row])
}

fn diagonal(phases: [f64; 4]) -> Unitary {
    build(|row, column| {
        if row == column {
            C64::from_polar(1.0, phases[row])
        } else {
            ZERO
        }
    })
}

fn magic() -> Unitary {
    let (r, i) = (C64::new(FRAC_1_SQRT_2, 0.0), C64::new(0.0, FRAC_1_SQRT_2));
    let rows = [
        [r, ZERO, ZERO, i],
        [ZERO, i, r, ZERO],
        [ZERO, i, -r, ZERO],
        [r, ZERO, ZERO, -i],
    ];
    build(|row, column| rows[row][column])
}

fn pauli_pair(axis: usize) -> Unitary {
    let p = [Matrix2::x(), Matrix2::y(), Matrix2::z()][axis];
    Unitary::kron(p, p)
}

fn weights() -> [[f64; 4]; 3] {
    let m = magic();
    [0, 1, 2].map(|axis| {
        let d = m.adjoint().after(&pauli_pair(axis)).after(&m);
        [0, 1, 2, 3].map(|j| d.cells[j * 5].re)
    })
}

fn canonical(coordinates: [f64; 3]) -> Unitary {
    let w = weights();
    let m = magic();
    let phases = [0, 1, 2, 3].map(|j| (0..3).map(|k| coordinates[k] * w[k][j]).sum());
    m.after(&diagonal(phases)).after(&m.adjoint())
}

pub(crate) fn det(u: &Unitary) -> C64 {
    let mut m: Vec<C64> = u.cells.clone();
    let mut result = ONE;
    for col in 0..4 {
        let pivot = (col..4)
            .max_by(|&x, &y| m[x * 4 + col].norm().total_cmp(&m[y * 4 + col].norm()))
            .unwrap_or(col);
        if m[pivot * 4 + col].norm() < EPSILON {
            return ZERO;
        }
        if pivot != col {
            for k in 0..4 {
                m.swap(pivot * 4 + k, col * 4 + k);
            }
            result = -result;
        }
        result *= m[col * 4 + col];
        for row in col + 1..4 {
            let factor = m[row * 4 + col] / m[col * 4 + col];
            for k in col..4 {
                let above = m[col * 4 + k];
                m[row * 4 + k] -= factor * above;
            }
        }
    }
    result
}

fn jacobi(mut s: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut v = [[0.0; 4]; 4];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _ in 0..64 {
        let off: f64 = (0..4)
            .flat_map(|p| (p + 1..4).map(move |q| (p, q)))
            .map(|(p, q)| s[p][q] * s[p][q])
            .sum();
        if off < 1e-30 {
            break;
        }
        for p in 0..4 {
            for q in p + 1..4 {
                if s[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (s[q][q] - s[p][p]) / (2.0 * s[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let sn = t * c;
                for row in s.iter_mut() {
                    let (a, b) = (row[p], row[q]);
                    row[p] = c * a - sn * b;
                    row[q] = sn * a + c * b;
                }
                let (a, b) = (s[p], s[q]);
                s[p] = [0, 1, 2, 3].map(|k| c * a[k] - sn * b[k]);
                s[q] = [0, 1, 2, 3].map(|k| sn * a[k] + c * b[k]);
                for row in v.iter_mut() {
                    let (a, b) = (row[p], row[q]);
                    row[p] = c * a - sn * b;
                    row[q] = sn * a + c * b;
                }
            }
        }
    }
    v
}

struct Kak {
    after: Unitary,
    coordinates: [f64; 3],
    before: Unitary,
}

fn decompose(u: &Unitary) -> Option<Kak> {
    let root = C64::from_polar(1.0, det(u).arg() / 4.0);
    let v = build(|row, column| u.cells[row * 4 + column] / root);
    let m = magic();
    let up = m.adjoint().after(&v).after(&m);
    let squared = transpose(&up).after(&up);
    let w = weights();

    for turn in TURNS {
        let (sin, cos) = turn.sin_cos();
        let mut s = [[0.0; 4]; 4];
        for (i, row) in s.iter_mut().enumerate() {
            for (j, value) in row.iter_mut().enumerate() {
                let x = (squared.cells[i * 4 + j] + squared.cells[j * 4 + i]) * 0.5;
                *value = x.re * cos + x.im * sin;
            }
        }
        let mut vectors = jacobi(s);
        let p = build(|row, column| C64::new(vectors[row][column], 0.0));
        if det(&p).re < 0.0 {
            for row in vectors.iter_mut() {
                row[0] = -row[0];
            }
        }
        let p = build(|row, column| C64::new(vectors[row][column], 0.0));
        let d = transpose(&p).after(&squared).after(&p);
        let off = (0..16)
            .filter(|i| i % 5 != 0)
            .map(|i| d.cells[i].norm())
            .fold(0.0, f64::max);
        if off > 1e-7 {
            continue;
        }
        let mut half = [0, 1, 2, 3].map(|j| d.cells[j * 5].arg() / 2.0);
        if (half.iter().sum::<f64>() / PI).round().rem_euclid(2.0) == 1.0 {
            half[0] += PI;
        }
        let k1 = up.after(&p).after(&diagonal(half.map(|h| -h)));
        let coordinates = [0, 1, 2].map(|k| (0..4).map(|j| w[k][j] * half[j]).sum::<f64>() / 4.0);
        let kak = Kak {
            after: m.after(&k1).after(&m.adjoint()),
            coordinates,
            before: m.after(&transpose(&p)).after(&m.adjoint()),
        };
        if kak
            .after
            .after(&canonical(coordinates))
            .after(&kak.before)
            .same(u)
        {
            return Some(kak);
        }
    }
    None
}

fn locals(u: &Unitary, out: &mut Vec<Piece>) -> Option<()> {
    let (low, high) = u.split()?;
    out.push(Piece::Local(0, low));
    out.push(Piece::Local(1, high));
    Some(())
}

fn three([a, b, c]: [f64; 3]) -> [Piece; 6] {
    [
        Piece::Cx(1, 0),
        Piece::Local(1, Matrix2::rx(2.0 * a + FRAC_PI_2)),
        Piece::Local(0, Matrix2::ry(2.0 * b + FRAC_PI_2)),
        Piece::Cx(0, 1),
        Piece::Local(0, Matrix2::ry(2.0 * c + FRAC_PI_2)),
        Piece::Cx(1, 0),
    ]
}

pub fn synthesize(u: &Unitary) -> Option<Vec<Piece>> {
    if u.dim != 4 {
        return None;
    }
    let kak = decompose(u)?;
    let mut reduced = kak.coordinates;
    let mut before = kak.before;
    for (axis, value) in reduced.iter_mut().enumerate() {
        let turns = (*value / FRAC_PI_2).round();
        *value -= turns * FRAC_PI_2;
        if (turns as i64).rem_euclid(2) == 1 {
            before = pauli_pair(axis).after(&before);
        }
    }

    let zeros: Vec<usize> = (0..3).filter(|&k| reduced[k].abs() < EPSILON).collect();
    let (h, s) = (Matrix2::h(), Matrix2::s());
    let both = |m: Matrix2| Unitary::kron(m, m);

    let mut out = Vec::new();
    match zeros.as_slice() {
        [_, _, _] => locals(&kak.after.after(&before), &mut out)?,
        &[x, y] if (reduced[3 - x - y].abs() - FRAC_PI_4).abs() < EPSILON => {
            let axis = 3 - x - y;
            let frame = match axis {
                0 => both(h),
                1 => both(s.multiply(h)),
                _ => Unitary::identity(2),
            };
            let phase = if reduced[axis] > 0.0 {
                Matrix2::s_dagger()
            } else {
                s
            };
            locals(&frame.adjoint().after(&before), &mut out)?;
            out.extend([Piece::Local(1, h), Piece::Cx(0, 1), Piece::Local(1, h)]);
            locals(&kak.after.after(&frame).after(&both(phase)), &mut out)?;
        }
        [missing, ..] => {
            let (frame, first, second) = match missing {
                0 => (both(s), reduced[1], reduced[2]),
                1 => (Unitary::identity(2), reduced[0], reduced[2]),
                _ => (both(Matrix2::rx(FRAC_PI_2)), reduced[0], reduced[1]),
            };
            locals(&frame.adjoint().after(&before), &mut out)?;
            out.extend([
                Piece::Cx(0, 1),
                Piece::Local(0, Matrix2::rx(-2.0 * first)),
                Piece::Local(1, Matrix2::rz(-2.0 * second)),
                Piece::Cx(0, 1),
            ]);
            locals(&kak.after.after(&frame), &mut out)?;
        }
        [] => {
            let fix = Unitary::kron(Matrix2::sx(), Matrix2::z());
            let tail = fix.adjoint().after(&product(&three([0.0; 3])));
            locals(&tail.adjoint().after(&before), &mut out)?;
            out.extend(three(reduced));
            locals(&kak.after.after(&fix.adjoint()), &mut out)?;
        }
    }

    let merged = merge(out);
    product(&merged).same(u).then_some(merged)
}

fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for piece in pieces {
        if let Piece::Local(q, m) = piece {
            let earlier = out.iter().rposition(|p| match p {
                Piece::Local(r, _) => *r == q,
                Piece::Cx(..) => true,
            });
            if let Some(i) = earlier
                && let Piece::Local(_, e) = out[i]
            {
                out[i] = Piece::Local(q, m.multiply(e));
                continue;
            }
        }
        out.push(piece);
    }
    out.retain(|p| match p {
        Piece::Local(_, m) => {
            m.b.norm() > EPSILON || m.c.norm() > EPSILON || (m.a - m.d).norm() > EPSILON
        }
        Piece::Cx(..) => true,
    });
    out
}

fn product(pieces: &[Piece]) -> Unitary {
    pieces.iter().fold(Unitary::identity(2), |acc, piece| {
        piece.unitary().after(&acc)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(pieces: &[Piece]) -> usize {
        pieces.iter().filter(|p| matches!(p, Piece::Cx(..))).count()
    }

    fn random(seed: u64) -> Unitary {
        let mut state = seed;
        let mut angle = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 11) as f64 / (1u64 << 53) as f64 * 6.0 - 3.0
        };
        let mut euler = || {
            Matrix2::rz(angle())
                .multiply(Matrix2::ry(angle()))
                .multiply(Matrix2::rz(angle()))
        };
        let mut u = Unitary::identity(2);
        for layer in 0..4 {
            u = Unitary::kron(euler(), euler()).after(&u);
            if layer < 3 {
                u = Piece::Cx(layer % 2, 1 - layer % 2).unitary().after(&u);
            }
        }
        u
    }

    #[test]
    fn random_gates() {
        for seed in 0..300 {
            let pieces = synthesize(&random(seed)).unwrap_or_else(|| panic!("seed {seed}"));
            assert!(count(&pieces) <= 3);
        }
    }

    #[test]
    fn clifford_words() {
        let letters = [
            Piece::Local(0, Matrix2::h()),
            Piece::Local(1, Matrix2::h()),
            Piece::Local(0, Matrix2::t()),
            Piece::Local(1, Matrix2::s()),
            Piece::Cx(0, 1),
            Piece::Cx(1, 0),
        ];
        let mut state = 7u64;
        for _ in 0..3000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let length = 1 + (state >> 60) as usize;
            let word: Vec<Piece> = (0..length)
                .map(|i| letters[(state >> (i * 3 % 57)) as usize % letters.len()])
                .collect();
            let pieces = synthesize(&product(&word)).expect("synthesis");
            assert!(count(&pieces) <= count(&word).min(3));
        }
    }

    #[test]
    fn minimal_counts() {
        let cx = |c, t| Piece::Cx(c, t).unitary();
        let h0 = Unitary::kron(Matrix2::h(), Matrix2::identity());
        let cases = [
            (Unitary::kron(Matrix2::rx(0.3), Matrix2::ry(1.2)), 0),
            (cx(0, 1), 1),
            (h0.after(&cx(0, 1)).after(&h0), 1),
            (
                cx(1, 0)
                    .after(&Unitary::kron(Matrix2::rz(0.4), Matrix2::rx(0.9)))
                    .after(&cx(0, 1)),
                2,
            ),
            (canonical([FRAC_PI_4, FRAC_PI_4, 0.0]), 2),
            (canonical([0.2, 0.0, -0.7]), 2),
            (canonical([0.0, -FRAC_PI_4, 0.0]), 1),
            (cx(0, 1).after(&cx(1, 0)).after(&cx(0, 1)), 3),
        ];
        for (u, expected) in cases {
            assert_eq!(count(&synthesize(&u).expect("synthesis")), expected);
        }
    }
}
