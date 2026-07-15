use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::{MemoryDestination, MemoryPaths, MemoryRecord, MemoryStatus, okf};

use super::super::safe_files::ensure_safe_directory;
use super::{super::runtime_records::RuntimeRecords, RepoIndexDrift};

pub(super) fn inspect(paths: &MemoryPaths, conn: &Connection) -> Result<RepoIndexDrift> {
    ensure_safe_directory(
        &paths.project_root,
        &paths.records_dir(),
        false,
        "canonical record root",
    )?;
    let canonical = okf::read_okf_record_files(paths.records_dir())?
        .into_iter()
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
    let fts_out_of_sync = !fts_content_index_is_current(conn)?;

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
        && indexed.expires_at == canonical.expires_at
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
