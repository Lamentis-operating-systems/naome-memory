# Fuzz targets

These targets are intentionally outside the workspace. They cover the JSON
domain decoder, canonical authority encoding, receipt verifier, and a real
SQLite draft/event ingest plus integrity-check boundary.

The `fuzz/Cargo.toml` manifest is owned by the workspace bootstrap so action
pinning and dependency policy remain centralized. Once that manifest is
present, run a target with, for example:

```text
cargo fuzz run canonical -- -max_total_time=120
```

No corpus output is an authority artifact. Crash reproducers are copied into
`target/proofs/fuzz-artifacts/<target>/` and bound into the fuzz receipt by
relative path, byte length, and SHA-256 digest. Release verification rejects
missing, changed, extra, cross-target, oversized, or unbound files. A real
product failure must still be minimized and promoted into a deterministic test
fixture before it can support a claim.

`xtask verify-tool-receipt` verifies the structural closure of both successful
and failed campaigns. Release preflight is stricter: it accepts only a passed
campaign and requires its crash-artifact set to be empty. Runner termination,
timeout, or an I/O failure before receipt publication is explicitly
inconclusive; partial files are diagnostic only and cannot satisfy a gate.
