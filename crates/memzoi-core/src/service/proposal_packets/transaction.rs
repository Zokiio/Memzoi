use super::*;

impl MemoryService {
    pub(super) fn load_pending_file_proposal_snapshot(
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

    pub(super) fn revalidate_moved_pending_file_proposal(
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
}

pub(super) fn cleanup_staged_file_resolution(
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

pub(super) fn rollback_file_resolution(
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

pub(super) fn rollback_rejected_file_proposal(
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

pub(super) fn attach_cleanup_error<T>(
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

pub(super) fn rebuild_fts_content_index(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO memory_fts(memory_fts) VALUES ('rebuild')", [])
        .context("failed to rebuild full-text index")?;
    Ok(())
}

pub(super) fn validate_resolution_actor(actor: &str) -> Result<()> {
    if actor.trim().is_empty() {
        bail!("proposal-file resolution actor cannot be empty");
    }
    Ok(())
}
