use super::matrix::Matrix2;

const LANES: usize = 4;
const LANE_BITS: usize = 2;

pub fn backend() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if avx2_available() {
            return "AVX2 + FMA (4 x f64 lanes)";
        }
    }
    "scalar"
}

#[cfg(target_arch = "x86_64")]
fn avx2_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma"))
}

pub fn apply_1q(re: &mut [f64], im: &mut [f64], m: &Matrix2, target: usize, controls: u64) {
    debug_assert_eq!(re.len(), im.len());
    debug_assert!((1usize << target) < re.len());
    debug_assert_eq!(controls & (1u64 << target), 0);

    #[cfg(target_arch = "x86_64")]
    {
        if re.len() >= LANES && avx2_available() {
            unsafe { apply_1q_avx2(re, im, m, target, controls) };
            return;
        }
    }

    apply_1q_scalar(re, im, m, target, controls);
}

pub fn apply_swap(re: &mut [f64], im: &mut [f64], a: usize, b: usize, controls: u64) {
    if a == b {
        return;
    }

    let (mask_a, mask_b) = (1usize << a, 1usize << b);

    for i in 0..re.len() {
        if i & mask_a != 0 && i & mask_b == 0 && (i as u64 & controls) == controls {
            let j = i ^ mask_a ^ mask_b;
            re.swap(i, j);
            im.swap(i, j);
        }
    }
}

fn apply_1q_scalar(re: &mut [f64], im: &mut [f64], m: &Matrix2, target: usize, controls: u64) {
    let len = re.len();
    let stride = 1usize << target;

    let mut base = 0;
    while base < len {
        for off in 0..stride {
            let i0 = base + off;
            let i1 = i0 + stride;

            if (i0 as u64 & controls) != controls {
                continue;
            }

            let (v0re, v0im) = (re[i0], im[i0]);
            let (v1re, v1im) = (re[i1], im[i1]);

            re[i0] = m.a.re * v0re - m.a.im * v0im + m.b.re * v1re - m.b.im * v1im;
            im[i0] = m.a.re * v0im + m.a.im * v0re + m.b.re * v1im + m.b.im * v1re;
            re[i1] = m.c.re * v0re - m.c.im * v0im + m.d.re * v1re - m.d.im * v1im;
            im[i1] = m.c.re * v0im + m.c.im * v0re + m.d.re * v1im + m.d.im * v1re;
        }

        base += stride << 1;
    }
}

fn lane_active(lane: usize, control_mask: u64) -> f64 {
    if (lane as u64 & control_mask) == control_mask {
        -1.0
    } else {
        0.0
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn apply_1q_avx2(re: &mut [f64], im: &mut [f64], m: &Matrix2, target: usize, controls: u64) {
    unsafe {
        use std::arch::x86_64::*;

        let len = re.len();
        let rp = re.as_mut_ptr();
        let ip = im.as_mut_ptr();

        let high_controls = controls & !((1u64 << LANE_BITS) - 1);
        let low_controls = controls & ((1u64 << LANE_BITS) - 1);
        let lane_mask = if low_controls == 0 {
            None
        } else {
            Some(_mm256_set_pd(
                lane_active(3, low_controls),
                lane_active(2, low_controls),
                lane_active(1, low_controls),
                lane_active(0, low_controls),
            ))
        };

        let stride = 1usize << target;

        if stride >= LANES {
            let are = _mm256_set1_pd(m.a.re);
            let aim = _mm256_set1_pd(m.a.im);
            let bre = _mm256_set1_pd(m.b.re);
            let bim = _mm256_set1_pd(m.b.im);
            let cre = _mm256_set1_pd(m.c.re);
            let cim = _mm256_set1_pd(m.c.im);
            let dre = _mm256_set1_pd(m.d.re);
            let dim = _mm256_set1_pd(m.d.im);

            let mut base = 0;
            while base < len {
                let mut off = 0;
                while off < stride {
                    let i0 = base + off;

                    if (i0 as u64 & high_controls) != high_controls {
                        off += LANES;
                        continue;
                    }

                    let i1 = i0 + stride;

                    let v0re = _mm256_loadu_pd(rp.add(i0));
                    let v0im = _mm256_loadu_pd(ip.add(i0));
                    let v1re = _mm256_loadu_pd(rp.add(i1));
                    let v1im = _mm256_loadu_pd(ip.add(i1));

                    let mut o0re = _mm256_mul_pd(are, v0re);
                    o0re = _mm256_fnmadd_pd(aim, v0im, o0re);
                    o0re = _mm256_fmadd_pd(bre, v1re, o0re);
                    o0re = _mm256_fnmadd_pd(bim, v1im, o0re);

                    let mut o0im = _mm256_mul_pd(are, v0im);
                    o0im = _mm256_fmadd_pd(aim, v0re, o0im);
                    o0im = _mm256_fmadd_pd(bre, v1im, o0im);
                    o0im = _mm256_fmadd_pd(bim, v1re, o0im);

                    let mut o1re = _mm256_mul_pd(cre, v0re);
                    o1re = _mm256_fnmadd_pd(cim, v0im, o1re);
                    o1re = _mm256_fmadd_pd(dre, v1re, o1re);
                    o1re = _mm256_fnmadd_pd(dim, v1im, o1re);

                    let mut o1im = _mm256_mul_pd(cre, v0im);
                    o1im = _mm256_fmadd_pd(cim, v0re, o1im);
                    o1im = _mm256_fmadd_pd(dre, v1im, o1im);
                    o1im = _mm256_fmadd_pd(dim, v1re, o1im);

                    if let Some(mask) = lane_mask {
                        o0re = _mm256_blendv_pd(v0re, o0re, mask);
                        o0im = _mm256_blendv_pd(v0im, o0im, mask);
                        o1re = _mm256_blendv_pd(v1re, o1re, mask);
                        o1im = _mm256_blendv_pd(v1im, o1im, mask);
                    }

                    _mm256_storeu_pd(rp.add(i0), o0re);
                    _mm256_storeu_pd(ip.add(i0), o0im);
                    _mm256_storeu_pd(rp.add(i1), o1re);
                    _mm256_storeu_pd(ip.add(i1), o1im);

                    off += LANES;
                }

                base += stride << 1;
            }
        } else {
            let row = |lane: usize| (lane >> target) & 1;
            let coeff_a = |lane: usize| if row(lane) == 0 { m.a } else { m.d };
            let coeff_b = |lane: usize| if row(lane) == 0 { m.b } else { m.c };

            let are = _mm256_set_pd(coeff_a(3).re, coeff_a(2).re, coeff_a(1).re, coeff_a(0).re);
            let aim = _mm256_set_pd(coeff_a(3).im, coeff_a(2).im, coeff_a(1).im, coeff_a(0).im);
            let bre = _mm256_set_pd(coeff_b(3).re, coeff_b(2).re, coeff_b(1).re, coeff_b(0).re);
            let bim = _mm256_set_pd(coeff_b(3).im, coeff_b(2).im, coeff_b(1).im, coeff_b(0).im);

            let mut base = 0;
            while base < len {
                if (base as u64 & high_controls) != high_controls {
                    base += LANES;
                    continue;
                }

                let vre = _mm256_loadu_pd(rp.add(base));
                let vim = _mm256_loadu_pd(ip.add(base));

                let (wre, wim) = if target == 0 {
                    (
                        _mm256_permute_pd::<0b0101>(vre),
                        _mm256_permute_pd::<0b0101>(vim),
                    )
                } else {
                    (
                        _mm256_permute2f128_pd::<0x01>(vre, vre),
                        _mm256_permute2f128_pd::<0x01>(vim, vim),
                    )
                };

                let mut ore = _mm256_mul_pd(are, vre);
                ore = _mm256_fnmadd_pd(aim, vim, ore);
                ore = _mm256_fmadd_pd(bre, wre, ore);
                ore = _mm256_fnmadd_pd(bim, wim, ore);

                let mut oim = _mm256_mul_pd(are, vim);
                oim = _mm256_fmadd_pd(aim, vre, oim);
                oim = _mm256_fmadd_pd(bre, wim, oim);
                oim = _mm256_fmadd_pd(bim, wre, oim);

                if let Some(mask) = lane_mask {
                    ore = _mm256_blendv_pd(vre, ore, mask);
                    oim = _mm256_blendv_pd(vim, oim, mask);
                }

                _mm256_storeu_pd(rp.add(base), ore);
                _mm256_storeu_pd(ip.add(base), oim);

                base += LANES;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::state::Rng;
    use num_complex::Complex;

    fn signed(rng: &mut Rng) -> f64 {
        rng.next_unit() * 2.0 - 1.0
    }

    fn random_state(n: usize, seed: u64) -> (Vec<f64>, Vec<f64>) {
        let mut rng = Rng::new(seed);
        let len = 1usize << n;
        (
            (0..len).map(|_| signed(&mut rng)).collect(),
            (0..len).map(|_| signed(&mut rng)).collect(),
        )
    }

    fn random_matrix(seed: u64) -> Matrix2 {
        let mut rng = Rng::new(seed);
        let mut c = || Complex::new(signed(&mut rng), signed(&mut rng));
        Matrix2::new(c(), c(), c(), c())
    }

    #[test]
    fn matches_scalar() {
        for n in 1..=7usize {
            for target in 0..n {
                let others: Vec<usize> = (0..n).filter(|&c| c != target).collect();

                let mut masks = vec![0u64];
                for &c in &others {
                    masks.push(1u64 << c);
                }
                for i in 0..others.len() {
                    for j in (i + 1)..others.len() {
                        masks.push((1u64 << others[i]) | (1u64 << others[j]));
                    }
                }
                if others.len() >= 3 {
                    masks.push((1u64 << others[0]) | (1u64 << others[1]) | (1u64 << others[2]));
                }

                for controls in masks {
                    let m = random_matrix(target as u64 * 31 + n as u64);
                    let (mut re, mut im) = random_state(n, 1000 + n as u64);
                    let (mut ref_re, mut ref_im) = (re.clone(), im.clone());

                    apply_1q(&mut re, &mut im, &m, target, controls);
                    apply_1q_scalar(&mut ref_re, &mut ref_im, &m, target, controls);

                    for i in 0..re.len() {
                        assert!(
                            (re[i] - ref_re[i]).abs() < 1e-12 && (im[i] - ref_im[i]).abs() < 1e-12,
                            "n={n} target={target} controls={controls:b} index={i}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn control_mask() {
        let n = 5usize;
        let len = 1usize << n;
        let m = Matrix2::x();
        let controls = (1u64 << 1) | (1u64 << 3);

        let (mut re, mut im) = random_state(n, 99);
        let (before_re, before_im) = (re.clone(), im.clone());

        apply_1q(&mut re, &mut im, &m, 0, controls);

        for i in 0..len {
            let active = (i as u64 & controls) == controls;
            if !active {
                assert_eq!(re[i], before_re[i], "index {i} should be untouched");
                assert_eq!(im[i], before_im[i], "index {i} should be untouched");
            }
        }

        let flipped = 0b01011usize;
        assert_eq!(re[flipped], before_re[flipped ^ 1]);
    }

    #[test]
    fn swap() {
        let n = 3;
        let (mut re, mut im) = random_state(n, 7);
        let (before_re, before_im) = (re.clone(), im.clone());

        apply_swap(&mut re, &mut im, 0, 2, 0);

        for i in 0..1usize << n {
            let bit0 = i & 1;
            let bit2 = (i >> 2) & 1;
            let j = (i & !0b101) | (bit0 << 2) | bit2;
            assert_eq!(re[j], before_re[i]);
            assert_eq!(im[j], before_im[i]);
        }
    }
}
