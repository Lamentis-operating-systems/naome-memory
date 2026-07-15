# NAOME Memory mechanism evidence — 2026-07-15

This directory preserves the raw evidence used by Section 13 of the
[NAOME Memory design paper](../../paper/NAOME-MEMORY.md). It supports only the
implemented deterministic PoC mechanisms and their frozen synthetic retrieval
experiment. It is not evidence that memory improves autonomous software
engineering.

## Source and environment

| Field | Recorded value |
| --- | --- |
| Clean execution source commit | `8459c128c177a0939b1c4bf8ad9d94ee8335ad60` |
| Implemented PoC code parent | `646b942501d7a56b53f9e6ce753f7f51ae33b851` |
| Commit subject | `docs: define kernel engineering memory research contract` |
| Commit time | `2026-07-15T23:40:30+02:00` |
| Rust | `rustc 1.95.0 (59807616e 2026-04-14) (Homebrew)` |
| Cargo | `cargo 1.95.0 (f2d3ce0bd 2026-03-21) (Homebrew)` |
| Target | `aarch64-apple-darwin` |
| Operating system | macOS 27.0, build `26A5378j` |
| Host class | MacBook Pro `Mac16,1`, Apple M4, 10 cores, 16 GB RAM |

The execution commit differs from the implemented PoC code parent only by
`README.md` and `paper/NAOME-MEMORY.md`.

`git status --short` was empty immediately before the scale command. The scale
runner independently checked that the same clean full commit remained current
at publication time. Ignored files below `target/` were the only command
outputs until both experiment commands and schema validation had completed.

## Executed commands and outcomes

All commands ran from the repository root with the toolchain above.

| Command | Exit | Outcome |
| --- | ---: | --- |
| `cargo run --release --locked -p xtask -- scale --atoms 100000` | 0 | `scale-receipt-v1` status `passed` |
| `cargo run --release --locked -p xtask -- proof --profile deep --output target/proofs/deep/deep-receipt.json` | 0 | Structurally valid receipt; scientific `overall_status=failed` |
| `cargo run --release --locked -p xtask -- replay --receipt target/proofs/deep/deep-receipt.json --repetitions 2` | 0 | `verified`; two fresh campaigns of 13 crash scenarios |
| `cargo run --release --locked -p xtask -- assert-claims --receipt target/proofs/deep/deep-receipt.json` | 1 | Expected claim-gate rejection: required claims are not all supported |
| `python3 scripts/validate_json_schemas.py` with the bindings below | 0 | Draft 2020-12 validation `passed` |

The exact schema-validation invocation was:

```console
python3 scripts/validate_json_schemas.py \
  --bind schemas/lab-proof-receipt-v1.schema.json=target/proofs/deep/deep-receipt.json \
  --bind schemas/decision-ledger-v1.schema.json=target/proofs/deep/decision-ledger.json \
  --bind schemas/dataset-manifest-v1.schema.json=target/proofs/deep/dataset-manifest-v1.json \
  --bind schemas/scale-receipt-v1.schema.json=target/proofs/scale-receipt.json \
  --bind schemas/proof-receipt-v1.schema.json=target/proofs/scale-core-retirement-receipt.json
```

The proof command intentionally exits successfully after producing and fully
verifying a valid receipt even when a scientific hypothesis is rejected. The
separate `assert-claims` command is the policy gate that fails in that case.

## Scale result

The [scale receipt](scale/scale-receipt.json) records:

| Coordinate | Observed | Declared maximum | Verdict |
| --- | ---: | ---: | --- |
| Atom count | 100,000 | exactly 100,000 for the declared deep scale envelope | passed |
| Wall time | 494,575 ms | 1,800,000 ms | passed |
| Sampled peak RSS | 2,452,963,328 bytes | 6,442,450,944 bytes | passed |
| Persisted temporary-directory bytes | 4,279,762,944 bytes | 10,737,418,240 bytes | passed |

It additionally records successful plan replay, receipt verification, and
idempotent apply. RSS was sampled every 50 ms from the running `xtask` process;
it is an observational value and may miss shorter peaks. Disk is the measured
size of the persisted temporary directory, not a lifetime deployment forecast.

## Frozen deep result

The [deep receipt](deep/deep-receipt.json) records 100 synthetic holdout worlds
and 20,000 paired-bootstrap repetitions. All twelve declared mechanism
invariants are `verified`.

| Hypothesis | Lower 95% bound | Threshold | Verdict |
| --- | ---: | ---: | --- |
| `semantic_utility` | +36,869 ppm | +100,000 ppm | rejected |
| `episodic_rare_recovery` | +100,000 ppm | +80,000 ppm | supported |
| `ordinary_utility_preserved` | +54,186 ppm | -20,000 ppm | supported |

The aggregate result digest is
`4e116a085fdca302ef75c08dd5103718c0847dfd61d065c7320cfab4dc51712a`.
The negative overall status is preserved; it is not a receipt-integrity
failure.

## Artifact hashes

The hashes below are SHA-256 over the exact committed file bytes.

| Artifact | SHA-256 |
| --- | --- |
| [`deep/crash-campaign-evidence.json`](deep/crash-campaign-evidence.json) | `c7c761daf59beec773c84822059b430ff962ba4987d0789b9c46b75f6d804d23` |
| [`deep/dataset-manifest-v1.json`](deep/dataset-manifest-v1.json) | `ea28858459d19689212f3b62a557e2be34c2ebe1fc839448e02348ecbd904190` |
| [`deep/decision-ledger.json`](deep/decision-ledger.json) | `7718c5e52c61ab9aa1d0f5fe827fd089dc6be631bd750f4d9395fda1fe8072a4` |
| [`deep/deep-receipt.json`](deep/deep-receipt.json) | `147160a666dff025c21f45b43663a7931399b634af8bb03a0d092717d4590a29` |
| [`scale/scale-core-retirement-receipt.json`](scale/scale-core-retirement-receipt.json) | `c59cc6547ae07f7d9a4d76ddc3934baf0cc6026b19396d99fad6f2d6ed0418c2` |
| [`scale/scale-receipt.json`](scale/scale-receipt.json) | `aab1148a3089f50bdcb3130e7d735b3f4bd47eebcecce666782f095e8685cf58` |

## Closure boundary

`scale-receipt-v1` directly binds the clean source commit. By contrast,
`lab-proof-receipt-v1` has no full Git-commit field. It binds the policy,
dataset manifest, synthetic generator source, decision ledger, and crash
campaign evidence through declared digests. This manifest records the clean
commit from which the deep command was executed, and Git commits the resulting
artifact bytes, but that is not cryptographic full-tree source closure inside
the deep receipt. A release-grade successor must add the source commit to the
lab receipt schema and verifier.

The synthetic worlds are structurally narrow, the comparison byte cap is
post-hoc, and the rare-retrieval gain is largely implied by the fixture. See
the paper audit before interpreting any metric.
