use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::{
    AuthorizationProof, MemoryDestination, MemoryPaths, OkfProposalSensitivity,
    RepositoryContentClass, RepositoryWriteRoute, SafetyFieldKind, ScopeKind, Visibility,
    expiry::{self, Clock},
    okf, proposals,
    session_end::{
        SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument, SessionEndResult,
        SessionEndWrite, repo_sensitivity_block_reason, session_end_proposal_draft,
        validate_session_end_document,
    },
};

use super::{
    proposal_packets::ProposalPacketLifecycle,
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, CreatedRepositoryFile, OwnedRepositoryProjection,
        RepositoryMutationAuthorization, authorize_repository_projection_batch,
        create_authorized_repository_batch, explicit_repository_provenance,
        okf_proposal_safety_values, remove_created_repository_file, safety_value,
    },
    runtime_records::{
        CheckpointInput, LocalMemoryInput, RuntimeRecords, reserved_runtime_record_ids,
    },
    safe_files::RepoLifecycleLock,
    shared_runtime,
};

pub(super) struct SessionEndRouteApply<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    shared_conn: &'a Connection,
    clock: &'a dyn Clock,
}

impl<'a> SessionEndRouteApply<'a> {
    pub(super) fn new(
        paths: &'a MemoryPaths,
        conn: &'a Connection,
        shared_conn: &'a Connection,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            paths,
            conn,
            shared_conn,
            clock,
        }
    }

    pub(super) fn promote(
        &self,
        actor: &str,
        document: SessionEndDocument,
    ) -> Result<SessionEndResult> {
        validate_session_end_document(&document)?;
        let mut unsafe_repo_candidates = BTreeSet::new();
        for (index, candidate) in document.candidates.iter().enumerate() {
            if candidate.destination != MemoryDestination::Repo {
                continue;
            }
            let scope = candidate.scope.as_ref();
            let mut values = vec![
                safety_value(
                    "session_end.task".to_owned(),
                    SafetyFieldKind::SourceReference,
                    &document.task,
                ),
                safety_value(
                    format!("candidate[{index}].title"),
                    SafetyFieldKind::Text,
                    &candidate.title,
                ),
                safety_value(
                    format!("candidate[{index}].body"),
                    SafetyFieldKind::Text,
                    &candidate.body,
                ),
            ];
            if let Some(reason) = candidate.reason.as_deref() {
                values.push(safety_value(
                    format!("candidate[{index}].reason"),
                    SafetyFieldKind::Reason,
                    reason,
                ));
            }
            for (tag_index, tag) in candidate.tags.iter().enumerate() {
                values.push(safety_value(
                    format!("candidate[{index}].tags[{tag_index}]"),
                    SafetyFieldKind::Text,
                    tag,
                ));
            }
            if authorize_repository_projection_batch(
                self.paths,
                RepositoryWriteRoute::SessionEndPromotion,
                candidate.sensitivity,
                scope.map_or(ScopeKind::Repo, |scope| scope.kind),
                scope.and_then(|scope| scope.id.as_deref()),
                Visibility::Repo,
                AuthorizationProof::ExplicitCommand {
                    operation: "session_end_assessment",
                },
                explicit_repository_provenance(candidate.content_class, &document.task),
                &values,
                &[],
            )
            .is_err()
            {
                unsafe_repo_candidates.insert(index);
            }
        }
        if !unsafe_repo_candidates.is_empty() {
            return Ok(blocked_result(document, &unsafe_repo_candidates));
        }
        let has_repo_writes = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let has_runtime_writes = document.candidates.iter().any(|candidate| {
            matches!(
                candidate.destination,
                MemoryDestination::Local | MemoryDestination::Session
            )
        });
        let _lifecycle_lock = (has_repo_writes || has_runtime_writes)
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        if has_repo_writes || has_runtime_writes {
            shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)?;
        }
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.shared_conn);
        let timestamp = expiry::format_timestamp(self.clock.now_utc())?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut reserved_proposal_ids = if has_repo_writes {
            proposal_packets.prepare_identity_space()?
        } else {
            BTreeSet::new()
        };
        let mut repo_plans = Vec::with_capacity(document.candidates.len());

        for candidate in &document.candidates {
            if candidate.destination == MemoryDestination::Repo {
                let base_slug = proposals::title_to_concept_slug(&candidate.title)
                    .unwrap_or_else(|| "memory".to_owned());
                let base_id = format!("mem_session_{base_slug}");
                let proposal_id = okf::reserve_okf_proposal_id(
                    &pending_root,
                    &base_id,
                    &mut reserved_proposal_ids,
                )?;
                let draft = session_end_proposal_draft(candidate, actor, &timestamp, proposal_id)?;
                repo_plans.push(Some(okf::plan_okf_create_proposal(&pending_root, &draft)?));
            } else {
                repo_plans.push(None);
            }
        }
        let repo_projections = repo_plans
            .iter()
            .filter_map(Option::as_ref)
            .map(|plan| {
                OwnedRepositoryProjection::from_absolute(
                    self.paths,
                    &plan.path,
                    plan.markdown.as_bytes(),
                    None,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let safety_values = session_end_repository_safety_values(&document.task, &repo_plans);
        let repo_authorization = has_repo_writes
            .then(|| {
                authorize_repository_projection_batch(
                    self.paths,
                    RepositoryWriteRoute::SessionEndPromotion,
                    OkfProposalSensitivity::RepoSafe,
                    ScopeKind::Repo,
                    None,
                    Visibility::Repo,
                    AuthorizationProof::ExplicitCommand {
                        operation: "session_end_promotion",
                    },
                    explicit_repository_provenance(
                        RepositoryContentClass::GeneralRepoKnowledge,
                        &document.task,
                    ),
                    &safety_values,
                    &repo_projections,
                )
            })
            .transpose()?;
        let reserved_runtime_ids = if has_runtime_writes {
            reserved_runtime_record_ids(self.paths, self.conn)?
        } else {
            BTreeSet::new()
        };

        let mut repo_writes = vec![None::<(String, PathBuf)>; document.candidates.len()];
        let mut runtime_writes =
            vec![None::<(String, MemoryDestination)>; document.candidates.len()];
        let mut created_proposal_files = Vec::new();
        let write_result = (|| -> Result<()> {
            if has_repo_writes {
                proposal_packets
                    .ensure_planned_available(repo_plans.iter().filter_map(Option::as_ref))?;
            }
            if let Some(authorization) = repo_authorization.as_ref() {
                let created = create_authorized_repository_batch(
                    self.paths,
                    RepositoryWriteRoute::SessionEndPromotion,
                    authorization,
                    &repo_projections,
                )?;
                created_proposal_files.extend(created);
                let mut created_iter = created_proposal_files.iter();
                for (index, plan) in repo_plans.iter().enumerate() {
                    let Some(plan) = plan else {
                        continue;
                    };
                    let created_file = created_iter
                        .next()
                        .context("authorized session-end projection count changed")?;
                    repo_writes[index] =
                        Some((plan.proposal_id.clone(), created_file.path().to_path_buf()));
                }
            }
            let tx = self.shared_conn.unchecked_transaction()?;
            for (index, candidate) in document.candidates.iter().enumerate() {
                match candidate.destination {
                    MemoryDestination::Local => {
                        let record = RuntimeRecords::new(&tx).create_local_avoiding(
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                            &reserved_runtime_ids,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Local));
                    }
                    MemoryDestination::Session => {
                        let record = RuntimeRecords::new(&tx).create_checkpoint_avoiding(
                            actor,
                            &CheckpointInput {
                                task: candidate.title.clone(),
                                note: candidate.body.clone(),
                            },
                            &timestamp,
                            &reserved_runtime_ids,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Session));
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Discard
                    | MemoryDestination::NeedsReview => {}
                }
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            if created_proposal_files.is_empty() {
                return Err(error);
            }
            if let Some(authorization) = repo_authorization.as_ref() {
                if let Err(cleanup_error) = cleanup_authorized_session_end_proposals(
                    self.paths,
                    authorization,
                    &repo_projections,
                    &created_proposal_files,
                ) {
                    return Err(error).context(format!(
                        "session-end promotion failed; additionally failed to clean up created proposal files: {cleanup_error}"
                    ));
                }
            } else {
                return Err(error).context(
                    "session-end promotion failed; created repository files have no matching authorization",
                );
            }
            return Err(error);
        }
        if has_runtime_writes {
            shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)
                .context("session-end runtime writes committed but worktree index refresh failed")?;
        }

        let mut results = Vec::with_capacity(document.candidates.len());
        for (index, candidate) in document.candidates.into_iter().enumerate() {
            let title = candidate.title.trim().to_owned();
            let write = match candidate.destination {
                MemoryDestination::Repo => {
                    let (proposal_id, path) = repo_writes[index]
                        .clone()
                        .context("repo session-end candidate should have a proposal write")?;
                    Some(SessionEndWrite::ProposalFile { proposal_id, path })
                }
                MemoryDestination::Local => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("local session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Session => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Discard | MemoryDestination::NeedsReview => None,
            };
            let status = match candidate.destination {
                MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                MemoryDestination::Repo | MemoryDestination::Local | MemoryDestination::Session => {
                    SessionEndCandidateStatus::Written
                }
            };
            results.push(SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason: match candidate.destination {
                    MemoryDestination::Discard => {
                        Some("discard destination performs no write".to_owned())
                    }
                    MemoryDestination::NeedsReview => {
                        Some("candidate requires human review before writing".to_owned())
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => None,
                },
                write,
            });
        }

        Ok(SessionEndResult {
            task: document.task.trim().to_owned(),
            candidates: results,
        })
    }
}

fn session_end_repository_safety_values(
    task: &str,
    repo_plans: &[Option<okf::OkfCreateProposalPlan>],
) -> Vec<super::repository_mutation::RepositorySafetyValue> {
    let mut values = vec![safety_value(
        "session_end.task".to_owned(),
        SafetyFieldKind::SourceReference,
        task.as_bytes(),
    )];
    for (index, plan) in repo_plans.iter().enumerate() {
        let Some(plan) = plan else {
            continue;
        };
        values.extend(okf_proposal_safety_values(
            &format!("candidate[{index}]"),
            &plan.parsed,
        ));
    }
    values
}

fn cleanup_authorized_session_end_proposals(
    paths: &crate::MemoryPaths,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    created: &[CreatedRepositoryFile],
) -> Result<()> {
    if created.len() != projections.len() {
        anyhow::bail!(
            "created session-end proposal batch does not match its authorized projections"
        );
    }
    let mutation = RepositoryMutationAuthorization {
        route: RepositoryWriteRoute::SessionEndPromotion,
        authorization,
        projections,
    };
    let mut cleanup_errors = Vec::new();
    for (created_file, _) in created.iter().zip(projections).rev() {
        if let Err(error) = remove_created_repository_file(paths, mutation, created_file) {
            cleanup_errors.push(format!("{}: {error:#}", created_file.path().display()));
        }
    }
    if cleanup_errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{}", cleanup_errors.join("; "))
    }
}

fn blocked_result(
    document: SessionEndDocument,
    unsafe_repo_candidates: &BTreeSet<usize>,
) -> SessionEndResult {
    let candidates = document
        .candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let unsafe_repo_candidate = unsafe_repo_candidates.contains(&index);
            let (title, status, reason) = if unsafe_repo_candidate {
                (
                    if candidate.sensitivity != OkfProposalSensitivity::RepoSafe {
                        "Redacted non-repo-safe candidate".to_owned()
                    } else {
                        "Redacted unsafe repository candidate".to_owned()
                    },
                    SessionEndCandidateStatus::Blocked,
                    Some(if candidate.sensitivity != OkfProposalSensitivity::RepoSafe {
                        repo_sensitivity_block_reason(candidate.sensitivity)
                    } else {
                        "repository safety policy blocked this candidate; inspect the hash-only safety report and sanitize the source before retrying".to_owned()
                    }),
                )
            } else {
                let status = match candidate.destination {
                    MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session
                    | MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                };
                let reason = match candidate.destination {
                    MemoryDestination::Discard => {
                        "discard destination performs no write".to_owned()
                    }
                    MemoryDestination::NeedsReview => {
                        "candidate requires human review before writing".to_owned()
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => "session-end batch contains an unsafe repo candidate; no writes were performed".to_owned(),
                };
                (
                    "Redacted blocked session-end candidate".to_owned(),
                    status,
                    Some(reason),
                )
            };
            SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason,
                write: None,
            }
        })
        .collect();
    SessionEndResult {
        task: "Redacted blocked session-end task".to_owned(),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn repository_safety_values_preserve_mixed_candidate_indices() -> Result<()> {
        let pending_root = tempfile::tempdir()?;
        let candidate = crate::SessionEndCandidate {
            destination: MemoryDestination::Repo,
            memory_type: crate::MemoryType::Fact,
            lane: crate::MemoryLane::Semantic,
            title: "Repository candidate".to_owned(),
            body: "Keep its original document index.".to_owned(),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            reason: None,
            scope: None,
            tags: Vec::new(),
        };
        let draft = session_end_proposal_draft(
            &candidate,
            "agent:test",
            "2026-07-15T00:00:00Z",
            "mem_session_indexed".to_owned(),
        )?;
        let plan = okf::plan_okf_create_proposal(pending_root.path(), &draft)?;

        let values = session_end_repository_safety_values("mixed batch", &[None, Some(plan)]);
        let locations = values
            .iter()
            .map(|value| value.location.as_str())
            .collect::<Vec<_>>();

        assert!(locations.contains(&"candidate[1].title"));
        assert!(
            !locations
                .iter()
                .any(|location| location.starts_with("candidate[0]."))
        );
        Ok(())
    }

    #[test]
    fn session_end_cleanup_preserves_a_concurrent_repository_replacement() -> Result<()> {
        let project = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let paths = crate::MemoryPaths::with_runtime_home(
            project.path().to_path_buf(),
            runtime.path().to_path_buf(),
        );
        let destination = paths.proposals_dir().join("pending/mem_session_cleanup.md");
        let authorized_bytes = b"authorized session-end proposal\n";
        let projections = vec![OwnedRepositoryProjection::from_absolute(
            &paths,
            &destination,
            authorized_bytes,
            None,
        )?];
        let authorization = authorize_repository_projection_batch(
            &paths,
            RepositoryWriteRoute::SessionEndPromotion,
            OkfProposalSensitivity::RepoSafe,
            ScopeKind::Repo,
            None,
            Visibility::Repo,
            AuthorizationProof::ExplicitCommand {
                operation: "session_end_cleanup_test",
            },
            explicit_repository_provenance(
                RepositoryContentClass::GeneralRepoKnowledge,
                "session-end-cleanup-test",
            ),
            &[],
            &projections,
        )?;
        let created = create_authorized_repository_batch(
            &paths,
            RepositoryWriteRoute::SessionEndPromotion,
            &authorization,
            &projections,
        )?;
        let replacement = b"concurrent human replacement\n";
        let mut destination_file = std::fs::File::options()
            .write(true)
            .truncate(true)
            .open(&destination)?;
        std::io::copy(&mut replacement.as_slice(), &mut destination_file)?;
        destination_file.flush()?;

        let error = cleanup_authorized_session_end_proposals(
            &paths,
            &authorization,
            &projections,
            &created,
        )
        .expect_err("cleanup must not delete bytes not authorized by session-end");

        assert!(format!("{error:#}").contains("does not match"));
        assert_eq!(std::fs::read(&destination)?, replacement);
        Ok(())
    }
}
