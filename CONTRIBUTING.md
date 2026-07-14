# Contributing to NAOME Memory

Thank you for helping improve NAOME Memory. This project is an experimental proof system, so contributions must preserve deterministic behavior and make their evidence explicit.

## Before opening a change

Use an issue for changes to the theory contract, public schemas, canonical encoding, storage migrations, retention policy, or empirical thresholds. Keep a pull request to one independently verifiable slice. Do not include real user, repository, or agent memory; fixtures must be synthetic and non-sensitive.

The protected development flow is:

1. Branch from current `main` using `task/<short-description>`.
2. Implement the smallest coherent change.
3. Run the applicable local gates and capture the proof receipt.
4. Open a pull request and complete every relevant template field.
5. Resolve review threads and merge only when all required checks pass.

## Developer Certificate of Origin

Every commit must carry a Developer Certificate of Origin 1.1 sign-off. By adding the line below, you certify that you have the right to submit the contribution under this project's license:

```text
Signed-off-by: Your Name <your.email@example.com>
```

Create it with `git commit --signoff`. The repository's DCO check rejects commits without a valid sign-off. GitHub web commits must use the repository's web commit sign-off option.

The full certificate is available at [developercertificate.org](https://developercertificate.org/).

## Local gates

Use the committed Rust toolchain and a clean lockfile. The schema command requires the Python packages pinned in `.github/schema-requirements.txt`:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo test --workspace --all-features --locked
python3 -m pip install --requirement .github/schema-requirements.txt
python3 scripts/validate_json_schemas.py
cargo run -p xtask -- proof --profile pr --output target/proofs/pr-receipt.json
cargo run -p xtask -- replay --receipt target/proofs/pr-receipt.json --repetitions 2
```

Policy and algorithm changes also require golden, property, differential, and tamper coverage. Storage changes require migration, foreign-key, integrity, stale-plan, retry, and crash-boundary coverage. Release-affecting changes require the deep profile.

## Proof requirements

A pull request that changes a decision must identify:

- the claim being changed;
- the exact policy version and digest;
- every explicit seed used by the evidence;
- the generated receipt and verification result;
- compatibility or migration effects;
- negative and adversarial tests;
- whether calibration or frozen holdout data changed.

Do not tune against `holdout-v1` after observing its results. A digest establishes input/result binding and reproducibility, not semantic correctness. A compiler-green change without its required current receipt is incomplete.

## Style and safety

- Repository language is English.
- Workspace crates forbid unsafe Rust.
- Core decisions do not read system time, global randomness, environment-dependent paths, floating-point values, or unordered map iteration.
- JSON is a projection; canonical domain-separated binary encoding is hash authority.
- `--json` writes one schema object to stdout and diagnostics to stderr.
- New dependencies require supply-chain and license review.
- No production PII, secrets, credentials, or real memory contents belong in issues, tests, receipts, logs, or artifacts.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
