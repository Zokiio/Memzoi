use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{AuthorizedRepositoryWriteBatch, RepositoryProjection, RepositoryWriteRoute};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RepositoryFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug)]
pub(crate) struct CreatedRepositoryFile {
    pub(crate) path: PathBuf,
    pub(crate) identity: RepositoryFileIdentity,
    projection_index: usize,
    #[cfg(unix)]
    directory: std::os::fd::OwnedFd,
    #[cfg(unix)]
    file_name: std::ffi::OsString,
    #[cfg(unix)]
    file: fs::File,
}

#[cfg(unix)]
fn repository_file_identity(file: &fs::File, label: &str) -> Result<RepositoryFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?;
    Ok(RepositoryFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn ensure_pinned_file_bytes(file: &fs::File, expected: &[u8], label: &str) -> Result<()> {
    use std::os::unix::fs::FileExt;

    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        bail!("{label} does not match its authorized bytes");
    }
    let mut actual = vec![0; expected.len()];
    file.read_exact_at(&mut actual, 0)
        .with_context(|| format!("failed to re-read {label}"))?;
    if actual != expected {
        bail!("{label} does not match its authorized bytes");
    }
    Ok(())
}

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

#[cfg(test)]
type BeforeRepositoryQuarantineHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
thread_local! {
    static BEFORE_REPOSITORY_QUARANTINE_HOOK: std::cell::RefCell<
        Option<BeforeRepositoryQuarantineHook>,
    > = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn inject_before_repository_quarantine_hook(hook: impl FnOnce() -> Result<()> + 'static) {
    BEFORE_REPOSITORY_QUARANTINE_HOOK.with(|injected| {
        *injected.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_repository_quarantine_hook() -> Result<()> {
    let hook = BEFORE_REPOSITORY_QUARANTINE_HOOK.with(|injected| injected.borrow_mut().take());
    if let Some(hook) = hook {
        hook()?;
    }
    Ok(())
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
fn open_absolute_directory_no_symlinks(path: &Path, label: &str) -> Result<std::os::fd::OwnedFd> {
    use rustix::fs::{CWD, Mode, openat};

    if !path.is_absolute() {
        bail!("{label} must be an absolute real path");
    }
    let mut directory = openat(CWD, Path::new("/"), directory_flags(), Mode::empty())
        .with_context(|| format!("failed to pin filesystem root for {label}"))?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                directory = openat(&directory, component, directory_flags(), Mode::empty())
                    .with_context(|| {
                        format!("failed to open {label} component without following symlinks")
                    })?;
            }
            _ => bail!("{label} must contain only absolute normal path components"),
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn project_identity_from_pinned_root(
    project_root: &Path,
    root: &std::os::fd::OwnedFd,
) -> Result<Vec<u8>> {
    use rustix::fs::fstat;

    let stat = fstat(root).context("failed to inspect pinned repository root")?;
    let path = project_root.as_os_str().as_encoded_bytes();
    let mut identity = Vec::with_capacity(path.len() + 64);
    identity.extend_from_slice(b"memzoi.repository-project-identity.v1\0");
    identity.extend_from_slice(&(path.len() as u64).to_le_bytes());
    identity.extend_from_slice(path);
    identity.extend_from_slice(&(stat.st_dev as u64).to_le_bytes());
    identity.extend_from_slice(&(stat.st_ino as u64).to_le_bytes());
    Ok(identity)
}

pub(crate) fn repository_project_identity(project_root: &Path) -> Result<Vec<u8>> {
    #[cfg(not(unix))]
    {
        let _ = project_root;
        bail!("secure repository identity is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let root = open_absolute_directory_no_symlinks(project_root, "repository root")?;
        project_identity_from_pinned_root(project_root, &root)
    }
}

#[cfg(unix)]
fn open_repository_parent(
    project_root: &Path,
    relative: &Path,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString, Vec<u8>)> {
    open_repository_parent_if_exists(project_root, relative)?
        .context("repository output parent is missing")
}

#[cfg(unix)]
fn open_repository_parent_if_exists(
    project_root: &Path,
    relative: &Path,
) -> Result<Option<(std::os::fd::OwnedFd, std::ffi::OsString, Vec<u8>)>> {
    use rustix::fs::{Mode, openat};
    use rustix::io::Errno;

    if !crate::repository_write_safety::projection_path_is_safe(relative) {
        bail!("unsafe repository-relative output path");
    }
    let mut directory = open_absolute_directory_no_symlinks(project_root, "repository root")?;
    let project_identity = project_identity_from_pinned_root(project_root, &directory)?;
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
        directory = match openat(&directory, component, directory_flags(), Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(error)
                    .context("failed to pin repository output parent without following symlinks");
            }
        };
    }
    Ok(Some((
        directory,
        file_name.context("repository destination has no file name")?,
        project_identity,
    )))
}

#[cfg(unix)]
fn open_transaction_parent(
    transaction_root: &Path,
    path: &Path,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
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
    let directory =
        open_absolute_directory_no_symlinks(transaction_root, "repository transaction root")?;
    Ok((directory, file_name.to_os_string()))
}

#[cfg(unix)]
fn read_regular_at_if_exists(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    expected_len: u64,
    label: &str,
) -> Result<Option<(fs::File, Vec<u8>)>> {
    use rustix::fs::{Mode, OFlags, openat};
    use rustix::io::Errno;

    let file = match openat(
        directory,
        file_name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open {label} without following symlinks"));
        }
    };
    let mut file = fs::File::from(file);
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?;
    if !metadata.is_file() {
        bail!("{label} must be a regular file");
    }
    if metadata.len() != expected_len {
        bail!("{label} size does not match the authorized projection");
    }
    let mut bytes = Vec::new();
    Read::take(&mut file, expected_len.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    if bytes.len() as u64 != expected_len {
        bail!("{label} changed while it was being read");
    }
    if !named_file_still_matches(directory, file_name, &file)? {
        bail!("{label} changed while it was being read");
    }
    Ok(Some((file, bytes)))
}

#[cfg(unix)]
fn read_regular_at(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    expected_len: u64,
    label: &str,
) -> Result<(fs::File, Vec<u8>)> {
    read_regular_at_if_exists(directory, file_name, expected_len, label)?
        .with_context(|| format!("{label} is missing"))
}

pub(crate) fn read_transaction_file_if_exists(
    transaction_root: &Path,
    path: &Path,
    expected_len: u64,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    #[cfg(not(unix))]
    {
        let _ = (transaction_root, path, expected_len, label);
        bail!("secure repository transaction reads are unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let (directory, file_name) = open_transaction_parent(transaction_root, path)?;
        Ok(
            read_regular_at_if_exists(&directory, &file_name, expected_len, label)?
                .map(|(_, bytes)| bytes),
        )
    }
}

pub(crate) fn read_repository_file_if_exists(
    project_root: &Path,
    relative_path: &Path,
    expected_len: u64,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    Ok(read_repository_file_with_identity_if_exists(
        project_root,
        relative_path,
        expected_len,
        label,
    )?
    .map(|(bytes, _)| bytes))
}

pub(crate) fn read_repository_file_with_identity_if_exists(
    project_root: &Path,
    relative_path: &Path,
    expected_len: u64,
    label: &str,
) -> Result<Option<(Vec<u8>, RepositoryFileIdentity)>> {
    #[cfg(not(unix))]
    {
        let _ = (project_root, relative_path, expected_len, label);
        bail!("secure repository reads are unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let Some((directory, file_name, _)) =
            open_repository_parent_if_exists(project_root, relative_path)?
        else {
            return Ok(None);
        };
        read_regular_at_if_exists(&directory, &file_name, expected_len, label)?
            .map(|(file, bytes)| Ok((bytes, repository_file_identity(&file, label)?)))
            .transpose()
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn create_authorized_repository_projection(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_purpose: crate::RepositoryProjectionPurpose,
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
    if projection.purpose != expected_purpose {
        bail!("repository projection purpose does not match the authorized mutation");
    }
    let (directory, file_name, project_identity) =
        open_repository_parent(project_root, projection.path)?;
    verify_repository_batch_for_identity(
        &project_identity,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    let file = openat(
        &directory,
        &file_name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
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
fn remove_pinned_named_file(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    opened: &fs::File,
    expected_bytes: Option<&[u8]>,
    label: &str,
) -> Result<()> {
    use rustix::fs::{AtFlags, RenameFlags, fsync, renameat_with, unlinkat};

    if !named_file_still_matches(directory, file_name, opened)
        .with_context(|| format!("failed to identify {label}"))?
    {
        bail!("{label} name no longer identifies the created file");
    }
    #[cfg(test)]
    run_before_repository_quarantine_hook()
        .with_context(|| format!("injected change before quarantining {label}"))?;

    let quarantine_name = std::ffi::OsString::from(format!(
        ".memzoi-removal-quarantine-{}.tmp",
        uuid::Uuid::now_v7()
    ));
    renameat_with(
        directory,
        file_name,
        directory,
        &quarantine_name,
        RenameFlags::NOREPLACE,
    )
    .with_context(|| format!("failed to atomically quarantine {label}"))?;

    let quarantine_validation_error =
        match named_file_still_matches(directory, &quarantine_name, opened) {
            Ok(true) => expected_bytes.and_then(|expected| {
                ensure_pinned_file_bytes(opened, expected, label)
                    .context(format!("{label} changed before atomic quarantine removal"))
                    .err()
            }),
            Ok(false) => Some(anyhow::anyhow!(
                "{label} changed before atomic quarantine removal"
            )),
            Err(error) => Some(error.context(format!("failed to inspect quarantined {label}"))),
        };
    if let Some(mismatch_error) = quarantine_validation_error {
        let restore_result = renameat_with(
            directory,
            &quarantine_name,
            directory,
            file_name,
            RenameFlags::NOREPLACE,
        );
        let sync_result = fsync(directory);
        let mut recovery = match restore_result {
            Ok(()) => "unexpected entry was restored without replacement".to_owned(),
            Err(error) => format!(
                "unexpected entry was retained under quarantine because its original name could not be restored: {error}"
            ),
        };
        if let Err(error) = sync_result {
            recovery.push_str(&format!("; failed to sync quarantine recovery: {error}"));
        }
        return Err(mismatch_error).context(recovery);
    }

    let unlink_result = unlinkat(directory, &quarantine_name, AtFlags::empty())
        .with_context(|| format!("failed to unlink quarantined {label}"));
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

#[cfg(unix)]
fn cleanup_created_file(
    directory: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    opened: &fs::File,
    label: &str,
) -> Result<()> {
    remove_pinned_named_file(directory, file_name, opened, None, label)
}

#[cfg(unix)]
fn rollback_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    created: &[CreatedRepositoryFile],
    primary_error: anyhow::Error,
) -> anyhow::Error {
    let mut cleanup_errors = Vec::new();
    for created_file in created.iter().rev() {
        if let Err(error) = remove_created_repository_file(
            project_root,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
            created_file,
        ) {
            cleanup_errors.push(format!("{error:#}"));
        }
    }
    if cleanup_errors.is_empty() {
        primary_error
    } else {
        primary_error.context(format!(
            "additionally failed to roll back repository batch: {}",
            cleanup_errors.join("; ")
        ))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_transaction_file_no_replace(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_purpose: crate::RepositoryProjectionPurpose,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    projection_index: usize,
    transaction_root: &Path,
    staged_path: &Path,
) -> Result<CreatedRepositoryFile> {
    #[cfg(not(unix))]
    {
        let _ = (
            project_root,
            expected_route,
            expected_purpose,
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
        if projection.purpose != expected_purpose {
            bail!("repository projection purpose does not match the authorized mutation");
        }
        let (source_directory, source_name) =
            open_transaction_parent(transaction_root, staged_path)?;
        let (source, bytes) = read_regular_at(
            &source_directory,
            &source_name,
            projection.bytes.len() as u64,
            "staged repository file",
        )?;
        if bytes != projection.bytes {
            bail!("staged repository bytes do not match the authorized projection");
        }
        let (destination_directory, destination_name, destination) =
            create_authorized_repository_projection(
                project_root,
                expected_route,
                expected_purpose,
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
        let identity = repository_file_identity(&destination, "created repository file")?;
        Ok(CreatedRepositoryFile {
            path: project_root.join(projection.path),
            identity,
            projection_index,
            directory: destination_directory,
            file_name: destination_name,
            file: destination,
        })
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
    expected_identity: Option<RepositoryFileIdentity>,
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
            expected_identity,
            transaction_root,
            backup_path,
        );
        bail!("secure repository backup is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        use rustix::fs::{OFlags, fsync, openat};

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
        let (source_directory, source_name, project_identity) =
            open_repository_parent(project_root, projection.path)?;
        verify_repository_batch_for_identity(
            &project_identity,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
        )?;
        let (source, bytes) = read_regular_at(
            &source_directory,
            &source_name,
            projection.bytes.len() as u64,
            "repository source",
        )
        .context("repository source changed after validation")?;
        if let Some(expected_identity) = expected_identity {
            let actual_identity = repository_file_identity(&source, "repository source")?;
            if actual_identity != expected_identity {
                bail!("repository source identity does not match the installed file ownership");
            }
        }
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
        if let Err(error) = fsync(&backup_directory) {
            let sync_error = anyhow::Error::new(error)
                .context("failed to sync repository transaction backup parent");
            return match cleanup_created_file(
                &backup_directory,
                &backup_name,
                &backup,
                "repository transaction backup after directory-sync failure",
            ) {
                Ok(()) => Err(sync_error),
                Err(cleanup_error) => Err(sync_error).context(format!(
                    "additionally failed to clean repository transaction backup: {cleanup_error:#}"
                )),
            };
        }
        remove_pinned_named_file(
            &source_directory,
            &source_name,
            &source,
            Some(projection.bytes),
            "securely backed-up repository source",
        )
        .context("failed to remove securely backed-up repository source")?;
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
    let project_identity = repository_project_identity(project_root)?;
    verify_repository_batch_for_identity(
        &project_identity,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    for projection in projections {
        verify_projection_path(project_root, projection.path)?;
    }
    Ok(())
}

fn verify_repository_batch_for_identity(
    project_identity: &[u8],
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<()> {
    if !authorization.authorizes(
        expected_route,
        project_identity,
        expected_policy_context_digest,
        projections,
    ) {
        bail!(
            "repository write authorization does not match the exact route, project, paths, revisions, and bytes"
        );
    }
    for projection in projections {
        if !crate::repository_write_safety::projection_path_is_safe(projection.path) {
            bail!("unsafe repository-relative output path");
        }
    }
    Ok(())
}

pub(crate) fn remove_created_repository_file(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
    created: &CreatedRepositoryFile,
) -> Result<()> {
    verify_repository_batch(
        project_root,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    let projection = projections
        .get(created.projection_index)
        .context("created repository file projection index is out of bounds")?;
    if projection.purpose != crate::RepositoryProjectionPurpose::Write
        || created.path != project_root.join(projection.path)
    {
        bail!("created repository file does not match its authorized projection");
    }

    #[cfg(not(unix))]
    {
        bail!("secure created repository cleanup is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        use rustix::fs::fstat;

        let (current_directory, current_name, project_identity) =
            open_repository_parent(project_root, projection.path)?;
        verify_repository_batch_for_identity(
            &project_identity,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
        )?;
        let created_parent = fstat(&created.directory)
            .context("failed to inspect created repository file parent")?;
        let current_parent = fstat(&current_directory)
            .context("failed to inspect current repository file parent")?;
        if current_name != created.file_name
            || created_parent.st_dev != current_parent.st_dev
            || created_parent.st_ino != current_parent.st_ino
        {
            bail!("created repository file is no longer at its authorized path");
        }
        ensure_pinned_file_bytes(&created.file, projection.bytes, "created repository file")?;
        remove_pinned_named_file(
            &created.directory,
            &created.file_name,
            &created.file,
            Some(projection.bytes),
            "created repository file",
        )
    }
}

pub(crate) fn create_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<Vec<CreatedRepositoryFile>> {
    for projection in projections {
        if projection.purpose != crate::RepositoryProjectionPurpose::Write {
            bail!("create-only repository batch requires authorized write projections");
        }
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
            fs::{Mode, OFlags, fsync, mkdirat, openat},
            io::Errno,
        };

        let root = open_absolute_directory_no_symlinks(project_root, "repository root")?;
        let project_identity = project_identity_from_pinned_root(project_root, &root)?;
        verify_repository_batch_for_identity(
            &project_identity,
            expected_route,
            expected_policy_context_digest,
            authorization,
            projections,
        )?;
        let directory_mode = repository_directory_mode();
        let file_mode = repository_file_mode();
        let mut created = Vec::<CreatedRepositoryFile>::with_capacity(projections.len());

        for (projection_index, projection) in projections.iter().enumerate() {
            let destination = project_root.join(projection.path);
            let result = (|| -> Result<(OwnedFd, OsString, fs::File)> {
                let mut directory = openat(&root, Path::new("."), directory_flags(), Mode::empty())
                    .context("failed to duplicate pinned repository root")?;
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
                        directory_flags(),
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
                            openat(&directory, component, directory_flags(), Mode::empty()).context(
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
                    OFlags::RDWR
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
                    let persistence_error = anyhow::Error::new(error).context(format!(
                        "failed to write authorized repository file {}",
                        destination.display()
                    ));
                    return match cleanup_created_file(
                        &directory,
                        &file_name,
                        &file,
                        "incomplete repository batch destination",
                    ) {
                        Ok(()) => Err(persistence_error),
                        Err(cleanup_error) => Err(persistence_error).context(format!(
                            "additionally failed to clean incomplete repository batch destination: {cleanup_error:#}"
                        )),
                    };
                }
                if let Err(error) = fsync(&directory) {
                    let sync_error = anyhow::Error::new(error)
                        .context("failed to sync authorized repository directory");
                    return match cleanup_created_file(
                        &directory,
                        &file_name,
                        &file,
                        "repository batch destination after directory-sync failure",
                    ) {
                        Ok(()) => Err(sync_error),
                        Err(cleanup_error) => Err(sync_error).context(format!(
                            "additionally failed to clean repository batch destination: {cleanup_error:#}"
                        )),
                    };
                }
                Ok((directory, file_name, file))
            })();
            let created_file = match result {
                Ok(created_file) => created_file,
                Err(error) => {
                    return Err(rollback_repository_batch(
                        project_root,
                        expected_route,
                        expected_policy_context_digest,
                        authorization,
                        projections,
                        &created,
                        error,
                    ));
                }
            };
            let (directory, file_name, file) = created_file;
            let identity = repository_file_identity(&file, "created repository batch file")?;
            created.push(CreatedRepositoryFile {
                path: destination,
                identity,
                projection_index,
                directory,
                file_name,
                file,
            });
        }
        Ok(created)
    }
}

pub(crate) fn verify_projection_path(project_root: &Path, relative: &Path) -> Result<PathBuf> {
    if !crate::repository_write_safety::projection_path_is_safe(relative) {
        bail!("unsafe repository-relative output path");
    }
    #[cfg(not(unix))]
    {
        let _ = project_root;
        bail!("secure repository path verification is unavailable on this platform");
    }
    #[cfg(unix)]
    let _root = open_absolute_directory_no_symlinks(project_root, "repository root")?;
    let mut cursor = project_root.to_path_buf();
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
        let project_identity = repository_project_identity(project_root).unwrap();
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
                current_project_identity: &project_identity,
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
        let project_root = temp.path().canonicalize().unwrap();
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
        let project_identity = repository_project_identity(&project_root).unwrap();
        let mut request = RepositoryWriteRequest {
            route: RepositoryWriteRoute::FileProposalCreate,
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            scope: RepositoryScope {
                kind: ScopeKind::Repo,
                id: None,
                current_project_identity: &project_identity,
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
                &project_root,
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &changed,
            )
            .is_err()
        );
        assert!(
            verify_repository_batch(
                &project_root,
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &projections,
            )
            .is_ok()
        );
        assert!(
            verify_repository_batch(
                &project_root,
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
                    &project_root,
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
        let project_root = project.path().canonicalize().unwrap();
        let transaction_root = transactions.path().canonicalize().unwrap();
        let authorized_relative = Path::new(".memzoi/records/authorized.md");
        let unauthorized_relative = Path::new(".memzoi/records/unauthorized.md");
        let authorized_bytes = b"authorized repository knowledge";
        let projections = [RepositoryProjection {
            path: authorized_relative,
            bytes: authorized_bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);
        fs::create_dir_all(project_root.join(".memzoi/records")).unwrap();
        let staged = transaction_root.join("staged.tmp");
        fs::write(&staged, b"different bytes for a different path").unwrap();

        let error = install_transaction_file_no_replace(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            crate::RepositoryProjectionPurpose::Write,
            &context_digest,
            &token,
            &projections,
            0,
            &transaction_root,
            &staged,
        )
        .expect_err("staged bytes outside the selected projection must fail closed");

        assert!(error.to_string().contains("authorized projection"));
        assert!(!project_root.join(authorized_relative).exists());
        assert!(!project_root.join(unauthorized_relative).exists());
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
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            crate::RepositoryProjectionPurpose::Write,
            &context_digest,
            &token,
            &unauthorized_projections,
            0,
            &transaction_root,
            &staged,
        )
        .expect_err("an authorized capability must not be reusable for another path");
        assert!(error.to_string().contains("exact route, project, paths"));
        assert!(!project_root.join(authorized_relative).exists());
        assert!(!project_root.join(unauthorized_relative).exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_only_batch_rejects_existing_state_projections() {
        let project = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let relative = Path::new(".memzoi/records/not-a-write.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"existing repository state",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Existing,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);

        let error = create_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .expect_err("create-only writes must not install an unscanned existing-state projection");

        assert!(
            error
                .to_string()
                .contains("requires authorized write projections")
        );
        assert!(!project_root.join(relative).exists());
    }

    #[cfg(unix)]
    #[test]
    fn pinned_reader_rejects_fifo_without_blocking() {
        use std::{sync::mpsc, time::Duration};

        let transactions = tempfile::tempdir().unwrap();
        let transaction_root = transactions.path().canonicalize().unwrap();
        let fifo = transaction_root.join("staged.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo must create the test fixture");
        let (directory, file_name) = open_transaction_parent(&transaction_root, &fifo).unwrap();
        let (sender, receiver) = mpsc::channel();

        std::thread::spawn(move || {
            let result = read_regular_at(&directory, &file_name, 0, "staged repository FIFO");
            let _ = sender.send(result);
        });

        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("opening a FIFO must return without waiting for a writer");
        let error = result.expect_err("a FIFO must not pass regular-file validation");
        assert!(error.to_string().contains("must be a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_reader_rejects_oversized_source_before_reading_it() {
        let transactions = tempfile::tempdir().unwrap();
        let transaction_root = transactions.path().canonicalize().unwrap();
        let staged = transaction_root.join("oversized.tmp");
        let file = fs::File::create(&staged).unwrap();
        file.set_len(1 << 40).unwrap();

        let error = read_transaction_file_if_exists(
            &transaction_root,
            &staged,
            1,
            "oversized staged repository file",
        )
        .expect_err("a source larger than the authorized projection must fail closed");

        assert!(error.to_string().contains("size does not match"));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_optional_readers_distinguish_missing_files_from_invalid_files() {
        use std::os::unix::fs::symlink;

        let project = tempfile::tempdir().unwrap();
        let transactions = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let transaction_root = transactions.path().canonicalize().unwrap();
        let repository_relative = Path::new(".memzoi/records/source.md");
        fs::create_dir_all(project_root.join(".memzoi/records")).unwrap();
        let missing_transaction = transaction_root.join("missing.tmp");

        assert_eq!(
            read_transaction_file_if_exists(
                &transaction_root,
                &missing_transaction,
                0,
                "missing staged repository file",
            )
            .unwrap(),
            None
        );
        assert_eq!(
            read_repository_file_if_exists(
                &project_root,
                repository_relative,
                0,
                "missing repository file",
            )
            .unwrap(),
            None
        );
        assert_eq!(
            read_repository_file_if_exists(
                &project_root,
                Path::new(".memzoi/missing-parent/source.md"),
                0,
                "repository file below a missing parent",
            )
            .unwrap(),
            None
        );

        let outside = project_root.join("outside.md");
        fs::write(&outside, b"safe").unwrap();
        symlink(&outside, project_root.join(repository_relative)).unwrap();
        let error = read_repository_file_if_exists(
            &project_root,
            repository_relative,
            4,
            "symlinked repository file",
        )
        .expect_err("a symlink must not be reported as an absent repository file");
        assert!(error.to_string().contains("without following symlinks"));
    }

    #[cfg(unix)]
    #[test]
    fn final_destination_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().canonicalize().unwrap();
        let target = project_root.join("outside.md");
        fs::write(&target, "outside").unwrap();
        let relative = Path::new(".memzoi/records/safe.md");
        let bytes = b"safe repository knowledge";
        let projections = [RepositoryProjection {
            path: relative,
            bytes,
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);
        let destination = project_root.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        symlink(&target, &destination).unwrap();

        assert!(
            create_repository_batch(
                &project_root,
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
    fn authorization_cannot_follow_a_replaced_project_root_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().canonicalize().unwrap();
        let project_root = container.join("project");
        let moved_project = container.join("moved-project");
        let outside = container.join("outside");
        fs::create_dir(&project_root).unwrap();
        fs::create_dir(&outside).unwrap();
        let relative = Path::new(".memzoi/records/safe.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"safe repository knowledge",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);

        fs::rename(&project_root, &moved_project).unwrap();
        symlink(&outside, &project_root).unwrap();

        let error = create_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .expect_err("a project-root symlink introduced after authorization must fail closed");

        assert!(format!("{error:#}").contains("without following symlinks"));
        assert!(!outside.join(relative).exists());
        assert!(!moved_project.join(relative).exists());
    }

    #[cfg(unix)]
    #[test]
    fn authorization_is_bound_to_the_project_root_inode() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().canonicalize().unwrap();
        let project_root = container.join("project");
        let original_project = container.join("original-project");
        fs::create_dir(&project_root).unwrap();
        let relative = Path::new(".memzoi/records/safe.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"safe repository knowledge",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);

        fs::rename(&project_root, &original_project).unwrap();
        fs::create_dir(&project_root).unwrap();

        let error = create_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .expect_err("replacing the project root inode must invalidate its authorization");

        assert!(format!("{error:#}").contains("does not match the exact route, project"));
        assert!(!project_root.join(relative).exists());
        assert!(!original_project.join(relative).exists());
    }

    #[cfg(unix)]
    #[test]
    fn batch_rollback_preserves_a_concurrent_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().canonicalize().unwrap();
        let destination = project_root.join("created.md");
        fs::write(&destination, b"created by batch").unwrap();
        let created_file = fs::File::open(&destination).unwrap();
        let directory =
            open_absolute_directory_no_symlinks(&project_root, "repository root").unwrap();
        let relative = Path::new("created.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"created by batch",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);

        fs::remove_file(&destination).unwrap();
        fs::write(&destination, b"concurrent replacement").unwrap();
        let created = [CreatedRepositoryFile {
            path: destination.clone(),
            identity: repository_file_identity(&created_file, "created repository test file")
                .unwrap(),
            projection_index: 0,
            directory,
            file_name: std::ffi::OsString::from("created.md"),
            file: created_file,
        }];

        let error = rollback_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
            &created,
            anyhow::anyhow!("later batch failure"),
        );

        assert_eq!(fs::read(&destination).unwrap(), b"concurrent replacement");
        assert!(format!("{error:#}").contains("name no longer identifies the created file"));
    }

    #[cfg(unix)]
    #[test]
    fn batch_rollback_removes_the_exact_created_file() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().canonicalize().unwrap();
        let first = Path::new(".memzoi/records/first.md");
        let existing = Path::new(".memzoi/records/existing.md");
        fs::create_dir_all(project_root.join(".memzoi/records")).unwrap();
        fs::write(project_root.join(existing), b"preexisting").unwrap();
        let projections = [
            RepositoryProjection {
                path: first,
                bytes: b"created by batch",
                target_revision: None,
                purpose: crate::RepositoryProjectionPurpose::Write,
            },
            RepositoryProjection {
                path: existing,
                bytes: b"must not replace",
                target_revision: None,
                purpose: crate::RepositoryProjectionPurpose::Write,
            },
        ];
        let (context_digest, token) = authorize_create(&project_root, &projections);

        let error = create_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .expect_err("a later create collision must roll back earlier batch files");

        assert!(format!("{error:#}").contains("already exists"));
        assert!(!project_root.join(first).exists());
        assert_eq!(
            fs::read(project_root.join(existing)).unwrap(),
            b"preexisting"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_quarantine_restores_a_replacement_after_initial_validation() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().canonicalize().unwrap();
        let destination = project_root.join("created.md");
        fs::write(&destination, b"created by batch").unwrap();
        let created_file = fs::File::open(&destination).unwrap();
        let directory =
            open_absolute_directory_no_symlinks(&project_root, "repository root").unwrap();
        let replacement_path = destination.clone();
        inject_before_repository_quarantine_hook(move || {
            fs::remove_file(&replacement_path)?;
            fs::write(&replacement_path, b"concurrent replacement")?;
            Ok(())
        });

        let error = remove_pinned_named_file(
            &directory,
            std::ffi::OsStr::new("created.md"),
            &created_file,
            None,
            "repository rollback file",
        )
        .expect_err("a replacement introduced after validation must not be deleted");

        assert_eq!(fs::read(&destination).unwrap(), b"concurrent replacement");
        assert!(format!("{error:#}").contains("changed before atomic quarantine removal"));
        assert_eq!(fs::read_dir(&project_root).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_quarantine_restores_in_place_bytes_changed_after_validation() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().canonicalize().unwrap();
        let destination = project_root.join("created.md");
        let expected = b"created by batch";
        fs::write(&destination, expected).unwrap();
        let created_file = fs::File::open(&destination).unwrap();
        let directory =
            open_absolute_directory_no_symlinks(&project_root, "repository root").unwrap();
        let changed_path = destination.clone();
        inject_before_repository_quarantine_hook(move || {
            fs::write(&changed_path, b"human edit")?;
            Ok(())
        });

        let error = remove_pinned_named_file(
            &directory,
            std::ffi::OsStr::new("created.md"),
            &created_file,
            Some(expected),
            "repository rollback file",
        )
        .expect_err("in-place bytes changed after validation must not be deleted");

        assert_eq!(fs::read(&destination).unwrap(), b"human edit");
        assert!(format!("{error:#}").contains("changed before atomic quarantine removal"));
        assert_eq!(fs::read_dir(&project_root).unwrap().count(), 1);
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
        let project_root = temp.path().canonicalize().unwrap();
        let relative = Path::new(".memzoi/records/nested/safe.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"safe repository knowledge",
            target_revision: None,
            purpose: crate::RepositoryProjectionPurpose::Write,
        }];
        let (context_digest, token) = authorize_create(&project_root, &projections);
        create_repository_batch(
            &project_root,
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .unwrap();

        let file_mode = fs::metadata(project_root.join(relative))
            .unwrap()
            .permissions()
            .mode();
        let directory_mode = fs::metadata(project_root.join(".memzoi/records/nested"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o022, 0);
        assert_eq!(directory_mode & 0o022, 0);
    }
}
