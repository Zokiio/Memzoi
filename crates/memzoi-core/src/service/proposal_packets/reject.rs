use super::transaction::{
    attach_cleanup_error, rollback_rejected_file_proposal, validate_resolution_actor,
};
use super::*;

impl MemoryService {
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
    pub(super) fn reject_file_proposal_with_finalize_hook<BeforeFinalize>(
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

    pub(super) fn reject_file_proposal_with_hooks<BeforePendingRevalidation, BeforeFinalize>(
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
}
