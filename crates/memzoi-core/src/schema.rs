use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)
        .context("failed to initialize SQLite schema")?;
    ensure_memory_lane_column(conn)?;
    ensure_memory_destination_column(conn)?;
    ensure_memory_proposal_id_column(conn)?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (2);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (3);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (4);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (5);",
    )
    .context("failed to record schema migrations 2 through 5")?;
    Ok(())
}

fn ensure_memory_proposal_id_column(conn: &Connection) -> Result<()> {
    let has_proposal_id: bool = conn
        .query_row(
            "SELECT EXISTS(
              SELECT 1 FROM pragma_table_info('memory_record') WHERE name = 'proposal_id'
            )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect memory_record proposal lineage schema")?;

    if !has_proposal_id {
        conn.execute_batch("ALTER TABLE memory_record ADD COLUMN proposal_id TEXT;")
            .context("failed to add memory_record.proposal_id column")?;
    }

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memory_record_proposal_id ON memory_record(proposal_id);",
    )
    .context("failed to create memory proposal lineage index")
}

fn ensure_memory_lane_column(conn: &Connection) -> Result<()> {
    let has_lane: bool = conn
        .query_row(
            "SELECT EXISTS(
              SELECT 1 FROM pragma_table_info('memory_record') WHERE name = 'lane'
            )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect memory_record schema")?;

    if !has_lane {
        conn.execute_batch(
            "ALTER TABLE memory_record
               ADD COLUMN lane TEXT NOT NULL DEFAULT 'semantic'
               CHECK (lane IN ('session', 'semantic', 'episodic', 'procedural'));",
        )
        .context("failed to add memory_record.lane column")?;
    }

    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_memory_record_lane ON memory_record(lane);")
        .context("failed to create memory lane index")
}

fn ensure_memory_destination_column(conn: &Connection) -> Result<()> {
    let has_destination: bool = conn
        .query_row(
            "SELECT EXISTS(
              SELECT 1 FROM pragma_table_info('memory_record') WHERE name = 'destination'
            )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect memory_record destination schema")?;

    if !has_destination {
        conn.execute_batch(
            "ALTER TABLE memory_record
               ADD COLUMN destination TEXT NOT NULL DEFAULT 'repo'
               CHECK (destination IN ('repo', 'local', 'session'));",
        )
        .context("failed to add memory_record.destination column")?;
    }

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_memory_record_destination ON memory_record(destination);",
    )
    .context("failed to create memory destination index")
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS event_log (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  actor TEXT NOT NULL,
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  record_id TEXT,
  proposal_id TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS memory_record (
  rowid INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  type TEXT NOT NULL CHECK (type IN ('fact', 'preference', 'decision', 'procedure', 'episode', 'relationship', 'warning', 'failed_attempt', 'risk', 'instruction_projection')),
  lane TEXT NOT NULL DEFAULT 'semantic' CHECK (lane IN ('session', 'semantic', 'episodic', 'procedural')),
  destination TEXT NOT NULL DEFAULT 'repo' CHECK (destination IN ('repo', 'local', 'session')),
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('personal', 'repo', 'project', 'team', 'org', 'agent', 'imported_untrusted')),
  scope_id TEXT,
  visibility TEXT NOT NULL DEFAULT 'repo' CHECK (visibility IN ('public', 'private', 'repo', 'team', 'org')),
  title TEXT NOT NULL CHECK (length(trim(title)) > 0),
  body TEXT NOT NULL CHECK (length(trim(body)) > 0),
  status TEXT NOT NULL CHECK (status IN ('proposed', 'active', 'rejected', 'superseded', 'expired', 'tombstoned', 'redacted')),
  confidence REAL NOT NULL DEFAULT 1.0 CHECK (confidence >= 0.0 AND confidence <= 1.0),
  source_kind TEXT,
  source_ref TEXT,
  proposal_id TEXT,
  content_hash TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  supersedes_id TEXT REFERENCES memory_record(id),
  expires_at TEXT
);

CREATE TABLE IF NOT EXISTS scope_binding (
  id TEXT PRIMARY KEY,
  record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('personal', 'repo', 'project', 'team', 'org', 'agent', 'imported_untrusted')),
  scope_id TEXT
);

CREATE TABLE IF NOT EXISTS memory_path (
  id TEXT PRIMARY KEY,
  record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  repo_id TEXT,
  path TEXT NOT NULL CHECK (length(trim(path)) > 0),
  symbol TEXT,
  line_start INTEGER CHECK (line_start IS NULL OR line_start > 0),
  line_end INTEGER CHECK (line_end IS NULL OR line_end > 0),
  CHECK (line_start IS NULL OR line_end IS NULL OR line_end >= line_start)
);

CREATE TABLE IF NOT EXISTS proposal (
  id TEXT PRIMARY KEY,
  operation TEXT NOT NULL CHECK (operation IN ('create', 'supersede', 'tombstone')),
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  status TEXT NOT NULL CHECK (status IN ('pending', 'validated', 'approved', 'rejected', 'applied')),
  actor TEXT NOT NULL,
  validation_json TEXT CHECK (validation_json IS NULL OR json_valid(validation_json)),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS memory_tag (
  record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  tag TEXT NOT NULL CHECK (length(trim(tag)) > 0),
  PRIMARY KEY (record_id, tag)
);

CREATE TABLE IF NOT EXISTS memory_capture (
  record_id TEXT PRIMARY KEY REFERENCES memory_record(id) ON DELETE CASCADE,
  provenance_json TEXT NOT NULL CHECK (json_valid(provenance_json))
);

CREATE TABLE IF NOT EXISTS read_audit (
  id TEXT PRIMARY KEY,
  operation TEXT NOT NULL,
  query_json TEXT NOT NULL CHECK (json_valid(query_json)),
  result_ids_json TEXT NOT NULL CHECK (json_valid(result_ids_json)),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
  title,
  body,
  content='memory_record',
  content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS memory_record_ai AFTER INSERT ON memory_record BEGIN
  INSERT INTO memory_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_record_ad AFTER DELETE ON memory_record BEGIN
  INSERT INTO memory_fts(memory_fts, rowid, title, body) VALUES ('delete', old.rowid, old.title, old.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_record_au AFTER UPDATE OF title, body ON memory_record BEGIN
  INSERT INTO memory_fts(memory_fts, rowid, title, body) VALUES ('delete', old.rowid, old.title, old.body);
  INSERT INTO memory_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;

CREATE INDEX IF NOT EXISTS idx_memory_record_id ON memory_record(id);
CREATE INDEX IF NOT EXISTS idx_memory_record_status_scope_type ON memory_record(status, scope_kind, type);
CREATE INDEX IF NOT EXISTS idx_memory_record_content_hash ON memory_record(content_hash);
CREATE INDEX IF NOT EXISTS idx_memory_path_record_id ON memory_path(record_id);
CREATE INDEX IF NOT EXISTS idx_memory_path_path ON memory_path(path);
CREATE INDEX IF NOT EXISTS idx_proposal_id_status ON proposal(id, status);
CREATE INDEX IF NOT EXISTS idx_event_log_created_at ON event_log(created_at);
CREATE INDEX IF NOT EXISTS idx_memory_capture_record_id ON memory_capture(record_id);

INSERT OR IGNORE INTO schema_migrations(version) VALUES (1);
"#;
