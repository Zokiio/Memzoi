use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{MemoryPath, MemoryRecord};

use super::{
    query::record_by_id,
    query::records_for_runtime_preservation,
    write::{InsertMode, insert_memory_record_row},
};

pub(super) fn record_tags(conn: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag FROM memory_tag WHERE record_id = ?1 ORDER BY tag ASC")?;
    let rows = stmt.query_map([record_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(in crate::service) struct RuntimeRecordSnapshot {
    record: MemoryRecord,
    tags: Vec<String>,
    paths: Vec<MemoryPath>,
}

pub(super) fn runtime_record_snapshots(conn: &Connection) -> Result<Vec<RuntimeRecordSnapshot>> {
    records_for_runtime_preservation(conn)?
        .into_iter()
        .map(|record| {
            let tags = record_tags(conn, &record.id)?;
            let paths = runtime_record_paths(conn, &record.id)?;
            Ok(RuntimeRecordSnapshot {
                record,
                tags,
                paths,
            })
        })
        .collect()
}

fn runtime_record_paths(conn: &Connection, record_id: &str) -> Result<Vec<MemoryPath>> {
    let mut stmt = conn.prepare(
        "SELECT path, symbol, line_start, line_end
         FROM memory_path
         WHERE record_id = ?1
         ORDER BY path ASC, COALESCE(symbol, '') ASC, COALESCE(line_start, 0) ASC",
    )?;
    let rows = stmt.query_map([record_id], |row| {
        Ok(MemoryPath {
            path: row.get(0)?,
            symbol: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl RuntimeRecordSnapshot {
    pub(in crate::service) fn from_parts(
        record: MemoryRecord,
        tags: Vec<String>,
        paths: Vec<MemoryPath>,
    ) -> Self {
        Self {
            record,
            tags,
            paths,
        }
    }

    pub(in crate::service) fn record(&self) -> &MemoryRecord {
        &self.record
    }
}

pub(super) fn restore_runtime_record_snapshots(
    conn: &Connection,
    records: &[RuntimeRecordSnapshot],
) -> Result<()> {
    for snapshot in records {
        let record = &snapshot.record;
        insert_memory_record_row(conn, record, InsertMode::RestoreIfAbsent)?;
        let restored_parent = record_by_id(conn, &record.id)?;
        if restored_parent.as_ref() != Some(record) {
            bail!(
                "runtime record {} collides with a different indexed parent record",
                record.id
            );
        }
        for tag in &snapshot.tags {
            conn.execute(
                "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
                rusqlite::params![record.id, tag],
            )?;
        }
        for (index, path) in snapshot.paths.iter().enumerate() {
            conn.execute(
                "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("{}_restored_path_{index}", record.id),
                    record.id,
                    path.path,
                    path.symbol,
                    path.line_start,
                    path.line_end,
                ],
            )?;
        }
    }
    Ok(())
}

pub(super) fn replace_runtime_record_snapshot_exact(
    conn: &Connection,
    snapshot: &RuntimeRecordSnapshot,
) -> Result<()> {
    let record = &snapshot.record;
    if !matches!(
        record.destination,
        crate::MemoryDestination::Local | crate::MemoryDestination::Session
    ) {
        bail!(
            "runtime record {} replacement requires a local or session destination",
            record.id
        );
    }

    let changed = conn.execute(
        "UPDATE memory_record SET
           type = ?2,
           lane = ?3,
           destination = ?4,
           scope_kind = ?5,
           scope_id = ?6,
           visibility = ?7,
           title = ?8,
           body = ?9,
           status = ?10,
           confidence = ?11,
           source_kind = ?12,
           source_ref = ?13,
           proposal_id = ?14,
           content_hash = ?15,
           created_at = ?16,
           updated_at = ?17,
           supersedes_id = ?18,
           expires_at = ?19
         WHERE id = ?1
           AND destination IN ('local', 'session')",
        rusqlite::params![
            record.id,
            record.memory_type.as_str(),
            record.lane.as_str(),
            record.destination.as_str(),
            record.scope_kind.as_str(),
            record.scope_id,
            record.visibility.as_str(),
            record.title,
            record.body,
            record.status.as_str(),
            record.confidence,
            record.source_kind,
            record.source_ref,
            record.proposal_id,
            record.content_hash,
            record.created_at,
            record.updated_at,
            record.supersedes_id,
            record.expires_at,
        ],
    )?;
    if changed != 1 {
        bail!(
            "runtime record {} replacement target is missing or owned by non-runtime memory",
            record.id
        );
    }

    conn.execute("DELETE FROM memory_tag WHERE record_id = ?1", [&record.id])?;
    conn.execute("DELETE FROM memory_path WHERE record_id = ?1", [&record.id])?;
    for tag in &snapshot.tags {
        conn.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
            rusqlite::params![record.id, tag],
        )?;
    }
    for (index, path) in snapshot.paths.iter().enumerate() {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("{}_restored_path_{index}", record.id),
                record.id,
                path.path,
                path.symbol,
                path.line_start,
                path.line_end,
            ],
        )?;
    }
    crate::capture::store_capture_provenance(conn, &record.id, record.capture.as_ref())?;

    let replaced = match record_by_id(conn, &record.id)? {
        Some(record) => Some(RuntimeRecordSnapshot {
            tags: record_tags(conn, &record.id)?,
            paths: runtime_record_paths(conn, &record.id)?,
            record,
        }),
        None => None,
    };
    if replaced.as_ref() != Some(snapshot) {
        bail!(
            "runtime record {} replacement did not reproduce the exact snapshot",
            record.id
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        MemoryDestination, MemoryLane, MemoryStatus, MemoryType, ScopeKind, Visibility, db,
    };
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn restore_refuses_inactive_repo_parent_before_writing_children() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let repo_record = MemoryRecord {
            id: "local-colliding-id".to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            destination: MemoryDestination::Repo,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: "Inactive repository parent".to_owned(),
            body: "This row owns the identifier.".to_owned(),
            status: MemoryStatus::Superseded,
            confidence: 1.0,
            source_kind: None,
            source_ref: None,
            proposal_id: None,
            capture: None,
            content_hash: "repo-content".to_owned(),
            created_at: "2026-07-16T12:00:00Z".to_owned(),
            updated_at: "2026-07-16T12:00:00Z".to_owned(),
            supersedes_id: None,
            expires_at: None,
        };
        insert_memory_record_row(&conn, &repo_record, InsertMode::Create)?;
        conn.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'repo-tag')",
            [&repo_record.id],
        )?;

        let mut runtime_record = repo_record.clone();
        runtime_record.destination = MemoryDestination::Local;
        runtime_record.scope_kind = ScopeKind::Personal;
        runtime_record.visibility = Visibility::Private;
        runtime_record.status = MemoryStatus::Active;
        runtime_record.title = "Colliding local record".to_owned();
        let snapshot = RuntimeRecordSnapshot::from_parts(
            runtime_record,
            vec!["local-tag".to_owned()],
            vec![MemoryPath {
                path: "crates/local/**".to_owned(),
                symbol: None,
                line_start: None,
                line_end: None,
            }],
        );

        let error = restore_runtime_record_snapshots(&conn, &[snapshot])
            .expect_err("a repo-owned identifier must fail before child restoration");
        assert!(format!("{error:#}").contains("collides"));
        assert_eq!(record_tags(&conn, &repo_record.id)?, vec!["repo-tag"]);
        assert!(runtime_record_paths(&conn, &repo_record.id)?.is_empty());
        assert_eq!(record_by_id(&conn, &repo_record.id)?, Some(repo_record));
        Ok(())
    }

    #[test]
    fn exact_replacement_preserves_rowid_and_replaces_children_capture_and_fts() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        db::init_database(&conn)?;
        let original = MemoryRecord {
            id: "local-exact-replacement".to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            destination: MemoryDestination::Local,
            scope_kind: ScopeKind::Personal,
            scope_id: None,
            visibility: Visibility::Private,
            title: "Obsoletelexeme runtime title".to_owned(),
            body: "The old runtime body must leave the full-text index.".to_owned(),
            status: MemoryStatus::Active,
            confidence: 1.0,
            source_kind: Some("old-source".to_owned()),
            source_ref: Some("old-ref".to_owned()),
            proposal_id: None,
            capture: None,
            content_hash: "old-content".to_owned(),
            created_at: "2026-07-16T10:00:00Z".to_owned(),
            updated_at: "2026-07-16T10:00:00Z".to_owned(),
            supersedes_id: None,
            expires_at: None,
        };
        insert_memory_record_row(&conn, &original, InsertMode::Create)?;
        conn.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'old-tag')",
            [&original.id],
        )?;
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path) VALUES ('old-path', ?1, 'old/**')",
            [&original.id],
        )?;
        conn.execute(
            "INSERT INTO memory_capture(record_id, provenance_json) VALUES (?1, '{}')",
            [&original.id],
        )?;
        let original_rowid: i64 = conn.query_row(
            "SELECT rowid FROM memory_record WHERE id = ?1",
            [&original.id],
            |row| row.get(0),
        )?;

        let mut referring = original.clone();
        referring.id = "session-referring-record".to_owned();
        referring.destination = MemoryDestination::Session;
        referring.title = "Referring runtime record".to_owned();
        referring.body = "This foreign key rules out delete-and-reinsert replacement.".to_owned();
        referring.content_hash = "referring-content".to_owned();
        referring.supersedes_id = Some(original.id.clone());
        insert_memory_record_row(&conn, &referring, InsertMode::Create)?;

        let mut replacement = original.clone();
        replacement.memory_type = MemoryType::Decision;
        replacement.lane = MemoryLane::Episodic;
        replacement.destination = MemoryDestination::Session;
        replacement.scope_kind = ScopeKind::Agent;
        replacement.scope_id = Some("agent:replacement".to_owned());
        replacement.title = "Freshlexeme runtime title".to_owned();
        replacement.body = "The exact replacement owns the new full-text projection.".to_owned();
        replacement.status = MemoryStatus::Superseded;
        replacement.confidence = 0.75;
        replacement.source_kind = Some("new-source".to_owned());
        replacement.source_ref = Some("new-ref".to_owned());
        replacement.proposal_id = Some("proposal-replacement".to_owned());
        replacement.content_hash = "new-content".to_owned();
        replacement.created_at = "2026-07-16T09:00:00Z".to_owned();
        replacement.updated_at = "2026-07-16T12:00:00Z".to_owned();
        replacement.expires_at = Some("2026-07-17T12:00:00Z".to_owned());
        let replacement = RuntimeRecordSnapshot::from_parts(
            replacement,
            vec!["new-tag".to_owned()],
            vec![MemoryPath {
                path: "new/**".to_owned(),
                symbol: Some("replacement_symbol".to_owned()),
                line_start: Some(2),
                line_end: Some(4),
            }],
        );

        let tx = conn.unchecked_transaction()?;
        replace_runtime_record_snapshot_exact(&tx, &replacement)?;
        tx.commit()?;

        let replacement_rowid: i64 = conn.query_row(
            "SELECT rowid FROM memory_record WHERE id = ?1",
            [&original.id],
            |row| row.get(0),
        )?;
        assert_eq!(replacement_rowid, original_rowid);
        let stored = runtime_record_snapshots(&conn)?
            .into_iter()
            .find(|snapshot| snapshot.record().id == original.id);
        assert_eq!(stored.as_ref(), Some(&replacement));
        assert_eq!(record_by_id(&conn, &referring.id)?, Some(referring));
        let capture_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_capture WHERE record_id = ?1",
            [&original.id],
            |row| row.get(0),
        )?;
        assert_eq!(capture_count, 0);
        let old_fts_count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM memory_fts
             JOIN memory_record ON memory_record.rowid = memory_fts.rowid
             WHERE memory_fts MATCH 'obsoletelexeme' AND memory_record.id = ?1",
            [&original.id],
            |row| row.get(0),
        )?;
        let new_fts_count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM memory_fts
             JOIN memory_record ON memory_record.rowid = memory_fts.rowid
             WHERE memory_fts MATCH 'freshlexeme' AND memory_record.id = ?1",
            [&original.id],
            |row| row.get(0),
        )?;
        assert_eq!(old_fts_count, 0);
        assert_eq!(new_fts_count, 1);
        let mut foreign_key_check = conn.prepare("PRAGMA foreign_key_check")?;
        assert!(foreign_key_check.query([])?.next()?.is_none());
        conn.execute(
            "INSERT INTO memory_fts(memory_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )?;
        Ok(())
    }
}
