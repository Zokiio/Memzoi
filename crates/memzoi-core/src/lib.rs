mod config;
mod context;
mod db;
mod error;
mod events;
mod expiry;
mod exporters;
mod handoff;
mod import;
mod memory_policy;
mod models;
mod okf;
mod precheck;
mod proposals;
mod schema;
mod search;
mod service;
mod session_end;

#[cfg(test)]
pub(crate) use db::{init_database, open_database};

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
    OkfProposalResolution, OkfProposalSensitivity, OkfProposalSource, OkfProposalStatus,
    OkfRecordFile, import_okf_records, parse_okf_proposal_file, parse_okf_proposal_markdown,
    parse_okf_record_file, parse_okf_record_markdown, read_okf_proposal_files,
    read_okf_record_files,
};
pub use precheck::PrecheckInput;
pub use proposals::{
    MemoryDraft, Proposal, ProposalStatus, ProposalStatusFilter, SupersedeResult, ValidationIssue,
    ValidationResult,
};
pub use search::SearchInput;
pub use service::{
    CheckpointInput, ExportFormat, ExportInput, ExportResult, FileProposalResolutionResult,
    InitBundleResult, InitRequest, InitResult, LocalMemoryInput, MemoryService,
    ProposalApprovalOverride, ProposeOptions, ProposeResult, RebuildResult, RepoIndexDrift,
    init_bundle,
};
pub use session_end::{
    SessionEndCandidate, SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument,
    SessionEndResult, SessionEndScope, SessionEndWrite, parse_session_end_document,
};
