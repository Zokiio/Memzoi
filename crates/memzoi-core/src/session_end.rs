use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    MemoryDestination, MemoryLane, MemoryType, ScopeKind,
    okf::{OkfProposalSensitivity, parse_okf_proposal_markdown},
    proposals::title_to_concept_slug,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionEndDocument {
    pub task: String,
    pub candidates: Vec<SessionEndCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionEndCandidate {
    pub destination: MemoryDestination,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub sensitivity: Option<OkfProposalSensitivity>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub scope: Option<SessionEndScope>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionEndScope {
    #[serde(default = "default_scope_kind")]
    pub kind: ScopeKind,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEndResult {
    pub task: String,
    pub candidates: Vec<SessionEndCandidateResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEndCandidateResult {
    pub index: usize,
    pub destination: MemoryDestination,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub status: SessionEndCandidateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<SessionEndWrite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndCandidateStatus {
    Written,
    Skipped,
    Blocked,
}

impl SessionEndCandidateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEndWrite {
    ProposalFile {
        proposal_id: String,
        path: PathBuf,
    },
    RuntimeRecord {
        record_id: String,
        destination: MemoryDestination,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionEndRepoProposalPlan {
    pub proposal_id: String,
    pub path: PathBuf,
    markdown: String,
}

pub fn parse_session_end_document(input: &str) -> Result<SessionEndDocument> {
    let yaml = session_end_yaml(input)?;
    let document: SessionEndDocument = serde_yaml::from_str(yaml)
        .context("failed to parse session-end structured input as YAML")?;
    validate_session_end_document(&document)?;
    Ok(document)
}

pub(crate) fn validate_session_end_document(document: &SessionEndDocument) -> Result<()> {
    if document.task.trim().is_empty() {
        bail!("session-end task is required");
    }
    if document.candidates.is_empty() {
        bail!("session-end input must include at least one candidate");
    }
    for (index, candidate) in document.candidates.iter().enumerate() {
        validate_session_end_candidate(index, candidate)?;
    }
    Ok(())
}

pub(crate) fn plan_repo_proposal(
    pending_root: &Path,
    candidate: &SessionEndCandidate,
    actor: &str,
    timestamp: &str,
    reserved_ids: &mut BTreeSet<String>,
) -> Result<SessionEndRepoProposalPlan> {
    let base_slug = title_to_concept_slug(&candidate.title).unwrap_or_else(|| "memory".to_owned());
    let base_id = format!("mem_session_{base_slug}");
    let proposal_id = next_available_proposal_id(pending_root, &base_id, reserved_ids)?;
    let path = pending_root.join(format!("{proposal_id}.md"));
    let markdown = render_repo_proposal_markdown(&proposal_id, candidate, actor, timestamp)?;
    parse_okf_proposal_markdown(pending_root, &path, &markdown)?
        .with_context(|| format!("rendered session-end proposal {proposal_id} was ignored"))?;
    reserved_ids.insert(proposal_id.clone());
    Ok(SessionEndRepoProposalPlan {
        proposal_id,
        path,
        markdown,
    })
}

pub(crate) fn write_repo_proposal_file(plan: &SessionEndRepoProposalPlan) -> Result<PathBuf> {
    if let Some(parent) = plan.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create proposal directory {}", parent.display()))?;
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&plan.path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, plan.markdown.as_bytes()))
        .with_context(|| {
            format!(
                "failed to create session-end proposal {}",
                plan.path.display()
            )
        })?;
    Ok(plan.path.clone())
}

fn session_end_yaml(input: &str) -> Result<&str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("session-end input cannot be empty");
    }

    let document = input.trim_start();
    if let Some(rest) = document
        .strip_prefix("---\n")
        .or_else(|| document.strip_prefix("---\r\n"))
    {
        let Some(end) = rest.find("\n---") else {
            bail!("session-end Markdown input is missing closing frontmatter delimiter");
        };
        return Ok(rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]));
    }

    Ok(input)
}

fn validate_session_end_candidate(index: usize, candidate: &SessionEndCandidate) -> Result<()> {
    if candidate.title.trim().is_empty() {
        bail!("session-end candidate {index} title is required");
    }
    if candidate.body.trim().is_empty() {
        bail!("session-end candidate {index} body is required");
    }
    if let Some(reason) = &candidate.reason
        && reason.trim().is_empty()
    {
        bail!("session-end candidate {index} reason cannot be empty");
    }
    for tag in &candidate.tags {
        if tag.trim().is_empty() {
            bail!("session-end candidate {index} tags cannot contain empty entries");
        }
    }
    if let Some(scope) = &candidate.scope {
        if let Some(scope_id) = &scope.id
            && scope_id.trim().is_empty()
        {
            bail!("session-end candidate {index} scope.id cannot be empty");
        }
        for path in &scope.paths {
            validate_relative_path(path)
                .with_context(|| format!("invalid session-end candidate {index} scope path"))?;
        }
    }

    match candidate.destination {
        MemoryDestination::Repo => match candidate.sensitivity {
            Some(OkfProposalSensitivity::RepoSafe) => {}
            Some(sensitivity) => bail!(
                "session-end candidate {index} destination repo requires sensitivity repo-safe; got {}",
                sensitivity.as_str()
            ),
            None => bail!(
                "session-end candidate {index} destination repo requires sensitivity repo-safe"
            ),
        },
        MemoryDestination::Session => {
            if candidate.memory_type != MemoryType::Episode {
                bail!(
                    "session-end candidate {index} with destination session must use type episode"
                );
            }
            if candidate.lane != MemoryLane::Session {
                bail!(
                    "session-end candidate {index} with destination session must use lane session"
                );
            }
        }
        MemoryDestination::Local | MemoryDestination::Discard | MemoryDestination::NeedsReview => {}
    }

    Ok(())
}

fn next_available_proposal_id(
    pending_root: &Path,
    base_id: &str,
    reserved_ids: &BTreeSet<String>,
) -> Result<String> {
    for suffix in 1.. {
        let candidate = if suffix == 1 {
            base_id.to_owned()
        } else {
            format!("{base_id}-{suffix}")
        };
        if reserved_ids.contains(&candidate) {
            continue;
        }
        let path = pending_root.join(format!("{candidate}.md"));
        if !path
            .try_exists()
            .with_context(|| format!("failed to inspect pending proposal {}", path.display()))?
        {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix search returns")
}

fn render_repo_proposal_markdown(
    proposal_id: &str,
    candidate: &SessionEndCandidate,
    actor: &str,
    timestamp: &str,
) -> Result<String> {
    let scope = candidate.scope.clone().unwrap_or(SessionEndScope {
        kind: ScopeKind::Repo,
        id: None,
        paths: Vec::new(),
    });
    let frontmatter = SessionEndProposalFrontmatter {
        id: proposal_id.to_owned(),
        kind: "proposal".to_owned(),
        version: "okf/v0.1".to_owned(),
        profile: "memzoi/v0".to_owned(),
        memory_type: candidate.memory_type,
        lane: candidate.lane,
        title: candidate.title.trim().to_owned(),
        description: first_non_empty_line(&candidate.body).to_owned(),
        status: "proposed".to_owned(),
        proposal: SessionEndProposalMetadata {
            action: "create".to_owned(),
            proposed_by: actor.trim().to_owned(),
            proposed_at: timestamp.to_owned(),
            reason: trimmed_optional(candidate.reason.as_deref()),
        },
        scope: SessionEndProposalScope {
            kind: scope.kind,
            id: trimmed_optional(scope.id.as_deref()),
            paths: scope
                .paths
                .into_iter()
                .map(|path| path.trim().to_owned())
                .collect(),
        },
        tags: candidate
            .tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .collect(),
        timestamp: timestamp.to_owned(),
        created_by: actor.trim().to_owned(),
        supersedes: Vec::new(),
        sensitivity: candidate
            .sensitivity
            .context("repo candidate sensitivity should be validated")?,
    };
    let yaml = serde_yaml::to_string(&frontmatter)
        .context("failed to render session-end proposal frontmatter")?;
    Ok(format!(
        "---\n{}---\n\n# {}\n\n{}\n",
        yaml,
        candidate.title.trim(),
        candidate.body.trim()
    ))
}

fn first_non_empty_line(body: &str) -> &str {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn validate_relative_path(value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("path cannot be empty");
    }
    let path = Path::new(trimmed);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("path must be relative and cannot contain traversal");
    }
    Ok(())
}

fn default_scope_kind() -> ScopeKind {
    ScopeKind::Repo
}

#[derive(Debug, Serialize)]
struct SessionEndProposalFrontmatter {
    id: String,
    kind: String,
    version: String,
    profile: String,
    #[serde(rename = "type")]
    memory_type: MemoryType,
    lane: MemoryLane,
    title: String,
    description: String,
    status: String,
    proposal: SessionEndProposalMetadata,
    scope: SessionEndProposalScope,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    timestamp: String,
    created_by: String,
    supersedes: Vec<String>,
    sensitivity: OkfProposalSensitivity,
}

#[derive(Debug, Serialize)]
struct SessionEndProposalMetadata {
    action: String,
    proposed_by: String,
    proposed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionEndProposalScope {
    kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<String>,
}
