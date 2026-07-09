use std::fmt;

use serde::{Deserialize, Serialize};

use crate::MemoryDestination;

/// The storage plane in which a memory may be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPlane {
    Git,
    Runtime,
}

impl MemoryPlane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Runtime => "runtime",
        }
    }
}

impl fmt::Display for MemoryPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The write mechanism permitted for a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteRoute {
    FileBackedProposal,
    RuntimeLocal,
    RuntimeSession,
    NoWrite,
}

impl MemoryWriteRoute {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileBackedProposal => "file_backed_proposal",
            Self::RuntimeLocal => "runtime_local",
            Self::RuntimeSession => "runtime_session",
            Self::NoWrite => "no_write",
        }
    }
}

impl fmt::Display for MemoryWriteRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The review gate required before a destination can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryReviewRequirement {
    ProposalReview,
    NoReview,
    HumanDecision,
}

impl MemoryReviewRequirement {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProposalReview => "proposal_review",
            Self::NoReview => "no_review",
            Self::HumanDecision => "human_decision",
        }
    }
}

impl fmt::Display for MemoryReviewRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The complete, destination-specific two-plane policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDestinationPolicy {
    pub destination: MemoryDestination,
    pub plane: Option<MemoryPlane>,
    pub write_route: MemoryWriteRoute,
    pub review: MemoryReviewRequirement,
}

/// Categories that must never be written to canonical repo records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoMemoryExclusion {
    Secrets,
    RawChatTranscripts,
    PrivatePersonalData,
    TemporaryTaskState,
    LocalOnlyState,
}

impl RepoMemoryExclusion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secrets => "secrets",
            Self::RawChatTranscripts => "raw_chat_transcripts",
            Self::PrivatePersonalData => "private_personal_data",
            Self::TemporaryTaskState => "temporary_task_state",
            Self::LocalOnlyState => "local_only_state",
        }
    }
}

impl fmt::Display for RepoMemoryExclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Immutable metadata describing the two-plane memory boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TwoPlaneMemoryPolicy {
    pub canonical_records_glob: &'static str,
    pub runtime_project_root_template: &'static str,
    pub repo_exclusions: &'static [RepoMemoryExclusion],
    pub future_destinations: &'static [&'static str],
}

const REPO_EXCLUSIONS: &[RepoMemoryExclusion] = &[
    RepoMemoryExclusion::Secrets,
    RepoMemoryExclusion::RawChatTranscripts,
    RepoMemoryExclusion::PrivatePersonalData,
    RepoMemoryExclusion::TemporaryTaskState,
    RepoMemoryExclusion::LocalOnlyState,
];

const FUTURE_DESTINATIONS: &[&str] = &["team", "cloud"];

pub static TWO_PLANE_MEMORY_POLICY: TwoPlaneMemoryPolicy = TwoPlaneMemoryPolicy {
    canonical_records_glob: ".memzoi/records/*.md",
    runtime_project_root_template: "${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/",
    repo_exclusions: REPO_EXCLUSIONS,
    future_destinations: FUTURE_DESTINATIONS,
};

impl MemoryDestination {
    pub const ALL: [Self; 5] = [
        Self::Repo,
        Self::Local,
        Self::Session,
        Self::Discard,
        Self::NeedsReview,
    ];

    pub const fn policy(self) -> MemoryDestinationPolicy {
        match self {
            Self::Repo => MemoryDestinationPolicy {
                destination: self,
                plane: Some(MemoryPlane::Git),
                write_route: MemoryWriteRoute::FileBackedProposal,
                review: MemoryReviewRequirement::ProposalReview,
            },
            Self::Local => MemoryDestinationPolicy {
                destination: self,
                plane: Some(MemoryPlane::Runtime),
                write_route: MemoryWriteRoute::RuntimeLocal,
                review: MemoryReviewRequirement::NoReview,
            },
            Self::Session => MemoryDestinationPolicy {
                destination: self,
                plane: Some(MemoryPlane::Runtime),
                write_route: MemoryWriteRoute::RuntimeSession,
                review: MemoryReviewRequirement::NoReview,
            },
            Self::Discard => MemoryDestinationPolicy {
                destination: self,
                plane: None,
                write_route: MemoryWriteRoute::NoWrite,
                review: MemoryReviewRequirement::NoReview,
            },
            Self::NeedsReview => MemoryDestinationPolicy {
                destination: self,
                plane: None,
                write_route: MemoryWriteRoute::NoWrite,
                review: MemoryReviewRequirement::HumanDecision,
            },
        }
    }
}
