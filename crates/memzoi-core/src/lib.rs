mod config;
mod context;
mod db;
mod error;
mod events;
mod exporters;
mod handoff;
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
pub use handoff::HandoffInput;
pub use models::{
    ContextPack, ContextPackBudget, ContextPackIncludedItem, ContextPackOmittedItem,
    ContextPackPolicy, ContextPackWarning, HandoffPack, MemoryCitation, MemoryDestination,
    MemoryDestinationClassification, MemoryEvent, MemoryLane, MemoryPath, MemoryProposal,
    MemoryRecord, MemoryStatus, MemoryType, PrecheckWarning, ProposalInboxSummary, ScopeKind,
    SearchRanking, SearchRankingSignals, SearchResult, Visibility,
};
pub use okf::{
    OkfProposalAction, OkfProposalApplyResult, OkfProposalFile, OkfProposalMetadata,
    OkfProposalSensitivity, OkfProposalSource, OkfProposalStatus, OkfRecordFile,
    apply_okf_create_proposal_file, import_okf_records, parse_okf_proposal_file,
    parse_okf_proposal_markdown, parse_okf_record_file, parse_okf_record_markdown,
    read_okf_proposal_files, read_okf_record_files, write_memory_record_file,
    write_memory_record_file_with_metadata, write_memory_record_file_with_tags,
};
pub use precheck::PrecheckInput;
pub use proposals::{
    MemoryDraft, Proposal, ProposalStatus, ProposalStatusFilter, SupersedeResult, ValidationIssue,
    ValidationResult,
};
pub use search::SearchInput;
pub use service::{
    CheckpointInput, ExportFormat, ExportInput, ExportResult, InitBundleResult, InitRequest,
    InitResult, LocalMemoryInput, MemoryService, ProposalApprovalOverride, ProposeOptions,
    ProposeResult, RebuildResult, init_bundle,
};
pub use session_end::{
    SessionEndCandidate, SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument,
    SessionEndResult, SessionEndScope, SessionEndWrite, parse_session_end_document,
};
