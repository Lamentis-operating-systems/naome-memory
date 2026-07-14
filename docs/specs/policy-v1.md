# Policy v1

Status: normative for [`poc-v1`](../../policies/poc-v1.toml).

The committed TOML file is a human projection of the closed `PolicyV1` type. A loader must deserialize it, require exact equality with `PolicyV1::poc_v1()`, and then bind the domain-separated canonical typed-policy digest into plans and receipts. TOML whitespace and key order are not hash authority. Runtime overrides are forbidden. A behavioral change requires a new policy ID, updated golden fixtures, calibration evidence, and a newly preregistered holdout when applicable.

## Fixed-point rules

`Ppm(x)` represents `x / 1,000,000`, where `0 <= x <= 1,000,000`. Weighted products use integer floor division by 1,000,000. Component sums use a signed wide accumulator and the final score is clamped to the `Ppm` range. Percentage counts use mathematical ceiling:

```text
ceil_rate(count, rate_ppm) = (count * rate_ppm + 999,999) / 1,000,000
```

No floating-point operation affects a policy decision or receipt.

## Cohorts and isolation

An STM atom expires exactly seven days after `recorded_at_us`; checked addition overflow and any shorter or longer expiry are rejected. Retirement groups atoms by UTC calendar day of expiry and permits one applied run per cohort. A plan requires a non-empty cohort and `as_of_us` at or after the next UTC-day boundary. Once the cohort is committed, later STM insertion into that memory-space/day is rejected. A cohort over 100,000 atoms is rejected without a partial result.

Every plan and query has exactly one `memory_space_id`. Candidate loading and core validation both enforce that boundary. Normal retrieval is repository-local; a caller must explicitly request memory-space-wide retrieval.

## Semantic similarity and clustering

For topic and entity sets, `J(A,B) = |A intersection B| / |A union B|` in Ppm; an empty union yields zero. `I(outcome)` is one only when both non-empty outcome classes are equal and otherwise zero.

```text
sim = 0.60 * J(topic) + 0.25 * J(entity) + 0.15 * I(outcome)
```

Atoms are considered in ascending `AtomId`. Each is added to the first existing cluster for which its similarity to every member is at least 650,000 ppm; otherwise it starts a new cluster. This is deterministic complete-link clustering, not an embedding model.

A cluster is eligible only with at least three atoms and two distinct sessions. Its fixed-point score is:

```text
C = 0.25 support
  + 0.20 diversity
  + 0.20 utility
  + 0.15 salience
  + 0.10 cohesion
  + 0.10 novelty
  - 0.15 uncertainty
```

and must be at least 550,000 ppm. Component definitions and their integer inputs are included in every score breakdown. Cohesion is the minimum pairwise similarity. Utility is derived only from validated positive/negative feedback with the Beta(1,1) prior. Structured semantic features require at least 666,667 ppm support from distinct source atoms; repeated values inside one atom count once. Semantic candidates are ordered by descending `C`, then ascending resulting semantic `AtomId`. The semantic body keeps a consensus `outcome_class` as structure but does not invent an `OutcomeV1`, prose summary, or success boolean from class spelling.

The cohort semantic budget is the minimum of `ceil(2% of cohort)`, 100 atoms, and 8 MiB of newly encoded semantic bodies. `RetirementPlanV1.semantic_decisions` records every score-eligible candidate in that order with its source IDs, score, encoded size, and one of `selected`, `skipped_atom_hard_limit`, `skipped_count_budget`, or `skipped_byte_budget`. Semantic bodies contain no newly generated prose.

## Independent episodic lottery

The population is every cohort episode with `semantic_and_exact_allowed` permission and complete atom/artifact integrity. Membership is independent of relevance, salience, utility, cluster eligibility, and semantic selection.

The winner count is `min(ceil(0.5% of population), 25)`. Sorted atom IDs are sampled without replacement by partial Fisher-Yates using the versioned RNG identifier `chacha20-rand_chacha-0.3/partial-fisher-yates-v1` and unbiased rejection-based bounded integer sampling. The only entropy is the explicit 32-byte seed. The receipt binds seed, RNG ID, ordered population digest, population count, requested winner count, byte budget, and ordered winners.

If winners and their retention-permitted artifact closure exceed 256 MiB for the cohort, the entire plan is rejected. A different winner may not be substituted. A winner may also be a source of a semantic atom.

## Retrieval

Candidate generation returns at most 512 integrity-complete candidates. For each candidate:

```text
R = 0.45 lexical
  + 0.25 semantic
  + 0.15 entity
  + 0.10 validated_utility
  + 0.05 recency
```

Validated utility is `(positive + 1) / (positive + negative + 2)` in fixed point. Retrieval count does not enter this value. Results order by descending `R` and ascending `AtomId`. Default `k` is 10 and maximum `k` is 100.

Flashbacks are disabled by default. An explicit query supplies a seed and budget from one to three; the channel uniformly samples exact episodic LTM without replacement, excludes normal result IDs, and labels every result `spontaneous` and `single_episode`.
