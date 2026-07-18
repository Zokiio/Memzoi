use anyhow::{Context, Result, bail};
use rusqlite::Connection;

pub const UNSUPPORTED_SCHEMA_ERROR_PREFIX: &str = "unsupported SQLite schema";

pub fn is_unsupported_schema_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains(UNSUPPORTED_SCHEMA_ERROR_PREFIX))
}

pub fn init(conn: &Connection) -> Result<()> {
    if database_has_no_application_objects(conn)? {
        conn.execute_batch(CURRENT_SCHEMA)
            .context("failed to initialize SQLite schema")?;
        conn.execute_batch(RUNTIME_MIRROR_TRIGGERS)
            .context("failed to initialize runtime mirror revision triggers")?;
    }
    validate_exact_current_schema(conn)?;
    Ok(())
}

pub(crate) fn validate_existing(conn: &Connection) -> Result<()> {
    if database_has_no_application_objects(conn)? {
        return Ok(());
    }
    validate_exact_current_schema(conn)
}

fn database_has_no_application_objects(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT NOT EXISTS(
           SELECT 1 FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'
         )",
        [],
        |row| row.get(0),
    )
    .context("failed to inspect SQLite schema")
}

fn validate_exact_current_schema(conn: &Connection) -> Result<()> {
    let expected = Connection::open_in_memory()
        .context("failed to create current-schema validation database")?;
    expected
        .execute_batch(CURRENT_SCHEMA)
        .context("failed to create current-schema validation tables")?;
    expected
        .execute_batch(RUNTIME_MIRROR_TRIGGERS)
        .context("failed to create current-schema validation triggers")?;

    if schema_snapshot(conn)? != schema_snapshot(&expected)? {
        bail!(
            "unsupported SQLite schema: database does not match the current Memzoi format; manually upgrade or remove it"
        );
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

fn schema_snapshot(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_master
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(SchemaObject {
            object_type: row.get(0)?,
            name: row.get(1)?,
            table_name: row.get(2)?,
            sql: row
                .get::<_, Option<String>>(3)?
                .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" ")),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read SQLite schema definition")
}

const CURRENT_SCHEMA: &str = r#"

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
  retention_json TEXT NOT NULL CHECK (json_valid(retention_json)),
  origin_json TEXT NOT NULL CHECK (json_valid(origin_json)),
  lineage_json TEXT CHECK (lineage_json IS NULL OR json_valid(lineage_json)),
  content_hash TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  supersedes_id TEXT REFERENCES memory_record(id)
);

CREATE TABLE IF NOT EXISTS origin_outcome (
  repository_key TEXT NOT NULL CHECK (length(trim(repository_key)) > 0),
  origin_key TEXT NOT NULL CHECK (length(trim(origin_key)) > 0),
  route TEXT NOT NULL CHECK (length(trim(route)) > 0),
  input_fingerprint TEXT NOT NULL CHECK (
    length(input_fingerprint) = 64
    AND input_fingerprint NOT GLOB '*[^0-9a-f]*'
  ),
  state TEXT NOT NULL CHECK (state IN ('prepared', 'finalized')),
  outcome_kind TEXT CHECK (
    outcome_kind IS NULL OR outcome_kind IN (
      'created',
      'existing_duplicate_no_write',
      'conflict_no_write',
      'needs_review_no_write',
      'rejected_no_write',
      'erased'
    )
  ),
  destination TEXT CHECK (
    destination IS NULL OR destination IN ('repo', 'local', 'session', 'discard', 'needs_review')
  ),
  record_id TEXT,
  proposal_id TEXT,
  lifecycle_event_id TEXT,
  prepared_at TEXT NOT NULL CHECK (length(trim(prepared_at)) > 0),
  recorded_at TEXT,
  PRIMARY KEY (repository_key, origin_key),
  CHECK (
    (state = 'prepared' AND outcome_kind IS NULL AND recorded_at IS NULL)
    OR
    (state = 'finalized' AND outcome_kind IS NOT NULL AND recorded_at IS NOT NULL)
  )
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

CREATE TABLE IF NOT EXISTS runtime_mirror_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  revision TEXT NOT NULL CHECK (
    length(revision) = 32 AND revision NOT GLOB '*[^0-9a-f]*'
  )
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
CREATE INDEX IF NOT EXISTS idx_memory_record_proposal_id ON memory_record(proposal_id);
CREATE INDEX IF NOT EXISTS idx_memory_path_record_id ON memory_path(record_id);
CREATE INDEX IF NOT EXISTS idx_memory_path_path ON memory_path(path);
CREATE INDEX IF NOT EXISTS idx_proposal_id_status ON proposal(id, status);
CREATE INDEX IF NOT EXISTS idx_event_log_created_at ON event_log(created_at);
CREATE INDEX IF NOT EXISTS idx_memory_capture_record_id ON memory_capture(record_id);
"#;

const RUNTIME_MIRROR_TRIGGERS: &str = r#"
CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_ai
AFTER INSERT ON memory_record
WHEN NEW.destination IN ('local', 'session')
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_au
AFTER UPDATE ON memory_record
WHEN OLD.destination IN ('local', 'session') OR NEW.destination IN ('local', 'session')
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_ad
AFTER DELETE ON memory_record
WHEN OLD.destination IN ('local', 'session')
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_tag_ai
AFTER INSERT ON memory_tag
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_tag_au
AFTER UPDATE ON memory_tag
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id IN (OLD.record_id, NEW.record_id) AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_tag_ad
AFTER DELETE ON memory_tag
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = OLD.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_path_ai
AFTER INSERT ON memory_path
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_path_au
AFTER UPDATE ON memory_path
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id IN (OLD.record_id, NEW.record_id) AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_path_ad
AFTER DELETE ON memory_path
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = OLD.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_capture_ai
AFTER INSERT ON memory_capture
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_capture_au
AFTER UPDATE ON memory_capture
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id IN (OLD.record_id, NEW.record_id) AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_capture_ad
AFTER DELETE ON memory_capture
WHEN EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = OLD.record_id AND destination IN ('local', 'session')
)
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_proposal_ai
AFTER INSERT ON proposal
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_proposal_au
AFTER UPDATE ON proposal
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_proposal_ad
AFTER DELETE ON proposal
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;
"#;
