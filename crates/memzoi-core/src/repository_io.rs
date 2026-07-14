use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{AuthorizedRepositoryWriteBatch, RepositoryProjection, RepositoryWriteRoute};

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum InjectedCreateFileFailure {
    Write,
    Sync,
}

#[cfg(test)]
thread_local! {
    static INJECTED_CREATE_FILE_FAILURE: std::cell::Cell<Option<InjectedCreateFileFailure>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn inject_repository_create_failure(failure: InjectedCreateFileFailure) {
    INJECTED_CREATE_FILE_FAILURE.with(|injected| injected.set(Some(failure)));
}

#[cfg(test)]
fn take_repository_create_failure() -> Option<InjectedCreateFileFailure> {
    INJECTED_CREATE_FILE_FAILURE.with(std::cell::Cell::take)
}

#[cfg(unix)]
fn repository_directory_mode() -> rustix::fs::Mode {
    use rustix::fs::Mode;

    Mode::RUSR | Mode::WUSR | Mode::XUSR | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH
}

#[cfg(unix)]
fn repository_file_mode() -> rustix::fs::Mode {
    use rustix::fs::Mode;

    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH
}

#[cfg(unix)]
fn directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;

    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(unix)]
fn open_repository_parent(
    project_root: &Path,
    relative: &Path,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    use rustix::fs::{CWD, Mode, openat};

    if !crate::repository_write_safety::projection_path_is_safe(relative) {
        bail!("unsafe repository-relative output path");
    }
    let root = project_root
        .canonicalize()
        .context("failed to resolve repository root for secure mutation")?;
    let mut directory = openat(CWD, &root, directory_flags(), Mode::empty())
        .context("failed to pin repository root without following symlinks")?;
    let mut components = relative.components().peekable();
    let mut file_name = None;
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            bail!("unsafe repository-relative output path");
        };
        if components.peek().is_none() {
            file_name = Some(component.to_os_string());
            break;
        }
        directory = openat(&directory, component, directory_flags(), Mode::empty())
            .context("failed to pin repository output parent without following symlinks")?;
    }
    Ok((
        directory,
        file_name.context("repository destination has no file name")?,
    ))
}

#[cfg(unix)]
fn open_transaction_parent(
    transaction_root: &Path,
    path: &Path,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    use rustix::fs::{CWD, Mode, openat};

    let relative = path
        .strip_prefix(transaction_root)
        .context("transaction file is outside the transaction root")?;
    let mut components = relative.components();
    let Some(std::path::Component::Normal(file_name)) = components.next() else {
        bail!("transaction file has an unsafe name");
    };
    if components.next().is_some() {
        bail!("transaction file must be a direct child of the transaction root");
    }
    let root = transaction_root
        .canonicalize()
        .context("failed to resolve repository transaction root")?;
    let directory = openat(CWD, &root, directory_flags(), Mode::empty())
        .context("failed to pin repository transaction root without following symlinks")?;
    Ok((directory, file_name.to_os_string()))
}

#[cfg(unix)]
fn read_regular_at(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    label: &str,
) -> Result<(fs::File, Vec<u8>)> {
    use rustix::fs::{Mode, OFlags, openat};

    let file = openat(
        directory,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("failed to open {label} without following symlinks"))?;
    let mut file = fs::File::from(file);
    if !file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?
        .is_file()
    {
        bail!("{label} must be a regular file");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    Ok((file, bytes))
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn create_authorized_repository_projection(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    projection_index: usize,
    label: &str,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString, fs::File)> {
    use rustix::fs::{OFlags, openat};

    verify_repository_batch(
        project_root,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    let projection = projections
        .get(projection_index)
        .context("authorized repository projection index is out of bounds")?;
    let (directory, file_name) = open_repository_parent(project_root, projection.path)?;
    let file = openat(
        &directory,
        &file_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        repository_file_mode(),
    )
    .with_context(|| format!("failed to create {label} without replacement"))?;
    let mut file = fs::File::from(file);
    #[cfg(test)]
    let injected_failure = take_repository_create_failure();

    #[cfg(test)]
    let persist_result = match injected_failure {
        Some(InjectedCreateFileFailure::Write) => {
            let partial_len = projection.bytes.len().min(1);
            file.write_all(&projection.bytes[..partial_len])
                .and_then(|_| Err(std::io::Error::other("injected repository write failure")))
        }
        Some(InjectedCreateFileFailure::Sync) => file
            .write_all(projection.bytes)
            .and_then(|_| Err(std::io::Error::other("injected repository sync failure"))),
        None => file
            .write_all(projection.bytes)
            .and_then(|_| file.sync_all()),
    };
    #[cfg(not(test))]
    let persist_result = file
        .write_all(projection.bytes)
        .and_then(|_| file.sync_all());

    if let Err(error) = persist_result {
        let persistence_error =
            anyhow::Error::new(error).context(format!("failed to persist {label}"));
        return match cleanup_created_file(
            &directory,
            &file_name,
            &file,
            "incomplete repository destination",
        ) {
            Ok(()) => Err(persistence_error),
            Err(cleanup_error) => Err(persistence_error).context(format!(
                "additionally failed to clean incomplete repository destination: {cleanup_error:#}"
            )),
        };
    }
    Ok((directory, file_name, file))
}

#[cfg(unix)]
fn named_file_still_matches(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    opened: &fs::File,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    use rustix::{
        fs::{AtFlags, statat},
        io::Errno,
    };

    let opened = opened.metadata().context("failed to inspect pinned file")?;
    let named = match statat(directory, file_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(named) => named,
        Err(Errno::NOENT) => return Ok(false),
        Err(error) => return Err(error).context("failed to revalidate pinned file name"),
    };
    Ok(opened.dev() == named.st_dev as u64 && opened.ino() == named.st_ino as u64)
}

#[cfg(unix)]
fn cleanup_created_file(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    opened: &fs::File,
    label: &str,
) -> Result<()> {
    use rustix::fs::{AtFlags, fsync, unlinkat};

    let unlink_result = match named_file_still_matches(directory, file_name, opened) {
        Ok(true) => unlinkat(directory, file_name, AtFlags::empty())
            .with_context(|| format!("failed to unlink {label}")),
        Ok(false) => Err(anyhow::anyhow!(
            "{label} name no longer identifies the created file"
        )),
        Err(error) => Err(error).with_context(|| format!("failed to identify {label}")),
    };
    let sync_result = fsync(directory).with_context(|| format!("failed to sync parent of {label}"));
    match (unlink_result, sync_result) {
        (Ok(()), Ok(())) => Ok(()),
        (unlink, sync) => {
            let mut errors = Vec::new();
            if let Err(error) = unlink {
                errors.push(format!("unlink: {error:#}"));
            }
            if let Err(error) = sync {
                errors.push(format!("directory sync: {error:#}"));
            }
            bail!("{}", errors.join("; "))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_transaction_file_no_replace(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    projection_index: usize,
    transaction_root: &Path,
    staged_path: &Path,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
            projection_index,
            transaction_root,
            staged_path,
        );
        bail!("secure staged repository installation is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        use rustix::fs::fsync;

        let (source_directory, source_name) =
            open_transaction_parent(transaction_root, staged_path)?;
        let (source, bytes) =
            read_regular_at(&source_directory, &source_name, "staged repository file")?;
        verify_repository_batch(
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
        )?;
        let projection = projections
            .get(projection_index)
            .context("authorized repository projection index is out of bounds")?;
        if bytes != projection.bytes {
            bail!("staged repository bytes do not match the authorized projection");
        }
        let (destination_directory, destination_name, destination) =
            create_authorized_repository_projection(
                project_root,
                expected_route,
                expected_policy_context_digest,
                authorization,
                projections,
                projection_index,
                "authorized repository destination",
            )?;
        let source_matches = named_file_still_matches(&source_directory, &source_name, &source)
            .context("failed to revalidate staged repository file during installation");
        if !matches!(source_matches, Ok(true)) {
            let install_error = match source_matches {
                Ok(false) => anyhow::anyhow!("staged repository file changed during installation"),
                Err(error) => error,
                Ok(true) => unreachable!(),
            };
            return match cleanup_created_file(
                &destination_directory,
                &destination_name,
                &destination,
                "repository destination after staged-source revalidation failure",
            ) {
                Ok(()) => Err(install_error),
                Err(cleanup_error) => Err(install_error).context(format!(
                    "additionally failed to clean repository destination: {cleanup_error:#}"
                )),
            };
        }
        if let Err(error) = fsync(&destination_directory) {
            let sync_error =
                anyhow::Error::new(error).context("failed to sync repository destination parent");
            return match cleanup_created_file(
                &destination_directory,
                &destination_name,
                &destination,
                "repository destination after directory-sync failure",
            ) {
                Ok(()) => Err(sync_error),
                Err(cleanup_error) => Err(sync_error).context(format!(
                    "additionally failed to clean repository destination: {cleanup_error:#}"
                )),
            };
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn backup_repository_file(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    projection_index: usize,
    transaction_root: &Path,
    backup_path: &Path,
) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
            projection_index,
            transaction_root,
            backup_path,
        );
        bail!("secure repository backup is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        use rustix::fs::{AtFlags, OFlags, fsync, openat, unlinkat};

        verify_repository_batch(
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
        )?;
        let projection = projections
            .get(projection_index)
            .context("authorized repository projection index is out of bounds")?;
        if projection.purpose != crate::RepositoryProjectionPurpose::Existing {
            bail!("repository backup requires an authorized existing-state projection");
        }
        let expected_revision = projection
            .target_revision
            .context("repository backup projection is missing its target revision")?;
        let (source_directory, source_name) =
            open_repository_parent(project_root, projection.path)?;
        let (source, bytes) =
            read_regular_at(&source_directory, &source_name, "repository source")?;
        if blake3::hash(&bytes).to_hex().as_str() != expected_revision {
            bail!(
                "repository source changed after validation and no longer matches the authorized target revision"
            );
        }
        if bytes != projection.bytes {
            bail!(
                "repository source changed after validation and no longer matches the authorized projection bytes"
            );
        }
        let (backup_directory, backup_name) =
            open_transaction_parent(transaction_root, backup_path)?;
        let backup = openat(
            &backup_directory,
            &backup_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            repository_file_mode(),
        )
        .context("failed to create repository transaction backup without replacement")?;
        let mut backup = fs::File::from(backup);
        if let Err(error) = backup.write_all(&bytes).and_then(|_| backup.sync_all()) {
            let backup_error = anyhow::Error::new(error)
                .context("failed to persist repository transaction backup");
            return match cleanup_created_file(
                &backup_directory,
                &backup_name,
                &backup,
                "incomplete repository transaction backup",
            ) {
                Ok(()) => Err(backup_error),
                Err(cleanup_error) => Err(backup_error).context(format!(
                    "additionally failed to clean incomplete transaction backup: {cleanup_error:#}"
                )),
            };
        }
        if !named_file_still_matches(&source_directory, &source_name, &source)? {
            if named_file_still_matches(&backup_directory, &backup_name, &backup)? {
                let _ = unlinkat(&backup_directory, &backup_name, AtFlags::empty());
            }
            bail!("repository source changed during secure backup");
        }
        if let Err(error) = unlinkat(&source_directory, &source_name, AtFlags::empty()) {
            if named_file_still_matches(&backup_directory, &backup_name, &backup)? {
                let _ = unlinkat(&backup_directory, &backup_name, AtFlags::empty());
            }
            return Err(error).context("failed to remove securely backed-up repository source");
        }
        fsync(&source_directory).context("failed to sync repository source parent")?;
        fsync(&backup_directory).context("failed to sync repository transaction root")?;
        Ok(())
    }
}

pub(crate) fn remove_repository_file_if_matching(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    projection_index: usize,
) -> Result<()> {
    verify_repository_batch(
        project_root,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    #[cfg(not(unix))]
    {
        let _ = (
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
            projection_index,
        );
        bail!("secure repository removal is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        use rustix::fs::{AtFlags, fsync, unlinkat};

        let projection = projections
            .get(projection_index)
            .context("authorized repository projection index is out of bounds")?;
        if projection.purpose != crate::RepositoryProjectionPurpose::Write {
            bail!("repository removal requires an authorized write projection");
        }
        let (directory, file_name) = open_repository_parent(project_root, projection.path)?;
        let (file, bytes) = read_regular_at(&directory, &file_name, "repository rollback file")?;
        if bytes != projection.bytes {
            bail!("repository rollback file does not match the authorized projection bytes");
        }
        if !named_file_still_matches(&directory, &file_name, &file)? {
            bail!("repository rollback file changed before removal");
        }
        unlinkat(&directory, &file_name, AtFlags::empty())
            .context("failed to remove repository rollback file")?;
        fsync(&directory).context("failed to sync repository rollback parent")?;
        Ok(())
    }
}

pub(crate) fn verify_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<()> {
    if !authorization.authorizes(
        expected_route,
        project_root.as_os_str().as_encoded_bytes(),
        expected_policy_context_digest,
        projections,
    ) {
        bail!(
            "repository write authorization does not match the exact route, project, paths, revisions, and bytes"
        );
    }
    for projection in projections {
        verify_projection_path(project_root, projection.path)?;
    }
    Ok(())
}

pub(crate) fn create_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<Vec<PathBuf>> {
    verify_repository_batch(
        project_root,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    for projection in projections {
        if projection.target_revision.is_some() {
            bail!("create-only repository batch cannot contain overwrite projections");
        }
    }

    #[cfg(not(unix))]
    {
        bail!(
            "secure repository file creation is unavailable on this platform; write fails closed"
        );
    }

    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::fd::OwnedFd};

        use rustix::{
            fs::{AtFlags, CWD, Mode, OFlags, fsync, mkdirat, openat, unlinkat},
            io::Errno,
        };

        let root = project_root
            .canonicalize()
            .context("failed to resolve repository root for secure creation")?;
        let directory_flags =
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let directory_mode = repository_directory_mode();
        let file_mode = repository_file_mode();
        let mut destinations = Vec::with_capacity(projections.len());
        let mut created = Vec::<(OwnedFd, OsString)>::with_capacity(projections.len());

        for projection in projections {
            let destination = root.join(projection.path);
            let result = (|| -> Result<(OwnedFd, OsString)> {
                let mut directory = openat(CWD, &root, directory_flags, Mode::empty())
                    .context("failed to open repository root without following symlinks")?;
                let mut components = projection.path.components().peekable();
                let mut file_name = None;
                while let Some(component) = components.next() {
                    let std::path::Component::Normal(component) = component else {
                        bail!("unsafe repository-relative output path");
                    };
                    if components.peek().is_none() {
                        file_name = Some(component.to_os_string());
                        break;
                    }
                    let next_directory = match openat(
                        &directory,
                        component,
                        directory_flags,
                        Mode::empty(),
                    ) {
                        Ok(directory) => directory,
                        Err(Errno::NOENT) => {
                            match mkdirat(&directory, component, directory_mode) {
                                Ok(()) | Err(Errno::EXIST) => {}
                                Err(error) => {
                                    return Err(error).context(
                                        "failed to create authorized repository directory",
                                    );
                                }
                            }
                            openat(&directory, component, directory_flags, Mode::empty()).context(
                                "failed to open authorized repository directory without following symlinks",
                            )?
                        }
                        Err(error) => {
                            return Err(error).context(
                                "failed to open authorized repository directory without following symlinks",
                            );
                        }
                    };
                    directory = next_directory;
                }
                let file_name = file_name.context("repository destination has no file name")?;
                let file = match openat(
                    &directory,
                    &file_name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    file_mode,
                ) {
                    Ok(file) => file,
                    Err(Errno::EXIST) => {
                        bail!(
                            "repository destination already exists: {}",
                            destination.display()
                        )
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "failed to create authorized repository file {}",
                                destination.display()
                            )
                        });
                    }
                };
                let mut file = fs::File::from(file);
                if let Err(error) = file
                    .write_all(projection.bytes)
                    .and_then(|_| file.sync_all())
                {
                    drop(file);
                    let _ = unlinkat(&directory, &file_name, AtFlags::empty());
                    return Err(error).with_context(|| {
                        format!(
                            "failed to write authorized repository file {}",
                            destination.display()
                        )
                    });
                }
                drop(file);
                if let Err(error) = fsync(&directory) {
                    let _ = unlinkat(&directory, &file_name, AtFlags::empty());
                    return Err(error).context("failed to sync authorized repository directory");
                }
                Ok((directory, file_name))
            })();
            let created_file = match result {
                Ok(created_file) => created_file,
                Err(error) => {
                    for (directory, file_name) in created.iter().rev() {
                        let _ = unlinkat(directory, file_name, AtFlags::empty());
                    }
                    return Err(error);
                }
            };
            created.push(created_file);
            destinations.push(destination);
        }
        Ok(destinations)
    }
}

pub(crate) fn verify_projection_path(project_root: &Path, relative: &Path) -> Result<PathBuf> {
    if !crate::repository_write_safety::projection_path_is_safe(relative) {
        bail!("unsafe repository-relative output path");
    }
    let root = project_root
        .canonicalize()
        .context("failed to resolve repository root")?;
    let mut cursor = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            bail!("unsafe repository-relative output path");
        };
        cursor.push(component);
        if components.peek().is_none() {
            match fs::symlink_metadata(&cursor) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!("repository output destination must not be a symlink")
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("failed to inspect repository output destination");
                }
            }
            break;
        }
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("repository output parent is not a safe directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to inspect repository output parent"),
        }
    }
    Ok(project_root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_write_safety::repository_write_policy_context_digest;
    use crate::{
        AuthorizationProof, FreshnessCheck, MemoryDestination, OkfProposalSensitivity,
        ProvenanceAssessment, RepositoryContentClass, RepositoryScope, RepositoryWriteRequest,
        RepositoryWriteRoute, SafetyField, SafetyFieldKind, ScopeKind, Visibility,
        authorize_repository_write,
    };

    fn authorize_create<'a>(
        project_root: &Path,
        projections: &[RepositoryProjection<'a>],
    ) -> ([u8; 32], AuthorizedRepositoryWriteBatch) {
        let fields = projections
            .iter()
            .map(|projection| SafetyField {
                location: "candidate.body",
                kind: SafetyFieldKind::Text,
                value: projection.bytes,
            })
            .collect::<Vec<_>>();
        let request = RepositoryWriteRequest {
            route: RepositoryWriteRoute::FileProposalCreate,
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            scope: RepositoryScope {
                kind: ScopeKind::Repo,
                id: None,
                current_project_identity: project_root.as_os_str().as_encoded_bytes(),
                configured_project_id: None,
            },
            visibility: Visibility::Repo,
            authorization: AuthorizationProof::ExplicitCommand { operation: "test" },
            freshness: Vec::<FreshnessCheck<'_>>::new(),
            provenance: ProvenanceAssessment {
                present: true,
                evidence_valid: true,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                source_identity: Some("test"),
            },
            fields,
            projections: projections.to_vec(),
        };
        let context_digest = repository_write_policy_context_digest(&request);
        let token = authorize_repository_write(&request).unwrap();
        (context_digest, token)
    }

    #[test]
    fn token_is_bound_to_exact_projection_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let relative = Path::new(".memzoi/proposals/pending/safe.md");
        let bytes = b"safe repository knowledge";
        let projections = [RepositoryProjection {
            path: relative,
            bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let fields = [SafetyField {
            location: "candidate.body",
            kind: SafetyFieldKind::Text,
            value: bytes,
        }];
        let mut request = RepositoryWriteRequest {
            route: RepositoryWriteRoute::FileProposalCreate,
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            scope: RepositoryScope {
                kind: ScopeKind::Repo,
                id: None,
                current_project_identity: temp.path().as_os_str().as_encoded_bytes(),
                configured_project_id: None,
            },
            visibility: Visibility::Repo,
            authorization: AuthorizationProof::ExplicitCommand { operation: "test" },
            freshness: Vec::<FreshnessCheck<'_>>::new(),
            provenance: ProvenanceAssessment {
                present: true,
                evidence_valid: true,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                source_identity: Some("test"),
            },
            fields: fields.to_vec(),
            projections: projections.to_vec(),
        };
        let context_digest = repository_write_policy_context_digest(&request);
        let token = authorize_repository_write(&request).unwrap();
        let changed = [RepositoryProjection {
            path: relative,
            bytes: b"changed repository knowledge",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &changed,
            )
            .is_err()
        );
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &projections,
            )
            .is_ok()
        );
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::ImportApply,
                &context_digest,
                &token,
                &projections,
            )
            .is_err(),
            "a capability minted for one route must fail at another route's mutation seam"
        );

        for changed_context_digest in {
            let mut digests = Vec::new();
            request.destination = MemoryDestination::Local;
            digests.push(repository_write_policy_context_digest(&request));
            request.destination = MemoryDestination::Repo;
            request.sensitivity = OkfProposalSensitivity::Unknown;
            digests.push(repository_write_policy_context_digest(&request));
            request.sensitivity = OkfProposalSensitivity::RepoSafe;
            request.scope.kind = ScopeKind::Team;
            digests.push(repository_write_policy_context_digest(&request));
            request.scope.kind = ScopeKind::Repo;
            request.visibility = Visibility::Private;
            digests.push(repository_write_policy_context_digest(&request));
            request.visibility = Visibility::Repo;
            request.provenance.content_class = RepositoryContentClass::Unknown;
            digests.push(repository_write_policy_context_digest(&request));
            digests
        } {
            assert!(
                verify_repository_batch(
                    temp.path(),
                    RepositoryWriteRoute::FileProposalCreate,
                    &changed_context_digest,
                    &token,
                    &projections,
                )
                .is_err(),
                "changing semantic policy context must invalidate the capability"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staged_install_cannot_reuse_a_capability_for_other_path_or_bytes() {
        let project = tempfile::tempdir().unwrap();
        let transactions = tempfile::tempdir().unwrap();
        let authorized_relative = Path::new(".memzoi/records/authorized.md");
        let unauthorized_relative = Path::new(".memzoi/records/unauthorized.md");
        let authorized_bytes = b"authorized repository knowledge";
        let projections = [RepositoryProjection {
            path: authorized_relative,
            bytes: authorized_bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(project.path(), &projections);
        fs::create_dir_all(project.path().join(".memzoi/records")).unwrap();
        let staged = transactions.path().join("staged.tmp");
        fs::write(&staged, b"different bytes for a different path").unwrap();

        let error = install_transaction_file_no_replace(
            project.path(),
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
            0,
            transactions.path(),
            &staged,
        )
        .expect_err("staged bytes outside the selected projection must fail closed");

        assert!(error.to_string().contains("authorized projection"));
        assert!(!project.path().join(authorized_relative).exists());
        assert!(!project.path().join(unauthorized_relative).exists());
        assert_eq!(
            fs::read(&staged).unwrap(),
            b"different bytes for a different path"
        );

        fs::write(&staged, authorized_bytes).unwrap();
        let unauthorized_projections = [RepositoryProjection {
            path: unauthorized_relative,
            bytes: authorized_bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let error = install_transaction_file_no_replace(
            project.path(),
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &unauthorized_projections,
            0,
            transactions.path(),
            &staged,
        )
        .expect_err("an authorized capability must not be reusable for another path");
        assert!(error.to_string().contains("exact route, project, paths"));
        assert!(!project.path().join(authorized_relative).exists());
        assert!(!project.path().join(unauthorized_relative).exists());
    }

    #[cfg(unix)]
    #[test]
    fn final_destination_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.md");
        fs::write(&target, "outside").unwrap();
        let relative = Path::new(".memzoi/records/safe.md");
        let bytes = b"safe repository knowledge";
        let projections = [RepositoryProjection {
            path: relative,
            bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(temp.path(), &projections);
        let destination = temp.path().join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        symlink(&target, &destination).unwrap();

        assert!(
            create_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &projections,
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "outside");
    }

    #[cfg(unix)]
    #[test]
    fn repository_creation_modes_never_request_group_or_world_write_access() {
        use std::os::unix::fs::PermissionsExt;

        use rustix::fs::Mode;

        let writable_by_others = Mode::WGRP | Mode::WOTH;
        assert!(!repository_directory_mode().intersects(writable_by_others));
        assert!(!repository_file_mode().intersects(writable_by_others));

        let temp = tempfile::tempdir().unwrap();
        let relative = Path::new(".memzoi/records/nested/safe.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"safe repository knowledge",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(temp.path(), &projections);
        create_repository_batch(
            temp.path(),
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .unwrap();

        let file_mode = fs::metadata(temp.path().join(relative))
            .unwrap()
            .permissions()
            .mode();
        let directory_mode = fs::metadata(temp.path().join(".memzoi/records/nested"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o022, 0);
        assert_eq!(directory_mode & 0o022, 0);
    }
}
