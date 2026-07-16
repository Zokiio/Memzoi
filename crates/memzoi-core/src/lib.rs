mod capture;
mod capture_eval;
mod config;
mod context;
mod db;
mod error;
mod events;
mod expiry;
mod exporters;
mod git_repository;
mod handoff;
mod import;
mod memory_policy;
mod models;
mod okf;
mod precheck;
mod proposals;
mod recall_candidate_eval;
mod recall_competitor_eval;
mod recall_development;
mod recall_eval;
mod recall_eval_v3;
mod recall_operational_eval;
mod repository_io;
mod repository_write_safety;
mod schema;
mod search;
mod service;
mod session_end;

#[cfg(test)]
pub(crate) use db::{init_database, open_database};

pub use capture::{
    ADR_EXTRACTOR_PROFILE, ADR_EXTRACTOR_VERSION, CAPTURE_APPLY_RESULT_SCHEMA,
    CAPTURE_MAX_AGGREGATE_SOURCE_BYTES, CAPTURE_MAX_CANDIDATES, CAPTURE_MAX_DIRECTORY_DEPTH,
    CAPTURE_MAX_DIRECTORY_FILES, CAPTURE_MAX_EVIDENCE_BYTES, CAPTURE_MAX_EVIDENCE_ITEM_BYTES,
    CAPTURE_MAX_INVENTORY_BYTES, CAPTURE_MAX_INVENTORY_DEPTH, CAPTURE_MAX_INVENTORY_ENTRIES,
    CAPTURE_MAX_INVENTORY_FILE_BYTES, CAPTURE_MAX_INVENTORY_FILES, CAPTURE_MAX_MARKDOWN_HEADINGS,
    CAPTURE_MAX_PATH_BYTES, CAPTURE_MAX_RUNTIME_INVENTORY_BYTES,
    CAPTURE_MAX_RUNTIME_INVENTORY_RECORDS, CAPTURE_MAX_RUNTIME_PATHS_PER_RECORD,
    CAPTURE_MAX_SERIALIZED_PLAN_BYTES, CAPTURE_MAX_SERIALIZED_REVIEW_BYTES,
    CAPTURE_MAX_SOURCE_BYTES, CAPTURE_PLAN_SCHEMA, CAPTURE_PROVENANCE_SCHEMA,
    CAPTURE_REQUEST_SCHEMA, CAPTURE_REVIEW_INPUT_SCHEMA, CAPTURE_REVIEW_SCHEMA, CaptureAction,
    CaptureApplyResult, CaptureCandidate, CaptureCandidatePreconditions, CaptureClassification,
    CaptureDataClass, CaptureDiagnostic, CaptureEvidence, CaptureEvidenceSpan,
    CaptureExtractorIdentity, CaptureExtractorRequest, CaptureGitSourceContext, CaptureMatch,
    CaptureMatchKind, CaptureMemoryDraft, CaptureMemoryScope, CapturePlan, CapturePlanStatus,
    CapturePlanSummary, CapturePlanningControl, CapturePolicyInputSnapshot, CaptureProvenance,
    CaptureRequest, CaptureReview, CaptureReviewDecision, CaptureReviewDecisionInput,
    CaptureReviewInput, CaptureReviewOutcome, CaptureSafeguards, CaptureSemanticLocation,
    CaptureSourceInputs, CaptureSourceLocator, CaptureSourceMemberSnapshot, CaptureSourceRequest,
    CaptureSourceSnapshot, CaptureWrite, GIT_CHANGE_EXTRACTOR_PROFILE,
    GIT_CHANGE_EXTRACTOR_VERSION, INSTRUCTION_EXTRACTOR_PROFILE, INSTRUCTION_EXTRACTOR_VERSION,
    MARKDOWN_EXTRACTOR_PROFILE, MAX_DIFF_SOURCE_BYTES, build_capture_review,
    build_capture_review_with_inputs, build_capture_review_with_prior,
    build_capture_review_with_prior_and_inputs, parse_capture_plan, parse_capture_request,
    parse_capture_review, parse_capture_review_input, plan_capture, plan_capture_with_control,
    plan_capture_with_inputs, plan_capture_with_inputs_and_control,
};
pub use capture_eval::*;
pub use config::{
    ConfigSource, ConfigSources, EffectiveConfig, MemoryPaths, ProposalApprovalPolicy,
    WorkflowConfig, discover_paths, load_effective_config, runtime_home, user_config_path,
};
pub use context::ContextPackInput;
pub use error::{CoreError, Result};
pub use expiry::{Clock, ExpiryDiagnostic, FixedClock, SystemClock};
pub use handoff::HandoffInput;
pub use import::{
    ImportApplyResult, ImportCandidate, ImportCandidateAction, ImportCandidateInput,
    ImportDocument, ImportDuplicate, ImportDuplicateKind, ImportPlan, ImportPlanCandidate,
    ImportPlanSummary, ImportScope, ImportWrite, parse_import_document,
};
pub use memory_policy::{
    MemoryDestinationPolicy, MemoryPlane, MemoryReviewRequirement, MemoryWriteRoute,
    RepoMemoryExclusion, TWO_PLANE_MEMORY_POLICY, TwoPlaneMemoryPolicy,
};
pub use models::{
    ContextPack, ContextPackBudget, ContextPackIncludedItem, ContextPackOmittedItem,
    ContextPackPolicy, ContextPackWarning, HandoffPack, MemoryCitation, MemoryDestination,
    MemoryDestinationClassification, MemoryEvent, MemoryLane, MemoryPath, MemoryProposal,
    MemoryRecord, MemoryStatus, MemoryType, PrecheckWarning, ProposalInboxSummary, ScopeKind,
    SearchRanking, SearchRankingSignals, SearchResult, Visibility,
};
pub use okf::{
    OkfProposalAction, OkfProposalFile, OkfProposalMetadata, OkfProposalOutcome,
    OkfProposalPreflight, OkfProposalResolution, OkfProposalSensitivity, OkfProposalSource,
    OkfProposalStatus, OkfRecordFile, import_okf_records, okf_proposal_matches_identity,
    parse_okf_proposal_file, parse_okf_proposal_markdown, parse_okf_record_file,
    parse_okf_record_markdown, preflight_okf_proposal_file, preflight_okf_proposal_markdown,
    read_okf_proposal_files, read_okf_record_files, redacted_okf_proposal_path,
};
pub use precheck::PrecheckInput;
pub use proposals::{
    MemoryDraft, Proposal, ProposalStatus, ProposalStatusFilter, SupersedeResult, ValidationIssue,
    ValidationResult,
};
pub use recall_candidate_eval::*;
pub use recall_competitor_eval::*;
pub use recall_development::*;
pub use recall_eval::{
    RECALL_EVAL_BASELINE_VERSION, RECALL_EVAL_CORPUS_VERSION,
    RECALL_EVAL_METRIC_DEFINITIONS_VERSION, RECALL_EVAL_REPORT_VERSION, RecallEvalBaseline,
    RecallEvalBaselineCase, RecallEvalBaselineComparison, RecallEvalBaselineCorpus,
    RecallEvalBaselineMetrics, RecallEvalBaselineStatus, RecallEvalCase, RecallEvalCaseReport,
    RecallEvalCorpus, RecallEvalCorpusMetadata, RecallEvalForbiddenIds, RecallEvalIntegrityMetric,
    RecallEvalLatencyMetrics, RecallEvalLeakageMetric, RecallEvalLeakageMetrics,
    RecallEvalMetricDefinitions, RecallEvalMetricDelta, RecallEvalMetrics,
    RecallEvalPrecheckMetrics, RecallEvalProposalFixture, RecallEvalProposalFixtureReport,
    RecallEvalRatioMetric, RecallEvalReport, RecallEvalRetrievedRecord, RecallEvalRuntimeFixture,
    RecallEvalRuntimeMetadata, RecallEvalSearchMetrics, RecallEvalSurface,
    RecallEvalThresholdResults, RecallEvalThresholds, RecallEvalTokenUsageMetrics,
    attach_recall_eval_baseline, run_recall_eval, write_recall_eval_baseline,
};
pub use recall_eval_v3::*;
pub use recall_operational_eval::*;
pub use repository_write_safety::{
    AuthorizationProof, AuthorizedRepositoryWriteBatch, FreshnessCheck, ProvenanceAssessment,
    REPOSITORY_WRITE_DETECTOR_POLICY_VERSION, REPOSITORY_WRITE_MAX_BLOB_BYTES,
    REPOSITORY_WRITE_SAFETY_SCHEMA, REPOSITORY_WRITE_SAFETY_VERSION, RepositoryContentClass,
    RepositoryProjection, RepositoryProjectionPurpose, RepositoryScope, RepositoryWriteBlocked,
    RepositoryWriteDecision, RepositoryWriteRequest, RepositoryWriteRoute,
    RepositoryWriteSafetyAssessment, RepositoryWriteSafetyFinding, RepositoryWriteSafetyReasonCode,
    RepositoryWriteSafetyReport, SafetyField, SafetyFieldKind, SafetyFieldLocation,
    assess_repository_candidate, authorize_repository_write, scan_managed_repository_blob,
    scan_repository_blob,
};
pub use search::SearchInput;
pub use service::{
    CheckpointInput, ExportFormat, ExportInput, ExportResult, FileProposalInventory,
    FileProposalInventoryEntry, FileProposalInventoryError, FileProposalResolutionResult,
    InitBundleResult, InitRequest, InitResult, LocalMemoryInput, MemoryService,
    ProposalApprovalOverride, ProposeOptions, ProposeResult, RebuildResult, RepoIndexDrift,
    init_bundle, lifecycle_transaction_artifact_count, scan_file_proposal_inventory,
};
pub use session_end::{
    SessionEndCandidate, SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument,
    SessionEndResult, SessionEndScope, SessionEndWrite, parse_session_end_document,
};
