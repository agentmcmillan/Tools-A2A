-- Content-addressed object store (Radmind-inspired).
--
-- Objects are immutable once written: sha256 is the identity.
-- Layout on disk: data/objects/{sha256[0..2]}/{sha256[2..]}
--
-- Snapshots are point-in-time transcripts of a project's file tree.
-- A transcript is a sorted list of lines:
--   F | path | sha256 | size | mode | mtime
--
-- snapshot_id is sha256(transcript content) — deterministic, content-addressed.
--
CREATE TABLE IF NOT EXISTS objects (
    sha256      TEXT PRIMARY KEY,
    size_bytes  INTEGER NOT NULL,
    created_at  DOUBLE PRECISION NOT NULL
);

-- One snapshot per commit of the file tree.
CREATE TABLE IF NOT EXISTS snapshots (
    id          TEXT PRIMARY KEY,   -- sha256 of the transcript
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    gateway     TEXT NOT NULL,      -- which gateway produced this snapshot
    transcript  TEXT NOT NULL,      -- full transcript text (F|path|sha256|size|mode|mtime lines)
    created_at  DOUBLE PRECISION NOT NULL
);

CREATE INDEX IF NOT EXISTS snapshots_project ON snapshots(project_id, created_at DESC);

-- HEAD pointer: latest accepted snapshot per project
CREATE TABLE IF NOT EXISTS project_heads (
    project_id   TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    snapshot_id  TEXT NOT NULL REFERENCES snapshots(id),
    updated_at   DOUBLE PRECISION NOT NULL
);
