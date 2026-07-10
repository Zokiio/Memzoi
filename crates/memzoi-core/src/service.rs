use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    fs::OpenOptions,
    io::ErrorKind,
    io::Write,
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, OffsetDateTime, Time, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::import::{self, ExistingDuplicate};
use crate::{
    ContextPack, ContextPackInput, HandoffInput, HandoffPack, ImportApplyResult, ImportDocument,
    ImportPlan, MemoryDestination, MemoryDraft, MemoryEvent, MemoryLane, MemoryPaths, MemoryRecord,
    MemoryStatus, MemoryType, OkfProposalAction, OkfProposalFile, OkfProposalOutcome,
    OkfProposalResolution, PrecheckInput, PrecheckWarning, Proposal, ProposalStatus,
    ProposalStatusFilter, ScopeKind, SearchInput, SearchResult, SupersedeResult, ValidationResult,
    Visibility,
};
use crate::{
    config::{
        ProposalApprovalPolicy, discover_existing_paths, discover_paths, load_effective_config,
    },
    context, db,
    events::{AppendEvent, append_event, for_each_event as stream_events},
    expiry::{self, Clock, ExpiryDiagnostic, SystemClock},
    exporters, handoff, okf, precheck, proposals, search,
    session_end::{
        SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument, SessionEndResult,
        SessionEndWrite, repo_sensitivity_block_reason, session_end_proposal_draft,
        validate_session_end_document,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRequest {
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitResult {
    pub paths: MemoryPaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitBundleResult {
    pub project_root: PathBuf,
    pub memory_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub exports_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Okf,
    AgentsMd,
    ClaudeMd,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Okf => "okf",
            Self::AgentsMd => "agents-md",
            Self::ClaudeMd => "claude-md",
        }
    }
}

impl FromStr for ExportFormat {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "okf" => Ok(Self::Okf),
            "agents-md" => Ok(Self::AgentsMd),
            "claude-md" => Ok(Self::ClaudeMd),
            _ => bail!("invalid export format: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportInput {
    pub format: ExportFormat,
    pub scope_kind: ScopeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    pub format: ExportFormat,
    pub written_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    pub records_root: PathBuf,
    pub db_path: PathBuf,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIndexDrift {
    pub missing_from_index: Vec<String>,
    pub stale_in_index: Vec<String>,
    pub changed_in_index: Vec<String>,
    pub fts_out_of_sync: bool,
}

impl RepoIndexDrift {
    pub fn is_current(&self) -> bool {
        self.missing_from_index.is_empty()
            && self.stale_in_index.is_empty()
            && self.changed_in_index.is_empty()
            && !self.fts_out_of_sync
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMemoryInput {
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInput {
    pub task: String,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalApprovalOverride {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposeOptions {
    pub approval_override: Option<ProposalApprovalOverride>,
    pub apply: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeResult {
    pub proposal: Proposal,
    pub record: Option<MemoryRecord>,
    pub validation: Option<ValidationResult>,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileProposalResolutionResult {
    pub proposal: OkfProposalFile,
    pub resolution: OkfProposalResolution,
    pub resolved_path: PathBuf,
    pub record: Option<MemoryRecord>,
    pub record_path: Option<PathBuf>,
    pub already_resolved: bool,
    pub runtime_index_updated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProposalInventoryEntry {
    pub proposal: OkfProposalFile,
    pub display_path: PathBuf,
    actual_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProposalInventoryError {
    pub display_path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileProposalInventory {
    pub pending: Vec<FileProposalInventoryEntry>,
    pub resolved: Vec<FileProposalInventoryEntry>,
    pub errors: Vec<FileProposalInventoryError>,
}

pub struct MemoryService {
    paths: MemoryPaths,
    conn: Connection,
    clock: Arc<dyn Clock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileWriteMode {
    CreateNew,
    Overwrite,
}

#[derive(Debug)]
struct FileProposalApplyPlan {
    writes: Vec<CanonicalFileWrite>,
    record: MemoryRecord,
    record_path: PathBuf,
    target_id: Option<String>,
}

#[derive(Debug)]
struct CanonicalFileWrite {
    record_file: okf::OkfRecordFile,
    path: PathBuf,
    markdown: String,
    mode: FileWriteMode,
    expected_existing_hash: Option<String>,
}

#[derive(Debug)]
struct StagedCanonicalFileWrite {
    path: PathBuf,
    temp_path: PathBuf,
    backup_path: Option<PathBuf>,
    mode: FileWriteMode,
    expected_existing_hash: Option<String>,
    installed: bool,
}

#[derive(Debug)]
struct PendingFileProposalSnapshot {
    proposal: OkfProposalFile,
    expected_hash: String,
    display_path: PathBuf,
}

struct RepoLifecycleLock {
    _file: fs::File,
}

impl MemoryService {
    pub fn open(start: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_clock(start, SystemClock)
    }

    pub fn open_with_clock(start: impl AsRef<Path>, clock: impl Clock + 'static) -> Result<Self> {
        let paths = discover_existing_paths(start)?;
        Self::open_paths_with_clock(paths, clock)
    }

    pub fn open_paths(paths: MemoryPaths) -> Result<Self> {
        Self::open_paths_with_clock(paths, SystemClock)
    }

    pub fn open_paths_with_clock(paths: MemoryPaths, clock: impl Clock + 'static) -> Result<Self> {
        if !paths.config_path.is_file() {
            bail!(
                "Memzoi bundle is not initialized at {}; run `memzoi init` first",
                paths.project_root.display()
            );
        }

        let conn = db::open_database(&paths.db_path)?;
        db::init_database(&conn)?;
        Ok(Self {
            paths,
            conn,
            clock: Arc::new(clock),
        })
    }

    pub fn initialize(start: impl AsRef<Path>, request: InitRequest) -> Result<InitResult> {
        let paths = discover_paths(start)?;
        Self::initialize_paths(paths, request)
    }

    pub fn initialize_paths(paths: MemoryPaths, request: InitRequest) -> Result<InitResult> {
        init_bundle(&paths, request.force)?;
        Ok(InitResult { paths })
    }

    pub fn paths(&self) -> &MemoryPaths {
        &self.paths
    }

    pub fn for_each_event(&self, visit: impl FnMut(MemoryEvent) -> Result<()>) -> Result<()> {
        stream_events(&self.conn, visit)
    }

    pub fn propose_memory(&self, actor: &str, draft: MemoryDraft) -> Result<Proposal> {
        proposals::propose_memory(&self.conn, actor, draft)
    }

    pub fn propose_memory_with_options(
        &self,
        actor: &str,
        draft: MemoryDraft,
        options: ProposeOptions,
    ) -> Result<ProposeResult> {
        let mut policy = load_effective_config(&self.paths)?
            .workflow
            .proposal_approval;
        if let Some(approval_override) = options.approval_override {
            policy = match approval_override {
                ProposalApprovalOverride::Auto => ProposalApprovalPolicy::Auto,
                ProposalApprovalOverride::Manual => ProposalApprovalPolicy::Manual,
            };
        }
        if options.apply && policy == ProposalApprovalPolicy::Manual {
            bail!(
                "proposal apply mode requires auto approval; manual proposals must be approved before apply"
            );
        }

        let proposal = proposals::propose_memory(&self.conn, actor, draft)?;
        if policy == ProposalApprovalPolicy::Manual {
            return Ok(ProposeResult {
                proposal,
                record: None,
                validation: None,
                applied: false,
            });
        }

        let validation = proposals::validate_proposal(&self.conn, &proposal.id)?;
        if !validation.is_valid {
            return Ok(ProposeResult {
                proposal: proposals::load_proposal_public(&self.conn, &proposal.id)?,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        let approved = proposals::approve_proposal(&self.conn, &proposal.id, actor)?;
        if !options.apply {
            return Ok(ProposeResult {
                proposal: approved,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        let record = self.apply_proposal(&proposal.id, actor)?;
        Ok(ProposeResult {
            proposal: proposals::load_proposal_public(&self.conn, &proposal.id)?,
            record: Some(record),
            validation: Some(validation),
            applied: true,
        })
    }

    pub fn list_proposals(&self, filter: ProposalStatusFilter) -> Result<Vec<Proposal>> {
        proposals::list_proposals(&self.conn, filter)
    }

    pub fn show_proposal(&self, proposal_id: &str) -> Result<Proposal> {
        proposals::load_proposal_public(&self.conn, proposal_id)
    }

    pub fn open_proposal_counts(&self) -> Result<BTreeMap<ProposalStatus, usize>> {
        proposals::open_proposal_counts(&self.conn)
    }

    pub fn approve_proposal(&self, proposal_id: &str, actor: &str) -> Result<Proposal> {
        proposals::approve_proposal(&self.conn, proposal_id, actor)
    }

    pub fn reject_proposal(
        &self,
        proposal_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<Proposal> {
        proposals::reject_proposal(&self.conn, proposal_id, actor, reason)
    }

    pub fn validate_proposal(&self, proposal_id: &str) -> Result<ValidationResult> {
        proposals::validate_proposal(&self.conn, proposal_id)
    }

    pub fn apply_proposal(&self, proposal_id: &str, actor: &str) -> Result<MemoryRecord> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let tx = self.conn.unchecked_transaction()?;
        let record = proposals::apply_proposal(&tx, proposal_id, actor)?;
        let write =
            self.prepare_record_file_write_with_conn(&tx, &record, FileWriteMode::CreateNew)?;
        commit_db_and_canonical_writes(&self.paths, tx, &[write])?;
        Ok(record)
    }

    pub fn apply_file_proposal(
        &self,
        proposal_path: impl AsRef<Path>,
        actor: &str,
    ) -> Result<FileProposalResolutionResult> {
        self.apply_file_proposal_with_hooks(proposal_path.as_ref(), actor, |_| Ok(()), |_| Ok(()))
    }

    pub fn apply_file_proposal_inventory_entry(
        &self,
        entry: &FileProposalInventoryEntry,
        actor: &str,
    ) -> Result<FileProposalResolutionResult> {
        self.apply_file_proposal(&entry.actual_path, actor)
    }

    #[cfg(test)]
    fn apply_file_proposal_with_finalize_hook<BeforeFinalize>(
        &self,
        proposal_path: &Path,
        actor: &str,
        before_finalize: BeforeFinalize,
    ) -> Result<FileProposalResolutionResult>
    where
        BeforeFinalize: FnOnce(&Path) -> Result<()>,
    {
        self.apply_file_proposal_with_hooks(proposal_path, actor, |_| Ok(()), before_finalize)
    }

    fn apply_file_proposal_with_hooks<BeforePendingRevalidation, BeforeFinalize>(
        &self,
        proposal_path: &Path,
        actor: &str,
        before_pending_revalidation: BeforePendingRevalidation,
        before_finalize: BeforeFinalize,
    ) -> Result<FileProposalResolutionResult>
    where
        BeforePendingRevalidation: FnOnce(&Path) -> Result<()>,
        BeforeFinalize: FnOnce(&Path) -> Result<()>,
    {
        validate_resolution_actor(actor)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let snapshot = self.load_pending_file_proposal_snapshot(proposal_path)?;
        let proposal = snapshot.proposal.clone();
        self.validate_fresh_file_proposal_identity(&proposal)?;
        let resolved_at = expiry::format_timestamp(self.now())?;
        let plan = self.build_file_proposal_apply_plan(&proposal, &resolved_at)?;
        let resolution = OkfProposalResolution {
            outcome: OkfProposalOutcome::Applied,
            resolved_by: actor.trim().to_owned(),
            resolved_at: resolved_at.clone(),
            reason: proposal.proposal.reason.clone(),
            record_id: Some(plan.record.id.clone()),
            target_id: plan.target_id.clone(),
        };
        let resolved_path = self
            .paths
            .proposals_dir()
            .join("resolved")
            .join("applied")
            .join(format!("{}.md", proposal.file_id));
        self.prepare_resolution_destination(&resolved_path)?;

        let resolved_markdown = okf::render_resolved_okf_proposal_markdown(&proposal, &resolution)?;
        let nonce = Uuid::now_v7().to_string();
        let mut staged_writes = stage_canonical_writes(&plan.writes, &nonce)?;
        let resolved_temp = match stage_file(&resolved_path, &resolved_markdown, &nonce) {
            Ok(path) => path,
            Err(error) => {
                return attach_cleanup_error(
                    error,
                    cleanup_staged_canonical_writes(&staged_writes),
                    "canonical staging cleanup",
                );
            }
        };
        let pending_backup = sibling_transaction_path(proposal_path, &nonce, "pending");

        let tx = match self.conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(error) => {
                return attach_cleanup_error(
                    error.into(),
                    cleanup_staged_file_resolution(&staged_writes, &resolved_temp),
                    "proposal-file staging cleanup",
                );
            }
        };
        let record_files = plan
            .writes
            .iter()
            .map(|write| write.record_file.clone())
            .collect::<Vec<_>>();
        if let Err(error) = okf::import_okf_records(&tx, &record_files).and_then(|_| {
            rebuild_fts_content_index(&tx)?;
            append_event(
                &tx,
                AppendEvent {
                    event_type: "proposal_file.applied".to_owned(),
                    actor: actor.trim().to_owned(),
                    payload: json!({
                        "proposal_id": proposal.id,
                        "file_id": proposal.file_id,
                        "action": proposal.proposal.action.as_str(),
                        "record_id": plan.record.id,
                        "target_id": plan.target_id,
                        "resolved_path": resolved_path,
                    }),
                    record_id: Some(plan.record.id.clone()),
                    proposal_id: Some(proposal.id.clone()),
                },
            )?;
            Ok(())
        }) {
            return attach_cleanup_error(
                error,
                cleanup_staged_file_resolution(&staged_writes, &resolved_temp),
                "proposal-file staging cleanup",
            );
        }

        let mut pending_moved = false;
        let mut resolved_installed = false;
        let install_result = (|| -> Result<()> {
            for write in &staged_writes {
                validate_canonical_write_precondition(&self.paths, write)?;
            }
            before_pending_revalidation(proposal_path)?;
            fs::rename(proposal_path, &pending_backup).with_context(|| {
                format!(
                    "failed to stage pending proposal {} for resolution",
                    proposal_path.display()
                )
            })?;
            pending_moved = true;
            self.revalidate_moved_pending_file_proposal(proposal_path, &pending_backup, &snapshot)?;
            install_staged_canonical_writes(&self.paths, &mut staged_writes, |_| Ok(()))?;
            install_staged_file_no_replace(&resolved_temp, &resolved_path)?;
            resolved_installed = true;
            Ok(())
        })();

        if let Err(error) = install_result {
            return attach_cleanup_error(
                error,
                rollback_file_resolution(
                    proposal_path,
                    &pending_backup,
                    pending_moved,
                    &mut staged_writes,
                    &resolved_path,
                    resolved_installed,
                    &resolved_temp,
                ),
                "proposal-file install rollback",
            );
        }

        if let Err(error) = tx.commit() {
            return attach_cleanup_error(
                anyhow::Error::new(error)
                    .context("failed to commit proposal-file runtime index update"),
                rollback_file_resolution(
                    proposal_path,
                    &pending_backup,
                    pending_moved,
                    &mut staged_writes,
                    &resolved_path,
                    resolved_installed,
                    &resolved_temp,
                ),
                "proposal-file commit rollback",
            );
        }

        before_finalize(&pending_backup)
            .context("proposal-file apply committed but finalization was interrupted")?;
        remove_staged_file(&pending_backup)
            .context("proposal-file apply committed but pending backup cleanup failed")?;
        finalize_staged_canonical_writes(&staged_writes)
            .context("proposal-file apply committed but canonical cleanup failed")?;
        Ok(FileProposalResolutionResult {
            proposal,
            resolution,
            resolved_path,
            record: Some(plan.record),
            record_path: Some(plan.record_path),
            already_resolved: false,
            runtime_index_updated: true,
        })
    }

    pub fn validate_file_proposal(&self, proposal: &OkfProposalFile) -> Result<()> {
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        self.build_file_proposal_apply_plan(proposal, &expiry::format_timestamp(self.now())?)?;
        Ok(())
    }

    pub fn file_proposal_inventory(&self) -> Result<FileProposalInventory> {
        scan_file_proposal_inventory(&self.paths)
    }

    pub fn validate_file_proposal_inventory(&self) -> Result<FileProposalInventory> {
        let mut inventory = scan_file_proposal_inventory(&self.paths)?;
        let resolved_at = expiry::format_timestamp(self.now())?;
        let mut valid = Vec::with_capacity(inventory.pending.len());
        for entry in std::mem::take(&mut inventory.pending) {
            if inventory
                .errors
                .iter()
                .any(|error| error.display_path == entry.display_path)
            {
                continue;
            }
            match self.build_file_proposal_apply_plan(&entry.proposal, &resolved_at) {
                Ok(_) => valid.push(entry),
                Err(error) => {
                    inventory.errors.push(FileProposalInventoryError {
                        display_path: entry.display_path,
                        error: error.to_string(),
                    });
                }
            }
        }
        inventory.pending = valid;
        inventory
            .errors
            .sort_by(|left, right| left.display_path.cmp(&right.display_path));
        Ok(inventory)
    }

    pub fn replay_file_proposal(
        &self,
        proposal_identity: &str,
        requested_outcome: OkfProposalOutcome,
        actor: &str,
    ) -> Result<FileProposalResolutionResult> {
        validate_resolution_actor(actor)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;

        let matches = inventory
            .resolved
            .iter()
            .filter(|entry| okf::okf_proposal_matches_identity(&entry.proposal, proposal_identity))
            .collect::<Vec<_>>();
        let entry = match matches.as_slice() {
            [] => bail!("proposal file not found: {proposal_identity}"),
            [entry] => *entry,
            _ => bail!(
                "proposal file identity {proposal_identity:?} matched multiple resolved packets"
            ),
        };
        let resolution = entry
            .proposal
            .resolution
            .clone()
            .context("resolved proposal is missing resolution metadata")?;
        if resolution.outcome != requested_outcome {
            bail!(
                "proposal file {} is already resolved as {}; cannot resolve as {}",
                entry.proposal.id,
                resolution.outcome.as_str(),
                requested_outcome.as_str()
            );
        }

        if requested_outcome == OkfProposalOutcome::Rejected {
            return Ok(FileProposalResolutionResult {
                proposal: entry.proposal.clone(),
                resolution,
                resolved_path: entry.actual_path.clone(),
                record: None,
                record_path: None,
                already_resolved: true,
                runtime_index_updated: false,
            });
        }

        let (canonical_records, primary_index) =
            self.validate_resolved_apply_canonical_truth(&entry.proposal, &resolution)?;
        let relational_drift = canonical_records
            .iter()
            .map(|canonical| self.indexed_record_matches_canonical(canonical))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|matches| !matches);
        let fts_out_of_sync = !fts_content_index_is_current(&self.conn)?;
        let runtime_index_updated = relational_drift || fts_out_of_sync;
        if runtime_index_updated {
            let tx = self.conn.unchecked_transaction()?;
            if relational_drift {
                okf::import_okf_records(&tx, &canonical_records)?;
            }
            rebuild_fts_content_index(&tx)?;
            append_event(
                &tx,
                AppendEvent {
                    event_type: "proposal_file.index_repaired".to_owned(),
                    actor: actor.trim().to_owned(),
                    payload: json!({
                        "proposal_id": entry.proposal.id,
                        "file_id": entry.proposal.file_id,
                        "record_ids": canonical_records
                            .iter()
                            .map(|record| record.concept_id.as_str())
                            .collect::<Vec<_>>(),
                        "resolved_path": entry.display_path,
                        "relational_drift": relational_drift,
                        "fts_out_of_sync": fts_out_of_sync,
                    }),
                    record_id: resolution.record_id.clone(),
                    proposal_id: Some(entry.proposal.id.clone()),
                },
            )?;
            tx.commit()?;
        }

        let canonical = &canonical_records[primary_index];
        let record = okf::project_okf_record(canonical);
        let record_path = self.canonical_record_path(&record.id)?;
        Ok(FileProposalResolutionResult {
            proposal: entry.proposal.clone(),
            resolution,
            resolved_path: entry.actual_path.clone(),
            record: Some(record),
            record_path: Some(record_path),
            already_resolved: true,
            runtime_index_updated,
        })
    }

    fn validate_fresh_file_proposal_identity(&self, proposal: &OkfProposalFile) -> Result<()> {
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        if inventory
            .resolved
            .iter()
            .any(|entry| okf::okf_proposals_share_identity(proposal, &entry.proposal))
        {
            let token = okf::proposal_identity_tokens(proposal)
                .into_iter()
                .next()
                .unwrap_or_else(|| "redacted-identity-unavailable".to_owned());
            bail!("pending proposal identity token {token} is already resolved");
        }
        let proposal_tokens = okf::proposal_identity_tokens(proposal);
        if let Some(token) = db_proposal_identity_tokens(&self.conn)?
            .intersection(&proposal_tokens)
            .next()
        {
            bail!("file proposal identity token {token} conflicts with a database proposal");
        }
        Ok(())
    }

    fn prepare_resolution_destination(&self, path: &Path) -> Result<()> {
        let proposals_root = self.paths.proposals_dir();
        ensure_safe_path_parent(
            &self.paths.project_root,
            &proposals_root,
            path,
            true,
            "resolved proposal packet",
        )?;
        ensure_path_absent(path, "resolved proposal packet")
    }

    fn validate_resolved_apply_canonical_truth(
        &self,
        proposal: &OkfProposalFile,
        resolution: &OkfProposalResolution,
    ) -> Result<(Vec<okf::OkfRecordFile>, usize)> {
        let mut pending_shape = proposal.clone();
        pending_shape.status = crate::OkfProposalStatus::Proposed;
        pending_shape.resolution = None;
        okf::validate_repo_apply_proposal(&pending_shape)?;
        let record_id = resolution
            .record_id
            .as_deref()
            .context("applied resolution is missing record_id")?;

        match proposal.proposal.action {
            OkfProposalAction::Create => {
                if resolution.target_id.is_some() {
                    bail!(
                        "resolved create proposal {} unexpectedly names a target",
                        proposal.id
                    );
                }
                let expected = okf::project_okf_create_proposal(&pending_shape)?;
                if expected.id != record_id {
                    bail!(
                        "resolved proposal {} record lineage drift: expected {}, found {}",
                        proposal.id,
                        expected.id,
                        record_id
                    );
                }
                let (canonical, path, markdown) = self.load_canonical_record(record_id)?;
                ensure_expected_canonical_bytes(
                    proposal,
                    &expected,
                    &proposal.tags,
                    &proposal.applies_to,
                    &path,
                    &markdown,
                )?;
                Ok((vec![canonical], 0))
            }
            OkfProposalAction::Supersede => {
                let target_id = proposal
                    .supersedes
                    .first()
                    .context("resolved supersede proposal is missing target")?;
                if proposal.supersedes.len() != 1
                    || resolution.target_id.as_deref() != Some(target_id.as_str())
                {
                    bail!(
                        "resolved supersede proposal {} has inconsistent target lineage",
                        proposal.id
                    );
                }
                let expected = okf::project_okf_supersede_proposal(&pending_shape, target_id)?;
                if expected.id != record_id {
                    bail!(
                        "resolved proposal {} record lineage drift: expected {}, found {}",
                        proposal.id,
                        expected.id,
                        record_id
                    );
                }
                let (target, _, _) = self.load_canonical_record(target_id)?;
                if target.status != MemoryStatus::Superseded
                    || target.draft.scope_kind != proposal.scope_kind
                    || target.draft.scope_id != proposal.scope_id
                    || target.updated.as_deref() != Some(resolution.resolved_at.as_str())
                {
                    bail!(
                        "resolved supersede proposal {} has canonical target drift at {}",
                        proposal.id,
                        target_id
                    );
                }
                let (replacement, path, markdown) = self.load_canonical_record(record_id)?;
                ensure_expected_canonical_bytes(
                    proposal,
                    &expected,
                    &proposal.tags,
                    &proposal.applies_to,
                    &path,
                    &markdown,
                )?;
                Ok((vec![target, replacement], 1))
            }
            OkfProposalAction::Tombstone => {
                let target_id = proposal
                    .proposal
                    .target
                    .as_deref()
                    .context("resolved tombstone proposal is missing target")?;
                if resolution.target_id.as_deref() != Some(target_id) || record_id != target_id {
                    bail!(
                        "resolved tombstone proposal {} has inconsistent target lineage",
                        proposal.id
                    );
                }
                let (target, _, _) = self.load_canonical_record(target_id)?;
                if target.status != MemoryStatus::Tombstoned
                    || target.draft.scope_kind != proposal.scope_kind
                    || target.draft.scope_id != proposal.scope_id
                    || target.updated.as_deref() != Some(resolution.resolved_at.as_str())
                {
                    bail!(
                        "resolved tombstone proposal {} has canonical target drift at {}",
                        proposal.id,
                        target_id
                    );
                }
                Ok((vec![target], 0))
            }
        }
    }

    fn load_canonical_record(
        &self,
        record_id: &str,
    ) -> Result<(okf::OkfRecordFile, PathBuf, String)> {
        let path = self.canonical_record_path(record_id)?;
        ensure_regular_file(&path, "canonical memory record").with_context(|| {
            format!("resolved proposal canonical drift: record {record_id} is missing or unsafe")
        })?;
        let markdown = fs::read_to_string(&path).with_context(|| {
            format!("failed to read canonical memory record {}", path.display())
        })?;
        let record = okf::parse_okf_record_markdown(self.paths.records_dir(), &path, &markdown)?
            .with_context(|| {
                format!("resolved proposal canonical drift: record {record_id} was ignored")
            })?;
        if record.concept_id != record_id {
            bail!(
                "resolved proposal canonical drift: expected record {record_id}, found {}",
                record.concept_id
            );
        }
        Ok((record, path, markdown))
    }

    fn canonical_record_path(&self, record_id: &str) -> Result<PathBuf> {
        okf::validate_concept_id(record_id)
            .with_context(|| format!("invalid canonical record lineage id {record_id:?}"))?;
        let records_root = self.paths.records_dir();
        let path = records_root.join(format!("{record_id}.md"));
        ensure_safe_path_parent(
            &self.paths.project_root,
            &records_root,
            &path,
            false,
            "canonical memory record",
        )
        .with_context(|| {
            format!(
                "failed to inspect canonical memory record {}",
                path.display()
            )
        })?;
        Ok(path)
    }

    fn indexed_record_matches_canonical(&self, canonical: &okf::OkfRecordFile) -> Result<bool> {
        let Some(indexed) = record_by_id(&self.conn, &canonical.concept_id)? else {
            return Ok(false);
        };
        if !repo_record_matches(canonical, &indexed) {
            return Ok(false);
        }

        let expected_tags = canonical
            .draft
            .tags
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if record_tags(&self.conn, &canonical.concept_id)? != expected_tags {
            return Ok(false);
        }
        let mut expected_paths = canonical.applies_to.clone();
        expected_paths.sort();
        let actual_paths = search::load_paths(&self.conn, &canonical.concept_id)?
            .into_iter()
            .map(|path| path.path)
            .collect::<Vec<_>>();
        Ok(actual_paths == expected_paths)
    }

    fn build_file_proposal_apply_plan(
        &self,
        proposal: &OkfProposalFile,
        resolved_at: &str,
    ) -> Result<FileProposalApplyPlan> {
        okf::validate_repo_apply_proposal(proposal)?;
        match proposal.proposal.action {
            OkfProposalAction::Create => {
                if proposal.proposal.target.is_some() || !proposal.supersedes.is_empty() {
                    bail!("OKF create proposals cannot name a target or supersedes record");
                }
                let record = okf::project_okf_create_proposal(proposal)?;
                let write = self.prepare_canonical_file_write(
                    record.clone(),
                    proposal.tags.clone(),
                    proposal.applies_to.clone(),
                    FileWriteMode::CreateNew,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: write.path.clone(),
                    writes: vec![write],
                    record,
                    target_id: None,
                })
            }
            OkfProposalAction::Supersede => {
                require_action_reason(proposal, "supersede")?;
                if proposal.supersedes.len() != 1 || proposal.proposal.target.is_some() {
                    bail!(
                        "OKF supersede proposals must include exactly one supersedes target and no proposal.target"
                    );
                }
                let target_id = proposal.supersedes[0].clone();
                let target = self.load_file_proposal_target(proposal, &target_id)?;
                let mut previous = okf::project_okf_record(&target);
                previous.status = MemoryStatus::Superseded;
                previous.updated_at = resolved_at.to_owned();
                let replacement = okf::project_okf_supersede_proposal(proposal, &target_id)?;
                if replacement.id == target_id {
                    bail!(
                        "supersede replacement record id {} collides with its target; use a distinct title or proposal file id",
                        replacement.id
                    );
                }
                let previous_write = self.prepare_canonical_file_write(
                    previous,
                    target.draft.tags.clone(),
                    target.applies_to.clone(),
                    FileWriteMode::Overwrite,
                )?;
                let replacement_write = self.prepare_canonical_file_write(
                    replacement.clone(),
                    proposal.tags.clone(),
                    proposal.applies_to.clone(),
                    FileWriteMode::CreateNew,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: replacement_write.path.clone(),
                    writes: vec![previous_write, replacement_write],
                    record: replacement,
                    target_id: Some(target_id),
                })
            }
            OkfProposalAction::Tombstone => {
                require_action_reason(proposal, "tombstone")?;
                if !proposal.supersedes.is_empty() {
                    bail!("OKF tombstone proposals cannot include supersedes records");
                }
                let target_id = proposal
                    .proposal
                    .target
                    .as_deref()
                    .context("OKF tombstone proposals must include exactly one proposal.target")?
                    .to_owned();
                let target = self.load_file_proposal_target(proposal, &target_id)?;
                let mut tombstoned = okf::project_okf_record(&target);
                tombstoned.status = MemoryStatus::Tombstoned;
                tombstoned.updated_at = resolved_at.to_owned();
                let write = self.prepare_canonical_file_write(
                    tombstoned.clone(),
                    target.draft.tags.clone(),
                    target.applies_to.clone(),
                    FileWriteMode::Overwrite,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: write.path.clone(),
                    writes: vec![write],
                    record: tombstoned,
                    target_id: Some(target_id),
                })
            }
        }
    }

    fn prepare_canonical_file_write(
        &self,
        record: MemoryRecord,
        tags: Vec<String>,
        applies_to: Vec<String>,
        mode: FileWriteMode,
    ) -> Result<CanonicalFileWrite> {
        let records_root = self.paths.records_dir();
        let path = records_root.join(format!("{}.md", record.id));
        ensure_safe_path_parent(
            &self.paths.project_root,
            &records_root,
            &path,
            false,
            "canonical memory record",
        )
        .with_context(|| {
            format!(
                "failed to inspect canonical memory record {}",
                path.display()
            )
        })?;
        match mode {
            FileWriteMode::CreateNew => ensure_path_absent(&path, "canonical memory record")?,
            FileWriteMode::Overwrite => ensure_regular_file(&path, "canonical memory record")?,
        }
        let expected_existing_hash = match mode {
            FileWriteMode::CreateNew => None,
            FileWriteMode::Overwrite => Some(file_content_hash(&path)?),
        };
        let markdown = okf::render_memory_record_markdown(&record, &tags, &applies_to);
        let record_file = okf::parse_okf_record_markdown(&records_root, &path, &markdown)?
            .context("projected canonical record was ignored")?;
        Ok(CanonicalFileWrite {
            record_file,
            path,
            markdown,
            mode,
            expected_existing_hash,
        })
    }

    fn load_file_proposal_target(
        &self,
        proposal: &OkfProposalFile,
        target_id: &str,
    ) -> Result<okf::OkfRecordFile> {
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        let records = okf::read_okf_record_files(self.paths.records_dir())?;
        let target = records
            .into_iter()
            .find(|record| record.concept_id == target_id)
            .with_context(|| format!("proposal target does not exist: {target_id}"))?;
        if target.status != MemoryStatus::Active {
            bail!(
                "proposal target {target_id} is inactive with status {}",
                target.status.as_str()
            );
        }
        if target.draft.scope_kind != proposal.scope_kind
            || target.draft.scope_id != proposal.scope_id
        {
            bail!(
                "proposal target {target_id} is cross-scope: target={}:{}, proposal={}:{}",
                target.draft.scope_kind.as_str(),
                target.draft.scope_id.as_deref().unwrap_or("-"),
                proposal.scope_kind.as_str(),
                proposal.scope_id.as_deref().unwrap_or("-")
            );
        }
        let target_updated = target.updated.as_deref().unwrap_or(&target.created);
        if parse_orderable_timestamp(target_updated, "target updated")?
            > parse_orderable_timestamp(&proposal.proposal.proposed_at, "proposal.proposed_at")?
        {
            bail!(
                "proposal target {target_id} is stale: target updated at {target_updated} after proposal at {}",
                proposal.proposal.proposed_at
            );
        }
        Ok(target)
    }

    pub fn reject_file_proposal(
        &self,
        proposal_path: impl AsRef<Path>,
        actor: &str,
        reason: &str,
    ) -> Result<FileProposalResolutionResult> {
        self.reject_file_proposal_with_hooks(
            proposal_path.as_ref(),
            actor,
            reason,
            |_| Ok(()),
            |_| Ok(()),
        )
    }

    pub fn reject_file_proposal_inventory_entry(
        &self,
        entry: &FileProposalInventoryEntry,
        actor: &str,
        reason: &str,
    ) -> Result<FileProposalResolutionResult> {
        self.reject_file_proposal(&entry.actual_path, actor, reason)
    }

    #[cfg(test)]
    fn reject_file_proposal_with_finalize_hook<BeforeFinalize>(
        &self,
        proposal_path: &Path,
        actor: &str,
        reason: &str,
        before_finalize: BeforeFinalize,
    ) -> Result<FileProposalResolutionResult>
    where
        BeforeFinalize: FnOnce(&Path) -> Result<()>,
    {
        self.reject_file_proposal_with_hooks(
            proposal_path,
            actor,
            reason,
            |_| Ok(()),
            before_finalize,
        )
    }

    fn reject_file_proposal_with_hooks<BeforePendingRevalidation, BeforeFinalize>(
        &self,
        proposal_path: &Path,
        actor: &str,
        reason: &str,
        before_pending_revalidation: BeforePendingRevalidation,
        before_finalize: BeforeFinalize,
    ) -> Result<FileProposalResolutionResult>
    where
        BeforePendingRevalidation: FnOnce(&Path) -> Result<()>,
        BeforeFinalize: FnOnce(&Path) -> Result<()>,
    {
        validate_resolution_actor(actor)?;
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("proposal-file rejection reason cannot be empty");
        }
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let snapshot = self.load_pending_file_proposal_snapshot(proposal_path)?;
        let proposal = snapshot.proposal.clone();
        self.validate_fresh_file_proposal_identity(&proposal)?;
        let mut resolution = OkfProposalResolution {
            outcome: OkfProposalOutcome::Rejected,
            resolved_by: actor.trim().to_owned(),
            resolved_at: expiry::format_timestamp(self.now())?,
            reason: Some(reason.to_owned()),
            record_id: None,
            target_id: proposal
                .proposal
                .target
                .clone()
                .or_else(|| proposal.supersedes.first().cloned()),
        };
        if proposal.sensitivity != crate::OkfProposalSensitivity::RepoSafe {
            resolution.target_id = None;
        }
        let resolved_path = self
            .paths
            .proposals_dir()
            .join("resolved")
            .join("rejected")
            .join(format!("{}.md", proposal.file_id));
        self.prepare_resolution_destination(&resolved_path)?;
        let archived_proposal = proposal.clone();
        let resolved_markdown =
            okf::render_resolved_okf_proposal_markdown(&archived_proposal, &resolution)?;
        let nonce = Uuid::now_v7().to_string();
        let resolved_temp = stage_file(&resolved_path, &resolved_markdown, &nonce)?;
        let pending_backup = sibling_transaction_path(proposal_path, &nonce, "pending");

        let display_pending_path = snapshot.display_path.clone();
        if let Err(error) = before_pending_revalidation(proposal_path) {
            return attach_cleanup_error(
                error,
                remove_staged_file(&resolved_temp),
                "rejection staging cleanup",
            );
        }
        if let Err(error) = fs::rename(proposal_path, &pending_backup).with_context(|| {
            format!(
                "failed to stage pending proposal {} for rejection",
                display_pending_path.display()
            )
        }) {
            return attach_cleanup_error(
                error,
                remove_staged_file(&resolved_temp),
                "rejection staging cleanup",
            );
        }
        if let Err(error) =
            self.revalidate_moved_pending_file_proposal(proposal_path, &pending_backup, &snapshot)
        {
            return attach_cleanup_error(
                error,
                rollback_rejected_file_proposal(
                    proposal_path,
                    &pending_backup,
                    &resolved_path,
                    false,
                    &resolved_temp,
                ),
                "rejection captured-byte rollback",
            );
        }
        if let Err(error) = install_staged_file_no_replace(&resolved_temp, &resolved_path) {
            return attach_cleanup_error(
                error,
                rollback_rejected_file_proposal(
                    proposal_path,
                    &pending_backup,
                    &resolved_path,
                    false,
                    &resolved_temp,
                ),
                "rejection install rollback",
            );
        }
        if let Err(error) = before_finalize(&pending_backup)
            .context("proposal-file rejection finalization was interrupted")
        {
            return attach_cleanup_error(
                error,
                rollback_rejected_file_proposal(
                    proposal_path,
                    &pending_backup,
                    &resolved_path,
                    true,
                    &resolved_temp,
                ),
                "rejection finalization rollback",
            );
        }
        if let Err(error) = remove_staged_file(&pending_backup).map_err(|_| {
            anyhow!(
                "failed to remove rejected proposal backup {}",
                snapshot.display_path.display()
            )
        }) {
            return attach_cleanup_error(
                error,
                rollback_rejected_file_proposal(
                    proposal_path,
                    &pending_backup,
                    &resolved_path,
                    true,
                    &resolved_temp,
                ),
                "rejection cleanup rollback",
            );
        }

        Ok(FileProposalResolutionResult {
            proposal: archived_proposal,
            resolution,
            resolved_path,
            record: None,
            record_path: None,
            already_resolved: false,
            runtime_index_updated: false,
        })
    }

    fn load_pending_file_proposal_snapshot(
        &self,
        proposal_path: &Path,
    ) -> Result<PendingFileProposalSnapshot> {
        let pending_root = self.paths.proposals_dir().join("pending");
        let fallback_display_path = okf::redacted_okf_proposal_path(&pending_root, proposal_path)
            .unwrap_or_else(|_| pending_root.join("redacted-proposal.md"));
        ensure_safe_existing_file(
            &self.paths.project_root,
            &pending_root,
            proposal_path,
            "pending proposal",
        )
        .map_err(|_| {
            anyhow!(
                "failed to inspect pending proposal {}",
                fallback_display_path.display()
            )
        })?;
        let markdown = fs::read_to_string(proposal_path).map_err(|_| {
            anyhow!(
                "failed to read pending proposal {}",
                fallback_display_path.display()
            )
        })?;
        let expected_hash = blake3::hash(markdown.as_bytes()).to_hex().to_string();
        let preflight =
            okf::preflight_okf_proposal_markdown(&pending_root, proposal_path, &markdown)
                .map_err(|_| {
                    anyhow!(
                        "failed to safety preflight pending proposal {}",
                        fallback_display_path.display()
                    )
                })?
                .context("pending proposal was ignored")?;
        let non_repo_safe = preflight.sensitivity != crate::OkfProposalSensitivity::RepoSafe;
        let display_path = if non_repo_safe {
            pending_root.join(format!("{}.md", preflight.receipt_proposal.file_id))
        } else {
            proposal_path.to_path_buf()
        };
        let proposal = if non_repo_safe {
            preflight.receipt_proposal
        } else {
            okf::parse_okf_proposal_markdown(&pending_root, proposal_path, &markdown)?
                .context("pending proposal was ignored")?
        };
        if proposal.status != crate::OkfProposalStatus::Proposed || proposal.resolution.is_some() {
            bail!(
                "pending proposal must be unresolved: {}",
                display_path.display()
            );
        }
        Ok(PendingFileProposalSnapshot {
            proposal,
            expected_hash,
            display_path,
        })
    }

    fn revalidate_moved_pending_file_proposal(
        &self,
        proposal_path: &Path,
        pending_backup: &Path,
        snapshot: &PendingFileProposalSnapshot,
    ) -> Result<()> {
        let pending_root = self.paths.proposals_dir().join("pending");
        ensure_safe_existing_file(
            &self.paths.project_root,
            &pending_root,
            pending_backup,
            "captured pending proposal",
        )
        .map_err(|_| {
            anyhow!(
                "pending proposal changed after validation: {}",
                snapshot.display_path.display()
            )
        })?;
        let current = fs::read(pending_backup).map_err(|_| {
            anyhow!(
                "pending proposal changed after validation: {}",
                snapshot.display_path.display()
            )
        })?;
        let current_hash = blake3::hash(&current).to_hex().to_string();
        if current_hash != snapshot.expected_hash {
            bail!(
                "pending proposal changed after validation: {}",
                snapshot.display_path.display()
            );
        }
        match fs::symlink_metadata(proposal_path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) | Err(_) => {
                bail!(
                    "pending proposal changed after validation: {}",
                    snapshot.display_path.display()
                );
            }
        }
        Ok(())
    }

    pub fn supersede_record(
        &self,
        record_id: &str,
        actor: &str,
        draft: MemoryDraft,
    ) -> Result<SupersedeResult> {
        self.supersede_record_with_hooks(record_id, actor, draft, |_| Ok(()), |_| Ok(()))
    }

    fn supersede_record_with_hooks<BeforeInstall, BeforeCommit>(
        &self,
        record_id: &str,
        actor: &str,
        draft: MemoryDraft,
        before_install: BeforeInstall,
        before_commit: BeforeCommit,
    ) -> Result<SupersedeResult>
    where
        BeforeInstall: FnMut(usize) -> Result<()>,
        BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
    {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let tx = self.conn.unchecked_transaction()?;
        let target = record_by_id(&tx, record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        if target.scope_kind != draft.scope_kind || target.scope_id != draft.scope_id {
            bail!(
                "cannot supersede record {record_id} cross-scope: target={}:{}, replacement={}:{}",
                target.scope_kind.as_str(),
                target.scope_id.as_deref().unwrap_or("-"),
                draft.scope_kind.as_str(),
                draft.scope_id.as_deref().unwrap_or("-")
            );
        }
        let result = proposals::supersede_record(&tx, record_id, actor, draft)?;
        let previous_write = self.prepare_record_file_write_with_conn(
            &tx,
            &result.previous,
            FileWriteMode::Overwrite,
        )?;
        let replacement_write = self.prepare_record_file_write_with_conn(
            &tx,
            &result.replacement,
            FileWriteMode::CreateNew,
        )?;
        commit_db_and_canonical_writes_with_hooks(
            &self.paths,
            tx,
            &[previous_write, replacement_write],
            before_install,
            before_commit,
        )?;
        Ok(result)
    }

    pub fn tombstone_record(
        &self,
        record_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<MemoryRecord> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let tx = self.conn.unchecked_transaction()?;
        let target = record_by_id(&tx, record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        let record = proposals::tombstone_record(&tx, record_id, actor, reason)?;
        let write =
            self.prepare_record_file_write_with_conn(&tx, &record, FileWriteMode::Overwrite)?;
        commit_db_and_canonical_writes(&self.paths, tx, &[write])?;
        Ok(record)
    }

    fn prepare_record_file_write_with_conn(
        &self,
        conn: &Connection,
        record: &MemoryRecord,
        mode: FileWriteMode,
    ) -> Result<CanonicalFileWrite> {
        let tags = record_tags(conn, &record.id)?;
        let applies_to = search::load_paths(conn, &record.id)?
            .into_iter()
            .map(|path| path.path)
            .collect::<Vec<_>>();
        self.prepare_canonical_file_write(record.clone(), tags, applies_to, mode)
    }

    pub fn search_memory(&self, input: SearchInput) -> Result<Vec<SearchResult>> {
        search::search_memory_at(&self.conn, input, self.now())
    }

    pub fn inspect_expiry(&self, record_id: &str) -> Result<ExpiryDiagnostic> {
        let record = record_by_id(&self.conn, record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        expiry::diagnose(record, self.now())
    }

    pub fn repo_index_drift(&self) -> Result<RepoIndexDrift> {
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        let canonical = okf::read_okf_record_files(self.paths.records_dir())?
            .into_iter()
            .filter(|record| record.status == MemoryStatus::Active)
            .map(|record| (record.concept_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let indexed = indexed_active_records_for_destination(&self.conn, MemoryDestination::Repo)?
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect::<BTreeMap<_, _>>();

        let missing_from_index = canonical
            .keys()
            .filter(|id| !indexed.contains_key(*id))
            .cloned()
            .collect();
        let stale_in_index = indexed
            .keys()
            .filter(|id| !canonical.contains_key(*id))
            .cloned()
            .collect();
        let changed_in_index = canonical
            .iter()
            .filter_map(|(id, canonical)| {
                indexed
                    .get(id)
                    .filter(|indexed| !repo_record_matches(canonical, indexed))
                    .map(|_| id.clone())
            })
            .collect();
        let fts_out_of_sync = !fts_content_index_is_current(&self.conn)?;

        Ok(RepoIndexDrift {
            missing_from_index,
            stale_in_index,
            changed_in_index,
            fts_out_of_sync,
        })
    }

    pub fn create_local_memory(
        &self,
        actor: &str,
        input: LocalMemoryInput,
    ) -> Result<MemoryRecord> {
        let now = self.now_timestamp()?;
        create_local_memory_with_conn(&self.conn, actor, &input, &now)
    }

    pub fn list_local_memory(&self) -> Result<Vec<MemoryRecord>> {
        active_records_for_destination(&self.conn, MemoryDestination::Local, self.now())
    }

    pub fn search_local_memory(&self, query: String, limit: usize) -> Result<Vec<SearchResult>> {
        search::search_memory_at(
            &self.conn,
            SearchInput {
                query,
                destination: Some(MemoryDestination::Local),
                limit,
                include_inactive: false,
                ..SearchInput::default()
            },
            self.now(),
        )
    }

    pub fn create_checkpoint(&self, actor: &str, input: CheckpointInput) -> Result<MemoryRecord> {
        let now = self.now_timestamp()?;
        create_checkpoint_with_conn(&self.conn, actor, &input, &now)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<MemoryRecord>> {
        active_checkpoint_records(&self.conn, self.now())
    }

    pub fn show_checkpoint(&self, record_id: &str) -> Result<MemoryRecord> {
        checkpoint_record(&self.conn, record_id, self.now())?
            .with_context(|| format!("checkpoint not found: {record_id}"))
    }

    pub fn promote_session_end(
        &self,
        actor: &str,
        document: SessionEndDocument,
    ) -> Result<SessionEndResult> {
        validate_session_end_document(&document)?;
        if document.candidates.iter().any(|candidate| {
            candidate.destination == MemoryDestination::Repo
                && candidate.sensitivity != crate::OkfProposalSensitivity::RepoSafe
        }) {
            return Ok(blocked_session_end_result(document));
        }
        let has_repo_writes = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let _lifecycle_lock = has_repo_writes
            .then(|| RepoLifecycleLock::acquire(&self.paths))
            .transpose()?;
        let timestamp = self.now_timestamp()?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut reserved_proposal_ids = if has_repo_writes {
            prepare_pending_proposal_root(&self.paths)?;
            let inventory = scan_file_proposal_inventory(&self.paths)?;
            require_clean_file_proposal_inventory(&inventory)?;
            reserved_proposal_identities(&self.conn, &inventory)?
        } else {
            BTreeSet::new()
        };
        let mut repo_plans = Vec::with_capacity(document.candidates.len());

        for candidate in &document.candidates {
            if candidate.destination == MemoryDestination::Repo {
                let base_slug = proposals::title_to_concept_slug(&candidate.title)
                    .unwrap_or_else(|| "memory".to_owned());
                let base_id = format!("mem_session_{base_slug}");
                let proposal_id = okf::reserve_okf_proposal_id(
                    &pending_root,
                    &base_id,
                    &mut reserved_proposal_ids,
                )?;
                let draft = session_end_proposal_draft(candidate, actor, &timestamp, proposal_id)?;
                repo_plans.push(Some(okf::plan_okf_create_proposal(&pending_root, &draft)?));
            } else {
                repo_plans.push(None);
            }
        }

        let mut repo_writes = vec![None::<(String, PathBuf)>; document.candidates.len()];
        let mut runtime_writes =
            vec![None::<(String, MemoryDestination)>; document.candidates.len()];
        let mut created_proposal_files = Vec::new();
        let write_result = (|| -> Result<()> {
            if has_repo_writes {
                let inventory = scan_file_proposal_inventory(&self.paths)?;
                require_clean_file_proposal_inventory(&inventory)?;
                ensure_planned_proposals_available(
                    &self.conn,
                    &inventory,
                    repo_plans.iter().filter_map(Option::as_ref),
                )?;
            }
            for (index, plan) in repo_plans.iter().enumerate() {
                let Some(plan) = plan else {
                    continue;
                };
                let path = okf::create_okf_proposal_file(plan)?;
                created_proposal_files.push(path.clone());
                repo_writes[index] = Some((plan.proposal_id.clone(), path));
            }
            let tx = self.conn.unchecked_transaction()?;
            for (index, candidate) in document.candidates.iter().enumerate() {
                match candidate.destination {
                    MemoryDestination::Local => {
                        let record = create_local_memory_with_conn(
                            &tx,
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Local));
                    }
                    MemoryDestination::Session => {
                        let record = create_checkpoint_with_conn(
                            &tx,
                            actor,
                            &CheckpointInput {
                                task: candidate.title.clone(),
                                note: candidate.body.clone(),
                            },
                            &timestamp,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Session));
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Discard
                    | MemoryDestination::NeedsReview => {}
                }
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            if let Err(cleanup_error) = okf::cleanup_okf_proposal_files(&created_proposal_files) {
                return Err(error).context(format!("session-end promotion failed; additionally failed to clean up created proposal files: {cleanup_error}"));
            }
            return Err(error);
        }

        let mut results = Vec::with_capacity(document.candidates.len());
        for (index, candidate) in document.candidates.into_iter().enumerate() {
            let title = candidate.title.trim().to_owned();
            let write = match candidate.destination {
                MemoryDestination::Repo => {
                    let (proposal_id, path) = repo_writes[index]
                        .clone()
                        .context("repo session-end candidate should have a proposal write")?;
                    Some(SessionEndWrite::ProposalFile { proposal_id, path })
                }
                MemoryDestination::Local => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("local session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Session => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Discard | MemoryDestination::NeedsReview => None,
            };
            let status = match candidate.destination {
                MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                MemoryDestination::Repo | MemoryDestination::Local | MemoryDestination::Session => {
                    SessionEndCandidateStatus::Written
                }
            };
            results.push(SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason: match candidate.destination {
                    MemoryDestination::Discard => {
                        Some("discard destination performs no write".to_owned())
                    }
                    MemoryDestination::NeedsReview => {
                        Some("candidate requires human review before writing".to_owned())
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => None,
                },
                write,
            });
        }

        Ok(SessionEndResult {
            task: document.task.trim().to_owned(),
            candidates: results,
        })
    }
    pub fn plan_import(&self, actor: &str, document: ImportDocument) -> Result<ImportPlan> {
        if actor.trim().is_empty() {
            bail!("import actor cannot be empty");
        }
        import::validate_document(&document)?;
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        let existing = self.load_import_duplicates(&inventory.pending)?;
        let reserved_proposal_ids = reserved_proposal_identities(&self.conn, &inventory)?;
        import::build_plan(
            actor,
            &document,
            &existing,
            &self.paths.proposals_dir().join("pending"),
            &reserved_proposal_ids,
        )
    }

    pub fn apply_import(
        &self,
        actor: &str,
        document: ImportDocument,
        expected_plan_id: &str,
    ) -> Result<ImportApplyResult> {
        let has_repo_candidates = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let _lifecycle_lock = has_repo_candidates
            .then(|| RepoLifecycleLock::acquire(&self.paths))
            .transpose()?;
        if has_repo_candidates {
            preflight_pending_proposal_root(&self.paths)?;
        }
        let plan = self.plan_import(actor, document.clone())?;
        if plan.plan_id != expected_plan_id {
            bail!(
                "stale import plan: expected {expected_plan_id}, recomputed {}",
                plan.plan_id
            );
        }
        if plan.candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                import::ImportCandidateAction::CreateProposal { .. }
            )
        }) {
            prepare_pending_proposal_root(&self.paths)?;
        }
        let timestamp = self.now_timestamp()?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut planned = Vec::new();
        for candidate in &plan.candidates {
            let import::ImportCandidateAction::CreateProposal { proposal_id, .. } =
                &candidate.action
            else {
                continue;
            };
            let draft =
                import::proposal_draft(candidate, actor, &timestamp, proposal_id, &plan.sources);
            planned.push((
                candidate.index,
                okf::plan_okf_create_proposal(&pending_root, &draft)?,
            ));
        }
        let mut writes = Vec::new();
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            if !planned.is_empty() {
                let inventory = scan_file_proposal_inventory(&self.paths)?;
                require_clean_file_proposal_inventory(&inventory)?;
                ensure_planned_proposals_available(
                    &self.conn,
                    &inventory,
                    planned.iter().map(|(_, proposal)| proposal),
                )?;
            }
            for (candidate_index, proposal) in &planned {
                let path = okf::create_okf_proposal_file(proposal)?;
                created.push(path.clone());
                let candidate_plan = plan
                    .candidates
                    .iter()
                    .find(|candidate| candidate.index == *candidate_index)
                    .context("import proposal candidate should remain in the plan")?;
                let display_path = match &candidate_plan.action {
                    import::ImportCandidateAction::CreateProposal { path, .. } => path.clone(),
                    _ => bail!("import proposal candidate action changed before apply"),
                };
                writes.push(crate::ImportWrite::ProposalFile {
                    index: *candidate_index,
                    proposal_id: proposal.proposal_id.clone(),
                    path: display_path,
                });
            }

            let tx = self.conn.unchecked_transaction()?;
            for candidate in &plan.candidates {
                let import::ImportCandidateAction::CreateRuntime { route } = candidate.action
                else {
                    continue;
                };
                let record = match route {
                    crate::MemoryWriteRoute::RuntimeLocal => create_local_memory_with_conn(
                        &tx,
                        actor,
                        &LocalMemoryInput {
                            memory_type: candidate.memory_type,
                            lane: candidate.lane,
                            title: candidate.title.clone(),
                            body: candidate.body.clone(),
                        },
                        &timestamp,
                    )?,
                    crate::MemoryWriteRoute::RuntimeSession => create_checkpoint_with_conn(
                        &tx,
                        actor,
                        &CheckpointInput {
                            task: candidate.title.clone(),
                            note: candidate.body.clone(),
                        },
                        &timestamp,
                    )?,
                    _ => bail!("import runtime candidate has invalid route {route}"),
                };
                writes.push(crate::ImportWrite::RuntimeRecord {
                    index: candidate.index,
                    record_id: record.id,
                    destination: record.destination,
                });
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(cleanup) = okf::cleanup_okf_proposal_files(&created) {
                return Err(error)
                    .context(format!("import apply failed; cleanup failed: {cleanup}"));
            }
            return Err(error);
        }
        writes.sort_by_key(|write| match write {
            crate::ImportWrite::ProposalFile { index, .. }
            | crate::ImportWrite::RuntimeRecord { index, .. } => *index,
        });
        Ok(ImportApplyResult { plan, writes })
    }

    fn load_import_duplicates(
        &self,
        pending_proposals: &[FileProposalInventoryEntry],
    ) -> Result<Vec<ExistingDuplicate>> {
        let mut entries = Vec::new();
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        for record in okf::read_okf_record_files(self.paths.records_dir())? {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::CanonicalRecord,
                id: record.concept_id,
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&record.draft.body),
            });
        }
        for entry in pending_proposals {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::PendingProposal,
                id: entry.proposal.id.clone(),
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&entry.proposal.body),
            });
        }
        let runtime_records = records_for_runtime_preservation(&self.conn)?;
        let now = self.now();
        let mut runtime = Vec::new();
        for record in runtime_records {
            if record.status == MemoryStatus::Active
                && !expiry::is_expired(record.expires_at.as_deref(), now)?
            {
                runtime.push(record);
            }
        }
        runtime.sort_by(|a, b| a.id.cmp(&b.id));
        for record in runtime {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::RuntimeRecord,
                id: record.id,
                destination: Some(record.destination),
                hash: import::content_hash(&record.body),
            });
        }
        entries.sort_by(|a, b| (a.kind, a.id.as_str()).cmp(&(b.kind, b.id.as_str())));
        Ok(entries)
    }

    pub fn build_context_pack(&self, input: ContextPackInput) -> Result<ContextPack> {
        context::build_context_pack_at(&self.conn, input, self.now())
    }

    pub fn build_handoff_pack(&self, input: HandoffInput) -> Result<HandoffPack> {
        handoff::build_handoff_pack_at(&self.conn, input, self.now())
    }

    pub fn precheck(&self, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
        precheck::precheck_at(&self.conn, input, self.now())
    }

    pub fn export(&self, input: ExportInput) -> Result<ExportResult> {
        let written_paths = match input.format {
            ExportFormat::Okf => exporters::export_okf_at(
                &self.conn,
                &self.paths.exports_dir.join("okf"),
                input.scope_kind,
                self.now(),
            )?,
            ExportFormat::AgentsMd => vec![exporters::export_agents_md_at(
                &self.conn,
                &self.paths.exports_dir.join("AGENTS.memory.md"),
                input.scope_kind,
                self.now(),
            )?],
            ExportFormat::ClaudeMd => vec![exporters::export_claude_md_at(
                &self.conn,
                &self.paths.exports_dir.join("CLAUDE.memory.md"),
                input.scope_kind,
                self.now(),
            )?],
        };

        Ok(ExportResult {
            format: input.format,
            written_paths,
        })
    }

    fn now(&self) -> OffsetDateTime {
        self.clock.now_utc()
    }

    fn now_timestamp(&self) -> Result<String> {
        expiry::format_timestamp(self.now())
    }

    pub fn rebuild(self) -> Result<RebuildResult> {
        let paths = self.paths.clone();
        drop(self);
        Self::rebuild_paths(paths)
    }

    pub fn rebuild_at(start: impl AsRef<Path>) -> Result<RebuildResult> {
        let paths = discover_existing_paths(start)?;
        Self::rebuild_paths(paths)
    }

    pub fn rebuild_paths(paths: MemoryPaths) -> Result<RebuildResult> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&paths)?;
        let records_root = paths.records_dir();
        ensure_safe_directory(
            &paths.project_root,
            &records_root,
            false,
            "canonical record root",
        )?;
        let records = okf::read_okf_record_files(&records_root)?;
        guard_no_open_proposals(&paths.db_path)?;
        let runtime_records = load_runtime_records_for_rebuild(&paths.db_path)?;
        guard_no_runtime_record_id_collisions(&records, &runtime_records)?;
        remove_database_files(&paths.db_path)?;
        let conn = db::open_database(&paths.db_path)?;
        db::init_database(&conn)?;
        okf::import_okf_records(&conn, &records)?;
        restore_runtime_records_after_rebuild(&conn, &runtime_records)?;
        Ok(RebuildResult {
            records_root,
            db_path: paths.db_path,
            record_ids: records
                .into_iter()
                .map(|record| record.concept_id)
                .collect(),
        })
    }
}

fn blocked_session_end_result(document: SessionEndDocument) -> SessionEndResult {
    let candidates = document
        .candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let non_repo_safe = candidate.destination == MemoryDestination::Repo
                && candidate.sensitivity != crate::OkfProposalSensitivity::RepoSafe;
            let (title, status, reason) = if non_repo_safe {
                (
                    "Redacted non-repo-safe candidate".to_owned(),
                    SessionEndCandidateStatus::Blocked,
                    Some(repo_sensitivity_block_reason(candidate.sensitivity)),
                )
            } else {
                let status = match candidate.destination {
                    MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session
                    | MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                };
                let reason = match candidate.destination {
                    MemoryDestination::Discard => {
                        "discard destination performs no write".to_owned()
                    }
                    MemoryDestination::NeedsReview => {
                        "candidate requires human review before writing".to_owned()
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => "session-end batch contains a non-repo-safe repo candidate; no writes were performed".to_owned(),
                };
                (candidate.title.trim().to_owned(), status, Some(reason))
            };
            SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason,
                write: None,
            }
        })
        .collect();
    SessionEndResult {
        task: document.task.trim().to_owned(),
        candidates,
    }
}

impl RepoLifecycleLock {
    fn acquire(paths: &MemoryPaths) -> Result<Self> {
        fs::create_dir_all(&paths.runtime_dir).with_context(|| {
            format!(
                "failed to create runtime directory {}",
                paths.runtime_dir.display()
            )
        })?;
        let lock_path = paths.runtime_dir.join("repo-lifecycle.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open lifecycle lock {}", lock_path.display()))?;
        file.try_lock().with_context(|| {
            format!(
                "another repo lifecycle operation is in progress; retry after {} is unlocked",
                lock_path.display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

pub fn scan_file_proposal_inventory(paths: &MemoryPaths) -> Result<FileProposalInventory> {
    let mut inventory = FileProposalInventory::default();
    let proposals_root = paths.proposals_dir();
    match fs::symlink_metadata(&proposals_root) {
        Ok(_) => {
            if let Err(error) =
                ensure_safe_directory(&paths.project_root, &proposals_root, false, "proposal root")
            {
                inventory.errors.push(FileProposalInventoryError {
                    display_path: proposals_root,
                    error: format!("failed to inspect proposal root: {error}"),
                });
                return Ok(inventory);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(inventory),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect proposal root {}",
                    proposals_root.display()
                )
            });
        }
    }
    let pending_root = paths.proposals_dir().join("pending");
    scan_proposal_directory(
        paths,
        &pending_root,
        crate::OkfProposalStatus::Proposed,
        None,
        "pending proposal root",
        &mut inventory.pending,
        &mut inventory.errors,
    )?;

    let resolved_root = paths.proposals_dir().join("resolved");
    if fs::symlink_metadata(&resolved_root).is_ok()
        && let Err(error) = ensure_safe_directory(
            &paths.project_root,
            &resolved_root,
            false,
            "resolved proposal root",
        )
    {
        inventory.errors.push(FileProposalInventoryError {
            display_path: resolved_root,
            error: format!("failed to inspect resolved proposal root: {error}"),
        });
        return Ok(inventory);
    }
    for (directory, status, outcome) in [
        (
            "applied",
            crate::OkfProposalStatus::Applied,
            OkfProposalOutcome::Applied,
        ),
        (
            "rejected",
            crate::OkfProposalStatus::Rejected,
            OkfProposalOutcome::Rejected,
        ),
    ] {
        scan_proposal_directory(
            paths,
            &resolved_root.join(directory),
            status,
            Some(outcome),
            "resolved proposal root",
            &mut inventory.resolved,
            &mut inventory.errors,
        )?;
    }

    append_duplicate_identity_errors(&inventory.pending, "pending", &mut inventory.errors);
    append_duplicate_identity_errors(&inventory.resolved, "resolved", &mut inventory.errors);
    append_cross_state_identity_errors(
        &inventory.pending,
        &inventory.resolved,
        &mut inventory.errors,
    );
    inventory.pending.sort_by(|left, right| {
        left.proposal
            .id
            .cmp(&right.proposal.id)
            .then_with(|| left.display_path.cmp(&right.display_path))
    });
    inventory.resolved.sort_by(|left, right| {
        left.proposal
            .id
            .cmp(&right.proposal.id)
            .then_with(|| left.display_path.cmp(&right.display_path))
    });
    inventory.errors.sort_by(|left, right| {
        left.display_path
            .cmp(&right.display_path)
            .then_with(|| left.error.cmp(&right.error))
    });
    inventory.errors.dedup();
    Ok(inventory)
}

pub fn lifecycle_transaction_artifact_count(paths: &MemoryPaths) -> Result<usize> {
    Ok(lifecycle_transaction_artifacts(paths)?.len())
}

fn lifecycle_transaction_artifacts(paths: &MemoryPaths) -> Result<Vec<PathBuf>> {
    let mut artifacts = Vec::new();
    for (root, label) in [
        (paths.records_dir(), "canonical record root"),
        (paths.proposals_dir(), "proposal root"),
    ] {
        match fs::symlink_metadata(&root) {
            Ok(_) => ensure_safe_directory(&paths.project_root, &root, false, label)?,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {label} {}", root.display()));
            }
        }
        collect_lifecycle_transaction_artifacts(&root, &mut artifacts)?;
    }
    artifacts.sort();
    Ok(artifacts)
}

fn collect_lifecycle_transaction_artifacts(
    directory: &Path,
    artifacts: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_lifecycle_transaction_artifacts(&path, artifacts)?;
        } else if file_type.is_file()
            && entry.file_name().to_str().is_some_and(|name| {
                name.starts_with('.')
                    && [".write.tmp", ".canonical.tmp", ".pending.tmp"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
            })
        {
            artifacts.push(path);
        }
    }
    Ok(())
}

fn scan_proposal_directory(
    paths: &MemoryPaths,
    root: &Path,
    expected_status: crate::OkfProposalStatus,
    expected_outcome: Option<OkfProposalOutcome>,
    label: &str,
    entries: &mut Vec<FileProposalInventoryEntry>,
    errors: &mut Vec<FileProposalInventoryError>,
) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {label} {}", root.display()));
        }
    }
    if let Err(error) = ensure_safe_directory(&paths.project_root, root, false, label) {
        errors.push(FileProposalInventoryError {
            display_path: root.to_path_buf(),
            error: if expected_outcome.is_some() {
                format!("failed to inspect resolved proposal packet: {error}")
            } else {
                format!("failed to inspect pending proposal root: {error}")
            },
        });
        return Ok(());
    }

    let mut files = Vec::new();
    collect_safe_markdown_files(root, &mut files)?;
    files.sort();
    for actual_path in files {
        let fallback_display_path = okf::redacted_okf_proposal_path(root, &actual_path)
            .unwrap_or_else(|_| root.join("redacted-proposal.md"));
        if ensure_safe_existing_file(&paths.project_root, root, &actual_path, "proposal packet")
            .is_err()
        {
            errors.push(FileProposalInventoryError {
                display_path: fallback_display_path,
                error: "failed to inspect proposal packet".to_owned(),
            });
            continue;
        }
        let markdown = match fs::read_to_string(&actual_path) {
            Ok(markdown) => markdown,
            Err(_) => {
                errors.push(FileProposalInventoryError {
                    display_path: fallback_display_path,
                    error: "failed to read proposal during safety preflight".to_owned(),
                });
                continue;
            }
        };
        let preflight = match okf::preflight_okf_proposal_markdown(root, &actual_path, &markdown) {
            Ok(Some(preflight)) => preflight,
            Ok(None) => continue,
            Err(error) => {
                errors.push(FileProposalInventoryError {
                    display_path: fallback_display_path,
                    error: error.to_string(),
                });
                continue;
            }
        };
        let non_repo_safe = preflight.sensitivity != crate::OkfProposalSensitivity::RepoSafe;
        let display_path = if non_repo_safe {
            root.join(format!("{}.md", preflight.receipt_proposal.file_id))
        } else {
            actual_path.clone()
        };
        let parsed = if non_repo_safe && expected_outcome.is_none() {
            Some(preflight.receipt_proposal.clone())
        } else {
            match okf::parse_okf_proposal_markdown(root, &actual_path, &markdown) {
                Ok(proposal) => proposal,
                Err(error) => {
                    errors.push(FileProposalInventoryError {
                        display_path,
                        error: if non_repo_safe {
                            "invalid redacted resolved proposal packet".to_owned()
                        } else {
                            error.to_string()
                        },
                    });
                    continue;
                }
            }
        };
        if let Some(mut proposal) = parsed {
            if non_repo_safe && expected_outcome.is_some() {
                proposal = redact_resolved_proposal_for_inventory(proposal, preflight);
            }
            let state_error = if proposal.status != expected_status {
                Some(format!(
                    "proposal identity token {} has status {} but this state requires {}",
                    okf::proposal_identity_tokens(&proposal)
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| "redacted-identity-unavailable".to_owned()),
                    proposal.status.as_str(),
                    expected_status.as_str()
                ))
            } else if expected_outcome.is_none() && proposal.resolution.is_some() {
                Some("pending proposal must be unresolved".to_owned())
            } else if let Some(expected_outcome) = expected_outcome {
                match proposal.resolution.as_ref() {
                    Some(resolution) if resolution.outcome == expected_outcome => None,
                    Some(resolution) => Some(format!(
                        "resolved proposal has outcome {} but this state requires {}",
                        resolution.outcome.as_str(),
                        expected_outcome.as_str()
                    )),
                    None => Some("resolved proposal is missing resolution metadata".to_owned()),
                }
            } else {
                None
            };
            if let Some(error) = state_error {
                errors.push(FileProposalInventoryError {
                    display_path,
                    error,
                });
            } else {
                entries.push(FileProposalInventoryEntry {
                    proposal,
                    display_path,
                    actual_path,
                });
            }
        }
    }
    Ok(())
}

fn collect_safe_markdown_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).context("failed to read proposal directory")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.starts_with('.')) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_safe_markdown_files(&path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn require_clean_file_proposal_inventory(inventory: &FileProposalInventory) -> Result<()> {
    if let Some(error) = inventory.errors.first() {
        bail!("{}", error.error);
    }
    Ok(())
}

fn prepare_pending_proposal_root(paths: &MemoryPaths) -> Result<()> {
    preflight_pending_proposal_root(paths)?;
    ensure_safe_directory(
        &paths.project_root,
        &paths.proposals_dir().join("pending"),
        true,
        "pending proposal root",
    )
    .context("failed to inspect pending proposal root")
}

fn preflight_pending_proposal_root(paths: &MemoryPaths) -> Result<()> {
    let pending_root = paths.proposals_dir().join("pending");
    let relative = pending_root
        .strip_prefix(&paths.project_root)
        .context("pending proposal root is outside the project root")?;
    let root_metadata = fs::symlink_metadata(&paths.project_root).with_context(|| {
        format!(
            "failed to inspect project root {}",
            paths.project_root.display()
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!(
            "project root must be a real directory: {}",
            paths.project_root.display()
        );
    }

    let mut current = paths.project_root.clone();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("pending proposal root contains traversal or an unsafe component");
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "pending proposal root ancestor must be a real directory: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect pending proposal root ancestor {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn proposal_inventory_identities(inventory: &FileProposalInventory) -> BTreeSet<String> {
    inventory
        .pending
        .iter()
        .chain(&inventory.resolved)
        .flat_map(|entry| okf::proposal_identity_tokens(&entry.proposal))
        .collect()
}

fn db_proposal_identity_tokens(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = conn.prepare("SELECT id FROM proposal ORDER BY id")?;
    let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut tokens = BTreeSet::new();
    for id in ids {
        tokens.insert(okf::okf_proposal_identity_token(&id?));
    }
    Ok(tokens)
}

fn reserved_proposal_identities(
    conn: &Connection,
    inventory: &FileProposalInventory,
) -> Result<BTreeSet<String>> {
    let mut identities = proposal_inventory_identities(inventory);
    identities.extend(db_proposal_identity_tokens(conn)?);
    Ok(identities)
}

fn ensure_planned_proposals_available<'a>(
    conn: &Connection,
    inventory: &FileProposalInventory,
    plans: impl IntoIterator<Item = &'a okf::OkfCreateProposalPlan>,
) -> Result<()> {
    let existing = reserved_proposal_identities(conn, inventory)?;
    for plan in plans {
        for identity in [&plan.proposal_id, &plan.parsed.file_id] {
            let identity_token = okf::okf_proposal_identity_token(identity);
            if existing.contains(&identity_token) {
                bail!(
                    "proposal packet identity token {identity_token} appeared after planning; recompute the operation"
                );
            }
        }
    }
    Ok(())
}

fn append_duplicate_identity_errors(
    entries: &[FileProposalInventoryEntry],
    state: &str,
    errors: &mut Vec<FileProposalInventoryError>,
) {
    let mut identities = BTreeMap::<String, usize>::new();
    for (index, entry) in entries.iter().enumerate() {
        for identity in okf::proposal_identity_tokens(&entry.proposal) {
            if let Some(previous) = identities.insert(identity.clone(), index)
                && previous != index
            {
                let error = format!("duplicate {state} proposal identity token {identity}");
                errors.push(FileProposalInventoryError {
                    display_path: entries[previous].display_path.clone(),
                    error: error.clone(),
                });
                errors.push(FileProposalInventoryError {
                    display_path: entry.display_path.clone(),
                    error,
                });
            }
        }
    }
}

fn append_cross_state_identity_errors(
    pending: &[FileProposalInventoryEntry],
    resolved: &[FileProposalInventoryEntry],
    errors: &mut Vec<FileProposalInventoryError>,
) {
    for pending_entry in pending {
        for resolved_entry in resolved {
            let pending_identities = okf::proposal_identity_tokens(&pending_entry.proposal);
            let resolved_identities = okf::proposal_identity_tokens(&resolved_entry.proposal);
            let overlap = pending_identities
                .intersection(&resolved_identities)
                .next()
                .cloned();
            if let Some(identity) = overlap {
                let outcome = resolved_entry
                    .proposal
                    .resolution
                    .as_ref()
                    .map(|resolution| resolution.outcome.as_str())
                    .unwrap_or("resolved");
                errors.push(FileProposalInventoryError {
                    display_path: pending_entry.display_path.clone(),
                    error: format!(
                        "pending proposal reintroduces resolved identity token {identity} already {outcome}"
                    ),
                });
            }
        }
    }
}

fn redact_resolved_proposal_for_inventory(
    parsed: OkfProposalFile,
    preflight: okf::OkfProposalPreflight,
) -> OkfProposalFile {
    let mut receipt = preflight.receipt_proposal;
    receipt.status = parsed.status;
    receipt.resolution = parsed.resolution.map(|resolution| OkfProposalResolution {
        outcome: resolution.outcome,
        resolved_by: "redacted".to_owned(),
        resolved_at: "1970-01-01T00:00:00Z".to_owned(),
        reason: Some(
            "Non-repo-safe proposal resolved at the repository trust boundary.".to_owned(),
        ),
        record_id: None,
        target_id: None,
    });
    receipt
}

fn ensure_expected_canonical_bytes(
    proposal: &OkfProposalFile,
    expected: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
    path: &Path,
    actual_markdown: &str,
) -> Result<()> {
    let expected_markdown = okf::render_memory_record_markdown(expected, tags, applies_to);
    if actual_markdown != expected_markdown {
        bail!(
            "resolved proposal {} canonical byte drift at {}",
            proposal.id,
            path.display()
        );
    }
    Ok(())
}

fn ensure_safe_directory(
    project_root: &Path,
    directory: &Path,
    create_missing: bool,
    label: &str,
) -> Result<()> {
    ensure_directory_chain(project_root, directory, create_missing, label)?;
    let canonical_project = fs::canonicalize(project_root).with_context(|| {
        format!(
            "failed to canonicalize project root {}",
            project_root.display()
        )
    })?;
    let canonical_directory = fs::canonicalize(directory)
        .with_context(|| format!("failed to canonicalize {label} {}", directory.display()))?;
    if !canonical_directory.starts_with(&canonical_project) {
        bail!("{label} escapes project root: {}", directory.display());
    }
    Ok(())
}

fn ensure_directory_chain(
    root: &Path,
    directory: &Path,
    create_missing: bool,
    label: &str,
) -> Result<()> {
    let relative = directory.strip_prefix(root).with_context(|| {
        format!(
            "{label} {} is not under trusted root {}",
            directory.display(),
            root.display()
        )
    })?;
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("failed to inspect trusted root {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("trusted root must be a real directory: {}", root.display());
    }

    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("{label} path contains traversal or an unsafe component");
        };
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound && create_missing => {
                fs::create_dir(&current).with_context(|| {
                    format!("failed to create {label} directory {}", current.display())
                })?;
                fs::symlink_metadata(&current)?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {label} {}", current.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "{label} ancestor must be a real directory: {}",
                current.display()
            );
        }
    }
    Ok(())
}

fn ensure_safe_path_parent(
    project_root: &Path,
    trusted_root: &Path,
    path: &Path,
    create_missing_parent: bool,
    label: &str,
) -> Result<()> {
    ensure_safe_directory(project_root, trusted_root, false, label)?;
    let relative = path.strip_prefix(trusted_root).with_context(|| {
        format!(
            "{label} {} is not under trusted root {}",
            path.display(),
            trusted_root.display()
        )
    })?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("{label} path contains traversal or an unsafe component");
    }
    let parent = path.parent().context("safe destination has no parent")?;
    ensure_directory_chain(trusted_root, parent, create_missing_parent, label)?;
    let canonical_root = fs::canonicalize(trusted_root).with_context(|| {
        format!(
            "failed to canonicalize trusted root {}",
            trusted_root.display()
        )
    })?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize {label} parent {}", parent.display()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!(
            "{label} destination escapes trusted root: {}",
            path.display()
        );
    }
    Ok(())
}

fn ensure_safe_existing_file(
    project_root: &Path,
    trusted_root: &Path,
    path: &Path,
    label: &str,
) -> Result<()> {
    ensure_safe_path_parent(project_root, trusted_root, path, false, label)?;
    ensure_regular_file(path, label)
}

fn file_content_hash(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn validate_canonical_write_precondition(
    paths: &MemoryPaths,
    write: &StagedCanonicalFileWrite,
) -> Result<()> {
    let records_root = paths.records_dir();
    ensure_safe_path_parent(
        &paths.project_root,
        &records_root,
        &write.path,
        false,
        "canonical memory record",
    )?;
    match write.mode {
        FileWriteMode::CreateNew => ensure_path_absent(&write.path, "canonical memory record"),
        FileWriteMode::Overwrite => {
            ensure_regular_file(&write.path, "canonical memory record")?;
            let expected = write
                .expected_existing_hash
                .as_deref()
                .context("overwrite write is missing captured canonical hash")?;
            let actual = file_content_hash(&write.path)?;
            if actual != expected {
                bail!(
                    "canonical target changed after validation: {}",
                    write.path.display()
                );
            }
            Ok(())
        }
    }
}

fn install_staged_file_no_replace(staged: &Path, destination: &Path) -> Result<()> {
    fs::hard_link(staged, destination).with_context(|| {
        format!(
            "failed to install {} without replacing an existing file",
            destination.display()
        )
    })?;
    if let Err(error) = fs::remove_file(staged) {
        let install_error = anyhow::Error::new(error).context(format!(
            "failed to finalize no-replace install {}",
            destination.display()
        ));
        return match remove_staged_file(destination) {
            Ok(()) => Err(install_error),
            Err(rollback_error) => Err(install_error).context(format!(
                "additionally failed to roll back {}: {rollback_error:#}",
                destination.display()
            )),
        };
    }
    Ok(())
}

fn repo_record_matches(canonical: &okf::OkfRecordFile, indexed: &MemoryRecord) -> bool {
    let draft = &canonical.draft;
    indexed.memory_type == draft.memory_type
        && indexed.lane == draft.lane
        && indexed.destination == MemoryDestination::Repo
        && indexed.scope_kind == draft.scope_kind
        && indexed.scope_id == draft.scope_id
        && indexed.visibility == draft.visibility
        && indexed.title == draft.title
        && indexed.body == draft.body
        && indexed.status == canonical.status
        && indexed.confidence == draft.confidence
        && indexed.source_kind == draft.source_kind
        && indexed.source_ref == draft.source_ref
        && indexed.proposal_id == canonical.proposal_id
        && indexed.content_hash == blake3::hash(draft.body.as_bytes()).to_hex().to_string()
        && indexed.created_at == canonical.created
        && indexed.updated_at == canonical.updated.as_deref().unwrap_or(&canonical.created)
        && indexed.supersedes_id == canonical.supersedes_id
        && indexed.expires_at == canonical.expires_at
}

fn fts_content_index_is_current(conn: &Connection) -> Result<bool> {
    match conn.execute(
        "INSERT INTO memory_fts(memory_fts, rank) VALUES ('integrity-check', 1)",
        [],
    ) {
        Ok(_) => Ok(true),
        Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt) => {
            Ok(false)
        }
        Err(error) => Err(error).context("failed to verify full-text index integrity"),
    }
}

fn rebuild_fts_content_index(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO memory_fts(memory_fts) VALUES ('rebuild')", [])
        .context("failed to rebuild full-text index")?;
    Ok(())
}

fn validate_resolution_actor(actor: &str) -> Result<()> {
    if actor.trim().is_empty() {
        bail!("proposal-file resolution actor cannot be empty");
    }
    Ok(())
}

fn require_action_reason(proposal: &OkfProposalFile, action: &str) -> Result<()> {
    if proposal
        .proposal
        .reason
        .as_deref()
        .is_none_or(|reason| reason.trim().is_empty())
    {
        bail!("OKF {action} proposals must include proposal.reason");
    }
    Ok(())
}

fn parse_orderable_timestamp(value: &str, label: &str) -> Result<OffsetDateTime> {
    if let Ok(timestamp) = OffsetDateTime::parse(value, &Rfc3339) {
        return Ok(timestamp);
    }
    if value.len() == 10 {
        let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")?;
        if let Ok(date) = Date::parse(value, &format) {
            return Ok(date.with_time(Time::MIDNIGHT).assume_utc());
        }
    }
    bail!("{label} must be an RFC 3339 timestamp or YYYY-MM-DD date: {value:?}")
}

fn validate_legacy_canonical_target(target: &MemoryRecord) -> Result<()> {
    if target.destination != MemoryDestination::Repo {
        bail!(
            "record {} cannot be changed canonically because destination {} is not repo",
            target.id,
            target.destination.as_str()
        );
    }
    if target.visibility == Visibility::Private {
        bail!(
            "record {} cannot be changed canonically because visibility private is not repo-shareable",
            target.id
        );
    }
    if target.status != MemoryStatus::Active {
        bail!(
            "record {} cannot be changed canonically because status {} is not active",
            target.id,
            target.status.as_str()
        );
    }
    Ok(())
}

fn ensure_path_absent(path: &Path, label: &str) -> Result<()> {
    if path
        .try_exists()
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?
    {
        bail!("{label} already exists: {}", path.display());
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} must be a regular file: {}", path.display());
    }
    Ok(())
}

fn sibling_transaction_path(path: &Path, nonce: &str, role: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memzoi");
    path.with_file_name(format!(".{name}.{nonce}.{role}.tmp"))
}

fn stage_file(final_path: &Path, contents: &str, nonce: &str) -> Result<PathBuf> {
    let parent = final_path.parent().context("staged file has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    let temp_path = sibling_transaction_path(final_path, nonce, "write");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .with_context(|| format!("failed to stage file {}", final_path.display()))?;
    if let Err(error) = file
        .write_all(contents.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let stage_error = anyhow::Error::new(error)
            .context(format!("failed to stage file {}", final_path.display()));
        return match remove_staged_file(&temp_path) {
            Ok(()) => Err(stage_error),
            Err(cleanup_error) => Err(stage_error).context(format!(
                "additionally failed to remove incomplete staged file {}: {cleanup_error:#}",
                temp_path.display()
            )),
        };
    }
    Ok(temp_path)
}

fn stage_canonical_writes(
    writes: &[CanonicalFileWrite],
    nonce: &str,
) -> Result<Vec<StagedCanonicalFileWrite>> {
    let mut staged = Vec::with_capacity(writes.len());
    for write in writes {
        let temp_path = match stage_file(&write.path, &write.markdown, nonce) {
            Ok(path) => path,
            Err(error) => {
                return attach_cleanup_error(
                    error,
                    cleanup_staged_canonical_writes(&staged),
                    "partial canonical staging cleanup",
                );
            }
        };
        let backup_path = (write.mode == FileWriteMode::Overwrite)
            .then(|| sibling_transaction_path(&write.path, nonce, "canonical"));
        staged.push(StagedCanonicalFileWrite {
            path: write.path.clone(),
            temp_path,
            backup_path,
            mode: write.mode,
            expected_existing_hash: write.expected_existing_hash.clone(),
            installed: false,
        });
    }
    Ok(staged)
}

fn install_staged_canonical_writes<BeforeInstall>(
    paths: &MemoryPaths,
    writes: &mut [StagedCanonicalFileWrite],
    before_install: BeforeInstall,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
{
    install_staged_canonical_writes_with_backup_hook(paths, writes, before_install, |_, _| Ok(()))
}

fn install_staged_canonical_writes_with_backup_hook<BeforeInstall, AfterBackup>(
    paths: &MemoryPaths,
    writes: &mut [StagedCanonicalFileWrite],
    mut before_install: BeforeInstall,
    mut after_backup: AfterBackup,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    AfterBackup: FnMut(usize, &Path) -> Result<()>,
{
    for (index, write) in writes.iter_mut().enumerate() {
        before_install(index)?;
        validate_canonical_write_precondition(paths, write)?;
        if let Some(backup_path) = &write.backup_path {
            validate_canonical_write_precondition(paths, write)?;
            fs::rename(&write.path, backup_path).with_context(|| {
                format!(
                    "failed to stage canonical memory record {}",
                    write.path.display()
                )
            })?;
            after_backup(index, &write.path)?;
        }
        match write.mode {
            FileWriteMode::CreateNew => {
                install_staged_file_no_replace(&write.temp_path, &write.path)?;
            }
            FileWriteMode::Overwrite => {
                install_staged_file_no_replace(&write.temp_path, &write.path)?;
            }
        }
        write.installed = true;
    }
    Ok(())
}

fn rollback_staged_canonical_writes(writes: &mut [StagedCanonicalFileWrite]) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes.iter_mut().rev() {
        if write.installed {
            record_cleanup_result(
                &mut errors,
                remove_staged_file(&write.path),
                format!("remove installed canonical file {}", write.path.display()),
            );
            write.installed = false;
        }
        if let Some(backup_path) = &write.backup_path
            && backup_path.exists()
        {
            record_cleanup_result(
                &mut errors,
                install_staged_file_no_replace(backup_path, &write.path),
                format!(
                    "restore canonical backup {} to {}",
                    backup_path.display(),
                    write.path.display()
                ),
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("canonical rollback", errors)
}

fn finalize_staged_canonical_writes(writes: &[StagedCanonicalFileWrite]) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes {
        if let Some(backup_path) = &write.backup_path {
            record_cleanup_result(
                &mut errors,
                remove_staged_file(backup_path),
                format!("remove canonical backup {}", backup_path.display()),
            );
        }
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("canonical finalization", errors)
}

fn commit_db_and_canonical_writes(
    paths: &MemoryPaths,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
) -> Result<()> {
    commit_db_and_canonical_writes_with_hooks(paths, tx, writes, |_| Ok(()), |_| Ok(()))
}

fn commit_db_and_canonical_writes_with_hooks<BeforeInstall, BeforeCommit>(
    paths: &MemoryPaths,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
    before_install: BeforeInstall,
    before_commit: BeforeCommit,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    commit_db_and_canonical_writes_with_backup_hook(
        paths,
        tx,
        writes,
        before_install,
        |_, _| Ok(()),
        before_commit,
    )
}

fn commit_db_and_canonical_writes_with_backup_hook<BeforeInstall, AfterBackup, BeforeCommit>(
    paths: &MemoryPaths,
    tx: Transaction<'_>,
    writes: &[CanonicalFileWrite],
    before_install: BeforeInstall,
    after_backup: AfterBackup,
    before_commit: BeforeCommit,
) -> Result<()>
where
    BeforeInstall: FnMut(usize) -> Result<()>,
    AfterBackup: FnMut(usize, &Path) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let nonce = Uuid::now_v7().to_string();
    let mut staged = stage_canonical_writes(writes, &nonce)?;
    if let Err(error) = install_staged_canonical_writes_with_backup_hook(
        paths,
        &mut staged,
        before_install,
        after_backup,
    ) {
        return attach_cleanup_error(
            error,
            rollback_staged_canonical_writes(&mut staged),
            "canonical install rollback",
        );
    }
    if let Err(error) = before_commit(&tx) {
        return attach_cleanup_error(
            error,
            rollback_staged_canonical_writes(&mut staged),
            "canonical pre-commit rollback",
        );
    }
    if let Err(error) = tx.commit() {
        return attach_cleanup_error(
            anyhow::Error::new(error).context("failed to commit memory lifecycle transaction"),
            rollback_staged_canonical_writes(&mut staged),
            "canonical commit rollback",
        );
    }
    finalize_staged_canonical_writes(&staged)
        .context("memory lifecycle committed but canonical cleanup failed")
}

fn remove_staged_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove lifecycle transaction file"),
    }
}

fn cleanup_staged_canonical_writes(writes: &[StagedCanonicalFileWrite]) -> Result<()> {
    let mut errors = Vec::new();
    for write in writes {
        record_cleanup_result(
            &mut errors,
            remove_staged_file(&write.temp_path),
            format!("remove staged canonical file {}", write.temp_path.display()),
        );
    }
    finish_cleanup("staged canonical cleanup", errors)
}

fn cleanup_staged_file_resolution(
    writes: &[StagedCanonicalFileWrite],
    resolved_temp: &Path,
) -> Result<()> {
    let mut errors = Vec::new();
    record_cleanup_result(
        &mut errors,
        cleanup_staged_canonical_writes(writes),
        "clean up staged canonical writes".to_owned(),
    );
    record_cleanup_result(
        &mut errors,
        remove_staged_file(resolved_temp),
        "remove staged resolved packet".to_owned(),
    );
    finish_cleanup("proposal-file staging cleanup", errors)
}

fn rollback_file_resolution(
    pending_path: &Path,
    pending_backup: &Path,
    pending_moved: bool,
    writes: &mut [StagedCanonicalFileWrite],
    resolved_path: &Path,
    resolved_installed: bool,
    resolved_temp: &Path,
) -> Result<()> {
    let mut errors = Vec::new();
    if resolved_installed {
        record_cleanup_result(
            &mut errors,
            remove_staged_file(resolved_path),
            "remove resolved packet".to_owned(),
        );
    }
    record_cleanup_result(
        &mut errors,
        rollback_staged_canonical_writes(writes),
        "roll back canonical writes".to_owned(),
    );
    if pending_moved {
        record_cleanup_result(
            &mut errors,
            install_staged_file_no_replace(pending_backup, pending_path)
                .map_err(|_| anyhow!("failed to restore pending proposal without replacing it")),
            "restore pending packet".to_owned(),
        );
    }
    record_cleanup_result(
        &mut errors,
        remove_staged_file(resolved_temp),
        "remove staged resolved packet".to_owned(),
    );
    finish_cleanup("proposal-file rollback", errors)
}

fn rollback_rejected_file_proposal(
    pending_path: &Path,
    pending_backup: &Path,
    resolved_path: &Path,
    resolved_installed: bool,
    resolved_temp: &Path,
) -> Result<()> {
    rollback_file_resolution(
        pending_path,
        pending_backup,
        true,
        &mut [],
        resolved_path,
        resolved_installed,
        resolved_temp,
    )
}

fn attach_cleanup_error<T>(
    operation_error: anyhow::Error,
    cleanup: Result<()>,
    label: &str,
) -> Result<T> {
    match cleanup {
        Ok(()) => Err(operation_error),
        Err(cleanup_error) => {
            Err(operation_error).context(format!("{label} also failed: {cleanup_error:#}"))
        }
    }
}

fn record_cleanup_result(errors: &mut Vec<String>, result: Result<()>, operation: String) {
    if let Err(error) = result {
        errors.push(format!("{operation}: {error:#}"));
    }
}

fn finish_cleanup(label: &str, errors: Vec<String>) -> Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{label} failed: {}", errors.join("; "))
    }
}

pub fn init_bundle(paths: &MemoryPaths, force: bool) -> Result<InitBundleResult> {
    fs::create_dir_all(&paths.memory_dir).with_context(|| {
        format!(
            "failed to create memory directory {}",
            paths.memory_dir.display()
        )
    })?;
    fs::create_dir_all(paths.records_dir()).with_context(|| {
        format!(
            "failed to create records directory {}",
            paths.records_dir().display()
        )
    })?;
    fs::create_dir_all(&paths.runtime_dir).with_context(|| {
        format!(
            "failed to create runtime directory {}",
            paths.runtime_dir.display()
        )
    })?;
    fs::create_dir_all(&paths.exports_dir).with_context(|| {
        format!(
            "failed to create exports directory {}",
            paths.exports_dir.display()
        )
    })?;

    if paths.config_path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            paths.config_path.display()
        );
    }

    if force || !paths.config_path.exists() {
        fs::write(&paths.config_path, default_config())
            .with_context(|| format!("failed to write config {}", paths.config_path.display()))?;
    }

    let conn = db::open_database(&paths.db_path)?;
    db::init_database(&conn)?;

    Ok(InitBundleResult {
        project_root: paths.project_root.clone(),
        memory_dir: paths.memory_dir.clone(),
        runtime_dir: paths.runtime_dir.clone(),
        config_path: paths.config_path.clone(),
        db_path: paths.db_path.clone(),
        exports_dir: paths.exports_dir.clone(),
    })
}

fn record_tags(conn: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag FROM memory_tag WHERE record_id = ?1 ORDER BY tag ASC")?;
    let rows = stmt.query_map([record_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertMode {
    Create,
    RestoreIfAbsent,
}

fn create_local_memory_with_conn(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
) -> Result<MemoryRecord> {
    validate_local_memory_input(input)?;
    let id = next_prefixed_record_id(conn, "local", &input.title)?;
    let body = input.body.trim().to_owned();
    let record = MemoryRecord {
        id,
        memory_type: input.memory_type,
        lane: input.lane,
        destination: MemoryDestination::Local,
        scope_kind: ScopeKind::Personal,
        scope_id: None,
        visibility: Visibility::Private,
        title: input.title.trim().to_owned(),
        body,
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind: Some("memzoi-local".to_owned()),
        source_ref: None,
        proposal_id: None,
        content_hash: blake3::hash(input.body.trim().as_bytes())
            .to_hex()
            .to_string(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
        supersedes_id: None,
        expires_at: None,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.local_created".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
                "title": &record.title,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn create_checkpoint_with_conn(
    conn: &Connection,
    actor: &str,
    input: &CheckpointInput,
    now: &str,
) -> Result<MemoryRecord> {
    validate_checkpoint_input(input)?;
    let id = next_prefixed_record_id(conn, "session", &input.task)?;
    let body = input.note.trim().to_owned();
    let record = MemoryRecord {
        id,
        memory_type: MemoryType::Episode,
        lane: MemoryLane::Session,
        destination: MemoryDestination::Session,
        scope_kind: ScopeKind::Personal,
        scope_id: None,
        visibility: Visibility::Private,
        title: input.task.trim().to_owned(),
        body,
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind: Some("memzoi-checkpoint".to_owned()),
        source_ref: None,
        proposal_id: None,
        content_hash: blake3::hash(input.note.trim().as_bytes())
            .to_hex()
            .to_string(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
        supersedes_id: None,
        expires_at: None,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.checkpoint_created".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
                "title": &record.title,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn validate_local_memory_input(input: &LocalMemoryInput) -> Result<()> {
    if input.title.trim().is_empty() {
        bail!("title is required");
    }
    if input.body.trim().is_empty() {
        bail!("body is required");
    }
    Ok(())
}

fn validate_checkpoint_input(input: &CheckpointInput) -> Result<()> {
    if input.task.trim().is_empty() {
        bail!("task is required");
    }
    if input.note.trim().is_empty() {
        bail!("note is required");
    }
    Ok(())
}

fn next_prefixed_record_id(conn: &Connection, prefix: &str, title: &str) -> Result<String> {
    let slug = proposals::title_to_concept_slug(title)
        .unwrap_or_else(|| format!("memory-{}", Uuid::now_v7()));
    let base = format!("{prefix}-{slug}");
    if !record_id_exists(conn, &base)? {
        return Ok(base);
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !record_id_exists(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix search returns")
}

fn record_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

fn insert_memory_record_row(
    conn: &Connection,
    record: &MemoryRecord,
    mode: InsertMode,
) -> Result<()> {
    let verb = match mode {
        InsertMode::Create => "INSERT INTO",
        InsertMode::RestoreIfAbsent => "INSERT OR IGNORE INTO",
    };
    let sql = format!(
        "{verb} memory_record (
          id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
          confidence, source_kind, source_ref, proposal_id, content_hash, created_at, updated_at,
          supersedes_id, expires_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"
    );
    conn.execute(
        &sql,
        rusqlite::params![
            &record.id,
            record.memory_type.as_str(),
            record.lane.as_str(),
            record.destination.as_str(),
            record.scope_kind.as_str(),
            &record.scope_id,
            record.visibility.as_str(),
            &record.title,
            &record.body,
            record.status.as_str(),
            record.confidence,
            &record.source_kind,
            &record.source_ref,
            &record.proposal_id,
            &record.content_hash,
            &record.created_at,
            &record.updated_at,
            &record.supersedes_id,
            &record.expires_at,
        ],
    )?;
    Ok(())
}

fn indexed_active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = ?1
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([destination.as_str()], search::record_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
    now: OffsetDateTime,
) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = ?1
           AND memzoi_is_expired(expires_at, ?2) = 0
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![destination.as_str(), expiry::format_timestamp(now)?],
        search::record_from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn active_checkpoint_records(conn: &Connection, now: OffsetDateTime) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND memzoi_is_expired(expires_at, ?1) = 0
         ORDER BY created_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([expiry::format_timestamp(now)?], search::record_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn checkpoint_record(
    conn: &Connection,
    record_id: &str,
    now: OffsetDateTime,
) -> Result<Option<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE id = ?1
           AND status = 'active'
           AND destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND memzoi_is_expired(expires_at, ?2) = 0",
    )?;
    stmt.query_row(
        rusqlite::params![record_id, expiry::format_timestamp(now)?],
        search::record_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn record_by_id(conn: &Connection, record_id: &str) -> Result<Option<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE id = ?1",
    )?;
    stmt.query_row([record_id], search::record_from_row)
        .optional()
        .map_err(Into::into)
}

fn records_for_runtime_preservation(conn: &Connection) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE destination IN ('local', 'session')
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([], search::record_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_runtime_records_for_rebuild(db_path: &Path) -> Result<Vec<MemoryRecord>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = db::open_database(db_path).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be preserved from {}",
            db_path.display()
        )
    })?;
    db::init_database(&conn).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be migrated before preservation from {}",
            db_path.display()
        )
    })?;
    records_for_runtime_preservation(&conn).context(
        "rebuild refused because local/session runtime memory could not be loaded for preservation",
    )
}

fn guard_no_runtime_record_id_collisions(
    records: &[okf::OkfRecordFile],
    runtime_records: &[MemoryRecord],
) -> Result<()> {
    if runtime_records.is_empty() {
        return Ok(());
    }

    let repo_ids = records
        .iter()
        .map(|record| record.concept_id.as_str())
        .collect::<BTreeSet<_>>();
    let collisions = runtime_records
        .iter()
        .filter_map(|record| {
            repo_ids
                .contains(record.id.as_str())
                .then_some(record.id.as_str())
        })
        .collect::<Vec<_>>();
    if collisions.is_empty() {
        return Ok(());
    }

    bail!(
        "rebuild refused because local/session runtime memory record id{} would collide with canonical repo record{}: {}",
        if collisions.len() == 1 { "" } else { "s" },
        if collisions.len() == 1 { "" } else { "s" },
        collisions.join(", ")
    );
}

fn restore_runtime_records_after_rebuild(
    conn: &Connection,
    records: &[MemoryRecord],
) -> Result<()> {
    for record in records {
        insert_memory_record_row(conn, record, InsertMode::RestoreIfAbsent)?;
    }
    Ok(())
}

fn guard_no_open_proposals(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }

    let Ok(open_proposals) = open_proposal_summaries(db_path) else {
        return Ok(());
    };
    if !open_proposals.is_empty() {
        let count = open_proposals.len();
        let summaries = open_proposals
            .into_iter()
            .map(|(id, status)| format!("{id} ({status})"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "rebuild refused because {count} open proposal{} would be discarded: {summaries}. Run `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason \"...\"` before rebuilding.",
            if count == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn open_proposal_summaries(db_path: &Path) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_proposal_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'proposal')",
        [],
        |row| row.get(0),
    )?;
    if !has_proposal_table {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, status
         FROM proposal
         WHERE status IN ('pending', 'validated', 'approved')
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn remove_database_files(db_path: &Path) -> Result<()> {
    for path in [
        db_path.to_path_buf(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to remove derived database file {}", path.display())
                });
            }
        }
    }
    Ok(())
}

fn default_config() -> &'static str {
    r#"version = 1
scope_kind = "repo"

[exports]
okf = "exports/okf"
agents_md = "exports/AGENTS.memory.md"
claude_md = "exports/CLAUDE.memory.md"
"#
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        ImportCandidateInput, MemoryLane, MemoryStatus, MemoryType, OkfProposalSensitivity,
        OkfProposalSource, ProposalStatus, ScopeKind, SessionEndCandidate, Visibility,
    };
    use tempfile::TempDir;

    #[test]
    fn repo_lifecycle_lock_refuses_concurrent_mutation() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let _first = RepoLifecycleLock::acquire(&service.paths)?;
        let error = RepoLifecycleLock::acquire(&service.paths)
            .err()
            .context("second lifecycle lock should be refused")?;
        assert!(
            error
                .to_string()
                .contains("another repo lifecycle operation is in progress"),
            "unexpected lock contention error: {error:#}"
        );
        Ok(())
    }

    #[test]
    fn repo_packet_writers_share_the_lifecycle_lock() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let _first = RepoLifecycleLock::acquire(&service.paths)?;

        let session_error = service
            .promote_session_end(
                "agent:red-tests",
                repo_session_document("Locked session packet"),
            )
            .expect_err("session-end repo writes must contend on the lifecycle lock");
        assert!(
            session_error
                .to_string()
                .contains("another repo lifecycle operation is in progress")
        );

        let import_error = service
            .apply_import(
                "agent:red-tests",
                repo_import_document("Locked import packet"),
                "unused-plan-id",
            )
            .expect_err("import repo writes must contend on the lifecycle lock");
        assert!(
            import_error
                .to_string()
                .contains("another repo lifecycle operation is in progress")
        );
        assert!(
            scan_file_proposal_inventory(&service.paths)?
                .pending
                .is_empty()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn repo_packet_writers_refuse_symlinked_pending_root() -> anyhow::Result<()> {
        for writer in ["session-end", "import"] {
            let (_temp, service) = initialized_service()?;
            let outside = TempDir::new()?;
            fs::create_dir_all(service.paths.proposals_dir())?;
            std::os::unix::fs::symlink(
                outside.path(),
                service.paths.proposals_dir().join("pending"),
            )?;

            let error = match writer {
                "session-end" => service
                    .promote_session_end(
                        "agent:red-tests",
                        repo_session_document("Escaped session packet"),
                    )
                    .expect_err("session-end must refuse the symlinked pending root"),
                "import" => service
                    .apply_import(
                        "agent:red-tests",
                        repo_import_document("Escaped import packet"),
                        "unused-plan-id",
                    )
                    .expect_err("import must refuse the symlinked pending root"),
                _ => unreachable!(),
            };
            assert!(
                format!("{error:#}").contains("ancestor must be a real directory"),
                "unexpected {writer} containment error: {error:#}"
            );
            assert_eq!(fs::read_dir(outside.path())?.count(), 0);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn canonical_readers_refuse_symlinked_records_root_before_reading_outside() -> anyhow::Result<()>
    {
        for operation in ["doctor", "rebuild"] {
            let (_temp, service) = initialized_service()?;
            let outside = TempDir::new()?;
            let sentinel = "OUTSIDE-CANONICAL-CONTENT-SENTINEL";
            fs::write(outside.path().join("outside.md"), sentinel)?;
            fs::remove_dir_all(service.paths.records_dir())?;
            std::os::unix::fs::symlink(outside.path(), service.paths.records_dir())?;

            let error = match operation {
                "doctor" => service
                    .repo_index_drift()
                    .expect_err("index drift must refuse a symlinked canonical root"),
                "rebuild" => service
                    .rebuild()
                    .expect_err("rebuild must refuse a symlinked canonical root"),
                _ => unreachable!(),
            };
            let rendered = format!("{error:#}");
            assert!(rendered.contains("must be a real directory"), "{rendered}");
            assert!(!rendered.contains(sentinel), "{rendered}");
        }
        Ok(())
    }

    #[test]
    fn repo_packet_planning_reserves_metadata_identities_from_resolved_packets()
    -> anyhow::Result<()> {
        let (_session_temp, session_service) = initialized_service()?;
        let session_pending = write_test_pending_proposal_with_id(
            &session_service,
            "mem_session_reused-title",
            "Previously reviewed session packet",
            OkfProposalSensitivity::RepoSafe,
        )?;
        let session_renamed = session_pending.with_file_name("different-session-file.md");
        fs::rename(&session_pending, &session_renamed)?;
        session_service.reject_file_proposal(
            &session_renamed,
            "reviewer:human",
            "Terminal identity fixture",
        )?;
        let session = session_service
            .promote_session_end("agent:red-tests", repo_session_document("Reused title"))?;
        assert!(matches!(
            session.candidates[0].write.as_ref(),
            Some(SessionEndWrite::ProposalFile { proposal_id, .. })
                if proposal_id == "mem_session_reused-title-2"
        ));

        let (_import_temp, import_service) = initialized_service()?;
        let import_pending = write_test_pending_proposal_with_id(
            &import_service,
            "mem_import_reused-title",
            "Previously reviewed import packet",
            OkfProposalSensitivity::RepoSafe,
        )?;
        let import_renamed = import_pending.with_file_name("different-import-file.md");
        fs::rename(&import_pending, &import_renamed)?;
        import_service.reject_file_proposal(
            &import_renamed,
            "reviewer:human",
            "Terminal identity fixture",
        )?;
        let document = repo_import_document("Reused title");
        let plan = import_service.plan_import("agent:red-tests", document.clone())?;
        assert!(matches!(
            &plan.candidates[0].action,
            crate::ImportCandidateAction::CreateProposal { proposal_id, .. }
                if proposal_id == "mem_import_reused-title-2"
        ));
        let applied = import_service.apply_import("agent:red-tests", document, &plan.plan_id)?;
        assert!(matches!(
            &applied.writes[0],
            crate::ImportWrite::ProposalFile { proposal_id, .. }
                if proposal_id == "mem_import_reused-title-2"
        ));
        Ok(())
    }

    #[test]
    fn repo_packet_planning_reserves_hash_only_receipt_aliases_and_database_proposal_ids()
    -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let raw_id = "mem_session_hash-reserved";
        let pending = write_test_pending_proposal_with_id(
            &service,
            raw_id,
            "Previously rejected unsafe packet",
            OkfProposalSensitivity::Secret,
        )?;
        service.reject_file_proposal(
            &pending,
            "reviewer:human",
            "Hash-only terminal identity fixture",
        )?;

        let session = service
            .promote_session_end("agent:red-tests", repo_session_document("Hash reserved"))?;
        assert!(matches!(
            session.candidates[0].write.as_ref(),
            Some(SessionEndWrite::ProposalFile { proposal_id, .. })
                if proposal_id == "mem_session_hash-reserved-2"
        ));

        service.conn.execute(
            "INSERT INTO proposal (id, operation, payload_json, status, actor)
             VALUES ('mem_import_db-reserved', 'create', '{}', 'pending', 'agent:red-tests')",
            [],
        )?;
        let plan = service.plan_import("agent:red-tests", repo_import_document("Db reserved"))?;
        assert!(matches!(
            &plan.candidates[0].action,
            crate::ImportCandidateAction::CreateProposal { proposal_id, .. }
                if proposal_id == "mem_import_db-reserved-2"
        ));
        Ok(())
    }

    #[test]
    fn file_create_refuses_to_replace_a_local_runtime_row() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let record_id = "runtime-collision-create";
        insert_runtime_record_with_id(&service, record_id, MemoryDestination::Local)?;
        let pending = write_test_pending_proposal_with_id(
            &service,
            "mem_runtime_collision_create",
            "Runtime collision create",
            OkfProposalSensitivity::RepoSafe,
        )?;

        let error = service
            .apply_file_proposal(&pending, "agent:applier")
            .expect_err("file apply must not replace local memory");
        assert!(error.to_string().contains("owned by non-repo memory"));
        assert!(pending.is_file());
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{record_id}.md"))
                .exists()
        );
        let runtime = record_by_id(&service.conn, record_id)?.context("runtime row survived")?;
        assert_eq!(runtime.destination, MemoryDestination::Local);
        assert_eq!(runtime.body, "Runtime collision sentinel");
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn file_supersede_runtime_collision_rolls_back_target_and_replacement() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft("Supersede collision target", "Original target body"),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let replacement_id = "supersede-collision-replacement";
        insert_runtime_record_with_id(&service, replacement_id, MemoryDestination::Session)?;
        let pending = write_test_supersede_proposal(
            &service,
            "mem_supersede_runtime_collision",
            "Supersede collision replacement",
            &target.id,
        )?;

        let error = service
            .apply_file_proposal(&pending, "agent:applier")
            .expect_err("supersede must not replace session memory");
        assert!(error.to_string().contains("owned by non-repo memory"));
        assert_eq!(fs::read(&target_path)?, target_before);
        assert_eq!(
            record_by_id(&service.conn, &target.id)?
                .context("target row survived")?
                .status,
            MemoryStatus::Active
        );
        let runtime = record_by_id(&service.conn, replacement_id)?
            .context("session collision row survived")?;
        assert_eq!(runtime.destination, MemoryDestination::Session);
        assert!(pending.is_file());
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn file_apply_refuses_database_proposal_identity_collision() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let proposal_id = "mem_db_proposal_collision";
        service.conn.execute(
            "INSERT INTO proposal (id, operation, payload_json, status, actor)
             VALUES (?1, 'create', '{}', 'pending', 'agent:red-tests')",
            [proposal_id],
        )?;
        let pending = write_test_pending_proposal_with_id(
            &service,
            proposal_id,
            "Database proposal collision",
            OkfProposalSensitivity::RepoSafe,
        )?;

        let error = service
            .apply_file_proposal(&pending, "agent:applier")
            .expect_err("ambiguous proposal lineage must be rejected");
        assert!(
            error
                .to_string()
                .contains("conflicts with a database proposal")
        );
        assert!(!error.to_string().contains(proposal_id));
        assert!(pending.is_file());
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn apply_revalidates_pending_bytes_immediately_before_resolution() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let pending = write_test_pending_proposal(
            &service,
            "Pending apply race",
            OkfProposalSensitivity::RepoSafe,
        )?;
        let mutated = b"concurrent human edit";

        let error = service
            .apply_file_proposal_with_hooks(
                &pending,
                "agent:applier",
                |path| fs::write(path, mutated).map_err(Into::into),
                |_| Ok(()),
            )
            .expect_err("changed pending bytes must abort apply");
        assert!(error.to_string().contains("changed after validation"));
        assert_eq!(fs::read(&pending)?, mutated);
        assert!(
            !service
                .paths
                .records_dir()
                .join("pending-apply-race.md")
                .exists()
        );
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn unsafe_reject_revalidates_pending_bytes_without_leaking_raw_identity() -> anyhow::Result<()>
    {
        let (_temp, service) = initialized_service()?;
        let original = write_test_pending_proposal_with_id(
            &service,
            "mem_unsafe_race",
            "Unsafe race packet",
            OkfProposalSensitivity::Secret,
        )?;
        let raw_id = "SECRET-RAW-ID-SENTINEL";
        let raw_file = service
            .paths
            .proposals_dir()
            .join("pending/SECRET-RAW-FILE-SENTINEL.md");
        let markdown = fs::read_to_string(&original)?
            .replace("id: mem_unsafe_race", &format!("id: {raw_id}"))
            .replace(
                "Lifecycle finalization must never hide cleanup failures.",
                "SECRET-BODY-SENTINEL",
            );
        fs::write(&raw_file, markdown)?;
        fs::remove_file(&original)?;
        let mutated = b"SECRET-CONCURRENT-EDIT-SENTINEL";

        let error = service
            .reject_file_proposal_with_hooks(
                &raw_file,
                "reviewer:human",
                "Reject unsafe race",
                |path| fs::write(path, mutated).map_err(Into::into),
                |_| Ok(()),
            )
            .expect_err("changed unsafe pending bytes must abort rejection");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("changed after validation"), "{rendered}");
        for forbidden in [
            raw_id,
            "SECRET-RAW-FILE-SENTINEL",
            "SECRET-BODY-SENTINEL",
            "SECRET-CONCURRENT-EDIT-SENTINEL",
        ] {
            assert!(!rendered.contains(forbidden), "{rendered}");
        }
        assert_eq!(fs::read(&raw_file)?, mutated);
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn overwrite_install_never_replaces_a_file_recreated_after_backup() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft("Backup race target", "Original target body"),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let mut replacement = target.clone();
        replacement.status = MemoryStatus::Superseded;
        replacement.updated_at = "2026-07-10T12:00:00Z".to_owned();
        let write = service.prepare_record_file_write_with_conn(
            &service.conn,
            &replacement,
            FileWriteMode::Overwrite,
        )?;
        let tx = service.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
            [&target.id],
        )?;

        let error = commit_db_and_canonical_writes_with_backup_hook(
            &service.paths,
            tx,
            &[write],
            |_| Ok(()),
            |_, path| fs::write(path, "fresh editor bytes").map_err(Into::into),
            |_| Ok(()),
        )
        .expect_err("no-replace overwrite must refuse the recreated target");
        assert!(format!("{error:#}").contains("without replacing"));
        assert_eq!(fs::read_to_string(&target_path)?, "fresh editor bytes");
        assert_eq!(
            record_by_id(&service.conn, &target.id)?
                .context("target row survived")?
                .status,
            MemoryStatus::Active
        );
        let artifacts = lifecycle_transaction_artifacts(&service.paths)?;
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert!(
            artifacts[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".canonical.tmp"))
        );
        Ok(())
    }

    #[test]
    fn rejection_finalization_failure_restores_raw_pending_packet() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let pending = write_test_pending_proposal(
            &service,
            "Rejected cleanup packet",
            OkfProposalSensitivity::Secret,
        )?;
        let original = fs::read(&pending)?;

        let error = service
            .reject_file_proposal_with_finalize_hook(
                &pending,
                "reviewer:human",
                "Unsafe content must remain pending when cleanup fails.",
                |_| Err(anyhow::anyhow!("injected raw-backup cleanup failure")),
            )
            .expect_err("rejection cleanup failure must be observable");
        assert!(error.to_string().contains("finalization was interrupted"));
        assert_eq!(fs::read(&pending)?, original);
        assert!(
            !service
                .paths
                .proposals_dir()
                .join("resolved/rejected/rejected-cleanup-packet.md")
                .exists()
        );
        assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
        Ok(())
    }

    #[test]
    fn unsafe_rejection_cleanup_failure_never_exposes_raw_backup_path() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let original = write_test_pending_proposal_with_id(
            &service,
            "mem_cleanup_leak",
            "Unsafe cleanup leak packet",
            OkfProposalSensitivity::Secret,
        )?;
        let raw_id = "SECRET-CLEANUP-ID-SENTINEL";
        let raw_file = service
            .paths
            .proposals_dir()
            .join("pending/SECRET-CLEANUP-FILE-SENTINEL.md");
        let markdown = fs::read_to_string(&original)?
            .replace("id: mem_cleanup_leak", &format!("id: {raw_id}"))
            .replace(
                "Lifecycle finalization must never hide cleanup failures.",
                "SECRET-CLEANUP-BODY-SENTINEL",
            );
        fs::write(&raw_file, markdown)?;
        fs::remove_file(&original)?;

        let error = service
            .reject_file_proposal_with_finalize_hook(
                &raw_file,
                "reviewer:human",
                "Exercise cleanup diagnostics",
                |backup| {
                    fs::remove_file(backup)?;
                    fs::create_dir(backup)?;
                    Ok(())
                },
            )
            .expect_err("backup cleanup failure must be reported");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("rejection cleanup rollback"),
            "{rendered}"
        );
        for forbidden in [
            raw_id,
            "SECRET-CLEANUP-FILE-SENTINEL",
            "SECRET-CLEANUP-BODY-SENTINEL",
        ] {
            assert!(!rendered.contains(forbidden), "{rendered}");
        }
        Ok(())
    }

    #[test]
    fn applied_finalization_failure_is_reported_and_discoverable() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let pending = write_test_pending_proposal(
            &service,
            "Applied cleanup packet",
            OkfProposalSensitivity::RepoSafe,
        )?;

        let error = service
            .apply_file_proposal_with_finalize_hook(&pending, "agent:applier", |_| {
                Err(anyhow::anyhow!("injected committed cleanup failure"))
            })
            .expect_err("committed cleanup failure must be observable");
        assert!(
            error
                .to_string()
                .contains("apply committed but finalization was interrupted")
        );
        assert!(!pending.exists());
        assert!(
            service
                .paths
                .records_dir()
                .join("applied-cleanup-packet.md")
                .is_file()
        );
        assert!(
            service
                .paths
                .proposals_dir()
                .join("resolved/applied/applied-cleanup-packet.md")
                .is_file()
        );
        let artifacts = lifecycle_transaction_artifacts(&service.paths)?;
        assert_eq!(artifacts.len(), 1, "unexpected artifacts: {artifacts:?}");
        assert!(
            artifacts[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".pending.tmp"))
        );
        Ok(())
    }

    #[test]
    fn changed_canonical_target_fails_captured_hash_revalidation() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let path = service.paths.records_dir().join("captured-target.md");
        fs::write(&path, "original canonical bytes")?;
        let staged = path.with_file_name(".captured-target.write.tmp");
        fs::write(&staged, "replacement bytes")?;
        let write = StagedCanonicalFileWrite {
            path: path.clone(),
            temp_path: staged,
            backup_path: Some(path.with_file_name(".captured-target.backup.tmp")),
            mode: FileWriteMode::Overwrite,
            expected_existing_hash: Some(file_content_hash(&path)?),
            installed: false,
        };
        fs::write(&path, "concurrent human edit")?;

        let error = validate_canonical_write_precondition(&service.paths, &write)
            .expect_err("changed target must fail before install");
        assert!(
            error.to_string().contains("changed after validation"),
            "unexpected changed-target error: {error:#}"
        );
        assert_eq!(fs::read_to_string(&path)?, "concurrent human edit");
        Ok(())
    }

    #[test]
    fn create_new_install_never_replaces_a_concurrent_destination() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let staged = temp.path().join("staged.tmp");
        let destination = temp.path().join("record.md");
        fs::write(&staged, "new proposal bytes")?;
        fs::write(&destination, "concurrent canonical bytes")?;

        let error = install_staged_file_no_replace(&staged, &destination)
            .expect_err("no-replace install must refuse a concurrent destination");
        assert!(error.to_string().contains("without replacing"));
        assert_eq!(
            fs::read_to_string(&destination)?,
            "concurrent canonical bytes"
        );
        assert_eq!(fs::read_to_string(&staged)?, "new proposal bytes");
        Ok(())
    }

    #[test]
    fn propose_with_options_auto_approves_unique_proposals_by_default() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Default auto proposal", "Unique default auto proposal body"),
            ProposeOptions {
                approval_override: None,
                apply: false,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Approved);
        assert_eq!(
            result
                .validation
                .as_ref()
                .map(|validation| validation.is_valid),
            Some(true)
        );
        assert_eq!(result.record, None);
        assert!(!result.applied);
        let approved =
            service.list_proposals(ProposalStatusFilter::Status(ProposalStatus::Approved))?;
        assert_eq!(
            approved
                .iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>(),
            vec![result.proposal.id.as_str()]
        );

        Ok(())
    }

    #[test]
    fn auto_approval_and_apply_cannot_bypass_unknown_sensitivity() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let mut draft = sample_memory_draft(
            "Unknown sensitivity proposal",
            "Content must remain outside canonical memory until classified.",
        );
        draft.sensitivity = crate::OkfProposalSensitivity::Unknown;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Auto),
                apply: true,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Pending);
        assert!(!result.applied);
        assert_eq!(result.record, None);
        assert!(result.validation.as_ref().is_some_and(|validation| {
            validation
                .issues
                .iter()
                .any(|issue| issue.code == "repo_sensitivity_required")
        }));
        let record_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(record_count, 0);

        Ok(())
    }

    #[test]
    fn propose_with_options_manual_override_leaves_proposal_pending() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Manual proposal", "Manual proposal body"),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Pending);
        assert_eq!(result.validation, None);
        assert_eq!(result.record, None);
        assert!(!result.applied);
        let counts = service.open_proposal_counts()?;
        assert_eq!(counts.get(&ProposalStatus::Pending), Some(&1));

        Ok(())
    }

    #[test]
    fn propose_with_options_apply_writes_canonical_record_file() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Applied proposal", "Applied proposal body"),
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;

        let record = result
            .record
            .as_ref()
            .expect("apply mode should return the canonical record");
        assert!(result.applied);
        assert_eq!(result.proposal.status, ProposalStatus::Applied);
        assert_eq!(record.status, MemoryStatus::Active);
        assert_eq!(record.title, "Applied proposal");
        assert_eq!(record.source_ref.as_deref(), Some("service-proposal-tests"));
        assert_eq!(record.source_kind.as_deref(), Some("test"));
        assert_eq!(
            record.proposal_id.as_deref(),
            Some(result.proposal.id.as_str())
        );

        let mut applied_event_proposal_ids = Vec::new();
        service.for_each_event(|event| {
            if event.event_type == "memory.applied" {
                applied_event_proposal_ids.push(event.proposal_id);
            }
            Ok(())
        })?;
        assert_eq!(
            applied_event_proposal_ids,
            vec![Some(result.proposal.id.clone())]
        );

        let record_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", record.id));
        let canonical = fs::read_to_string(&record_path)?;
        assert!(
            canonical.contains("status: active\n"),
            "canonical record should be written as an active OKF record: {canonical}"
        );
        assert!(
            canonical.contains("# Applied proposal"),
            "canonical record should include the approved title: {canonical}"
        );
        assert!(
            canonical.contains("Applied proposal body"),
            "canonical record should include the approved body: {canonical}"
        );
        assert!(
            canonical.contains("source_ref: service-proposal-tests\n"),
            "canonical record should preserve the evidence reference: {canonical}"
        );
        assert!(
            canonical.contains(&format!("proposal_id: {}\n", result.proposal.id)),
            "canonical record should store proposal lineage separately: {canonical}"
        );

        Ok(())
    }

    #[test]
    fn evidence_provenance_and_proposal_lineage_survive_rebuild_and_export() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let paths = service.paths.clone();
        let mut draft = sample_memory_draft(
            "Lineage survives rebuild",
            "\n  Evidence-backed zircon lineage.  \n",
        );
        draft.source_kind = Some("  test  ".to_owned());
        draft.source_ref = Some("  service-proposal-tests  ".to_owned());
        let applied = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;
        let proposal_id = applied.proposal.id.clone();
        let applied_record = applied.record.expect("record should be applied");
        let record_id = applied_record.id.clone();
        let expected_hash = blake3::hash("Evidence-backed zircon lineage.".as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(applied_record.content_hash, expected_hash);
        assert!(
            service.repo_index_drift()?.is_current(),
            "normalized proposal apply must agree with its canonical file immediately"
        );

        service.rebuild()?;
        let rebuilt = MemoryService::open_paths(paths)?;
        let results = rebuilt.search_memory(SearchInput {
            query: "zircon lineage".to_owned(),
            scope_kind: Some(ScopeKind::Repo),
            scope_id: None,
            memory_type: None,
            lane: None,
            destination: Some(MemoryDestination::Repo),
            path_prefix: None,
            limit: 10,
            include_inactive: false,
        })?;
        let result = results
            .iter()
            .find(|result| result.record.id == record_id)
            .expect("rebuilt recall should return the applied record");
        assert_eq!(result.record.source_kind.as_deref(), Some("test"));
        assert_eq!(
            result.record.source_ref.as_deref(),
            Some("service-proposal-tests")
        );
        assert_eq!(
            result.record.proposal_id.as_deref(),
            Some(proposal_id.as_str())
        );
        assert_eq!(result.record.content_hash, expected_hash);
        assert_eq!(
            result.citations[0].source_ref.as_deref(),
            Some("service-proposal-tests"),
            "recall citations must point at original evidence, not the review packet"
        );

        let exported = rebuilt.export(ExportInput {
            format: ExportFormat::Okf,
            scope_kind: ScopeKind::Repo,
        })?;
        let markdown = fs::read_to_string(&exported.written_paths[0])?;
        assert!(markdown.contains("source_kind: \"test\""));
        assert!(markdown.contains("source_ref: \"service-proposal-tests\""));
        assert!(markdown.contains(&format!("proposal_id: \"{proposal_id}\"")));

        Ok(())
    }

    #[test]
    fn propose_with_options_rejects_manual_apply_without_creating_proposal() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let error = service
            .propose_memory_with_options(
                "agent:red-tests",
                sample_memory_draft("Manual apply proposal", "Manual apply proposal body"),
                ProposeOptions {
                    approval_override: Some(ProposalApprovalOverride::Manual),
                    apply: true,
                },
            )
            .expect_err("manual apply should fail before creating an unappliable proposal");

        let message = error.to_string();
        assert!(
            message.contains("proposal apply mode requires auto approval"),
            "manual apply error should explain the auto-approval requirement: {message}"
        );
        assert!(
            message.contains("manual proposals must be approved before apply"),
            "manual apply error should tell callers how manual proposals progress: {message}"
        );
        assert!(
            service
                .list_proposals(ProposalStatusFilter::All)?
                .is_empty(),
            "manual apply refusal should not leave an unreviewed proposal behind"
        );

        Ok(())
    }

    #[test]
    fn duplicate_propose_with_apply_remains_unapproved_and_unapplied() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let draft = sample_memory_draft("Duplicate proposal", "Duplicate proposal body");
        let original = service.propose_memory_with_options(
            "agent:red-tests",
            draft.clone(),
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;
        let original_record = original
            .record
            .as_ref()
            .expect("initial unique proposal should apply");

        let duplicate = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;

        let validation = duplicate
            .validation
            .as_ref()
            .expect("duplicate proposal should be validated before approval");
        assert!(!validation.is_valid);
        assert!(
            validation.issues.iter().any(|issue| {
                issue.code == "duplicate_content_hash"
                    && issue.record_id.as_deref() == Some(original_record.id.as_str())
            }),
            "duplicate validation should name the conflicting canonical record: {validation:?}"
        );
        assert_eq!(duplicate.proposal.status, ProposalStatus::Pending);
        assert_eq!(duplicate.record, None);
        assert!(!duplicate.applied);
        assert_eq!(
            service
                .show_proposal(duplicate.proposal.id.as_str())?
                .status,
            ProposalStatus::Pending
        );

        Ok(())
    }

    #[test]
    fn legacy_lifecycle_rejects_runtime_targets_without_canonical_leaks() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let local = service.create_local_memory(
            "agent:red-tests",
            LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Private runtime preference".to_owned(),
                body: "This local-only body must never become canonical.".to_owned(),
            },
        )?;
        let checkpoint = service.create_checkpoint(
            "agent:red-tests",
            CheckpointInput {
                task: "Private runtime checkpoint".to_owned(),
                note: "This session-only body must never become canonical.".to_owned(),
            },
        )?;

        let local_error = service
            .tombstone_record(&local.id, "agent:red-tests", "must stay local")
            .expect_err("local targets must not be written as canonical tombstones");
        assert!(local_error.to_string().contains("destination local"));

        let session_error = service
            .supersede_record(
                &checkpoint.id,
                "agent:red-tests",
                sample_memory_draft(
                    "Replacement for session checkpoint",
                    "A repo replacement must not promote the private target.",
                ),
            )
            .expect_err("session targets must not be written as canonical supersedes");
        assert!(session_error.to_string().contains("destination session"));

        for record in [&local, &checkpoint] {
            assert!(
                !service
                    .paths
                    .records_dir()
                    .join(format!("{}.md", record.id))
                    .exists(),
                "runtime target {} leaked into canonical records",
                record.id
            );
            let stored = service.inspect_expiry(&record.id)?.record;
            assert_eq!(stored.status, MemoryStatus::Active);
            assert_eq!(stored.destination, record.destination);
        }
        Ok(())
    }

    #[test]
    fn legacy_lifecycle_rejects_private_and_inactive_repo_targets() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let mut private_draft = sample_memory_draft(
            "Private repo target",
            "Private visibility must not be rewritten through a legacy lifecycle route.",
        );
        private_draft.visibility = Visibility::Private;
        let private = apply_test_record(&service, private_draft)?;
        let private_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", private.id));
        let private_before = fs::read(&private_path)?;

        let private_error = service
            .tombstone_record(&private.id, "agent:red-tests", "must stay private")
            .expect_err("private targets must not be rewritten canonically");
        assert!(private_error.to_string().contains("visibility private"));
        assert_eq!(fs::read(&private_path)?, private_before);
        assert_eq!(
            service.inspect_expiry(&private.id)?.record.status,
            MemoryStatus::Active
        );

        let active = apply_test_record(
            &service,
            sample_memory_draft(
                "Inactive lifecycle target",
                "Inactive targets must be rejected before file mutation.",
            ),
        )?;
        let active_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", active.id));
        let active_before = fs::read(&active_path)?;
        service.conn.execute(
            "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
            [active.id.as_str()],
        )?;

        let inactive_error = service
            .tombstone_record(&active.id, "agent:red-tests", "already inactive")
            .expect_err("inactive targets must be rejected");
        assert!(inactive_error.to_string().contains("status superseded"));
        assert_eq!(fs::read(&active_path)?, active_before);
        assert_eq!(
            service.inspect_expiry(&active.id)?.record.status,
            MemoryStatus::Superseded
        );
        Ok(())
    }

    #[test]
    fn legacy_supersede_rejects_cross_scope_replacements_before_mutation() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Same-scope lifecycle target",
                "The target must remain active when replacement scope differs.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let mut replacement = sample_memory_draft(
            "Cross-scope lifecycle replacement",
            "A team-scoped replacement cannot supersede a repo-scoped target.",
        );
        replacement.scope_kind = ScopeKind::Team;
        replacement.scope_id = Some("platform".to_owned());

        let error = service
            .supersede_record(&target.id, "agent:red-tests", replacement)
            .expect_err("cross-scope replacements must be rejected");
        assert!(error.to_string().contains("cross-scope"));
        assert_eq!(fs::read(&target_path)?, target_before);
        assert_eq!(
            service.inspect_expiry(&target.id)?.record.status,
            MemoryStatus::Active
        );
        assert!(
            !service
                .paths
                .records_dir()
                .join("cross-scope-lifecycle-replacement.md")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn legacy_supersede_rolls_back_db_and_files_when_second_install_fails() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Second-write rollback target",
                "The original canonical body must survive a second-write failure.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let replacement = sample_memory_draft(
            "Second-write rollback replacement",
            "This replacement must disappear when its install is interrupted.",
        );

        let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                replacement,
                |index| {
                    if index == 1 {
                        return Err(anyhow::anyhow!("injected second-write install failure"));
                    }
                    Ok(())
                },
                |_| Ok(()),
            )
            .expect_err("second-write failure must abort the lifecycle transaction");
        assert!(error.to_string().contains("second-write install failure"));

        assert_legacy_supersede_unchanged(
            &service,
            &target,
            &target_path,
            &target_before,
            "second-write-rollback-replacement",
        )?;
        Ok(())
    }

    #[test]
    fn legacy_supersede_rolls_back_installed_files_when_db_commit_fails() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Commit rollback target",
                "The original canonical body must survive a database commit failure.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let replacement = sample_memory_draft(
            "Commit rollback replacement",
            "This replacement must disappear when SQLite refuses commit.",
        );

        let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                replacement,
                |_| Ok(()),
                |tx| {
                    tx.pragma_update(None, "defer_foreign_keys", "ON")?;
                    tx.execute(
                        "INSERT INTO memory_tag(record_id, tag) VALUES ('missing-record', 'deferred-commit-failure')",
                        [],
                    )?;
                    Ok(())
                },
            )
            .expect_err("deferred foreign-key violation must fail commit");
        assert!(
            error
                .to_string()
                .contains("failed to commit memory lifecycle transaction"),
            "expected actual commit failure, got: {error:#}"
        );

        assert_legacy_supersede_unchanged(
            &service,
            &target,
            &target_path,
            &target_before,
            "commit-rollback-replacement",
        )?;
        Ok(())
    }

    #[test]
    fn rebuild_refuses_to_discard_open_proposals_with_actionable_details() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let pending = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Pending rebuild proposal", "Pending rebuild proposal body"),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;
        let validated = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft(
                "Validated rebuild proposal",
                "Validated rebuild proposal body",
            ),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;
        service.conn.execute(
            "UPDATE proposal SET status = 'validated' WHERE id = ?1",
            [validated.proposal.id.as_str()],
        )?;
        let approved = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft(
                "Approved rebuild proposal",
                "Approved rebuild proposal body",
            ),
            ProposeOptions {
                approval_override: None,
                apply: false,
            },
        )?;

        let error = service
            .rebuild()
            .expect_err("rebuild should not discard open proposals");
        let message = error.to_string();

        assert!(
            message.contains("rebuild refused because 3 open proposals would be discarded"),
            "rebuild refusal should include the open proposal count: {message}"
        );
        for (proposal_id, status) in [
            (pending.proposal.id.as_str(), "pending"),
            (validated.proposal.id.as_str(), "validated"),
            (approved.proposal.id.as_str(), "approved"),
        ] {
            let summary = format!("{proposal_id} ({status})");
            assert!(
                message.contains(&summary),
                "rebuild refusal should include open proposal summary {summary}: {message}"
            );
        }
        assert!(
            message.contains("memzoi proposals list --status open"),
            "rebuild refusal should suggest listing open proposals: {message}"
        );
        assert!(
            message.contains("memzoi proposals apply --all-approved"),
            "rebuild refusal should suggest applying approved proposals: {message}"
        );
        assert!(
            message.contains("memzoi reject <proposal-id> --reason"),
            "rebuild refusal should suggest rejecting proposals before rebuild: {message}"
        );

        Ok(())
    }

    #[test]
    fn show_proposal_reports_missing_ids() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let error = service
            .show_proposal("prop_missing")
            .expect_err("missing proposal show should fail");

        assert!(
            error
                .to_string()
                .contains("proposal not found: prop_missing"),
            "missing proposal show error should include the requested id: {error:#}"
        );

        Ok(())
    }

    fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
        let temp = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            temp.path().canonicalize()?,
            temp.path().join(".memzoi-runtime"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(paths)?;
        Ok((temp, service))
    }

    fn apply_test_record(
        service: &MemoryService,
        draft: MemoryDraft,
    ) -> anyhow::Result<MemoryRecord> {
        let proposal = service.propose_memory("agent:red-tests", draft)?;
        service.validate_proposal(&proposal.id)?;
        service.approve_proposal(&proposal.id, "reviewer:human")?;
        service.apply_proposal(&proposal.id, "agent:applier")
    }

    fn assert_legacy_supersede_unchanged(
        service: &MemoryService,
        target: &MemoryRecord,
        target_path: &Path,
        target_before: &[u8],
        replacement_id: &str,
    ) -> anyhow::Result<()> {
        assert_eq!(fs::read(target_path)?, target_before);
        assert_eq!(
            service.inspect_expiry(&target.id)?.record.status,
            MemoryStatus::Active
        );
        assert!(
            service.inspect_expiry(replacement_id).is_err(),
            "replacement row {replacement_id} survived rollback"
        );
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{replacement_id}.md"))
                .exists(),
            "replacement file {replacement_id} survived rollback"
        );
        let transaction_files = fs::read_dir(service.paths.records_dir())?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with('.') && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            transaction_files.is_empty(),
            "staged transaction files survived rollback: {transaction_files:?}"
        );
        Ok(())
    }

    fn repo_session_document(title: &str) -> SessionEndDocument {
        SessionEndDocument {
            task: "Exercise guarded proposal creation".to_owned(),
            candidates: vec![SessionEndCandidate {
                destination: MemoryDestination::Repo,
                memory_type: MemoryType::Decision,
                lane: MemoryLane::Semantic,
                title: title.to_owned(),
                body: "The pending packet must remain inside the guarded repository root."
                    .to_owned(),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                reason: Some("Lifecycle regression coverage".to_owned()),
                scope: None,
                tags: vec!["lifecycle".to_owned()],
            }],
        }
    }

    fn repo_import_document(title: &str) -> ImportDocument {
        ImportDocument {
            version: "memzoi/import-v1".to_owned(),
            sources: vec![OkfProposalSource {
                path: Some("src/lifecycle.rs".to_owned()),
                url: None,
                reference: None,
            }],
            candidates: vec![ImportCandidateInput {
                destination: MemoryDestination::Repo,
                reason: "Lifecycle regression coverage".to_owned(),
                memory_type: Some(MemoryType::Decision),
                lane: Some(MemoryLane::Semantic),
                title: title.to_owned(),
                body: "The imported pending packet must remain inside the guarded repository root."
                    .to_owned(),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                scope: None,
                tags: vec!["lifecycle".to_owned()],
            }],
        }
    }

    fn write_test_pending_proposal(
        service: &MemoryService,
        title: &str,
        sensitivity: OkfProposalSensitivity,
    ) -> anyhow::Result<PathBuf> {
        let proposal_id = proposals::title_to_concept_slug(title)
            .context("test proposal title should produce a slug")?;
        write_test_pending_proposal_with_id(service, &proposal_id, title, sensitivity)
    }

    fn write_test_pending_proposal_with_id(
        service: &MemoryService,
        proposal_id: &str,
        title: &str,
        sensitivity: OkfProposalSensitivity,
    ) -> anyhow::Result<PathBuf> {
        prepare_pending_proposal_root(&service.paths)?;
        let draft = okf::OkfCreateProposalDraft {
            proposal_id: proposal_id.to_owned(),
            memory_type: MemoryType::Decision,
            lane: MemoryLane::Semantic,
            title: title.to_owned(),
            body: "Lifecycle finalization must never hide cleanup failures.".to_owned(),
            actor: "agent:red-tests".to_owned(),
            timestamp: "2026-07-10T00:00:00Z".to_owned(),
            reason: Some("Lifecycle regression coverage".to_owned()),
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            applies_to: vec!["crates/memzoi-core/**".to_owned()],
            tags: vec!["lifecycle".to_owned()],
            sources: vec![OkfProposalSource {
                path: Some("crates/memzoi-core/src/service.rs".to_owned()),
                url: None,
                reference: None,
            }],
            sensitivity: OkfProposalSensitivity::RepoSafe,
        };
        let plan =
            okf::plan_okf_create_proposal(&service.paths.proposals_dir().join("pending"), &draft)?;
        if sensitivity == OkfProposalSensitivity::RepoSafe {
            okf::create_okf_proposal_file(&plan)
        } else {
            let markdown = plan.markdown.replacen(
                "sensitivity: repo-safe",
                &format!("sensitivity: {}", sensitivity.as_str()),
                1,
            );
            fs::write(&plan.path, markdown)?;
            Ok(plan.path)
        }
    }

    fn write_test_supersede_proposal(
        service: &MemoryService,
        proposal_id: &str,
        title: &str,
        target_id: &str,
    ) -> anyhow::Result<PathBuf> {
        let path = write_test_pending_proposal_with_id(
            service,
            proposal_id,
            title,
            OkfProposalSensitivity::RepoSafe,
        )?;
        let markdown = fs::read_to_string(&path)?
            .replace("action: create", "action: supersede")
            .replace("supersedes: []", &format!("supersedes:\n- {target_id}"))
            .replace("2026-07-10T00:00:00Z", "2099-07-10T00:00:00Z");
        fs::write(&path, markdown)?;
        Ok(path)
    }

    fn insert_runtime_record_with_id(
        service: &MemoryService,
        record_id: &str,
        destination: MemoryDestination,
    ) -> anyhow::Result<()> {
        let record = MemoryRecord {
            id: record_id.to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            destination,
            scope_kind: ScopeKind::Personal,
            scope_id: None,
            visibility: Visibility::Private,
            title: "Runtime collision sentinel".to_owned(),
            body: "Runtime collision sentinel".to_owned(),
            status: MemoryStatus::Active,
            confidence: 1.0,
            source_kind: Some("test-runtime".to_owned()),
            source_ref: None,
            proposal_id: None,
            content_hash: blake3::hash(b"Runtime collision sentinel")
                .to_hex()
                .to_string(),
            created_at: "2026-07-10T00:00:00Z".to_owned(),
            updated_at: "2026-07-10T00:00:00Z".to_owned(),
            supersedes_id: None,
            expires_at: None,
        };
        insert_memory_record_row(&service.conn, &record, InsertMode::Create)
    }

    fn sample_memory_draft(title: &str, body: &str) -> MemoryDraft {
        MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: title.to_owned(),
            body: body.to_owned(),
            tags: vec!["rust".to_owned(), "tests".to_owned()],
            source_kind: Some("test".to_owned()),
            source_ref: Some("service-proposal-tests".to_owned()),
            sensitivity: crate::OkfProposalSensitivity::RepoSafe,
            confidence: 0.82,
        }
    }
}
