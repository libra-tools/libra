CREATE TABLE IF NOT EXISTS memory_episode (
    episode_id TEXT NOT NULL PRIMARY KEY,
    repo_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('commit','agent_session','agent_run','bridge_operation')),
    source_key TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded','failed','aborted','partial','unknown')),
    actor TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL,
    anchor_commit TEXT,
    change_id TEXT,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    content_digest TEXT NOT NULL CHECK (length(content_digest) = 64 AND content_digest NOT GLOB '*[^0-9a-f]*'),
    producer TEXT NOT NULL DEFAULT 'derived-v1',
    rules_version INTEGER NOT NULL CHECK (rules_version > 0),
    UNIQUE (repo_id, source_kind, source_key)
);
CREATE INDEX IF NOT EXISTS memory_episode_list
    ON memory_episode (repo_id, ended_at DESC, episode_id);
CREATE TABLE IF NOT EXISTS memory_episode_evidence (
    episode_id TEXT NOT NULL REFERENCES memory_episode(episode_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('commit','checkpoint','review_run','operation')),
    ref_id TEXT NOT NULL,
    link_confidence TEXT NOT NULL CHECK (link_confidence IN ('identity','operation','temporal')),
    resolution_status TEXT NOT NULL CHECK (resolution_status IN ('resolved','unresolved')),
    PRIMARY KEY (episode_id, ordinal)
);
CREATE TABLE IF NOT EXISTS memory_projection_state (
    repo_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('commit','agent_session','agent_run','bridge_operation')),
    cursor_json TEXT NOT NULL CHECK (json_valid(cursor_json)),
    fingerprint TEXT NOT NULL CHECK (length(fingerprint) = 64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
    rules_version INTEGER NOT NULL CHECK (rules_version > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    horizon_truncated INTEGER NOT NULL CHECK (horizon_truncated IN (0,1)),
    rebuilt_at INTEGER NOT NULL,
    fts_synced_fingerprint TEXT CHECK (fts_synced_fingerprint IS NULL OR (length(fts_synced_fingerprint) = 64 AND fts_synced_fingerprint NOT GLOB '*[^0-9a-f]*')),
    PRIMARY KEY (repo_id, source_kind)
);
