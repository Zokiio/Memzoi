use std::collections::BTreeSet;

use crate::{
    AuthorizationProof, OkfProposalSensitivity, RepositoryContentClass, RepositoryWriteRoute,
    SafetyFieldKind,
};

use super::canonical_write::{
    CanonicalFileWrite, StagedCanonicalFileWrite, cleanup_staged_canonical_writes,
    finalize_staged_canonical_writes, install_staged_canonical_writes,
    prepare_canonical_file_write, rollback_staged_canonical_writes, stage_canonical_writes,
    validate_canonical_write_precondition,
};
use super::repository_mutation::{
    CreatedRepositoryFile, OwnedRepositoryProjection, RepositoryMutationAuthorization,
    authorize_repository_projection_batch, backup_repository_file_to_transaction,
    canonical_write_projections, explicit_repository_provenance,
    install_verified_staged_file_no_replace, okf_proposal_safety_values,
    remove_installed_repository_file, repository_transaction_path, repository_transaction_root,
    restore_verified_staged_file_no_replace, safety_value, stage_authorized_file,
};
use super::safe_files::{
    ensure_path_absent, ensure_regular_file, ensure_safe_directory, ensure_safe_existing_file,
    ensure_safe_path_parent, remove_staged_file,
};
use super::*;

mod apply;
mod inventory;
mod reject;
#[cfg(test)]
mod tests;
mod transaction;
mod validation;

pub(super) use self::inventory::prepare_pending_proposal_root;
pub use self::inventory::scan_file_proposal_inventory;
use self::inventory::{
    ensure_planned_proposals_available, preflight_pending_proposal_root,
    require_clean_file_proposal_inventory, reserved_proposal_identities,
};

fn repository_relative_event_path(paths: &MemoryPaths, absolute_path: &Path) -> Result<String> {
    let relative = absolute_path
        .strip_prefix(&paths.project_root)
        .context("resolved proposal event path escaped the repository")?;
    let mut segments = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            bail!("resolved proposal event path contains an unsafe component");
        };
        let segment = segment
            .to_str()
            .context("resolved proposal event path is not valid UTF-8")?;
        ensure!(
            !segment.is_empty() && !segment.contains('\\'),
            "resolved proposal event path contains an unsafe component"
        );
        segments.push(segment);
    }
    ensure!(
        !segments.is_empty(),
        "resolved proposal event path cannot be empty"
    );
    Ok(segments.join("/"))
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
    pub source_sensitivity: crate::OkfProposalSensitivity,
    pub source_content_class: RepositoryContentClass,
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

#[derive(Debug)]
struct FileProposalApplyPlan {
    writes: Vec<CanonicalFileWrite>,
    record: MemoryRecord,
    record_path: PathBuf,
    target_id: Option<String>,
}

#[derive(Debug)]
struct PendingFileProposalSnapshot {
    proposal: OkfProposalFile,
    source_sensitivity: crate::OkfProposalSensitivity,
    source_content_class: RepositoryContentClass,
    bytes: Vec<u8>,
    expected_hash: String,
    display_path: PathBuf,
}

pub(super) struct ProposalPacketLifecycle<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
}

impl<'a> ProposalPacketLifecycle<'a> {
    pub(super) fn new(paths: &'a MemoryPaths, conn: &'a Connection) -> Self {
        Self { paths, conn }
    }

    pub(super) fn planning_inventory(&self) -> Result<(FileProposalInventory, BTreeSet<String>)> {
        let inventory = scan_file_proposal_inventory(self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        let reserved = reserved_proposal_identities(self.conn, &inventory)?;
        Ok((inventory, reserved))
    }

    pub(super) fn prepare_identity_space(&self) -> Result<BTreeSet<String>> {
        prepare_pending_proposal_root(self.paths)?;
        let (_, reserved) = self.planning_inventory()?;
        Ok(reserved)
    }

    pub(super) fn ensure_planned_available<'p>(
        &self,
        plans: impl IntoIterator<Item = &'p okf::OkfCreateProposalPlan>,
    ) -> Result<()> {
        let inventory = scan_file_proposal_inventory(self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        ensure_planned_proposals_available(self.conn, &inventory, plans)
    }

    pub(super) fn prepare_pending_root(&self) -> Result<()> {
        prepare_pending_proposal_root(self.paths)
    }

    pub(super) fn preflight_pending_root(&self) -> Result<()> {
        preflight_pending_proposal_root(self.paths)
    }
}
