# Theory Contract v1

Status: normative for `poc-v1`. Passing evidence has to be produced for a specific commit; this document does not assert that the thresholds have already passed.

## Question

Can a deterministic hybrid memory preserve useful repeated structure while a small, relevance-independent episodic lottery recovers rare exact events without materially damaging ordinary retrieval under the same total byte budget?

## Mechanically required invariants

Every valid proof receipt must show:

1. A sealed atom body and its canonical digest never change.
2. An exact episodic winner retains the identical atom ID, body bytes, and every retention-permitted artifact byte.
3. Semantic and episodic count and byte budgets are never exceeded.
4. Inputs and outputs never cross `memory_space_id` boundaries.
5. Every semantic atom closes over complete source IDs and source body digests.
6. Equal ordered inputs, canonical typed-policy digest, explicit time, and seed produce identical decisions and state digests.
7. Retirement apply is atomic and idempotent.
8. Atom, artifact, policy, seed, decision, state, or receipt tampering is detected fail-closed.
9. A single episode is never silently returned as a general semantic statement.

Invariant status is `verified`, `rejected`, or `inconclusive`.

## Empirical hypotheses

Experiments use paired synthetic worlds and the same per-world maximum byte cap
for both models in a comparison. The ledger reports the shared cap and each
model's actual bytes separately; unused capacity is never reported as stored
semantic content. Semantic-only contains every valid semantic body emitted by
`poc-v1`; hybrid contains those same bodies plus only the lottery-selected
exact package. If either model exceeds the cap or the candidate-selection rule
is not closed, the hypothesis is `inconclusive` rather than silently repaired.

### `semantic_utility`

The lower bound of the 95% paired bootstrap interval for

```text
NDCG@10(semantic-only) - NDCG@10(recency-only)
```

must be at least `+100_000 ppm` (+0.10).

### `episodic_rare_recovery`

The lower interval bound for rare-event recall of hybrid memory minus semantic-only memory must be at least `+80_000 ppm` (+0.08).

### `ordinary_utility_preserved`

The lower interval bound for

```text
NDCG@10(hybrid) - NDCG@10(semantic-only)
```

must be at least `-20_000 ppm` (-0.02).

Hypothesis status is `supported`, `rejected`, or `inconclusive`. A completed experiment that rejects a hypothesis exits with code 6; malformed or unverifiable evidence is `invalid`, not rejected.

## Datasets

- `golden-v1`: about 20 human-auditable atoms containing semantic selection, exact episodic selection, both, and forgotten fates.
- `calibration-v1`: 40 explicit worlds and the only dataset used by the PR
  profile or permitted for tuning.
- `holdout-v1`: 100 explicit frozen worlds and the only dataset used by the
  deep profile. After any result is inspected, policy tuning against this
  dataset is prohibited; a changed policy requires a new preregistered holdout
  version.
- `adversarial-v1`: scope mixing, digest substitution, oversize inputs, contradictions, missing artifacts, stale snapshots, and receipt-closure attacks.
- `scale-v1`: 100,000 atoms with P50 16 KiB, P95 64 KiB, and maximum 256 KiB.

The committed dataset manifest binds the generator source SHA-256, versioned
dataset-specific seed domains, explicit ordered world IDs, ordinary and rare
query judgments, golden lottery seeds, comparator rule, and workload sizes.
The verifier hashes the compiled generator source and fails closed when it
diverges from the manifest. All data is synthetic and non-sensitive.

## Statistical procedure

Metrics are integer fixed-point values. Worlds are the resampling unit. The PR
profile performs 2,048 deterministic paired bootstrap resamples over
`calibration-v1`; the deep profile performs 20,000 over frozen `holdout-v1`.
Bootstrap and world seeds are dataset-domain separated. The procedure sorts
paired differences and takes the lower 2.5th-percentile order statistic. No
host RNG, wall clock, or floating-point result participates in the receipt.
Calibration receipts report these metrics but all empirical hypothesis
verdicts remain `inconclusive`; only the deep holdout profile may support or
reject a preregistered hypothesis.

The harness executes the declared comparisons rather than using count proxies:

- semantic-only ranks the actual semantic bodies emitted by retirement;
- recency-only ranks the source episodes newest-first;
- hybrid ranks those semantic bodies plus the exact lottery winners;
- each of ten preregistered rare queries judges one exact episode at `Recall@1`;
- ordinary queries use binary `NDCG@10` over the complete manifest qrels:
  episode indices `0`, `1`, and `2`, plus the one semantic body matching the
  committed semantic topic judgment, each have gain `1`;
- the ideal DCG denominator always includes every committed positive judgment,
  including relevant items a model failed to return.

Every world ledger entry contains query hit IDs, judged IDs and their grades,
left/right scores, paired deltas, shared byte caps, and actual model bytes.

If a query has no judged relevant item for a metric, its value is absent rather than silently set to zero; a hypothesis is `inconclusive` if preregistered minimum sample support is not met. The receipt contains sample counts, point estimates, lower bounds, thresholds, and comparison byte totals.

## Receipt result

Overall status is:

- `passed` only when every required invariant is `verified` and every required hypothesis is `supported`;
- `failed` when any invariant is `rejected` or any required hypothesis is `rejected`;
- `inconclusive` when nothing is rejected but at least one required result is inconclusive;
- `invalid` when input closure, schema, policy, seed, implementation identity, or receipt verification fails.

A receipt digest attests only to reproducibility and declared closure. Independent review is still required for theory validity, dataset adequacy, and implementation correctness.

`verify_lab_bundle` regenerates and reevaluates every committed world, then
recomputes byte evidence, metrics, bootstrap bounds, statuses, the complete
decision ledger, and overall result before accepting a bundle. Rehashing a
changed bound, status, budget, or ledger value does not make it valid.

The test-only slow retirement reference model independently recomputes, without
calling the production decision helpers, complete-link clusters and score
breakdowns; full semantic bodies, IDs, digests, canonical sizes, candidate
ordering and budget dispositions; lottery population evidence, count/byte
budgets and winners; every per-source fate and exact-artifact set; and the
complete projected logical post-state plus its digest. It also reconstructs the
ordered input-set authority and compares the complete `RetirementPlanV1`. The
PR profile compares 1,000 deterministic adversarial worlds and the deep/release
profile compares 10,000. A separate deterministic fixture creates six score-eligible clusters
under a one-atom semantic budget, proving both selected and skipped-count
dispositions and their byte accounting. Another fixture constructs 34 large,
score-eligible clusters under the immutable 8 MiB budget and compares selected
and skipped-byte dispositions without changing the policy. This differential
campaign does not claim an independent implementation of input validation,
SQLite durability, crash behavior, or empirical judgments; those remain
separate proof surfaces. A second independent valid-input reference reconstructs
every fixed-point retrieval score component, total ordering and tie-break,
query digest, ranked-hit generality, flashback exclusion population/digest, and
seeded flashback winners. A third reference reconstructs plan, decision, run,
and receipt hashes, closure, invariant declarations, and observed state/artifact
binding; selected seed, closure, state, and artifact tampering must be rejected
by both implementations. The retrieval reference does not duplicate malformed
candidate rejection rules, and the receipt reference is not an exhaustive
structural oracle for every malformed plan.

The frozen `holdout-v1` binary judgments must not be retroactively graded or
otherwise changed to rescue a threshold after results are observed. A rejected
hypothesis blocks `v0.1.0`; calibration may inform only a newly versioned
policy and preregistered dataset contract.

## Current v0.1 holdout result

The independently replayed 100-world `holdout-v1` baseline reports these lower
95% confidence bounds:

- `semantic_utility`: `+36_869 ppm`, below `+100_000 ppm` — `rejected`;
- `episodic_rare_recovery`: `+100_000 ppm`, above `+80_000 ppm` — `supported`;
- `ordinary_utility_preserved`: `+54_186 ppm`, above `-20_000 ppm` —
  `supported`.

The overall deep experiment is therefore `failed`, irrespective of whether a
separate crash campaign closes the remaining atomicity evidence. This baseline
is not eligible for a `v0.1.0` release. Improving it requires a new versioned
policy and a newly preregistered holdout; the frozen v1 judgments and thresholds
remain unchanged.

The lab's SQLite replay and exact-byte checks do not by themselves establish
child-process crash atomicity. Until crash-campaign evidence is hash-attached,
`apply_atomic_and_idempotent` and therefore the overall deep result remain
`inconclusive`, even if all empirical hypotheses meet their thresholds.
Accepted crash evidence must cover all 13 ordered, explicit repository fault
points through the actual typed append, seal, and retirement APIs. For every
point it binds the current harness and repository sources and proves hard
termination with the write transaction open, exact rollback to the complete
logical pre-state, equality with an independent control post-state after
recovery, SQLite/FK integrity, and idempotent retirement replay. A raw-SQL
facsimile is not admissible evidence.

The `xtask replay` repetitions are fresh reproofs: each repetition executes
the complete 13-point child-process campaign again and requires exact equality
with the crash attachment before regenerating the receipt and decision ledger.
Validating or rehashing the attachment without executing the campaign is not a
replay pass.
