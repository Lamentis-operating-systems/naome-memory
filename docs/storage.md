# Storage Model

## SQLite profile

The local store uses bundled SQLite, foreign keys, WAL mode, `STRICT` tables, and explicit transactions. The adapter configures a finite busy timeout and uses `BEGIN IMMEDIATE` for schema decisions and retirement apply. `PRAGMA integrity_check` and `PRAGMA foreign_key_check` are distinct checks and both must pass.

WAL is a single-host choice. Database files and the artifact directory must reside on a local filesystem with reliable atomic rename and file synchronization. Network filesystems and concurrent writers on separate hosts are outside the PoC contract.

## Logical table groups

Migrations provide tables for:

- schema versions and validated committed policy projections/canonical typed digests;
- open episode drafts and ordered events;
- immutable atom headers, bodies, complete provenance payloads, and ordered source-digest edges;
- mutable atom state and typed relations;
- FTS5 and structured search projections;
- validated feedback counters;
- retirement runs, cohort members, per-atom decisions, and immutable historical post-states;
- proof receipts and their closure members;
- artifact metadata, atom references, exact-retention pins, and GC marks.

Immutable atom bodies are inserted once. Public writes accept typed
`MemoryAtomBodyV1` plus `AtomStateV1`; the bounded batch path is deliberately
limited to unlinked typed atoms, while linked writes validate CAS and relation
closure. Raw `AtomRecord` persistence helpers exist only inside store tests, so
an external caller cannot supply JSON, header, or search fields that diverge
from canonical typed authority. Database constraints and adapter checks reject
a different body for an existing `AtomId`. Forgetting removes the episode body
only after a successful retirement transaction; the header, body digest,
canonical provenance payload, ordered source-event/body digest edges, relation
edges, and decision remain.

Initial typed insertion rejects nonzero feedback/retrieval counters,
supersession, a preexisting retirement-run binding, incoherent kind/tier
combinations, and any STM expiry other than `recorded_at_us + 7 days` instead
of silently discarding them. Mutable changes go through their dedicated feedback,
retrieval, and retirement transitions. Artifact, provenance, and relation rows are derived
from the immutable body; there is no public raw API for adding undeclared
links. A split episode chain inserts all bodies before resolving its in-chain
`continues` edges, then seals the draft in the same transaction.

The lab-only `lab-fault-injection` feature exposes synchronous observers at 13
named boundaries inside the same typed append, seal, and retirement
transactions used by production. It has no environment-driven or global
activation path. The crash child blocks at the selected observer while the
real `BEGIN IMMEDIATE` transaction remains open, and only its parent terminates
the process. Recovery compares every logical table with both the exact
pre-state and an independently committed control post-state, then reruns
integrity checks and the idempotent retirement apply. These observers are a
proof surface, not a runtime failure policy.

CAS recovery has a separate lab-only observer after a durable GC unlink and
before its SQLite commit. It is deliberately not added to the ordered
13-point append/seal/retirement receipt contract. The SQLite crash suite uses
purpose-built hard-killed children to cover both finalized CAS bytes before
registration and the post-unlink/pre-commit GC state, then reopens the store,
replays recovery, and requires a clean integrity result.

## Content-addressed artifacts

Artifacts are addressed by SHA-256 and stored under:

```text
artifacts/sha256/<first-two-hex>/<next-two-hex>/<64-hex-digest>
```

Ingest creates a temporary file inside the destination filesystem, streams bytes while hashing, enforces the 64 MiB artifact limit, and synchronizes its content before an atomic rename to the digest path. Shard directories are created one level at a time and each new directory entry is synchronized through its parent. Because the rename crosses from `.tmp` into a shard, both the destination shard and the staging directory are synchronized. An existing path is accepted only after its regular-file type, length, and digest match; symbolic links are rejected. Host paths are never part of atom IDs, plans, or receipts.

Inline payloads are limited to 4 KiB. A sealed atom is limited to 256 KiB, with 128 KiB as the target; larger episodes are split into a `continues` chain. An exact episodic package is limited to 64 MiB including the body and retention-permitted artifacts.

## Artifact closure and garbage collection

An atom references artifact digest, size, media type, provenance, and whether retention permits pinning it. Exact episodic retention pins every permitted referenced artifact as part of the same retirement transaction and removes live references to disallowed external bytes while retaining their digest metadata. A retention-disallowed inline payload cannot enter the exact-eligible population because inline bytes are part of the immutable body. Missing, mismatched, or oversized closure rejects the whole plan.

Garbage collection is two stage. A scan transaction records currently unreferenced digests with a mark time supplied explicitly by the caller. The scan also walks and hashes the canonical CAS tree: a finalized digest-path file without a SQLite row (the crash state after rename but before registration) is adopted as verified, unreferenced metadata and receives the same mark and 24-hour grace period. Non-canonical paths, non-regular files, oversize files, and digest mismatches fail closed. `artifact gc --dry-run` reports these orphans as `would_mark` without mutating metadata or files.

A later sweep runs under `BEGIN IMMEDIATE`, freshly confirms that each matured candidate is still unreferenced and unpinned, deletes its SQLite row inside the uncommitted transaction, verifies and unlinks the CAS file, synchronizes the shard, and only then commits SQLite. This ordering prevents a committed metadata deletion from hiding an undeleted file. If the process stops after the durable unlink but before SQLite commit, the rolled-back marked row and missing file form an explicit recoverable state: the next sweep completes the metadata deletion. A missing file is tolerated only in that marked, unreferenced GC path; referenced and pinned artifact closure remains fail-closed.

Pre-rename `.tmp/*.partial` files are not finalized artifacts and never become atom authority. Normal error paths remove and synchronize them. Automatic reclamation after process death is intentionally not claimed in v0.1 because safely distinguishing an abandoned staging writer from a live concurrent ingest requires a lease protocol that this single-host PoC does not define.

## Capacity envelope

The scale contract is a maximum of 100,000 atoms in one UTC retirement cohort. `scale-v1` uses atom body sizes with P50 16 KiB, P95 64 KiB, and maximum 256 KiB. The release gate is at most 30 minutes, 6 GiB resident memory, and 10 GiB disk. Exceeding the count or a byte budget is a rejected plan, not a best-effort partial retirement.

Storage demand is workload-dependent. Roughly, before retirement it is the sum of SQLite/index overhead, atom bodies, artifact bytes, and WAL headroom. After retirement, non-selected episode bodies are removed but their small headers and proof/provenance records remain. The v0.1 scale receipt binds logical body bytes and reports total persisted-directory bytes, sampled peak RSS, and wall time as explicitly observational fields; it does not claim per-table or per-index attribution.
