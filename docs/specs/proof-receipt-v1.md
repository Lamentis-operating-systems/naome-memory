# Proof Receipts v1

Status: normative for the two distinct v1 receipt contracts.

## Core retirement receipt: `proof-receipt-v1`

`naome-memory-core::ProofReceiptV1` is the closure record for one retirement
plan applied by a repository. Its JSON projection is described by
`schemas/proof-receipt-v1.schema.json`; its hash authority is the core's
canonical binary encoding.

The receipt binds its domain-derived run ID, memory space, cohort and explicit
`as_of_us`, `poc-v1` policy ID and digest, 32-byte seed,
input/population/plan/decision/state digests, exact
episode and artifact closure, and the narrowly observed structural statuses
`plan_structure_verified`, `budgets_structurally_bounded`, and
`repository_outcome_closure_verified`. Its ID is:

```text
SHA256("naome-memory:proof-receipt:v1\0" || canonical_core_receipt_authority)
```

`verify_receipt` independently recomputes the retirement plan from the bound
policy, snapshot, and seed, derives the expected run ID from the recomputed
plan digest, and compares every receipt identity field. The state digest is the
digest of a canonical logical post-state manifest: immutable header-body
authority, body presence and digest, complete mutable state, retirement
binding, search-projection presence, and artifact pins for every source and
semantic output. SQLite persists this manifest as immutable historical
evidence and rebuilds the live structural projection from committed rows
rather than echoing the planned digest. Feedback and retrieval counters may
advance after retirement; current counters must remain monotonic and feedback
remains closed by its event rows, while the historical receipt digest stays
unchanged. SQLite additionally rehashes retained bodies and their
typed JSON/search projections, requires forgotten
bodies/projections/live-artifact references to be absent, and verifies exact
decision/member closure. Missing sources, artifacts, decisions, or observed
post-state make verification fail closed. The core receipt does not contain
empirical metrics and does not claim that a store transaction survived a
process crash; those are separate evidence surfaces.

## Lab experiment receipt: `lab-proof-receipt-v1`

`naome-memory-lab::LabProofReceiptV1` is the experiment envelope described by
`schemas/lab-proof-receipt-v1.schema.json`. It is deliberately not called
`proof-receipt-v1` because it binds a different authority graph:

- profile-to-dataset mapping (`pr` to `calibration-v1`, `deep` to
  `holdout-v1`);
- policy, dataset-manifest, and generator-source digests;
- per-world query judgments, derived seeds, input/result digests, and byte
  comparator evidence in the separate decision ledger;
- golden fate scenarios;
- paired fixed-point metrics, confidence bounds, invariant/hypothesis statuses,
  and overall experiment status.

Its digest is:

```text
SHA256("naome-memory:lab-proof-receipt:v1\0" || canonical_lab_receipt_without_result_digest)
```

The decision ledger uses:

```text
SHA256("naome-memory:decision-ledger:v1\0" || canonical_ledger_without_ledger_digest)
```

`verify_lab_bundle` requires the receipt and its ledger, then independently
regenerates, persists, and evaluates every committed profile world. It
recomputes the exact manifest qrels, hit scores, byte budgets, world results,
golden fates, paired metrics, bootstrap bounds, statuses, ledger digests, and
overall outcome. A rehashed alteration still diverges from this replay. A
standalone receipt check validates only its metadata and self-digest; it cannot
prove that its ledger attachment exists or independently justify semantic
values. Evidence claims therefore require the bundle verifier.

`cargo xtask replay --repetitions N` is a stronger orchestration contract than
bundle verification alone. Every repetition freshly executes all 13 real
child-process crash scenarios, requires the resulting typed campaign evidence
to equal the campaign attached to the supplied closure, and only then
regenerates and compares the complete lab bundle. `N` therefore counts full
proof-and-crash reproofs, not repeated hashing of one historical crash object.
The extra process runtime is observational and never enters a logical digest.

## Evidence boundary

Neither digest proves semantic truth or statistical adequacy. The current lab
runs real SQLite apply/replay and compares exact winner body bytes and CAS
artifact bytes, but it marks `apply_atomic_and_idempotent` inconclusive until a
separate child-process crash campaign digest is attached to the lab closure.
The campaign covers the exact ordered repository fault-point contract: two
append boundaries, two seal boundaries, and retirement boundaries after run
creation, semantic insertion, decision persistence, state mutation,
forgetting, artifact pins, receipt insertion, run completion, and immediately
before commit. Each child parks inside the real `BEGIN IMMEDIATE` call stack
and is hard-terminated by its parent. Evidence binds both harness and
repository source digests, the action digest, exact logical pre/crash/replayed
state digests, an independently committed control post-state, SQLite/FK
checks, and idempotent retirement replay. Successful bundle replay alone is
not crash-atomicity evidence. A successful `xtask replay`, however, includes a
fresh crash campaign per repetition and therefore reproves this separately
attached evidence before accepting the deterministic receipt replay.

Host paths, wall-clock runtime, logs, thread order, and JSON formatting never
participate in logical digests. Observational performance evidence must remain
separately labeled.

Mutation and fuzz campaigns use the separate `tool-receipt-v1` envelope. It
binds the exact source commit, pinned tool and Rust versions, argument vector,
requested and observed duration, process exit code, and SHA-256 digests of
retained stdout and stderr logs. Release preflight re-hashes those logs and
rejects a status-only, cross-commit, wrong-command, or incomplete receipt.
Fuzz crash reproducers are additionally copied into bounded proof storage and
bound by relative path, exact byte length, and SHA-256 digest. Verification
requires exact directory closure, including the absence of unbound files.
Structural verification accepts a status-consistent failed campaign so its
reproducer remains independently auditable; release acceptance separately
requires `passed`, zero exit codes, positive bounded durations, and no crash
reproducers. A runner-level kill before publication produces no receipt and is
therefore inconclusive rather than failed or passed.

The separate `scale-receipt-v1` envelope binds its source commit and a
full-evidence digest over the logical scale closure plus explicitly labeled
wall-time, RSS, disk, and phase observations. Its logical digest intentionally
excludes host observations so Linux and macOS can compare the same decisions;
release preflight still requires the full evidence digest and exact tag commit.
At 100,000 atoms, a missing required resource measurement or a source tree that
is not clean and bound to the same full commit at start and publication is
`inconclusive`, while an observed threshold breach is `failed`; neither status
satisfies the pass-only verifier. Once the full logical path has completed, the
harness structurally verifies the core binding and scale closure, publishes the
core receipt followed by the scale manifest, and only then returns a nonzero
status. Rejected evidence therefore remains inspectable instead of leaving an
older receipt in place.

## Tamper behavior

An unknown contract, mismatched policy/seed/profile/dataset, changed generator
source, changed manifest, reordered or missing world, changed query evidence,
missing golden fate scenario, or mismatched ledger/receipt digest is invalid,
not a rejected hypothesis.
