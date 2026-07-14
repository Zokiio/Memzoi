use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::MemoryPaths;

pub(super) struct RepoLifecycleLock {
    _file: fs::File,
}

impl RepoLifecycleLock {
    pub(super) fn acquire(paths: &MemoryPaths) -> Result<Self> {
        fs::create_dir_all(&paths.runtime_dir).with_context(|| {
            format!(
                "failed to create runtime directory {}",
                paths.runtime_dir.display()
            )
        })?;
        let lock_path = paths.runtime_dir.join("repo-lifecycle.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open lifecycle lock {}", lock_path.display()))?;
        file.try_lock().with_context(|| {
            format!(
                "another repo lifecycle operation is in progress; retry after {} is unlocked",
                lock_path.display()
            )
        })?;
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

pub(super) fn sibling_transaction_path(path: &Path, nonce: &str, role: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memzoi");
    path.with_file_name(format!(".{name}.{nonce}.{role}.tmp"))
}

pub(super) fn stage_file(final_path: &Path, contents: &str, nonce: &str) -> Result<PathBuf> {
    let parent = final_path.parent().context("staged file has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    let temp_path = sibling_transaction_path(final_path, nonce, "write");
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
    Ok(temp_path)
}

pub(super) fn install_staged_file_no_replace(staged: &Path, destination: &Path) -> Result<()> {
    fs::hard_link(staged, destination).with_context(|| {
        format!(
            "failed to install {} without replacing an existing file",
            destination.display()
        )
    })?;
    if let Err(error) = fs::remove_file(staged) {
        let install_error = anyhow::Error::new(error).context(format!(
            "failed to finalize no-replace install {}",
            destination.display()
        ));
        return match remove_staged_file(destination) {
            Ok(()) => Err(install_error),
            Err(rollback_error) => Err(install_error).context(format!(
                "additionally failed to roll back {}: {rollback_error:#}",
                destination.display()
            )),
        };
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
    ] {
        match fs::symlink_metadata(&root) {
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

    #[test]
    fn no_replace_install_preserves_concurrent_destination() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let staged = temp.path().join("staged.tmp");
        let destination = temp.path().join("record.md");
        fs::write(&staged, "new proposal bytes")?;
        fs::write(&destination, "concurrent canonical bytes")?;

        let error = install_staged_file_no_replace(&staged, &destination)
            .expect_err("no-replace install must refuse a concurrent destination");
        assert!(error.to_string().contains("without replacing"));
        assert_eq!(
            fs::read_to_string(&destination)?,
            "concurrent canonical bytes"
        );
        assert_eq!(fs::read_to_string(&staged)?, "new proposal bytes");
        Ok(())
    }
}
