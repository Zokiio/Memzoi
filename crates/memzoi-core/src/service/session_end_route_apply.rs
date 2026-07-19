use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::json;

use crate::{
    AuthorizationProof, MemoryDestination, MemoryEventDataClass, MemoryPaths,
    OkfProposalSensitivity, OriginIdentity, OriginLookup, OriginOutcome, OriginOutcomeKind,
    OriginPreparation, OriginRoute, RepositoryContentClass, RepositoryWriteRoute, SafetyFieldKind,
    ScopeKind, Visibility,
    events::{AppendEvent, append_event},
    expiry::{self, Clock},
    okf, proposals,
    session_end::{
        SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument, SessionEndResult,
        SessionEndWrite, repo_sensitivity_block_reason, session_end_proposal_draft,
        validate_session_end_document,
    },
};

use super::{
    SessionEndFromCheckpointCommand, SessionEndFromCheckpointResult, checkpoint_result_from_origin,
    proposal_packets::ProposalPacketLifecycle,
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, CreatedRepositoryFile, OwnedRepositoryProjection,
        RepositoryMutationAuthorization, authorize_repository_projection_batch,
        create_authorized_repository_batch, explicit_repository_provenance,
        okf_proposal_safety_values, remove_created_repository_file, safety_value,
    },
    runtime_records::{
        CheckpointInput, CloseCheckpointCommand, LocalMemoryInput, RuntimeRecords,
        reserved_runtime_record_ids,
    },
    safe_files::RepoLifecycleLock,
    shared_runtime,
};

mod journal;

use self::journal::{
    ArtifactState, SessionEndOriginArtifact, SessionEndOriginJournal, artifact_state, journal_for,
    read_pending, remove_journal, write_journal,
};

#[cfg(test)]
thread_local! {
    static AFTER_REPOSITORY_INSTALL_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn inject_after_repository_install_hook(hook: impl FnOnce() + 'static) {
    AFTER_REPOSITORY_INSTALL_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_after_repository_install_hook() {
    AFTER_REPOSITORY_INSTALL_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_after_repository_install_hook() {}

pub(super) struct SessionEndRouteApply<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    shared_conn: &'a Connection,
    clock: &'a dyn Clock,
}

struct SessionEndRouteResult {
    promotion: SessionEndResult,
    closure: Option<super::CheckpointCommandResult>,
}

struct CheckpointPromotion {
    operation_id: String,
    checkpoint_id: String,
    expected_version: String,
    identity: OriginIdentity,
    fingerprint: String,
    recovery_journal: Option<SessionEndOriginJournal>,
}

fn replayed_checkpoint_promotion(
    conn: &Connection,
    operation_id: &str,
    outcome: &OriginOutcome,
) -> Result<SessionEndFromCheckpointResult> {
    let event_id = outcome
        .lifecycle_event_id
        .as_deref()
        .context("checkpoint session-end origin outcome has no lifecycle event")?;
    let payload_json = conn
        .query_row(
            "SELECT payload_json FROM event_log WHERE id = ?1",
            [event_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("checkpoint session-end event not found: {event_id}"))?;
    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
    let promotion = serde_json::from_value::<SessionEndResult>(
        payload
            .get("promotion")
            .cloned()
            .context("checkpoint session-end event has no promotion outcome")?,
    )?;
    Ok(SessionEndFromCheckpointResult {
        promotion,
        closure: Some(checkpoint_result_from_origin(
            conn,
            operation_id,
            outcome,
            true,
        )?),
    })
}

fn ensure_recovery_artifacts_installed(
    paths: &MemoryPaths,
    journal: &SessionEndOriginJournal,
) -> Result<()> {
    anyhow::ensure!(
        artifact_state(paths, journal)? == ArtifactState::AllInstalled,
        "finalized session-end origin is missing its repository artifact batch"
    );
    Ok(())
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

    pub(super) fn recover_on_open(&self) -> Result<()> {
        let journals = read_pending(self.paths)?;
        if journals.is_empty() {
            return Ok(());
        }
        for journal in journals {
            match crate::lookup_origin(
                self.shared_conn,
                &journal.identity,
                &journal.input_fingerprint,
            )? {
                OriginLookup::Replay(_) => {
                    ensure_recovery_artifacts_installed(self.paths, &journal)?;
                    remove_journal(self.paths, &journal)?;
                }
                OriginLookup::Prepared(_) => anyhow::bail!(
                    "origin_operation_pending: session-end recovery found a durable prepared origin"
                ),
                OriginLookup::Unseen => match artifact_state(self.paths, &journal)? {
                    ArtifactState::NoneInstalled => remove_journal(self.paths, &journal)?,
                    ArtifactState::AllInstalled => {
                        let evaluated_at = time::OffsetDateTime::parse(
                            &journal.evaluated_at,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .context("session-end recovery journal evaluated_at is invalid")?;
                        let checkpoint = RuntimeRecords::new(self.shared_conn)
                            .checkpoint(&journal.checkpoint_id, evaluated_at)?
                            .with_context(|| {
                                format!(
                                    "checkpoint {} is not eligible for ordinary session-end recovery",
                                    journal.checkpoint_id
                                )
                            })?;
                        let document = crate::parse_session_end_document(&checkpoint.body)?;
                        let command = SessionEndFromCheckpointCommand {
                            operation_id: journal.operation_id.clone(),
                            checkpoint_id: journal.checkpoint_id.clone(),
                            expected_version: journal.expected_version.clone(),
                            document: document.clone(),
                        };
                        let fingerprint =
                            crate::origin_input_fingerprint(OriginRoute::SessionEnd, &command)?;
                        anyhow::ensure!(
                            fingerprint == journal.input_fingerprint,
                            "session-end recovery input no longer matches its journal fingerprint"
                        );
                        self.promote_internal(
                            &journal.actor,
                            document,
                            Some(CheckpointPromotion {
                                operation_id: journal.operation_id.clone(),
                                checkpoint_id: journal.checkpoint_id.clone(),
                                expected_version: journal.expected_version.clone(),
                                identity: journal.identity.clone(),
                                fingerprint,
                                recovery_journal: Some(journal.clone()),
                            }),
                        )?;
                    }
                },
            }
        }
        Ok(())
    }

    pub(super) fn promote(
        &self,
        actor: &str,
        document: SessionEndDocument,
    ) -> Result<SessionEndResult> {
        self.promote_internal(actor, document, None)
            .map(|result| result.promotion)
    }

    pub(super) fn promote_from_checkpoint(
        &self,
        actor: &str,
        command: SessionEndFromCheckpointCommand,
    ) -> Result<SessionEndFromCheckpointResult> {
        validate_session_end_document(&command.document)?;
        let route = OriginRoute::SessionEnd;
        let identity = OriginIdentity::new(
            self.paths.repository_key(),
            crate::OriginDescriptor::owner_command(&command.operation_id, route),
        );
        let fingerprint = crate::origin_input_fingerprint(route, &command)?;
        match crate::lookup_origin(self.shared_conn, &identity, &fingerprint)? {
            OriginLookup::Replay(outcome) => {
                return replayed_checkpoint_promotion(
                    self.shared_conn,
                    &command.operation_id,
                    &outcome,
                );
            }
            OriginLookup::Prepared(_) => {
                anyhow::bail!(
                    "origin_operation_pending: checkpoint session-end promotion is already prepared"
                )
            }
            OriginLookup::Unseen => {}
        }
        let operation_id = command.operation_id.clone();
        let route_result = self.promote_internal(
            actor,
            command.document,
            Some(CheckpointPromotion {
                operation_id,
                checkpoint_id: command.checkpoint_id,
                expected_version: command.expected_version,
                identity,
                fingerprint,
                recovery_journal: None,
            }),
        )?;
        Ok(SessionEndFromCheckpointResult {
            promotion: route_result.promotion,
            closure: route_result.closure,
        })
    }

    fn promote_internal(
        &self,
        actor: &str,
        document: SessionEndDocument,
        checkpoint_promotion: Option<CheckpointPromotion>,
    ) -> Result<SessionEndRouteResult> {
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
            return Ok(SessionEndRouteResult {
                promotion: blocked_result(document, &unsafe_repo_candidates),
                closure: None,
            });
        }
        let has_repo_writes = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let has_runtime_writes = checkpoint_promotion.is_some()
            || document.candidates.iter().any(|candidate| {
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
        let evaluated_at = checkpoint_promotion
            .as_ref()
            .and_then(|checkpoint| checkpoint.recovery_journal.as_ref())
            .map(|journal| {
                time::OffsetDateTime::parse(
                    &journal.evaluated_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .context("session-end recovery journal evaluated_at is invalid")
            })
            .transpose()?
            .unwrap_or_else(|| self.clock.now_utc());
        let timestamp = expiry::format_timestamp(evaluated_at)?;
        if let Some(checkpoint) = checkpoint_promotion.as_ref() {
            let record = RuntimeRecords::new(self.shared_conn)
                .checkpoint(&checkpoint.checkpoint_id, evaluated_at)?
                .with_context(|| {
                    format!(
                        "checkpoint {} is not eligible for ordinary session-end promotion",
                        checkpoint.checkpoint_id
                    )
                })?;
            let stored_document = crate::parse_session_end_document(&record.body)?;
            anyhow::ensure!(
                stored_document == document,
                "session-end document does not match checkpoint {}",
                checkpoint.checkpoint_id
            );
            anyhow::ensure!(
                RuntimeRecords::new(self.shared_conn).checkpoint_record_version(&record.id)?
                    == checkpoint.expected_version,
                "checkpoint {} version mismatch before session-end promotion",
                checkpoint.checkpoint_id
            );
        }
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut reserved_proposal_ids = if has_repo_writes {
            proposal_packets.prepare_identity_space()?
        } else {
            BTreeSet::new()
        };
        let mut repo_plans = Vec::with_capacity(document.candidates.len());
        let recovery_artifacts = checkpoint_promotion
            .as_ref()
            .and_then(|checkpoint| checkpoint.recovery_journal.as_ref())
            .map(|journal| journal.artifacts.as_slice());
        let mut repo_candidate_index = 0usize;

        for candidate in &document.candidates {
            if candidate.destination == MemoryDestination::Repo {
                let recovery_artifact =
                    recovery_artifacts.and_then(|artifacts| artifacts.get(repo_candidate_index));
                let proposal_id = match recovery_artifact {
                    Some(artifact) => artifact.proposal_id.clone(),
                    None => {
                        let base_slug = proposals::title_to_concept_slug(&candidate.title)
                            .unwrap_or_else(|| "memory".to_owned());
                        let base_id = format!("mem_session_{base_slug}");
                        okf::reserve_okf_proposal_id(
                            &pending_root,
                            &base_id,
                            &mut reserved_proposal_ids,
                        )?
                    }
                };
                let command_origin = checkpoint_promotion
                    .as_ref()
                    .map(|checkpoint| checkpoint.identity.descriptor());
                let draft = session_end_proposal_draft(
                    candidate,
                    actor,
                    &timestamp,
                    proposal_id,
                    command_origin,
                )?;
                let plan = match recovery_artifact {
                    Some(artifact) => {
                        let path = self.paths.project_root.join(&artifact.relative_path);
                        let markdown = std::fs::read_to_string(&path).with_context(|| {
                            format!(
                                "failed to read recovered session-end proposal {}",
                                path.display()
                            )
                        })?;
                        anyhow::ensure!(
                            blake3::hash(markdown.as_bytes()).to_hex().as_str() == artifact.digest,
                            "session-end recovery proposal bytes changed"
                        );
                        let parsed =
                            okf::parse_okf_proposal_markdown(&pending_root, &path, &markdown)?
                                .context("recovered session-end proposal was ignored")?;
                        anyhow::ensure!(
                            parsed.id == artifact.proposal_id
                                && parsed.origin == draft.origin
                                && parsed.retention == draft.retention,
                            "recovered session-end proposal metadata changed"
                        );
                        okf::OkfCreateProposalPlan {
                            proposal_id: artifact.proposal_id.clone(),
                            path,
                            markdown,
                            parsed,
                        }
                    }
                    None => okf::plan_okf_create_proposal(&pending_root, &draft)?,
                };
                repo_plans.push(Some(plan));
                repo_candidate_index += 1;
            } else {
                repo_plans.push(None);
            }
        }
        if let Some(artifacts) = recovery_artifacts {
            anyhow::ensure!(
                repo_candidate_index == artifacts.len(),
                "session-end recovery artifact count changed"
            );
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
        let session_end_origin_journal = match (checkpoint_promotion.as_ref(), has_repo_writes) {
            (Some(checkpoint), true) => match checkpoint.recovery_journal.as_ref() {
                Some(journal) => Some(journal.clone()),
                None => {
                    let artifacts = repo_plans
                        .iter()
                        .filter_map(Option::as_ref)
                        .map(|plan| {
                            Ok(SessionEndOriginArtifact {
                                proposal_id: plan.proposal_id.clone(),
                                relative_path: plan
                                    .path
                                    .strip_prefix(&self.paths.project_root)
                                    .context("session-end proposal is outside the project root")?
                                    .to_path_buf(),
                                digest: blake3::hash(plan.markdown.as_bytes()).to_hex().to_string(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let journal =
                        journal_for(self.paths, checkpoint, actor, &timestamp, artifacts)?;
                    write_journal(self.paths, &journal)?;
                    Some(journal)
                }
            },
            _ => None,
        };
        let recovering_repository_artifacts = checkpoint_promotion
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.recovery_journal.is_some());

        let mut repo_writes = vec![None::<(String, PathBuf)>; document.candidates.len()];
        let mut runtime_writes =
            vec![None::<(String, MemoryDestination)>; document.candidates.len()];
        let mut created_proposal_files = Vec::new();
        let mut checkpoint_origin_outcome = None::<OriginOutcome>;
        let mut checkpoint_promotion_result = None::<SessionEndResult>;
        let write_result = (|| -> Result<()> {
            if has_repo_writes && !recovering_repository_artifacts {
                proposal_packets
                    .ensure_planned_available(repo_plans.iter().filter_map(Option::as_ref))?;
            }
            if recovering_repository_artifacts {
                for (index, plan) in repo_plans.iter().enumerate() {
                    let Some(plan) = plan else {
                        continue;
                    };
                    repo_writes[index] = Some((plan.proposal_id.clone(), plan.path.clone()));
                }
            } else if let Some(authorization) = repo_authorization.as_ref() {
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
                run_after_repository_install_hook();
            }
            let tx = self.shared_conn.unchecked_transaction()?;
            if let Some(checkpoint) = checkpoint_promotion.as_ref() {
                match crate::prepare_origin(
                    &tx,
                    &checkpoint.identity,
                    &checkpoint.fingerprint,
                    &timestamp,
                )? {
                    OriginPreparation::Acquired => {}
                    OriginPreparation::Replay(_) => anyhow::bail!(
                        "checkpoint session-end origin finalized while the repository lifecycle lock was held"
                    ),
                    OriginPreparation::Pending(_) => anyhow::bail!(
                        "origin_operation_pending: checkpoint session-end promotion is already prepared"
                    ),
                }
            }
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
            if let Some(checkpoint) = checkpoint_promotion.as_ref() {
                let mutation = RuntimeRecords::new(&tx).close_checkpoint(
                    actor,
                    &CloseCheckpointCommand {
                        operation_id: checkpoint.operation_id.clone(),
                        checkpoint_id: checkpoint.checkpoint_id.clone(),
                        expected_version: checkpoint.expected_version.clone(),
                    },
                    evaluated_at,
                    &timestamp,
                )?;
                let promotion =
                    successful_session_end_result(&document, &repo_writes, &runtime_writes)?;
                let record_version =
                    RuntimeRecords::new(&tx).checkpoint_record_version(&mutation.record.id)?;
                let summary_event = append_event(
                    &tx,
                    AppendEvent {
                        event_type: "memory.checkpoint_session_ended".to_owned(),
                        actor: actor.to_owned(),
                        data_class: MemoryEventDataClass::Private,
                        payload: json!({
                            "operation_id": &checkpoint.operation_id,
                            "checkpoint_id": &checkpoint.checkpoint_id,
                            "record_version": record_version,
                            "promotion": &promotion,
                        }),
                        record_id: Some(checkpoint.checkpoint_id.clone()),
                        proposal_id: None,
                    },
                )?;
                let outcome_kind = if mutation.applied {
                    OriginOutcomeKind::Created
                } else {
                    OriginOutcomeKind::ExistingDuplicateNoWrite
                };
                let outcome = OriginOutcome::new(
                    checkpoint.identity.clone(),
                    checkpoint.fingerprint.clone(),
                    outcome_kind,
                    &timestamp,
                )
                .with_destination(MemoryDestination::Session)
                .with_record_id(&checkpoint.checkpoint_id)
                .with_lifecycle_event_id(&summary_event.id);
                checkpoint_origin_outcome = Some(crate::finalize_origin(&tx, &outcome)?);
                checkpoint_promotion_result = Some(promotion);
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            if created_proposal_files.is_empty() {
                if !recovering_repository_artifacts
                    && let Some(journal) = session_end_origin_journal.as_ref()
                {
                    remove_journal(self.paths, journal)?;
                }
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
                if let Some(journal) = session_end_origin_journal.as_ref() {
                    remove_journal(self.paths, journal)?;
                }
            } else {
                return Err(error).context(
                    "session-end promotion failed; created repository files have no matching authorization",
                );
            }
            return Err(error);
        }
        if let Some(journal) = session_end_origin_journal.as_ref() {
            remove_journal(self.paths, journal)?;
        }
        if has_runtime_writes {
            shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)
                .context("session-end runtime writes committed but worktree index refresh failed")?;
        }

        let promotion = match checkpoint_promotion_result {
            Some(result) => result,
            None => successful_session_end_result(&document, &repo_writes, &runtime_writes)?,
        };
        let closure = match (
            checkpoint_promotion.as_ref(),
            checkpoint_origin_outcome.as_ref(),
        ) {
            (Some(checkpoint), Some(outcome)) => Some(checkpoint_result_from_origin(
                self.shared_conn,
                &checkpoint.operation_id,
                outcome,
                false,
            )?),
            (None, None) => None,
            _ => anyhow::bail!("checkpoint session-end origin outcome was not committed"),
        };
        Ok(SessionEndRouteResult { promotion, closure })
    }
}

fn successful_session_end_result(
    document: &SessionEndDocument,
    repo_writes: &[Option<(String, PathBuf)>],
    runtime_writes: &[Option<(String, MemoryDestination)>],
) -> Result<SessionEndResult> {
    let mut results = Vec::with_capacity(document.candidates.len());
    for (index, candidate) in document.candidates.iter().enumerate() {
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
                MemoryDestination::Repo | MemoryDestination::Local | MemoryDestination::Session => {
                    None
                }
            },
            write,
        });
    }

    Ok(SessionEndResult {
        task: document.task.trim().to_owned(),
        candidates: results,
    })
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
mod tests;
