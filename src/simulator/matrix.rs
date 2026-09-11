use num_complex::Complex;

use crate::ir::{self, GateKind};

pub type C64 = Complex<f64>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix2 {
    pub a: C64,
    pub b: C64,
    pub c: C64,
    pub d: C64,
}

impl Matrix2 {
    pub fn new(a: C64, b: C64, c: C64, d: C64) -> Self {
        Self { a, b, c, d }
    }

    pub fn identity() -> Self {
        Self::new(
            C64::new(1.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(1.0, 0.0),
        )
    }

    pub fn x() -> Self {
        Self::new(
            C64::new(0.0, 0.0),
            C64::new(1.0, 0.0),
            C64::new(1.0, 0.0),
            C64::new(0.0, 0.0),
        )
    }

    pub fn y() -> Self {
        Self::new(
            C64::new(0.0, 0.0),
            C64::new(0.0, -1.0),
            C64::new(0.0, 1.0),
            C64::new(0.0, 0.0),
        )
    }

    pub fn z() -> Self {
        Self::new(
            C64::new(1.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(-1.0, 0.0),
        )
    }

    pub fn h() -> Self {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        Self::new(
            C64::new(s, 0.0),
            C64::new(s, 0.0),
            C64::new(s, 0.0),
            C64::new(-s, 0.0),
        )
    }

    pub fn phase(angle: f64) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self::new(
            C64::new(1.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(cos, sin),
        )
    }

    pub fn s() -> Self {
        Self::phase(std::f64::consts::FRAC_PI_2)
    }

    pub fn s_dagger() -> Self {
        Self::phase(-std::f64::consts::FRAC_PI_2)
    }

    pub fn t() -> Self {
        Self::phase(std::f64::consts::FRAC_PI_4)
    }

    pub fn t_dagger() -> Self {
        Self::phase(-std::f64::consts::FRAC_PI_4)
    }

    pub fn sx() -> Self {
        Self::new(
            C64::new(0.5, 0.5),
            C64::new(0.5, -0.5),
            C64::new(0.5, -0.5),
            C64::new(0.5, 0.5),
        )
    }

    pub fn sx_dagger() -> Self {
        Self::sx().adjoint()
    }

    pub fn rx(theta: f64) -> Self {
        let (sin, cos) = (theta / 2.0).sin_cos();
        Self::new(
            C64::new(cos, 0.0),
            C64::new(0.0, -sin),
            C64::new(0.0, -sin),
            C64::new(cos, 0.0),
        )
    }

    pub fn ry(theta: f64) -> Self {
        let (sin, cos) = (theta / 2.0).sin_cos();
        Self::new(
            C64::new(cos, 0.0),
            C64::new(-sin, 0.0),
            C64::new(sin, 0.0),
            C64::new(cos, 0.0),
        )
    }

    pub fn rz(theta: f64) -> Self {
        let (sin, cos) = (theta / 2.0).sin_cos();
        Self::new(
            C64::new(cos, -sin),
            C64::new(0.0, 0.0),
            C64::new(0.0, 0.0),
            C64::new(cos, sin),
        )
    }

    pub fn multiply(self, rhs: Matrix2) -> Matrix2 {
        Matrix2::new(
            self.a * rhs.a + self.b * rhs.c,
            self.a * rhs.b + self.b * rhs.d,
            self.c * rhs.a + self.d * rhs.c,
            self.c * rhs.b + self.d * rhs.d,
        )
    }

    pub fn adjoint(self) -> Matrix2 {
        Matrix2::new(self.a.conj(), self.c.conj(), self.b.conj(), self.d.conj())
    }

    pub fn approx_eq(&self, other: &Matrix2, epsilon: f64) -> bool {
        (self.a - other.a).norm() < epsilon
            && (self.b - other.b).norm() < epsilon
            && (self.c - other.c).norm() < epsilon
            && (self.d - other.d).norm() < epsilon
    }

    pub fn is_identity(&self, epsilon: f64) -> bool {
        self.approx_eq(&Matrix2::identity(), epsilon)
    }

    pub fn is_diagonal(&self, epsilon: f64) -> bool {
        self.b.norm() < epsilon && self.c.norm() < epsilon
    }

    pub fn to_ir(self) -> ir::Matrix2 {
        ir::Matrix2 {
            a: (self.a.re, self.a.im),
            b: (self.b.re, self.b.im),
            c: (self.c.re, self.c.im),
            d: (self.d.re, self.d.im),
        }
    }

    pub fn from_ir(m: ir::Matrix2) -> Self {
        Self::new(
            C64::new(m.a.0, m.a.1),
            C64::new(m.b.0, m.b.1),
            C64::new(m.c.0, m.c.1),
            C64::new(m.d.0, m.d.1),
        )
    }
}

pub fn matrix_for(kind: GateKind, params: &[f64]) -> Matrix2 {
    let theta = params.first().copied().unwrap_or(0.0);

    match kind {
        GateKind::I => Matrix2::identity(),
        GateKind::X => Matrix2::x(),
        GateKind::Y => Matrix2::y(),
        GateKind::Z => Matrix2::z(),
        GateKind::H => Matrix2::h(),
        GateKind::S => Matrix2::s(),
        GateKind::SDag => Matrix2::s_dagger(),
        GateKind::T => Matrix2::t(),
        GateKind::TDag => Matrix2::t_dagger(),
        GateKind::SX => Matrix2::sx(),
        GateKind::SXDag => Matrix2::sx_dagger(),
        GateKind::Rx => Matrix2::rx(theta),
        GateKind::Ry => Matrix2::ry(theta),
        GateKind::Rz => Matrix2::rz(theta),
        GateKind::R1 => Matrix2::phase(theta),
        GateKind::Swap => Matrix2::identity(),
        GateKind::Unitary(m) => Matrix2::from_ir(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_unitary(m: Matrix2) {
        let product = m.multiply(m.adjoint());
        assert!(
            product.is_identity(1e-12),
            "matrix is not unitary: {product:?}"
        );
    }

    #[test]
    fn every_named_gate_is_unitary() {
        for m in [
            Matrix2::identity(),
            Matrix2::x(),
            Matrix2::y(),
            Matrix2::z(),
            Matrix2::h(),
            Matrix2::s(),
            Matrix2::s_dagger(),
            Matrix2::t(),
            Matrix2::t_dagger(),
            Matrix2::sx(),
            Matrix2::sx_dagger(),
            Matrix2::rx(0.7),
            Matrix2::ry(-1.3),
            Matrix2::rz(2.1),
            Matrix2::phase(0.4),
        ] {
            assert_unitary(m);
        }
    }

    #[test]
    fn daggers_invert_their_partners() {
        assert!(
            Matrix2::s()
                .multiply(Matrix2::s_dagger())
                .is_identity(1e-12)
        );
        assert!(
            Matrix2::t()
                .multiply(Matrix2::t_dagger())
                .is_identity(1e-12)
        );
        assert!(
            Matrix2::sx()
                .multiply(Matrix2::sx_dagger())
                .is_identity(1e-12)
        );
    }

    #[test]
    fn s_squared_is_z_and_t_squared_is_s() {
        assert!(
            Matrix2::s()
                .multiply(Matrix2::s())
                .approx_eq(&Matrix2::z(), 1e-12)
        );
        assert!(
            Matrix2::t()
                .multiply(Matrix2::t())
                .approx_eq(&Matrix2::s(), 1e-12)
        );
    }

    #[test]
    fn sx_squared_is_x() {
        assert!(
            Matrix2::sx()
                .multiply(Matrix2::sx())
                .approx_eq(&Matrix2::x(), 1e-12)
        );
    }

    #[test]
    fn hadamard_conjugates_x_into_z() {
        let h = Matrix2::h();
        let result = h.multiply(Matrix2::x()).multiply(h);
        assert!(result.approx_eq(&Matrix2::z(), 1e-12));
    }

    #[test]
    fn rotations_compose_by_adding_angles() {
        let merged = Matrix2::rz(0.3).multiply(Matrix2::rz(0.4));
        assert!(merged.approx_eq(&Matrix2::rz(0.7), 1e-12));

        let merged = Matrix2::ry(1.1).multiply(Matrix2::ry(-0.6));
        assert!(merged.approx_eq(&Matrix2::ry(0.5), 1e-12));
    }

    #[test]
    fn ir_round_trip_preserves_the_matrix() {
        let original = Matrix2::rx(0.9);
        let restored = Matrix2::from_ir(original.to_ir());
        assert!(original.approx_eq(&restored, 1e-15));
    }
}
