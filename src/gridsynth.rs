use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_4, PI, SQRT_2};
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::sync::OnceLock;

use num_complex::Complex;

use crate::codegen::zyz_angles;
use crate::diag::Span;
use crate::ir::*;
use crate::simulator::matrix::{C64, Matrix2, matrix_for};
use crate::transpile::GateSet;

const MAX_K: u32 = 62;
const PRIMES: [u128; 15] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47];
const LAMBDA: f64 = 1.0 + SQRT_2;
const GCD_STEPS: usize = 200;
const RHO_STEPS: u64 = 1 << 22;
const MAX_POWER: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Root2 {
    a: i128,
    b: i128,
}

impl Root2 {
    const ZERO: Root2 = Root2 { a: 0, b: 0 };
    const ONE: Root2 = Root2 { a: 1, b: 0 };
    const LAMBDA: Root2 = Root2 { a: 1, b: 1 };
    const INVERSE: Root2 = Root2 { a: -1, b: 1 };

    fn integer(a: i128) -> Root2 {
        Root2 { a, b: 0 }
    }

    fn bullet(self) -> Root2 {
        Root2 {
            a: self.a,
            b: -self.b,
        }
    }

    fn norm(self) -> i128 {
        self.a * self.a - 2 * self.b * self.b
    }

    fn value(self) -> f64 {
        self.a as f64 + self.b as f64 * SQRT_2
    }

    fn quotient(self, by: Root2) -> Option<(Root2, bool)> {
        let mut n = by.norm();
        if n == 0 {
            return None;
        }
        let mut top = self * by.bullet();
        if n < 0 {
            n = -n;
            top = -top;
        }
        let exact = top.a % n == 0 && top.b % n == 0;
        Some((
            Root2 {
                a: nearest(top.a, n),
                b: nearest(top.b, n),
            },
            exact,
        ))
    }

    fn divide(self, by: Root2) -> Option<Root2> {
        self.quotient(by).and_then(|(q, exact)| exact.then_some(q))
    }
}

impl Add for Root2 {
    type Output = Root2;
    fn add(self, o: Root2) -> Root2 {
        Root2 {
            a: self.a + o.a,
            b: self.b + o.b,
        }
    }
}

impl Sub for Root2 {
    type Output = Root2;
    fn sub(self, o: Root2) -> Root2 {
        Root2 {
            a: self.a - o.a,
            b: self.b - o.b,
        }
    }
}

impl Neg for Root2 {
    type Output = Root2;
    fn neg(self) -> Root2 {
        Root2 {
            a: -self.a,
            b: -self.b,
        }
    }
}

impl Mul for Root2 {
    type Output = Root2;
    fn mul(self, o: Root2) -> Root2 {
        Root2 {
            a: self.a * o.a + 2 * self.b * o.b,
            b: self.a * o.b + self.b * o.a,
        }
    }
}

fn nearest(a: i128, n: i128) -> i128 {
    (2 * a + n).div_euclid(2 * n)
}

fn gcd_root2(mut x: Root2, mut y: Root2) -> Option<Root2> {
    for _ in 0..GCD_STEPS {
        if y == Root2::ZERO {
            return Some(x);
        }
        let (q, _) = x.quotient(y)?;
        (x, y) = (y, x - q * y);
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Omega([i128; 4]);

impl Omega {
    const ZERO: Omega = Omega([0; 4]);
    const ONE: Omega = Omega([1, 0, 0, 0]);
    const OMEGA: Omega = Omega([0, 1, 0, 0]);
    const ROOT2: Omega = Omega([0, 1, 0, -1]);

    fn real(r: Root2) -> Omega {
        Omega([r.a, r.b, 0, -r.b])
    }

    fn dagger(self) -> Omega {
        let [a, b, c, d] = self.0;
        Omega([a, -d, -c, -b])
    }

    fn bullet(self) -> Omega {
        let [a, b, c, d] = self.0;
        Omega([a, -b, c, -d])
    }

    fn sigma3(self) -> Omega {
        let [a, b, c, d] = self.0;
        Omega([a, d, -c, b])
    }

    fn to_root2(self) -> Option<Root2> {
        let [a, b, c, d] = self.0;
        (c == 0 && d == -b).then_some(Root2 { a, b })
    }

    fn power(k: u8) -> Omega {
        let mut out = Omega::ONE;
        for _ in 0..k % 8 {
            out = out * Omega::OMEGA;
        }
        out
    }

    fn quotient(self, by: Omega) -> Option<(Omega, bool)> {
        let rest = by.sigma3() * by.bullet() * by.dagger();
        let n = (by * rest).0[0];
        if n <= 0 {
            return None;
        }
        let top = self * rest;
        let exact = top.0.iter().all(|x| x % n == 0);
        Some((Omega(top.0.map(|x| nearest(x, n))), exact))
    }

    fn halve_root2(self) -> Option<Omega> {
        let product = self * Omega::ROOT2;
        product
            .0
            .iter()
            .all(|x| x % 2 == 0)
            .then(|| Omega(product.0.map(|x| x / 2)))
    }
}

impl Add for Omega {
    type Output = Omega;
    fn add(self, o: Omega) -> Omega {
        Omega([0, 1, 2, 3].map(|i| self.0[i] + o.0[i]))
    }
}

impl Sub for Omega {
    type Output = Omega;
    fn sub(self, o: Omega) -> Omega {
        Omega([0, 1, 2, 3].map(|i| self.0[i] - o.0[i]))
    }
}

impl Neg for Omega {
    type Output = Omega;
    fn neg(self) -> Omega {
        Omega(self.0.map(|x| -x))
    }
}

impl Mul for Omega {
    type Output = Omega;
    fn mul(self, o: Omega) -> Omega {
        let mut out = [0; 4];
        for i in 0..4 {
            for j in 0..4 {
                let term = self.0[i] * o.0[j];
                if i + j < 4 {
                    out[i + j] += term;
                } else {
                    out[i + j - 4] -= term;
                }
            }
        }
        Omega(out)
    }
}

fn gcd_omega(mut x: Omega, mut y: Omega) -> Option<Omega> {
    for _ in 0..GCD_STEPS {
        if y == Omega::ZERO {
            return Some(x);
        }
        let (q, _) = x.quotient(y)?;
        (x, y) = (y, x - q * y);
    }
    None
}

fn mul_mod(a: u128, b: u128, m: u128) -> u128 {
    if m <= u128::from(u64::MAX) {
        return a % m * (b % m) % m;
    }
    let (mut a, mut b, mut out) = (a % m, b % m, 0u128);
    while b > 0 {
        if b & 1 == 1 {
            out = if out >= m - a { out - (m - a) } else { out + a };
        }
        a = if a >= m - a { a - (m - a) } else { a + a };
        b >>= 1;
    }
    out
}

fn pow_mod(mut base: u128, mut exponent: u128, m: u128) -> u128 {
    let mut out = 1 % m;
    base %= m;
    while exponent > 0 {
        if exponent & 1 == 1 {
            out = mul_mod(out, base, m);
        }
        base = mul_mod(base, base, m);
        exponent >>= 1;
    }
    out
}

fn is_prime(n: u128) -> bool {
    if n < 2 {
        return false;
    }
    if let Some(&p) = PRIMES.iter().find(|&&p| n.is_multiple_of(p)) {
        return n == p;
    }
    let shift = (n - 1).trailing_zeros();
    let odd = (n - 1) >> shift;
    PRIMES.iter().all(|&a| {
        let mut x = pow_mod(a, odd, n);
        if x == 1 || x == n - 1 {
            return true;
        }
        for _ in 1..shift {
            x = mul_mod(x, x, n);
            if x == n - 1 {
                return true;
            }
        }
        false
    })
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn rho(n: u128) -> Option<u128> {
    if n.is_multiple_of(2) {
        return Some(2);
    }
    let limit = if n <= u128::from(u64::MAX) {
        RHO_STEPS
    } else {
        RHO_STEPS >> 13
    };
    for seed in 1..20u128 {
        let step = |x: u128| (mul_mod(x, x, n) + seed) % n;
        let (mut slow, mut fast, mut found) = (2u128, 2u128, 1u128);
        let mut steps = 0;
        while found == 1 && steps < limit {
            slow = step(slow);
            fast = step(step(fast));
            found = gcd(slow.abs_diff(fast), n);
            steps += 1;
        }
        if found != 1 && found != n {
            return Some(found);
        }
    }
    None
}

fn factor(n: u128, out: &mut HashMap<u128, u32>) -> Option<()> {
    if n == 1 {
        return Some(());
    }
    for p in PRIMES {
        if n.is_multiple_of(p) && n != p {
            *out.entry(p).or_insert(0) += 1;
            return factor(n / p, out);
        }
    }
    if is_prime(n) {
        *out.entry(n).or_insert(0) += 1;
        return Some(());
    }
    let d = rho(n)?;
    factor(d, out)?;
    factor(n / d, out)
}

fn sqrt_mod(a: u128, p: u128) -> Option<u128> {
    let a = a % p;
    if a == 0 {
        return Some(0);
    }
    if pow_mod(a, (p - 1) / 2, p) != 1 {
        return None;
    }
    let shift = (p - 1).trailing_zeros();
    let odd = (p - 1) >> shift;
    let z = (2..p).find(|&z| pow_mod(z, (p - 1) / 2, p) == p - 1)?;
    let (mut m, mut c, mut t, mut r) = (
        shift,
        pow_mod(z, odd, p),
        pow_mod(a, odd, p),
        pow_mod(a, odd.div_ceil(2), p),
    );
    while t != 1 {
        let mut i = 0;
        let mut x = t;
        while x != 1 {
            x = mul_mod(x, x, p);
            i += 1;
        }
        let b = pow_mod(c, 1 << (m - i - 1), p);
        m = i;
        c = mul_mod(b, b, p);
        t = mul_mod(t, c, p);
        r = mul_mod(r, b, p);
    }
    Some(r)
}

fn cornacchia(d: u128, p: u128) -> Option<(i128, i128)> {
    let mut r = sqrt_mod(p - d % p, p)?;
    if 2 * r < p {
        r = p - r;
    }
    let (mut a, mut b) = (p, r);
    while b.checked_mul(b)? >= p {
        (a, b) = (b, a % b);
    }
    let rest = p - b * b;
    if !rest.is_multiple_of(d) {
        return None;
    }
    let y = (rest / d).isqrt();
    (y * y == rest / d).then_some((i128::try_from(b).ok()?, i128::try_from(y).ok()?))
}

fn norm_equation(xi: Root2) -> Option<Omega> {
    if xi == Root2::ZERO {
        return Some(Omega::ZERO);
    }
    if xi.value() < 0.0 || xi.bullet().value() < 0.0 {
        return None;
    }
    let n = u128::try_from(xi.norm()).ok()?;
    let mut primes = HashMap::new();
    factor(n, &mut primes)?;
    let mut rest = xi;
    let mut t = Omega::ONE;
    let mut take = |by: Root2, part: Omega, rest: &mut Root2| {
        while let Some(q) = rest.divide(by) {
            *rest = q;
            t = t * part;
        }
    };
    for &p in primes.keys() {
        let prime = Root2::integer(i128::try_from(p).ok()?);
        match p % 8 {
            2 => take(Root2 { a: 0, b: 1 }, Omega([1, -1, 0, 0]), &mut rest),
            3 => {
                let (x, y) = cornacchia(2, p)?;
                take(prime, Omega([x, y, 0, y]), &mut rest);
            }
            5 => {
                let (x, y) = cornacchia(1, p)?;
                take(prime, Omega([x, 0, y, 0]), &mut rest);
            }
            1 => {
                let (x, y) = cornacchia(1, p)?;
                let (u, v) = cornacchia(2, p)?;
                let (gauss, root) = (Omega([x, 0, y, 0]), Omega([u, v, 0, v]));
                for s in [gcd_omega(gauss, root)?, gcd_omega(gauss, root.dagger())?] {
                    take((s * s.dagger()).to_root2()?, s, &mut rest);
                }
            }
            7 => {
                let h = i128::try_from(sqrt_mod(2, p)?).ok()?;
                let eta = gcd_root2(prime, Root2 { a: h, b: -1 })?;
                for eta in [eta, eta.bullet()] {
                    take(eta * eta, Omega::real(eta), &mut rest);
                }
            }
            _ => return None,
        }
    }
    let mut unit = xi.divide((t * t.dagger()).to_root2()?)?;
    let square = Root2::LAMBDA * Root2::LAMBDA;
    for _ in 0..128 {
        if unit == Root2::ONE {
            return (t * t.dagger()).to_root2().filter(|&r| r == xi).map(|_| t);
        }
        if unit.value() > 1.0 {
            unit = unit.divide(square)?;
            t = t * Omega::real(Root2::LAMBDA);
        } else {
            unit = unit * square;
            t = t * Omega::real(Root2::INVERSE);
        }
    }
    None
}

fn lambda_power(n: i32) -> Root2 {
    static POWERS: OnceLock<Vec<Root2>> = OnceLock::new();
    let powers = POWERS.get_or_init(|| {
        let mut table = vec![Root2::ONE; 2 * MAX_POWER + 1];
        for i in 1..=MAX_POWER {
            table[MAX_POWER + i] = table[MAX_POWER + i - 1] * Root2::LAMBDA;
            table[MAX_POWER - i] = table[MAX_POWER - i + 1] * Root2::INVERSE;
        }
        table
    });
    match usize::try_from(n + MAX_POWER as i32) {
        Ok(at) if at < powers.len() => powers[at],
        _ => {
            let (base, count) = if n >= 0 {
                (Root2::LAMBDA, n)
            } else {
                (Root2::INVERSE, -n)
            };
            (0..count).fold(Root2::ONE, |acc, _| acc * base)
        }
    }
}

fn grid(x0: f64, x1: f64, y0: f64, y1: f64, out: &mut Vec<Root2>) {
    if x1 < x0 || y1 < y0 {
        return;
    }
    let n = if x1 > x0 && y1 > y0 {
        (((y1 - y0) / (x1 - x0)).ln() / (2.0 * LAMBDA.ln())).round() as i32
    } else {
        0
    };
    let stretch = LAMBDA.powi(n);
    let squeeze = (-1.0 / LAMBDA).powi(n);
    let (x0, x1) = (x0 * stretch, x1 * stretch);
    let (y0, y1) = if squeeze > 0.0 {
        (y0 * squeeze, y1 * squeeze)
    } else {
        (y1 * squeeze, y0 * squeeze)
    };
    let back = lambda_power(-n);
    let low = ((x0 - y1) / (2.0 * SQRT_2)).ceil() as i128;
    let high = ((x1 - y0) / (2.0 * SQRT_2)).floor() as i128;
    for b in low..=high {
        let shift = b as f64 * SQRT_2;
        let from = (x0 - shift).max(y0 + shift).ceil() as i128;
        let to = (x1 - shift).min(y1 + shift).floor() as i128;
        out.extend((from..=to).map(|a| back * Root2 { a, b }));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct Wide {
    hi: f64,
    lo: f64,
}

impl Wide {
    const SQRT_2: Wide = Wide {
        hi: SQRT_2,
        lo: -9.667_293_313_452_913e-17,
    };
    const PI: Wide = Wide {
        hi: PI,
        lo: 1.224_646_799_147_353_2e-16,
    };

    fn joined(hi: f64, lo: f64) -> Wide {
        let sum = hi + lo;
        Wide {
            hi: sum,
            lo: lo - (sum - hi),
        }
    }

    fn sqrt(self) -> Wide {
        if self.hi <= 0.0 {
            return Wide::from(0.0);
        }
        let guess = Wide::from(self.hi.sqrt());
        guess + (self - guess * guess) / (guess * Wide::from(2.0))
    }

    fn integer(value: i128) -> Wide {
        let hi = value as f64;
        Wide::joined(hi, (value - hi as i128) as f64)
    }

    fn root2(x: Root2) -> Wide {
        Wide::integer(x.a) + Wide::integer(x.b) * Wide::SQRT_2
    }

    fn sin_cos(angle: f64) -> (Wide, Wide) {
        let turn = Wide::PI * Wide::from(2.0);
        let turns = (angle / turn.hi).round();
        let x = Wide::from(angle) - turn * Wide::from(turns);
        let (mut sin, mut cos) = (Wide::from(0.0), Wide::from(0.0));
        let mut term = Wide::from(1.0);
        for n in 0..40u32 {
            match n % 4 {
                0 => cos = cos + term,
                1 => sin = sin + term,
                2 => cos = cos - term,
                _ => sin = sin - term,
            }
            term = term * x / Wide::from(f64::from(n + 1));
        }
        (sin, cos)
    }
}

impl From<f64> for Wide {
    fn from(value: f64) -> Wide {
        Wide { hi: value, lo: 0.0 }
    }
}

impl Add for Wide {
    type Output = Wide;
    fn add(self, o: Wide) -> Wide {
        let sum = self.hi + o.hi;
        let bent = sum - self.hi;
        let error = (self.hi - (sum - bent)) + (o.hi - bent);
        Wide::joined(sum, error + self.lo + o.lo)
    }
}

impl Sub for Wide {
    type Output = Wide;
    fn sub(self, o: Wide) -> Wide {
        self + -o
    }
}

impl Neg for Wide {
    type Output = Wide;
    fn neg(self) -> Wide {
        Wide {
            hi: -self.hi,
            lo: -self.lo,
        }
    }
}

impl Mul for Wide {
    type Output = Wide;
    fn mul(self, o: Wide) -> Wide {
        let product = self.hi * o.hi;
        let error = self.hi.mul_add(o.hi, -product) + (self.hi * o.lo + self.lo * o.hi);
        Wide::joined(product, error)
    }
}

impl Div for Wide {
    type Output = Wide;
    fn div(self, o: Wide) -> Wide {
        let first = self.hi / o.hi;
        let rest = self - o * Wide::from(first);
        let second = rest.hi / o.hi;
        let rest = rest - o * Wide::from(second);
        Wide::joined(first, second) + Wide::from(rest.hi / o.hi)
    }
}

struct Region {
    re: Wide,
    im: Wide,
    depth: Wide,
    low: C64,
    high: C64,
}

fn padding(scale: Wide) -> f64 {
    scale.hi * 1e-15 + 1e-12
}

impl Region {
    fn new(theta: f64, epsilon: f64) -> Region {
        let (sin, cos) = Wide::sin_cos(-theta / 2.0);
        let square = Wide::from(epsilon) * Wide::from(epsilon);
        let depth = Wide::from(1.0) - square / Wide::from(2.0);
        let center = Complex::new(cos.hi, sin.hi);
        let reach = epsilon * (1.0 - epsilon * epsilon / 4.0).max(0.0).sqrt();
        let across = center * Complex::new(0.0, reach);
        let points = [
            center,
            center * depth.hi + across,
            center * depth.hi - across,
        ];
        let margin = epsilon * epsilon + 1e-15;
        let bound = |pick: fn(&C64) -> f64, fold: fn(f64, f64) -> f64, start: f64| {
            points.iter().map(pick).fold(start, fold)
        };
        Region {
            re: cos,
            im: sin,
            depth,
            low: Complex::new(
                bound(|p| p.re, f64::min, f64::INFINITY) - margin,
                bound(|p| p.im, f64::min, f64::INFINITY) - margin,
            ),
            high: Complex::new(
                bound(|p| p.re, f64::max, f64::NEG_INFINITY) + margin,
                bound(|p| p.im, f64::max, f64::NEG_INFINITY) + margin,
            ),
        }
    }

    fn column(&self, re: Wide, scale: Wide) -> Option<(f64, f64)> {
        let pad = padding(scale);
        let reach = (scale * scale - re * re).sqrt().hi + pad;
        let (mut low, mut high) = (scale.hi * self.low.im - pad, scale.hi * self.high.im + pad);
        low = low.max(-reach);
        high = high.min(reach);
        let threshold = self.depth * scale - re * self.re;
        if self.im.hi > 1e-12 {
            low = low.max((threshold / self.im).hi - pad);
        } else if self.im.hi < -1e-12 {
            high = high.min((threshold / self.im).hi + pad);
        } else if threshold.hi > pad {
            return None;
        }
        (low <= high).then_some((low, high))
    }

    fn inside(&self, re: Wide, im: Wide, scale: Wide) -> bool {
        let limit = scale * scale;
        re * re + im * im <= limit && re * self.re + im * self.im >= self.depth * scale
    }

    fn candidates(&self, k: u32) -> Vec<Omega> {
        let scale = if k.is_multiple_of(2) {
            Wide::integer(1 << (k / 2))
        } else {
            Wide::integer(1 << (k / 2)) * Wide::SQRT_2
        };
        let half = Wide::SQRT_2 / Wide::from(2.0);
        let limit = scale * scale;
        let pad = padding(scale);
        let mut found = Vec::new();
        let mut ys = Vec::new();
        for (offset, shifted) in [(Wide::from(0.0), false), (half, true)] {
            let mut xs = Vec::new();
            grid(
                scale.hi * self.low.re - offset.hi - pad,
                scale.hi * self.high.re - offset.hi + pad,
                offset.hi - scale.hi - pad,
                offset.hi + scale.hi + pad,
                &mut xs,
            );
            for x in xs {
                let re = Wide::root2(x) + offset;
                let bullet = Wide::root2(x.bullet()) - offset;
                if re * re > limit || bullet * bullet > limit {
                    continue;
                }
                let Some((low, high)) = self.column(re, scale) else {
                    continue;
                };
                let reach = (limit - bullet * bullet).sqrt().hi + pad;
                ys.clear();
                grid(
                    low - offset.hi,
                    high - offset.hi,
                    offset.hi - reach,
                    offset.hi + reach,
                    &mut ys,
                );
                for &y in &ys {
                    let im = Wide::root2(y) + offset;
                    let conjugate = Wide::root2(y.bullet()) - offset;
                    if self.inside(re, im, scale)
                        && bullet * bullet + conjugate * conjugate <= limit
                    {
                        let u = Omega([x.a, x.b + y.b, y.a, y.b - x.b]);
                        found.push(if shifted { u + Omega::OMEGA } else { u });
                    }
                }
            }
        }
        found
    }
}

#[derive(Clone, Copy)]
struct Exact {
    m: [Omega; 4],
    k: u32,
}

impl Exact {
    fn reduced(mut self) -> Exact {
        while self.k > 0 {
            let halved: Option<Vec<Omega>> = self.m.iter().map(|x| x.halve_root2()).collect();
            let Some(halved) = halved else {
                break;
            };
            self.m = [halved[0], halved[1], halved[2], halved[3]];
            self.k -= 1;
        }
        self
    }

    fn step(self, j: u8) -> Exact {
        let phase = Omega::power(j);
        let [a, b, c, d] = self.m;
        let (c, d) = (c * phase, d * phase);
        Exact {
            m: [a + c, b + d, a - c, b - d],
            k: self.k + 1,
        }
        .reduced()
    }
}

fn unit_power(x: Omega) -> Option<u8> {
    (0..8).find(|&j| Omega::power(j) == x)
}

fn powers(j: u8) -> Vec<GateKind> {
    match j % 8 {
        0 => vec![],
        1 => vec![GateKind::T],
        2 => vec![GateKind::S],
        3 => vec![GateKind::S, GateKind::T],
        4 => vec![GateKind::Z],
        5 => vec![GateKind::Z, GateKind::T],
        6 => vec![GateKind::SDag],
        _ => vec![GateKind::TDag],
    }
}

fn synthesize(u: Omega, t: Omega, k: u32) -> Option<Vec<GateKind>> {
    let mut m = Exact {
        m: [u, -t.dagger(), t, u.dagger()],
        k,
    }
    .reduced();
    let mut rounds = Vec::new();
    for _ in 0..4 * k + 16 {
        if m.k == 0 {
            break;
        }
        let single = (0..4)
            .map(|j| (vec![j], m.step(j)))
            .min_by_key(|(_, next)| next.k)?;
        let (js, next) = if single.1.k < m.k {
            single
        } else {
            (0..4)
                .flat_map(|a| (0..4).map(move |b| (a, b)))
                .map(|(a, b)| (vec![a, b], m.step(a).step(b)))
                .find(|(_, next)| next.k < m.k)?
        };
        rounds.extend(js);
        m = next;
    }
    if m.k != 0 {
        return None;
    }
    let [a, b, c, d] = m.m;
    let mut gates = if b == Omega::ZERO && c == Omega::ZERO {
        powers(8 + unit_power(d)? - unit_power(a)?)
    } else if a == Omega::ZERO && d == Omega::ZERO {
        let mut gates = powers(8 + unit_power(b)? - unit_power(c)?);
        gates.push(GateKind::X);
        gates
    } else {
        return None;
    };
    for &j in rounds.iter().rev() {
        gates.push(GateKind::H);
        gates.extend(powers(8 - j));
    }
    Some(gates)
}

fn product(gates: &[GateKind]) -> Matrix2 {
    gates.iter().fold(Matrix2::identity(), |m, &kind| {
        matrix_for(kind, &[]).multiply(m)
    })
}

fn distance(a: &Matrix2, b: &Matrix2) -> f64 {
    let overlap = a.adjoint().multiply(*b);
    let trace = overlap.a + overlap.d;
    let phase = if trace.norm() > 1e-300 {
        trace / trace.norm()
    } else {
        Complex::new(1.0, 0.0)
    };
    let entries = [
        a.a - b.a / phase,
        a.b - b.b / phase,
        a.c - b.c / phase,
        a.d - b.d / phase,
    ];
    let gram = [
        entries[0].norm_sqr() + entries[2].norm_sqr(),
        entries[1].norm_sqr() + entries[3].norm_sqr(),
        (entries[0].conj() * entries[1] + entries[2].conj() * entries[3]).norm(),
    ];
    let (sum, gap) = (
        gram[0] + gram[1],
        ((gram[0] - gram[1]).powi(2) + 4.0 * gram[2] * gram[2]).sqrt(),
    );
    ((sum + gap) / 2.0).sqrt()
}

fn eighths(angle: f64) -> Option<u8> {
    let turns = angle / FRAC_PI_4;
    ((turns - turns.round()).abs() < 1e-12).then(|| turns.round().rem_euclid(8.0) as u8)
}

pub fn exact(theta: f64) -> Option<Vec<GateKind>> {
    eighths(theta).map(powers)
}

pub fn rz(theta: f64, epsilon: f64) -> Option<Vec<GateKind>> {
    if let Some(gates) = exact(theta) {
        return Some(gates);
    }
    let region = Region::new(theta, epsilon);
    let target = matrix_for(GateKind::Rz, &[theta]);
    for k in 0..=MAX_K {
        for u in region.candidates(k) {
            let xi = Root2::integer(1 << k) - (u * u.dagger()).to_root2()?;
            let Some(t) = norm_equation(xi) else {
                continue;
            };
            if let Some(gates) = synthesize(u, t, k)
                && distance(&product(&gates), &target) <= epsilon * (1.0 + 1e-9)
            {
                return Some(gates);
            }
        }
    }
    None
}

fn single(kind: GateKind, target: QubitId, span: Span) -> Op {
    Op::Gate(Gate {
        kind,
        controls: Vec::new(),
        targets: vec![target],
        params: Vec::new(),
        span,
    })
}

pub fn approximate(program: &mut Program, set: &GateSet, epsilon: f64) -> Result<usize, String> {
    if !(set.has(GateKind::H, 0) && set.has(GateKind::T, 0)) {
        return Err(format!(
            "approximating rotations needs h and t in the gate set, and {} has no {}",
            set.name(),
            if set.has(GateKind::H, 0) { "t" } else { "h" }
        ));
    }
    let mut cache: HashMap<(u64, u64), Vec<GateKind>> = HashMap::new();
    let mut approximated = 0;
    for block in &mut program.blocks {
        let mut ops = Vec::with_capacity(block.ops.len());
        for op in block.ops.drain(..) {
            let Op::Gate(gate) = &op else {
                ops.push(op);
                continue;
            };
            let params: Option<Vec<f64>> = gate
                .params
                .iter()
                .map(|p| p.constant().map(Const::as_f64))
                .collect();
            let (Some(params), [target], []) = (params, &gate.targets[..], &gate.controls[..])
            else {
                ops.push(op);
                continue;
            };
            if set.allows(gate) || gate.kind == GateKind::Swap {
                ops.push(op);
                continue;
            }
            let (theta, phi, lambda) = zyz_angles(&matrix_for(gate.kind, &params));
            let rough = [lambda, theta, phi]
                .iter()
                .filter(|&&angle| eighths(angle).is_none())
                .count()
                .max(1);
            let share = epsilon / rough as f64;
            let mut sequence = Vec::new();
            let mut rotate = |angle: f64, sequence: &mut Vec<GateKind>| -> Result<(), String> {
                let key = (angle.to_bits(), share.to_bits());
                let words = match cache.get(&key) {
                    Some(words) => words.clone(),
                    None => {
                        let words = rz(angle, share).ok_or_else(|| {
                            format!("cannot approximate rz({angle}) to within {share:e}, qirc reaches about 1e-10 for each rotation")
                        })?;
                        cache.insert(key, words.clone());
                        words
                    }
                };
                sequence.extend(words);
                Ok(())
            };
            rotate(lambda, &mut sequence)?;
            if theta.abs() > 1e-12 {
                sequence.extend([GateKind::SDag, GateKind::H]);
                rotate(theta, &mut sequence)?;
                sequence.extend([GateKind::H, GateKind::S]);
            }
            rotate(phi, &mut sequence)?;
            ops.extend(
                sequence
                    .into_iter()
                    .map(|kind| single(kind, *target, gate.span)),
            );
            approximated += 1;
        }
        block.ops = ops;
    }
    Ok(approximated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_norms() {
        for xi in [
            Root2 { a: 5, b: 2 },
            Root2 { a: 17, b: 0 },
            Root2 { a: 3, b: 1 },
            Root2 { a: 23, b: 11 },
            Root2 { a: 2, b: 1 },
        ] {
            if let Some(t) = norm_equation(xi) {
                assert_eq!((t * t.dagger()).to_root2(), Some(xi), "{xi:?}");
            }
        }
        assert_eq!(norm_equation(Root2 { a: 7, b: 0 }), None);
    }

    #[test]
    fn approximates() {
        for (theta, epsilon) in [
            (0.3, 1e-2),
            (1.0, 1e-3),
            (-2.1, 1e-4),
            (0.001, 1e-4),
            (3.0, 1e-5),
            (0.7, 1e-9),
            (0.001, 1e-10),
        ] {
            let gates = rz(theta, epsilon).unwrap_or_else(|| panic!("rz({theta}) to {epsilon}"));
            let error = distance(&product(&gates), &matrix_for(GateKind::Rz, &[theta]));
            assert!(error <= epsilon * (1.0 + 1e-9), "{theta} {epsilon} {error}");
            let t = gates
                .iter()
                .filter(|&&g| matches!(g, GateKind::T | GateKind::TDag))
                .count() as f64;
            assert!(
                t <= 4.0 * (1.0 / epsilon).log2() + 12.0,
                "{theta} {epsilon} {t}"
            );
        }
        assert_eq!(
            rz(FRAC_PI_4 * 3.0, 1e-3),
            Some(vec![GateKind::S, GateKind::T])
        );
    }
}
