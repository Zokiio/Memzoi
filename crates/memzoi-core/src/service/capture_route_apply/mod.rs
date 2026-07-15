use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{
    CaptureAction, CaptureApplyResult, CapturePlan, CaptureReview, CaptureReviewDecisionInput,
    CaptureReviewInput, CaptureReviewOutcome, CaptureSourceInputs, CaptureWrite, MemoryDestination,
    MemoryPaths, OkfProposalSource,
    expiry::{self, Clock},
    okf,
};

use super::{proposal_packets::ProposalPacketLifecycle, safe_files::RepoLifecycleLock};

mod journal;
mod runtime;
#[cfg(test)]
mod tests;

use self::journal::{
    CaptureApplyRecoveryOutcome, append_capture_apply_commit_marker, build_capture_apply_journal,
    install_capture_apply_proposals, recover_capture_apply, stage_capture_apply_proposals,
    write_capture_apply_journal,
};
use self::runtime::{capture_provenance, create_capture_runtime_with_conn};

pub(super) struct CaptureRouteApply<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
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
    pub(super) fn new(paths: &'a MemoryPaths, conn: &'a Connection, clock: &'a dyn Clock) -> Self {
        Self { paths, conn, clock }
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
        crate::capture::validate_capture_plan_live_state(
            self.paths,
            Some(self.conn),
            &plan,
            source_inputs,
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
        if !has_repo_writes && !has_runtime_writes {
            return Ok(CaptureApplyResult {
                schema: crate::CAPTURE_APPLY_RESULT_SCHEMA.to_owned(),
                plan_id: plan.plan_id,
                review_id: review.review_id,
                writes: Vec::new(),
            });
        }

        let _lifecycle_lock = (has_repo_writes || has_runtime_writes)
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        recover_capture_apply(self.paths, self.conn)
            .context("failed to recover an interrupted capture apply")?;

        crate::capture::validate_capture_plan_live_state(
            self.paths,
            Some(self.conn),
            &plan,
            source_inputs,
            None,
        )
        .context("stale capture plan after lifecycle lock")?;

        let timestamp = expiry::format_timestamp(self.clock.now_utc())?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut planned = Vec::new();
        for (decision, candidate) in &selected {
            let CaptureAction::CreateProposal { proposal_id, .. } = &candidate.action else {
                continue;
            };
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
                capture: Some(provenance),
            };
            planned.push((
                candidate.candidate_id.clone(),
                okf::plan_okf_create_proposal(&pending_root, &draft)?,
            ));
        }

        let mut writes = Vec::new();
        let result = (|| -> Result<()> {
            let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
            crate::capture::validate_capture_plan_live_state(
                self.paths,
                Some(&tx),
                &plan,
                source_inputs,
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
            if !planned.is_empty() {
                let proposal_packets = ProposalPacketLifecycle::new(self.paths, &tx);
                proposal_packets.prepare_pending_root()?;
                proposal_packets
                    .ensure_planned_available(planned.iter().map(|(_, proposal)| proposal))?;
            }

            let journal = (!planned.is_empty())
                .then(|| build_capture_apply_journal(&plan, &review, &planned))
                .transpose()?;
            if let Some(journal) = journal.as_ref() {
                write_capture_apply_journal(self.paths, journal)?;
                stage_capture_apply_proposals(self.paths, journal, &planned)?;
                install_capture_apply_proposals(self.paths, journal)?;
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
                let record = create_capture_runtime_with_conn(
                    &tx,
                    actor,
                    candidate,
                    destination,
                    &timestamp,
                    provenance,
                )?;
                writes.push(CaptureWrite::RuntimeRecord {
                    candidate_id: candidate.candidate_id.clone(),
                    record_id: record.id,
                    destination,
                });
            }
            if let Some(journal) = journal.as_ref() {
                append_capture_apply_commit_marker(&tx, journal, actor, &timestamp)?;
            }
            tx.commit()?;
            if journal.is_some() {
                let outcome = recover_capture_apply(self.paths, self.conn)?;
                if outcome != CaptureApplyRecoveryOutcome::Committed {
                    bail!("capture apply committed without a recoverable journal marker");
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            match recover_capture_apply(self.paths, self.conn) {
                Ok(CaptureApplyRecoveryOutcome::Committed) => {}
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

pub(super) fn recover_on_open(paths: &MemoryPaths, conn: &Connection) -> Result<()> {
    if journal::capture_apply_journal_exists(paths)? {
        let _lifecycle_lock = RepoLifecycleLock::acquire(paths)?;
        recover_capture_apply(paths, conn)
            .context("failed to recover an interrupted capture apply")?;
    }
    Ok(())
}
