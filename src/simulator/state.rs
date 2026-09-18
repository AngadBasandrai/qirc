use num_complex::Complex;
use std::fmt;

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

pub const MAX_QUBITS: usize = 30;

pub fn memory_required(n: usize) -> Option<u64> {
    1u64.checked_shl(n as u32)?.checked_mul(16)
}

pub struct Sampler {
    cumulative: Vec<f64>,
}

impl Sampler {
    pub fn draw(&self, rng: &mut Rng) -> usize {
        let total = self.cumulative.last().copied().unwrap_or(0.0);
        let point = rng.next_unit() * total;
        self.cumulative
            .partition_point(|&c| c <= point)
            .min(self.cumulative.len().saturating_sub(1))
    }
}

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

    pub fn num_qubits(&self) -> usize {
        self.n
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

    pub fn apply(&mut self, matrix: &Matrix2, target: usize, controls: u64) {
        if target >= self.n {
            return;
        }
        simd::apply_1q(&mut self.re, &mut self.im, matrix, target, controls);
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

    pub fn collapse(&mut self, qubit: usize, outcome: bool) {
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

    pub fn reset(&mut self, qubit: usize, rng: &mut Rng) {
        if self.measure(qubit, rng) {
            self.apply(&Matrix2::x(), qubit, 0);
        }
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

    pub fn ket(&self, index: usize) -> String {
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
    use super::*;

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
            state.apply(&Matrix2::ry(std::f64::consts::FRAC_PI_3), 0, 0);
            if state.measure(0, &mut rng) {
                ones += 1;
            }
        }

        let expected = (std::f64::consts::FRAC_PI_6).sin().powi(2);
        let observed = ones as f64 / trials as f64;
        assert!(
            (observed - expected).abs() < 0.03,
            "expected about {expected}, observed {observed}"
        );
    }

    #[test]
    fn resets() {
        let mut rng = Rng::new(99);
        for _ in 0..50 {
            let mut state = State::new(2);
            state.apply(&Matrix2::h(), 0, 0);
            state.apply(&Matrix2::x(), 1, 1 << 0);
            state.reset(0, &mut rng);
            assert!(state.qubit_probability(0) < 1e-12);
            assert!((state.norm() - 1.0).abs() < 1e-12);
        }
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
