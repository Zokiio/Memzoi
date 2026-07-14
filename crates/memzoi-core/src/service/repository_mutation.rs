use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    AuthorizedRepositoryWriteBatch, MemoryPaths, RepositoryProjectionPurpose, RepositoryWriteRoute,
    SafetyFieldKind, repository_io,
};

use super::{
    borrowed_repository_projections,
    safe_files::{ensure_safe_directory, remove_staged_file, sync_directory},
};

#[derive(Debug)]
pub(super) struct RepositorySafetyValue {
    pub(super) location: String,
    pub(super) kind: SafetyFieldKind,
    pub(super) value: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct OwnedRepositoryProjection {
    pub(super) relative_path: PathBuf,
    pub(super) bytes: Vec<u8>,
    pub(super) target_revision: Option<String>,
    pub(super) purpose: RepositoryProjectionPurpose,
}

#[derive(Debug)]
pub(super) struct AuthorizedRepositoryProjectionBatch {
    pub(super) capability: AuthorizedRepositoryWriteBatch,
    pub(super) policy_context_digest: [u8; 32],
}

impl AuthorizedRepositoryProjectionBatch {
    pub(super) fn digest(&self) -> String {
        self.capability.digest()
    }
}

impl OwnedRepositoryProjection {
    pub(super) fn from_absolute(
        paths: &MemoryPaths,
        path: &Path,
        bytes: &[u8],
        target_revision: Option<&str>,
    ) -> Result<Self> {
        let relative_path = path
            .strip_prefix(&paths.project_root)
            .context("repository projection is outside the current project")?
            .to_path_buf();
        Ok(Self {
            relative_path,
            bytes: bytes.to_vec(),
            target_revision: target_revision.map(str::to_owned),
            purpose: RepositoryProjectionPurpose::Write,
        })
    }

    pub(super) fn existing_from_absolute(
        paths: &MemoryPaths,
        path: &Path,
        bytes: &[u8],
        target_revision: &str,
    ) -> Result<Self> {
        let mut projection = Self::from_absolute(paths, path, bytes, Some(target_revision))?;
        if blake3::hash(bytes).to_hex().as_str() != target_revision {
            bail!("existing repository projection revision does not match its bytes");
        }
        projection.purpose = RepositoryProjectionPurpose::Existing;
        Ok(projection)
    }
}

#[derive(Clone, Copy)]
pub(super) struct RepositoryMutationAuthorization<'a> {
    pub(super) route: RepositoryWriteRoute,
    pub(super) authorization: &'a AuthorizedRepositoryProjectionBatch,
    pub(super) projections: &'a [OwnedRepositoryProjection],
}

pub(super) fn repository_transaction_root(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join("repository-transactions")
}

pub(super) fn repository_transaction_path(
    paths: &MemoryPaths,
    repository_path: &Path,
    nonce: &str,
    role: &str,
) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi.repository-transaction-path.v1\0");
    hasher.update(repository_path.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    hasher.update(nonce.as_bytes());
    hasher.update(b"\0");
    hasher.update(role.as_bytes());
    repository_transaction_root(paths).join(format!(
        ".{nonce}.{}.{}.tmp",
        hasher.finalize().to_hex(),
        role
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_authorized_file(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    final_path: &Path,
    contents: &str,
    nonce: &str,
) -> Result<PathBuf> {
    let borrowed = borrowed_repository_projections(projections);
    repository_io::verify_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )?;
    let expected =
        OwnedRepositoryProjection::from_absolute(paths, final_path, contents.as_bytes(), None)?;
    if !projections.iter().any(|projection| {
        projection.purpose == RepositoryProjectionPurpose::Write
            && projection.relative_path == expected.relative_path
            && projection.bytes == expected.bytes
    }) {
        bail!("staged repository file is not present in the authorized projection batch");
    }
    let temp_path = repository_transaction_path(paths, final_path, nonce, "write");
    let parent = temp_path.parent().context("staged file has no parent")?;
    if parent.starts_with(&paths.project_root) {
        bail!("local repository transaction storage must be outside the project worktree");
    }
    ensure_safe_directory(
        &paths.runtime_dir,
        parent,
        true,
        "local repository transaction root",
    )?;
    if parent
        .canonicalize()
        .context("failed to resolve local repository transaction root")?
        .starts_with(
            paths
                .project_root
                .canonicalize()
                .context("failed to resolve project root for repository staging")?,
        )
    {
        bail!("local repository transaction storage must be outside the project worktree");
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .with_context(|| format!("failed to stage file {}", final_path.display()))?;
    if let Err(error) = file
        .write_all(contents.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let stage_error = anyhow::Error::new(error)
            .context(format!("failed to stage file {}", final_path.display()));
        return match remove_staged_file(&temp_path) {
            Ok(()) => Err(stage_error),
            Err(cleanup_error) => Err(stage_error).context(format!(
                "additionally failed to remove incomplete staged file {}: {cleanup_error:#}",
                temp_path.display()
            )),
        };
    }
    sync_directory(parent).context("failed to sync local repository transaction root")?;
    Ok(temp_path)
}

pub(super) fn install_verified_staged_file_no_replace(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    staged: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<()> {
    let relative = destination
        .strip_prefix(&paths.project_root)
        .context("repository install destination is outside the project root")?;
    let matches = mutation
        .projections
        .iter()
        .enumerate()
        .filter(|(_, projection)| {
            projection.relative_path == relative
                && blake3::hash(&projection.bytes).to_hex().as_str() == expected_hash
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [projection_index] = matches.as_slice() else {
        bail!("repository install must select exactly one authorized path and byte projection");
    };
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::install_transaction_file_no_replace(
        &paths.project_root,
        mutation.route,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        *projection_index,
        &repository_transaction_root(paths),
        staged,
    )
}

pub(super) fn restore_verified_staged_file_no_replace(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    staged: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<()> {
    install_verified_staged_file_no_replace(paths, mutation, staged, destination, expected_hash)?;
    remove_staged_file(staged).with_context(|| {
        format!(
            "failed to remove restored transaction source {}",
            staged.display()
        )
    })
}

pub(super) fn backup_repository_file_to_transaction(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    source: &Path,
    backup: &Path,
    expected_hash: &str,
) -> Result<()> {
    let relative = source
        .strip_prefix(&paths.project_root)
        .context("repository backup source is outside the project root")?;
    let matches = mutation
        .projections
        .iter()
        .enumerate()
        .filter(|(_, projection)| {
            projection.purpose == RepositoryProjectionPurpose::Existing
                && projection.relative_path == relative
                && projection.target_revision.as_deref() == Some(expected_hash)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [projection_index] = matches.as_slice() else {
        bail!("repository backup must select exactly one authorized target revision");
    };
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::backup_repository_file(
        &paths.project_root,
        mutation.route,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        *projection_index,
        &repository_transaction_root(paths),
        backup,
    )
}

pub(super) fn remove_installed_repository_file(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    path: &Path,
    expected_hash: &str,
) -> Result<()> {
    let relative = path
        .strip_prefix(&paths.project_root)
        .context("repository rollback path is outside the project root")?;
    let matches = mutation
        .projections
        .iter()
        .enumerate()
        .filter(|(_, projection)| {
            projection.purpose == RepositoryProjectionPurpose::Write
                && projection.relative_path == relative
                && blake3::hash(&projection.bytes).to_hex().as_str() == expected_hash
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [projection_index] = matches.as_slice() else {
        bail!("repository removal must select exactly one authorized path and byte projection");
    };
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::remove_repository_file_if_matching(
        &paths.project_root,
        mutation.route,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        *projection_index,
    )
}
