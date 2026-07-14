use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{AuthorizedRepositoryWriteBatch, RepositoryProjection, RepositoryWriteRoute};

pub(crate) fn verify_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<()> {
    if !authorization.authorizes(
        expected_route,
        project_root.as_os_str().as_encoded_bytes(),
        expected_policy_context_digest,
        projections,
    ) {
        bail!(
            "repository write authorization does not match the exact route, project, paths, revisions, and bytes"
        );
    }
    for projection in projections {
        verify_projection_path(project_root, projection.path)?;
    }
    Ok(())
}

pub(crate) fn create_repository_batch(
    project_root: &Path,
    expected_route: RepositoryWriteRoute,
    expected_policy_context_digest: &[u8; 32],
    authorization: &AuthorizedRepositoryWriteBatch,
    projections: &[RepositoryProjection<'_>],
) -> Result<Vec<PathBuf>> {
    verify_repository_batch(
        project_root,
        expected_route,
        expected_policy_context_digest,
        authorization,
        projections,
    )?;
    let mut destinations = Vec::with_capacity(projections.len());
    for projection in projections {
        if projection.target_revision.is_some() {
            bail!("create-only repository batch cannot contain overwrite projections");
        }
        let destination = project_root.join(projection.path);
        if destination.try_exists().with_context(|| {
            format!(
                "failed to inspect repository destination {}",
                destination.display()
            )
        })? {
            bail!(
                "repository destination already exists: {}",
                destination.display()
            );
        }
    }

    for projection in projections {
        let destination = project_root.join(projection.path);
        let result = (|| -> Result<()> {
            let parent = destination
                .parent()
                .context("repository destination has no parent")?;
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create repository directory {}", parent.display())
            })?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .with_context(|| {
                    format!(
                        "failed to create authorized repository file {}",
                        destination.display()
                    )
                })?;
            file.write_all(projection.bytes)
                .and_then(|_| file.sync_all())
                .with_context(|| {
                    format!(
                        "failed to write authorized repository file {}",
                        destination.display()
                    )
                })
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&destination);
            for created in destinations.iter().rev() {
                let _ = fs::remove_file(created);
            }
            return Err(error);
        }
        destinations.push(destination);
    }
    Ok(destinations)
}

pub(crate) fn verify_projection_path(project_root: &Path, relative: &Path) -> Result<PathBuf> {
    if !crate::repository_write_safety::projection_path_is_safe(relative) {
        bail!("unsafe repository-relative output path");
    }
    let root = project_root
        .canonicalize()
        .context("failed to resolve repository root")?;
    let mut cursor = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            bail!("unsafe repository-relative output path");
        };
        cursor.push(component);
        if components.peek().is_none() {
            match fs::symlink_metadata(&cursor) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!("repository output destination must not be a symlink")
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("failed to inspect repository output destination");
                }
            }
            break;
        }
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("repository output parent is not a safe directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to inspect repository output parent"),
        }
    }
    Ok(project_root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_write_safety::repository_write_policy_context_digest;
    use crate::{
        AuthorizationProof, FreshnessCheck, MemoryDestination, OkfProposalSensitivity,
        ProvenanceAssessment, RepositoryContentClass, RepositoryScope, RepositoryWriteRequest,
        RepositoryWriteRoute, SafetyField, SafetyFieldKind, ScopeKind, Visibility,
        authorize_repository_write,
    };

    #[test]
    fn token_is_bound_to_exact_projection_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let relative = Path::new(".memzoi/proposals/pending/safe.md");
        let bytes = b"safe repository knowledge";
        let projections = [RepositoryProjection {
            path: relative,
            bytes,
            target_revision: None,
        }];
        let fields = [SafetyField {
            location: "candidate.body",
            kind: SafetyFieldKind::Text,
            value: bytes,
        }];
        let mut request = RepositoryWriteRequest {
            route: RepositoryWriteRoute::FileProposalCreate,
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            scope: RepositoryScope {
                kind: ScopeKind::Repo,
                id: None,
                current_project_identity: temp.path().as_os_str().as_encoded_bytes(),
                configured_project_id: None,
            },
            visibility: Visibility::Repo,
            authorization: AuthorizationProof::ExplicitCommand { operation: "test" },
            freshness: Vec::<FreshnessCheck<'_>>::new(),
            provenance: ProvenanceAssessment {
                present: true,
                evidence_valid: true,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                source_identity: Some("test"),
            },
            fields: fields.to_vec(),
            projections: projections.to_vec(),
        };
        let context_digest = repository_write_policy_context_digest(&request);
        let token = authorize_repository_write(&request).unwrap();
        let changed = [RepositoryProjection {
            path: relative,
            bytes: b"changed repository knowledge",
            target_revision: None,
        }];
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &changed,
            )
            .is_err()
        );
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &projections,
            )
            .is_ok()
        );
        assert!(
            verify_repository_batch(
                temp.path(),
                RepositoryWriteRoute::ImportApply,
                &context_digest,
                &token,
                &projections,
            )
            .is_err(),
            "a capability minted for one route must fail at another route's mutation seam"
        );

        for changed_context_digest in {
            let mut digests = Vec::new();
            request.destination = MemoryDestination::Local;
            digests.push(repository_write_policy_context_digest(&request));
            request.destination = MemoryDestination::Repo;
            request.sensitivity = OkfProposalSensitivity::Unknown;
            digests.push(repository_write_policy_context_digest(&request));
            request.sensitivity = OkfProposalSensitivity::RepoSafe;
            request.scope.kind = ScopeKind::Team;
            digests.push(repository_write_policy_context_digest(&request));
            request.scope.kind = ScopeKind::Repo;
            request.visibility = Visibility::Private;
            digests.push(repository_write_policy_context_digest(&request));
            request.visibility = Visibility::Repo;
            request.provenance.content_class = RepositoryContentClass::Unknown;
            digests.push(repository_write_policy_context_digest(&request));
            digests
        } {
            assert!(
                verify_repository_batch(
                    temp.path(),
                    RepositoryWriteRoute::FileProposalCreate,
                    &changed_context_digest,
                    &token,
                    &projections,
                )
                .is_err(),
                "changing semantic policy context must invalidate the capability"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn final_destination_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.md");
        fs::write(&target, "outside").unwrap();
        let destination = temp.path().join(".memzoi/records/safe.md");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        symlink(&target, &destination).unwrap();

        assert!(verify_projection_path(temp.path(), Path::new(".memzoi/records/safe.md")).is_err());
    }
}
