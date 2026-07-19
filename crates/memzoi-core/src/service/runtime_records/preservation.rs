use std::collections::BTreeSet;

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{MemoryDestination, MemoryPath, MemoryRecord};

use super::{
    PrivateLifecycleRelation, PrivateLifecycleState, PrivateLifecycleStorage,
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
    lifecycle_state: PrivateLifecycleState,
    lifecycle_relations: Vec<PrivateLifecycleRelation>,
}

pub(super) fn runtime_record_snapshots(conn: &Connection) -> Result<Vec<RuntimeRecordSnapshot>> {
    records_for_runtime_preservation(conn)?
        .into_iter()
        .map(|record| runtime_record_snapshot(conn, record))
        .collect()
}

pub(super) fn runtime_record_snapshots_for_ids(
    conn: &Connection,
    record_ids: &BTreeSet<String>,
) -> Result<Vec<RuntimeRecordSnapshot>> {
    let mut snapshots = Vec::with_capacity(record_ids.len());
    for record_id in record_ids {
        let Some(record) = record_by_id(conn, record_id)? else {
            continue;
        };
        if matches!(
            record.destination,
            MemoryDestination::Local | MemoryDestination::Session
        ) {
            snapshots.push(runtime_record_snapshot(conn, record)?);
        }
    }
    Ok(snapshots)
}

fn runtime_record_snapshot(
    conn: &Connection,
    record: MemoryRecord,
) -> Result<RuntimeRecordSnapshot> {
    let tags = record_tags(conn, &record.id)?;
    let paths = runtime_record_paths(conn, &record.id)?;
    let lifecycle = PrivateLifecycleStorage::new(conn);
    let lifecycle_state = lifecycle.require_state(&record.id)?;
    let lifecycle_relations = lifecycle.relations_for_subject(&record.id)?;
    Ok(RuntimeRecordSnapshot {
        record,
        tags,
        paths,
        lifecycle_state,
        lifecycle_relations,
    })
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
    #[cfg(test)]
    fn from_parts(
        record: MemoryRecord,
        tags: Vec<String>,
        paths: Vec<MemoryPath>,
        lifecycle_state: PrivateLifecycleState,
    ) -> Self {
        Self {
            record,
            tags,
            paths,
            lifecycle_state,
            lifecycle_relations: Vec::new(),
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
    // Records must all exist before cross-record lifecycle relations are
    // restored. Their INSERT triggers create temporary random lifecycle state.
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
    // Relations are append-only and may point at a record restored later in the
    // input batch. Inserting them rotates the temporary endpoint versions.
    for snapshot in records {
        for relation in &snapshot.lifecycle_relations {
            conn.execute(
                "INSERT OR IGNORE INTO private_lifecycle_relation(
                   id, relation_kind, subject_record_id, related_record_id, application_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    relation.id,
                    relation.relation_kind.as_str(),
                    relation.subject_record_id,
                    relation.related_record_id,
                    relation.application_id,
                    relation.created_at,
                ],
            )?;
            if PrivateLifecycleStorage::new(conn)
                .relation(&relation.id)?
                .as_ref()
                != Some(relation)
            {
                bail!(
                    "private lifecycle relation {} collides with a different indexed relation",
                    relation.id
                );
            }
        }
    }
    // Restore independent lifecycle facts, then install the authoritative
    // opaque tokens last. The facts UPDATE intentionally fires rotation
    // triggers; the final token-only UPDATE does not.
    for snapshot in records {
        let state = &snapshot.lifecycle_state;
        let changed = conn.execute(
            "UPDATE private_lifecycle_state
             SET automatic_recall_until = ?1,
                 validity_until = ?2,
                 retain_until = ?3,
                 pinned = ?4,
                 quarantined = ?5,
                 quarantine_reason_code = ?6,
                 automatic_recall_event_id = ?7,
                 validity_event_id = ?8,
                 retention_event_id = ?9,
                 quarantine_event_id = ?10
             WHERE record_id = ?11",
            rusqlite::params![
                state.automatic_recall_until,
                state.validity_until,
                state.retain_until,
                state.pinned,
                state.quarantined,
                state.quarantine_reason_code,
                state.automatic_recall_event_id,
                state.validity_event_id,
                state.retention_event_id,
                state.quarantine_event_id,
                state.record_id,
            ],
        )?;
        if changed != 1 {
            bail!(
                "private lifecycle state disappeared while restoring {}",
                state.record_id
            );
        }
        let changed = conn.execute(
            "UPDATE private_lifecycle_state
             SET record_version = ?1, updated_at = ?2
             WHERE record_id = ?3",
            rusqlite::params![state.record_version, state.updated_at, state.record_id],
        )?;
        if changed != 1 {
            bail!(
                "private lifecycle version disappeared while restoring {}",
                state.record_id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        MemoryDestination, MemoryLane, MemoryStatus, MemoryType, OriginDescriptor, OriginRoute,
        ScopeKind, Visibility, db, retention_facts_for_creation,
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
            retention: retention_facts_for_creation(
                MemoryLane::Semantic,
                "2026-07-16T12:00:00Z",
                None,
                None,
            )?,
            origin: OriginDescriptor::new("test:repo-collision", OriginRoute::OwnerCommand),
            lineage: None,
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
        let lifecycle_state = PrivateLifecycleState {
            record_id: runtime_record.id.clone(),
            automatic_recall_until: None,
            validity_until: None,
            retain_until: None,
            pinned: false,
            quarantined: false,
            quarantine_reason_code: None,
            record_version: "0123456789abcdef0123456789abcdef".to_owned(),
            automatic_recall_event_id: None,
            validity_event_id: None,
            retention_event_id: None,
            quarantine_event_id: None,
            updated_at: runtime_record.updated_at.clone(),
        };
        let snapshot = RuntimeRecordSnapshot::from_parts(
            runtime_record,
            vec!["local-tag".to_owned()],
            vec![MemoryPath {
                path: "crates/local/**".to_owned(),
                symbol: None,
                line_start: None,
                line_end: None,
            }],
            lifecycle_state,
        );

        let error = restore_runtime_record_snapshots(&conn, &[snapshot])
            .expect_err("a repo-owned identifier must fail before child restoration");
        assert!(format!("{error:#}").contains("collides"));
        assert_eq!(record_tags(&conn, &repo_record.id)?, vec!["repo-tag"]);
        assert!(runtime_record_paths(&conn, &repo_record.id)?.is_empty());
        assert_eq!(record_by_id(&conn, &repo_record.id)?, Some(repo_record));
        Ok(())
    }
}
