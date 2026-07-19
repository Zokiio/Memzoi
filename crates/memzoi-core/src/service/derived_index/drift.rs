use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::{MemoryDestination, MemoryPaths, MemoryRecord, MemoryStatus, okf};

use super::{
    super::runtime_records::RuntimeRecords,
    RepoIndexDrift,
    admission::{RepositoryRecordAdmission, read_admitted_repository_record_snapshots},
};

pub(super) fn inspect(paths: &MemoryPaths, conn: &Connection) -> Result<RepoIndexDrift> {
    inspect_with_admission(
        paths,
        conn,
        RepositoryRecordAdmission::EnforceForRepositoryReads,
        false,
    )
}

pub(super) fn inspect_read_only(paths: &MemoryPaths, conn: &Connection) -> Result<RepoIndexDrift> {
    inspect_with_admission(
        paths,
        conn,
        RepositoryRecordAdmission::EnforceForRepositoryReads,
        true,
    )
}

pub(super) fn inspect_for_trusted_recall_eval(
    paths: &MemoryPaths,
    conn: &Connection,
) -> Result<RepoIndexDrift> {
    inspect_with_admission(
        paths,
        conn,
        RepositoryRecordAdmission::TrustedRecallEvaluationBypass,
        false,
    )
}

fn inspect_with_admission(
    paths: &MemoryPaths,
    conn: &Connection,
    admission: RepositoryRecordAdmission,
    read_only: bool,
) -> Result<RepoIndexDrift> {
    let canonical = read_admitted_repository_record_snapshots(paths, admission, || Ok(()))?
        .into_iter()
        .map(|snapshot| snapshot.record)
        .filter(|record| record.status == MemoryStatus::Active)
        .map(|record| (record.concept_id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let indexed = RuntimeRecords::new(conn)
        .indexed_active_for_destination(MemoryDestination::Repo)?
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<BTreeMap<_, _>>();

    let missing_from_index = canonical
        .keys()
        .filter(|id| !indexed.contains_key(*id))
        .cloned()
        .collect();
    let stale_in_index = indexed
        .keys()
        .filter(|id| !canonical.contains_key(*id))
        .cloned()
        .collect();
    let changed_in_index = canonical
        .iter()
        .filter_map(|(id, canonical)| {
            indexed
                .get(id)
                .filter(|indexed| !repo_record_matches(canonical, indexed))
                .map(|_| id.clone())
        })
        .collect();
    let fts_out_of_sync = if read_only {
        !fts_content_index_is_current_read_only(conn)?
    } else {
        !fts_content_index_is_current(conn)?
    };

    Ok(RepoIndexDrift {
        missing_from_index,
        stale_in_index,
        changed_in_index,
        fts_out_of_sync,
    })
}

pub(super) fn repo_record_matches(canonical: &okf::OkfRecordFile, indexed: &MemoryRecord) -> bool {
    let draft = &canonical.draft;
    indexed.memory_type == draft.memory_type
        && indexed.lane == draft.lane
        && indexed.destination == MemoryDestination::Repo
        && indexed.scope_kind == draft.scope_kind
        && indexed.scope_id == draft.scope_id
        && indexed.visibility == draft.visibility
        && indexed.title == draft.title
        && indexed.body == draft.body
        && indexed.status == canonical.status
        && indexed.confidence == draft.confidence
        && indexed.source_kind == draft.source_kind
        && indexed.source_ref == draft.source_ref
        && indexed.proposal_id == canonical.proposal_id
        && indexed.content_hash == blake3::hash(draft.body.as_bytes()).to_hex().to_string()
        && indexed.created_at == canonical.created
        && indexed.updated_at == canonical.updated.as_deref().unwrap_or(&canonical.created)
        && indexed.supersedes_id == canonical.supersedes_id
        && indexed.retention == canonical.retention
        && indexed.origin == canonical.origin
        && indexed.lineage == canonical.lineage
}

pub(super) fn fts_content_index_is_current(conn: &Connection) -> Result<bool> {
    match conn.execute(
        "INSERT INTO memory_fts(memory_fts, rank) VALUES ('integrity-check', 1)",
        [],
    ) {
        Ok(_) => Ok(true),
        Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt) => {
            Ok(false)
        }
        Err(error) => Err(error).context("failed to verify full-text index integrity"),
    }
}

/// Verify external-content FTS state without issuing the FTS5 integrity-check
/// INSERT, which SQLite correctly rejects on an immutable connection.
///
/// The expected index and both vocab readers live only in SQLite's temporary
/// schema. Comparing every indexed token instance (term, record, column, and
/// offset) detects missing, stale, and orphaned index entries without changing
/// either managed database.
fn fts_content_index_is_current_read_only(conn: &Connection) -> Result<bool> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS temp.memzoi_expected_memory_fts
             USING fts5(title, body);
         DELETE FROM temp.memzoi_expected_memory_fts;
         INSERT INTO temp.memzoi_expected_memory_fts(rowid, title, body)
             SELECT rowid, title, body FROM main.memory_record;
         CREATE VIRTUAL TABLE IF NOT EXISTS temp.memzoi_actual_memory_fts_vocab
             USING fts5vocab(main, memory_fts, 'instance');
         CREATE VIRTUAL TABLE IF NOT EXISTS temp.memzoi_expected_memory_fts_vocab
             USING fts5vocab(temp, memzoi_expected_memory_fts, 'instance');",
    )
    .context("failed to prepare read-only full-text index verification")?;

    let differs = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM (
                     SELECT term, doc, col, offset
                     FROM temp.memzoi_actual_memory_fts_vocab
                     EXCEPT
                     SELECT term, doc, col, offset
                     FROM temp.memzoi_expected_memory_fts_vocab
                 )
                 UNION ALL
                 SELECT 1 FROM (
                     SELECT term, doc, col, offset
                     FROM temp.memzoi_expected_memory_fts_vocab
                     EXCEPT
                     SELECT term, doc, col, offset
                     FROM temp.memzoi_actual_memory_fts_vocab
                 )
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to compare read-only full-text index contents")?;
    Ok(!differs)
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::fts_content_index_is_current_read_only;

    #[test]
    fn read_only_fts_verification_detects_missing_token_instances() -> anyhow::Result<()> {
        let conn = Connection::open_in_memory()?;
        crate::schema::init(&conn)?;
        conn.execute(
            "INSERT INTO memory_record(
                 id, type, scope_kind, title, body, status,
                 retention_json, origin_json, content_hash
             ) VALUES (?1, 'fact', 'repo', ?2, ?3, 'active', '{}', '{}', ?4)",
            params![
                "fts-read-only-verification",
                "Immutable FTS verification",
                "The read-only verifier compares every token instance.",
                "test-content-hash"
            ],
        )?;
        assert!(fts_content_index_is_current_read_only(&conn)?);

        let (rowid, title, body) = conn.query_row(
            "SELECT rowid, title, body FROM memory_record WHERE id = ?1",
            ["fts-read-only-verification"],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        conn.execute(
            "INSERT INTO memory_fts(memory_fts, rowid, title, body)
             VALUES ('delete', ?1, ?2, ?3)",
            params![rowid, title, body],
        )?;

        assert!(!fts_content_index_is_current_read_only(&conn)?);
        Ok(())
    }
}
