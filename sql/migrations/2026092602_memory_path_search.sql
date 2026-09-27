CREATE TABLE IF NOT EXISTS memory_episode_path (
    episode_id TEXT NOT NULL REFERENCES memory_episode(episode_id) ON DELETE CASCADE,
    code_path BLOB NOT NULL CHECK (typeof(code_path) = 'blob' AND length(code_path) > 0 AND substr(code_path, 1, 1) <> x'2f' AND instr(code_path, x'00') = 0),
    change_kind TEXT NOT NULL CHECK (change_kind IN ('added','modified','deleted','renamed')),
    blob_oid_at_end TEXT,
    mode_at_end INTEGER CHECK (mode_at_end IN (33188,33261,40960,57344)),
    ended_at INTEGER NOT NULL,
    PRIMARY KEY (episode_id, code_path),
    CHECK (change_kind <> 'deleted' OR (blob_oid_at_end IS NULL AND mode_at_end IS NULL))
);
CREATE INDEX IF NOT EXISTS memory_episode_path_lookup
    ON memory_episode_path (code_path, ended_at DESC, episode_id);
CREATE TABLE IF NOT EXISTS memory_episode_search_doc (
    rowid INTEGER PRIMARY KEY,
    episode_id TEXT NOT NULL UNIQUE REFERENCES memory_episode(episode_id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    paths_text TEXT NOT NULL
);
