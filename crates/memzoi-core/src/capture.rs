use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    MemoryDestination, MemoryDestinationPolicy, MemoryLane, MemoryPaths, MemoryStatus, MemoryType,
    MemoryWriteRoute, OkfProposalFile, OkfProposalSensitivity, OkfProposalStatus, OkfRecordFile,
    ScopeKind, parse_okf_proposal_markdown, parse_okf_record_markdown,
};

mod adapters;
mod sources;

pub const CAPTURE_REQUEST_SCHEMA: &str = "memzoi/capture-request-v1";
pub const CAPTURE_PLAN_SCHEMA: &str = "memzoi/capture-plan-v1";
pub const CAPTURE_REVIEW_INPUT_SCHEMA: &str = "memzoi/capture-review-input-v1";
pub const CAPTURE_REVIEW_SCHEMA: &str = "memzoi/capture-review-v1";
pub const CAPTURE_APPLY_RESULT_SCHEMA: &str = "memzoi/capture-apply-result-v1";
pub const CAPTURE_PROVENANCE_SCHEMA: &str = "memzoi/capture-provenance-v1";
pub const MARKDOWN_EXTRACTOR_PROFILE: &str = "markdown-deterministic";
pub const MARKDOWN_EXTRACTOR_VERSION: &str = "1.0.0";
pub const INSTRUCTION_EXTRACTOR_PROFILE: &str = "instruction-deterministic";
pub const INSTRUCTION_EXTRACTOR_VERSION: &str = "1.0.0";
pub const ADR_EXTRACTOR_PROFILE: &str = "adr-deterministic";
pub const ADR_EXTRACTOR_VERSION: &str = "1.0.0";
pub const GIT_CHANGE_EXTRACTOR_PROFILE: &str = "git-change-deterministic";
pub const GIT_CHANGE_EXTRACTOR_VERSION: &str = "1.0.0";
pub const MAX_MARKDOWN_SOURCE_BYTES: u64 = 1024 * 1024;
pub const MAX_DIFF_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
pub const CAPTURE_MAX_AGGREGATE_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
pub const CAPTURE_MAX_DIRECTORY_FILES: usize = 128;
pub const CAPTURE_MAX_DIRECTORY_DEPTH: usize = 8;
pub const CAPTURE_MAX_GIT_CHANGED_FILES: usize = 512;
pub const CAPTURE_MAX_GIT_DIFF_HUNKS: usize = 4096;
pub const CAPTURE_MAX_GIT_POLICY_FILE_BYTES: u64 = 64 * 1024;
pub const CAPTURE_MAX_GIT_POLICY_BYTES: u64 = 256 * 1024;
pub const CAPTURE_GIT_PROCESS_TIMEOUT_MILLIS: u64 = 60_000;
pub const CAPTURE_MAX_SOURCE_BYTES: usize = MAX_MARKDOWN_SOURCE_BYTES as usize;
pub const CAPTURE_MAX_PATH_BYTES: usize = 4096;
pub const CAPTURE_MAX_CANDIDATES: usize = 100;
pub const CAPTURE_MAX_MARKDOWN_HEADINGS: usize = 4096;
pub const CAPTURE_MAX_EVIDENCE_ITEM_BYTES: usize = 16 * 1024;
pub const CAPTURE_MAX_EVIDENCE_BYTES: usize = 256 * 1024;
pub const CAPTURE_MAX_INVENTORY_FILES: usize = 10_000;
pub const CAPTURE_MAX_INVENTORY_ENTRIES: usize = 20_000;
pub const CAPTURE_MAX_INVENTORY_DEPTH: usize = 16;
pub const CAPTURE_MAX_INVENTORY_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const CAPTURE_MAX_INVENTORY_BYTES: u64 = 32 * 1024 * 1024;
pub const CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS: usize = 10_000;
pub const CAPTURE_MAX_RUNTIME_INVENTORY_BYTES: u64 = 32 * 1024 * 1024;
pub const CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD: usize = 256;
pub const CAPTURE_MAX_SERIALIZED_PLAN_BYTES: usize = 2 * 1024 * 1024 - 4096;
pub const CAPTURE_MAX_SERIALIZED_REVIEW_BYTES: usize = 2 * 1024 * 1024 - 4096;
const INSTRUCTION_REVIEW_MARKERS: &[&[&str]] = &[
    &["temporary"],
    &["current", "task"],
    &["this", "task"],
    &["session"],
    &["wip"],
    &["scratch"],
    &["personal"],
    &["private"],
    &["local", "only"],
];

#[derive(Debug, Clone)]
pub struct CapturePlanningControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl CapturePlanningControl {
    pub fn new(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("capture planning cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("capture planning timed out");
        }
        Ok(())
    }
}

fn check_planning_control(control: Option<&CapturePlanningControl>) -> Result<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub schema: String,
    pub sources: Vec<CaptureSourceRequest>,
    pub extractor: CaptureExtractorRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSourceRequest {
    pub source_id: String,
    pub locator: CaptureSourceLocator,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<CaptureGitSourceContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureSourceLocator {
    ProjectPath {
        path: String,
    },
    ProjectDirectory {
        path: String,
        recursive: bool,
        ignore_policy: String,
        include: Vec<String>,
    },
    SuppliedBytes {
        display_name: String,
        media_type: String,
        byte_length: u64,
        source_content_hash: String,
    },
    GitRange {
        repository: String,
        base: String,
        head: String,
        merge_parent: String,
        rename_detection: bool,
        diff_format: String,
    },
}

impl CaptureSourceLocator {
    pub fn project_path(&self) -> Option<&str> {
        match self {
            Self::ProjectPath { path } | Self::ProjectDirectory { path, .. } => Some(path),
            Self::SuppliedBytes { .. } | Self::GitRange { .. } => None,
        }
    }

    pub fn durable_reference(&self) -> String {
        match self {
            Self::ProjectPath { path } | Self::ProjectDirectory { path, .. } => path.clone(),
            Self::SuppliedBytes {
                display_name,
                source_content_hash,
                ..
            } => format!("supplied:{display_name}@{source_content_hash}"),
            Self::GitRange {
                repository,
                base,
                head,
                ..
            } => format!("git:{repository}@{base}..{head}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureGitSourceContext {
    pub repository: String,
    pub base: String,
    pub head: String,
}

#[derive(Default)]
pub struct CaptureSourceInputs {
    supplied_bytes: BTreeMap<String, Vec<u8>>,
}

impl CaptureSourceInputs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_supplied_bytes(
        &mut self,
        source_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let source_id = source_id.into();
        if !valid_source_id(&source_id) {
            bail!("capture supplied source_id is invalid");
        }
        if self.supplied_bytes.contains_key(&source_id) {
            bail!("capture supplied source_id is duplicated");
        }
        self.supplied_bytes.insert(source_id, bytes);
        Ok(())
    }

    fn supplied_bytes(&self, source_id: &str) -> Option<&[u8]> {
        self.supplied_bytes.get(source_id).map(Vec::as_slice)
    }

    fn is_empty(&self) -> bool {
        self.supplied_bytes.is_empty()
    }
}

#[derive(Debug, Clone)]
pub(super) struct CaptureSourceDocument {
    pub(super) request: CaptureSourceRequest,
    pub(super) snapshot: CaptureSourceSnapshot,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(super) struct CaptureLoadedSource {
    pub(super) snapshot: CaptureSourceSnapshot,
    pub(super) documents: Vec<CaptureSourceDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureExtractorRequest {
    pub profile: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePlanStatus {
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureDataClass {
    RepoSafe,
    Private,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSourceSnapshot {
    pub source_id: String,
    pub locator: CaptureSourceLocator,
    pub media_type: String,
    pub byte_length: u64,
    pub source_content_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<CaptureSourceMemberSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_inputs: Vec<CapturePolicyInputSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSourceMemberSnapshot {
    pub path: String,
    pub byte_length: u64,
    pub source_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePolicyInputSnapshot {
    pub path: String,
    pub source_content_hash: String,
    pub engine_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvidenceSpan {
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvidence {
    pub source_id: String,
    pub locator: CaptureSourceLocator,
    pub source_content_hash: String,
    pub span: CaptureEvidenceSpan,
    pub evidence_content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub heading_path: Vec<String>,
    pub section_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_location: Option<CaptureSemanticLocation>,
}

impl CaptureEvidence {
    pub fn durable_reference(&self) -> String {
        if let Some(CaptureSemanticLocation::GitChange {
            repository,
            base,
            head,
            hunk,
            old_path,
            new_path,
            ..
        }) = &self.semantic_location
        {
            let path = new_path
                .as_deref()
                .or(old_path.as_deref())
                .unwrap_or("unknown");
            return format!("git:{repository}@{base}..{head}:{path}#{hunk}");
        }
        self.locator.durable_reference()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
// Keep the public v1 semantic-location shape direct and human-readable in Rust and JSON.
#[allow(clippy::large_enum_variant)]
pub enum CaptureSemanticLocation {
    Instruction,
    Adr {
        field: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    GitChange {
        repository: String,
        base: String,
        head: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_blob: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_blob: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_path: Option<String>,
        change_kind: String,
        hunk: String,
        side: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_line_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_line_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_line_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_line_end: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureExtractorIdentity {
    pub kind: String,
    pub id: String,
    pub version: String,
    pub configuration_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureScope {
    pub kind: ScopeKind,
    pub id: Option<String>,
    pub paths: Vec<String>,
}

pub type CaptureMemoryScope = CaptureScope;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureMemoryDraft {
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
    pub scope: CaptureScope,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureClassification {
    pub destination: MemoryDestination,
    pub destination_reason: String,
    pub sensitivity: OkfProposalSensitivity,
    pub sensitivity_reason: String,
    #[serde(default)]
    pub content_class: crate::RepositoryContentClass,
    pub policy: MemoryDestinationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMatchKind {
    CanonicalRecord,
    PendingProposal,
    RuntimeRecord,
    EarlierCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureMatch {
    pub kind: CaptureMatchKind,
    pub id: String,
    pub destination: Option<MemoryDestination>,
    pub content_hash: String,
    pub status: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureAction {
    CreateProposal { proposal_id: String, path: String },
    CreateRuntime { route: MemoryWriteRoute },
    Duplicate { matches: Vec<CaptureMatch> },
    Conflict { matches: Vec<CaptureMatch> },
    NoWrite { reason_code: String },
    Blocked { code: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCandidate {
    pub claim_id: String,
    pub candidate_id: String,
    pub memory: CaptureMemoryDraft,
    pub evidence: Vec<CaptureEvidence>,
    pub extraction: CaptureExtractorIdentity,
    pub confidence: f64,
    pub classification: CaptureClassification,
    pub action: CaptureAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRelevantRecord {
    pub kind: CaptureMatchKind,
    pub id: String,
    pub content_hash: String,
    pub status: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCandidatePrecondition {
    pub duplicate_match_set_hash: String,
    pub conflict_match_set_hash: String,
    pub reserved_proposal_id: Option<String>,
    pub relevant_record_hashes: Vec<CaptureRelevantRecord>,
}

pub type CaptureCandidatePreconditions = CaptureCandidatePrecondition;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePreconditions {
    pub policy_version: String,
    pub candidates: BTreeMap<String, CaptureCandidatePrecondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSafeguards {
    pub policy_version: String,
    pub configuration_hash: String,
    pub max_source_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_aggregate_source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_directory_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_directory_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changed_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_diff_hunks: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_policy_file_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_policy_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_timeout_millis: Option<u64>,
    pub max_path_bytes: usize,
    pub max_candidates: usize,
    pub max_markdown_headings: usize,
    pub max_evidence_item_bytes: usize,
    pub max_evidence_bytes: usize,
    pub max_inventory_files: usize,
    pub max_inventory_entries: usize,
    pub max_inventory_depth: usize,
    pub max_inventory_file_bytes: u64,
    pub max_inventory_bytes: u64,
    pub max_runtime_inventory_records: usize,
    pub max_runtime_inventory_bytes: u64,
    pub max_runtime_paths_per_record: usize,
    pub max_serialized_plan_bytes: usize,
    pub max_serialized_review_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CapturePlanSummary {
    pub sources: usize,
    pub candidates: usize,
    pub create_proposals: usize,
    pub runtime_writes: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    pub needs_review: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureDiagnostic {
    pub code: String,
    pub source_id: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePlan {
    pub schema: String,
    pub plan_id: String,
    pub status: CapturePlanStatus,
    pub data_class: CaptureDataClass,
    pub request: CaptureRequest,
    pub sources: Vec<CaptureSourceSnapshot>,
    pub safeguards: CaptureSafeguards,
    pub preconditions: CapturePreconditions,
    pub extractor: CaptureExtractorIdentity,
    pub candidates: Vec<CaptureCandidate>,
    pub summary: CapturePlanSummary,
    pub diagnostics: Vec<CaptureDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureReviewOutcome {
    Accept,
    Reject,
    Edit,
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureReviewDecisionInput {
    pub candidate_id: String,
    pub outcome: CaptureReviewOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<CaptureMemoryDraft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_destination: Option<MemoryDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_class: Option<crate::RepositoryContentClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureReviewInput {
    pub schema: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_review_id: Option<String>,
    pub decisions: Vec<CaptureReviewDecisionInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureReviewDecision {
    pub candidate_id: String,
    pub outcome: CaptureReviewOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_candidate: Option<CaptureCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureReview {
    pub schema: String,
    pub review_id: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_review_id: Option<String>,
    pub data_class: CaptureDataClass,
    pub reviewed_by: String,
    pub reviewed_at: String,
    pub decisions: Vec<CaptureReviewDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureWrite {
    ProposalFile {
        candidate_id: String,
        proposal_id: String,
        path: String,
    },
    RuntimeRecord {
        candidate_id: String,
        record_id: String,
        destination: MemoryDestination,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureApplyResult {
    pub schema: String,
    pub plan_id: String,
    pub review_id: String,
    pub writes: Vec<CaptureWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProvenance {
    pub schema: String,
    pub plan_id: String,
    pub review_id: String,
    pub claim_id: String,
    pub reviewed_claim_id: String,
    pub candidate_id: String,
    pub reviewed_candidate_id: String,
    pub extraction: CaptureExtractorIdentity,
    pub evidence: Vec<CaptureEvidence>,
    pub confidence: String,
    pub classification: CaptureClassification,
    pub destination: MemoryDestination,
    pub sensitivity: OkfProposalSensitivity,
    pub review_outcome: CaptureReviewOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_reason_code: Option<String>,
    pub reviewed_by: String,
    pub reviewed_at: String,
    pub routed_by: String,
}

impl CaptureProvenance {
    pub fn compact_for_record(&self) -> Self {
        let mut value = self.clone();
        for evidence in &mut value.evidence {
            evidence.text = None;
        }
        value
    }
}

pub fn parse_capture_request(input: &str) -> Result<CaptureRequest> {
    if input.trim().is_empty() {
        bail!("capture request cannot be empty");
    }
    let request: CaptureRequest =
        serde_yaml::from_str(input).context("failed to parse capture request")?;
    validate_request(&request)?;
    Ok(request)
}

pub fn parse_capture_plan(input: &str) -> Result<CapturePlan> {
    if input.trim().is_empty() {
        bail!("capture plan cannot be empty");
    }
    let plan: CapturePlan = serde_json::from_str(input).context("failed to parse capture plan")?;
    validate_plan_identity(&plan)?;
    Ok(plan)
}

pub fn parse_capture_review_input(input: &str) -> Result<CaptureReviewInput> {
    let review: CaptureReviewInput = parse_capture_artifact(input, "capture review input")?;
    if review.schema != CAPTURE_REVIEW_INPUT_SCHEMA {
        bail!("unsupported capture review input schema");
    }
    if review.plan_id.trim().is_empty() {
        bail!("capture review input plan_id cannot be empty");
    }
    if review.decisions.is_empty() {
        bail!("capture review input must contain decisions");
    }
    Ok(review)
}

pub fn parse_capture_review(input: &str) -> Result<CaptureReview> {
    let review: CaptureReview = parse_capture_artifact(input, "capture review")?;
    validate_review_identity(&review)?;
    Ok(review)
}

fn parse_capture_artifact<T>(input: &str, label: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    if input.trim().is_empty() {
        bail!("{label} cannot be empty");
    }
    serde_json::from_str(input)
        .or_else(|_| serde_yaml::from_str(input))
        .with_context(|| format!("failed to parse {label}"))
}

pub fn build_capture_review(
    paths: &MemoryPaths,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    build_capture_review_with_inputs(
        paths,
        plan,
        input,
        &CaptureSourceInputs::default(),
        reviewed_by,
        reviewed_at,
    )
}

pub fn build_capture_review_with_inputs(
    paths: &MemoryPaths,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    source_inputs: &CaptureSourceInputs,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    build_capture_review_inner(
        paths,
        None,
        plan,
        input,
        None,
        source_inputs,
        reviewed_by,
        reviewed_at,
    )
}

pub fn build_capture_review_with_prior(
    paths: &MemoryPaths,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    prior_review: &CaptureReview,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    build_capture_review_with_prior_and_inputs(
        paths,
        plan,
        input,
        prior_review,
        &CaptureSourceInputs::default(),
        reviewed_by,
        reviewed_at,
    )
}

pub fn build_capture_review_with_prior_and_inputs(
    paths: &MemoryPaths,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    prior_review: &CaptureReview,
    source_inputs: &CaptureSourceInputs,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    build_capture_review_inner(
        paths,
        None,
        plan,
        input,
        Some(prior_review),
        source_inputs,
        reviewed_by,
        reviewed_at,
    )
}

// Every argument is an independently revalidated review-boundary input.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_capture_review_with_connection_and_inputs(
    paths: &MemoryPaths,
    conn: &Connection,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    prior_review: Option<&CaptureReview>,
    source_inputs: &CaptureSourceInputs,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    build_capture_review_inner(
        paths,
        Some(conn),
        plan,
        input,
        prior_review,
        source_inputs,
        reviewed_by,
        reviewed_at,
    )
}

// The private implementation mirrors the complete public review boundary.
#[allow(clippy::too_many_arguments)]
fn build_capture_review_inner(
    paths: &MemoryPaths,
    runtime_conn: Option<&Connection>,
    plan: &CapturePlan,
    input: CaptureReviewInput,
    prior_review: Option<&CaptureReview>,
    source_inputs: &CaptureSourceInputs,
    reviewed_by: &str,
    reviewed_at: &str,
) -> Result<CaptureReview> {
    validate_plan_identity(plan)?;
    if plan.status != CapturePlanStatus::Ready {
        bail!("blocked capture plans cannot be reviewed");
    }
    if input.schema != CAPTURE_REVIEW_INPUT_SCHEMA {
        bail!("unsupported capture review input schema");
    }
    if input.plan_id != plan.plan_id {
        bail!("capture review input does not match the plan");
    }
    validate_prior_review_lineage(
        paths,
        runtime_conn,
        plan,
        &input,
        prior_review,
        source_inputs,
    )?;
    let reviewed_by = reviewed_by.trim();
    let reviewed_at = reviewed_at.trim();
    validate_capture_actor(reviewed_by)?;
    time::OffsetDateTime::parse(reviewed_at, &time::format_description::well_known::Rfc3339)
        .context("capture reviewed_at must be RFC 3339")?;

    validate_capture_plan_live_state(paths, runtime_conn, plan, source_inputs, None)
        .context("stale capture plan")?;

    let mut inputs = BTreeMap::new();
    for decision in input.decisions {
        if inputs
            .insert(decision.candidate_id.clone(), decision)
            .is_some()
        {
            bail!("capture review contains a duplicate candidate decision");
        }
    }
    if inputs.len() != plan.candidates.len() {
        bail!("capture review must decide every plan candidate exactly once");
    }

    let inventory = match runtime_conn {
        Some(conn) => load_inventory_with_connection(paths, conn, None)?,
        None => load_inventory(paths, None)?,
    };
    let mut reviewed_reserved_ids = inventory.reserved_ids.clone();
    for candidate in &plan.candidates {
        if inputs
            .get(&candidate.candidate_id)
            .is_some_and(|decision| decision.outcome == CaptureReviewOutcome::Accept)
            && let CaptureAction::CreateProposal { proposal_id, .. } = &candidate.action
            && !reviewed_reserved_ids.insert(proposal_id.clone())
        {
            bail!("capture proposal reservation became stale");
        }
    }

    let mut decisions = Vec::with_capacity(plan.candidates.len());
    for candidate in &plan.candidates {
        let decision = inputs
            .remove(&candidate.candidate_id)
            .context("capture review omitted a plan candidate")?;
        let reason_code = decision
            .reason_code
            .as_deref()
            .map(str::trim)
            .map(str::to_owned);
        validate_reason_code(reason_code.as_deref())?;
        let reviewed_candidate = match decision.outcome {
            CaptureReviewOutcome::Accept => {
                if decision.memory.is_some()
                    || decision.requested_destination.is_some()
                    || decision.content_class.is_some()
                {
                    bail!("accept decisions cannot edit capture candidates");
                }
                require_routeable_review_action(&candidate.action)?;
                Some(candidate.clone())
            }
            CaptureReviewOutcome::Reject | CaptureReviewOutcome::Defer => {
                if decision.memory.is_some()
                    || decision.requested_destination.is_some()
                    || decision.content_class.is_some()
                {
                    bail!("reject and defer decisions cannot edit capture candidates");
                }
                None
            }
            CaptureReviewOutcome::Edit => {
                if matches!(candidate.action, CaptureAction::Duplicate { .. }) {
                    bail!(
                        "duplicate capture candidates require regeneration or lifecycle resolution"
                    );
                }
                if matches!(candidate.action, CaptureAction::Conflict { .. }) {
                    bail!("conflicting capture candidates require lifecycle resolution");
                }
                let memory = decision
                    .memory
                    .as_ref()
                    .context("edit decisions require a complete memory draft")?;
                let destination = decision
                    .requested_destination
                    .unwrap_or(candidate.classification.destination);
                Some(rebuild_edited_candidate(
                    &inventory,
                    &mut reviewed_reserved_ids,
                    candidate,
                    memory,
                    destination,
                    decision.content_class,
                )?)
            }
        };
        decisions.push(CaptureReviewDecision {
            candidate_id: candidate.candidate_id.clone(),
            outcome: decision.outcome,
            reason_code,
            reviewed_candidate,
        });
    }
    if !inputs.is_empty() {
        bail!("capture review names an unknown candidate");
    }
    validate_selected_review_matches(&decisions)?;

    let data_class = review_data_class(plan.data_class, &decisions);
    let mut review = CaptureReview {
        schema: CAPTURE_REVIEW_SCHEMA.to_owned(),
        review_id: String::new(),
        plan_id: plan.plan_id.clone(),
        prior_review_id: input.prior_review_id,
        data_class,
        reviewed_by: reviewed_by.to_owned(),
        reviewed_at: reviewed_at.to_owned(),
        decisions,
    };
    review.review_id = recompute_capture_review_id(&review)?;
    if serde_json::to_vec(&review)?.len() > CAPTURE_MAX_SERIALIZED_REVIEW_BYTES {
        bail!("capture review exceeds the configured serialized output limit");
    }
    Ok(review)
}

fn validate_prior_review_lineage(
    paths: &MemoryPaths,
    runtime_conn: Option<&Connection>,
    plan: &CapturePlan,
    input: &CaptureReviewInput,
    prior_review: Option<&CaptureReview>,
    source_inputs: &CaptureSourceInputs,
) -> Result<()> {
    let prior_id = input.prior_review_id.as_deref();
    if let Some(prior_id) = prior_id
        && !valid_capture_identity(prior_id, "review")
    {
        bail!("capture prior_review_id is invalid");
    }

    let Some(prior_review) = prior_review else {
        if prior_id.is_some() {
            bail!("a prior review artifact is required for deferred-review lineage");
        }
        return Ok(());
    };
    validate_review_identity(prior_review)?;
    if prior_id != Some(prior_review.review_id.as_str()) {
        bail!("capture prior review artifact does not match prior_review_id");
    }
    if prior_review.plan_id != plan.plan_id {
        bail!("capture prior review belongs to a different plan");
    }
    if prior_review.prior_review_id.is_some() {
        bail!(
            "capture review lineage beyond one predecessor requires a complete review chain and is unsupported"
        );
    }

    let replay_input = CaptureReviewInput {
        schema: CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
        plan_id: plan.plan_id.clone(),
        prior_review_id: None,
        decisions: prior_review
            .decisions
            .iter()
            .map(review_decision_as_input)
            .collect::<Result<Vec<_>>>()?,
    };
    let replayed = build_capture_review_inner(
        paths,
        runtime_conn,
        plan,
        replay_input,
        None,
        source_inputs,
        &prior_review.reviewed_by,
        &prior_review.reviewed_at,
    )
    .context("capture prior review is not a complete semantic review of the plan")?;
    if replayed.decisions != prior_review.decisions
        || replayed.data_class != prior_review.data_class
    {
        bail!("capture prior review is not a complete semantic review of the plan");
    }

    let next = input
        .decisions
        .iter()
        .map(|decision| (decision.candidate_id.as_str(), decision))
        .collect::<BTreeMap<_, _>>();
    for prior in &prior_review.decisions {
        if prior.outcome == CaptureReviewOutcome::Defer {
            continue;
        }
        let current = next
            .get(prior.candidate_id.as_str())
            .context("later capture review omitted a prior terminal decision")?;
        let expected = review_decision_as_input(prior)?;
        if normalized_review_decision_input(current) != normalized_review_decision_input(&expected)
        {
            bail!("later capture reviews may change only previously deferred decisions");
        }
    }
    Ok(())
}

fn review_decision_as_input(
    decision: &CaptureReviewDecision,
) -> Result<CaptureReviewDecisionInput> {
    let (memory, requested_destination, content_class) =
        if decision.outcome == CaptureReviewOutcome::Edit {
            let candidate = decision
                .reviewed_candidate
                .as_ref()
                .context("edited prior review decision is missing its candidate")?;
            (
                Some(candidate.memory.clone()),
                Some(candidate.classification.destination),
                Some(candidate.classification.content_class),
            )
        } else {
            (None, None, None)
        };
    Ok(CaptureReviewDecisionInput {
        candidate_id: decision.candidate_id.clone(),
        outcome: decision.outcome,
        reason_code: decision.reason_code.clone(),
        memory,
        requested_destination,
        content_class,
    })
}

fn normalized_review_decision_input(
    decision: &CaptureReviewDecisionInput,
) -> CaptureReviewDecisionInput {
    let mut normalized = decision.clone();
    normalized.reason_code = normalized
        .reason_code
        .as_deref()
        .map(str::trim)
        .map(str::to_owned);
    normalized
}

fn valid_capture_identity(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('_'))
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn require_routeable_review_action(action: &CaptureAction) -> Result<()> {
    match action {
        CaptureAction::CreateProposal { .. } | CaptureAction::CreateRuntime { .. } => Ok(()),
        CaptureAction::Duplicate { .. } => {
            bail!("duplicate capture candidates cannot be accepted as new memory")
        }
        CaptureAction::Conflict { .. } => {
            bail!("conflicting capture candidates require lifecycle resolution")
        }
        CaptureAction::NoWrite { .. } => {
            bail!("no-write capture candidates must be edited, rejected, or deferred")
        }
        CaptureAction::Blocked { .. } => bail!("blocked capture candidates cannot be reviewed"),
    }
}

fn validate_reason_code(reason: Option<&str>) -> Result<()> {
    let Some(reason) = reason else {
        return Ok(());
    };
    let reason = reason.trim();
    if reason.is_empty()
        || reason.len() > 128
        || !reason
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("capture review reason_code is invalid");
    }
    if prohibited_finding(reason.as_bytes()).is_some() {
        bail!("capture review reason_code contains prohibited content");
    }
    Ok(())
}

pub(crate) fn validate_capture_actor(actor: &str) -> Result<()> {
    let actor = actor.trim();
    if actor.is_empty()
        || actor.len() > 256
        || actor.chars().any(char::is_control)
        || prohibited_finding(actor.as_bytes()).is_some()
    {
        bail!("capture actor identity is invalid");
    }
    Ok(())
}

fn review_data_class(
    plan_class: CaptureDataClass,
    decisions: &[CaptureReviewDecision],
) -> CaptureDataClass {
    if plan_class == CaptureDataClass::Blocked {
        return CaptureDataClass::Blocked;
    }
    if plan_class == CaptureDataClass::Private
        || decisions.iter().any(|decision| {
            decision
                .reviewed_candidate
                .as_ref()
                .is_some_and(|candidate| {
                    candidate.classification.destination != MemoryDestination::Repo
                        || candidate.classification.sensitivity != OkfProposalSensitivity::RepoSafe
                })
        })
    {
        CaptureDataClass::Private
    } else {
        CaptureDataClass::RepoSafe
    }
}

fn validate_selected_review_matches(decisions: &[CaptureReviewDecision]) -> Result<()> {
    let mut earlier = Vec::<&CaptureMemoryDraft>::new();
    for candidate in decisions
        .iter()
        .filter_map(|decision| decision.reviewed_candidate.as_ref())
    {
        let exact = draft_key(&candidate.memory)?;
        let conflict = conflict_key(&candidate.memory);
        for memory in &earlier {
            if draft_key(memory)? == exact {
                bail!("reviewed capture candidates would create an exact duplicate");
            }
            if conflict_key(memory) == conflict {
                bail!("reviewed capture candidates would create a possible conflict");
            }
        }
        earlier.push(&candidate.memory);
    }
    Ok(())
}

pub fn plan_capture(paths: &MemoryPaths, request: CaptureRequest) -> Result<CapturePlan> {
    plan_capture_with_inputs(paths, request, &CaptureSourceInputs::default())
}

pub fn plan_capture_with_inputs(
    paths: &MemoryPaths,
    request: CaptureRequest,
    source_inputs: &CaptureSourceInputs,
) -> Result<CapturePlan> {
    plan_capture_inner(paths, request, source_inputs, None, None)
}

pub fn plan_capture_with_control(
    paths: &MemoryPaths,
    request: CaptureRequest,
    control: &CapturePlanningControl,
) -> Result<CapturePlan> {
    plan_capture_with_inputs_and_control(paths, request, &CaptureSourceInputs::default(), control)
}

pub fn plan_capture_with_inputs_and_control(
    paths: &MemoryPaths,
    request: CaptureRequest,
    source_inputs: &CaptureSourceInputs,
    control: &CapturePlanningControl,
) -> Result<CapturePlan> {
    plan_capture_inner(paths, request, source_inputs, None, Some(control))
}

fn plan_capture_inner(
    paths: &MemoryPaths,
    request: CaptureRequest,
    source_inputs: &CaptureSourceInputs,
    runtime_conn: Option<&Connection>,
    control: Option<&CapturePlanningControl>,
) -> Result<CapturePlan> {
    check_planning_control(control)?;
    let extractor = extractor_identity(&request.extractor.profile)?;
    let safeguards = safeguards(&request.extractor.profile)?;
    if let Err(error) = validate_request(&request) {
        if request.schema == CAPTURE_REQUEST_SCHEMA
            && request.sources.len() == 1
            && supported_extractor_profile(&request.extractor.profile)
        {
            let _ = error;
            return blocked_after_planning_failure(
                &request,
                safeguards,
                extractor,
                "source_preflight_failed",
                control,
            );
        }
        return Err(error);
    }
    let source = &request.sources[0];
    let loaded = match sources::load_capture_source(paths, source, source_inputs, control) {
        Ok(source) => source,
        Err(error) => {
            let _ = error;
            return blocked_after_planning_failure(
                &request,
                safeguards,
                extractor,
                "source_preflight_failed",
                control,
            );
        }
    };

    for document in &loaded.documents {
        if let Some((_finding, line)) =
            adapters::prohibited_finding_for_profile(&request.extractor.profile, &document.bytes)
        {
            return blocked_plan(
                &request,
                safeguards,
                extractor,
                "prohibited_content_detected",
                line,
            );
        }
        if std::str::from_utf8(&document.bytes).is_err() {
            return blocked_plan(
                &request,
                safeguards,
                extractor,
                "unsupported_utf8_content",
                None,
            );
        }
        if document.bytes.contains(&0) {
            return blocked_plan(
                &request,
                safeguards,
                extractor,
                "unsupported_binary_content",
                None,
            );
        }
    }
    check_planning_control(control)?;
    let inventory_result = match runtime_conn {
        Some(conn) => load_inventory_with_connection(paths, conn, control),
        None => load_inventory(paths, control),
    };
    let inventory = match inventory_result {
        Ok(inventory) => inventory,
        Err(error) => {
            let _ = error;
            return blocked_after_planning_failure(
                &request,
                safeguards,
                extractor,
                "inventory_safeguard_failed",
                control,
            );
        }
    };
    let extraction = match adapters::extract_profile(
        source,
        &loaded,
        &extractor,
        &request.extractor.profile,
        control,
    ) {
        Ok(extraction) => extraction,
        Err(error) => {
            let _ = error;
            return blocked_after_planning_failure(
                &request,
                safeguards,
                extractor,
                "extractor_safeguard_failed",
                control,
            );
        }
    };
    let mut candidates = extraction.candidates;
    if let Err(error) = classify_actions(&inventory, &mut candidates, control) {
        let _ = error;
        return blocked_after_planning_failure(
            &request,
            safeguards,
            extractor,
            "inventory_safeguard_failed",
            control,
        );
    }
    let preconditions = candidate_preconditions(&candidates);
    let data_class = if candidates.iter().all(candidate_is_repo_safe_for_plan) {
        CaptureDataClass::RepoSafe
    } else {
        CaptureDataClass::Private
    };
    let summary = summarize(&candidates);
    let runtime_inventory_affected = candidates.iter().any(|candidate| {
        matches!(
            &candidate.action,
            CaptureAction::NoWrite { reason_code }
                if reason_code == "runtime_inventory_unavailable"
        )
    });
    let mut diagnostics = extraction.diagnostics;
    if runtime_inventory_affected {
        diagnostics.push(CaptureDiagnostic {
            code: "runtime_inventory_unavailable".to_owned(),
            source_id: None,
            line: None,
        });
    }
    let mut plan = CapturePlan {
        schema: CAPTURE_PLAN_SCHEMA.to_owned(),
        plan_id: String::new(),
        status: CapturePlanStatus::Ready,
        data_class,
        request,
        sources: vec![loaded.snapshot],
        safeguards,
        preconditions,
        extractor,
        candidates,
        summary,
        diagnostics,
    };
    check_planning_control(control)?;
    plan.plan_id = recompute_capture_plan_id(&plan)?;
    if serde_json::to_vec(&plan)?.len() > CAPTURE_MAX_SERIALIZED_PLAN_BYTES {
        return blocked_after_planning_failure(
            &plan.request,
            plan.safeguards.clone(),
            plan.extractor.clone(),
            "output_safeguard_failed",
            control,
        );
    }
    check_planning_control(control)?;
    Ok(plan)
}

pub(crate) fn validate_capture_plan_live_state(
    paths: &MemoryPaths,
    runtime_conn: Option<&Connection>,
    plan: &CapturePlan,
    source_inputs: &CaptureSourceInputs,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    validate_plan_identity(plan)?;
    if plan.status != CapturePlanStatus::Ready {
        bail!("blocked capture plans have no live review state");
    }
    check_planning_control(control)?;
    if plan.extractor != extractor_identity(&plan.request.extractor.profile)? {
        bail!("capture extractor profile changed");
    }
    if plan.safeguards != safeguards(&plan.request.extractor.profile)? {
        bail!("capture safeguards changed");
    }
    let source = plan
        .request
        .sources
        .first()
        .context("capture plan source is missing")?;
    let loaded = sources::load_capture_source(paths, source, source_inputs, control)?;
    if plan.sources.as_slice() != [loaded.snapshot.clone()] {
        bail!("capture source snapshot changed");
    }
    validate_loaded_source_preflight(&plan.request.extractor.profile, &loaded)?;
    validate_plan_evidence(plan, &loaded)?;

    let inventory = match runtime_conn {
        Some(conn) => load_inventory_with_connection(paths, conn, control)?,
        None => load_inventory(paths, control)?,
    };
    if plan.extractor.kind != "deterministic" {
        bail!("capture profile requires a trusted issuance attestation before replay");
    }
    let replay = adapters::extract_profile(
        source,
        &loaded,
        &plan.extractor,
        &plan.request.extractor.profile,
        control,
    )?;
    let mut replay_candidates = replay.candidates;
    let mut replay_diagnostics = replay.diagnostics;
    classify_actions(&inventory, &mut replay_candidates, control)?;
    if replay_candidates.iter().any(|candidate| {
        matches!(
            &candidate.action,
            CaptureAction::NoWrite { reason_code }
                if reason_code == "runtime_inventory_unavailable"
        )
    }) {
        replay_diagnostics.push(CaptureDiagnostic {
            code: "runtime_inventory_unavailable".to_owned(),
            source_id: None,
            line: None,
        });
    }
    if replay_candidates != plan.candidates || replay_diagnostics != plan.diagnostics {
        bail!("capture plan no longer matches deterministic extraction output");
    }
    let mut expected_candidates = Vec::with_capacity(plan.candidates.len());
    for planned in &plan.candidates {
        check_planning_control(control)?;
        let (memory_type, lane, destination, sensitivity, destination_reason, sensitivity_reason) =
            core_candidate_policy(&plan.request, planned)?;
        if planned.memory.memory_type != memory_type || planned.memory.lane != lane {
            bail!("capture candidate memory shape is inconsistent with its evidence profile");
        }
        let normalized = normalize_reviewed_memory(&planned.memory)?;
        if normalized.title != planned.memory.title
            || normalized.body != planned.memory.body
            || normalized.scope != planned.memory.scope
            || normalized.tags.len() != planned.memory.tags.len()
            || normalized.tags.iter().collect::<BTreeSet<_>>()
                != planned.memory.tags.iter().collect::<BTreeSet<_>>()
        {
            bail!("capture candidate memory is not in canonical form");
        }
        expected_candidates.push(candidate(
            planned.memory.clone(),
            planned.evidence.clone(),
            &plan.extractor,
            destination,
            sensitivity,
            capture_content_class(destination, sensitivity),
            destination_reason,
            sensitivity_reason,
        )?);
    }
    classify_actions(&inventory, &mut expected_candidates, control)?;
    if expected_candidates != plan.candidates {
        bail!("capture candidate identity, classification, or action changed");
    }
    if candidate_preconditions(&expected_candidates) != plan.preconditions {
        bail!("capture routing preconditions changed");
    }
    let expected_data_class = if expected_candidates
        .iter()
        .all(candidate_is_repo_safe_for_plan)
    {
        CaptureDataClass::RepoSafe
    } else {
        CaptureDataClass::Private
    };
    if plan.data_class != expected_data_class || plan.summary != summarize(&expected_candidates) {
        bail!("capture plan classification or summary changed");
    }
    Ok(())
}

fn validate_loaded_source_preflight(profile: &str, loaded: &CaptureLoadedSource) -> Result<()> {
    for document in &loaded.documents {
        if adapters::prohibited_finding_for_profile(profile, &document.bytes).is_some() {
            bail!("capture source now contains prohibited content");
        }
        if document.bytes.contains(&0) || std::str::from_utf8(&document.bytes).is_err() {
            bail!("capture source is no longer supported text");
        }
    }
    Ok(())
}

fn validate_plan_evidence(plan: &CapturePlan, loaded: &CaptureLoadedSource) -> Result<()> {
    for candidate in &plan.candidates {
        if candidate.extraction != plan.extractor || candidate.evidence.is_empty() {
            bail!("capture candidate extractor or evidence changed");
        }
        let mut candidate_text =
            Vec::with_capacity(candidate.memory.title.len() + candidate.memory.body.len() + 1);
        candidate_text.extend_from_slice(candidate.memory.title.as_bytes());
        candidate_text.push(b'\n');
        candidate_text.extend_from_slice(candidate.memory.body.as_bytes());
        if prohibited_finding(&candidate_text).is_some() {
            bail!("capture candidate contains prohibited content");
        }
        for evidence in &candidate.evidence {
            let document = loaded
                .documents
                .iter()
                .find(|document| {
                    document.request.source_id == evidence.source_id
                        && document.request.locator == evidence.locator
                        && document.snapshot.source_content_hash == evidence.source_content_hash
                })
                .context("capture evidence source changed")?;
            let start = usize::try_from(evidence.span.byte_start)
                .context("capture evidence start is out of range")?;
            let end = usize::try_from(evidence.span.byte_end)
                .context("capture evidence end is out of range")?;
            let document_text = std::str::from_utf8(&document.bytes)
                .context("capture evidence source is no longer UTF-8")?;
            if start >= end
                || end > document.bytes.len()
                || !document_text.is_char_boundary(start)
                || !document_text.is_char_boundary(end)
            {
                bail!("capture evidence span changed");
            }
            let excerpt = &document.bytes[start..end];
            if content_hash(excerpt) != evidence.evidence_content_hash
                || evidence.text.as_deref().map(str::as_bytes) != Some(excerpt)
            {
                bail!("capture evidence content changed");
            }
            let line_start = 1 + document.bytes[..start]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count() as u64;
            let line_end = line_start
                + excerpt[..excerpt.len() - 1]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count() as u64;
            if evidence.span.line_start != line_start || evidence.span.line_end != line_end {
                bail!("capture evidence line location changed");
            }
            let semantic_matches = matches!(
                (
                    &plan.request.extractor.profile[..],
                    &evidence.semantic_location
                ),
                (MARKDOWN_EXTRACTOR_PROFILE, None)
                    | (
                        INSTRUCTION_EXTRACTOR_PROFILE,
                        Some(CaptureSemanticLocation::Instruction)
                    )
                    | (
                        ADR_EXTRACTOR_PROFILE,
                        Some(CaptureSemanticLocation::Adr { .. })
                    )
                    | (
                        GIT_CHANGE_EXTRACTOR_PROFILE,
                        Some(CaptureSemanticLocation::GitChange { .. })
                    )
            );
            if !semantic_matches {
                bail!("capture evidence semantic location is inconsistent with its profile");
            }
        }
    }
    Ok(())
}

type CoreCandidatePolicy = (
    MemoryType,
    MemoryLane,
    MemoryDestination,
    OkfProposalSensitivity,
    &'static str,
    &'static str,
);

fn core_candidate_policy(
    request: &CaptureRequest,
    candidate: &CaptureCandidate,
) -> Result<CoreCandidatePolicy> {
    match request.extractor.profile.as_str() {
        MARKDOWN_EXTRACTOR_PROFILE => markdown_candidate_policy(candidate),
        INSTRUCTION_EXTRACTOR_PROFILE => instruction_candidate_policy(candidate),
        ADR_EXTRACTOR_PROFILE => adr_candidate_policy(candidate),
        GIT_CHANGE_EXTRACTOR_PROFILE => git_candidate_policy(request, candidate),
        _ => bail!("unsupported capture extractor profile"),
    }
}

fn markdown_candidate_policy(candidate: &CaptureCandidate) -> Result<CoreCandidatePolicy> {
    validate_project_evidence_scope(candidate)?;
    let section_kind = one_section_kind(candidate)?;
    if section_kind == "unclassified" {
        return Ok((
            MemoryType::Fact,
            MemoryLane::Semantic,
            MemoryDestination::NeedsReview,
            OkfProposalSensitivity::Unknown,
            "unrecognized_markdown_requires_review",
            "unrecognized_markdown_sensitivity_unknown",
        ));
    }
    let (memory_type, lane, destination, sensitivity) =
        typed_section_policy(section_kind).context("capture Markdown section kind is invalid")?;
    validate_typed_evidence_heading(candidate, section_kind, None)?;
    if destination == MemoryDestination::Repo {
        Ok((
            memory_type,
            lane,
            MemoryDestination::NeedsReview,
            OkfProposalSensitivity::Unknown,
            "generic_markdown_requires_contextual_review",
            "generic_markdown_sensitivity_unknown",
        ))
    } else {
        Ok((
            memory_type,
            lane,
            destination,
            sensitivity,
            "deterministic_typed_markdown_section",
            "deterministic_typed_markdown_profile",
        ))
    }
}

fn instruction_candidate_policy(candidate: &CaptureCandidate) -> Result<CoreCandidatePolicy> {
    validate_instruction_evidence_scope(candidate)?;
    if candidate.evidence.iter().any(|evidence| {
        !matches!(
            evidence.semantic_location,
            Some(CaptureSemanticLocation::Instruction)
        )
    }) {
        bail!("instruction candidate evidence is missing its semantic location");
    }
    let section_kind = one_section_kind(candidate)?;
    let has_heading = candidate
        .evidence
        .first()
        .and_then(|evidence| evidence.text.as_deref())
        .and_then(|text| text.lines().next())
        .and_then(parse_atx_heading)
        .is_some();
    if section_kind == "instruction" && !has_heading {
        return Ok(if instruction_evidence_requires_review(candidate) {
            (
                MemoryType::Procedure,
                MemoryLane::Procedural,
                MemoryDestination::NeedsReview,
                OkfProposalSensitivity::Unknown,
                "temporary_or_ambiguous_instruction_requires_review",
                "instruction_sharing_or_lifetime_is_ambiguous",
            )
        } else {
            (
                MemoryType::Procedure,
                MemoryLane::Procedural,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "explicit_instruction_file_preamble",
                "explicit_repo_instruction_file_passed_safeguards",
            )
        });
    }
    let typed_kind = section_kind.strip_prefix("instruction_");
    let (memory_type, lane, destination, sensitivity) = match typed_kind {
        Some(kind) => {
            let policy = typed_section_policy(kind)
                .context("capture instruction section kind is invalid")?;
            validate_typed_evidence_heading(candidate, kind, None)?;
            policy
        }
        None if section_kind == "instruction" => {
            validate_untyped_instruction_heading(candidate)?;
            (
                MemoryType::Procedure,
                MemoryLane::Procedural,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
            )
        }
        None => bail!("capture instruction section kind is invalid"),
    };
    if instruction_evidence_requires_review(candidate) {
        Ok((
            memory_type,
            lane,
            MemoryDestination::NeedsReview,
            OkfProposalSensitivity::Unknown,
            "temporary_or_ambiguous_instruction_requires_review",
            "instruction_sharing_or_lifetime_is_ambiguous",
        ))
    } else {
        Ok((
            memory_type,
            lane,
            destination,
            sensitivity,
            "deterministic_instruction_section",
            "explicit_repo_instruction_file_passed_safeguards",
        ))
    }
}

fn adr_candidate_policy(candidate: &CaptureCandidate) -> Result<CoreCandidatePolicy> {
    validate_project_evidence_scope(candidate)?;
    let statuses = candidate
        .evidence
        .iter()
        .filter_map(|evidence| match &evidence.semantic_location {
            Some(CaptureSemanticLocation::Adr { status, .. }) => Some(status.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if statuses.len() != 1 {
        bail!("ADR candidate evidence has inconsistent status metadata");
    }
    let status = statuses
        .first()
        .copied()
        .context("ADR candidate status evidence is missing")?;
    let field_tag = one_prefixed_tag(&candidate.memory.tags, "adr-field:")?;
    let status_tag = one_prefixed_tag(&candidate.memory.tags, "adr-status:")?;
    if status_tag != status || !candidate.memory.tags.iter().any(|tag| tag == "adr") {
        bail!("ADR candidate tags do not match evidence status");
    }
    for required_field in ["title", "status", field_tag] {
        if !candidate.evidence.iter().any(|evidence| {
            matches!(
                &evidence.semantic_location,
                Some(CaptureSemanticLocation::Adr {
                    field,
                    status: evidence_status,
                    ..
                })
                    if field == required_field && evidence_status == status
            )
        }) {
            bail!("ADR candidate is missing title, status, or field evidence");
        }
    }
    let lifecycle_targets = candidate
        .evidence
        .iter()
        .filter_map(|evidence| match &evidence.semantic_location {
            Some(CaptureSemanticLocation::Adr {
                field,
                target: Some(target),
                ..
            }) if field == "supersession" => Some(target.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if field_tag == "supersession" {
        if lifecycle_targets.len() != 1
            || lifecycle_targets.first().copied() != Some(candidate.memory.body.as_str())
        {
            bail!("ADR supersession target does not match exact lifecycle evidence");
        }
    } else if !lifecycle_targets.is_empty() {
        bail!("ADR lifecycle target appears on a non-supersession candidate");
    }
    let (memory_type, lane) = match field_tag {
        "context" | "consequences" => (MemoryType::Fact, MemoryLane::Semantic),
        "decision" | "supersession" => (MemoryType::Decision, MemoryLane::Semantic),
        "risk" => (MemoryType::Risk, MemoryLane::Semantic),
        _ => bail!("ADR candidate field is invalid"),
    };
    let status_allows_repo = status == "accepted" && field_tag != "supersession";
    Ok(if status_allows_repo {
        (
            memory_type,
            lane,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
            "accepted_adr_field",
            "explicit_repo_adr_passed_safeguards",
        )
    } else {
        (
            memory_type,
            lane,
            MemoryDestination::NeedsReview,
            OkfProposalSensitivity::Unknown,
            if field_tag == "supersession" {
                "adr_supersession_requires_lifecycle_review"
            } else {
                "adr_status_requires_review"
            },
            "adr_status_or_lifecycle_authority_is_not_repo_safe",
        )
    })
}

fn git_candidate_policy(
    request: &CaptureRequest,
    candidate: &CaptureCandidate,
) -> Result<CoreCandidatePolicy> {
    let section_kind = one_section_kind(candidate)?;
    let (memory_type, lane, _, _) =
        typed_section_policy(section_kind).context("Git candidate section kind is invalid")?;
    if !matches!(
        memory_type,
        MemoryType::Decision
            | MemoryType::Procedure
            | MemoryType::Warning
            | MemoryType::Risk
            | MemoryType::FailedAttempt
    ) {
        bail!("Git candidate memory type is not durable guidance");
    }
    let source = request
        .sources
        .first()
        .context("Git capture source is missing")?;
    let mut paths = BTreeSet::new();
    let mut sides = BTreeSet::new();
    for evidence in &candidate.evidence {
        let CaptureSemanticLocation::GitChange {
            repository,
            base,
            head,
            old_path,
            new_path,
            side,
            ..
        } = evidence
            .semantic_location
            .as_ref()
            .context("Git candidate semantic location is missing")?
        else {
            bail!("Git candidate semantic location is invalid");
        };
        match (&source.locator, &source.git) {
            (
                CaptureSourceLocator::GitRange {
                    repository: expected_repository,
                    base: expected_base,
                    head: expected_head,
                    ..
                },
                None,
            ) if repository == expected_repository
                && base == expected_base
                && head == expected_head => {}
            (_, Some(git))
                if repository == &git.repository && base == &git.base && head == &git.head => {}
            _ => bail!("Git candidate revision identity does not match its source request"),
        }
        let path = if side == "old" {
            old_path.as_deref().or(new_path.as_deref())
        } else {
            new_path.as_deref().or(old_path.as_deref())
        }
        .context("Git candidate has no evidence path")?;
        validate_git_candidate_path(path)?;
        paths.insert(path.to_owned());
        sides.insert(side.as_str());
    }
    if sides.len() != 1 {
        bail!("Git candidate evidence has inconsistent diff sides");
    }
    let side = sides
        .first()
        .copied()
        .context("Git candidate evidence side is missing")?;
    validate_typed_evidence_heading(
        candidate,
        section_kind,
        Some(match side {
            "new" => '+',
            "old" => '-',
            _ => bail!("Git candidate evidence side is invalid"),
        }),
    )?;
    if candidate.memory.scope.kind != ScopeKind::Repo
        || candidate.memory.scope.id.is_some()
        || candidate
            .memory
            .scope
            .paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != paths
    {
        bail!("Git candidate scope does not match its evidence path");
    }
    Ok(if side == "old" {
        (
            memory_type,
            lane,
            MemoryDestination::NeedsReview,
            OkfProposalSensitivity::Unknown,
            "deleted_git_guidance_requires_review",
            "deleted_git_guidance_has_no_direct_lifecycle_authority",
        )
    } else {
        (
            memory_type,
            lane,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
            "deterministic_typed_added_git_guidance",
            "explicit_repo_diff_passed_safeguards",
        )
    })
}

fn typed_section_policy(
    section_kind: &str,
) -> Option<(
    MemoryType,
    MemoryLane,
    MemoryDestination,
    OkfProposalSensitivity,
)> {
    Some(match section_kind {
        "fact" => (
            MemoryType::Fact,
            MemoryLane::Semantic,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        "preference" => (
            MemoryType::Preference,
            MemoryLane::Semantic,
            MemoryDestination::Local,
            OkfProposalSensitivity::LocalOnly,
        ),
        "decision" => (
            MemoryType::Decision,
            MemoryLane::Semantic,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        "procedure" => (
            MemoryType::Procedure,
            MemoryLane::Procedural,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        "episode" => (
            MemoryType::Episode,
            MemoryLane::Session,
            MemoryDestination::Session,
            OkfProposalSensitivity::TemporaryState,
        ),
        "warning" => (
            MemoryType::Warning,
            MemoryLane::Semantic,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        "failed_attempt" => (
            MemoryType::FailedAttempt,
            MemoryLane::Episodic,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        "risk" => (
            MemoryType::Risk,
            MemoryLane::Semantic,
            MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
        ),
        _ => return None,
    })
}

fn one_section_kind(candidate: &CaptureCandidate) -> Result<&str> {
    let kinds = candidate
        .evidence
        .iter()
        .map(|evidence| evidence.section_kind.as_str())
        .collect::<BTreeSet<_>>();
    if kinds.len() != 1 {
        bail!("capture candidate evidence has inconsistent section kinds");
    }
    kinds
        .first()
        .copied()
        .context("capture candidate evidence section kind is missing")
}

fn validate_project_evidence_scope(candidate: &CaptureCandidate) -> Result<()> {
    let paths = candidate
        .evidence
        .iter()
        .map(|evidence| {
            evidence
                .locator
                .project_path()
                .context("capture project evidence path is missing")
                .map(str::to_owned)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if candidate.memory.scope.kind != ScopeKind::Repo
        || candidate.memory.scope.id.is_some()
        || candidate
            .memory
            .scope
            .paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != paths
    {
        bail!("capture candidate scope does not match project evidence");
    }
    Ok(())
}

fn validate_instruction_evidence_scope(candidate: &CaptureCandidate) -> Result<()> {
    let paths = candidate
        .evidence
        .iter()
        .map(|evidence| {
            let path = evidence
                .locator
                .project_path()
                .context("instruction evidence path is missing")?;
            Ok(path.rsplit_once('/').map(|(parent, _)| parent.to_owned()))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if paths.len() != 1 {
        bail!("instruction candidate evidence paths are inconsistent");
    }
    let expected = paths
        .first()
        .cloned()
        .flatten()
        .into_iter()
        .collect::<BTreeSet<_>>();
    if candidate.memory.scope.kind != ScopeKind::Repo
        || candidate.memory.scope.id.is_some()
        || candidate
            .memory
            .scope
            .paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected
    {
        bail!("instruction candidate scope does not match its source directory");
    }
    Ok(())
}

fn validate_typed_evidence_heading(
    candidate: &CaptureCandidate,
    section_kind: &str,
    diff_side: Option<char>,
) -> Result<()> {
    let evidence = candidate
        .evidence
        .first()
        .context("capture typed evidence is missing")?;
    let first = evidence
        .text
        .as_deref()
        .and_then(|text| text.lines().next())
        .context("capture typed evidence heading is missing")?;
    let first = match diff_side {
        Some(side) => first
            .strip_prefix(side)
            .context("Git capture evidence must match its declared diff side")?,
        None => first,
    };
    let (_, heading) = parse_atx_heading(first).context("capture evidence heading is invalid")?;
    let (typed, title) = typed_heading(&heading);
    let Some((_, _, _, _, actual_kind)) = typed else {
        bail!("capture evidence heading is not typed");
    };
    if actual_kind != section_kind || title != candidate.memory.title {
        bail!("capture evidence heading does not match candidate memory");
    }
    Ok(())
}

fn validate_untyped_instruction_heading(candidate: &CaptureCandidate) -> Result<()> {
    let first = candidate
        .evidence
        .first()
        .and_then(|evidence| evidence.text.as_deref())
        .and_then(|text| text.lines().next())
        .context("instruction evidence heading is missing")?;
    let (_, heading) =
        parse_atx_heading(first).context("instruction evidence heading is invalid")?;
    if typed_heading(&heading).0.is_some() || heading != candidate.memory.title {
        bail!("instruction evidence heading does not match candidate memory");
    }
    Ok(())
}

fn instruction_evidence_requires_review(candidate: &CaptureCandidate) -> bool {
    candidate.evidence.iter().any(|evidence| {
        evidence
            .heading_path
            .iter()
            .any(|heading| contains_instruction_review_marker(heading))
            || evidence
                .text
                .as_deref()
                .is_some_and(|text| text.lines().any(contains_instruction_review_marker))
    })
}

fn contains_instruction_review_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let words = lower
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    INSTRUCTION_REVIEW_MARKERS
        .iter()
        .any(|marker| words.windows(marker.len()).any(|window| window == *marker))
}

fn one_prefixed_tag<'a>(tags: &'a [String], prefix: &str) -> Result<&'a str> {
    let matches = tags
        .iter()
        .filter_map(|tag| tag.strip_prefix(prefix))
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0].is_empty() {
        bail!("capture candidate typed tag is missing or duplicated");
    }
    Ok(matches[0])
}

fn validate_git_candidate_path(path: &str) -> Result<()> {
    validate_project_relative(path, "Git evidence")?;
    if adapters::git_path_exclusion_code(path).is_some() {
        bail!("Git candidate path is generated, managed, or dependency content");
    }
    Ok(())
}

fn blocked_after_planning_failure(
    request: &CaptureRequest,
    safeguards: CaptureSafeguards,
    extractor: CaptureExtractorIdentity,
    code: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<CapturePlan> {
    check_planning_control(control)?;
    blocked_plan(request, safeguards, extractor, code, None)
}

fn candidate_is_repo_safe_for_plan(candidate: &CaptureCandidate) -> bool {
    candidate.classification.sensitivity == OkfProposalSensitivity::RepoSafe
        && candidate.classification.destination == MemoryDestination::Repo
        && !action_matches_private_runtime(&candidate.action)
}

fn action_matches_private_runtime(action: &CaptureAction) -> bool {
    match action {
        CaptureAction::Duplicate { matches } | CaptureAction::Conflict { matches } => matches
            .iter()
            .any(|matching| matching.kind == CaptureMatchKind::RuntimeRecord),
        _ => false,
    }
}

fn blocked_plan(
    request: &CaptureRequest,
    safeguards: CaptureSafeguards,
    extractor: CaptureExtractorIdentity,
    code: &str,
    line: Option<u64>,
) -> Result<CapturePlan> {
    let mut plan = CapturePlan {
        schema: CAPTURE_PLAN_SCHEMA.to_owned(),
        plan_id: String::new(),
        status: CapturePlanStatus::Blocked,
        data_class: CaptureDataClass::Blocked,
        request: redacted_request(request),
        sources: Vec::new(),
        safeguards,
        preconditions: empty_preconditions(),
        extractor,
        candidates: Vec::new(),
        summary: CapturePlanSummary {
            sources: 1,
            blocked: 1,
            ..Default::default()
        },
        diagnostics: vec![CaptureDiagnostic {
            code: code.to_owned(),
            source_id: request.sources.first().map(|_| "source-1".to_owned()),
            line,
        }],
    };
    plan.plan_id = recompute_capture_plan_id(&plan)?;
    Ok(plan)
}

fn redacted_request(request: &CaptureRequest) -> CaptureRequest {
    let mut value = request.clone();
    for (index, source) in value.sources.iter_mut().enumerate() {
        source.source_id = format!("source-{}", index + 1);
        source.locator = CaptureSourceLocator::ProjectPath {
            path: "redacted.md".to_owned(),
        };
        source.media_type = "text/markdown".to_owned();
        source.git = None;
    }
    value.extractor.profile = MARKDOWN_EXTRACTOR_PROFILE.to_owned();
    value
}

pub(crate) fn recompute_capture_plan_id(plan: &CapturePlan) -> Result<String> {
    let mut value = plan.clone();
    value.plan_id.clear();
    let bytes =
        canonical_json_bytes(&value).context("failed to serialize capture plan identity")?;
    Ok(domain_id("capture", CAPTURE_PLAN_SCHEMA, &bytes))
}

pub(crate) fn validate_plan_identity(plan: &CapturePlan) -> Result<()> {
    if plan.schema != CAPTURE_PLAN_SCHEMA {
        bail!("unsupported capture plan schema");
    }
    validate_request(&plan.request)?;
    let expected = recompute_capture_plan_id(plan)?;
    if plan.plan_id != expected {
        bail!("capture plan identity mismatch");
    }
    Ok(())
}

fn recompute_capture_review_id(review: &CaptureReview) -> Result<String> {
    let mut value = review.clone();
    value.review_id.clear();
    let bytes =
        canonical_json_bytes(&value).context("failed to serialize capture review identity")?;
    Ok(domain_id("review", CAPTURE_REVIEW_SCHEMA, &bytes))
}

pub(crate) fn validate_review_identity(review: &CaptureReview) -> Result<()> {
    if review.schema != CAPTURE_REVIEW_SCHEMA {
        bail!("unsupported capture review schema");
    }
    if review.plan_id.trim().is_empty()
        || review.reviewed_by.trim().is_empty()
        || review.decisions.is_empty()
    {
        bail!("capture review is incomplete");
    }
    if let Some(prior_review_id) = review.prior_review_id.as_deref() {
        if !valid_capture_identity(prior_review_id, "review") {
            bail!("capture prior_review_id is invalid");
        }
        if prior_review_id == review.review_id {
            bail!("capture review cannot name itself as its prior review");
        }
    }
    time::OffsetDateTime::parse(
        review.reviewed_at.trim(),
        &time::format_description::well_known::Rfc3339,
    )
    .context("capture reviewed_at must be RFC 3339")?;
    let mut seen = BTreeSet::new();
    for decision in &review.decisions {
        if !seen.insert(&decision.candidate_id) {
            bail!("capture review contains a duplicate candidate decision");
        }
        validate_reason_code(decision.reason_code.as_deref())?;
        match decision.outcome {
            CaptureReviewOutcome::Accept | CaptureReviewOutcome::Edit
                if decision.reviewed_candidate.is_none() =>
            {
                bail!("accepted and edited capture decisions require a reviewed candidate")
            }
            CaptureReviewOutcome::Reject | CaptureReviewOutcome::Defer
                if decision.reviewed_candidate.is_some() =>
            {
                bail!("rejected and deferred capture decisions cannot route a candidate")
            }
            _ => {}
        }
    }
    let expected = recompute_capture_review_id(review)?;
    if review.review_id != expected {
        bail!("capture review identity mismatch");
    }
    Ok(())
}

fn validate_request(request: &CaptureRequest) -> Result<()> {
    if request.schema != CAPTURE_REQUEST_SCHEMA {
        bail!("unsupported capture request schema");
    }
    if request.sources.len() != 1 {
        bail!("capture request must name exactly one source");
    }
    let source = &request.sources[0];
    if !valid_source_id(&source.source_id) {
        bail!("capture source_id is invalid");
    }
    if prohibited_finding(source.source_id.as_bytes()).is_some() {
        bail!("capture source_id contains prohibited content");
    }
    match request.extractor.profile.as_str() {
        MARKDOWN_EXTRACTOR_PROFILE => {
            require_markdown_media(source)?;
            let CaptureSourceLocator::ProjectPath { path } = &source.locator else {
                bail!("Markdown capture requires one project_path source");
            };
            if source.git.is_some() {
                bail!("Markdown capture cannot include Git revision context");
            }
            validate_markdown_path(path)?;
        }
        INSTRUCTION_EXTRACTOR_PROFILE => {
            require_markdown_media(source)?;
            let CaptureSourceLocator::ProjectPath { path } = &source.locator else {
                bail!("instruction capture requires one project_path source");
            };
            if source.git.is_some() {
                bail!("instruction capture cannot include Git revision context");
            }
            validate_markdown_path(path)?;
            let name = Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !matches!(name, "AGENTS.md" | "CLAUDE.md") {
                bail!("instruction capture source must be AGENTS.md or CLAUDE.md");
            }
        }
        ADR_EXTRACTOR_PROFILE => {
            require_markdown_media(source)?;
            if source.git.is_some() {
                bail!("ADR capture cannot include Git revision context");
            }
            match &source.locator {
                CaptureSourceLocator::ProjectPath { path } => validate_markdown_path(path)?,
                CaptureSourceLocator::ProjectDirectory {
                    path,
                    ignore_policy,
                    include,
                    ..
                } => {
                    validate_project_relative(path, "ADR directory")?;
                    if ignore_policy != "git-v1" {
                        bail!("ADR directory capture requires ignore_policy git-v1");
                    }
                    if include.as_slice() != ["*.md"] {
                        bail!("ADR directory capture supports only the deterministic *.md include");
                    }
                }
                _ => bail!("ADR capture requires one project_path or project_directory source"),
            }
        }
        GIT_CHANGE_EXTRACTOR_PROFILE => {
            if source.media_type != "text/x-diff" {
                bail!("Git-change capture source media_type must be text/x-diff");
            }
            match &source.locator {
                CaptureSourceLocator::ProjectPath { path } => {
                    validate_project_relative(path, "Git diff source")?;
                    if !matches!(
                        Path::new(path).extension().and_then(|value| value.to_str()),
                        Some("diff" | "patch")
                    ) {
                        bail!("Git diff source must use a .diff or .patch extension");
                    }
                    validate_git_source_context(
                        source
                            .git
                            .as_ref()
                            .context("Git diff capture requires explicit revision context")?,
                    )?;
                }
                CaptureSourceLocator::SuppliedBytes {
                    display_name,
                    media_type,
                    byte_length,
                    source_content_hash,
                } => {
                    if media_type != &source.media_type {
                        bail!("supplied-bytes media_type does not match the source descriptor");
                    }
                    validate_safe_display_name(display_name)?;
                    if *byte_length > MAX_DIFF_SOURCE_BYTES {
                        bail!("supplied Git diff exceeds the configured size limit");
                    }
                    validate_content_hash(source_content_hash)?;
                    validate_git_source_context(
                        source
                            .git
                            .as_ref()
                            .context("supplied Git diff requires explicit revision context")?,
                    )?;
                }
                CaptureSourceLocator::GitRange {
                    repository,
                    base,
                    head,
                    merge_parent,
                    diff_format,
                    ..
                } => {
                    if source.git.is_some() {
                        bail!("git_range stores revision context in its locator");
                    }
                    validate_repository_identity(repository)?;
                    validate_git_object_pair(base, head, "git_range")?;
                    if !matches!(merge_parent.as_str(), "base_to_head" | "first_parent") {
                        bail!("git_range merge_parent is unsupported");
                    }
                    if diff_format != "git-unified-v1" {
                        bail!("git_range diff_format is unsupported");
                    }
                }
                CaptureSourceLocator::ProjectDirectory { .. } => {
                    bail!("Git-change capture does not accept a project directory")
                }
            }
        }
        _ => bail!("unsupported capture extractor profile"),
    }
    Ok(())
}

fn require_markdown_media(source: &CaptureSourceRequest) -> Result<()> {
    if source.media_type != "text/markdown" {
        bail!("capture source media_type must be text/markdown");
    }
    Ok(())
}

fn valid_source_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn validate_markdown_path(value: &str) -> Result<()> {
    validate_project_relative(value, "Markdown source")?;
    if Path::new(value)
        .extension()
        .and_then(|value| value.to_str())
        != Some("md")
    {
        bail!("capture source must be one Markdown file");
    }
    Ok(())
}

fn validate_project_relative(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > CAPTURE_MAX_PATH_BYTES
        || value.contains('\\')
        || value.contains('\0')
        || value.chars().any(char::is_control)
        || value.starts_with('/')
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("capture {label} path must be POSIX project-relative");
    }
    for component in Path::new(value).components() {
        let Component::Normal(component) = component else {
            bail!("capture source path contains an unsafe component");
        };
        let component = component.to_string_lossy();
        if component.eq_ignore_ascii_case(".memzoi") || component.eq_ignore_ascii_case(".git") {
            bail!("capture source cannot read managed repository state");
        }
    }
    if prohibited_finding(value.as_bytes()).is_some() {
        bail!("capture source locator contains prohibited content");
    }
    Ok(())
}

fn validate_safe_display_name(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 256
        || value.contains(['/', '\\', '\0'])
        || value.chars().any(char::is_control)
        || prohibited_finding(value.as_bytes()).is_some()
    {
        bail!("capture supplied source display_name is invalid");
    }
    Ok(())
}

fn validate_repository_identity(value: &str) -> Result<()> {
    if value != "." {
        bail!("capture Git repository identity must be the current project root");
    }
    Ok(())
}

fn validate_git_source_context(context: &CaptureGitSourceContext) -> Result<()> {
    validate_repository_identity(&context.repository)?;
    validate_git_object_pair(&context.base, &context.head, "Git diff")
}

fn validate_git_object_pair(base: &str, head: &str, label: &str) -> Result<()> {
    validate_git_object_id(base)?;
    validate_git_object_id(head)?;
    let (base_algorithm, base_digest) = base
        .split_once(':')
        .context("capture Git base object ID is malformed")?;
    let (head_algorithm, head_digest) = head
        .split_once(':')
        .context("capture Git head object ID is malformed")?;
    if base_algorithm != head_algorithm {
        bail!("{label} base and head must use the same object algorithm");
    }
    if base_digest.bytes().all(|byte| byte == b'0') || head_digest.bytes().all(|byte| byte == b'0')
    {
        bail!("{label} commit identities cannot be null object IDs");
    }
    if base == head {
        bail!("{label} base and head must differ");
    }
    Ok(())
}

fn validate_git_object_id(value: &str) -> Result<()> {
    let valid = value.strip_prefix("sha1:").is_some_and(|digest| {
        digest.len() == 40 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) || value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    if !valid || value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!("capture Git object ID must be a full lowercase algorithm-prefixed digest");
    }
    Ok(())
}

fn validate_content_hash(value: &str) -> Result<()> {
    if !value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !digest.bytes().any(|byte| byte.is_ascii_uppercase())
    }) {
        bail!("capture source content hash is invalid");
    }
    Ok(())
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

#[cfg(unix)]
fn open_capture_source(project_root: &Path, relative: &str) -> Result<File> {
    use rustix::fs::{CWD, Mode, OFlags, openat};

    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, project_root, directory_flags, Mode::empty())
        .context("failed to open capture project root without following symbolic links")?;
    let mut components = Path::new(relative).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            bail!("capture source path contains an unsafe component");
        };
        if components.peek().is_none() {
            let file = openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("failed to open capture source without following symbolic links")?;
            return Ok(File::from(file));
        }
        directory = openat(&directory, component, directory_flags, Mode::empty())
            .context("failed to open capture source path without following symbolic links")?;
    }
    bail!("capture source path is empty")
}

#[cfg(not(unix))]
fn open_capture_source(_project_root: &Path, _relative: &str) -> Result<File> {
    bail!("secure capture file access is unavailable on this platform; capture fails closed")
}

#[derive(Debug, Clone)]
struct InventoryEntry {
    kind: CaptureMatchKind,
    id: String,
    destination: MemoryDestination,
    memory_type: MemoryType,
    lane: MemoryLane,
    title: String,
    body: String,
    scope: CaptureScope,
    status: String,
    updated_at: Option<String>,
}

#[derive(Debug)]
struct CaptureInventorySnapshot {
    entries: Vec<InventoryEntry>,
    reserved_ids: BTreeSet<String>,
    runtime_available: bool,
}

fn load_inventory(
    paths: &MemoryPaths,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureInventorySnapshot> {
    let mut inventory = load_file_inventory(paths, control)?;
    let mut runtime_entries = Vec::new();
    inventory.runtime_available = paths.db_path.try_exists().unwrap_or(false)
        && load_runtime_inventory(paths, &mut runtime_entries, control).is_ok();
    if inventory.runtime_available {
        inventory.entries.extend(runtime_entries);
    }
    finish_inventory_snapshot(&mut inventory);
    Ok(inventory)
}

fn load_inventory_with_connection(
    paths: &MemoryPaths,
    conn: &Connection,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureInventorySnapshot> {
    let mut inventory = load_file_inventory(paths, control)?;
    load_runtime_inventory_from_connection(conn, &mut inventory.entries, control)?;
    inventory.runtime_available = true;
    finish_inventory_snapshot(&mut inventory);
    Ok(inventory)
}

fn finish_inventory_snapshot(inventory: &mut CaptureInventorySnapshot) {
    inventory
        .reserved_ids
        .extend(inventory.entries.iter().map(|entry| entry.id.clone()));
    sort_inventory(&mut inventory.entries);
}

fn load_file_inventory(
    paths: &MemoryPaths,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureInventorySnapshot> {
    let mut entries = Vec::new();
    let mut reserved_ids = BTreeSet::new();
    let mut budget = CaptureInventoryBudget::default();
    for record in read_bounded_okf_record_files(&paths.records_dir(), &mut budget, control)? {
        reserved_ids.insert(record.concept_id.clone());
        if record.status != MemoryStatus::Active {
            continue;
        }
        let updated_at = record
            .updated
            .clone()
            .unwrap_or_else(|| record.created.clone());
        entries.push(InventoryEntry {
            kind: CaptureMatchKind::CanonicalRecord,
            id: record.concept_id,
            destination: MemoryDestination::Repo,
            memory_type: record.draft.memory_type,
            lane: record.draft.lane,
            title: record.draft.title,
            body: record.draft.body,
            scope: CaptureScope {
                kind: record.draft.scope_kind,
                id: record.draft.scope_id,
                paths: record.applies_to,
            },
            status: record.status.as_str().to_owned(),
            updated_at: Some(updated_at),
        });
    }
    for proposal in read_bounded_okf_proposal_files(&paths.proposals_dir(), &mut budget, control)? {
        reserved_ids.insert(proposal.id.clone());
        if proposal.status == OkfProposalStatus::Proposed {
            entries.push(InventoryEntry {
                kind: CaptureMatchKind::PendingProposal,
                id: proposal.id,
                destination: MemoryDestination::Repo,
                memory_type: proposal.memory_type,
                lane: proposal.lane,
                title: proposal.title,
                body: proposal.body,
                scope: CaptureScope {
                    kind: proposal.scope_kind,
                    id: proposal.scope_id,
                    paths: proposal.applies_to,
                },
                status: proposal.status.as_str().to_owned(),
                updated_at: Some(proposal.timestamp),
            });
        }
    }
    Ok(CaptureInventorySnapshot {
        entries,
        reserved_ids,
        runtime_available: false,
    })
}

#[derive(Debug, Default)]
struct CaptureInventoryBudget {
    entries: usize,
    files: usize,
    metadata_bytes: u64,
    read_bytes: u64,
}

fn read_bounded_okf_record_files(
    root: &Path,
    budget: &mut CaptureInventoryBudget,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<OkfRecordFile>> {
    let mut files = Vec::new();
    collect_bounded_inventory_markdown(root, root, 0, budget, &mut files, control)?;
    files.sort();
    let mut records = Vec::new();
    for file in files {
        check_planning_control(control)?;
        let markdown = read_bounded_inventory_markdown(root, &file, budget, control)?;
        if let Some(record) = parse_okf_record_markdown(root, &file, &markdown)? {
            records.push(record);
        }
    }
    records.sort_by(|left, right| left.concept_id.cmp(&right.concept_id));
    Ok(records)
}

fn read_bounded_okf_proposal_files(
    root: &Path,
    budget: &mut CaptureInventoryBudget,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<OkfProposalFile>> {
    let mut files = Vec::new();
    collect_bounded_inventory_markdown(root, root, 0, budget, &mut files, control)?;
    files.sort();
    let mut proposals = Vec::new();
    for file in files {
        check_planning_control(control)?;
        let markdown = read_bounded_inventory_markdown(root, &file, budget, control)?;
        if let Some(proposal) = parse_okf_proposal_markdown(root, &file, &markdown)? {
            proposals.push(proposal);
        }
    }
    proposals.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(proposals)
}

fn collect_bounded_inventory_markdown(
    root: &Path,
    directory: &Path,
    depth: usize,
    budget: &mut CaptureInventoryBudget,
    files: &mut Vec<PathBuf>,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    check_planning_control(control)?;
    if !root
        .try_exists()
        .context("failed to inspect capture file inventory")?
    {
        return Ok(());
    }
    if depth > CAPTURE_MAX_INVENTORY_DEPTH {
        bail!("capture file inventory exceeds the directory depth limit");
    }

    let mut directory_entries = Vec::new();
    for entry in fs::read_dir(directory).context("failed to scan capture file inventory")? {
        check_planning_control(control)?;
        budget.entries = budget.entries.saturating_add(1);
        if budget.entries > CAPTURE_MAX_INVENTORY_ENTRIES {
            bail!("capture file inventory exceeds the directory entry limit");
        }
        directory_entries.push(entry?);
    }
    directory_entries.sort_by_key(|entry| entry.file_name());
    for entry in directory_entries {
        if entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let file_type = entry
            .file_type()
            .context("failed to inspect capture file inventory entry")?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_bounded_inventory_markdown(root, &path, depth + 1, budget, files, control)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
            let file_bytes = entry
                .metadata()
                .context("failed to inspect capture inventory Markdown")?
                .len();
            if file_bytes > CAPTURE_MAX_INVENTORY_FILE_BYTES {
                bail!("capture inventory Markdown exceeds the per-file byte limit");
            }
            budget.files = budget.files.saturating_add(1);
            if budget.files > CAPTURE_MAX_INVENTORY_FILES {
                bail!("capture file inventory exceeds the file-count limit");
            }
            budget.metadata_bytes = budget.metadata_bytes.saturating_add(file_bytes);
            if budget.metadata_bytes > CAPTURE_MAX_INVENTORY_BYTES {
                bail!("capture file inventory exceeds the aggregate byte limit");
            }
            files.push(path);
        }
    }
    Ok(())
}

fn read_bounded_inventory_markdown(
    root: &Path,
    path: &Path,
    budget: &mut CaptureInventoryBudget,
    control: Option<&CapturePlanningControl>,
) -> Result<String> {
    check_planning_control(control)?;
    let relative = path
        .strip_prefix(root)
        .context("capture inventory path escaped its root")?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("capture inventory path is not UTF-8"))?;
    let mut file = open_capture_source(root, relative)?;
    let metadata = file
        .metadata()
        .context("failed to inspect opened capture inventory Markdown")?;
    if !metadata.is_file() {
        bail!("capture inventory Markdown is not a regular file");
    }
    if metadata.len() > CAPTURE_MAX_INVENTORY_FILE_BYTES {
        bail!("capture inventory Markdown exceeds the per-file byte limit");
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len().min(CAPTURE_MAX_INVENTORY_FILE_BYTES)).unwrap_or(0),
    );
    file.by_ref()
        .take(CAPTURE_MAX_INVENTORY_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read capture inventory Markdown")?;
    check_planning_control(control)?;
    if bytes.len() as u64 > CAPTURE_MAX_INVENTORY_FILE_BYTES {
        bail!("capture inventory Markdown exceeds the per-file byte limit");
    }
    budget.read_bytes = budget.read_bytes.saturating_add(bytes.len() as u64);
    if budget.read_bytes > CAPTURE_MAX_INVENTORY_BYTES {
        bail!("capture file inventory exceeds the aggregate byte limit");
    }
    String::from_utf8(bytes).context("capture inventory Markdown is not valid UTF-8")
}

fn sort_inventory(entries: &mut [InventoryEntry]) {
    entries.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));
}

fn load_runtime_inventory(
    paths: &MemoryPaths,
    entries: &mut Vec<InventoryEntry>,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    check_planning_control(control)?;
    if !paths
        .db_path
        .try_exists()
        .context("failed to inspect capture runtime inventory")?
    {
        return Ok(());
    }
    let before = runtime_database_read_state(&paths.db_path)?;
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        percent_encode_sqlite_uri_path(&paths.db_path)
    );
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("failed to open capture runtime inventory read-only")?;
    load_runtime_inventory_from_connection(&conn, entries, control)?;
    check_planning_control(control)?;
    let after = runtime_database_read_state(&paths.db_path)?;
    if before != after {
        bail!("capture runtime inventory changed while it was read");
    }
    Ok(())
}

fn load_runtime_inventory_from_connection(
    conn: &Connection,
    entries: &mut Vec<InventoryEntry>,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    check_planning_control(control)?;
    let compatible = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'memory_record'
            )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect capture runtime inventory schema")?;
    if !compatible {
        bail!("capture runtime inventory schema is incompatible");
    }

    ensure_runtime_inventory_bounds(conn)?;

    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, title, body, updated_at
         FROM memory_record
         WHERE status = 'active' AND destination IN ('local', 'session')
         ORDER BY id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS as i64 + 1], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let starting_entries = entries.len();
    for row in rows {
        check_planning_control(control)?;
        if entries.len().saturating_sub(starting_entries) >= CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS {
            bail!("capture runtime inventory exceeds the record-count limit");
        }
        let (id, memory_type, lane, destination, scope_kind, scope_id, title, body, updated_at) =
            row?;
        let paths = runtime_inventory_paths(conn, &id)?;
        entries.push(InventoryEntry {
            kind: CaptureMatchKind::RuntimeRecord,
            id,
            destination: parse_stored_enum(&destination, "destination")?,
            memory_type: parse_stored_enum(&memory_type, "memory type")?,
            lane: parse_stored_enum(&lane, "memory lane")?,
            title,
            body,
            scope: CaptureScope {
                kind: parse_stored_enum(&scope_kind, "scope kind")?,
                id: scope_id,
                paths,
            },
            status: "active".to_owned(),
            updated_at: Some(updated_at),
        });
    }
    Ok(())
}

fn ensure_runtime_inventory_bounds(conn: &Connection) -> Result<()> {
    let (record_count, record_bytes) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
             length(CAST(id AS BLOB))
             + length(CAST(type AS BLOB))
             + length(CAST(lane AS BLOB))
             + length(CAST(destination AS BLOB))
             + length(CAST(scope_kind AS BLOB))
             + COALESCE(length(CAST(scope_id AS BLOB)), 0)
             + length(CAST(title AS BLOB))
             + length(CAST(body AS BLOB))
             + length(CAST(updated_at AS BLOB))
         ), 0)
         FROM memory_record
         WHERE status = 'active' AND destination IN ('local', 'session')",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    if record_count > CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS as i64 {
        bail!("capture runtime inventory exceeds the record-count limit");
    }
    if record_bytes > CAPTURE_MAX_RUNTIME_INVENTORY_BYTES as i64 {
        bail!("capture runtime inventory exceeds the aggregate byte limit");
    }

    let (path_count, path_bytes) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(memory_path.path AS BLOB))), 0)
         FROM memory_path
         JOIN memory_record ON memory_record.id = memory_path.record_id
         WHERE memory_record.status = 'active'
           AND memory_record.destination IN ('local', 'session')",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let max_path_count = (CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS as i64)
        .saturating_mul(CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD as i64);
    if path_count > max_path_count {
        bail!("capture runtime inventory exceeds the path-count limit");
    }
    if record_bytes.saturating_add(path_bytes) > CAPTURE_MAX_RUNTIME_INVENTORY_BYTES as i64 {
        bail!("capture runtime inventory exceeds the aggregate byte limit");
    }
    Ok(())
}

fn runtime_inventory_paths(conn: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM memory_path
         WHERE record_id = ?1
         ORDER BY path ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![record_id, CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD as i64 + 1],
        |row| row.get::<_, String>(0),
    )?;
    let paths = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if paths.len() > CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD {
        bail!("capture runtime inventory exceeds the per-record path limit");
    }
    Ok(paths)
}

fn parse_stored_enum<T>(value: &str, label: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .with_context(|| format!("capture runtime inventory has invalid {label}"))
}

fn percent_encode_sqlite_uri_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeDatabaseReadState {
    database: (u64, Option<std::time::SystemTime>),
    wal: Option<(u64, Option<std::time::SystemTime>)>,
    wal_index_header: Option<Vec<u8>>,
}

fn runtime_database_read_state(path: &Path) -> Result<RuntimeDatabaseReadState> {
    let database = capture_file_metadata(path)?;
    let wal_path = sqlite_sidecar_path(path, "-wal");
    let wal = optional_capture_file_metadata(&wal_path)?;
    let wal_index_header = if wal.is_some_and(|(length, _)| length > 32) {
        let shm_path = sqlite_sidecar_path(path, "-shm");
        let mut file = File::open(&shm_path)
            .context("capture runtime inventory WAL is not safely checkpointed")?;
        let mut header = vec![0_u8; 100];
        file.read_exact(&mut header)
            .context("capture runtime inventory WAL index is incomplete")?;
        let first_initialized = header[12] == 1;
        let second_initialized = header[60] == 1;
        let first_max_frame = native_u32(&header[16..20]);
        let second_max_frame = native_u32(&header[64..68]);
        let backfilled_frames = native_u32(&header[96..100]);
        if !first_initialized
            || !second_initialized
            || first_max_frame != second_max_frame
            || backfilled_frames < first_max_frame
        {
            bail!(
                "capture runtime inventory has uncheckpointed changes; retry after the writer exits"
            );
        }
        Some(header)
    } else {
        None
    };
    Ok(RuntimeDatabaseReadState {
        database,
        wal,
        wal_index_header,
    })
}

fn capture_file_metadata(path: &Path) -> Result<(u64, Option<std::time::SystemTime>)> {
    let metadata =
        fs::metadata(path).context("failed to inspect capture runtime inventory file")?;
    Ok((metadata.len(), metadata.modified().ok()))
}

fn optional_capture_file_metadata(
    path: &Path,
) -> Result<Option<(u64, Option<std::time::SystemTime>)>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.len(), metadata.modified().ok()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to inspect capture runtime inventory file"),
    }
}

fn native_u32(bytes: &[u8]) -> u32 {
    u32::from_ne_bytes(
        bytes
            .try_into()
            .expect("capture WAL index slice is four bytes"),
    )
}

#[derive(Debug, Clone)]
struct MarkdownSection {
    start: usize,
    end: usize,
    direct_end: usize,
    line: u64,
    kind: Option<(
        MemoryType,
        MemoryLane,
        MemoryDestination,
        OkfProposalSensitivity,
        &'static str,
    )>,
    title: String,
    heading_path: Vec<String>,
}

struct CaptureExtraction {
    candidates: Vec<CaptureCandidate>,
    diagnostics: Vec<CaptureDiagnostic>,
}

fn extract_candidates(
    source: &CaptureSourceRequest,
    snapshot: &CaptureSourceSnapshot,
    text: &str,
    extractor: &CaptureExtractorIdentity,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureExtraction> {
    check_planning_control(control)?;
    let sections = markdown_sections(text, control)?;
    let recognized = sections
        .iter()
        .filter(|section| section.kind.is_some())
        .count();
    if recognized > CAPTURE_MAX_CANDIDATES {
        bail!("capture source exceeds the configured candidate limit");
    }
    if recognized == 0 {
        let evidence = evidence_for(
            source,
            snapshot,
            text,
            EvidenceLocation {
                start: 0,
                end: text.len(),
                line_start: 1,
                section_kind: "unclassified",
                heading_path: &[],
            },
        )?;
        let title =
            first_heading(text).unwrap_or_else(|| "Unclassified Markdown source".to_owned());
        return Ok(CaptureExtraction {
            candidates: vec![candidate(
                CaptureMemoryDraft {
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    title,
                    body: text.trim().to_owned(),
                    scope: default_scope(source),
                    tags: Vec::new(),
                },
                evidence,
                extractor,
                MemoryDestination::NeedsReview,
                OkfProposalSensitivity::Unknown,
                crate::RepositoryContentClass::Unknown,
                "unrecognized_markdown_requires_review",
                "unrecognized_markdown_sensitivity_unknown",
            )?],
            diagnostics: Vec::new(),
        });
    }
    let diagnostics = unsupported_markdown_diagnostics(source, text, &sections, control)?;
    let mut candidates = Vec::new();
    for section in sections
        .into_iter()
        .filter(|section| section.kind.is_some())
    {
        check_planning_control(control)?;
        let (memory_type, lane, destination, sensitivity, section_kind) = section.kind.unwrap();
        let body_start = text[section.start..section.end]
            .find('\n')
            .map(|offset| section.start + offset + 1)
            .unwrap_or(section.end);
        let body = text[body_start..section.end].trim();
        if section.title.trim().is_empty() || body.is_empty() {
            continue;
        }
        let evidence = evidence_for(
            source,
            snapshot,
            text,
            EvidenceLocation {
                start: section.start,
                end: section.end,
                line_start: section.line,
                section_kind,
                heading_path: &section.heading_path,
            },
        )?;
        let (destination, sensitivity, destination_reason, sensitivity_reason) =
            if destination == MemoryDestination::Repo {
                (
                    MemoryDestination::NeedsReview,
                    OkfProposalSensitivity::Unknown,
                    "generic_markdown_requires_contextual_review",
                    "generic_markdown_sensitivity_unknown",
                )
            } else {
                (
                    destination,
                    sensitivity,
                    "deterministic_typed_markdown_section",
                    "deterministic_typed_markdown_profile",
                )
            };
        candidates.push(candidate(
            CaptureMemoryDraft {
                memory_type,
                lane,
                title: section.title.trim().to_owned(),
                body: body.to_owned(),
                scope: default_scope(source),
                tags: Vec::new(),
            },
            evidence,
            extractor,
            destination,
            sensitivity,
            capture_content_class(destination, sensitivity),
            destination_reason,
            sensitivity_reason,
        )?);
    }
    if candidates.is_empty() {
        bail!("capture source has typed headings but no non-empty typed sections");
    }
    let evidence_bytes = candidates
        .iter()
        .flat_map(|candidate| &candidate.evidence)
        .map(|evidence| evidence.span.byte_end - evidence.span.byte_start)
        .sum::<u64>();
    if evidence_bytes > CAPTURE_MAX_EVIDENCE_BYTES as u64 {
        bail!("capture source exceeds the configured evidence limit");
    }
    Ok(CaptureExtraction {
        candidates,
        diagnostics,
    })
}

fn unsupported_markdown_diagnostics(
    source: &CaptureSourceRequest,
    text: &str,
    sections: &[MarkdownSection],
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<CaptureDiagnostic>> {
    let mut diagnostics = Vec::new();
    let preamble_end = sections
        .first()
        .map(|section| section.start)
        .unwrap_or(text.len());
    if !text[..preamble_end].trim().is_empty() {
        diagnostics.push(CaptureDiagnostic {
            code: "unsupported_markdown_content".to_owned(),
            source_id: Some(source.source_id.clone()),
            line: Some(1),
        });
    }
    for (index, section) in sections.iter().enumerate() {
        if index % 128 == 0 {
            check_planning_control(control)?;
        }
        if section.kind.is_some() {
            continue;
        }
        let body_start = text[section.start..section.direct_end]
            .find('\n')
            .map(|offset| section.start + offset + 1)
            .unwrap_or(section.direct_end);
        let has_direct_content = !text[body_start..section.direct_end].trim().is_empty();
        let has_descendant = sections
            .get(index + 1..)
            .unwrap_or_default()
            .iter()
            .take_while(|candidate| candidate.start < section.end)
            .next()
            .is_some();
        if has_direct_content || !has_descendant {
            diagnostics.push(CaptureDiagnostic {
                code: "unsupported_markdown_content".to_owned(),
                source_id: Some(source.source_id.clone()),
                line: Some(section.line),
            });
        }
    }
    Ok(diagnostics)
}

fn markdown_sections(
    text: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<MarkdownSection>> {
    let mut headings: Vec<(usize, u64, usize, String)> = Vec::new();
    let mut offset = 0usize;
    let mut line_number = 1u64;
    let mut fence: Option<(char, usize)> = None;
    for line in text.split_inclusive('\n') {
        if line_number % 128 == 1 {
            check_planning_control(control)?;
        }
        let logical = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or(line.strip_suffix('\n').unwrap_or(line));
        let trimmed = logical.trim_start();
        let marker_char = trimmed.chars().next().filter(|ch| *ch == '`' || *ch == '~');
        if let Some(ch) = marker_char {
            let count = trimmed
                .chars()
                .take_while(|candidate| *candidate == ch)
                .count();
            if count >= 3 {
                match fence {
                    None => fence = Some((ch, count)),
                    Some((open, minimum)) if open == ch && count >= minimum => fence = None,
                    _ => {}
                }
                offset += line.len();
                line_number += 1;
                continue;
            }
        }
        if fence.is_none()
            && let Some((level, heading)) = parse_atx_heading(logical)
        {
            headings.push((offset, line_number, level, heading));
            if headings.len() > CAPTURE_MAX_MARKDOWN_HEADINGS {
                bail!("capture source exceeds the configured Markdown heading limit");
            }
        }
        offset += line.len();
        line_number += 1;
    }
    let mut ends = vec![text.len(); headings.len()];
    let mut open = Vec::<(usize, usize)>::new();
    for (index, (start, _, level, _)) in headings.iter().enumerate() {
        while open
            .last()
            .is_some_and(|(_, open_level)| open_level >= level)
        {
            let (open_index, _) = open.pop().expect("checked non-empty heading stack");
            ends[open_index] = *start;
        }
        open.push((index, *level));
    }
    let mut sections = Vec::with_capacity(headings.len());
    let mut heading_stack = Vec::<(usize, String)>::new();
    for (index, (start, line, level, heading)) in headings.iter().enumerate() {
        while heading_stack
            .last()
            .is_some_and(|(ancestor_level, _)| ancestor_level >= level)
        {
            heading_stack.pop();
        }
        let mut heading_path = heading_stack
            .iter()
            .map(|(_, title)| title.clone())
            .collect::<Vec<_>>();
        heading_path.push(heading.clone());
        let (kind, title) = typed_heading(heading);
        sections.push(MarkdownSection {
            start: *start,
            end: ends[index],
            direct_end: headings
                .get(index + 1)
                .map(|(start, _, _, _)| *start)
                .unwrap_or(text.len()),
            line: *line,
            kind,
            title,
            heading_path,
        });
        heading_stack.push((*level, heading.clone()));
    }
    Ok(sections)
}

fn parse_atx_heading(line: &str) -> Option<(usize, String)> {
    let line = line.trim_start();
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let heading = line[level + 1..]
        .trim()
        .trim_end_matches('#')
        .trim()
        .to_owned();
    Some((level, heading))
}

type TypedHeading = (
    MemoryType,
    MemoryLane,
    MemoryDestination,
    OkfProposalSensitivity,
    &'static str,
);

fn typed_heading(heading: &str) -> (Option<TypedHeading>, String) {
    let mappings: [(&str, TypedHeading); 8] = [
        (
            "Fact:",
            (
                MemoryType::Fact,
                MemoryLane::Semantic,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "fact",
            ),
        ),
        (
            "Preference:",
            (
                MemoryType::Preference,
                MemoryLane::Semantic,
                MemoryDestination::Local,
                OkfProposalSensitivity::LocalOnly,
                "preference",
            ),
        ),
        (
            "Decision:",
            (
                MemoryType::Decision,
                MemoryLane::Semantic,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "decision",
            ),
        ),
        (
            "Procedure:",
            (
                MemoryType::Procedure,
                MemoryLane::Procedural,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "procedure",
            ),
        ),
        (
            "Episode:",
            (
                MemoryType::Episode,
                MemoryLane::Session,
                MemoryDestination::Session,
                OkfProposalSensitivity::TemporaryState,
                "episode",
            ),
        ),
        (
            "Warning:",
            (
                MemoryType::Warning,
                MemoryLane::Semantic,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "warning",
            ),
        ),
        (
            "Failed attempt:",
            (
                MemoryType::FailedAttempt,
                MemoryLane::Episodic,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "failed_attempt",
            ),
        ),
        (
            "Risk:",
            (
                MemoryType::Risk,
                MemoryLane::Semantic,
                MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                "risk",
            ),
        ),
    ];
    for (prefix, kind) in mappings {
        if let Some(title) = heading.strip_prefix(prefix) {
            return (Some(kind), title.trim().to_owned());
        }
    }
    (None, heading.to_owned())
}

fn first_heading(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| parse_atx_heading(line).map(|(_, heading)| heading))
}

struct EvidenceLocation<'a> {
    start: usize,
    end: usize,
    line_start: u64,
    section_kind: &'a str,
    heading_path: &'a [String],
}

fn evidence_for(
    source: &CaptureSourceRequest,
    snapshot: &CaptureSourceSnapshot,
    text: &str,
    location: EvidenceLocation<'_>,
) -> Result<Vec<CaptureEvidence>> {
    let EvidenceLocation {
        start,
        end,
        line_start,
        section_kind,
        heading_path,
    } = location;
    if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        bail!("capture evidence span is invalid");
    }
    if end - start > CAPTURE_MAX_EVIDENCE_ITEM_BYTES {
        bail!("capture evidence item exceeds the configured size limit");
    }
    let excerpt = &text[start..end];
    let line_end = line_start
        + excerpt.as_bytes()[..excerpt.len() - 1]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64;
    Ok(vec![CaptureEvidence {
        source_id: source.source_id.clone(),
        locator: source.locator.clone(),
        source_content_hash: snapshot.source_content_hash.clone(),
        span: CaptureEvidenceSpan {
            byte_start: start as u64,
            byte_end: end as u64,
            line_start,
            line_end,
        },
        evidence_content_hash: content_hash(excerpt.as_bytes()),
        text: Some(excerpt.to_owned()),
        heading_path: heading_path.to_vec(),
        section_kind: section_kind.to_owned(),
        semantic_location: None,
    }])
}

fn default_scope(source: &CaptureSourceRequest) -> CaptureScope {
    CaptureScope {
        kind: ScopeKind::Repo,
        id: None,
        paths: source
            .locator
            .project_path()
            .map(str::to_owned)
            .into_iter()
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    memory: CaptureMemoryDraft,
    evidence: Vec<CaptureEvidence>,
    extractor: &CaptureExtractorIdentity,
    destination: MemoryDestination,
    sensitivity: OkfProposalSensitivity,
    content_class: crate::RepositoryContentClass,
    destination_reason: &str,
    sensitivity_reason: &str,
) -> Result<CaptureCandidate> {
    let claim_payload = canonical_json_bytes(&(&memory, &evidence, extractor))?;
    let claim_id = domain_id("claim", "memzoi/capture-claim-v1", &claim_payload);
    let classification = CaptureClassification {
        destination,
        destination_reason: destination_reason.to_owned(),
        sensitivity,
        sensitivity_reason: sensitivity_reason.to_owned(),
        content_class,
        policy: destination.policy(),
    };
    let action = preliminary_action(&classification);
    let candidate_payload = canonical_json_bytes(&(&claim_id, 1.0f64, &classification, &action))?;
    let candidate_id = domain_id(
        "candidate",
        "memzoi/capture-candidate-v1",
        &candidate_payload,
    );
    Ok(CaptureCandidate {
        claim_id,
        candidate_id,
        memory,
        evidence,
        extraction: extractor.clone(),
        confidence: 1.0,
        classification,
        action,
    })
}

fn rebuild_edited_candidate(
    inventory: &CaptureInventorySnapshot,
    reserved_proposal_ids: &mut BTreeSet<String>,
    original: &CaptureCandidate,
    memory: &CaptureMemoryDraft,
    destination: MemoryDestination,
    content_class: Option<crate::RepositoryContentClass>,
) -> Result<CaptureCandidate> {
    let memory = normalize_reviewed_memory(memory)?;
    let mut scan = Vec::with_capacity(memory.title.len() + memory.body.len() + 1);
    scan.extend_from_slice(memory.title.as_bytes());
    scan.push(b'\n');
    scan.extend_from_slice(memory.body.as_bytes());
    if prohibited_finding(&scan).is_some() {
        bail!("reviewed capture edit contains prohibited content");
    }

    let (sensitivity, destination_reason, sensitivity_reason) = match destination {
        MemoryDestination::Repo => {
            if content_class != Some(crate::RepositoryContentClass::GeneralRepoKnowledge) {
                bail!(
                    "reviewed capture edits routed to repo require explicit general_repo_knowledge classification"
                );
            }
            if original.evidence.iter().any(|evidence| {
                matches!(
                    &evidence.semantic_location,
                    Some(CaptureSemanticLocation::Adr { field, .. })
                        if field == "supersession"
                )
            }) {
                bail!(
                    "ADR supersession candidates require an explicit lifecycle proposal and cannot be converted to a create proposal"
                );
            }
            if !matches!(
                original.classification.sensitivity,
                OkfProposalSensitivity::RepoSafe | OkfProposalSensitivity::Unknown
            ) {
                bail!(
                    "reviewed capture evidence is not classified repo-safe and cannot be routed to repo"
                );
            }
            (
                OkfProposalSensitivity::RepoSafe,
                "reviewer_requested_repo_after_safeguards",
                if original.classification.sensitivity == OkfProposalSensitivity::RepoSafe {
                    "retained_evidence_was_already_repo_safe"
                } else {
                    "retained_evidence_passed_prohibited_content_safeguards"
                },
            )
        }
        MemoryDestination::Local => (
            OkfProposalSensitivity::LocalOnly,
            "reviewer_requested_private_local_route",
            "reviewed_content_is_local_only",
        ),
        MemoryDestination::Session => (
            OkfProposalSensitivity::TemporaryState,
            "reviewer_requested_private_session_route",
            "reviewed_content_is_temporary_state",
        ),
        MemoryDestination::Discard => (
            original.classification.sensitivity,
            "reviewer_requested_discard",
            "review_preserved_original_sensitivity",
        ),
        MemoryDestination::NeedsReview => {
            bail!("edit decisions must request repo, local, session, or discard")
        }
    };
    let mut reviewed = candidate(
        memory,
        original.evidence.clone(),
        &original.extraction,
        destination,
        sensitivity,
        content_class.unwrap_or_else(|| capture_content_class(destination, sensitivity)),
        destination_reason,
        sensitivity_reason,
    )?;
    classify_actions_with_reserved(
        &inventory.entries,
        std::slice::from_mut(&mut reviewed),
        reserved_proposal_ids,
        None,
        inventory.runtime_available,
    )?;
    match (&reviewed.action, destination) {
        (CaptureAction::CreateProposal { .. }, MemoryDestination::Repo)
        | (CaptureAction::CreateRuntime { .. }, MemoryDestination::Local)
        | (CaptureAction::CreateRuntime { .. }, MemoryDestination::Session)
        | (CaptureAction::NoWrite { .. }, MemoryDestination::Discard) => Ok(reviewed),
        (CaptureAction::Duplicate { .. }, _) => {
            bail!("reviewed capture edit duplicates existing memory")
        }
        (CaptureAction::Conflict { .. }, _) => {
            bail!("reviewed capture edit conflicts with existing memory")
        }
        _ => bail!("reviewed capture edit did not produce the requested safe route"),
    }
}

fn capture_content_class(
    destination: MemoryDestination,
    sensitivity: OkfProposalSensitivity,
) -> crate::RepositoryContentClass {
    use crate::RepositoryContentClass;

    match sensitivity {
        OkfProposalSensitivity::RawTranscript => RepositoryContentClass::RawTranscript,
        OkfProposalSensitivity::PrivatePersonalData => RepositoryContentClass::PrivatePersonalData,
        OkfProposalSensitivity::TemporaryState => RepositoryContentClass::TemporaryTaskState,
        OkfProposalSensitivity::LocalOnly => RepositoryContentClass::LocalOnlyState,
        OkfProposalSensitivity::RepoSafe if destination == MemoryDestination::Repo => {
            RepositoryContentClass::GeneralRepoKnowledge
        }
        _ => RepositoryContentClass::Unknown,
    }
}

fn normalize_reviewed_memory(memory: &CaptureMemoryDraft) -> Result<CaptureMemoryDraft> {
    let mut value = memory.clone();
    value.title = value.title.trim().to_owned();
    value.body = value.body.trim().to_owned();
    if value.title.is_empty() || value.body.is_empty() {
        bail!("reviewed capture memory title and body are required");
    }
    if value.title.chars().any(char::is_control) {
        bail!("reviewed capture memory title contains control characters");
    }
    if let Some(id) = value.scope.id.as_mut() {
        *id = id.trim().to_owned();
        if id.is_empty() || id.chars().any(char::is_control) {
            bail!("reviewed capture scope id is invalid");
        }
        if prohibited_finding(id.as_bytes()).is_some() {
            bail!("reviewed capture edit contains prohibited content");
        }
    }
    for path in &mut value.scope.paths {
        *path = path.trim().to_owned();
        validate_review_scope_path(path)?;
        if prohibited_finding(path.as_bytes()).is_some() {
            bail!("reviewed capture edit contains prohibited content");
        }
    }
    value.scope.paths.sort();
    value.scope.paths.dedup();
    for tag in &mut value.tags {
        *tag = tag.trim().to_owned();
        if tag.is_empty() || tag.chars().any(char::is_control) {
            bail!("reviewed capture tag is invalid");
        }
        if prohibited_finding(tag.as_bytes()).is_some() {
            bail!("reviewed capture edit contains prohibited content");
        }
    }
    value.tags.sort();
    value.tags.dedup();
    Ok(value)
}

fn validate_review_scope_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > CAPTURE_MAX_PATH_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains('\0')
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("reviewed capture scope path must be project-relative");
    }
    if !Path::new(value)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        bail!("reviewed capture scope path contains an unsafe component");
    }
    Ok(())
}

fn preliminary_action(classification: &CaptureClassification) -> CaptureAction {
    match classification.policy.write_route {
        MemoryWriteRoute::FileBackedProposal => CaptureAction::CreateProposal {
            proposal_id: String::new(),
            path: String::new(),
        },
        MemoryWriteRoute::RuntimeLocal | MemoryWriteRoute::RuntimeSession => {
            CaptureAction::CreateRuntime {
                route: classification.policy.write_route,
            }
        }
        MemoryWriteRoute::NoWrite => CaptureAction::NoWrite {
            reason_code: "needs_review".to_owned(),
        },
    }
}

fn classify_actions(
    inventory: &CaptureInventorySnapshot,
    candidates: &mut [CaptureCandidate],
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    let mut used_ids = inventory.reserved_ids.clone();
    classify_actions_with_reserved(
        &inventory.entries,
        candidates,
        &mut used_ids,
        control,
        inventory.runtime_available,
    )
}

fn apply_runtime_inventory_availability(runtime_available: bool, candidate: &mut CaptureCandidate) {
    if runtime_available {
        return;
    }
    if matches!(candidate.action, CaptureAction::CreateRuntime { .. }) {
        candidate.classification.destination = MemoryDestination::NeedsReview;
        candidate.classification.destination_reason =
            "runtime_inventory_unavailable_requires_review".to_owned();
        candidate.classification.policy = MemoryDestination::NeedsReview.policy();
        candidate.action = CaptureAction::NoWrite {
            reason_code: "runtime_inventory_unavailable".to_owned(),
        };
    }
}

fn classify_actions_with_reserved(
    inventory: &[InventoryEntry],
    candidates: &mut [CaptureCandidate],
    used_ids: &mut BTreeSet<String>,
    control: Option<&CapturePlanningControl>,
    runtime_available: bool,
) -> Result<()> {
    let mut earlier = Vec::<InventoryEntry>::new();
    for candidate in candidates {
        check_planning_control(control)?;
        let exact_key = draft_key(&candidate.memory)?;
        let candidate_conflict_key = conflict_key(&candidate.memory);
        let mut duplicates = Vec::new();
        let mut conflicts = Vec::new();
        for (index, entry) in inventory.iter().chain(earlier.iter()).enumerate() {
            if index % 128 == 0 {
                check_planning_control(control)?;
            }
            let entry_memory = CaptureMemoryDraft {
                memory_type: entry.memory_type,
                lane: entry.lane,
                title: entry.title.clone(),
                body: entry.body.clone(),
                scope: entry.scope.clone(),
                tags: Vec::new(),
            };
            let entry_exact = draft_key(&entry_memory)?;
            let matching = CaptureMatch {
                kind: entry.kind,
                id: entry.id.clone(),
                destination: Some(entry.destination),
                content_hash: entry_exact.clone(),
                status: entry.status.clone(),
                updated_at: entry.updated_at.clone(),
            };
            if entry_exact == exact_key {
                duplicates.push(matching);
            } else if conflict_key(&entry_memory) == candidate_conflict_key {
                conflicts.push(matching);
            }
        }
        duplicates.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));
        conflicts.sort_by(|left, right| (left.kind, &left.id).cmp(&(right.kind, &right.id)));
        candidate.action = if !duplicates.is_empty() {
            CaptureAction::Duplicate {
                matches: duplicates,
            }
        } else if !conflicts.is_empty() {
            candidate.classification.destination = MemoryDestination::NeedsReview;
            candidate.classification.destination_reason =
                "possible_conflict_requires_lifecycle_resolution".to_owned();
            candidate.classification.policy = MemoryDestination::NeedsReview.policy();
            CaptureAction::Conflict { matches: conflicts }
        } else {
            match candidate.classification.policy.write_route {
                MemoryWriteRoute::FileBackedProposal => {
                    let proposal_id = reserve_proposal_id(&candidate.memory.title, used_ids);
                    CaptureAction::CreateProposal {
                        path: format!(".memzoi/proposals/pending/{proposal_id}.md"),
                        proposal_id,
                    }
                }
                MemoryWriteRoute::RuntimeLocal | MemoryWriteRoute::RuntimeSession => {
                    CaptureAction::CreateRuntime {
                        route: candidate.classification.policy.write_route,
                    }
                }
                MemoryWriteRoute::NoWrite => CaptureAction::NoWrite {
                    reason_code: "needs_review".to_owned(),
                },
            }
        };
        apply_runtime_inventory_availability(runtime_available, candidate);
        let payload = canonical_json_bytes(&(
            &candidate.claim_id,
            candidate.confidence,
            &candidate.classification,
            &candidate.action,
        ))?;
        candidate.candidate_id = domain_id("candidate", "memzoi/capture-candidate-v1", &payload);
        earlier.push(InventoryEntry {
            kind: CaptureMatchKind::EarlierCandidate,
            id: candidate.candidate_id.clone(),
            destination: candidate.classification.destination,
            memory_type: candidate.memory.memory_type,
            lane: candidate.memory.lane,
            title: candidate.memory.title.clone(),
            body: candidate.memory.body.clone(),
            scope: candidate.memory.scope.clone(),
            status: "planned".to_owned(),
            updated_at: None,
        });
    }
    Ok(())
}

fn candidate_preconditions(candidates: &[CaptureCandidate]) -> CapturePreconditions {
    let mut map = BTreeMap::new();
    for candidate in candidates {
        let (duplicates, conflicts) = match &candidate.action {
            CaptureAction::Duplicate { matches } => (matches.as_slice(), &[][..]),
            CaptureAction::Conflict { matches } => (&[][..], matches.as_slice()),
            _ => (&[][..], &[][..]),
        };
        let relevant_record_hashes = duplicates
            .iter()
            .chain(conflicts.iter())
            .map(|matching| CaptureRelevantRecord {
                kind: matching.kind,
                id: matching.id.clone(),
                content_hash: matching.content_hash.clone(),
                status: matching.status.clone(),
                updated_at: matching.updated_at.clone(),
            })
            .collect();
        let reserved_proposal_id = match &candidate.action {
            CaptureAction::CreateProposal { proposal_id, .. } => Some(proposal_id.clone()),
            _ => None,
        };
        map.insert(
            candidate.candidate_id.clone(),
            CaptureCandidatePrecondition {
                duplicate_match_set_hash: match_set_hash("duplicate", duplicates),
                conflict_match_set_hash: match_set_hash("conflict", conflicts),
                reserved_proposal_id,
                relevant_record_hashes,
            },
        );
    }
    CapturePreconditions {
        policy_version: "memzoi/destination-policy-v1".to_owned(),
        candidates: map,
    }
}

fn empty_preconditions() -> CapturePreconditions {
    CapturePreconditions {
        policy_version: "memzoi/destination-policy-v1".to_owned(),
        candidates: BTreeMap::new(),
    }
}

fn summarize(candidates: &[CaptureCandidate]) -> CapturePlanSummary {
    let mut summary = CapturePlanSummary {
        sources: 1,
        candidates: candidates.len(),
        ..Default::default()
    };
    for candidate in candidates {
        match candidate.action {
            CaptureAction::CreateProposal { .. } => summary.create_proposals += 1,
            CaptureAction::CreateRuntime { .. } => summary.runtime_writes += 1,
            CaptureAction::Duplicate { .. } => summary.duplicates += 1,
            CaptureAction::Conflict { .. } => {
                summary.conflicts += 1;
                summary.needs_review += 1;
            }
            CaptureAction::NoWrite { .. } => summary.needs_review += 1,
            CaptureAction::Blocked { .. } => summary.blocked += 1,
        }
    }
    summary
}

fn supported_extractor_profile(profile: &str) -> bool {
    matches!(
        profile,
        MARKDOWN_EXTRACTOR_PROFILE
            | INSTRUCTION_EXTRACTOR_PROFILE
            | ADR_EXTRACTOR_PROFILE
            | GIT_CHANGE_EXTRACTOR_PROFILE
    )
}

fn extractor_identity(profile: &str) -> Result<CaptureExtractorIdentity> {
    let (id, version, configuration) = match profile {
        MARKDOWN_EXTRACTOR_PROFILE => (
            "memzoi-markdown",
            MARKDOWN_EXTRACTOR_VERSION,
            b"memzoi/markdown-deterministic-v1\0typed-atx-sections\0unsupported-regions=diagnostic-v1"
                .as_slice(),
        ),
        INSTRUCTION_EXTRACTOR_PROFILE => (
            "memzoi-instructions",
            INSTRUCTION_EXTRACTOR_VERSION,
            b"memzoi/instruction-deterministic-v1\0structured-sections\0generated-blocks=exclude-v1\0temporary-heading-preamble-body=needs-review-v3"
                .as_slice(),
        ),
        ADR_EXTRACTOR_PROFILE => (
            "memzoi-adr",
            ADR_EXTRACTOR_VERSION,
            b"memzoi/adr-deterministic-v1\0status-context-decision-consequences-supersession\0title-status-field-target-evidence=v2\0conflicting-metadata=fail-safe-v1\0directory-order=path-v1"
                .as_slice(),
        ),
        GIT_CHANGE_EXTRACTOR_PROFILE => (
            "memzoi-git-change",
            GIT_CHANGE_EXTRACTOR_VERSION,
            b"memzoi/git-change-deterministic-v1\0unified-diff-v1\0strict-headers-and-blobs=v2\0typed-durable-guidance-v1\0heading-boundary=all-v1\0path-policy=git-durable-v1\0files=512\0hunks=4096\0rename=exact-blob-v1\0mode-authority=regular-blob-evidence-v1"
                .as_slice(),
        ),
        _ => bail!("unsupported capture extractor profile"),
    };
    Ok(CaptureExtractorIdentity {
        kind: "deterministic".to_owned(),
        id: id.to_owned(),
        version: version.to_owned(),
        configuration_hash: content_hash(configuration),
    })
}

fn safeguards(profile: &str) -> Result<CaptureSafeguards> {
    if !supported_extractor_profile(profile) {
        bail!("unsupported capture extractor profile");
    }
    let git_change = profile == GIT_CHANGE_EXTRACTOR_PROFILE;
    let adr_directory = profile == ADR_EXTRACTOR_PROFILE;
    Ok(CaptureSafeguards {
        policy_version: "memzoi/capture-safeguards-v1".to_owned(),
        configuration_hash: if adr_directory {
            content_hash(
                b"memzoi/capture-safeguards-v1\0prohibited-detectors=6\0source-hard-links=reject\0max-source=1048576\0max-aggregate-source=4194304\0max-directory-files=128\0max-directory-depth=8\0directory-ignore=git-v1+ignore-0.4.28\0policy-prohibited-scan=raw-v1\0max-path=4096\0max-candidates=100\0max-headings=4096\0max-evidence-item=16384\0max-evidence=262144\0max-inventory-files=10000\0max-inventory-entries=20000\0max-inventory-depth=16\0max-inventory-file=2097152\0max-inventory-bytes=33554432\0max-runtime-inventory-records=10000\0max-runtime-inventory-bytes=33554432\0max-runtime-paths-per-record=256\0max-serialized-plan=2093056\0max-serialized-review=2093056",
            )
        } else if git_change {
            content_hash(
                b"memzoi/capture-safeguards-v1\0prohibited-detectors=6\0source-hard-links=reject\0max-source=2097152\0max-changed-files=512\0max-diff-hunks=4096\0max-policy-file=65536\0max-policy-bytes=262144\0policy-prohibited-scan=raw-v1\0adapter-timeout-ms=60000\0gitignore-engine=ignore-0.4.28\0git-no-lazy-fetch=1\0git-repository-identity=filesystem-v1\0git-local-config=memzoi/git-local-config-v1+no-includes+no-worktree-config\0git-hermetic-env=env-clear+explicit-path+nonexistent-home-xdg-tmp+trace-disabled-v1\0git-renderer-min=2.43\0git-renderer-options=order-null+inter-hunk-0+prefix-a-b+indicators-v1\0git-attributes=head-tree-only-v1\0git-control-files=stable-nlink1+per-child-revalidation-v1\0git-quote-path=true\0git-regular-blob-evidence=100644+100755-v1\0max-path=4096\0max-candidates=100\0max-headings=4096\0max-evidence-item=16384\0max-evidence=262144\0max-inventory-files=10000\0max-inventory-entries=20000\0max-inventory-depth=16\0max-inventory-file=2097152\0max-inventory-bytes=33554432\0max-runtime-inventory-records=10000\0max-runtime-inventory-bytes=33554432\0max-runtime-paths-per-record=256\0max-serialized-plan=2093056\0max-serialized-review=2093056",
            )
        } else {
            content_hash(
                b"memzoi/capture-safeguards-v1\0prohibited-detectors=6\0source-hard-links=reject\0max-source=1048576\0max-path=4096\0max-candidates=100\0max-headings=4096\0max-evidence-item=16384\0max-evidence=262144\0max-inventory-files=10000\0max-inventory-entries=20000\0max-inventory-depth=16\0max-inventory-file=2097152\0max-inventory-bytes=33554432\0max-runtime-inventory-records=10000\0max-runtime-inventory-bytes=33554432\0max-runtime-paths-per-record=256\0max-serialized-plan=2093056\0max-serialized-review=2093056",
            )
        },
        max_source_bytes: if git_change {
            MAX_DIFF_SOURCE_BYTES
        } else {
            MAX_MARKDOWN_SOURCE_BYTES
        },
        max_aggregate_source_bytes: adr_directory.then_some(CAPTURE_MAX_AGGREGATE_SOURCE_BYTES),
        max_directory_files: adr_directory.then_some(CAPTURE_MAX_DIRECTORY_FILES),
        max_directory_depth: adr_directory.then_some(CAPTURE_MAX_DIRECTORY_DEPTH),
        max_changed_files: git_change.then_some(CAPTURE_MAX_GIT_CHANGED_FILES),
        max_diff_hunks: git_change.then_some(CAPTURE_MAX_GIT_DIFF_HUNKS),
        max_source_policy_file_bytes: git_change.then_some(CAPTURE_MAX_GIT_POLICY_FILE_BYTES),
        max_source_policy_bytes: git_change.then_some(CAPTURE_MAX_GIT_POLICY_BYTES),
        adapter_timeout_millis: git_change.then_some(CAPTURE_GIT_PROCESS_TIMEOUT_MILLIS),
        max_path_bytes: CAPTURE_MAX_PATH_BYTES,
        max_candidates: CAPTURE_MAX_CANDIDATES,
        max_markdown_headings: CAPTURE_MAX_MARKDOWN_HEADINGS,
        max_evidence_item_bytes: CAPTURE_MAX_EVIDENCE_ITEM_BYTES,
        max_evidence_bytes: CAPTURE_MAX_EVIDENCE_BYTES,
        max_inventory_files: CAPTURE_MAX_INVENTORY_FILES,
        max_inventory_entries: CAPTURE_MAX_INVENTORY_ENTRIES,
        max_inventory_depth: CAPTURE_MAX_INVENTORY_DEPTH,
        max_inventory_file_bytes: CAPTURE_MAX_INVENTORY_FILE_BYTES,
        max_inventory_bytes: CAPTURE_MAX_INVENTORY_BYTES,
        max_runtime_inventory_records: CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS,
        max_runtime_inventory_bytes: CAPTURE_MAX_RUNTIME_INVENTORY_BYTES,
        max_runtime_paths_per_record: CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD,
        max_serialized_plan_bytes: CAPTURE_MAX_SERIALIZED_PLAN_BYTES,
        max_serialized_review_bytes: CAPTURE_MAX_SERIALIZED_REVIEW_BYTES,
    })
}

fn prohibited_finding(bytes: &[u8]) -> Option<(String, Option<u64>)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut transcript_roles = 0usize;
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("<!-- memzoi:start -->") || lower.contains("<!-- memzoi:end -->") {
            return Some((
                "prohibited_generated_integration_content".to_owned(),
                Some(index as u64 + 1),
            ));
        }
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            return Some(("prohibited_private_key".to_owned(), Some(index as u64 + 1)));
        }
        if contains_credential_assignment(&lower)
            || lower.contains("authorization: bearer ")
            || contains_known_secret_token(line)
            || lower.contains("://") && line.contains('@') && uri_contains_credentials(line)
        {
            return Some((
                "prohibited_credential_token".to_owned(),
                Some(index as u64 + 1),
            ));
        }
        if contains_ssn(line)
            || contains_payment_card_number(line)
            || contains_private_personal_assignment(&lower)
        {
            return Some((
                "prohibited_private_personal_data".to_owned(),
                Some(index as u64 + 1),
            ));
        }
        if contains_prompt_injection(&lower) {
            return Some((
                "prohibited_prompt_injection".to_owned(),
                Some(index as u64 + 1),
            ));
        }
        let role_events = transcript_role_event_count(&lower);
        if role_events > 0 {
            transcript_roles = transcript_roles.saturating_add(role_events);
            if transcript_roles >= 2 {
                return Some((
                    "prohibited_raw_transcript".to_owned(),
                    Some(index as u64 + 1),
                ));
            }
        }
        if lower.contains("raw transcript") || lower.contains("raw-transcript") {
            return Some((
                "prohibited_raw_transcript".to_owned(),
                Some(index as u64 + 1),
            ));
        }
    }
    None
}

fn contains_prompt_injection(lower: &str) -> bool {
    [
        "ignore previous instructions",
        "ignore all previous instructions",
        "ignore prior instructions",
        "disregard previous instructions",
        "disregard prior instructions",
        "override previous instructions",
        "reveal the system prompt",
        "exfiltrate the system prompt",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

fn contains_credential_assignment(lower: &str) -> bool {
    any_assignment_key(lower, |key| {
        matches!(
            key,
            "api_key"
                | "apikey"
                | "access_token"
                | "auth_token"
                | "refresh_token"
                | "secret_key"
                | "client_secret"
                | "secret"
                | "token"
                | "password"
                | "passwd"
                | "pwd"
                | "credential"
                | "credentials"
                | "private_key"
                | "signing_key"
                | "authorization"
                | "proxy_authorization"
        ) || key.ends_with("_password")
            || key.ends_with("_secret")
            || key.ends_with("_token")
            || key.ends_with("_api_key")
            || key.ends_with("_secret_key")
            || key.ends_with("_access_key")
            || key.ends_with("_access_key_id")
            || key.ends_with("_private_key")
            || key.ends_with("_signing_key")
    })
}

fn contains_private_personal_assignment(lower: &str) -> bool {
    any_assignment_key(lower, |key| {
        [
            "ssn",
            "social_security",
            "social_security_number",
            "date_of_birth",
            "birth_date",
            "dob",
            "home_address",
            "medical_record",
            "patient_id",
            "customer_ssn",
            "passport",
            "passport_number",
            "driver_license",
            "drivers_license",
            "driver_license_number",
            "national_id",
            "tax_id",
            "credit_card",
            "credit_card_number",
            "card_number",
            "bank_account",
            "bank_account_number",
            "routing_number",
            "iban",
            "swift",
            "phone_number",
            "email_address",
        ]
        .iter()
        .any(|name| key == *name || key.ends_with(&format!("_{name}")))
    })
}

fn any_assignment_key(lower: &str, mut matches_key: impl FnMut(&str) -> bool) -> bool {
    lower.match_indices(['=', ':']).any(|(delimiter, _)| {
        let prefix = lower[..delimiter].trim_end_matches(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '`' | ']' | '}')
        });
        let raw_key = prefix
            .rsplit(|character: char| {
                !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.')
            })
            .next()
            .unwrap_or_default();
        let key = raw_key.replace(['-', '.'], "_");
        !key.is_empty() && matches_key(&key)
    })
}

fn contains_ssn(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.windows(11).any(|window| {
        window[0..3].iter().all(u8::is_ascii_digit)
            && window[3] == b'-'
            && window[4..6].iter().all(u8::is_ascii_digit)
            && window[6] == b'-'
            && window[7..11].iter().all(u8::is_ascii_digit)
    })
}

fn contains_payment_card_number(line: &str) -> bool {
    let mut digits = Vec::new();
    let mut in_candidate = false;
    for character in line.chars().chain(std::iter::once('\0')) {
        if character.is_ascii_digit() {
            digits.push((character as u8) - b'0');
            in_candidate = true;
        } else if in_candidate && matches!(character, ' ' | '-') {
            continue;
        } else if in_candidate {
            if valid_payment_card_digits(&digits) {
                return true;
            }
            digits.clear();
            in_candidate = false;
        }
    }
    false
}

fn valid_payment_card_digits(digits: &[u8]) -> bool {
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|digit| *digit == digits[0]) {
        return false;
    }
    let parity = digits.len() % 2;
    let sum = digits
        .iter()
        .enumerate()
        .map(|(index, digit)| {
            let mut value = u32::from(*digit);
            if index % 2 == parity {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            value
        })
        .sum::<u32>();
    sum % 10 == 0
}

fn contains_known_secret_token(line: &str) -> bool {
    let tokens = line.split(|character: char| {
        character.is_whitespace() || matches!(character, '"' | '\'' | '`' | ',' | ';' | '=' | ':')
    });
    tokens.into_iter().any(|token| {
        (token.starts_with("AKIA") && token.len() == 20)
            || [
                "ghp_",
                "gho_",
                "ghu_",
                "ghs_",
                "github_pat_",
                "xoxb-",
                "xoxp-",
            ]
            .iter()
            .any(|prefix| token.starts_with(prefix) && token.len() > prefix.len() + 8)
            || token.starts_with("sk-") && token.len() > 16
            || token.starts_with("eyJ") && token.matches('.').count() == 2 && token.len() > 40
    })
}

fn transcript_role_event_count(lower: &str) -> usize {
    const ROLES: [&str; 7] = [
        "user",
        "assistant",
        "human",
        "system",
        "developer",
        "tool",
        "function",
    ];
    let plain_labels = ROLES
        .iter()
        .map(|role| {
            lower
                .match_indices(&format!("{role}:"))
                .filter(|(offset, _)| {
                    *offset == 0
                        || lower[..*offset]
                            .chars()
                            .next_back()
                            .is_some_and(|character| {
                                character.is_whitespace()
                                    || matches!(
                                        character,
                                        '>' | '-' | '*' | '#' | '[' | '{' | '(' | ',' | ';' | '|'
                                    )
                            })
                })
                .count()
        })
        .sum::<usize>();
    let compact = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let serialized_roles = ROLES
        .iter()
        .map(|role| {
            compact.matches(&format!("\"role\":\"{role}\"")).count()
                + compact.matches(&format!("'role':'{role}'")).count()
                + compact.matches(&format!("role:{role}")).count()
                + compact.matches(&format!("role:\"{role}\"")).count()
                + compact.matches(&format!("role:'{role}'")).count()
                + compact.matches(&format!("role={role}")).count()
                + compact.matches(&format!("role=\"{role}\"")).count()
                + compact.matches(&format!("role='{role}'")).count()
        })
        .sum::<usize>();
    plain_labels.saturating_add(serialized_roles)
}

fn uri_contains_credentials(line: &str) -> bool {
    line.match_indices("://").any(|(separator, _)| {
        let authority = line[separator + 3..]
            .split(|character: char| {
                character.is_whitespace()
                    || matches!(
                        character,
                        '/' | '?'
                            | '#'
                            | '"'
                            | '\''
                            | '`'
                            | '<'
                            | '>'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | ','
                            | ';'
                    )
            })
            .next()
            .unwrap_or_default();
        authority
            .rsplit_once('@')
            .is_some_and(|(userinfo, host)| !host.is_empty() && userinfo.contains(':'))
    })
}

fn draft_key(memory: &CaptureMemoryDraft) -> Result<String> {
    let normalized = (
        memory.memory_type,
        memory.lane,
        memory.title.trim().to_lowercase(),
        memory.body.trim(),
        &memory.scope,
    );
    Ok(domain_hash(
        "memzoi/capture-exact-draft-v1",
        &canonical_json_bytes(&normalized)?,
    ))
}

fn conflict_key(memory: &CaptureMemoryDraft) -> (MemoryType, String, ScopeKind, Option<String>) {
    (
        memory.memory_type,
        memory.title.trim().to_lowercase(),
        memory.scope.kind,
        memory.scope.id.clone(),
    )
}

fn reserve_proposal_id(title: &str, used: &mut BTreeSet<String>) -> String {
    let mut slug = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').chars().take(48).collect();
    if slug.is_empty() {
        slug = "memory".to_owned();
    }
    let base = format!("mem_capture_{slug}");
    for suffix in 1.. {
        let candidate = if suffix == 1 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn match_set_hash(label: &str, matches: &[CaptureMatch]) -> String {
    let bytes = canonical_json_bytes(matches).expect("capture matches are serializable");
    domain_hash(&format!("memzoi/capture-{label}-match-set-v1"), &bytes)
}

fn canonical_json_bytes<T>(value: &T) -> Result<Vec<u8>>
where
    T: ?Sized + Serialize,
{
    serde_json_canonicalizer::to_vec(&value).context("failed to canonicalize capture JSON")
}

fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn domain_hash(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(bytes);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn domain_id(prefix: &str, domain: &str, bytes: &[u8]) -> String {
    let digest = domain_hash(domain, bytes);
    format!("{prefix}_{}", digest.trim_start_matches("blake3:"))
}

pub(crate) fn store_capture_provenance(
    conn: &Connection,
    record_id: &str,
    provenance: Option<&CaptureProvenance>,
) -> Result<()> {
    match provenance {
        Some(provenance) => {
            validate_capture_provenance(provenance)?;
            let json = serde_json::to_string(provenance)
                .context("failed to serialize capture provenance")?;
            conn.execute(
                "INSERT INTO memory_capture(record_id, provenance_json) VALUES (?1, ?2)
                 ON CONFLICT(record_id) DO UPDATE SET provenance_json = excluded.provenance_json",
                rusqlite::params![record_id, json],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM memory_capture WHERE record_id = ?1",
                [record_id],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn load_capture_provenance(
    conn: &Connection,
    record_id: &str,
) -> Result<Option<CaptureProvenance>> {
    let json = conn
        .query_row(
            "SELECT provenance_json FROM memory_capture WHERE record_id = ?1",
            [record_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    json.map(|json| {
        let provenance: CaptureProvenance =
            serde_json::from_str(&json).context("failed to parse stored capture provenance")?;
        validate_capture_provenance(&provenance)?;
        Ok(provenance)
    })
    .transpose()
}

fn validate_capture_provenance(provenance: &CaptureProvenance) -> Result<()> {
    if provenance.schema != CAPTURE_PROVENANCE_SCHEMA
        || provenance.plan_id.trim().is_empty()
        || provenance.review_id.trim().is_empty()
        || provenance.claim_id.trim().is_empty()
        || provenance.reviewed_claim_id.trim().is_empty()
        || provenance.candidate_id.trim().is_empty()
        || provenance.reviewed_candidate_id.trim().is_empty()
        || provenance.reviewed_by.trim().is_empty()
        || provenance.reviewed_at.trim().is_empty()
        || provenance.routed_by.trim().is_empty()
        || provenance.evidence.is_empty()
    {
        bail!("capture provenance is incomplete");
    }
    let confidence = provenance
        .confidence
        .parse::<f64>()
        .context("capture provenance confidence is invalid")?;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        bail!("capture provenance confidence is outside 0..=1");
    }
    Ok(())
}
