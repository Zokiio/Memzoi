use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{
    CaptureAction, CaptureApplyResult, CapturePlan, CaptureReview, CaptureReviewDecisionInput,
    CaptureReviewInput, CaptureReviewOutcome, CaptureSourceInputs, CaptureWrite, MemoryDestination,
    MemoryPaths, OkfProposalSensitivity, OkfProposalSource, OriginDescriptor, OriginRoute,
    RepositoryContentClass, RepositoryWriteRoute, ScopeKind, Visibility,
    expiry::{self, Clock},
    okf,
};

use super::{
    proposal_packets::ProposalPacketLifecycle,
    repository_mutation::{
        OwnedRepositoryProjection, authorize_repository_projection_batch,
        explicit_repository_provenance, okf_proposal_safety_values,
    },
    safe_files::RepoLifecycleLock,
    shared_runtime,
};

mod journal;
mod runtime;
#[cfg(test)]
mod tests;

use self::journal::{
    CaptureApplyRecoveryOutcome, append_capture_apply_commit_marker, build_capture_apply_journal,
    install_capture_apply_proposals, recover_capture_apply, stage_capture_apply_proposals,
    write_capture_apply_journal,
};
use self::runtime::capture_provenance;
use super::runtime_records::{RuntimeRecords, reserved_runtime_record_ids};

pub(super) struct CaptureRouteApply<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    shared_conn: &'a Connection,
    clock: &'a dyn Clock,
}

pub(super) struct CaptureRouteApplyCommand<'a> {
    pub(super) actor: &'a str,
    pub(super) plan: CapturePlan,
    pub(super) review: CaptureReview,
    pub(super) prior_review: Option<&'a CaptureReview>,
    pub(super) source_inputs: &'a CaptureSourceInputs,
    pub(super) expected_plan_id: &'a str,
    pub(super) expected_review_id: &'a str,
}

impl<'a> CaptureRouteApply<'a> {
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

    pub(super) fn apply(
        &self,
        command: CaptureRouteApplyCommand<'_>,
    ) -> Result<CaptureApplyResult> {
        let CaptureRouteApplyCommand {
            actor,
            plan,
            review,
            prior_review,
            source_inputs,
            expected_plan_id,
            expected_review_id,
        } = command;
        let actor = actor.trim();
        let evaluated_at = expiry::format_timestamp(self.clock.now_utc())?;
        crate::capture::validate_capture_actor(actor)?;
        crate::capture::validate_plan_identity(&plan)?;
        crate::capture::validate_review_identity(&review)?;
        if expected_plan_id.trim() != plan.plan_id || expected_review_id.trim() != review.review_id
        {
            bail!("stale capture apply identity");
        }
        if review.plan_id != plan.plan_id {
            bail!("capture review does not match the supplied plan");
        }
        shared_runtime::refresh_index_mirrors(self.paths, self.shared_conn, self.conn)?;
        crate::capture::validate_capture_plan_live_state(
            self.paths,
            Some(self.conn),
            &plan,
            source_inputs,
            &evaluated_at,
            None,
        )
        .context("stale capture plan")?;

        let review_input = CaptureReviewInput {
            schema: crate::CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
            plan_id: plan.plan_id.clone(),
            prior_review_id: review.prior_review_id.clone(),
            decisions: review
                .decisions
                .iter()
                .map(|decision| CaptureReviewDecisionInput {
                    candidate_id: decision.candidate_id.clone(),
                    outcome: decision.outcome,
                    reason_code: decision.reason_code.clone(),
                    memory: (decision.outcome == CaptureReviewOutcome::Edit)
                        .then(|| {
                            decision
                                .reviewed_candidate
                                .as_ref()
                                .map(|c| c.memory.clone())
                        })
                        .flatten(),
                    requested_destination: (decision.outcome == CaptureReviewOutcome::Edit)
                        .then(|| {
                            decision
                                .reviewed_candidate
                                .as_ref()
                                .map(|c| c.classification.destination)
                        })
                        .flatten(),
                    content_class: (decision.outcome == CaptureReviewOutcome::Edit)
                        .then(|| {
                            decision
                                .reviewed_candidate
                                .as_ref()
                                .map(|c| c.classification.content_class)
                        })
                        .flatten(),
                })
                .collect(),
        };
        let rebuilt_review = crate::capture::build_capture_review_with_connection_and_inputs(
            self.paths,
            self.conn,
            &plan,
            review_input.clone(),
            prior_review,
            source_inputs,
            &review.reviewed_by,
            &review.reviewed_at,
        )?;
        if rebuilt_review.review_id != review.review_id || rebuilt_review != review {
            bail!("stale or modified capture review");
        }

        let selected = review
            .decisions
            .iter()
            .filter_map(|decision| {
                decision
                    .reviewed_candidate
                    .as_ref()
                    .map(|candidate| (decision, candidate))
            })
            .collect::<Vec<_>>();
        let has_repo_writes = selected
            .iter()
            .any(|(_, candidate)| matches!(candidate.action, CaptureAction::CreateProposal { .. }));
        let has_runtime_writes = selected
            .iter()
            .any(|(_, candidate)| matches!(candidate.action, CaptureAction::CreateRuntime { .. }));
        let has_origin_writes = plan.candidates.iter().any(|candidate| {
            !matches!(candidate.action, CaptureAction::Replay { .. })
                && crate::capture::capture_origin_is_admissible(candidate)
        });

        let _lifecycle_lock = (has_repo_writes || has_runtime_writes || has_origin_writes)
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)?;
        recover_capture_apply(self.paths, self.conn)
            .context("failed to recover an interrupted capture apply")?;

        crate::capture::validate_capture_plan_live_state(
            self.paths,
            Some(self.conn),
            &plan,
            source_inputs,
            &evaluated_at,
            None,
        )
        .context("stale capture plan after lifecycle lock")?;

        let reserved_runtime_ids = if has_runtime_writes {
            reserved_runtime_record_ids(self.paths, self.conn)?
        } else {
            Default::default()
        };

        let timestamp = evaluated_at.clone();
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut planned = Vec::new();
        for (decision, candidate) in &selected {
            let CaptureAction::CreateProposal { proposal_id, .. } = &candidate.action else {
                continue;
            };
            validate_capture_proposal_policy(
                &candidate.memory.scope,
                candidate.classification.destination,
                candidate.classification.sensitivity,
                candidate.classification.content_class,
            )?;
            let provenance = capture_provenance(&plan, &review, decision, candidate, actor);
            let draft = okf::OkfCreateProposalDraft {
                proposal_id: proposal_id.clone(),
                memory_type: candidate.memory.memory_type,
                lane: candidate.memory.lane,
                title: candidate.memory.title.clone(),
                body: candidate.memory.body.clone(),
                actor: actor.to_owned(),
                timestamp: timestamp.clone(),
                reason: Some(candidate.classification.destination_reason.clone()),
                scope_kind: candidate.memory.scope.kind,
                scope_id: candidate.memory.scope.id.clone(),
                applies_to: candidate.memory.scope.paths.clone(),
                tags: candidate.memory.tags.clone(),
                sources: candidate
                    .evidence
                    .iter()
                    .map(|evidence| {
                        let path = evidence.locator.project_path().map(str::to_owned);
                        OkfProposalSource {
                            reference: (path.is_none() || evidence.semantic_location.is_some())
                                .then(|| evidence.durable_reference()),
                            path,
                            url: None,
                        }
                    })
                    .collect(),
                sensitivity: candidate.classification.sensitivity,
                content_class: candidate.classification.content_class,
                capture: Some(provenance),
                retention: crate::retention_facts_for_creation(
                    candidate.memory.lane,
                    &timestamp,
                    None,
                    None,
                )?,
                origin: OriginDescriptor::new(
                    format!("capture:{}", candidate.claim_id),
                    OriginRoute::Capture,
                ),
                lineage: None,
            };
            planned.push((
                candidate.candidate_id.clone(),
                okf::plan_okf_create_proposal(&pending_root, &draft)?,
            ));
        }

        let repo_projections = planned
            .iter()
            .map(|(_, proposal)| {
                OwnedRepositoryProjection::from_absolute(
                    self.paths,
                    &proposal.path,
                    proposal.markdown.as_bytes(),
                    None,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut repo_safety_values = Vec::new();
        for (candidate_id, proposal) in &planned {
            repo_safety_values.extend(okf_proposal_safety_values(
                &format!("candidate[{candidate_id}]"),
                &proposal.parsed,
            ));
        }
        let repo_authorization = (!planned.is_empty())
            .then(|| {
                authorize_repository_projection_batch(
                    self.paths,
                    RepositoryWriteRoute::CaptureApply,
                    OkfProposalSensitivity::RepoSafe,
                    ScopeKind::Repo,
                    None,
                    Visibility::Repo,
                    crate::AuthorizationProof::CaptureReview {
                        plan_id: &plan.plan_id,
                        review_id: &review.review_id,
                    },
                    explicit_repository_provenance(
                        RepositoryContentClass::GeneralRepoKnowledge,
                        &review.review_id,
                    ),
                    &repo_safety_values,
                    &repo_projections,
                )
            })
            .transpose()?;

        let mut writes = Vec::new();
        for candidate in &plan.candidates {
            let CaptureAction::Replay {
                outcome,
                destination,
                record_id,
                proposal_id,
            } = &candidate.action
            else {
                continue;
            };
            if *outcome != crate::OriginOutcomeKind::Created {
                continue;
            }
            match (*destination, record_id, proposal_id) {
                (Some(MemoryDestination::Repo), _, Some(proposal_id)) => {
                    writes.push(CaptureWrite::ProposalFile {
                        candidate_id: candidate.candidate_id.clone(),
                        proposal_id: proposal_id.clone(),
                        path: format!(".memzoi/proposals/pending/{proposal_id}.md"),
                    });
                }
                (
                    Some(destination @ (MemoryDestination::Local | MemoryDestination::Session)),
                    Some(record_id),
                    _,
                ) => {
                    writes.push(CaptureWrite::RuntimeRecord {
                        candidate_id: candidate.candidate_id.clone(),
                        record_id: record_id.clone(),
                        destination,
                    });
                }
                _ => bail!("recorded capture origin outcome is missing its durable identifier"),
            }
        }
        let mut runtime_record_ids = Vec::new();
        let result = (|| -> Result<()> {
            let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
            crate::capture::validate_capture_plan_live_state(
                self.paths,
                Some(&tx),
                &plan,
                source_inputs,
                &evaluated_at,
                None,
            )
            .context("stale capture plan at write boundary")?;
            let transactional_review =
                crate::capture::build_capture_review_with_connection_and_inputs(
                    self.paths,
                    &tx,
                    &plan,
                    review_input.clone(),
                    prior_review,
                    source_inputs,
                    &review.reviewed_by,
                    &review.reviewed_at,
                )?;
            if transactional_review.review_id != review.review_id || transactional_review != review
            {
                bail!("stale capture review at write boundary");
            }
            let mut prepared_origins = BTreeMap::new();
            for candidate in &plan.candidates {
                if matches!(candidate.action, CaptureAction::Replay { .. })
                    || !crate::capture::capture_origin_is_admissible(candidate)
                {
                    continue;
                }
                let decision = review
                    .decisions
                    .iter()
                    .find(|decision| decision.candidate_id == candidate.candidate_id)
                    .context("capture review omitted candidate at origin boundary")?;
                let effective = decision.reviewed_candidate.as_ref().unwrap_or(candidate);
                let (identity, fingerprint) =
                    crate::capture::capture_origin_binding(self.paths, effective)?;
                match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
                    crate::OriginPreparation::Acquired => {
                        prepared_origins
                            .insert(candidate.candidate_id.clone(), (identity, fingerprint));
                    }
                    crate::OriginPreparation::Replay(_) => {
                        bail!(
                            "capture origin finalized after planning; retry to receive its recorded outcome"
                        )
                    }
                    crate::OriginPreparation::Pending(_) => {
                        bail!("origin_operation_pending: capture origin is already prepared")
                    }
                }
            }
            if !planned.is_empty() {
                let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.shared_conn);
                proposal_packets.prepare_pending_root()?;
                proposal_packets
                    .ensure_planned_available(planned.iter().map(|(_, proposal)| proposal))?;
            }

            let journal = repo_authorization
                .as_ref()
                .map(|authorization| {
                    build_capture_apply_journal(self.paths, &plan, &review, &planned, authorization)
                })
                .transpose()?;
            if let Some(journal) = journal.as_ref() {
                write_capture_apply_journal(self.paths, journal)?;
                stage_capture_apply_proposals(
                    self.paths,
                    journal,
                    &planned,
                    repo_authorization
                        .as_ref()
                        .context("capture repository authorization disappeared")?,
                    &repo_projections,
                )?;
                install_capture_apply_proposals(
                    self.paths,
                    journal,
                    repo_authorization
                        .as_ref()
                        .context("capture repository authorization disappeared")?,
                    &repo_projections,
                )?;
            }
            for (candidate_id, proposal) in &planned {
                writes.push(CaptureWrite::ProposalFile {
                    candidate_id: candidate_id.clone(),
                    proposal_id: proposal.proposal_id.clone(),
                    path: format!(".memzoi/proposals/pending/{}.md", proposal.proposal_id),
                });
            }
            for (decision, candidate) in &selected {
                let CaptureAction::CreateRuntime { route } = candidate.action else {
                    continue;
                };
                let destination = match route {
                    crate::MemoryWriteRoute::RuntimeLocal => MemoryDestination::Local,
                    crate::MemoryWriteRoute::RuntimeSession => MemoryDestination::Session,
                    _ => bail!("capture runtime candidate has an invalid route"),
                };
                let provenance = capture_provenance(&plan, &review, decision, candidate, actor);
                let record = RuntimeRecords::new(&tx).create_capture(
                    actor,
                    candidate,
                    destination,
                    &timestamp,
                    provenance,
                    &reserved_runtime_ids,
                )?;
                writes.push(CaptureWrite::RuntimeRecord {
                    candidate_id: candidate.candidate_id.clone(),
                    record_id: record.id.clone(),
                    destination,
                });
                runtime_record_ids.push(record.id);
            }
            for candidate in &plan.candidates {
                let Some((identity, fingerprint)) =
                    prepared_origins.remove(&candidate.candidate_id)
                else {
                    continue;
                };
                let decision = review
                    .decisions
                    .iter()
                    .find(|decision| decision.candidate_id == candidate.candidate_id)
                    .context("capture review omitted candidate at outcome boundary")?;
                let effective = decision.reviewed_candidate.as_ref().unwrap_or(candidate);
                let mut outcome = match &effective.action {
                    CaptureAction::CreateProposal { proposal_id, .. }
                        if decision.reviewed_candidate.is_some() =>
                    {
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::Created,
                            &timestamp,
                        )
                        .with_destination(MemoryDestination::Repo)
                        .with_proposal_id(proposal_id)
                    }
                    CaptureAction::CreateRuntime { .. }
                        if decision.reviewed_candidate.is_some() =>
                    {
                        let (record_id, destination) = writes
                            .iter()
                            .find_map(|write| match write {
                                CaptureWrite::RuntimeRecord {
                                    candidate_id,
                                    record_id,
                                    destination,
                                } if candidate_id == &effective.candidate_id => {
                                    Some((record_id.as_str(), *destination))
                                }
                                _ => None,
                            })
                            .context("capture runtime outcome has no created record")?;
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::Created,
                            &timestamp,
                        )
                        .with_destination(destination)
                        .with_record_id(record_id)
                    }
                    CaptureAction::Duplicate { matches } => {
                        let mut outcome = crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::ExistingDuplicateNoWrite,
                            &timestamp,
                        );
                        if let Some(existing) = matches.first() {
                            outcome = match existing.kind {
                                crate::CaptureMatchKind::PendingProposal => {
                                    outcome.with_proposal_id(&existing.id)
                                }
                                crate::CaptureMatchKind::CanonicalRecord
                                | crate::CaptureMatchKind::RuntimeRecord => {
                                    outcome.with_record_id(&existing.id)
                                }
                                crate::CaptureMatchKind::EarlierCandidate => outcome,
                            };
                        }
                        outcome
                    }
                    CaptureAction::Conflict { .. } => crate::OriginOutcome::new(
                        identity,
                        fingerprint,
                        crate::OriginOutcomeKind::ConflictNoWrite,
                        &timestamp,
                    ),
                    CaptureAction::NoWrite { .. } | CaptureAction::Blocked { .. } => {
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::NeedsReviewNoWrite,
                            &timestamp,
                        )
                    }
                    CaptureAction::CreateProposal { .. } | CaptureAction::CreateRuntime { .. } => {
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::RejectedNoWrite,
                            &timestamp,
                        )
                    }
                    CaptureAction::Replay { .. } => {
                        bail!("replayed capture origin was unexpectedly prepared")
                    }
                };
                if outcome.destination.is_none() {
                    outcome.destination = Some(effective.classification.destination);
                }
                crate::finalize_origin(&tx, &outcome)?;
            }
            if let Some(journal) = journal.as_ref() {
                append_capture_apply_commit_marker(&tx, journal, actor, &timestamp)?;
            }
            if has_runtime_writes || has_origin_writes {
                shared_runtime::prepare_runtime_sync_journal(self.paths, &tx, &runtime_record_ids)?;
            }
            tx.commit()?;
            if journal.is_some() {
                let outcome = recover_capture_apply(self.paths, self.conn)?;
                if outcome != CaptureApplyRecoveryOutcome::Committed {
                    bail!("capture apply committed without a recoverable journal marker");
                }
            }
            if has_runtime_writes || has_origin_writes {
                shared_runtime::complete_pending_shared_sync_locked(self.paths, self.shared_conn)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            match recover_capture_apply(self.paths, self.conn) {
                Ok(CaptureApplyRecoveryOutcome::Committed) => {
                    if (has_runtime_writes || has_origin_writes)
                        && let Err(shared_error) =
                            shared_runtime::complete_pending_shared_sync_locked(
                                self.paths,
                                self.shared_conn,
                            )
                    {
                        return Err(error).context(format!(
                            "capture apply shared-sync recovery also failed: {shared_error:#}"
                        ));
                    }
                }
                Ok(CaptureApplyRecoveryOutcome::NoJournal)
                | Ok(CaptureApplyRecoveryOutcome::RolledBack) => return Err(error),
                Err(recovery_error) => {
                    return Err(error).context(format!(
                        "capture apply recovery also failed: {recovery_error:#}"
                    ));
                }
            }
        }

        Ok(CaptureApplyResult {
            schema: crate::CAPTURE_APPLY_RESULT_SCHEMA.to_owned(),
            plan_id: plan.plan_id,
            review_id: review.review_id,
            writes,
        })
    }
}

fn validate_capture_proposal_policy(
    scope: &crate::CaptureMemoryScope,
    destination: MemoryDestination,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
) -> Result<()> {
    if destination != MemoryDestination::Repo {
        bail!("capture repository proposals require the repo destination");
    }
    validate_capture_proposal_projection_values(
        scope.kind,
        scope.id.as_deref(),
        sensitivity,
        content_class,
    )
}

fn validate_capture_proposal_projection_values(
    kind: ScopeKind,
    id: Option<&str>,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
) -> Result<()> {
    if kind != ScopeKind::Repo || id.is_some() {
        bail!("capture repository proposals require repo scope without a scope id");
    }
    if sensitivity != OkfProposalSensitivity::RepoSafe
        || content_class != RepositoryContentClass::GeneralRepoKnowledge
    {
        bail!(
            "capture repository proposals require repo-safe sensitivity and general_repo_knowledge content"
        );
    }
    Ok(())
}

pub(super) fn recover_on_open(paths: &MemoryPaths, conn: &Connection) -> Result<()> {
    if journal::capture_apply_journal_exists(paths)?
        || journal::legacy_capture_apply_journal_exists(paths)?
    {
        let _lifecycle_lock = RepoLifecycleLock::acquire(paths)?;
        recover_capture_apply(paths, conn)
            .context("failed to recover an interrupted capture apply")?;
    }
    Ok(())
}
