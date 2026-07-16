use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};

use crate::{MemoryPaths, db, okf};

use super::{
    super::{
        runtime_records::RuntimeRecordSnapshot,
        safe_files::{RepoLifecycleLock, ensure_safe_directory},
        shared_runtime,
    },
    RebuildResult,
};

pub(super) fn rebuild(paths: MemoryPaths) -> Result<RebuildResult> {
    rebuild_with_options(paths, || Ok(()), true)
}

pub(super) fn rebuild_for_trusted_recall_eval(paths: MemoryPaths) -> Result<RebuildResult> {
    rebuild_with_options(paths, || Ok(()), false)
}

#[cfg(test)]
pub(super) fn rebuild_with_snapshot_hook(
    paths: MemoryPaths,
    after_snapshot: impl FnOnce() -> Result<()>,
) -> Result<RebuildResult> {
    rebuild_with_options(paths, after_snapshot, true)
}

fn rebuild_with_options(
    paths: MemoryPaths,
    after_snapshot: impl FnOnce() -> Result<()>,
    validate_repository_safety: bool,
) -> Result<RebuildResult> {
    shared_runtime::migrate_legacy_runtime_if_needed(&paths)?;
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
    ensure_safe_directory(
        &paths.project_root,
        &records_root,
        false,
        "canonical record root",
    )?;
    let snapshots = okf::read_okf_record_snapshots(&records_root)?;
    after_snapshot()?;
    if validate_repository_safety {
        validate_canonical_record_snapshots_for_rebuild(&paths, &snapshots)?;
    }
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

fn validate_canonical_record_snapshots_for_rebuild(
    paths: &MemoryPaths,
    snapshots: &[okf::OkfRecordSnapshot],
) -> Result<()> {
    for snapshot in snapshots {
        let relative = snapshot
            .path
            .strip_prefix(&paths.project_root)
            .context("canonical record escaped the project root during rebuild")?;
        let report = crate::scan_managed_repository_blob(
            paths.project_root.as_os_str().as_encoded_bytes(),
            relative,
            &snapshot.bytes,
        );
        if !report.allowed {
            let findings = report
                .findings
                .iter()
                .map(|finding| format!("{}:{}", finding.code.as_str(), finding.fingerprint))
                .collect::<Vec<_>>()
                .join(",");
            bail!(
                "rebuild refused because a canonical record failed repository safety validation ({findings})"
            );
        }
    }
    Ok(())
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
