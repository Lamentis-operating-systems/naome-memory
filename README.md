# NAOME Memory

NAOME Memory is an experimental, deterministic memory-consolidation and episodic-retention engine for the NAOME autonomous agent kernel. It stores rich episode atoms in short-term memory, derives only sufficiently supported semantic structures, and preserves a small, seeded random sample of eligible episodes exactly.

This repository is a proof of concept. It does **not** claim human-like memory, semantic truth, optimal ranking, production handling of personal data, distributed persistence, encryption, or scalability beyond its declared synthetic test workloads.

## Research direction

The current implementation is a mechanism PoC, not yet the memory architecture
needed to demonstrate better autonomous software engineering. The
[`NAOME Memory design paper`](paper/NAOME-MEMORY.md) defines that next system:
an evidence-bound, NSIT-revision-aware engineering experience substrate with a
typed kernel API, explicit authority boundaries, local resource accounting,
and a falsifiable evaluation against strong baselines. It also audits the
current PoC and preserves its rejected semantic-retrieval result.

The paper is an architecture and preregistration blueprint. Its proposed
Engineering Memory contracts are not claims about features already implemented
in this repository.

## Design contract

- Rust 1.95.0, Edition 2024, library-first workspace plus a thin CLI.
- Explicit time (`as_of_us`) and explicit 32-byte seeds; no hidden clock or randomness in logical decisions.
- Immutable atom bodies identified by a domain-separated canonical SHA-256 digest.
- A kernel-wide `memory_space_id`; repository, task, agent, and session identifiers are context filters.
- Deterministic structured semantics only; no LLMs and no embeddings.
- SQLite for indexed state and a content-addressed filesystem for artifacts.
- Synthetic, non-sensitive proof data only.
- Machine-verifiable plans and receipts bind inputs, the validated canonical typed-policy digest, seed, decisions, and resulting state.

The committed [`poc-v1` policy](policies/poc-v1.toml) defines a seven-day STM horizon, a 2% semantic budget, and an independent 0.5% episodic lottery for integrity-complete atoms that permit exact retention. Flashbacks are a separate retrieval channel and are disabled unless a query supplies an explicit seed and budget.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `naome-memory-core` | Pure domain types, canonical encoding, sealing, consolidation, lottery, retrieval ranking, and receipt verification |
| `naome-memory-sqlite` | SQLite persistence, migrations, transactional retirement, FTS5 candidate generation, and artifact CAS |
| `naome-memory-lab` | Independent reference model, synthetic worlds, metrics, and proof receipts |
| `naome-memory-cli` | Thin command-line adapter |
| `xtask` | Identical local and CI proof orchestration |

## Quick start

Install Rust 1.95.0 with `rustfmt`, `clippy`, and `llvm-tools-preview`. Draft 2020-12 schema validation additionally uses Python 3 with the versions pinned in `.github/schema-requirements.txt`. Then run:

```console
cargo test --workspace --locked
python3 -m pip install --requirement .github/schema-requirements.txt
cargo +1.95.0 run --locked -p xtask -- schema-fixtures --output-dir target/schema-fixtures
python3 scripts/validate_json_schemas.py --require-typed-fixtures --bindings-manifest target/schema-fixtures/bindings-v1.json
cargo run -p xtask -- proof --profile pr --output target/proofs/pr-receipt.json
cargo run -p xtask -- replay --receipt target/proofs/pr-receipt.json --repetitions 2
```

Each replay repetition regenerates the lab bundle and freshly executes all 13
real child-process crash scenarios; it is not a receipt-only hash check.

The CLI surface is:

```text
naome-memory db init|check
naome-memory episode create|append|seal
naome-memory cycle plan|apply
naome-memory retrieve
naome-memory feedback add
naome-memory verify receipt
naome-memory artifact gc --dry-run
naome-memory lab run
```

When `--json` is used, stdout contains exactly one object conforming to the Draft 2020-12 [`CLI response schema`](schemas/cli-response-v1.schema.json). That envelope deliberately permits an arbitrary `result`: the operation payload is schema-bound only when it is one of the public core projections listed below. CI generates those fixtures from typed Rust values, runs the real algorithms, and validates each Serde projection against its operation schema. Diagnostics go to stderr. Exit codes are documented in [`docs/architecture.md`](docs/architecture.md).

## Specifications and evidence

The normative PoC contracts are versioned with the code:

- [`Theory Contract`](docs/specs/theory-contract-v1.md)
- [`Memory Atom v1`](docs/specs/memory-atom-v1.md)
- [`Policy v1`](docs/specs/policy-v1.md)
- [`Proof Receipt v1`](docs/specs/proof-receipt-v1.md)
- [`Core retirement receipt schema`](schemas/proof-receipt-v1.schema.json)
- [`Lab experiment receipt schema`](schemas/lab-proof-receipt-v1.schema.json)
- [`Decision ledger schema`](schemas/decision-ledger-v1.schema.json)
- [`Bounded tool receipt schema`](schemas/tool-receipt-v1.schema.json)
- [`Scale receipt schema`](schemas/scale-receipt-v1.schema.json)
- [`Dataset manifest schema`](schemas/dataset-manifest-v1.schema.json)
- Public episode schemas: [`event`](schemas/episode-event-v1.schema.json), [`buffer`](schemas/episode-buffer-v1.schema.json), [`seal request`](schemas/seal-episode-request-v1.schema.json), and [`sealed chain`](schemas/sealed-episode-chain-v1.schema.json)
- Public retirement schemas: [`snapshot`](schemas/retirement-snapshot-v1.schema.json) and [`plan`](schemas/retirement-plan-v1.schema.json)
- Public retrieval schemas: [`query`](schemas/retrieval-query-v1.schema.json) and [`result`](schemas/retrieval-result-v1.schema.json)
- Public receipt-verification schemas: [`inputs`](schemas/receipt-verification-inputs-v1.schema.json) and [`verified result`](schemas/verified-receipt-v1.schema.json)
- [`Architecture`](docs/architecture.md)
- [`Storage model`](docs/storage.md)
- [`Release process`](docs/release.md)
- [`Non-claims`](docs/non-claims.md)

A digest proves reproducibility and closure over declared inputs; it does not prove the semantic theory. Current evidence is produced by the test and lab commands. This README intentionally makes no claim that release thresholds have already passed.

## Contributing and security

Contributions are accepted under Apache-2.0 and require a [Developer Certificate of Origin sign-off](CONTRIBUTING.md). See [`SECURITY.md`](SECURITY.md) for private vulnerability reporting. Do not report real personal or sensitive memory data; this PoC accepts synthetic fixtures only.
