# Fuzz targets

These targets are intentionally outside the workspace. They cover the JSON
domain decoder, canonical authority encoding, receipt verifier, and a real
SQLite draft/event ingest plus integrity-check boundary.

The `fuzz/Cargo.toml` manifest is owned by the workspace bootstrap so action
pinning and dependency policy remain centralized. Once that manifest is
present, run a target with, for example:

```text
cargo fuzz run canonical --locked -- -max_total_time=120
```

No corpus output is an authority artifact. A minimized regression must be
promoted into a deterministic test fixture before it can support a claim.
