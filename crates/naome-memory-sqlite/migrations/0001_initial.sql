CREATE TABLE schema_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE policies (
    policy_id TEXT PRIMARY KEY NOT NULL,
    policy_digest BLOB NOT NULL CHECK(length(policy_digest) = 32),
    canonical_body BLOB NOT NULL,
    json_projection TEXT NOT NULL CHECK(json_valid(json_projection)),
    installed_at_us INTEGER NOT NULL CHECK(installed_at_us >= 0)
) STRICT;

CREATE TABLE episode_drafts (
    draft_id TEXT PRIMARY KEY NOT NULL,
    memory_space_id TEXT NOT NULL,
    repository_id TEXT,
    task_id TEXT,
    agent_id TEXT,
    session_id TEXT NOT NULL,
    created_at_us INTEGER NOT NULL CHECK(created_at_us >= 0),
    next_sequence INTEGER NOT NULL DEFAULT 0 CHECK(next_sequence >= 0),
    status TEXT NOT NULL CHECK(status IN ('open', 'sealed', 'abandoned')),
    sealed_atom_id BLOB CHECK(sealed_atom_id IS NULL OR length(sealed_atom_id) = 32)
) STRICT;

CREATE INDEX episode_drafts_scope_idx
    ON episode_drafts(memory_space_id, repository_id, status);

CREATE TABLE episode_events (
    draft_id TEXT NOT NULL REFERENCES episode_drafts(draft_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK(sequence >= 0),
    event_at_us INTEGER NOT NULL CHECK(event_at_us >= 0),
    event_kind TEXT NOT NULL,
    canonical_payload BLOB NOT NULL,
    payload_digest BLOB NOT NULL CHECK(length(payload_digest) = 32),
    PRIMARY KEY(draft_id, sequence)
) STRICT, WITHOUT ROWID;

CREATE TABLE atom_headers (
    atom_id BLOB PRIMARY KEY NOT NULL CHECK(length(atom_id) = 32),
    atom_kind TEXT NOT NULL CHECK(atom_kind IN ('episode', 'semantic')),
    memory_space_id TEXT NOT NULL,
    repository_id TEXT,
    task_id TEXT,
    agent_id TEXT,
    session_id TEXT,
    recorded_at_us INTEGER NOT NULL CHECK(recorded_at_us >= 0),
    body_digest BLOB NOT NULL CHECK(length(body_digest) = 32),
    body_bytes INTEGER NOT NULL CHECK(body_bytes >= 0 AND body_bytes <= 262144),
    retention_permission TEXT NOT NULL CHECK(retention_permission IN (
        'transient_only', 'semantic_allowed', 'semantic_and_exact_allowed'
    )),
    provenance_digest BLOB NOT NULL CHECK(length(provenance_digest) = 32)
) STRICT;

CREATE INDEX atom_headers_cohort_idx
    ON atom_headers(memory_space_id, recorded_at_us, atom_id);
CREATE INDEX atom_headers_repository_idx
    ON atom_headers(memory_space_id, repository_id, atom_id);

CREATE TABLE atom_bodies (
    atom_id BLOB PRIMARY KEY NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    canonical_body BLOB NOT NULL,
    json_projection TEXT NOT NULL CHECK(json_valid(json_projection)),
    CHECK(length(canonical_body) <= 262144)
) STRICT;

CREATE TABLE atom_state (
    atom_id BLOB PRIMARY KEY NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    retention_tier TEXT NOT NULL CHECK(retention_tier IN ('stm', 'ltm_semantic', 'ltm_episodic', 'ltm_dual', 'forgotten')),
    content_status TEXT NOT NULL CHECK(content_status IN ('present', 'forgotten', 'quarantined')),
    expires_at_us INTEGER CHECK(expires_at_us IS NULL OR expires_at_us >= 0),
    superseded_by BLOB REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    feedback_positive INTEGER NOT NULL DEFAULT 0 CHECK(feedback_positive >= 0),
    feedback_negative INTEGER NOT NULL DEFAULT 0 CHECK(feedback_negative >= 0),
    retrieval_count INTEGER NOT NULL DEFAULT 0 CHECK(retrieval_count >= 0),
    retirement_run_id BLOB CHECK(retirement_run_id IS NULL OR length(retirement_run_id) = 32),
    CHECK(superseded_by IS NULL OR length(superseded_by) = 32),
    CHECK(content_status != 'present' OR retention_tier != 'forgotten')
) STRICT;

CREATE INDEX atom_state_expiry_idx ON atom_state(retention_tier, expires_at_us, atom_id);

-- Full immutable provenance authority survives body forgetting. The indexed
-- source rows make individual source-event/body digests traversable without
-- treating JSON as hash authority.
CREATE TABLE atom_provenance (
    atom_id BLOB PRIMARY KEY NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    producer TEXT NOT NULL,
    policy_digest BLOB NOT NULL CHECK(length(policy_digest) = 32),
    canonical_provenance BLOB NOT NULL,
    json_projection TEXT NOT NULL CHECK(json_valid(json_projection)),
    provenance_digest BLOB NOT NULL CHECK(length(provenance_digest) = 32)
) STRICT;

CREATE TABLE atom_provenance_sources (
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    source_event_digest BLOB NOT NULL CHECK(length(source_event_digest) = 32),
    PRIMARY KEY(atom_id, ordinal),
    UNIQUE(atom_id, source_event_digest)
) STRICT, WITHOUT ROWID;

CREATE INDEX atom_provenance_source_digest_idx
    ON atom_provenance_sources(source_event_digest, atom_id);

CREATE TABLE atom_relations (
    source_atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    target_atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    relation_kind TEXT NOT NULL,
    relation_digest BLOB NOT NULL CHECK(length(relation_digest) = 32),
    PRIMARY KEY(source_atom_id, target_atom_id, relation_kind)
) STRICT, WITHOUT ROWID;

CREATE TABLE search_projection (
    atom_id BLOB PRIMARY KEY NOT NULL REFERENCES atom_headers(atom_id) ON DELETE CASCADE,
    memory_space_id TEXT NOT NULL,
    repository_id TEXT,
    searchable_text TEXT NOT NULL,
    topic_keys_json TEXT NOT NULL CHECK(json_valid(topic_keys_json)),
    entity_ids_json TEXT NOT NULL CHECK(json_valid(entity_ids_json)),
    outcome_class TEXT,
    projection_digest BLOB NOT NULL CHECK(length(projection_digest) = 32)
) STRICT;

CREATE INDEX search_projection_scope_idx
    ON search_projection(memory_space_id, repository_id, atom_id);

CREATE VIRTUAL TABLE atom_search USING fts5(
    atom_id UNINDEXED,
    searchable_text,
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TABLE feedback (
    feedback_id BLOB PRIMARY KEY NOT NULL CHECK(length(feedback_id) = 32),
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    as_of_us INTEGER NOT NULL CHECK(as_of_us >= 0),
    outcome INTEGER NOT NULL CHECK(outcome IN (-1, 1)),
    source TEXT NOT NULL,
    evidence_digest BLOB NOT NULL CHECK(length(evidence_digest) = 32)
) STRICT;

CREATE INDEX feedback_atom_idx ON feedback(atom_id, as_of_us, feedback_id);

CREATE TABLE retirement_runs (
    run_id BLOB PRIMARY KEY NOT NULL CHECK(length(run_id) = 32),
    memory_space_id TEXT NOT NULL,
    cohort_day INTEGER NOT NULL,
    as_of_us INTEGER NOT NULL CHECK(as_of_us >= 0),
    policy_id TEXT NOT NULL REFERENCES policies(policy_id) ON DELETE RESTRICT,
    policy_digest BLOB NOT NULL CHECK(length(policy_digest) = 32),
    seed BLOB NOT NULL CHECK(length(seed) = 32),
    rng_id TEXT NOT NULL,
    input_set_digest BLOB NOT NULL CHECK(length(input_set_digest) = 32),
    store_snapshot_digest BLOB NOT NULL CHECK(length(store_snapshot_digest) = 32),
    population_digest BLOB NOT NULL CHECK(length(population_digest) = 32),
    plan_digest BLOB NOT NULL CHECK(length(plan_digest) = 32),
    projected_state_digest BLOB NOT NULL CHECK(length(projected_state_digest) = 32),
    status TEXT NOT NULL CHECK(status IN ('applying', 'applied', 'rejected')),
    started_at_us INTEGER NOT NULL CHECK(started_at_us >= 0),
    completed_at_us INTEGER CHECK(completed_at_us IS NULL OR completed_at_us >= started_at_us),
    receipt_digest BLOB REFERENCES proof_receipts(receipt_digest) ON DELETE RESTRICT
        CHECK(receipt_digest IS NULL OR length(receipt_digest) = 32),
    UNIQUE(memory_space_id, cohort_day)
) STRICT;

CREATE TABLE retirement_members (
    run_id BLOB NOT NULL REFERENCES retirement_runs(run_id) ON DELETE RESTRICT,
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    pre_state_digest BLOB NOT NULL CHECK(length(pre_state_digest) = 32),
    PRIMARY KEY(run_id, atom_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE retirement_decisions (
    run_id BLOB NOT NULL REFERENCES retirement_runs(run_id) ON DELETE RESTRICT,
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    decision TEXT NOT NULL CHECK(decision IN ('semantic_source', 'episodic_winner', 'semantic_and_episodic', 'forgotten', 'unchanged')),
    canonical_details BLOB NOT NULL,
    details_digest BLOB NOT NULL CHECK(length(details_digest) = 32),
    PRIMARY KEY(run_id, atom_id)
) STRICT, WITHOUT ROWID;

-- Historical retirement state is immutable receipt evidence. Live feedback
-- and retrieval counters may advance afterwards without rewriting history.
CREATE TABLE retirement_post_states (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES retirement_runs(run_id) ON DELETE RESTRICT,
    state_digest BLOB NOT NULL CHECK(length(state_digest) = 32),
    canonical_state BLOB NOT NULL,
    json_projection TEXT NOT NULL CHECK(json_valid(json_projection))
) STRICT;

CREATE TABLE proof_receipts (
    receipt_digest BLOB PRIMARY KEY NOT NULL CHECK(length(receipt_digest) = 32),
    run_id BLOB NOT NULL REFERENCES retirement_runs(run_id) ON DELETE RESTRICT,
    canonical_receipt BLOB NOT NULL,
    json_projection TEXT NOT NULL CHECK(json_valid(json_projection)),
    closure_digest BLOB NOT NULL CHECK(length(closure_digest) = 32),
    created_at_us INTEGER NOT NULL CHECK(created_at_us >= 0),
    UNIQUE(run_id)
) STRICT;

CREATE TABLE artifacts (
    artifact_digest BLOB PRIMARY KEY NOT NULL CHECK(length(artifact_digest) = 32),
    byte_len INTEGER NOT NULL CHECK(byte_len >= 0 AND byte_len <= 67108864),
    relative_path TEXT NOT NULL UNIQUE,
    integrity_status TEXT NOT NULL CHECK(integrity_status IN ('verified', 'quarantined')),
    created_at_us INTEGER NOT NULL CHECK(created_at_us >= 0),
    gc_marked_at_us INTEGER CHECK(gc_marked_at_us IS NULL OR gc_marked_at_us >= created_at_us)
) STRICT;

CREATE TABLE artifact_refs (
    artifact_digest BLOB NOT NULL REFERENCES artifacts(artifact_digest) ON DELETE RESTRICT,
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    retention_allowed INTEGER NOT NULL CHECK(retention_allowed IN (0, 1)),
    PRIMARY KEY(artifact_digest, atom_id, role)
) STRICT, WITHOUT ROWID;

CREATE INDEX artifact_refs_atom_idx ON artifact_refs(atom_id, artifact_digest);

-- Immutable provenance survives body forgetting and artifact-file garbage
-- collection. It intentionally does not reference the live `artifacts` row.
CREATE TABLE artifact_provenance_edges (
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    artifact_digest BLOB NOT NULL CHECK(length(artifact_digest) = 32),
    role TEXT NOT NULL,
    retention_allowed INTEGER NOT NULL CHECK(retention_allowed IN (0, 1)),
    PRIMARY KEY(atom_id, artifact_digest, role)
) STRICT, WITHOUT ROWID;

CREATE INDEX artifact_provenance_digest_idx
    ON artifact_provenance_edges(artifact_digest, atom_id);

CREATE TABLE artifact_pins (
    artifact_digest BLOB NOT NULL REFERENCES artifacts(artifact_digest) ON DELETE RESTRICT,
    run_id BLOB NOT NULL REFERENCES retirement_runs(run_id) ON DELETE RESTRICT,
    atom_id BLOB NOT NULL REFERENCES atom_headers(atom_id) ON DELETE RESTRICT,
    pinned_at_us INTEGER NOT NULL CHECK(pinned_at_us >= 0),
    PRIMARY KEY(artifact_digest, run_id, atom_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX artifact_pins_run_idx ON artifact_pins(run_id, atom_id);

INSERT INTO schema_metadata(key, value) VALUES
    ('schema_name', 'naome-memory-sqlite'),
    ('schema_version', '1');
