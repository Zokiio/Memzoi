use super::super::safe_files::RepoLifecycleLock;
use super::transaction::{
    attach_cleanup_error, cleanup_staged_file_resolution, rebuild_fts_content_index,
    rollback_file_resolution, validate_resolution_actor,
};
use super::*;

impl MemoryService {
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
    pub(super) fn apply_file_proposal_with_finalize_hook<BeforeFinalize>(
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

    pub(super) fn apply_file_proposal_with_hooks<BeforePendingRevalidation, BeforeFinalize>(
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
}
