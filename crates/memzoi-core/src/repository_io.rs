use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{AuthorizedRepositoryWriteBatch, RepositoryProjection, RepositoryWriteRoute};

#[cfg(unix)]
fn repository_directory_mode() -> rustix::fs::Mode {
    use rustix::fs::Mode;

    Mode::RUSR | Mode::WUSR | Mode::XUSR | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH
}

#[cfg(unix)]
fn repository_file_mode() -> rustix::fs::Mode {
    use rustix::fs::Mode;

    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH
}

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
    for projection in projections {
        if projection.target_revision.is_some() {
            bail!("create-only repository batch cannot contain overwrite projections");
        }
    }

    #[cfg(not(unix))]
    {
        bail!(
            "secure repository file creation is unavailable on this platform; write fails closed"
        );
    }

    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::fd::OwnedFd};

        use rustix::{
            fs::{AtFlags, CWD, Mode, OFlags, fsync, mkdirat, openat, unlinkat},
            io::Errno,
        };

        let root = project_root
            .canonicalize()
            .context("failed to resolve repository root for secure creation")?;
        let directory_flags =
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let directory_mode = repository_directory_mode();
        let file_mode = repository_file_mode();
        let mut destinations = Vec::with_capacity(projections.len());
        let mut created = Vec::<(OwnedFd, OsString)>::with_capacity(projections.len());

        for projection in projections {
            let destination = root.join(projection.path);
            let result = (|| -> Result<(OwnedFd, OsString)> {
                let mut directory = openat(CWD, &root, directory_flags, Mode::empty())
                    .context("failed to open repository root without following symlinks")?;
                let mut components = projection.path.components().peekable();
                let mut file_name = None;
                while let Some(component) = components.next() {
                    let std::path::Component::Normal(component) = component else {
                        bail!("unsafe repository-relative output path");
                    };
                    if components.peek().is_none() {
                        file_name = Some(component.to_os_string());
                        break;
                    }
                    let next_directory = match openat(
                        &directory,
                        component,
                        directory_flags,
                        Mode::empty(),
                    ) {
                        Ok(directory) => directory,
                        Err(Errno::NOENT) => {
                            match mkdirat(&directory, component, directory_mode) {
                                Ok(()) | Err(Errno::EXIST) => {}
                                Err(error) => {
                                    return Err(error).context(
                                        "failed to create authorized repository directory",
                                    );
                                }
                            }
                            openat(&directory, component, directory_flags, Mode::empty()).context(
                                "failed to open authorized repository directory without following symlinks",
                            )?
                        }
                        Err(error) => {
                            return Err(error).context(
                                "failed to open authorized repository directory without following symlinks",
                            );
                        }
                    };
                    directory = next_directory;
                }
                let file_name = file_name.context("repository destination has no file name")?;
                let file = match openat(
                    &directory,
                    &file_name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    file_mode,
                ) {
                    Ok(file) => file,
                    Err(Errno::EXIST) => {
                        bail!(
                            "repository destination already exists: {}",
                            destination.display()
                        )
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "failed to create authorized repository file {}",
                                destination.display()
                            )
                        });
                    }
                };
                let mut file = fs::File::from(file);
                if let Err(error) = file
                    .write_all(projection.bytes)
                    .and_then(|_| file.sync_all())
                {
                    drop(file);
                    let _ = unlinkat(&directory, &file_name, AtFlags::empty());
                    return Err(error).with_context(|| {
                        format!(
                            "failed to write authorized repository file {}",
                            destination.display()
                        )
                    });
                }
                drop(file);
                if let Err(error) = fsync(&directory) {
                    let _ = unlinkat(&directory, &file_name, AtFlags::empty());
                    return Err(error).context("failed to sync authorized repository directory");
                }
                Ok((directory, file_name))
            })();
            let created_file = match result {
                Ok(created_file) => created_file,
                Err(error) => {
                    for (directory, file_name) in created.iter().rev() {
                        let _ = unlinkat(directory, file_name, AtFlags::empty());
                    }
                    return Err(error);
                }
            };
            created.push(created_file);
            destinations.push(destination);
        }
        Ok(destinations)
    }
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

    fn authorize_create<'a>(
        project_root: &Path,
        projections: &[RepositoryProjection<'a>],
    ) -> ([u8; 32], AuthorizedRepositoryWriteBatch) {
        let fields = projections
            .iter()
            .map(|projection| SafetyField {
                location: "candidate.body",
                kind: SafetyFieldKind::Text,
                value: projection.bytes,
            })
            .collect::<Vec<_>>();
        let request = RepositoryWriteRequest {
            route: RepositoryWriteRoute::FileProposalCreate,
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            scope: RepositoryScope {
                kind: ScopeKind::Repo,
                id: None,
                current_project_identity: project_root.as_os_str().as_encoded_bytes(),
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
            fields,
            projections: projections.to_vec(),
        };
        let context_digest = repository_write_policy_context_digest(&request);
        let token = authorize_repository_write(&request).unwrap();
        (context_digest, token)
    }

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
        let relative = Path::new(".memzoi/records/safe.md");
        let bytes = b"safe repository knowledge";
        let projections = [RepositoryProjection {
            path: relative,
            bytes,
            target_revision: None,
        }];
        let (context_digest, token) = authorize_create(temp.path(), &projections);
        let destination = temp.path().join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        symlink(&target, &destination).unwrap();

        assert!(
            create_repository_batch(
                temp.path(),
                RepositoryWriteRoute::FileProposalCreate,
                &context_digest,
                &token,
                &projections,
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "outside");
    }

    #[cfg(unix)]
    #[test]
    fn repository_creation_modes_never_request_group_or_world_write_access() {
        use std::os::unix::fs::PermissionsExt;

        use rustix::fs::Mode;

        let writable_by_others = Mode::WGRP | Mode::WOTH;
        assert!(!repository_directory_mode().intersects(writable_by_others));
        assert!(!repository_file_mode().intersects(writable_by_others));

        let temp = tempfile::tempdir().unwrap();
        let relative = Path::new(".memzoi/records/nested/safe.md");
        let projections = [RepositoryProjection {
            path: relative,
            bytes: b"safe repository knowledge",
            target_revision: None,
        }];
        let (context_digest, token) = authorize_create(temp.path(), &projections);
        create_repository_batch(
            temp.path(),
            RepositoryWriteRoute::FileProposalCreate,
            &context_digest,
            &token,
            &projections,
        )
        .unwrap();

        let file_mode = fs::metadata(temp.path().join(relative))
            .unwrap()
            .permissions()
            .mode();
        let directory_mode = fs::metadata(temp.path().join(".memzoi/records/nested"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o022, 0);
        assert_eq!(directory_mode & 0o022, 0);
    }
}
