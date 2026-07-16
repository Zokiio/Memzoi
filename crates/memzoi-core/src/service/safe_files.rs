use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::MemoryPaths;

pub(super) struct RepoLifecycleLock {
    _file: fs::File,
}

impl RepoLifecycleLock {
    pub(super) fn acquire(paths: &MemoryPaths) -> Result<Self> {
        fs::create_dir_all(&paths.repository_runtime_dir).with_context(|| {
            format!(
                "failed to create runtime directory {}",
                paths.repository_runtime_dir.display()
            )
        })?;
        let lock_path = paths.repository_runtime_dir.join("repo-lifecycle.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open lifecycle lock {}", lock_path.display()))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "another repo lifecycle operation is in progress; retry after {} is unlocked",
                            lock_path.display()
                        )
                    });
                }
            }
        }
        Ok(Self { _file: file })
    }
}

pub(super) fn ensure_safe_directory(
    project_root: &Path,
    directory: &Path,
    create_missing: bool,
    label: &str,
) -> Result<()> {
    ensure_directory_chain(project_root, directory, create_missing, label)?;
    let canonical_project = fs::canonicalize(project_root).with_context(|| {
        format!(
            "failed to canonicalize project root {}",
            project_root.display()
        )
    })?;
    let canonical_directory = fs::canonicalize(directory)
        .with_context(|| format!("failed to canonicalize {label} {}", directory.display()))?;
    if !canonical_directory.starts_with(&canonical_project) {
        bail!("{label} escapes project root: {}", directory.display());
    }
    Ok(())
}

fn ensure_directory_chain(
    root: &Path,
    directory: &Path,
    create_missing: bool,
    label: &str,
) -> Result<()> {
    let relative = directory.strip_prefix(root).with_context(|| {
        format!(
            "{label} {} is not under trusted root {}",
            directory.display(),
            root.display()
        )
    })?;
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("failed to inspect trusted root {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("trusted root must be a real directory: {}", root.display());
    }

    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("{label} path contains traversal or an unsafe component");
        };
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound && create_missing => {
                fs::create_dir(&current).with_context(|| {
                    format!("failed to create {label} directory {}", current.display())
                })?;
                fs::symlink_metadata(&current)?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {label} {}", current.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "{label} ancestor must be a real directory: {}",
                current.display()
            );
        }
    }
    Ok(())
}

pub(super) fn ensure_safe_path_parent(
    project_root: &Path,
    trusted_root: &Path,
    path: &Path,
    create_missing_parent: bool,
    label: &str,
) -> Result<()> {
    ensure_safe_directory(project_root, trusted_root, false, label)?;
    let relative = path.strip_prefix(trusted_root).with_context(|| {
        format!(
            "{label} {} is not under trusted root {}",
            path.display(),
            trusted_root.display()
        )
    })?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("{label} path contains traversal or an unsafe component");
    }
    let parent = path.parent().context("safe destination has no parent")?;
    ensure_directory_chain(trusted_root, parent, create_missing_parent, label)?;
    let canonical_root = fs::canonicalize(trusted_root).with_context(|| {
        format!(
            "failed to canonicalize trusted root {}",
            trusted_root.display()
        )
    })?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize {label} parent {}", parent.display()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!(
            "{label} destination escapes trusted root: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn ensure_safe_existing_file(
    project_root: &Path,
    trusted_root: &Path,
    path: &Path,
    label: &str,
) -> Result<()> {
    ensure_safe_path_parent(project_root, trusted_root, path, false, label)?;
    ensure_regular_file(path, label)
}

pub(super) fn file_content_hash(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn ensure_path_absent(path: &Path, label: &str) -> Result<()> {
    if path
        .try_exists()
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?
    {
        bail!("{label} already exists: {}", path.display());
    }
    Ok(())
}

pub(super) fn ensure_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a regular file: {}", path.display());
    }
    Ok(())
}

pub(super) fn remove_staged_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove lifecycle transaction file"),
    }
}

pub(super) fn lifecycle_transaction_artifacts(paths: &MemoryPaths) -> Result<Vec<PathBuf>> {
    let mut artifacts = Vec::new();
    for (root, label) in [
        (paths.records_dir(), "canonical record root"),
        (paths.proposals_dir(), "proposal root"),
        (
            super::repository_mutation::repository_transaction_root(paths),
            "local repository transaction root",
        ),
    ] {
        match fs::symlink_metadata(&root) {
            Ok(_) if root.starts_with(&paths.runtime_dir) => {
                ensure_safe_directory(&paths.runtime_dir, &root, false, label)?
            }
            Ok(_) => ensure_safe_directory(&paths.project_root, &root, false, label)?,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {label} {}", root.display()));
            }
        }
        collect_lifecycle_transaction_artifacts(&root, &mut artifacts)?;
    }
    artifacts.sort();
    Ok(artifacts)
}

fn collect_lifecycle_transaction_artifacts(
    directory: &Path,
    artifacts: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_lifecycle_transaction_artifacts(&path, artifacts)?;
        } else if file_type.is_file()
            && entry.file_name().to_str().is_some_and(|name| {
                name.starts_with('.')
                    && [".write.tmp", ".canonical.tmp", ".pending.tmp"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
            })
        {
            artifacts.push(path);
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lifecycle_lock_refuses_concurrent_mutation() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            temp.path().to_path_buf(),
            temp.path().join("runtime-home"),
        );
        let _first = RepoLifecycleLock::acquire(&paths)?;
        let error = RepoLifecycleLock::acquire(&paths)
            .err()
            .context("second lifecycle lock should be refused")?;
        assert!(
            error
                .to_string()
                .contains("another repo lifecycle operation is in progress"),
            "unexpected lock contention error: {error:#}"
        );
        Ok(())
    }
}
