use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};

use crate::{MemoryPaths, db, okf};

use super::{
    super::{
        runtime_records::RuntimeRecordSnapshot, safe_files::RepoLifecycleLock, shared_runtime,
    },
    RebuildResult,
    admission::{RepositoryRecordAdmission, read_admitted_repository_record_snapshots},
};

pub(super) fn rebuild(paths: MemoryPaths) -> Result<RebuildResult> {
    rebuild_with_options(
        paths,
        || Ok(()),
        RepositoryRecordAdmission::EnforceForRepositoryReads,
    )
}

pub(super) fn rebuild_for_trusted_recall_eval(paths: MemoryPaths) -> Result<RebuildResult> {
    rebuild_with_options(
        paths,
        || Ok(()),
        RepositoryRecordAdmission::TrustedRecallEvaluationBypass,
    )
}

#[cfg(test)]
pub(super) fn rebuild_with_snapshot_hook(
    paths: MemoryPaths,
    after_snapshot: impl FnOnce() -> Result<()>,
) -> Result<RebuildResult> {
    rebuild_with_options(
        paths,
        after_snapshot,
        RepositoryRecordAdmission::EnforceForRepositoryReads,
    )
}

fn rebuild_with_options(
    paths: MemoryPaths,
    after_snapshot: impl FnOnce() -> Result<()>,
    admission: RepositoryRecordAdmission,
) -> Result<RebuildResult> {
    shared_runtime::reject_unsupported_runtime_layout(&paths)?;
    let _lifecycle_lock = RepoLifecycleLock::acquire(&paths)?;
    let shared = db::open_database(&paths.shared_db_path).with_context(|| {
        format!(
            "local/session runtime memory could not be preserved: failed to open shared database {} before index rebuild",
            paths.shared_db_path.display()
        )
    })?;
    db::init_database(&shared).with_context(|| {
        format!(
            "local/session runtime memory could not be preserved: failed to initialize shared database {} before index rebuild",
            paths.shared_db_path.display()
        )
    })?;
    shared_runtime::complete_pending_shared_sync_locked(&paths, &shared)?;
    let records_root = paths.records_dir();
    let snapshots = read_admitted_repository_record_snapshots(&paths, admission, after_snapshot)?;
    let records = snapshots
        .into_iter()
        .map(|snapshot| snapshot.record)
        .collect::<Vec<_>>();
    let runtime_records = shared_runtime::load_runtime_snapshots(&paths.shared_db_path)?;
    guard_no_runtime_record_id_collisions(&records, &runtime_records)?;
    remove_database_files(&paths.index_db_path)?;
    let conn = db::open_database(&paths.index_db_path)?;
    db::init_database(&conn)?;
    okf::import_okf_records(&conn, &records)?;
    shared_runtime::refresh_index_mirrors_locked(&paths, &shared, &conn)?;
    Ok(RebuildResult {
        records_root,
        db_path: paths.index_db_path,
        record_ids: records
            .into_iter()
            .map(|record| record.concept_id)
            .collect(),
    })
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
