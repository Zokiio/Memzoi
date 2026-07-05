use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

const MEMORY_DIR_NAME: &str = ".memzoi";
const RUNTIME_PROJECTS_DIR: &str = "projects";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPaths {
    pub project_root: PathBuf,
    pub memory_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub db_path: PathBuf,
    pub config_path: PathBuf,
    pub exports_dir: PathBuf,
}

impl MemoryPaths {
    pub fn new(project_root: PathBuf) -> Self {
        Self::with_runtime_home(project_root, runtime_home())
    }

    pub fn with_runtime_home(project_root: PathBuf, runtime_home: PathBuf) -> Self {
        let memory_dir = project_root.join(MEMORY_DIR_NAME);
        let runtime_dir = runtime_home
            .join(RUNTIME_PROJECTS_DIR)
            .join(project_runtime_key(&project_root));
        Self {
            project_root,
            db_path: runtime_dir.join("memory.db"),
            config_path: runtime_dir.join("config.toml"),
            exports_dir: runtime_dir.join("exports"),
            runtime_dir,
            memory_dir,
        }
    }

    pub fn records_dir(&self) -> PathBuf {
        self.memory_dir.join("records")
    }

    pub fn proposals_dir(&self) -> PathBuf {
        self.memory_dir.join("proposals")
    }
}

pub fn discover_paths(start: impl AsRef<Path>) -> Result<MemoryPaths> {
    let start = normalize_start(start.as_ref())?;

    if let Some(root) = find_configured_root(&start) {
        return Ok(MemoryPaths::new(root));
    }

    if let Some(root) = find_ancestor_with(&start, |candidate| candidate.join(".git").exists()) {
        return Ok(MemoryPaths::new(root));
    }

    Ok(MemoryPaths::new(start))
}

pub(crate) fn discover_existing_paths(start: impl AsRef<Path>) -> Result<MemoryPaths> {
    let start = normalize_start(start.as_ref())?;

    if let Some(root) = find_configured_root(&start) {
        return Ok(MemoryPaths::new(root));
    }

    bail!(
        "Memzoi bundle is not initialized for {}; run `memzoi init` first",
        start.display()
    )
}

fn find_configured_root(start: &Path) -> Option<PathBuf> {
    find_ancestor_with(start, |candidate| {
        let memory_dir = candidate.join(MEMORY_DIR_NAME);
        memory_dir.join("records").is_dir() || memory_dir.join("config.toml").is_file()
    })
}

fn runtime_home() -> PathBuf {
    if let Some(path) = env::var_os("MEMZOI_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = env::var_os("HOME") {
        return PathBuf::from(path).join(MEMORY_DIR_NAME);
    }
    PathBuf::from(MEMORY_DIR_NAME)
}

fn project_runtime_key(project_root: &Path) -> String {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
    let prefix = root
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_runtime_segment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "project".to_owned());
    format!("{prefix}-{}", &hash[..16])
}

fn sanitize_runtime_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

fn normalize_start(start: &Path) -> Result<PathBuf> {
    let absolute = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to read current directory")?
            .join(start)
    };

    let existing = if absolute.is_file() {
        absolute
            .parent()
            .context("start path file had no parent")?
            .to_path_buf()
    } else {
        absolute
    };

    existing
        .canonicalize()
        .with_context(|| format!("failed to resolve start path {}", existing.display()))
}

fn find_ancestor_with(start: &Path, predicate: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(candidate) = current {
        if predicate(candidate) {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_dir(path: impl AsRef<Path>) {
        std::fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn existing_config_takes_precedence_over_git_root() {
        let temp = TempDir::new().unwrap();
        let git_root = temp.path().join("repo");
        create_dir(git_root.join(".git"));

        let configured_root = git_root.join("packages").join("app");
        create_dir(configured_root.join(".memzoi").join("records"));

        let start = configured_root.join("src");
        create_dir(&start);

        let paths = discover_paths(&start).unwrap();

        let expected_root = configured_root.canonicalize().unwrap();
        assert_eq!(paths.project_root, expected_root);
        assert_eq!(
            paths.records_dir(),
            configured_root
                .canonicalize()
                .unwrap()
                .join(".memzoi")
                .join("records")
        );
    }

    #[test]
    fn git_root_is_used_when_no_config_exists() {
        let temp = TempDir::new().unwrap();
        let git_root = temp.path().join("repo");
        let start = git_root.join("crates").join("agent");
        create_dir(git_root.join(".git"));
        create_dir(&start);

        let paths = discover_paths(&start).unwrap();

        assert_eq!(paths.project_root, git_root.canonicalize().unwrap());
        assert_eq!(
            paths.memory_dir,
            git_root.canonicalize().unwrap().join(".memzoi")
        );
        assert!(paths.db_path.starts_with(paths.runtime_dir));
    }

    #[test]
    fn start_directory_is_used_without_config_or_git_root() {
        let temp = TempDir::new().unwrap();
        let start = temp.path().join("scratch");
        create_dir(&start);

        let paths = discover_paths(&start).unwrap();

        assert_eq!(paths.project_root, start.canonicalize().unwrap());
        assert_eq!(
            paths.records_dir(),
            start
                .canonicalize()
                .unwrap()
                .join(".memzoi")
                .join("records")
        );
    }

    #[test]
    fn existing_discovery_requires_memzoi_config() {
        let temp = TempDir::new().unwrap();
        let git_root = temp.path().join("repo");
        let start = git_root.join("crates").join("agent");
        create_dir(git_root.join(".git"));
        create_dir(&start);

        let error = discover_existing_paths(&start).unwrap_err();

        assert!(
            error.to_string().contains("memzoi init"),
            "error should tell users how to initialize a bundle: {error}"
        );
    }
}
