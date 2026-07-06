mod config;
mod context;
mod db;
mod error;
mod events;
mod exporters;
mod models;
mod okf;
mod precheck;
mod proposals;
mod schema;
mod search;
mod service;

#[cfg(test)]
pub(crate) use db::{init_database, open_database};

pub use config::{
    ConfigSource, ConfigSources, EffectiveConfig, MemoryPaths, ProposalApprovalPolicy,
    WorkflowConfig, discover_paths, load_effective_config, runtime_home, user_config_path,
};
pub use context::ContextPackInput;
pub use error::{CoreError, Result};
pub use models::{
    ContextPack, MemoryCitation, MemoryEvent, MemoryLane, MemoryPath, MemoryProposal, MemoryRecord,
    MemoryStatus, MemoryType, PrecheckWarning, ScopeKind, SearchResult, Visibility,
};
pub use okf::{
    OkfProposalAction, OkfProposalFile, OkfProposalMetadata, OkfProposalSensitivity,
    OkfProposalSource, OkfProposalStatus, OkfRecordFile, import_okf_records,
    parse_okf_proposal_file, parse_okf_proposal_markdown, parse_okf_record_file,
    parse_okf_record_markdown, read_okf_proposal_files, read_okf_record_files,
    write_memory_record_file, write_memory_record_file_with_metadata,
    write_memory_record_file_with_tags,
};
pub use precheck::PrecheckInput;
pub use proposals::{
    MemoryDraft, Proposal, ProposalStatus, ProposalStatusFilter, SupersedeResult, ValidationIssue,
    ValidationResult,
};
pub use search::SearchInput;
pub use service::{
    ExportFormat, ExportInput, ExportResult, InitBundleResult, InitRequest, InitResult,
    MemoryService, ProposalApprovalOverride, ProposeOptions, ProposeResult, RebuildResult,
    init_bundle,
};
