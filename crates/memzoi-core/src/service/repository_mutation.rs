use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    AuthorizationProof, AuthorizedRepositoryWriteBatch, MemoryDestination, MemoryDraft,
    MemoryPaths, OkfProposalFile, OkfProposalSensitivity, ProvenanceAssessment,
    RepositoryContentClass, RepositoryProjection, RepositoryProjectionPurpose, RepositoryScope,
    RepositoryWriteRequest, RepositoryWriteRoute, SafetyField, SafetyFieldKind, ScopeKind,
    Visibility, authorize_repository_write, repository_io,
    repository_write_safety::repository_write_policy_context_digest,
};

use super::{
    canonical_write::CanonicalFileWrite,
    safe_files::{ensure_safe_directory, remove_staged_file, sync_directory},
};

pub(super) use crate::repository_io::RepositoryFileIdentity;

#[derive(Debug)]
pub(super) struct RepositorySafetyValue {
    pub(super) location: String,
    pub(super) kind: SafetyFieldKind,
    pub(super) value: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct OwnedRepositoryProjection {
    pub(super) relative_path: PathBuf,
    pub(super) bytes: Vec<u8>,
    pub(super) target_revision: Option<String>,
    pub(super) purpose: RepositoryProjectionPurpose,
}

#[derive(Debug)]
pub(super) struct AuthorizedRepositoryProjectionBatch {
    pub(super) capability: AuthorizedRepositoryWriteBatch,
    pub(super) policy_context_digest: [u8; 32],
}

#[derive(Debug)]
pub(super) struct CreatedRepositoryFile(repository_io::CreatedRepositoryFile);

impl CreatedRepositoryFile {
    pub(super) fn path(&self) -> &Path {
        &self.0.path
    }

    pub(super) fn identity(&self) -> RepositoryFileIdentity {
        self.0.identity
    }
}

impl AuthorizedRepositoryProjectionBatch {
    pub(super) fn digest(&self) -> String {
        self.capability.digest()
    }
}

impl OwnedRepositoryProjection {
    pub(super) fn from_absolute(
        paths: &MemoryPaths,
        path: &Path,
        bytes: &[u8],
        target_revision: Option<&str>,
    ) -> Result<Self> {
        let relative_path = path
            .strip_prefix(&paths.project_root)
            .context("repository projection is outside the current project")?
            .to_path_buf();
        Ok(Self {
            relative_path,
            bytes: bytes.to_vec(),
            target_revision: target_revision.map(str::to_owned),
            purpose: RepositoryProjectionPurpose::Write,
        })
    }

    pub(super) fn existing_from_absolute(
        paths: &MemoryPaths,
        path: &Path,
        bytes: &[u8],
        target_revision: &str,
    ) -> Result<Self> {
        let mut projection = Self::from_absolute(paths, path, bytes, Some(target_revision))?;
        if blake3::hash(bytes).to_hex().as_str() != target_revision {
            bail!("existing repository projection revision does not match its bytes");
        }
        projection.purpose = RepositoryProjectionPurpose::Existing;
        Ok(projection)
    }
}

pub(super) fn memory_draft_safety_values(
    prefix: &str,
    draft: &MemoryDraft,
) -> Vec<RepositorySafetyValue> {
    let mut values = vec![
        safety_value(
            format!("{prefix}.title"),
            SafetyFieldKind::Text,
            &draft.title,
        ),
        safety_value(format!("{prefix}.body"), SafetyFieldKind::Text, &draft.body),
        safety_value(
            format!("{prefix}.content_class"),
            SafetyFieldKind::Identifier,
            draft.content_class.as_str(),
        ),
    ];
    for (index, tag) in draft.tags.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.tags[{index}]"),
            SafetyFieldKind::Text,
            tag,
        ));
    }
    if let Some(scope_id) = draft.scope_id.as_deref() {
        values.push(safety_value(
            format!("{prefix}.scope_id"),
            SafetyFieldKind::Identifier,
            scope_id,
        ));
    }
    if let Some(source_kind) = draft.source_kind.as_deref() {
        values.push(safety_value(
            format!("{prefix}.source_kind"),
            SafetyFieldKind::SourceReference,
            source_kind,
        ));
    }
    if let Some(source_ref) = draft.source_ref.as_deref() {
        values.push(safety_value(
            format!("{prefix}.source_ref"),
            SafetyFieldKind::SourceReference,
            source_ref,
        ));
    }
    values
}

pub(super) fn okf_proposal_safety_values(
    prefix: &str,
    proposal: &OkfProposalFile,
) -> Vec<RepositorySafetyValue> {
    let mut values = vec![
        safety_value(
            format!("{prefix}.id"),
            SafetyFieldKind::Identifier,
            &proposal.id,
        ),
        safety_value(
            format!("{prefix}.file_id"),
            SafetyFieldKind::Identifier,
            &proposal.file_id,
        ),
        safety_value(
            format!("{prefix}.title"),
            SafetyFieldKind::Text,
            &proposal.title,
        ),
        safety_value(
            format!("{prefix}.description"),
            SafetyFieldKind::Text,
            &proposal.description,
        ),
        safety_value(
            format!("{prefix}.body"),
            SafetyFieldKind::Text,
            &proposal.body,
        ),
        safety_value(
            format!("{prefix}.proposed_by"),
            SafetyFieldKind::Identifier,
            &proposal.proposal.proposed_by,
        ),
        safety_value(
            format!("{prefix}.content_class"),
            SafetyFieldKind::Identifier,
            proposal.content_class.as_str(),
        ),
    ];
    if let Some(reason) = proposal.proposal.reason.as_deref() {
        values.push(safety_value(
            format!("{prefix}.reason"),
            SafetyFieldKind::Reason,
            reason,
        ));
    }
    if let Some(target) = proposal.proposal.target.as_deref() {
        values.push(safety_value(
            format!("{prefix}.target"),
            SafetyFieldKind::Identifier,
            target,
        ));
    }
    if let Some(scope_id) = proposal.scope_id.as_deref() {
        values.push(safety_value(
            format!("{prefix}.scope_id"),
            SafetyFieldKind::Identifier,
            scope_id,
        ));
    }
    for (index, path) in proposal.applies_to.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.applies_to[{index}]"),
            SafetyFieldKind::Path,
            path,
        ));
    }
    for (index, tag) in proposal.tags.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.tags[{index}]"),
            SafetyFieldKind::Text,
            tag,
        ));
    }
    for (index, source) in proposal.sources.iter().enumerate() {
        for (name, value) in [
            ("path", source.path.as_deref()),
            ("url", source.url.as_deref()),
            ("ref", source.reference.as_deref()),
        ] {
            if let Some(value) = value {
                values.push(safety_value(
                    format!("{prefix}.sources[{index}].{name}"),
                    SafetyFieldKind::SourceReference,
                    value,
                ));
            }
        }
    }
    values
}

pub(super) fn safety_value(
    location: String,
    kind: SafetyFieldKind,
    value: impl AsRef<[u8]>,
) -> RepositorySafetyValue {
    RepositorySafetyValue {
        location,
        kind,
        value: value.as_ref().to_vec(),
    }
}

pub(super) fn borrowed_repository_projections(
    projections: &[OwnedRepositoryProjection],
) -> Vec<RepositoryProjection<'_>> {
    projections
        .iter()
        .map(|projection| RepositoryProjection {
            path: &projection.relative_path,
            bytes: &projection.bytes,
            target_revision: projection.target_revision.as_deref(),
            purpose: projection.purpose,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authorize_repository_projection_batch(
    paths: &MemoryPaths,
    route: RepositoryWriteRoute,
    sensitivity: OkfProposalSensitivity,
    scope_kind: ScopeKind,
    scope_id: Option<&str>,
    visibility: Visibility,
    authorization: AuthorizationProof<'_>,
    provenance: ProvenanceAssessment<'_>,
    values: &[RepositorySafetyValue],
    projections: &[OwnedRepositoryProjection],
) -> Result<AuthorizedRepositoryProjectionBatch> {
    let project_identity = repository_io::repository_project_identity(&paths.project_root)?;
    let fields = values
        .iter()
        .map(|value| SafetyField {
            location: &value.location,
            kind: value.kind,
            value: &value.value,
        })
        .collect();
    let projections = borrowed_repository_projections(projections);
    let request = RepositoryWriteRequest {
        route,
        destination: MemoryDestination::Repo,
        sensitivity,
        scope: RepositoryScope {
            kind: scope_kind,
            id: scope_id,
            current_project_identity: &project_identity,
            configured_project_id: None,
        },
        visibility,
        authorization,
        freshness: Vec::new(),
        provenance,
        fields,
        projections,
    };
    let policy_context_digest = repository_write_policy_context_digest(&request);
    let capability = authorize_repository_write(&request).map_err(anyhow::Error::new)?;
    Ok(AuthorizedRepositoryProjectionBatch {
        capability,
        policy_context_digest,
    })
}

pub(super) fn create_authorized_repository_batch(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
) -> Result<Vec<CreatedRepositoryFile>> {
    let borrowed = borrowed_repository_projections(projections);
    repository_io::create_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )
    .map(|created| created.into_iter().map(CreatedRepositoryFile).collect())
}

pub(super) fn remove_created_repository_file(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    created: &CreatedRepositoryFile,
) -> Result<()> {
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::remove_created_repository_file(
        &paths.project_root,
        mutation.route,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        &created.0,
    )
}

pub(super) fn explicit_repository_provenance(
    content_class: RepositoryContentClass,
    source_identity: &str,
) -> ProvenanceAssessment<'_> {
    let valid = !source_identity.trim().is_empty();
    ProvenanceAssessment {
        present: valid,
        evidence_valid: valid,
        content_class,
        source_identity: valid.then_some(source_identity),
    }
}

pub(super) fn canonical_write_projections(
    paths: &MemoryPaths,
    writes: &[CanonicalFileWrite],
) -> Result<Vec<OwnedRepositoryProjection>> {
    let mut projections = Vec::with_capacity(writes.len() * 2);
    for write in writes {
        projections.push(OwnedRepositoryProjection::from_absolute(
            paths,
            &write.path,
            write.markdown.as_bytes(),
            write.expected_existing_hash.as_deref(),
        )?);
        if let Some(expected_revision) = write.expected_existing_hash.as_deref() {
            let existing_bytes = fs::read(&write.path)
                .context("failed to snapshot existing repository projection bytes")?;
            projections.push(OwnedRepositoryProjection::existing_from_absolute(
                paths,
                &write.path,
                &existing_bytes,
                expected_revision,
            )?);
        }
    }
    Ok(projections)
}

#[derive(Clone, Copy)]
pub(super) struct RepositoryMutationAuthorization<'a> {
    pub(super) route: RepositoryWriteRoute,
    pub(super) authorization: &'a AuthorizedRepositoryProjectionBatch,
    pub(super) projections: &'a [OwnedRepositoryProjection],
}

pub(super) fn repository_transaction_root(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join("repository-transactions")
}

pub(super) fn repository_transaction_path(
    paths: &MemoryPaths,
    repository_path: &Path,
    nonce: &str,
    role: &str,
) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi.repository-transaction-path.v1\0");
    hasher.update(repository_path.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    hasher.update(nonce.as_bytes());
    hasher.update(b"\0");
    hasher.update(role.as_bytes());
    repository_transaction_root(paths).join(format!(
        ".{nonce}.{}.{}.tmp",
        hasher.finalize().to_hex(),
        role
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_authorized_file(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    final_path: &Path,
    contents: &str,
    nonce: &str,
) -> Result<PathBuf> {
    let borrowed = borrowed_repository_projections(projections);
    repository_io::verify_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )?;
    let expected =
        OwnedRepositoryProjection::from_absolute(paths, final_path, contents.as_bytes(), None)?;
    if !projections.iter().any(|projection| {
        projection.purpose == RepositoryProjectionPurpose::Write
            && projection.relative_path == expected.relative_path
            && projection.bytes == expected.bytes
    }) {
        bail!("staged repository file is not present in the authorized projection batch");
    }
    let temp_path = repository_transaction_path(paths, final_path, nonce, "write");
    let parent = temp_path.parent().context("staged file has no parent")?;
    if parent.starts_with(&paths.project_root) {
        bail!("local repository transaction storage must be outside the project worktree");
    }
    ensure_safe_directory(
        &paths.runtime_dir,
        parent,
        true,
        "local repository transaction root",
    )?;
    if parent
        .canonicalize()
        .context("failed to resolve local repository transaction root")?
        .starts_with(
            paths
                .project_root
                .canonicalize()
                .context("failed to resolve project root for repository staging")?,
        )
    {
        bail!("local repository transaction storage must be outside the project worktree");
    }
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
    sync_directory(parent).context("failed to sync local repository transaction root")?;
    Ok(temp_path)
}

pub(super) fn install_verified_staged_file_no_replace(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    staged: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<CreatedRepositoryFile> {
    install_verified_staged_projection_no_replace(
        paths,
        mutation,
        staged,
        destination,
        expected_hash,
        RepositoryProjectionPurpose::Write,
    )
}

fn install_verified_staged_projection_no_replace(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    staged: &Path,
    destination: &Path,
    expected_hash: &str,
    expected_purpose: RepositoryProjectionPurpose,
) -> Result<CreatedRepositoryFile> {
    let relative = destination
        .strip_prefix(&paths.project_root)
        .context("repository install destination is outside the project root")?;
    let projection_index = select_authorized_projection_index(
        mutation.projections,
        relative,
        expected_hash,
        expected_purpose,
    )?;
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::install_transaction_file_no_replace(
        &paths.project_root,
        mutation.route,
        expected_purpose,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        projection_index,
        &repository_transaction_root(paths),
        staged,
    )
    .map(CreatedRepositoryFile)
}

fn select_authorized_projection_index(
    projections: &[OwnedRepositoryProjection],
    relative_path: &Path,
    expected_hash: &str,
    expected_purpose: RepositoryProjectionPurpose,
) -> Result<usize> {
    let matches = projections
        .iter()
        .enumerate()
        .filter(|(_, projection)| {
            projection.purpose == expected_purpose
                && projection.relative_path == relative_path
                && blake3::hash(&projection.bytes).to_hex().as_str() == expected_hash
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [projection_index] = matches.as_slice() else {
        bail!(
            "repository install must select exactly one authorized path, byte, and purpose projection"
        );
    };
    Ok(*projection_index)
}

pub(super) fn restore_verified_staged_file_no_replace(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    staged: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<()> {
    install_verified_staged_projection_no_replace(
        paths,
        mutation,
        staged,
        destination,
        expected_hash,
        RepositoryProjectionPurpose::Existing,
    )?;
    remove_staged_file(staged).with_context(|| {
        format!(
            "failed to remove restored transaction source {}",
            staged.display()
        )
    })
}

pub(super) fn backup_repository_file_to_transaction(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    source: &Path,
    backup: &Path,
    expected_hash: &str,
) -> Result<()> {
    backup_repository_file_to_transaction_with_identity(
        paths,
        mutation,
        source,
        backup,
        expected_hash,
        None,
    )
}

pub(super) fn backup_repository_file_to_transaction_with_identity(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    source: &Path,
    backup: &Path,
    expected_hash: &str,
    expected_identity: Option<RepositoryFileIdentity>,
) -> Result<()> {
    let relative = source
        .strip_prefix(&paths.project_root)
        .context("repository backup source is outside the project root")?;
    let matches = mutation
        .projections
        .iter()
        .enumerate()
        .filter(|(_, projection)| {
            projection.purpose == RepositoryProjectionPurpose::Existing
                && projection.relative_path == relative
                && projection.target_revision.as_deref() == Some(expected_hash)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [projection_index] = matches.as_slice() else {
        bail!("repository backup must select exactly one authorized target revision");
    };
    let borrowed = borrowed_repository_projections(mutation.projections);
    repository_io::backup_repository_file(
        &paths.project_root,
        mutation.route,
        &mutation.authorization.policy_context_digest,
        &mutation.authorization.capability,
        &borrowed,
        *projection_index,
        expected_identity,
        &repository_transaction_root(paths),
        backup,
    )
}

pub(super) fn remove_installed_repository_file(
    paths: &MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    installed: &CreatedRepositoryFile,
) -> Result<()> {
    remove_created_repository_file(paths, mutation, installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_selection_distinguishes_identical_write_and_existing_bytes() {
        let relative_path = Path::new(".memzoi/records/identical.md");
        let bytes = b"identical canonical bytes";
        let hash = blake3::hash(bytes).to_hex().to_string();
        let projections = vec![
            OwnedRepositoryProjection {
                relative_path: relative_path.to_path_buf(),
                bytes: bytes.to_vec(),
                target_revision: None,
                purpose: RepositoryProjectionPurpose::Write,
            },
            OwnedRepositoryProjection {
                relative_path: relative_path.to_path_buf(),
                bytes: bytes.to_vec(),
                target_revision: Some(hash.clone()),
                purpose: RepositoryProjectionPurpose::Existing,
            },
        ];

        assert_eq!(
            select_authorized_projection_index(
                &projections,
                relative_path,
                &hash,
                RepositoryProjectionPurpose::Write,
            )
            .unwrap(),
            0
        );
        assert_eq!(
            select_authorized_projection_index(
                &projections,
                relative_path,
                &hash,
                RepositoryProjectionPurpose::Existing,
            )
            .unwrap(),
            1
        );
    }
}
