use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::{retention, schema, search};

pub fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }

    let conn = Connection::open(path)
        .with_context(|| format!("failed to open SQLite database {}", path.display()))?;
    schema::validate_existing(&conn)?;
    configure_connection(&conn)?;
    Ok(conn)
}

/// Open an already-existing current-schema database without creating files,
/// initializing schema, changing persistent journal mode, or running recovery.
/// Lifecycle commands use this boundary so invalid artifacts and read-only
/// operations cannot trigger unrelated database mutations.
pub(crate) fn open_existing_database(path: &Path, read_only: bool) -> Result<Connection> {
    if !path.is_file() {
        anyhow::bail!("current SQLite database does not exist: {}", path.display());
    }
    let conn = if read_only {
        ensure_checkpointed_database(path)?;
        let mut uri = url::Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("SQLite path cannot be represented as a file URI"))?;
        uri.query_pairs_mut().append_pair("immutable", "1");
        Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    } else {
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }
    .with_context(|| format!("failed to open existing SQLite database {}", path.display()))?;
    schema::validate_current(&conn)?;
    configure_existing_connection(&conn)?;
    Ok(conn)
}

fn ensure_checkpointed_database(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-journal"] {
        let mut sidecar_name = OsString::from(path.as_os_str());
        sidecar_name.push(suffix);
        let sidecar_path = std::path::PathBuf::from(sidecar_name);
        match std::fs::symlink_metadata(&sidecar_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 0 {
                    anyhow::bail!(
                        "read-only lifecycle access requires a checkpointed SQLite database; recovery or checkpoint is required for {}",
                        path.display()
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to inspect SQLite journal state"),
        }
    }
    Ok(())
}

pub fn init_database(conn: &Connection) -> Result<()> {
    schema::init(conn)
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign keys")?;
    conn.pragma_update(None, "busy_timeout", 5_000_i64)
        .context("failed to set SQLite busy timeout")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable SQLite WAL journal mode")?;
    retention::register_sqlite_functions(conn)
        .context("failed to register SQLite retention functions")?;
    search::register_sqlite_functions(conn)
        .context("failed to register SQLite path applicability functions")?;
    Ok(())
}

fn configure_existing_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign keys")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("failed to set SQLite busy timeout")?;
    retention::register_sqlite_functions(conn)
        .context("failed to register SQLite retention functions")?;
    search::register_sqlite_functions(conn)
        .context("failed to register SQLite path applicability functions")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, ErrorCode, params};
    use tempfile::TempDir;

    const EXPECTED_TABLES: &[&str] = &[
        "event_log",
        "memory_record",
        "origin_outcome",
        "scope_binding",
        "memory_path",
        "proposal",
        "memory_tag",
        "memory_capture",
        "runtime_mirror_state",
        "private_lifecycle_generation",
        "private_lifecycle_state",
        "private_lifecycle_relation",
        "owner_action_grant",
        "private_lifecycle_application",
        "private_maintenance_grant",
        "private_maintenance_projection",
        "private_conflict_set",
        "private_conflict_member",
        "private_conflict_edge",
        "read_audit",
        "memory_fts",
    ];

    #[test]
    fn init_database_is_idempotent_and_creates_expected_tables() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;

        init_database(&conn)?;
        let schema_version: i64 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(schema_version, crate::schema::CURRENT_SCHEMA_VERSION);
        conn.execute(
            "INSERT INTO memory_record(
               id, type, scope_kind, title, body, status, retention_json, origin_json, content_hash
             ) VALUES (?1, 'fact', 'repo', ?2, ?3, 'active', ?4, ?5, ?6)",
            params![
                "rec-existing",
                "Existing memory",
                "This row must survive a second schema init.",
                r#"{}"#,
                r#"{"origin_key":"test:rec-existing","route":"local_memory"}"#,
                "hash-existing"
            ],
        )?;
        init_database(&conn)?;

        for table in EXPECTED_TABLES {
            assert!(table_exists(&conn, table)?, "missing table {table}");
        }

        let capture_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_capture')",
            [],
            |row| row.get(0),
        )?;
        assert!(capture_table);

        let records: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_record WHERE id = 'rec-existing'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(records, 1);

        let lane: String = conn.query_row(
            "SELECT lane FROM memory_record WHERE id = 'rec-existing'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(lane, "semantic");

        let destination: String = conn.query_row(
            "SELECT destination FROM memory_record WHERE id = 'rec-existing'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(destination, "repo");

        Ok(())
    }

    #[test]
    fn open_database_rejects_any_non_current_schema_without_modifying_it() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE memory_record (
              rowid INTEGER PRIMARY KEY,
              id TEXT NOT NULL UNIQUE,
              type TEXT NOT NULL,
              scope_kind TEXT NOT NULL,
              scope_id TEXT,
              visibility TEXT NOT NULL DEFAULT 'repo',
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              status TEXT NOT NULL,
              confidence REAL NOT NULL DEFAULT 1.0,
              source_kind TEXT,
              source_ref TEXT,
              content_hash TEXT NOT NULL,
              created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
              updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
              supersedes_id TEXT,
              expires_at TEXT
            );
            INSERT INTO memory_record(id, type, scope_kind, title, body, status, content_hash)
            VALUES ('legacy-record', 'decision', 'repo', 'Legacy record', 'Legacy body', 'active', 'legacy-hash');
            "#,
        )?;
        drop(conn);

        let error = open_database(&db_path).expect_err("non-current schema must be rejected");
        let message = format!("{error:#}");
        assert!(message.contains("database does not match the current Memzoi format"));

        let conn = Connection::open(&db_path)?;
        let journal_mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        assert_eq!(journal_mode, "delete");
        let records: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(records, 1);

        Ok(())
    }

    #[test]
    fn open_database_rejects_wrong_user_version_without_modifying_it() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;
        conn.pragma_update(None, "user_version", 1_i64)?;
        drop(conn);

        let error = open_database(&db_path).expect_err("old user_version must be rejected");
        assert!(format!("{error:#}").contains("user_version 1"));

        let conn = Connection::open(&db_path)?;
        let actual: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(actual, 1);
        Ok(())
    }

    #[test]
    fn open_database_enables_foreign_key_enforcement() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;

        let err = conn
            .execute(
                "INSERT INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
                params!["missing-record", "orphan"],
            )
            .unwrap_err();

        assert!(
            matches!(
                err,
                rusqlite::Error::SqliteFailure(error, _)
                    if error.code == ErrorCode::ConstraintViolation
            ),
            "expected foreign-key constraint failure, got {err:?}"
        );

        Ok(())
    }

    #[test]
    fn fts_index_returns_inserted_active_record() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;

        conn.execute(
            "INSERT INTO memory_record(
               id, type, scope_kind, title, body, status, retention_json, origin_json, content_hash
             ) VALUES (?1, 'fact', 'repo', ?2, ?3, 'active', ?4, ?5, ?6)",
            params![
                "rec-active",
                "Searchable setup note",
                "The zircon token must be findable through the FTS index.",
                r#"{}"#,
                r#"{"origin_key":"test:rec-active","route":"local_memory"}"#,
                "hash-active"
            ],
        )?;

        let ids = matching_active_ids(&conn, "zircon")?;

        assert_eq!(ids, vec!["rec-active"]);
        Ok(())
    }

    fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
    }

    fn matching_active_ids(conn: &Connection, query: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT memory_record.id
             FROM memory_fts
             JOIN memory_record ON memory_record.rowid = memory_fts.rowid
             WHERE memory_fts MATCH ?1 AND memory_record.status = 'active'
             ORDER BY memory_record.id",
        )?;

        let rows = stmt.query_map([query], |row| row.get(0))?;
        rows.collect()
    }
}
