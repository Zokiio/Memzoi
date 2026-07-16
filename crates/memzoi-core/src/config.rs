use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::git_repository::{
    GitRepositoryIdentity, discover_git_repository, list_git_worktree_roots,
};

const MEMORY_DIR_NAME: &str = ".memzoi";
const RUNTIME_PROJECTS_DIR: &str = "projects";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalApprovalPolicy {
    Auto,
    Manual,
}

impl ProposalApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

impl FromStr for ProposalApprovalPolicy {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "manual" => Ok(Self::Manual),
            other => {
                bail!("invalid proposal approval policy {other:?}; expected \"auto\" or \"manual\"")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowConfig {
    pub proposal_approval: ProposalApprovalPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveConfig {
    pub workflow: WorkflowConfig,
    pub sources: ConfigSources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSources {
    pub user_config_path: PathBuf,
    pub repo_config_path: PathBuf,
    pub proposal_approval_source: ConfigSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    BuiltInDefault,
    UserGlobal(PathBuf),
    Repo(PathBuf),
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    workflow: Option<FileWorkflowConfig>,
}

#[derive(Debug, Deserialize)]
struct FileWorkflowConfig {
    proposal_approval: Option<toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPaths {
    pub project_root: PathBuf,
    pub memory_dir: PathBuf,
    pub repository_runtime_dir: PathBuf,
    pub worktree_runtime_dir: PathBuf,
    pub shared_db_path: PathBuf,
    pub index_db_path: PathBuf,
    pub legacy_runtime_dirs: Vec<PathBuf>,
    pub runtime_dir: PathBuf,
    pub db_path: PathBuf,
    pub config_path: PathBuf,
    pub exports_dir: PathBuf,
    pub(crate) runtime_identity_error: Option<String>,
}

impl MemoryPaths {
    pub fn new(project_root: PathBuf) -> Self {
        Self::with_runtime_home(project_root, runtime_home())
    }

    pub fn with_runtime_home(project_root: PathBuf, runtime_home: PathBuf) -> Self {
        let project_root = normalize_root_path(project_root);
        let runtime_home = normalize_root_path(runtime_home);
        let identity = RuntimeIdentity::discover(&project_root);
        Self::with_runtime_identity(project_root, runtime_home, identity)
    }

    fn with_runtime_identity(
        project_root: PathBuf,
        runtime_home: PathBuf,
        identity: RuntimeIdentity,
    ) -> Self {
        let memory_dir = project_root.join(MEMORY_DIR_NAME);
        let repository_runtime_dir = runtime_home
            .join(RUNTIME_PROJECTS_DIR)
            .join(&identity.repository_key);
        let worktree_runtime_dir = repository_runtime_dir
            .join("worktrees")
            .join(&identity.worktree_key);
        let shared_db_path = repository_runtime_dir.join("shared.db");
        let index_db_path = worktree_runtime_dir.join("index.db");
        let mut legacy_runtime_dirs = identity
            .legacy_project_roots
            .iter()
            .flat_map(|root| {
                [project_runtime_key(root), non_git_repository_key(root)]
                    .into_iter()
                    .map(|key| runtime_home.join(RUNTIME_PROJECTS_DIR).join(key))
            })
            .filter(|path| path != &repository_runtime_dir)
            .collect::<Vec<_>>();
        legacy_runtime_dirs.sort();
        legacy_runtime_dirs.dedup();
        Self {
            project_root,
            db_path: index_db_path.clone(),
            config_path: repository_runtime_dir.join("config.toml"),
            exports_dir: worktree_runtime_dir.join("exports"),
            runtime_dir: worktree_runtime_dir.clone(),
            repository_runtime_dir,
            worktree_runtime_dir,
            shared_db_path,
            index_db_path,
            legacy_runtime_dirs,
            memory_dir,
            runtime_identity_error: identity.discovery_error,
        }
    }

    pub(crate) fn validate_runtime_identity(&self) -> Result<()> {
        if let Some(error) = &self.runtime_identity_error {
            bail!("{error}");
        }
        Ok(())
    }

    pub fn records_dir(&self) -> PathBuf {
        self.memory_dir.join("records")
    }

    pub fn proposals_dir(&self) -> PathBuf {
        self.memory_dir.join("proposals")
    }

    pub fn runtime_project_config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn repo_config_path(&self) -> PathBuf {
        self.memory_dir.join("config.toml")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeIdentity {
    repository_key: String,
    worktree_key: String,
    legacy_project_roots: Vec<PathBuf>,
    discovery_error: Option<String>,
}

impl RuntimeIdentity {
    fn discover(project_root: &Path) -> Self {
        Self::discover_with(
            project_root,
            discover_git_repository,
            list_git_worktree_roots,
        )
    }

    fn discover_with(
        project_root: &Path,
        discover_git: impl FnOnce(&Path) -> Result<Option<GitRepositoryIdentity>>,
        list_worktrees: impl FnOnce(&Path) -> Result<Vec<PathBuf>>,
    ) -> Self {
        let project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let git = match discover_git(&project_root) {
            Ok(Some(git)) => git,
            Ok(None) => {
                return match find_git_marker(&project_root) {
                    Ok(Some(marker)) => Self::unresolved_git(
                        &project_root,
                        format!(
                            "Git repository identity is unavailable for {} despite Git metadata at {}; refusing non-Git runtime fallback",
                            project_root.display(),
                            marker.display()
                        ),
                    ),
                    Ok(None) => Self::non_git(&project_root),
                    Err(error) => Self::unresolved_git(
                        &project_root,
                        format!(
                            "Git repository identity could not be verified for {}: {error:#}",
                            project_root.display()
                        ),
                    ),
                };
            }
            Err(error) => {
                return Self::unresolved_git(
                    &project_root,
                    format!(
                        "Git repository identity discovery failed for {}: {error:#}",
                        project_root.display()
                    ),
                );
            }
        };
        let relative_project = project_root
            .strip_prefix(&git.worktree_root)
            .unwrap_or(Path::new("."));
        let repository_name = repository_display_name(&git.common_dir, relative_project);
        let repository_key = runtime_key(
            &repository_name,
            &[
                git.common_dir.as_os_str().as_encoded_bytes(),
                relative_project.as_os_str().as_encoded_bytes(),
            ],
        );
        let worktree_key = runtime_key(
            "worktree",
            &[
                git.git_dir.as_os_str().as_encoded_bytes(),
                relative_project.as_os_str().as_encoded_bytes(),
            ],
        );
        let roots = match list_worktrees(&git.worktree_root) {
            Ok(roots) if !roots.is_empty() => roots,
            Ok(_) => {
                return Self {
                    repository_key,
                    worktree_key,
                    legacy_project_roots: vec![project_root.clone()],
                    discovery_error: Some(format!(
                        "Git repository identity discovery failed for {}: worktree enumeration returned no worktrees",
                        project_root.display()
                    )),
                };
            }
            Err(error) => {
                return Self {
                    repository_key,
                    worktree_key,
                    legacy_project_roots: vec![project_root.clone()],
                    discovery_error: Some(format!(
                        "Git repository identity discovery failed for {}: linked worktree enumeration failed: {error:#}",
                        project_root.display()
                    )),
                };
            }
        };
        let mut legacy_project_roots = roots
            .into_iter()
            .map(|root| root.join(relative_project))
            .map(normalize_root_path)
            .collect::<Vec<_>>();
        if !legacy_project_roots.contains(&project_root) {
            legacy_project_roots.push(project_root);
        }
        legacy_project_roots.sort();
        legacy_project_roots.dedup();
        Self {
            repository_key,
            worktree_key,
            legacy_project_roots,
            discovery_error: None,
        }
    }

    fn non_git(project_root: &Path) -> Self {
        let repository_key = non_git_repository_key(project_root);
        let worktree_key = runtime_key(
            "worktree",
            &[
                b"non-git-worktree-v1",
                project_root.as_os_str().as_encoded_bytes(),
            ],
        );
        Self {
            repository_key,
            worktree_key,
            legacy_project_roots: vec![project_root.to_path_buf()],
            discovery_error: None,
        }
    }

    fn unresolved_git(project_root: &Path, discovery_error: String) -> Self {
        let repository_key = runtime_key(
            "unresolved-git",
            &[
                b"unresolved-git-repository-v1",
                project_root.as_os_str().as_encoded_bytes(),
            ],
        );
        let worktree_key = runtime_key(
            "unresolved-git-worktree",
            &[
                b"unresolved-git-worktree-v1",
                project_root.as_os_str().as_encoded_bytes(),
            ],
        );
        Self {
            repository_key,
            worktree_key,
            legacy_project_roots: vec![project_root.to_path_buf()],
            discovery_error: Some(discovery_error),
        }
    }
}

fn find_git_marker(start: &Path) -> Result<Option<PathBuf>> {
    for candidate in start.ancestors() {
        let marker = candidate.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(_) => return Ok(Some(marker)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect Git metadata {}", marker.display())
                });
            }
        }
    }
    Ok(None)
}

fn repository_display_name(common_dir: &Path, relative_project: &Path) -> String {
    if relative_project != Path::new("")
        && relative_project != Path::new(".")
        && let Some(name) = relative_project
            .file_name()
            .and_then(|value| value.to_str())
    {
        return name.to_owned();
    }
    if common_dir.file_name().and_then(|value| value.to_str()) == Some(".git")
        && let Some(name) = common_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
    {
        return name.to_owned();
    }
    common_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repository")
        .to_owned()
}

fn runtime_key(prefix: &str, fields: &[&[u8]]) -> String {
    let mut material = Vec::new();
    for field in fields {
        material.extend_from_slice(&(field.len() as u64).to_be_bytes());
        material.extend_from_slice(field);
    }
    let hash = blake3::hash(&material).to_hex();
    let prefix = sanitize_runtime_segment(prefix);
    let prefix = if prefix.is_empty() {
        "project"
    } else {
        &prefix
    };
    format!("{prefix}-{}", &hash[..16])
}

fn normalize_root_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map(|current| current.join(&path))
            .unwrap_or(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut resolved) = cursor.canonicalize() {
            for component in missing.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(name) = cursor.file_name() else {
            return absolute;
        };
        missing.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return absolute;
        };
        cursor = parent;
    }
}

pub fn load_effective_config(paths: &MemoryPaths) -> Result<EffectiveConfig> {
    let user_config_path = runtime_home_for_paths(paths).join("config.toml");
    let repo_config_path = paths.repo_config_path();
    let mut workflow = WorkflowConfig {
        proposal_approval: ProposalApprovalPolicy::Auto,
    };
    let mut proposal_approval_source = ConfigSource::BuiltInDefault;

    if let Some(policy) = read_proposal_policy(&user_config_path)? {
        workflow.proposal_approval = policy;
        proposal_approval_source = ConfigSource::UserGlobal(user_config_path.clone());
    }

    if let Some(policy) = read_proposal_policy(&repo_config_path)? {
        workflow.proposal_approval = policy;
        proposal_approval_source = ConfigSource::Repo(repo_config_path.clone());
    }

    Ok(EffectiveConfig {
        workflow,
        sources: ConfigSources {
            user_config_path,
            repo_config_path,
            proposal_approval_source,
        },
    })
}

pub fn user_config_path() -> PathBuf {
    runtime_home().join("config.toml")
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

pub fn runtime_home() -> PathBuf {
    if let Some(path) = env::var_os("MEMZOI_HOME") {
        return PathBuf::from(path);
    }
    if let Some(path) = env::var_os("HOME") {
        return PathBuf::from(path).join(MEMORY_DIR_NAME);
    }
    PathBuf::from(MEMORY_DIR_NAME)
}

fn runtime_home_for_paths(paths: &MemoryPaths) -> PathBuf {
    paths
        .repository_runtime_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(runtime_home)
}

fn read_proposal_policy(path: &Path) -> Result<Option<ProposalApprovalPolicy>> {
    if !path.is_file() {
        return Ok(None);
    }

    let config = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let parsed: FileConfig = toml::from_str(&config)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    let Some(workflow) = parsed.workflow else {
        return Ok(None);
    };
    let Some(value) = workflow.proposal_approval else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        bail!(
            "invalid workflow.proposal_approval {value} in {}; expected \"auto\" or \"manual\"",
            path.display()
        );
    };
    value.parse().map(Some).map_err(|_| {
        anyhow::anyhow!(
            "invalid workflow.proposal_approval {value:?} in {}; expected \"auto\" or \"manual\"",
            path.display()
        )
    })
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

fn non_git_repository_key(project_root: &Path) -> String {
    let project_name = project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    runtime_key(
        project_name,
        &[
            b"non-git-repository-v2",
            project_root.as_os_str().as_encoded_bytes(),
        ],
    )
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
    use std::process::Command;
    use tempfile::TempDir;

    fn create_dir(path: impl AsRef<Path>) {
        std::fs::create_dir_all(path).unwrap();
    }

    fn paths_with_runtime_home(temp: &TempDir) -> MemoryPaths {
        let project_root = temp.path().join("repo");
        create_dir(&project_root);
        MemoryPaths::with_runtime_home(
            project_root.canonicalize().unwrap(),
            temp.path().join(".memzoi-runtime"),
        )
    }

    fn write_config(path: impl AsRef<Path>, contents: &str) {
        if let Some(parent) = path.as_ref().parent() {
            create_dir(parent);
        }
        std::fs::write(path, contents).unwrap();
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
        assert_ne!(
            paths.repository_runtime_dir, paths.legacy_runtime_dirs[0],
            "the versioned runtime must remain distinct from its path-keyed migration source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_paths_normalize_symlinked_existing_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let container = temp.path().canonicalize().unwrap();
        let real = container.join("real");
        let alias = container.join("alias");
        create_dir(&real);
        symlink(&real, &alias).unwrap();

        let paths = MemoryPaths::with_runtime_home(
            alias.join("missing-project"),
            alias.join("missing-runtime-home"),
        );

        assert_eq!(paths.project_root, real.join("missing-project"));
        assert!(
            paths
                .runtime_dir
                .starts_with(real.join("missing-runtime-home"))
        );
    }

    #[test]
    fn proposal_approval_policy_precedence_uses_user_global_then_repo_override() {
        let temp = TempDir::new().unwrap();
        let paths = paths_with_runtime_home(&temp);
        let user_config_path =
            normalize_root_path(temp.path().join(".memzoi-runtime")).join("config.toml");
        let repo_config_path = paths.repo_config_path();

        let built_in = load_effective_config(&paths).unwrap();
        assert_eq!(
            built_in.workflow.proposal_approval,
            ProposalApprovalPolicy::Auto
        );
        assert_eq!(
            built_in.sources.proposal_approval_source,
            ConfigSource::BuiltInDefault
        );

        write_config(
            &user_config_path,
            r#"
[workflow]
proposal_approval = "manual"
"#,
        );
        let user_global = load_effective_config(&paths).unwrap();
        assert_eq!(
            user_global.workflow.proposal_approval,
            ProposalApprovalPolicy::Manual
        );
        assert_eq!(
            user_global.sources.proposal_approval_source,
            ConfigSource::UserGlobal(user_config_path.clone())
        );

        write_config(
            &repo_config_path,
            r#"
[workflow]
proposal_approval = "auto"
"#,
        );
        let repo_override = load_effective_config(&paths).unwrap();
        assert_eq!(
            repo_override.workflow.proposal_approval,
            ProposalApprovalPolicy::Auto
        );
        assert_eq!(
            repo_override.sources.proposal_approval_source,
            ConfigSource::Repo(repo_config_path.clone())
        );
    }

    #[test]
    fn invalid_proposal_approval_error_names_path_and_allowed_values() {
        let temp = TempDir::new().unwrap();
        let paths = paths_with_runtime_home(&temp);
        let repo_config_path = paths.repo_config_path();
        write_config(
            &repo_config_path,
            r#"
[workflow]
proposal_approval = "sometimes"
"#,
        );

        let error = load_effective_config(&paths).unwrap_err().to_string();

        assert!(
            error.contains(repo_config_path.to_string_lossy().as_ref()),
            "error should include the invalid config path: {error}"
        );
        assert!(
            error.contains("workflow.proposal_approval"),
            "error should name the invalid key: {error}"
        );
        assert!(
            error.contains("\"auto\"") && error.contains("\"manual\""),
            "error should list allowed policy values: {error}"
        );
        assert!(
            error.contains("sometimes"),
            "error should include the rejected value: {error}"
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

    #[test]
    fn linked_worktrees_share_repository_runtime_but_not_index_runtime() -> anyhow::Result<()> {
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = TempDir::new()?;
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        let runtime_home = temp.path().join("runtime");
        create_dir(&main);
        run_git(&main, &["init", "-q"])?;
        run_git(&main, &["config", "user.email", "fixture@example.test"])?;
        run_git(&main, &["config", "user.name", "Fixture"])?;
        std::fs::write(main.join("README.md"), "fixture\n")?;
        run_git(&main, &["add", "README.md"])?;
        run_git(&main, &["commit", "-qm", "base"])?;
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-qb",
                "linked",
                linked
                    .to_str()
                    .context("linked worktree path must be UTF-8")?,
            ],
        )?;

        let main_paths = MemoryPaths::with_runtime_home(main.canonicalize()?, runtime_home.clone());
        let linked_paths =
            MemoryPaths::with_runtime_home(linked.canonicalize()?, runtime_home.clone());

        assert_eq!(
            main_paths.repository_runtime_dir,
            linked_paths.repository_runtime_dir
        );
        assert_eq!(main_paths.shared_db_path, linked_paths.shared_db_path);
        assert_eq!(main_paths.config_path, linked_paths.config_path);
        assert_ne!(
            main_paths.worktree_runtime_dir,
            linked_paths.worktree_runtime_dir
        );
        assert_ne!(main_paths.index_db_path, linked_paths.index_db_path);
        assert_ne!(main_paths.exports_dir, linked_paths.exports_dir);
        assert_eq!(
            runtime_home_for_paths(&linked_paths),
            normalize_root_path(runtime_home)
        );
        Ok(())
    }

    #[test]
    fn git_marker_without_discovery_never_selects_the_non_git_runtime() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        create_dir(project.join(".git"));
        let project = project.canonicalize()?;
        let runtime_home = normalize_root_path(temp.path().join("runtime"));

        let paths = MemoryPaths::with_runtime_home(project.clone(), runtime_home.clone());

        assert!(paths.validate_runtime_identity().is_err());
        assert_ne!(
            paths.repository_runtime_dir,
            runtime_home
                .join(RUNTIME_PROJECTS_DIR)
                .join(non_git_repository_key(&project))
        );
        Ok(())
    }

    #[test]
    fn linked_worktree_enumeration_failure_is_stored_in_memory_paths() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        create_dir(&project);
        run_git(&project, &["init", "-q"])?;
        let project = project.canonicalize()?;
        let git = discover_git_repository(&project)?
            .context("Git fixture should have a repository identity")?;
        let identity = RuntimeIdentity::discover_with(
            &project,
            |_| Ok(Some(git)),
            |_| bail!("injected worktree enumeration failure"),
        );
        let paths = MemoryPaths::with_runtime_identity(
            project,
            normalize_root_path(temp.path().join("runtime")),
            identity,
        );

        let error = paths
            .validate_runtime_identity()
            .expect_err("partial linked-worktree discovery must fail closed");
        assert!(format!("{error:#}").contains("injected worktree enumeration failure"));
        Ok(())
    }

    fn run_git(directory: &Path, args: &[&str]) -> anyhow::Result<()> {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .context("failed to run Git fixture command")?;
        if !output.status.success() {
            bail!(
                "Git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
}
