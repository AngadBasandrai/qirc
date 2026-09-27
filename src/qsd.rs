use crate::diag::Span;
use crate::ir::{Gate, GateKind, QubitId};
use crate::kak::{self, Piece};
use crate::simulator::matrix::{C64, Matrix2};
use crate::synth::Unitary;

const SMALL: f64 = 1e-7;
const ZERO: C64 = C64::new(0.0, 0.0);
const ONE: C64 = C64::new(1.0, 0.0);
const TURNS: [f64; 6] = [0.3, 1.1, 2.0, 0.7, 1.7, 2.6];
const WIRES: [QubitId; 3] = [QubitId(0), QubitId(1), QubitId(2)];

pub const MAX_CX: usize = 21;

fn square(f: impl Fn(usize, usize) -> C64) -> Unitary {
    Unitary {
        dim: 4,
        cells: (0..16).map(|i| f(i / 4, i % 4)).collect(),
    }
}

fn block(u: &Unitary, row: usize, column: usize) -> Unitary {
    square(|r, c| u.cells[(row * 4 + r) * 8 + column * 4 + c])
}

fn diagonal(values: &[C64; 4]) -> Unitary {
    square(|r, c| if r == c { values[r] } else { ZERO })
}

fn column(u: &Unitary, c: usize) -> [C64; 4] {
    [0, 1, 2, 3].map(|r| u.cells[r * 4 + c])
}

fn norm(v: &[C64; 4]) -> f64 {
    v.iter().map(|x| x.norm_sqr()).sum::<f64>().sqrt()
}

fn hermitian(h: &Unitary) -> (Unitary, [f64; 4]) {
    let mut a: Vec<C64> = h.cells.clone();
    let mut v = Unitary::identity(2).cells;
    let at = |i: usize, j: usize| i * 4 + j;
    for _ in 0..100 {
        let off: f64 = (0..4)
            .flat_map(|p| (p + 1..4).map(move |q| (p, q)))
            .map(|(p, q)| a[at(p, q)].norm_sqr())
            .sum();
        if off < 1e-30 {
            break;
        }
        for p in 0..4 {
            for q in p + 1..4 {
                let r = a[at(p, q)].norm();
                if r < 1e-300 {
                    continue;
                }
                let theta = (a[at(q, q)].re - a[at(p, p)].re) / (2.0 * r);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                let e = C64::from_polar(1.0, -a[at(p, q)].arg());
                let (gpp, gpq, gqp, gqq) = (C64::new(c, 0.0), C64::new(s, 0.0), e * -s, e * c);
                for m in [&mut a, &mut v] {
                    for k in 0..4 {
                        let (kp, kq) = (m[at(k, p)], m[at(k, q)]);
                        m[at(k, p)] = kp * gpp + kq * gqp;
                        m[at(k, q)] = kp * gpq + kq * gqq;
                    }
                }
                for k in 0..4 {
                    let (pk, qk) = (a[at(p, k)], a[at(q, k)]);
                    a[at(p, k)] = gpp.conj() * pk + gqp.conj() * qk;
                    a[at(q, k)] = gpq.conj() * pk + gqq.conj() * qk;
                }
            }
        }
    }
    let values = [0, 1, 2, 3].map(|i| a[at(i, i)].re);
    (Unitary { dim: 4, cells: v }, values)
}

fn eigen(m: &Unitary) -> Option<(Unitary, [C64; 4])> {
    let adjoint = m.adjoint();
    for turn in TURNS {
        let (sin, cos) = turn.sin_cos();
        let mixed = square(|r, c| {
            let (x, y) = (m.cells[r * 4 + c], adjoint.cells[r * 4 + c]);
            (x + y) * (0.5 * cos) + (x - y) * C64::new(0.0, -0.5 * sin)
        });
        let (vectors, _) = hermitian(&mixed);
        let d = vectors.adjoint().after(m).after(&vectors);
        let off = (0..16)
            .filter(|i| i % 5 != 0)
            .map(|i| d.cells[i].norm())
            .fold(0.0, f64::max);
        if off < 1e-10 {
            return Some((vectors, [0, 1, 2, 3].map(|i| d.cells[i * 5])));
        }
    }
    None
}

fn complete(columns: &mut [Option<[C64; 4]>; 4]) {
    for i in 0..4 {
        if columns[i].is_some() {
            continue;
        }
        let mut best = ([ZERO; 4], 0.0);
        for axis in 0..4 {
            let mut v = [ZERO; 4];
            v[axis] = ONE;
            for other in columns.iter().flatten() {
                let dot: C64 = (0..4).map(|k| other[k].conj() * v[k]).sum();
                for k in 0..4 {
                    v[k] -= other[k] * dot;
                }
            }
            let n = norm(&v);
            if n > best.1 {
                best = (v.map(|x| x / n), n);
            }
        }
        columns[i] = Some(best.0);
    }
}

fn from_columns(columns: [Option<[C64; 4]>; 4]) -> Unitary {
    let columns = columns.map(Option::unwrap_or_default);
    square(|r, c| columns[c][r])
}

struct Csd {
    left: [Unitary; 2],
    angles: [f64; 4],
    right: [Unitary; 2],
}

fn csd(u: &Unitary) -> Csd {
    let (u00, u01, u10, u11) = (
        block(u, 0, 0),
        block(u, 0, 1),
        block(u, 1, 0),
        block(u, 1, 1),
    );
    let (r0, _) = hermitian(&u00.adjoint().after(&u00));
    let (x0, x1) = (u00.after(&r0), u10.after(&r0));
    let mut l0 = [None; 4];
    let mut l1 = [None; 4];
    let mut cosines = [0.0; 4];
    let mut sines = [0.0; 4];
    for i in 0..4 {
        let (a, b) = (column(&x0, i), column(&x1, i));
        cosines[i] = norm(&a);
        sines[i] = norm(&b);
        if cosines[i] > SMALL {
            l0[i] = Some(a.map(|x| x / cosines[i]));
        }
        if sines[i] > SMALL {
            l1[i] = Some(b.map(|x| x / sines[i]));
        }
    }
    complete(&mut l0);
    complete(&mut l1);
    let (l0, l1) = (from_columns(l0), from_columns(l1));
    let (top, bottom) = (l1.adjoint().after(&u11), l0.adjoint().after(&u01));
    let r1 = square(|r, c| {
        if cosines[r] >= sines[r] {
            top.cells[r * 4 + c] / cosines[r]
        } else {
            -bottom.cells[r * 4 + c] / sines[r]
        }
    });
    Csd {
        left: [l0, l1],
        angles: [0, 1, 2, 3].map(|i| 2.0 * sines[i].atan2(cosines[i])),
        right: [r0.adjoint(), r1],
    }
}

fn demultiplex([a, b]: &[Unitary; 2]) -> Option<(Unitary, [f64; 4], Unitary)> {
    let (v, values) = eigen(&a.after(&b.adjoint()))?;
    let roots = values.map(|x| C64::from_polar(1.0, x.arg() / 2.0));
    let w = diagonal(&roots).after(&v.adjoint()).after(b);
    Some((v, roots.map(|d| -2.0 * d.arg()), w))
}

fn up_to_diagonal(u: &Unitary) -> (Unitary, Unitary) {
    let yy = square(|r, c| match (r, c) {
        (0, 3) | (3, 0) => -ONE,
        (1, 2) | (2, 1) => ONE,
        _ => ZERO,
    });
    let transpose = square(|r, c| u.cells[c * 4 + r]);
    let m = u.after(&yy).after(&transpose);
    let scale = C64::from_polar(1.0, -kak::det(u).arg() / 2.0);
    let a = (m.cells[6] + m.cells[9]) * scale;
    let b = (m.cells[3] + m.cells[12]) * scale;
    let psi = 2.0 * (a.im - b.im).atan2(a.re + b.re);
    let fix = [ONE, ONE, ONE, C64::from_polar(1.0, psi)];
    let undo = fix.map(|x| x.conj());
    (diagonal(&fix).after(u), diagonal(&undo))
}

fn multiplexed(rotation: fn(f64) -> Matrix2, angles: [f64; 4]) -> [Piece; 8] {
    let sign = |k: usize, j: usize| {
        let (b0, b1) = (k & 1, k >> 1 & 1);
        let flips = [0, b0, b0 ^ b1, b1][j];
        if flips == 1 { -1.0 } else { 1.0 }
    };
    let a = [0, 1, 2, 3].map(|j| (0..4).map(|k| sign(k, j) * angles[k]).sum::<f64>() / 4.0);
    [
        Piece::Local(2, rotation(a[0])),
        Piece::Cx(0, 2),
        Piece::Local(2, rotation(a[1])),
        Piece::Cx(1, 2),
        Piece::Local(2, rotation(a[2])),
        Piece::Cx(0, 2),
        Piece::Local(2, rotation(a[3])),
        Piece::Cx(1, 2),
    ]
}

fn product(pieces: &[Piece]) -> Option<Unitary> {
    let gates: Vec<Gate> = pieces
        .iter()
        .map(|piece| match *piece {
            Piece::Local(q, m) => Gate {
                kind: GateKind::Unitary(m.to_ir()),
                controls: Vec::new(),
                targets: vec![WIRES[q]],
                params: Vec::new(),
                span: Span::DUMMY,
            },
            Piece::Cx(c, t) => Gate {
                kind: GateKind::X,
                controls: vec![WIRES[c]],
                targets: vec![WIRES[t]],
                params: Vec::new(),
                span: Span::DUMMY,
            },
        })
        .collect();
    Unitary::of(&gates, &WIRES)
}

pub fn synthesize(u: &Unitary) -> Option<Vec<Piece>> {
    if u.dim != 8 {
        return None;
    }
    let Csd {
        left,
        angles,
        right,
    } = csd(u);
    let (v_right, phases_right, w_right) = demultiplex(&right)?;
    let (v_left, phases_left, w_left) = demultiplex(&left)?;

    let (first, carry) = up_to_diagonal(&w_right);
    let (second, carry) = up_to_diagonal(&v_right.after(&carry));
    let (third, carry) = up_to_diagonal(&w_left.after(&carry));
    let fourth = v_left.after(&carry);

    let mut pieces = Vec::new();
    for (index, unitary) in [first, second, third, fourth].iter().enumerate() {
        pieces.extend(kak::synthesize(unitary)?);
        match index {
            0 => pieces.extend(multiplexed(Matrix2::rz, phases_right)),
            1 => pieces.extend(multiplexed(Matrix2::ry, angles)),
            2 => pieces.extend(multiplexed(Matrix2::rz, phases_left)),
            _ => {}
        }
    }
    product(&pieces)?.same(u).then_some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cnots(pieces: &[Piece]) -> usize {
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
        let mut pieces = Vec::new();
        for layer in 0..9 {
            for q in 0..3 {
                pieces.push(Piece::Local(
                    q,
                    Matrix2::rz(angle())
                        .multiply(Matrix2::ry(angle()))
                        .multiply(Matrix2::rz(angle())),
                ));
            }
            pieces.push(Piece::Cx(layer % 3, (layer + 1) % 3));
        }
        product(&pieces).unwrap()
    }

    #[test]
    fn random_three() {
        for seed in 0..60 {
            let pieces = synthesize(&random(seed)).unwrap_or_else(|| panic!("seed {seed}"));
            assert!(cnots(&pieces) <= MAX_CX);
        }
    }

    #[test]
    fn clifford_words() {
        let letters = [
            Piece::Local(0, Matrix2::h()),
            Piece::Local(1, Matrix2::t()),
            Piece::Local(2, Matrix2::h()),
            Piece::Local(2, Matrix2::s()),
            Piece::Cx(0, 1),
            Piece::Cx(1, 2),
            Piece::Cx(2, 0),
        ];
        let mut state = 5u64;
        let mut failed = 0;
        for _ in 0..600 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let word: Vec<Piece> = (0..1 + (state >> 59) as usize)
                .map(|i| letters[(state >> (i * 3 % 55)) as usize % letters.len()])
                .collect();
            if synthesize(&product(&word).unwrap()).is_none() {
                failed += 1;
            }
        }
        assert_eq!(failed, 0);
    }

    #[test]
    fn toffoli() {
        let gate = Gate {
            kind: GateKind::X,
            controls: vec![WIRES[0], WIRES[1]],
            targets: vec![WIRES[2]],
            params: Vec::new(),
            span: Span::DUMMY,
        };
        let u = Unitary::of(&[gate], &WIRES).unwrap();
        let pieces = synthesize(&u).expect("synthesis");
        assert!(cnots(&pieces) <= MAX_CX);
    }
}
