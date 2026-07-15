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
        OwnedRepositoryProjection, authorize_repository_projection_batch,
        create_authorized_repository_batch, explicit_repository_provenance,
        okf_proposal_safety_values, safety_value,
    },
    runtime_records::{CheckpointInput, LocalMemoryInput, RuntimeRecords},
    safe_files::RepoLifecycleLock,
};

pub(super) struct SessionEndRouteApply<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    clock: &'a dyn Clock,
}

impl<'a> SessionEndRouteApply<'a> {
    pub(super) fn new(paths: &'a MemoryPaths, conn: &'a Connection, clock: &'a dyn Clock) -> Self {
        Self { paths, conn, clock }
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
        let _lifecycle_lock = has_repo_writes
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.conn);
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
        let mut safety_values = vec![safety_value(
            "session_end.task".to_owned(),
            SafetyFieldKind::SourceReference,
            document.task.as_bytes(),
        )];
        for (index, plan) in repo_plans.iter().filter_map(Option::as_ref).enumerate() {
            safety_values.extend(okf_proposal_safety_values(
                &format!("candidate[{index}]"),
                &plan.parsed,
            ));
        }
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
                created_proposal_files.extend(created.iter().cloned());
                let mut created = created.into_iter();
                for (index, plan) in repo_plans.iter().enumerate() {
                    let Some(plan) = plan else {
                        continue;
                    };
                    let path = created
                        .next()
                        .context("authorized session-end projection count changed")?;
                    repo_writes[index] = Some((plan.proposal_id.clone(), path));
                }
            }
            let tx = self.conn.unchecked_transaction()?;
            for (index, candidate) in document.candidates.iter().enumerate() {
                match candidate.destination {
                    MemoryDestination::Local => {
                        let record = RuntimeRecords::new(&tx).create_local(
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Local));
                    }
                    MemoryDestination::Session => {
                        let record = RuntimeRecords::new(&tx).create_checkpoint(
                            actor,
                            &CheckpointInput {
                                task: candidate.title.clone(),
                                note: candidate.body.clone(),
                            },
                            &timestamp,
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
            if let Err(cleanup_error) = okf::cleanup_okf_proposal_files(&created_proposal_files) {
                return Err(error).context(format!(
                    "session-end promotion failed; additionally failed to clean up created proposal files: {cleanup_error}"
                ));
            }
            return Err(error);
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
                (candidate.title.trim().to_owned(), status, Some(reason))
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
        task: document.task.trim().to_owned(),
        candidates,
    }
}
