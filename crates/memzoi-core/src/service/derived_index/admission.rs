use anyhow::{Result, bail};

use crate::{
    MemoryPaths, ScopeKind, Visibility,
    git_repository::{GitReviewVisibility, GitReviewVisibilityError, git_review_visibility},
    materialization::canonical_revision_for_okf_record,
    okf,
};

const MAX_REDACTED_SAFETY_CODES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepositoryRecordAdmission {
    EnforceForRepositoryReads,
    /// Explicit fixture-only bypass preserving trusted recall-evaluation behavior.
    TrustedRecallEvaluationBypass,
}

impl RepositoryRecordAdmission {
    fn requires_repository_admission(self) -> bool {
        matches!(self, Self::EnforceForRepositoryReads)
    }
}

pub(super) fn read_admitted_repository_record_snapshots(
    paths: &MemoryPaths,
    admission: RepositoryRecordAdmission,
    after_snapshot: impl FnOnce() -> Result<()>,
) -> Result<Vec<okf::OkfRecordSnapshot>> {
    let records_root = paths.records_dir();
    super::super::safe_files::ensure_safe_directory(
        &paths.project_root,
        &records_root,
        false,
        "canonical record root",
    )
    .map_err(|_| redacted_refusal("canonical-root"))?;

    let snapshots = okf::read_okf_record_snapshots(&records_root)
        .map_err(|_| redacted_refusal("typed-record"))?;
    after_snapshot()?;
    if admission.requires_repository_admission() {
        for snapshot in &snapshots {
            admit_repository_record_snapshot(paths, snapshot)?;
        }
    }
    Ok(snapshots)
}

pub(super) fn admit_repository_record_snapshot(
    paths: &MemoryPaths,
    snapshot: &okf::OkfRecordSnapshot,
) -> Result<()> {
    let records_root = paths.records_dir();
    super::super::safe_files::ensure_safe_existing_file(
        &paths.project_root,
        &records_root,
        &snapshot.path,
        "canonical record",
    )
    .map_err(|_| redacted_refusal("canonical-path"))?;

    let expected_path = records_root
        .join(&snapshot.record.concept_id)
        .with_extension("md");
    if snapshot.path != expected_path {
        bail!("repository-record admission refused: canonical-path-id");
    }

    if snapshot.record.draft.scope_kind != ScopeKind::Repo {
        bail!("repository-record admission refused: scope-not-repository");
    }
    if snapshot.record.draft.visibility != Visibility::Repo {
        bail!("repository-record admission refused: visibility-not-repository");
    }

    verify_existing_materialization_attestation(&snapshot.record)?;

    let repository_relative_path = snapshot
        .path
        .strip_prefix(&paths.project_root)
        .map_err(|_| redacted_refusal("canonical-path"))?;
    let report = crate::scan_managed_repository_blob(
        paths.project_root.as_os_str().as_encoded_bytes(),
        repository_relative_path,
        &snapshot.bytes,
    );
    if !report.allowed {
        let codes = report
            .findings
            .iter()
            .take(MAX_REDACTED_SAFETY_CODES)
            .map(|finding| finding.code.as_str())
            .collect::<Vec<_>>();
        let diagnostic = if codes.is_empty() {
            "unspecified".to_owned()
        } else {
            codes.join(",")
        };
        bail!("repository-record admission refused: safety-scan[{diagnostic}]");
    }

    match git_review_visibility(&paths.project_root, repository_relative_path) {
        Ok(GitReviewVisibility::Tracked | GitReviewVisibility::UntrackedAndNotIgnored) => Ok(()),
        Ok(GitReviewVisibility::IgnoredUntracked) => {
            bail!("repository-record admission refused: ignored-untracked")
        }
        // Pre-existing file-native records remain readable outside a Git
        // worktree. A materialized revision is Git-native by contract and
        // therefore always requires review visibility.
        Err(GitReviewVisibilityError::NotAWorktree)
            if snapshot.record.materialization.is_none() =>
        {
            Ok(())
        }
        Err(_) => bail!("repository-record admission refused: review-visibility"),
    }
}

fn verify_existing_materialization_attestation(record: &okf::OkfRecordFile) -> Result<()> {
    // Materialization attestations remain optional in the current profile; when present they
    // must verify exactly against the artifact bytes.
    let Some(metadata) = record.materialization.as_ref() else {
        return Ok(());
    };
    metadata
        .validate()
        .map_err(|_| redacted_refusal("materialization-metadata"))?;
    let current_revision = canonical_revision_for_okf_record(record)
        .map_err(|_| redacted_refusal("materialization-revision"))?;
    if metadata.revision != current_revision {
        bail!("repository-record admission refused: materialization-revision");
    }
    Ok(())
}

fn redacted_refusal(code: &str) -> anyhow::Error {
    anyhow::anyhow!("repository-record admission refused: {code}")
}
