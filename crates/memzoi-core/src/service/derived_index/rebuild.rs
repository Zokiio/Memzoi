use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};

use crate::{MemoryPaths, db, okf};

use super::{
    super::{
        runtime_records::{RuntimeRecordSnapshot, RuntimeRecords},
        safe_files::{RepoLifecycleLock, ensure_safe_directory},
    },
    RebuildResult,
};

pub(super) fn rebuild(paths: MemoryPaths) -> Result<RebuildResult> {
    let _lifecycle_lock = RepoLifecycleLock::acquire(&paths)?;
    let records_root = paths.records_dir();
    ensure_safe_directory(
        &paths.project_root,
        &records_root,
        false,
        "canonical record root",
    )?;
    let records = okf::read_okf_record_files(&records_root)?;
    guard_no_open_proposals(&paths.db_path)?;
    let runtime_records = load_runtime_records_for_rebuild(&paths.db_path)?;
    guard_no_runtime_record_id_collisions(&records, &runtime_records)?;
    remove_database_files(&paths.db_path)?;
    let conn = db::open_database(&paths.db_path)?;
    db::init_database(&conn)?;
    okf::import_okf_records(&conn, &records)?;
    RuntimeRecords::new(&conn).restore_snapshots(&runtime_records)?;
    Ok(RebuildResult {
        records_root,
        db_path: paths.db_path,
        record_ids: records
            .into_iter()
            .map(|record| record.concept_id)
            .collect(),
    })
}

fn load_runtime_records_for_rebuild(db_path: &Path) -> Result<Vec<RuntimeRecordSnapshot>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = db::open_database(db_path).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be preserved from {}",
            db_path.display()
        )
    })?;
    db::init_database(&conn).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be migrated before preservation from {}",
            db_path.display()
        )
    })?;
    RuntimeRecords::new(&conn).snapshots().context(
        "rebuild refused because local/session runtime memory could not be loaded for preservation",
    )
}

fn guard_no_runtime_record_id_collisions(
    records: &[okf::OkfRecordFile],
    runtime_records: &[RuntimeRecordSnapshot],
) -> Result<()> {
    if runtime_records.is_empty() {
        return Ok(());
    }

    let repo_ids = records
        .iter()
        .map(|record| record.concept_id.as_str())
        .collect::<BTreeSet<_>>();
    let collisions = runtime_records
        .iter()
        .filter_map(|snapshot| {
            let record = snapshot.record();
            repo_ids
                .contains(record.id.as_str())
                .then_some(record.id.as_str())
        })
        .collect::<Vec<_>>();
    if collisions.is_empty() {
        return Ok(());
    }

    bail!(
        "rebuild refused because local/session runtime memory record id{} would collide with canonical repo record{}: {}",
        if collisions.len() == 1 { "" } else { "s" },
        if collisions.len() == 1 { "" } else { "s" },
        collisions.join(", ")
    );
}

fn guard_no_open_proposals(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }

    let Ok(open_proposals) = open_proposal_summaries(db_path) else {
        return Ok(());
    };
    if !open_proposals.is_empty() {
        let count = open_proposals.len();
        let summaries = open_proposals
            .into_iter()
            .map(|(id, status)| format!("{id} ({status})"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "rebuild refused because {count} open proposal{} would be discarded: {summaries}. Run `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason \"...\"` before rebuilding.",
            if count == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn open_proposal_summaries(db_path: &Path) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_proposal_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'proposal')",
        [],
        |row| row.get(0),
    )?;
    if !has_proposal_table {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, status
         FROM proposal
         WHERE status IN ('pending', 'validated', 'approved')
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn remove_database_files(db_path: &Path) -> Result<()> {
    for path in [
        db_path.to_path_buf(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to remove derived database file {}", path.display())
                });
            }
        }
    }
    Ok(())
}
