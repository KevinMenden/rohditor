//! Helpers shared by unit tests.

/// Deterministic Gaussian noise (SplitMix64 + Box–Muller) so this crate
/// needs no RNG dependency.
pub(crate) struct Gauss(pub u64);
impl Gauss {
    pub(crate) fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1_u64 << 53) as f64
    }
    pub(crate) fn next(&mut self) -> f32 {
        let (u, v) = (self.uniform().max(1e-12), self.uniform());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }
}
