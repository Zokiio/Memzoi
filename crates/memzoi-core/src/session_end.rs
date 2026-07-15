use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    MemoryDestination, MemoryLane, MemoryType, RepositoryContentClass, ScopeKind,
    okf::{OkfCreateProposalDraft, OkfProposalSensitivity},
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
    pub sensitivity: OkfProposalSensitivity,
    #[serde(default = "default_repository_content_class")]
    pub content_class: RepositoryContentClass,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub scope: Option<SessionEndScope>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_repository_content_class() -> RepositoryContentClass {
    RepositoryContentClass::Unknown
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
    pub sensitivity: OkfProposalSensitivity,
    pub status: SessionEndCandidateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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

pub(crate) fn session_end_proposal_draft(
    candidate: &SessionEndCandidate,
    actor: &str,
    timestamp: &str,
    proposal_id: String,
) -> Result<OkfCreateProposalDraft> {
    if candidate.destination != MemoryDestination::Repo {
        bail!("session-end proposal drafts require repo destination");
    }
    let scope = candidate.scope.clone().unwrap_or(SessionEndScope {
        kind: ScopeKind::Repo,
        id: None,
        paths: Vec::new(),
    });
    if candidate.sensitivity != OkfProposalSensitivity::RepoSafe {
        bail!("{}", repo_sensitivity_block_reason(candidate.sensitivity));
    }
    Ok(OkfCreateProposalDraft {
        proposal_id,
        memory_type: candidate.memory_type,
        lane: candidate.lane,
        title: candidate.title.clone(),
        body: candidate.body.clone(),
        actor: actor.to_owned(),
        timestamp: timestamp.to_owned(),
        reason: candidate.reason.clone(),
        scope_kind: scope.kind,
        scope_id: scope.id,
        applies_to: scope.paths,
        tags: candidate.tags.clone(),
        sources: Vec::new(),
        sensitivity: candidate.sensitivity,
        content_class: candidate.content_class,
        capture: None,
    })
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
        MemoryDestination::Repo => {}
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

pub(crate) fn repo_sensitivity_block_reason(sensitivity: OkfProposalSensitivity) -> String {
    format!(
        "repo destination requires sensitivity repo-safe; got {}; classify the candidate as repo-safe or choose a non-repo destination",
        sensitivity.as_str()
    )
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
