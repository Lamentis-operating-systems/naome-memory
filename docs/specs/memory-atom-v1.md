# Memory Atom v1

Status: normative for schema `naome-memory.atom.v1`.

## Identity and immutability

A memory atom consists of an immutable body, an immutable header derived from that body, and mutable state stored separately. Draft events are not atoms. Once sealed, a body is never updated; corrections and late events form new atoms connected by typed provenance relations.

```text
AtomId = SHA256("naome-memory:atom:v1\0" || canonical_atom_body)
```

`canonical_atom_body` is the typed canonical binary tree defined below. The SHA-256 digest is encoded as 64 lowercase hexadecimal characters at JSON boundaries. JSON serialization is a readable projection and is never hash authority.

## Body

`MemoryAtomBodyV1` contains:

- `kind`: `episode` or `semantic`;
- `scope`: required `memory_space_id` and optional repository, task, agent, and session context;
- `interval`: `started_at_us`, `ended_at_us`, and `recorded_at_us` integer microseconds;
- episode boundary data: trigger and seal reason;
- goal, plan, and internal state before and after;
- observations, interpretations, and beliefs as separate collections;
- decisions, rejected alternatives, and actions;
- outcome, feedback, and formation signals;
- structured `topic_keys`, `entity_ids`, optional `outcome_class`, and optional `goal_key`;
- source provenance, typed relations, and artifact references;
- `retention_permission`: `transient_only`, `semantic_allowed`, or `semantic_and_exact_allowed`;
- episode continuation information or semantic consolidation evidence, as appropriate to `kind`.

Text is byte-exact valid UTF-8 and receives no Unicode normalization, case folding, or whitespace rewriting during sealing. Maps use lexical byte-order keys. Scores and probabilities use `Ppm`, an integer from zero through 1,000,000 inclusive.

An `episode` body records direct episode material and may identify a preceding atom through `continues`. A `semantic` body contains no generated natural-language assertion. It contains only deterministic structure with at least two-thirds source support, its source interval, support count, sorted source atom IDs and body digests, score breakdown, and optionally one superseded semantic atom.

## Canonical typed binary tree

The encoder first maps the body to a closed typed tree. Floats and unspecified values are invalid. Each value starts with a one-byte type tag. Integers are fixed-width big-endian. Byte-string and UTF-8 string lengths, array counts, and object counts are unsigned big-endian 64-bit integers. Every array element and every object value is additionally length framed with an unsigned big-endian 64-bit byte length.

Objects include their schema field names as UTF-8 keys. All keys, including struct field names and map keys, are sorted by raw UTF-8 bytes before encoding. Duplicate keys are invalid. This makes canonical bytes independent of Rust declaration order and JSON object order. Arrays retain their declared order; fields whose semantics are sets are sorted and duplicate-free before they enter the canonical tree.

The schema version is present in the body tree. Encoders reject unknown enum variants, invalid UTF-8, out-of-range `Ppm`, oversize lengths, and floats. The PoC does not expose a canonical-binary decoder: persisted JSON projections are decoded into the closed typed model, re-encoded canonically, and checked against the authoritative digest before use.

## Sealing

`seal_episode(buffer, request, policy)` validates:

- one stable `memory_space_id` and compatible context on every event;
- contiguous sequence numbers with no duplicates or gaps;
- non-decreasing event time and a coherent episode interval;
- a recognized boundary: `goal_completed`, `terminal_failure`, `goal_changed`, `context_changed`, `checkpoint`, `handoff`, or `timeout`;
- required episode content and retention permission;
- artifact digest and size closure;
- observation-to-artifact declaration closure, observation time bounds, unique relations, and exact `continues` closure;
- target and hard byte limits.

The target body size is 128 KiB and the hard body limit is 256 KiB. Inline payload is at most 4 KiB, one artifact is at most 64 MiB, and an exact package is at most 64 MiB. Oversize episode material must be sealed through `seal_episode_chain`; `seal_episode` rejects it. The chain planner hashes and measures each event once, then selects the fewest globally complete target-sized segments; a tie selects the longest earliest prefix. Every segment requires an observation, decision, or action unless shared outcome or feedback evidence supplies the segment's substantive content. `continues` is derived only from the input predecessor and the immediately preceding sealed member, never from a stale requested relation. The concatenated source-event digests and inclusive sequence ranges of the chain must reproduce the original ordered event stream exactly. A single event, shared metadata block, or event layout with no complete target-sized partition is rejected rather than truncated.

## Mutable state

Mutable state is not part of `AtomId`. It contains retention tier, content status, expiry time, optional supersession, validated positive and negative feedback counts, retrieval count, and retirement run. The retirement transition and its historical post-state are receipt-bound. Later feedback and retrieval counters are separately validated live transitions and do not rewrite that historical digest. Retrieval count alone never changes validated utility.

Forgetting an unselected episode removes its body bytes but retains its atom header, body digest, scope, interval, canonical provenance payload, ordered source-event digests, typed relation edges, and retirement decision. Exact episodic retention preserves the original body bytes and all permitted artifact bytes without rewriting. Retention-disallowed external bytes lose their live reference and become GC-eligible; exact-eligible atoms may not embed such bytes inline.
