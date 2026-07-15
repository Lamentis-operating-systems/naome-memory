# Release Process

NAOME Memory v0.x releases use manually created annotated Git tags followed by a gated GitHub Actions release. Cryptographic tag signing is not required. The project is not published to crates.io, and v0.1 has no Windows artifact.

## Preconditions

- `main` is protected and the exact commit has green required checks.
- The scheduled deep proof for that commit passed, including the 100,000-atom envelope.
- Every mechanical invariant is `verified` and every required empirical hypothesis is `supported` in the frozen holdout receipt.
- `Cargo.lock`, theory contract, `poc-v1` policy, dataset manifest, schemas, and changelog match the candidate.

Run locally:

```console
cargo run -p xtask -- release-preflight --version 0.1.0
cargo run --release -p xtask -- proof --profile deep --output target/proofs/release-receipt.json
cargo run --release -p xtask -- replay --receipt target/proofs/release-receipt.json --repetitions 2
```

Each replay repetition includes a fresh execution of all 13 child-process
crash scenarios and must reproduce the crash evidence attached by `proof`.
Thus the command above performs 26 hard-termination scenarios in addition to
two complete deterministic lab-bundle recomputations. Runtime is host
observation only and is not hashed into the proof.

Create an ordinary annotated tag after every release precondition is satisfied:

```console
git tag --no-sign -a v0.1.0 -m "NAOME Memory v0.1.0"
git push origin v0.1.0
```

The tag workflow independently verifies that the ref is an annotated tag bound
to the workflow commit, requires that its commit belongs to `main`, and checks
that the latest required CI aggregator plus both CodeQL jobs succeeded on that
commit. It then repeats deep proof on the exact tagged commit, builds Linux
x86_64 GNU and macOS arm64 binaries, produces SHA-256 checksums and CycloneDX
SBOMs, attaches the Theory Contract, policy, generated dataset manifest,
receipt, and decision ledger, creates GitHub artifact attestations, and then
creates the GitHub Release.

The final `gh release create --verify-tag` flag only requires the already-pushed
remote tag to exist. It does not perform cryptographic signature verification.

Mutation and fuzz evidence must also close over the exact tag commit, pinned
toolchain and tool versions, exact commands, all required targets, exit codes,
durations, and retained output-log digests. A JSON object that merely says
`passed` is not accepted by release preflight.

The GitHub Release includes `tool-evidence-v1.tar.gz`, which preserves the
receipt verifier's `tool-logs/` and `fuzz-artifacts/` directory structure in
addition to the convenient standalone receipt files. Its checksum and build
attestation are published with the other assets. A release-eligible fuzz
receipt has an empty crash-artifact set; failed crash receipts remain
structurally verifiable evidence but can never satisfy release preflight.

If tag validation, closure, an invariant, a hypothesis, scale, build, checksum, SBOM, or attestation step fails, no release is created. Correct the source through a new pull request and create a new version tag; do not move or replace an existing release tag.

The current frozen `holdout-v1` evidence rejects `semantic_utility`: its lower
95% confidence bound is `+36_869 ppm`, below the preregistered `+100_000 ppm`
threshold. The overall deep proof is therefore `failed`, and no `v0.1.0` tag
or release is permitted from this baseline. Attaching a complete child-process
crash campaign can verify atomicity evidence, but cannot change that empirical
verdict. Any improvement must use a newly versioned policy and preregistered
holdout rather than rewriting `holdout-v1`.
