use super::super::safe_files::RepoLifecycleLock;
use super::transaction::{
    RejectedFileProposalRollback, attach_cleanup_error, rollback_rejected_file_proposal,
    validate_resolution_actor,
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
        let mut proposal = snapshot.proposal.clone();
        let original_values = okf_proposal_safety_values("proposal", &proposal);
        if authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::FileProposalRejectReceipt,
            proposal.sensitivity,
            proposal.scope_kind,
            proposal.scope_id.as_deref(),
            Visibility::Repo,
            AuthorizationProof::ExplicitCommand {
                operation: "file_proposal_reject",
            },
            explicit_repository_provenance(proposal.content_class, &proposal.id),
            &original_values,
            &[],
        )
        .is_err()
        {
            let raw = fs::read_to_string(proposal_path)
                .context("failed to read rejected proposal during redacted preflight")?;
            proposal = okf::preflight_okf_proposal_markdown(
                self.paths.proposals_dir().join("pending"),
                proposal_path,
                &raw,
            )?
            .context("pending proposal was ignored during redacted rejection")?
            .receipt_proposal;
        }
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
        let archived_proposal = proposal.clone();
        let resolved_markdown =
            okf::render_resolved_okf_proposal_markdown(&archived_proposal, &resolution)?;
        let projections = vec![
            OwnedRepositoryProjection::from_absolute(
                &self.paths,
                &resolved_path,
                resolved_markdown.as_bytes(),
                None,
            )?,
            OwnedRepositoryProjection::existing_from_absolute(
                &self.paths,
                proposal_path,
                &snapshot.bytes,
                &snapshot.expected_hash,
            )?,
        ];
        let mut safety_values = okf_proposal_safety_values("receipt", &archived_proposal);
        safety_values.push(safety_value(
            "resolution.reason".to_owned(),
            SafetyFieldKind::Reason,
            reason,
        ));
        safety_values.push(safety_value(
            "resolution.resolved_by".to_owned(),
            SafetyFieldKind::Identifier,
            actor,
        ));
        let authorization = authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::FileProposalRejectReceipt,
            OkfProposalSensitivity::RepoSafe,
            ScopeKind::Repo,
            None,
            Visibility::Repo,
            AuthorizationProof::ExplicitCommand {
                operation: "file_proposal_reject",
            },
            explicit_repository_provenance(
                RepositoryContentClass::GeneralRepoKnowledge,
                "redacted_file_proposal_rejection_receipt",
            ),
            &safety_values,
            &projections,
        )?;
        let mutation = RepositoryMutationAuthorization {
            route: RepositoryWriteRoute::FileProposalRejectReceipt,
            authorization: &authorization,
            projections: &projections,
        };
        self.prepare_resolution_destination(&resolved_path)?;
        let nonce = Uuid::now_v7().to_string();
        let resolved_temp = stage_authorized_file(
            &self.paths,
            RepositoryWriteRoute::FileProposalRejectReceipt,
            &authorization,
            &projections,
            &resolved_path,
            &resolved_markdown,
            &nonce,
        )?;
        let pending_backup =
            repository_transaction_path(&self.paths, proposal_path, &nonce, "pending");
        let resolved_hash = blake3::hash(resolved_markdown.as_bytes())
            .to_hex()
            .to_string();

        if let Err(error) = before_pending_revalidation(proposal_path) {
            return attach_cleanup_error(
                error,
                remove_staged_file(&resolved_temp),
                "rejection staging cleanup",
            );
        }
        if let Err(error) = backup_repository_file_to_transaction(
            &self.paths,
            mutation,
            proposal_path,
            &pending_backup,
            &snapshot.expected_hash,
        ) {
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
                rollback_rejected_file_proposal(RejectedFileProposalRollback {
                    paths: &self.paths,
                    mutation,
                    pending_path: proposal_path,
                    pending_backup: &pending_backup,
                    pending_hash: &snapshot.expected_hash,
                    resolved_path: &resolved_path,
                    resolved_hash: &resolved_hash,
                    resolved_identity: None,
                    resolved_temp: &resolved_temp,
                }),
                "rejection captured-byte rollback",
            );
        }
        let resolved_identity = match install_verified_staged_file_no_replace(
            &self.paths,
            mutation,
            &resolved_temp,
            &resolved_path,
            &resolved_hash,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                return attach_cleanup_error(
                    error,
                    rollback_rejected_file_proposal(RejectedFileProposalRollback {
                        paths: &self.paths,
                        mutation,
                        pending_path: proposal_path,
                        pending_backup: &pending_backup,
                        pending_hash: &snapshot.expected_hash,
                        resolved_path: &resolved_path,
                        resolved_hash: &resolved_hash,
                        resolved_identity: None,
                        resolved_temp: &resolved_temp,
                    }),
                    "rejection install rollback",
                );
            }
        };
        if let Err(error) = before_finalize(&pending_backup)
            .context("proposal-file rejection finalization was interrupted")
        {
            return attach_cleanup_error(
                error,
                rollback_rejected_file_proposal(RejectedFileProposalRollback {
                    paths: &self.paths,
                    mutation,
                    pending_path: proposal_path,
                    pending_backup: &pending_backup,
                    pending_hash: &snapshot.expected_hash,
                    resolved_path: &resolved_path,
                    resolved_hash: &resolved_hash,
                    resolved_identity: Some(resolved_identity),
                    resolved_temp: &resolved_temp,
                }),
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
                rollback_rejected_file_proposal(RejectedFileProposalRollback {
                    paths: &self.paths,
                    mutation,
                    pending_path: proposal_path,
                    pending_backup: &pending_backup,
                    pending_hash: &snapshot.expected_hash,
                    resolved_path: &resolved_path,
                    resolved_hash: &resolved_hash,
                    resolved_identity: Some(resolved_identity),
                    resolved_temp: &resolved_temp,
                }),
                "rejection cleanup rollback",
            );
        }

        remove_staged_file(&resolved_temp)
            .context("proposal-file rejection completed but resolved staging cleanup failed")?;

        let mut reported_proposal = archived_proposal;
        reported_proposal.sensitivity = snapshot.source_sensitivity;
        Ok(FileProposalResolutionResult {
            proposal: reported_proposal,
            resolution,
            resolved_path,
            record: None,
            record_path: None,
            already_resolved: false,
            runtime_index_updated: false,
        })
    }
}
