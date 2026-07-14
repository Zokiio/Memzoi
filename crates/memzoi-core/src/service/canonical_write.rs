use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Transaction;
use uuid::Uuid;

use crate::{MemoryPaths, MemoryRecord, RepositoryWriteRoute, okf, repository_io};

use super::{
    canonical_write_projections,
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection,
        RepositoryMutationAuthorization, backup_repository_file_to_transaction,
        install_verified_staged_file_no_replace, remove_installed_repository_file,
        repository_transaction_path, restore_verified_staged_file_no_replace,
        stage_authorized_file,
    },
    safe_files::{
        RepoLifecycleLock, ensure_path_absent, ensure_regular_file, ensure_safe_path_parent,
        file_content_hash, remove_staged_file,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileWriteMode {
    CreateNew,
    Overwrite,
}

#[derive(Debug)]
pub(super) struct CanonicalFileWrite {
    pub(super) record_file: okf::OkfRecordFile,
    pub(super) path: PathBuf,
    pub(super) markdown: String,
    pub(super) mode: FileWriteMode,
    pub(super) expected_existing_hash: Option<String>,
}

#[derive(Debug)]
pub(super) struct StagedCanonicalFileWrite {
    pub(super) path: PathBuf,
    pub(super) temp_path: PathBuf,
    pub(super) backup_path: Option<PathBuf>,
    pub(super) mode: FileWriteMode,
    pub(super) expected_existing_hash: Option<String>,
    pub(super) expected_staged_hash: String,
    pub(super) installed: bool,
}

/// Holds the repository lifecycle lock from canonical precondition capture
/// through the SQLite commit and filesystem finalization.
pub(super) struct CanonicalWriteSession<'a> {
    paths: &'a MemoryPaths,
    _lock: RepoLifecycleLock,
}

impl<'a> CanonicalWriteSession<'a> {
    pub(super) fn begin(paths: &'a MemoryPaths) -> Result<Self> {
        Ok(Self {
            paths,
            _lock: RepoLifecycleLock::acquire(paths)?,
        })
    }

    pub(super) fn prepare_create(
        &self,
        record: MemoryRecord,
        tags: Vec<String>,
        applies_to: Vec<String>,
    ) -> Result<CanonicalFileWrite> {
        prepare_canonical_file_write(
            self.paths,
            record,
            tags,
            applies_to,
            FileWriteMode::CreateNew,
        )
    }

    pub(super) fn prepare_replace(
        &self,
        record: MemoryRecord,
        tags: Vec<String>,
        applies_to: Vec<String>,
    ) -> Result<CanonicalFileWrite> {
        prepare_canonical_file_write(
            self.paths,
            record,
            tags,
            applies_to,
            FileWriteMode::Overwrite,
        )
    }

    pub(super) fn commit(
        self,
        expected_route: RepositoryWriteRoute,
        authorization: &AuthorizedRepositoryProjectionBatch,
        tx: Transaction<'_>,
        writes: &[CanonicalFileWrite],
    ) -> Result<()> {
        commit_db_and_canonical_writes(self.paths, expected_route, authorization, tx, writes)
    }

    pub(super) fn commit_with_hooks<BeforeInstall, BeforeCommit>(
        self,
        expected_route: RepositoryWriteRoute,
        authorization: &AuthorizedRepositoryProjectionBatch,
        tx: Transaction<'_>,
        writes: &[CanonicalFileWrite],
        before_install: BeforeInstall,
        before_commit: BeforeCommit,
    ) -> Result<()>
    where
        BeforeInstall: FnMut(usize) -> Result<()>,
        BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
    {
        commit_db_and_canonical_writes_with_hooks(
            self.paths,
            expected_route,
            authorization,
            tx,
            writes,
            before_install,
            before_commit,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_with_backup_hook<BeforeInstall, AfterBackup, BeforeCommit>(
        self,
        expected_route: RepositoryWriteRoute,
        authorization: &AuthorizedRepositoryProjectionBatch,
        tx: Transaction<'_>,
        writes: &[CanonicalFileWrite],
        before_install: BeforeInstall,
        after_backup: AfterBackup,
        before_commit: BeforeCommit,
    ) -> Result<()>
    where
        BeforeInstall: FnMut(usize) -> Result<()>,
        AfterBackup: FnMut(usize, &Path) -> Result<()>,
        BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
    {
        commit_db_and_canonical_writes_with_backup_hook(
            self.paths,
            expected_route,
            authorization,
            tx,
            writes,
            before_install,
            after_backup,
            before_commit,
        )
    }
}

pub(super) fn prepare_canonical_file_write(
    paths: &MemoryPaths,
    record: MemoryRecord,
    tags: Vec<String>,
    applies_to: Vec<String>,
    mode: FileWriteMode,
) -> Result<CanonicalFileWrite> {
    let records_root = paths.records_dir();
    let path = records_root.join(format!("{}.md", record.id));
    ensure_safe_path_parent(
        &paths.project_root,
        &records_root,
        &path,
        false,
        "canonical memory record",
    )
    .with_context(|| {
        format!(
            "failed to inspect canonical memory record {}",
            path.display()
        )
    })?;
    match mode {
        FileWriteMode::CreateNew => ensure_path_absent(&path, "canonical memory record")?,
        FileWriteMode::Overwrite => ensure_regular_file(&path, "canonical memory record")?,
    }
    let expected_existing_hash = match mode {
        FileWriteMode::CreateNew => None,
        FileWriteMode::Overwrite => Some(file_content_hash(&path)?),
    };
    let markdown = okf::render_memory_record_markdown(&record, &tags, &applies_to);
    let record_file = okf::parse_okf_record_markdown(&records_root, &path, &markdown)?
        .context("projected canonical record was ignored")?;
    Ok(CanonicalFileWrite {
        record_file,
        path,
        markdown,
        mode,
        expected_existing_hash,
    })
}

pub(super) fn validate_canonical_write_precondition(
    paths: &MemoryPaths,
    write: &StagedCanonicalFileWrite,
) -> Result<()> {
    let records_root = paths.records_dir();
    ensure_safe_path_parent(
        &paths.project_root,
        &records_root,
        &write.path,
        false,
        "canonical memory record",
    )?;
    match write.mode {
        FileWriteMode::CreateNew => ensure_path_absent(&write.path, "canonical memory record"),
        FileWriteMode::Overwrite => {
            ensure_regular_file(&write.path, "canonical memory record")?;
            let expected = write
                .expected_existing_hash
                .as_deref()
                .context("overwrite write is missing captured canonical hash")?;
            let actual = file_content_hash(&write.path)?;
            if actual != expected {
                bail!(
                    "canonical target changed after validation: {}",
                    write.path.display()
                );
            }
            Ok(())
        }
    }
}

fn verify_staged_file_contents(path: &Path, expected_hash: &str) -> Result<()> {
    ensure_regular_file(path, "staged repository file")?;
    if file_content_hash(path)? != expected_hash {
        bail!("staged repository bytes changed after authorization");
    }
    Ok(())
}

pub(super) fn stage_canonical_writes(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    writes: &[CanonicalFileWrite],
    nonce: &str,
) -> Result<Vec<StagedCanonicalFileWrite>> {
    let borrowed = super::borrowed_repository_projections(projections);
    repository_io::verify_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )?;
    let mut staged = Vec::with_capacity(writes.len());
    for write in writes {
        let temp_path = match stage_authorized_file(
            paths,
            expected_route,
            authorization,
            projections,
            &write.path,
            &write.markdown,
            nonce,
        ) {
            Ok(path) => path,
            Err(error) => {
                return attach_cleanup_error(
                    error,
                    cleanup_staged_canonical_writes(&staged),
                    "partial canonical staging cleanup",
                );
            }
        };
        let backup_path = (write.mode == FileWriteMode::Overwrite)
            .then(|| repository_transaction_path(paths, &write.path, nonce, "canonical"));
        staged.push(StagedCanonicalFileWrite {
            path: write.path.clone(),
            temp_path,
            backup_path,
            mode: write.mode,
            expected_existing_hash: write.expected_existing_hash.clone(),
            expected_staged_hash: blake3::hash(write.markdown.as_bytes()).to_hex().to_string(),
            installed: false,
        });
    }
    Ok(staged)
}

pub(super) fn install_staged_canonical_writes<BeforeInstall>(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    writes: &mut [StagedCanonicalFileWrite],
    before_install: BeforeInstall,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
{
    install_staged_canonical_writes_with_backup_hook(
        paths,
        mutation,
        writes,
        before_install,
        |_, _| Ok(()),
    )
}

pub(super) fn install_staged_canonical_writes_with_backup_hook<BeforeInstall, AfterBackup>(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    writes: &mut [StagedCanonicalFileWrite],
    mut before_install: BeforeInstall,
    mut after_backup: AfterBackup,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    AfterBackup: FnMut(usize, &Path) -> Result<()>,
{
    for (index, write) in writes.iter_mut().enumerate() {
        before_install(index)?;
        verify_staged_file_contents(&write.temp_path, &write.expected_staged_hash)?;
        validate_canonical_write_precondition(paths, write)?;
        if let Some(backup_path) = &write.backup_path {
            validate_canonical_write_precondition(paths, write)?;
            backup_repository_file_to_transaction(
                paths,
                mutation,
                &write.path,
                backup_path,
                write
                    .expected_existing_hash
                    .as_deref()
                    .context("overwrite write is missing captured canonical hash")?,
            )?;
            after_backup(index, &write.path)?;
        }
        install_verified_staged_file_no_replace(
            paths,
            mutation,
            &write.temp_path,
            &write.path,
            &write.expected_staged_hash,
        )?;
        write.installed = true;
    }
    Ok(())
}

pub(super) fn rollback_staged_canonical_writes(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    writes: &mut [StagedCanonicalFileWrite],
) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes.iter_mut().rev() {
        if write.installed {
            record_cleanup_result(
                &mut errors,
                remove_installed_repository_file(
                    paths,
                    mutation,
                    &write.path,
                    &write.expected_staged_hash,
                ),
                format!("remove installed canonical file {}", write.path.display()),
            );
            write.installed = false;
        }
        if let Some(backup_path) = &write.backup_path
            && backup_path.exists()
        {
            record_cleanup_result(
                &mut errors,
                restore_verified_staged_file_no_replace(
                    paths,
                    mutation,
                    backup_path,
                    &write.path,
                    write
                        .expected_existing_hash
                        .as_deref()
                        .unwrap_or("<missing-canonical-backup-hash>"),
                ),
                format!(
                    "restore canonical backup {} to {}",
                    backup_path.display(),
                    write.path.display()
                ),
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("canonical rollback", errors)
}

pub(super) fn finalize_staged_canonical_writes(writes: &[StagedCanonicalFileWrite]) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes {
        if let Some(backup_path) = &write.backup_path {
            record_cleanup_result(
                &mut errors,
                remove_staged_file(backup_path),
                format!("remove canonical backup {}", backup_path.display()),
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("canonical finalization", errors)
}

pub(super) fn commit_db_and_canonical_writes(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
) -> Result<()> {
    commit_db_and_canonical_writes_with_hooks(
        paths,
        expected_route,
        authorization,
        tx,
        writes,
        |_| Ok(()),
        |_| Ok(()),
    )
}

pub(super) fn commit_db_and_canonical_writes_with_hooks<BeforeInstall, BeforeCommit>(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
    before_install: BeforeInstall,
    before_commit: BeforeCommit,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    commit_db_and_canonical_writes_with_backup_hook(
        paths,
        expected_route,
        authorization,
        tx,
        writes,
        before_install,
        |_, _| Ok(()),
        before_commit,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn commit_db_and_canonical_writes_with_backup_hook<
    BeforeInstall,
    AfterBackup,
    BeforeCommit,
>(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
    before_install: BeforeInstall,
    after_backup: AfterBackup,
    before_commit: BeforeCommit,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    AfterBackup: FnMut(usize, &Path) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let projections = canonical_write_projections(paths, writes)?;
    let mutation = RepositoryMutationAuthorization {
        route: expected_route,
        authorization,
        projections: &projections,
    };
    let borrowed = super::borrowed_repository_projections(&projections);
    repository_io::verify_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )?;
    let nonce = Uuid::now_v7().to_string();
    let mut staged = stage_canonical_writes(
        paths,
        expected_route,
        authorization,
        &projections,
        writes,
        &nonce,
    )?;
    if let Err(error) = install_staged_canonical_writes_with_backup_hook(
        paths,
        mutation,
        &mut staged,
        before_install,
        after_backup,
    ) {
        return attach_cleanup_error(
            error,
            rollback_staged_canonical_writes(paths, mutation, &mut staged),
            "canonical install rollback",
        );
    }
    if let Err(error) = before_commit(&tx) {
        return attach_cleanup_error(
            error,
            rollback_staged_canonical_writes(paths, mutation, &mut staged),
            "canonical pre-commit rollback",
        );
    }
    if let Err(error) = tx.commit() {
        return attach_cleanup_error(
            anyhow::Error::new(error).context("failed to commit memory lifecycle transaction"),
            rollback_staged_canonical_writes(paths, mutation, &mut staged),
            "canonical commit rollback",
        );
    }
    finalize_staged_canonical_writes(&staged)
        .context("memory lifecycle committed but canonical cleanup failed")
}

pub(super) fn cleanup_staged_canonical_writes(writes: &[StagedCanonicalFileWrite]) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes {
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("staged canonical cleanup", errors)
}

fn attach_cleanup_error<T>(
    operation_error: anyhow::Error,
    cleanup: Result<()>,
    label: &str,
) -> Result<T> {
    match cleanup {
        Ok(()) => Err(operation_error),
        Err(cleanup_error) => {
            Err(operation_error).context(format!("{label} also failed: {cleanup_error:#}"))
        }
    }
}

fn record_cleanup_result(errors: &mut Vec<String>, result: Result<()>, operation: String) {
    if let Err(error) = result {
        errors.push(format!("{operation}: {error:#}"));
    }
}

fn finish_cleanup(label: &str, errors: Vec<String>) -> Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{label} failed: {}", errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn changed_target_fails_captured_hash_revalidation() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            temp.path().to_path_buf(),
            temp.path().join("runtime-home"),
        );
        fs::create_dir_all(paths.records_dir())?;
        let path = paths.records_dir().join("captured-target.md");
        fs::write(&path, "original canonical bytes")?;
        let staged = path.with_file_name(".captured-target.write.tmp");
        fs::write(&staged, "replacement bytes")?;
        let write = StagedCanonicalFileWrite {
            path: path.clone(),
            temp_path: staged,
            backup_path: Some(path.with_file_name(".captured-target.backup.tmp")),
            mode: FileWriteMode::Overwrite,
            expected_existing_hash: Some(file_content_hash(&path)?),
            expected_staged_hash: blake3::hash(b"replacement bytes").to_hex().to_string(),
            installed: false,
        };
        fs::write(&path, "concurrent human edit")?;

        let error = validate_canonical_write_precondition(&paths, &write)
            .expect_err("changed target must fail before install");
        assert!(
            error.to_string().contains("changed after validation"),
            "unexpected changed-target error: {error:#}"
        );
        assert_eq!(fs::read_to_string(&path)?, "concurrent human edit");
        Ok(())
    }
}
