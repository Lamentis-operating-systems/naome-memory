use naome_memory_core::Seed32;
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore as _, SeedableRng as _};
use serde::{Deserialize, Serialize};

/// A signed difference measured in millionths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub struct SignedPpm(i32);

impl SignedPpm {
    pub const SCALE: i32 = 1_000_000;
    pub const ZERO: Self = Self(0);

    /// Construct a bounded signed fixed-point value.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is outside `-1_000_000..=1_000_000`.
    pub const fn new(value: i32) -> std::result::Result<Self, &'static str> {
        if value >= -Self::SCALE && value <= Self::SCALE {
            Ok(Self(value))
        } else {
            Err("signed PPM must be in -1,000,000..=1,000,000")
        }
    }

    #[must_use]
    /// Constructs a signed fixed-point value from a prevalidated raw value.
    ///
    /// # Panics
    ///
    /// Panics when `value` is outside `-1_000_000..=1_000_000`. The check is
    /// retained in release builds.
    pub const fn from_raw_unchecked(value: i32) -> Self {
        assert!(
            value >= -Self::SCALE,
            "signed PPM value is outside the supported range"
        );
        assert!(
            value <= Self::SCALE,
            "signed PPM value is outside the supported range"
        );
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl TryFrom<i32> for SignedPpm {
    type Error = &'static str;

    fn try_from(value: i32) -> std::result::Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SignedPpm> for i32 {
    fn from(value: SignedPpm) -> Self {
        value.0
    }
}

/// Integer graded NDCG@10 against the complete committed judgment set.
///
/// `judged_grades_in_rank_order` contains the relevance grade for every
/// returned hit (zero for an unjudged hit). `all_judged_grades` contains every
/// positive grade in the query's frozen qrels, including judged items that the
/// model did not return. Supplying the complete qrels is essential: deriving
/// the ideal denominator from returned hits would make a missed relevant item
/// disappear from the metric.
///
/// Gains are the committed integer grades themselves and discounts are the
/// fixed millionth approximations below. No floating-point value participates
/// in the result.
#[must_use]
pub fn ndcg_at_10(judged_grades_in_rank_order: &[u32], all_judged_grades: &[u32]) -> u32 {
    // floor(1/log2(rank + 1) * 1_000_000), committed as integer authority.
    const DISCOUNTS: [u32; 10] = [
        1_000_000, 630_929, 500_000, 430_676, 386_852, 356_207, 333_333, 315_464, 301_029, 289_064,
    ];
    let mut ideal_grades = all_judged_grades
        .iter()
        .copied()
        .filter(|grade| *grade > 0)
        .collect::<Vec<_>>();
    ideal_grades.sort_unstable_by(|left, right| right.cmp(left));
    if ideal_grades.is_empty() {
        return 0;
    }
    let actual = judged_grades_in_rank_order
        .iter()
        .take(DISCOUNTS.len())
        .zip(DISCOUNTS)
        .map(|(grade, discount)| u128::from(*grade) * u128::from(discount))
        .sum::<u128>();
    let ideal = ideal_grades
        .iter()
        .take(DISCOUNTS.len())
        .zip(DISCOUNTS)
        .map(|(grade, discount)| u128::from(*grade) * u128::from(discount))
        .sum::<u128>();
    if ideal == 0 {
        return 0;
    }
    u32::try_from((actual * 1_000_000_u128) / ideal)
        .unwrap_or(1_000_000)
        .min(1_000_000)
}

/// Deterministic non-parametric bootstrap lower 95% confidence bound.
///
/// Every replicate samples `values.len()` observations with replacement. The
/// returned order statistic is the lower 2.5th percentile of replicate means.
#[must_use]
pub fn bootstrap_lower_95(values: &[SignedPpm], replicates: usize, seed: Seed32) -> SignedPpm {
    if values.is_empty() || replicates == 0 {
        return SignedPpm::ZERO;
    }
    let mut rng = ChaCha20Rng::from_seed(*seed.as_bytes());
    let mut means = Vec::with_capacity(replicates);
    for _ in 0..replicates {
        let mut sum = 0_i64;
        for _ in 0..values.len() {
            let index = bounded_index(&mut rng, values.len());
            sum = sum.saturating_add(i64::from(values[index].get()));
        }
        let denominator = i64::try_from(values.len()).unwrap_or(i64::MAX);
        let mean = sum.div_euclid(denominator);
        means.push(i32::try_from(mean).unwrap_or_else(|_| {
            if mean.is_negative() {
                -SignedPpm::SCALE
            } else {
                SignedPpm::SCALE
            }
        }));
    }
    means.sort_unstable();
    let percentile_index = replicates
        .saturating_mul(25)
        .div_ceil(1_000)
        .saturating_sub(1);
    SignedPpm::from_raw_unchecked(means[percentile_index.min(means.len() - 1)])
}

fn bounded_index(rng: &mut ChaCha20Rng, upper_exclusive: usize) -> usize {
    let upper = u64::try_from(upper_exclusive).unwrap_or(u64::MAX);
    let acceptance = u64::MAX - (u64::MAX % upper);
    loop {
        let value = rng.next_u64();
        if value < acceptance {
            return usize::try_from(value % upper).unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SignedPpm, bootstrap_lower_95, ndcg_at_10};
    use naome_memory_core::Seed32;

    #[test]
    fn ndcg_matches_hand_calculated_complete_qrel_fixtures() {
        // Frozen v1 qrels are binary: one consolidated semantic result and
        // three exact source episodes. The ideal fixed-point DCG is 2_561_605
        // = 1_000_000 + 630_929 + 500_000 + 430_676.
        let qrels = [1, 1, 1, 1];
        assert_eq!(ndcg_at_10(&[1, 1, 1, 1], &qrels), 1_000_000);
        assert_eq!(ndcg_at_10(&[1], &qrels), 390_380);
        assert_eq!(ndcg_at_10(&[0, 0, 1], &qrels), 195_190);
        assert_eq!(ndcg_at_10(&[], &qrels), 0);
    }

    #[test]
    fn ndcg_does_not_shrink_the_ideal_when_relevant_items_are_missed() {
        let complete_qrels = [1, 1, 1];
        assert_eq!(ndcg_at_10(&[1], &complete_qrels), 469_278);
        assert_eq!(ndcg_at_10(&[1], &[1]), 1_000_000);
    }

    #[test]
    fn bootstrap_is_deterministic_and_integer_only() {
        let values = vec![SignedPpm::from_raw_unchecked(100_000); 100];
        let seed = Seed32::new([7; 32]);
        assert_eq!(
            bootstrap_lower_95(&values, 2_048, seed),
            SignedPpm::from_raw_unchecked(100_000)
        );
    }

    #[test]
    #[should_panic(expected = "signed PPM value is outside the supported range")]
    fn unchecked_signed_ppm_constructor_is_release_safe() {
        let _ = SignedPpm::from_raw_unchecked(SignedPpm::SCALE.saturating_add(1));
    }
}
