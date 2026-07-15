use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::{
    ImportApplyResult, ImportDocument, ImportPlan, MemoryDestination, MemoryPaths, MemoryStatus,
    expiry::{self, Clock},
    import::{self, ExistingDuplicate},
    okf,
};

use super::{
    proposal_packets::{FileProposalInventoryEntry, ProposalPacketLifecycle},
    runtime_records::{CheckpointInput, LocalMemoryInput, RuntimeRecords},
    safe_files::{RepoLifecycleLock, ensure_safe_directory},
};

pub(super) struct ImportLifecycle<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    clock: &'a dyn Clock,
}

impl<'a> ImportLifecycle<'a> {
    pub(super) fn new(paths: &'a MemoryPaths, conn: &'a Connection, clock: &'a dyn Clock) -> Self {
        Self { paths, conn, clock }
    }

    pub(super) fn plan(&self, actor: &str, document: ImportDocument) -> Result<ImportPlan> {
        if actor.trim().is_empty() {
            bail!("import actor cannot be empty");
        }
        import::validate_document(&document)?;
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.conn);
        let (inventory, reserved_proposal_ids) = proposal_packets.planning_inventory()?;
        let existing = self.load_duplicates(&inventory.pending)?;
        import::build_plan(
            actor,
            &document,
            &existing,
            &self.paths.proposals_dir().join("pending"),
            &reserved_proposal_ids,
        )
    }

    pub(super) fn apply(
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
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.conn);
        if has_repo_candidates {
            proposal_packets.preflight_pending_root()?;
        }
        let plan = self.plan(actor, document.clone())?;
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
            proposal_packets.prepare_pending_root()?;
        }
        let timestamp = expiry::format_timestamp(self.clock.now_utc())?;
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
                proposal_packets
                    .ensure_planned_available(planned.iter().map(|(_, proposal)| proposal))?;
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
                    crate::MemoryWriteRoute::RuntimeLocal => RuntimeRecords::new(&tx)
                        .create_local(
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                        )?,
                    crate::MemoryWriteRoute::RuntimeSession => RuntimeRecords::new(&tx)
                        .create_checkpoint(
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

    fn load_duplicates(
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
        let runtime_records = RuntimeRecords::new(self.conn).records_for_preservation()?;
        let now = self.clock.now_utc();
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
}
