use crate::{MemoryError, Result};
use serde::{Deserialize, Serialize};

/// A fixed-point value in millionths. Logical decisions never depend on a
/// floating-point implementation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash,
)]
#[serde(try_from = "u32", into = "u32")]
pub struct Ppm(u32);

impl Ppm {
    pub const SCALE: u32 = 1_000_000;
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(Self::SCALE);

    pub const fn new(value: u32) -> Result<Self> {
        if value <= Self::SCALE {
            Ok(Self(value))
        } else {
            Err(MemoryError::InvalidPpm { value })
        }
    }

    /// Constructs a fixed-point value from a compile-time or otherwise
    /// prevalidated raw value.
    ///
    /// # Panics
    ///
    /// Panics when `value` exceeds [`Ppm::SCALE`]. Safe callers therefore
    /// cannot construct an out-of-range `Ppm`, including in release builds.
    pub const fn from_raw_unchecked(value: u32) -> Self {
        assert!(value <= Self::SCALE, "PPM value exceeds one million");
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Floor of an unsigned ratio, saturated to one.
    pub fn ratio(numerator: u64, denominator: u64) -> Self {
        if denominator == 0 {
            return Self::ZERO;
        }
        let scaled =
            u128::from(numerator).saturating_mul(u128::from(Self::SCALE)) / u128::from(denominator);
        let bounded = scaled.min(u128::from(Self::SCALE));
        Self(u32::try_from(bounded).unwrap_or(Self::SCALE))
    }

    /// Floor fixed-point multiplication. This is the normative arithmetic used
    /// by every policy weight.
    pub fn multiply(self, other: Self) -> Self {
        let product = u64::from(self.0) * u64::from(other.0);
        let floored = product / u64::from(Self::SCALE);
        Self(
            u32::try_from(floored)
                .unwrap_or(Self::SCALE)
                .min(Self::SCALE),
        )
    }

    pub fn complement(self) -> Self {
        Self(Self::SCALE - self.0)
    }

    pub fn mean(values: impl IntoIterator<Item = Self>) -> Self {
        let mut count = 0_u64;
        let mut sum = 0_u64;
        for value in values {
            count = count.saturating_add(1);
            sum = sum.saturating_add(u64::from(value.0));
        }
        let Some(rounded) = (sum + count / 2).checked_div(count) else {
            return Self::ZERO;
        };
        Self(
            u32::try_from(rounded)
                .unwrap_or(Self::SCALE)
                .min(Self::SCALE),
        )
    }

    /// `ceil(population * self)` without floating point.
    pub fn ceil_count(self, population: usize) -> usize {
        let numerator = (population as u128) * u128::from(self.0);
        let value = numerator.div_ceil(u128::from(Self::SCALE));
        usize::try_from(value).unwrap_or(usize::MAX)
    }
}

impl TryFrom<u32> for Ppm {
    type Error = MemoryError;

    fn try_from(value: u32) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Ppm> for u32 {
    fn from(value: Ppm) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::Ppm;

    #[test]
    fn bounds_and_ceiling_are_exact() {
        assert!(Ppm::new(1_000_001).is_err());
        let half_percent = Ppm::new(5_000).unwrap_or(Ppm::ZERO);
        assert_eq!(Ppm::ONE.ceil_count(0), 0);
        assert_eq!(Ppm::ZERO.ceil_count(42), 0);
        assert_eq!(half_percent.ceil_count(1), 1);
        assert_eq!(half_percent.ceil_count(200), 1);
        assert_eq!(half_percent.ceil_count(201), 2);
    }

    #[test]
    fn complement_and_rounded_mean_cover_small_nonzero_values() {
        assert_eq!(Ppm::ZERO.complement(), Ppm::ONE);
        let one_ppm = Ppm::from_raw_unchecked(1);
        assert_eq!(Ppm::mean([Ppm::ZERO, one_ppm]), one_ppm);
    }

    #[test]
    fn beta_prior_is_representable_as_ratio() {
        assert_eq!(Ppm::ratio(1, 2).get(), 500_000);
        assert_eq!(Ppm::ratio(3, 4).get(), 750_000);
    }

    #[test]
    fn unchecked_constructor_still_rejects_out_of_range_safe_inputs() {
        assert!(std::panic::catch_unwind(|| Ppm::from_raw_unchecked(Ppm::SCALE + 1)).is_err());
    }
}
