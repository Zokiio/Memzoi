use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::import::{self, ExistingDuplicate};
use crate::{
    ContextPack, ContextPackInput, HandoffInput, HandoffPack, ImportApplyResult, ImportDocument,
    ImportPlan, MemoryDestination, MemoryDraft, MemoryEvent, MemoryLane, MemoryPaths, MemoryRecord,
    MemoryStatus, MemoryType, PrecheckInput, PrecheckWarning, Proposal, ProposalStatus,
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
        SessionEndWrite, session_end_proposal_draft, validate_session_end_document,
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
}

impl RepoIndexDrift {
    pub fn is_current(&self) -> bool {
        self.missing_from_index.is_empty()
            && self.stale_in_index.is_empty()
            && self.changed_in_index.is_empty()
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
        let tx = self.conn.unchecked_transaction()?;
        let record = proposals::apply_proposal(&tx, proposal_id, actor)?;
        self.write_record_file_with_conn(&tx, &record, FileWriteMode::CreateNew)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn supersede_record(
        &self,
        record_id: &str,
        actor: &str,
        draft: MemoryDraft,
    ) -> Result<SupersedeResult> {
        let tx = self.conn.unchecked_transaction()?;
        let result = proposals::supersede_record(&tx, record_id, actor, draft)?;
        self.ensure_record_file_absent(&result.replacement.id)?;
        self.write_record_file_with_conn(&tx, &result.previous, FileWriteMode::Overwrite)?;
        self.write_record_file_with_conn(&tx, &result.replacement, FileWriteMode::CreateNew)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn tombstone_record(
        &self,
        record_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<MemoryRecord> {
        let tx = self.conn.unchecked_transaction()?;
        let record = proposals::tombstone_record(&tx, record_id, actor, reason)?;
        self.write_record_file_with_conn(&tx, &record, FileWriteMode::Overwrite)?;
        tx.commit()?;
        Ok(record)
    }

    fn write_record_file_with_conn(
        &self,
        conn: &Connection,
        record: &MemoryRecord,
        mode: FileWriteMode,
    ) -> Result<()> {
        let tags = record_tags(conn, &record.id)?;
        let applies_to = search::load_paths(conn, &record.id)?
            .into_iter()
            .map(|path| path.path)
            .collect::<Vec<_>>();
        match mode {
            FileWriteMode::CreateNew => okf::create_memory_record_file_with_metadata(
                &self.paths.records_dir(),
                record,
                &tags,
                &applies_to,
            )?,
            FileWriteMode::Overwrite => okf::write_memory_record_file_with_metadata(
                &self.paths.records_dir(),
                record,
                &tags,
                &applies_to,
            )?,
        };
        Ok(())
    }

    fn ensure_record_file_absent(&self, record_id: &str) -> Result<()> {
        let path = self.paths.records_dir().join(format!("{record_id}.md"));
        if path
            .try_exists()
            .with_context(|| format!("failed to inspect memory record {}", path.display()))?
        {
            bail!("canonical memory record already exists: {}", path.display());
        }
        Ok(())
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

        Ok(RepoIndexDrift {
            missing_from_index,
            stale_in_index,
            changed_in_index,
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
        let timestamp = self.now_timestamp()?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut reserved_proposal_ids = BTreeSet::new();
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
                status,
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
        let existing = self.load_import_duplicates()?;
        import::build_plan(
            actor,
            &document,
            &existing,
            &self.paths.proposals_dir().join("pending"),
        )
    }

    pub fn apply_import(
        &self,
        actor: &str,
        document: ImportDocument,
        expected_plan_id: &str,
    ) -> Result<ImportApplyResult> {
        let plan = self.plan_import(actor, document.clone())?;
        if plan.plan_id != expected_plan_id {
            bail!(
                "stale import plan: expected {expected_plan_id}, recomputed {}",
                plan.plan_id
            );
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

    fn load_import_duplicates(&self) -> Result<Vec<ExistingDuplicate>> {
        let mut entries = Vec::new();
        for record in okf::read_okf_record_files(self.paths.records_dir())? {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::CanonicalRecord,
                id: record.concept_id,
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&record.draft.body),
            });
        }
        for proposal in okf::read_okf_proposal_files(self.paths.proposals_dir().join("pending"))? {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::PendingProposal,
                id: proposal.id,
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&proposal.body),
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
        let records_root = paths.records_dir();
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
                supersedes_id, expires_at
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
                supersedes_id, expires_at
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
    use crate::{MemoryLane, MemoryStatus, MemoryType, ProposalStatus, ScopeKind, Visibility};
    use tempfile::TempDir;

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
        let applied = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft(
                "Lineage survives rebuild",
                "Evidence-backed zircon lineage.",
            ),
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;
        let proposal_id = applied.proposal.id.clone();
        let record_id = applied.record.expect("record should be applied").id;

        service.rebuild()?;
        let rebuilt = MemoryService::open_paths(paths)?;
        let results = rebuilt.search_memory(SearchInput {
            query: "zircon lineage".to_owned(),
            scope_kind: Some(ScopeKind::Repo),
            scope_id: None,
            memory_type: None,
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
