use anyhow::{Context, Result, bail};
use rusqlite::Connection;

pub const UNSUPPORTED_SCHEMA_ERROR_PREFIX: &str = "unsupported SQLite schema";
pub const CURRENT_SCHEMA_VERSION: i64 = 2;

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

pub(crate) fn validate_current(conn: &Connection) -> Result<()> {
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
    let actual_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read SQLite user_version")?;
    if actual_version != CURRENT_SCHEMA_VERSION {
        bail!(
            "unsupported SQLite schema: database does not match the current Memzoi format (user_version {actual_version}, current {CURRENT_SCHEMA_VERSION}); manually upgrade or remove it"
        );
    }
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
    let lifecycle_singleton_exists: bool = conn
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM private_lifecycle_generation
               WHERE singleton = 1 AND generation >= 0
             )",
            [],
            |row| row.get(0),
        )
        .context("failed to validate private lifecycle generation")?;
    if !lifecycle_singleton_exists {
        bail!(
            "unsupported SQLite schema: private lifecycle generation is not initialized; manually upgrade or remove it"
        );
    }
    let private_state_is_complete: bool = conn
        .query_row(
            "SELECT
               NOT EXISTS(
                 SELECT 1
                 FROM memory_record AS record
                 LEFT JOIN private_lifecycle_state AS lifecycle
                   ON lifecycle.record_id = record.id
                 WHERE record.destination IN ('local', 'session')
                   AND lifecycle.record_id IS NULL
               )
               AND NOT EXISTS(
                 SELECT 1
                 FROM private_lifecycle_state AS lifecycle
                 JOIN memory_record AS record ON record.id = lifecycle.record_id
                 WHERE record.destination NOT IN ('local', 'session')
               )",
            [],
            |row| row.get(0),
        )
        .context("failed to validate private lifecycle state coverage")?;
    if !private_state_is_complete {
        bail!(
            "unsupported SQLite schema: private lifecycle state is incomplete; manually upgrade or remove it"
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

PRAGMA user_version = 2;

CREATE TABLE IF NOT EXISTS event_log (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  actor TEXT NOT NULL,
  data_class TEXT NOT NULL CHECK (data_class IN ('repository', 'private')),
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

CREATE TABLE IF NOT EXISTS private_lifecycle_generation (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  generation INTEGER NOT NULL CHECK (generation >= 0)
);

INSERT OR IGNORE INTO private_lifecycle_generation(singleton, generation) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS private_lifecycle_state (
  record_id TEXT PRIMARY KEY REFERENCES memory_record(id) ON DELETE CASCADE,
  automatic_recall_until TEXT,
  validity_until TEXT,
  retain_until TEXT,
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  quarantined INTEGER NOT NULL DEFAULT 0 CHECK (quarantined IN (0, 1)),
  quarantine_reason_code TEXT CHECK (
    quarantine_reason_code IS NULL OR (
      length(trim(quarantine_reason_code)) > 0
      AND length(CAST(quarantine_reason_code AS BLOB)) <= 128
    )
  ),
  record_version TEXT NOT NULL CHECK (
    length(record_version) = 32
    AND record_version NOT GLOB '*[^0-9a-f]*'
  ),
  automatic_recall_event_id TEXT,
  validity_event_id TEXT,
  retention_event_id TEXT,
  quarantine_event_id TEXT,
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  CHECK (
    (quarantined = 1 AND quarantine_reason_code IS NOT NULL)
    OR (quarantined = 0 AND quarantine_reason_code IS NULL)
  )
);

CREATE TABLE IF NOT EXISTS owner_action_grant (
  grant_id TEXT PRIMARY KEY CHECK (length(trim(grant_id)) > 0 AND length(grant_id) <= 128),
  request_id TEXT NOT NULL CHECK (length(trim(request_id)) > 0),
  request_json TEXT NOT NULL CHECK (json_valid(request_json)),
  state TEXT NOT NULL CHECK (state IN ('active', 'consumed', 'revoked')),
  authorized_at TEXT NOT NULL CHECK (length(trim(authorized_at)) > 0),
  expires_at TEXT NOT NULL CHECK (length(trim(expires_at)) > 0),
  revoked_at TEXT,
  consumed_at TEXT,
  consumed_application_id TEXT,
  CHECK (
    (state = 'active' AND revoked_at IS NULL AND consumed_at IS NULL AND consumed_application_id IS NULL)
    OR (state = 'revoked' AND revoked_at IS NOT NULL AND consumed_at IS NULL AND consumed_application_id IS NULL)
    OR (state = 'consumed' AND revoked_at IS NULL AND consumed_at IS NOT NULL AND consumed_application_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS private_lifecycle_application (
  application_id TEXT PRIMARY KEY CHECK (length(trim(application_id)) > 0 AND length(application_id) <= 128),
  operation_id TEXT NOT NULL UNIQUE CHECK (length(trim(operation_id)) > 0),
  request_id TEXT NOT NULL CHECK (length(trim(request_id)) > 0),
  grant_id TEXT NOT NULL UNIQUE REFERENCES owner_action_grant(grant_id),
  result_json TEXT NOT NULL CHECK (json_valid(result_json)),
  lifecycle_generation INTEGER NOT NULL CHECK (lifecycle_generation > 0),
  applied_at TEXT NOT NULL CHECK (length(trim(applied_at)) > 0)
);

CREATE TABLE IF NOT EXISTS private_lifecycle_relation (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0 AND length(id) <= 128),
  relation_kind TEXT NOT NULL CHECK (relation_kind IN (
    'renewed_by',
    'corrected_by',
    'superseded_by',
    'consolidated_into',
    'contradiction_resolved_by'
  )),
  subject_record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  related_record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  application_id TEXT NOT NULL CHECK (length(trim(application_id)) > 0),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  CHECK (subject_record_id <> related_record_id),
  UNIQUE (relation_kind, subject_record_id, related_record_id, application_id)
);

CREATE TABLE IF NOT EXISTS private_maintenance_grant (
  grant_id TEXT PRIMARY KEY CHECK (length(trim(grant_id)) > 0 AND length(grant_id) <= 128),
  grant_fingerprint TEXT NOT NULL UNIQUE CHECK (length(trim(grant_fingerprint)) > 0),
  state TEXT NOT NULL CHECK (state IN ('active', 'revoked')),
  policy_version TEXT NOT NULL CHECK (length(trim(policy_version)) > 0),
  authorized_at TEXT NOT NULL CHECK (length(trim(authorized_at)) > 0),
  revoked_at TEXT,
  CHECK (
    (state = 'active' AND revoked_at IS NULL)
    OR (state = 'revoked' AND revoked_at IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS private_maintenance_projection (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  state TEXT NOT NULL CHECK (state IN ('disabled', 'current', 'dirty', 'blocked')),
  grant_fingerprint TEXT,
  projection_id TEXT,
  plan_id TEXT,
  authoritative_generation INTEGER NOT NULL CHECK (authoritative_generation >= 0),
  policy_version TEXT NOT NULL CHECK (length(trim(policy_version)) > 0),
  detector_digest TEXT,
  reason_code TEXT,
  member_count INTEGER NOT NULL DEFAULT 0 CHECK (member_count >= 0),
  edge_count INTEGER NOT NULL DEFAULT 0 CHECK (edge_count >= 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  CHECK (
    (state = 'disabled' AND grant_fingerprint IS NULL AND projection_id IS NULL AND plan_id IS NULL
      AND detector_digest IS NULL AND reason_code IS NULL AND member_count = 0 AND edge_count = 0)
    OR (state = 'current' AND grant_fingerprint IS NOT NULL AND projection_id IS NOT NULL
      AND plan_id IS NOT NULL AND detector_digest IS NOT NULL AND reason_code IS NULL)
    OR (state IN ('dirty', 'blocked') AND grant_fingerprint IS NOT NULL AND reason_code IS NOT NULL)
  )
);

INSERT OR IGNORE INTO private_maintenance_projection(
  singleton, state, authoritative_generation, policy_version,
  updated_at
) VALUES (
  1, 'disabled', 0, 'maintenance-policy/1',
  strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

CREATE TABLE IF NOT EXISTS private_conflict_set (
  conflict_id TEXT PRIMARY KEY CHECK (length(trim(conflict_id)) > 0),
  projection_id TEXT NOT NULL CHECK (length(trim(projection_id)) > 0),
  finding_id TEXT NOT NULL CHECK (length(trim(finding_id)) > 0),
  comparison_set_digest TEXT NOT NULL CHECK (length(trim(comparison_set_digest)) > 0),
  grant_fingerprint TEXT NOT NULL CHECK (length(trim(grant_fingerprint)) > 0),
  detector_version TEXT NOT NULL CHECK (length(trim(detector_version)) > 0),
  policy_version TEXT NOT NULL CHECK (length(trim(policy_version)) > 0),
  reason_code TEXT NOT NULL CHECK (reason_code = 'high_confidence_unresolved_contradiction'),
  resolution_state TEXT NOT NULL CHECK (resolution_state = 'unresolved'),
  recall_effect TEXT NOT NULL CHECK (recall_effect = 'suppress_all_automatic_recall')
);

CREATE TABLE IF NOT EXISTS private_conflict_member (
  conflict_id TEXT NOT NULL REFERENCES private_conflict_set(conflict_id) ON DELETE CASCADE,
  record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  record_version TEXT NOT NULL CHECK (
    length(record_version) = 32 AND record_version NOT GLOB '*[^0-9a-f]*'
  ),
  PRIMARY KEY (conflict_id, record_id)
);

CREATE TABLE IF NOT EXISTS private_conflict_edge (
  conflict_id TEXT NOT NULL REFERENCES private_conflict_set(conflict_id) ON DELETE CASCADE,
  left_record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  right_record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
  evidence_digest TEXT NOT NULL CHECK (length(trim(evidence_digest)) > 0),
  reason_code TEXT NOT NULL CHECK (reason_code = 'high_confidence_unresolved_contradiction'),
  CHECK (left_record_id < right_record_id),
  PRIMARY KEY (conflict_id, left_record_id, right_record_id),
  FOREIGN KEY (conflict_id, left_record_id)
    REFERENCES private_conflict_member(conflict_id, record_id) ON DELETE CASCADE,
  FOREIGN KEY (conflict_id, right_record_id)
    REFERENCES private_conflict_member(conflict_id, record_id) ON DELETE CASCADE
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
CREATE INDEX IF NOT EXISTS idx_private_lifecycle_relation_subject ON private_lifecycle_relation(subject_record_id, relation_kind);
CREATE INDEX IF NOT EXISTS idx_private_lifecycle_relation_related ON private_lifecycle_relation(related_record_id, relation_kind);
CREATE INDEX IF NOT EXISTS idx_owner_action_grant_request_state ON owner_action_grant(request_id, state, expires_at);
CREATE INDEX IF NOT EXISTS idx_private_lifecycle_application_request ON private_lifecycle_application(request_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_private_maintenance_one_active_grant
  ON private_maintenance_grant(state) WHERE state = 'active';
CREATE INDEX IF NOT EXISTS idx_private_conflict_member_record
  ON private_conflict_member(record_id, record_version);
CREATE INDEX IF NOT EXISTS idx_private_conflict_edge_left
  ON private_conflict_edge(left_record_id, right_record_id);
CREATE INDEX IF NOT EXISTS idx_private_conflict_edge_right
  ON private_conflict_edge(right_record_id, left_record_id);
"#;

const RUNTIME_MIRROR_TRIGGERS: &str = r#"
CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_ai
AFTER INSERT ON memory_record
WHEN NEW.destination IN ('local', 'session')
BEGIN
  INSERT INTO private_lifecycle_state(
    record_id, record_version, updated_at
  ) VALUES (
    NEW.id, lower(hex(randomblob(16))), NEW.updated_at
  );
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_au
AFTER UPDATE ON memory_record
WHEN OLD.destination IN ('local', 'session') OR NEW.destination IN ('local', 'session')
BEGIN
  INSERT INTO private_lifecycle_state(
    record_id, record_version, updated_at
  )
  SELECT NEW.id, lower(hex(randomblob(16))), NEW.updated_at
  WHERE NEW.destination IN ('local', 'session')
  ON CONFLICT(record_id) DO UPDATE SET
    record_version = lower(hex(randomblob(16))),
    updated_at = excluded.updated_at;
  DELETE FROM private_lifecycle_state
  WHERE record_id = NEW.id
    AND NEW.destination NOT IN ('local', 'session');
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_memory_record_ad
AFTER DELETE ON memory_record
WHEN OLD.destination IN ('local', 'session')
BEGIN
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_state_au
AFTER UPDATE OF
  automatic_recall_until,
  validity_until,
  retain_until,
  pinned,
  quarantined,
  quarantine_reason_code,
  automatic_recall_event_id,
  validity_event_id,
  retention_event_id,
  quarantine_event_id
ON private_lifecycle_state
BEGIN
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = NEW.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_state_bi
BEFORE INSERT ON private_lifecycle_state
WHEN NOT EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.record_id AND destination IN ('local', 'session')
)
BEGIN
  SELECT RAISE(ABORT, 'private lifecycle state requires a local/session record');
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_state_ad
AFTER DELETE ON private_lifecycle_state
WHEN EXISTS (SELECT 1 FROM memory_record WHERE id = OLD.record_id)
BEGIN
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_relation_ai
AFTER INSERT ON private_lifecycle_relation
BEGIN
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16))),
      updated_at = NEW.created_at
  WHERE record_id IN (NEW.subject_record_id, NEW.related_record_id);
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_relation_bi
BEFORE INSERT ON private_lifecycle_relation
WHEN NOT EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.subject_record_id AND destination IN ('local', 'session')
) OR NOT EXISTS (
  SELECT 1 FROM memory_record
  WHERE id = NEW.related_record_id AND destination IN ('local', 'session')
)
BEGIN
  SELECT RAISE(ABORT, 'private lifecycle relations require local/session records');
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_relation_bu
BEFORE UPDATE ON private_lifecycle_relation
BEGIN
  SELECT RAISE(ABORT, 'private lifecycle relations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS private_lifecycle_relation_ad
AFTER DELETE ON private_lifecycle_relation
BEGIN
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id IN (OLD.subject_record_id, OLD.related_record_id);
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = NEW.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id IN (OLD.record_id, NEW.record_id);
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = OLD.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = NEW.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id IN (OLD.record_id, NEW.record_id);
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = OLD.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = NEW.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id IN (OLD.record_id, NEW.record_id);
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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
  UPDATE private_lifecycle_state
  SET record_version = lower(hex(randomblob(16)))
  WHERE record_id = OLD.record_id;
  UPDATE private_lifecycle_generation
  SET generation = generation + 1
  WHERE singleton = 1;
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

CREATE TRIGGER IF NOT EXISTS private_maintenance_generation_dirty
AFTER UPDATE OF generation ON private_lifecycle_generation
WHEN NEW.generation <> OLD.generation
  AND EXISTS (SELECT 1 FROM private_maintenance_grant WHERE state = 'active')
BEGIN
  UPDATE private_maintenance_projection
  SET state = 'dirty',
      grant_fingerprint = (
        SELECT grant_fingerprint FROM private_maintenance_grant WHERE state = 'active'
      ),
      authoritative_generation = NEW.generation,
      reason_code = 'authoritative_generation_changed',
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS private_maintenance_grant_insert_dirty
AFTER INSERT ON private_maintenance_grant
WHEN NEW.state = 'active'
BEGIN
  UPDATE private_maintenance_projection
  SET state = 'dirty',
      grant_fingerprint = NEW.grant_fingerprint,
      authoritative_generation = (
        SELECT generation FROM private_lifecycle_generation WHERE singleton = 1
      ),
      reason_code = 'grant_fingerprint_changed',
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS private_maintenance_grant_update_dirty
AFTER UPDATE OF grant_fingerprint, policy_version ON private_maintenance_grant
WHEN NEW.state = 'active'
  AND (NEW.grant_fingerprint <> OLD.grant_fingerprint
       OR NEW.policy_version <> OLD.policy_version)
BEGIN
  UPDATE private_maintenance_projection
  SET state = 'dirty',
      grant_fingerprint = NEW.grant_fingerprint,
      authoritative_generation = (
        SELECT generation FROM private_lifecycle_generation WHERE singleton = 1
      ),
      reason_code = 'grant_fingerprint_changed',
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_maintenance_grant_ai
AFTER INSERT ON private_maintenance_grant
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_maintenance_grant_au
AFTER UPDATE ON private_maintenance_grant
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_maintenance_grant_ad
AFTER DELETE ON private_maintenance_grant
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_maintenance_projection_au
AFTER UPDATE ON private_maintenance_projection
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_set_ai
AFTER INSERT ON private_conflict_set
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_set_ad
AFTER DELETE ON private_conflict_set
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_member_ai
AFTER INSERT ON private_conflict_member
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_member_ad
AFTER DELETE ON private_conflict_member
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_edge_ai
AFTER INSERT ON private_conflict_edge
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;

CREATE TRIGGER IF NOT EXISTS runtime_mirror_private_conflict_edge_ad
AFTER DELETE ON private_conflict_edge
BEGIN
  INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
  VALUES (1, lower(hex(randomblob(16))));
END;
"#;
