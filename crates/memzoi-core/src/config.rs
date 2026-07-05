use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

    pub fn runtime_project_config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn repo_config_path(&self) -> PathBuf {
        self.memory_dir.join("config.toml")
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
        .runtime_dir
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
    }

    #[test]
    fn proposal_approval_policy_precedence_uses_user_global_then_repo_override() {
        let temp = TempDir::new().unwrap();
        let paths = paths_with_runtime_home(&temp);
        let user_config_path = temp.path().join(".memzoi-runtime").join("config.toml");
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
}
