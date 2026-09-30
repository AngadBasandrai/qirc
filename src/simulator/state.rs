use num_complex::Complex;
use std::fmt;
use std::num::NonZero;
use std::thread;

use super::matrix::{C64, Matrix2};
use super::simd;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

const PARALLEL_ABOVE: usize = 21;
const MAX_WORKERS: usize = 16;

type Halves<'a> = (
    usize,
    &'a mut [f64],
    &'a mut [f64],
    &'a mut [f64],
    &'a mut [f64],
);

fn pair((offset, re0, re1, im0, im1): Halves<'_>, m: &Matrix2, controls: u64) {
    for i in 0..re0.len() {
        if ((offset + i) as u64 & controls) != controls {
            continue;
        }
        let (a, b) = (C64::new(re0[i], im0[i]), C64::new(re1[i], im1[i]));
        let (x, y) = (m.a * a + m.b * b, m.c * a + m.d * b);
        (re0[i], im0[i], re1[i], im1[i]) = (x.re, x.im, y.re, y.im);
    }
}

fn apply_parallel(
    re: &mut [f64],
    im: &mut [f64],
    m: &Matrix2,
    target: usize,
    controls: u64,
    workers: usize,
) {
    let chunk = (re.len() / workers.next_power_of_two()).max(2);
    let low = (chunk - 1) as u64;
    if (1usize << target) < chunk {
        let high = controls & !low;
        thread::scope(|scope| {
            for (i, (r, j)) in re.chunks_mut(chunk).zip(im.chunks_mut(chunk)).enumerate() {
                if (i * chunk) as u64 & high == high {
                    scope.spawn(move || simd::apply_1q(r, j, m, target, controls & low));
                }
            }
        });
        return;
    }
    let stride = 1usize << target;
    let mut tasks: Vec<Halves<'_>> = Vec::new();
    for (block, (r, j)) in re
        .chunks_mut(2 * stride)
        .zip(im.chunks_mut(2 * stride))
        .enumerate()
    {
        let (r0, r1) = r.split_at_mut(stride);
        let (j0, j1) = j.split_at_mut(stride);
        let halves = r0
            .chunks_mut(chunk)
            .zip(r1.chunks_mut(chunk))
            .zip(j0.chunks_mut(chunk).zip(j1.chunks_mut(chunk)));
        for (k, ((a, b), (c, d))) in halves.enumerate() {
            tasks.push((block * 2 * stride + k * chunk, a, b, c, d));
        }
    }
    thread::scope(|scope| {
        for task in tasks {
            scope.spawn(move || pair(task, m, controls));
        }
    });
}

pub const MAX_QUBITS: usize = if cfg!(target_arch = "wasm32") { 20 } else { 30 };

pub fn memory_required(n: usize) -> Option<u64> {
    1u64.checked_shl(n as u32)?.checked_mul(16)
}

pub struct Sampler {
    cumulative: Vec<f64>,
}

impl Sampler {
    pub fn from_probabilities(probabilities: &[f64]) -> Sampler {
        let mut running = 0.0;
        Sampler {
            cumulative: probabilities
                .iter()
                .map(|p| {
                    running += p;
                    running
                })
                .collect(),
        }
    }

    pub fn draw(&self, rng: &mut Rng) -> usize {
        let total = self.cumulative.last().copied().unwrap_or(0.0);
        let point = rng.next_unit() * total;
        self.cumulative
            .partition_point(|&c| c <= point)
            .min(self.cumulative.len().saturating_sub(1))
    }
}

#[derive(Clone)]
pub struct State {
    n: usize,
    re: Vec<f64>,
    im: Vec<f64>,
}

impl State {
    pub fn new(n: usize) -> Self {
        assert!(
            n <= MAX_QUBITS,
            "cannot build a state vector over {n} qubits: the limit is {MAX_QUBITS}"
        );

        let len = 1usize << n;
        let mut re = vec![0.0; len];
        re[0] = 1.0;

        Self {
            n,
            re,
            im: vec![0.0; len],
        }
    }

    pub fn len(&self) -> usize {
        self.re.len()
    }

    pub fn is_empty(&self) -> bool {
        self.re.is_empty()
    }

    pub fn amplitude(&self, index: usize) -> C64 {
        Complex::new(self.re[index], self.im[index])
    }

    pub fn pauli_element(&self, ket: &State, x: usize, z: usize) -> C64 {
        let sum: C64 = (0..ket.len())
            .map(|j| {
                let sign = if (j & z).count_ones() % 2 == 1 {
                    -1.0
                } else {
                    1.0
                };
                self.amplitude(j ^ x).conj() * ket.amplitude(j) * sign
            })
            .sum();
        let phase = [
            C64::new(1.0, 0.0),
            C64::new(0.0, 1.0),
            C64::new(-1.0, 0.0),
            C64::new(0.0, -1.0),
        ][(x & z).count_ones() as usize % 4];
        phase * sum
    }

    pub fn apply(&mut self, matrix: &Matrix2, target: usize, controls: u64) {
        if target >= self.n {
            return;
        }
        let workers = if cfg!(target_arch = "wasm32") || self.n < PARALLEL_ABOVE {
            1
        } else {
            thread::available_parallelism()
                .map_or(1, NonZero::get)
                .min(MAX_WORKERS)
        };
        if workers > 1 {
            apply_parallel(
                &mut self.re,
                &mut self.im,
                matrix,
                target,
                controls,
                workers,
            );
        } else {
            simd::apply_1q(&mut self.re, &mut self.im, matrix, target, controls);
        }
    }

    pub fn swap(&mut self, a: usize, b: usize, controls: u64) {
        if a >= self.n || b >= self.n {
            return;
        }
        simd::apply_swap(&mut self.re, &mut self.im, a, b, controls);
    }

    pub fn probabilities(&self) -> Vec<f64> {
        self.re
            .iter()
            .zip(&self.im)
            .map(|(r, i)| r * r + i * i)
            .collect()
    }

    pub fn qubit_probability(&self, qubit: usize) -> f64 {
        if qubit >= self.n {
            return 0.0;
        }
        let mask = 1usize << qubit;
        self.re
            .iter()
            .zip(&self.im)
            .enumerate()
            .filter(|(i, _)| i & mask != 0)
            .map(|(_, (r, im))| r * r + im * im)
            .sum()
    }

    pub fn norm(&self) -> f64 {
        self.re
            .iter()
            .zip(&self.im)
            .map(|(r, i)| r * r + i * i)
            .sum::<f64>()
            .sqrt()
    }

    pub(crate) fn collapse(&mut self, qubit: usize, outcome: bool) {
        if qubit >= self.n {
            return;
        }

        let mask = 1usize << qubit;
        let mut norm_sq = 0.0;

        for i in 0..self.len() {
            let keep = (i & mask != 0) == outcome;
            if keep {
                norm_sq += self.re[i] * self.re[i] + self.im[i] * self.im[i];
            } else {
                self.re[i] = 0.0;
                self.im[i] = 0.0;
            }
        }

        if norm_sq <= 0.0 {
            self.re[0] = 1.0;
            return;
        }

        let scale = 1.0 / norm_sq.sqrt();
        for i in 0..self.len() {
            self.re[i] *= scale;
            self.im[i] *= scale;
        }
    }

    pub fn measure(&mut self, qubit: usize, rng: &mut Rng) -> bool {
        let probability_one = self.qubit_probability(qubit);
        let outcome = rng.next_unit() < probability_one;
        self.collapse(qubit, outcome);
        outcome
    }

    pub fn sampler(&self) -> Sampler {
        let mut running = 0.0;
        let cumulative = self
            .re
            .iter()
            .zip(&self.im)
            .map(|(r, i)| {
                running += r * r + i * i;
                running
            })
            .collect();
        Sampler { cumulative }
    }

    fn ket(&self, index: usize) -> String {
        format!("|{:0width$b}>", index, width = self.n.max(1))
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "State vector ({} qubits, {} amplitudes):",
            self.n,
            self.len()
        )?;

        const EPS: f64 = 1e-12;
        const MAX_ROWS: usize = 64;

        let mut shown = 0;
        let mut hidden = 0;

        for i in 0..self.len() {
            let amp = self.amplitude(i);
            let p = amp.norm_sqr();

            if p <= EPS {
                continue;
            }

            if shown == MAX_ROWS {
                hidden += 1;
                continue;
            }

            writeln!(
                f,
                "  {} {:>9.6} {} {:>8.6}i   p = {:.6}",
                self.ket(i),
                amp.re,
                if amp.im < 0.0 { '-' } else { '+' },
                amp.im.abs(),
                p
            )?;
            shown += 1;
        }

        if shown == 0 {
            writeln!(f, "  (all amplitudes vanish)")?;
        }

        if hidden > 0 {
            writeln!(f, "  ... and {hidden} more non-zero amplitudes")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn threads() {
        let mut rng = Rng::new(17);
        for round in 0..40 {
            let n = 8;
            let target = round % n;
            let controls = if round % 3 == 0 {
                0
            } else {
                rng.next_u64() & 0xff & !(1 << target)
            };
            let theta = rng.next_unit() * 3.0;
            let m = Matrix2::new(
                C64::new(theta.cos(), 0.1),
                C64::new(0.2, theta.sin()),
                C64::new(-0.3, 0.4),
                C64::new(theta.sin(), -theta.cos()),
            );
            let re: Vec<f64> = (0..1 << n).map(|_| rng.next_unit() - 0.5).collect();
            let im: Vec<f64> = (0..1 << n).map(|_| rng.next_unit() - 0.5).collect();
            let (mut single_re, mut single_im) = (re.clone(), im.clone());
            simd::apply_1q(&mut single_re, &mut single_im, &m, target, controls);
            for workers in [2, 4, 8] {
                let (mut many_re, mut many_im) = (re.clone(), im.clone());
                apply_parallel(&mut many_re, &mut many_im, &m, target, controls, workers);
                let gap = single_re
                    .iter()
                    .zip(&many_re)
                    .chain(single_im.iter().zip(&many_im))
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f64::max);
                assert!(gap < 1e-12, "{round} {workers} {target} {controls:b} {gap}");
            }
        }
    }

    use super::*;
    use std::f64::consts::{FRAC_PI_3, FRAC_PI_6};

    #[test]
    fn initial_state() {
        let state = State::new(3);
        assert_eq!(state.len(), 8);
        assert!((state.amplitude(0).re - 1.0).abs() < 1e-15);
        assert!((state.norm() - 1.0).abs() < 1e-15);
    }

    #[test]
    fn bell_pair() {
        let mut state = State::new(2);
        state.apply(&Matrix2::h(), 0, 0);
        state.apply(&Matrix2::x(), 1, 1 << 0);

        let probabilities = state.probabilities();
        assert!((probabilities[0b00] - 0.5).abs() < 1e-12);
        assert!((probabilities[0b11] - 0.5).abs() < 1e-12);
        assert!(probabilities[0b01].abs() < 1e-12);
        assert!(probabilities[0b10].abs() < 1e-12);
    }

    #[test]
    fn toffoli() {
        let mut state = State::new(3);
        state.apply(&Matrix2::x(), 0, 0);
        state.apply(&Matrix2::x(), 2, (1 << 0) | (1 << 1));
        assert!(state.qubit_probability(2) < 1e-12);

        state.apply(&Matrix2::x(), 1, 0);
        state.apply(&Matrix2::x(), 2, (1 << 0) | (1 << 1));
        assert!((state.qubit_probability(2) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn collapses() {
        let mut state = State::new(2);
        state.apply(&Matrix2::h(), 0, 0);
        state.apply(&Matrix2::x(), 1, 1 << 0);

        let mut rng = Rng::new(12345);
        let outcome = state.measure(0, &mut rng);

        assert!((state.norm() - 1.0).abs() < 1e-12);
        assert!((state.qubit_probability(1) - if outcome { 1.0 } else { 0.0 }).abs() < 1e-12);

        let second = state.measure(1, &mut rng);
        assert_eq!(second, outcome);
    }

    #[test]
    fn measurement_statistics() {
        let mut rng = Rng::new(7);
        let mut ones = 0;
        let trials = 4000;

        for _ in 0..trials {
            let mut state = State::new(1);
            state.apply(&Matrix2::ry(FRAC_PI_3), 0, 0);
            if state.measure(0, &mut rng) {
                ones += 1;
            }
        }

        let expected = FRAC_PI_6.sin().powi(2);
        let observed = ones as f64 / trials as f64;
        assert!(
            (observed - expected).abs() < 0.03,
            "expected about {expected}, observed {observed}"
        );
    }

    #[test]
    fn controlled_swap() {
        let mut state = State::new(3);
        state.apply(&Matrix2::x(), 0, 0);
        state.swap(0, 1, 1 << 2);
        assert!((state.qubit_probability(0) - 1.0).abs() < 1e-12);

        state.apply(&Matrix2::x(), 2, 0);
        state.swap(0, 1, 1 << 2);
        assert!(state.qubit_probability(0) < 1e-12);
        assert!((state.qubit_probability(1) - 1.0).abs() < 1e-12);
    }
}
