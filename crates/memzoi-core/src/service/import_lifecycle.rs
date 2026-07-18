use std::collections::BTreeMap;
use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::{
    AuthorizationProof, ImportApplyResult, ImportDocument, ImportPlan, MemoryDestination,
    MemoryPaths, OkfProposalSensitivity, RepositoryContentClass, RepositoryWriteRoute,
    SafetyFieldKind, ScopeKind, Visibility,
    expiry::{self, Clock},
    import::{self, ExistingDuplicate},
    okf,
};

use super::{
    import_origin_journal::{
        ImportOriginJournal, ImportOriginJournalEntry, remove_journal as remove_import_journal,
        write_journal as write_import_journal,
    },
    proposal_packets::{FileProposalInventoryEntry, ProposalPacketLifecycle},
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, CreatedRepositoryFile, OwnedRepositoryProjection,
        RepositoryMutationAuthorization, RepositorySafetyValue,
        authorize_repository_projection_batch, create_authorized_repository_batch,
        explicit_repository_provenance, okf_proposal_safety_values, remove_created_repository_file,
        safety_value,
    },
    runtime_records::{
        CheckpointInput, LocalMemoryInput, RuntimeRecords, reserved_runtime_record_ids,
    },
    safe_files::{RepoLifecycleLock, ensure_safe_directory},
    shared_runtime,
};

pub(super) struct ImportLifecycle<'a> {
    paths: &'a MemoryPaths,
    conn: &'a Connection,
    shared_conn: &'a Connection,
    clock: &'a dyn Clock,
}

impl<'a> ImportLifecycle<'a> {
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

    pub(super) fn plan(&self, actor: &str, mut document: ImportDocument) -> Result<ImportPlan> {
        if actor.trim().is_empty() {
            bail!("import actor cannot be empty");
        }
        import::validate_document(&document)?;
        let mut shared_values = Vec::new();
        for (index, source) in document.sources.iter().enumerate() {
            for (name, value) in [
                ("path", source.path.as_deref()),
                ("url", source.url.as_deref()),
                ("ref", source.reference.as_deref()),
            ] {
                if let Some(value) = value {
                    shared_values.push(safety_value(
                        format!("sources[{index}].{name}"),
                        SafetyFieldKind::SourceReference,
                        value,
                    ));
                }
            }
        }
        for (index, candidate) in document.candidates.iter_mut().enumerate() {
            if candidate.destination != MemoryDestination::Repo
                || candidate.sensitivity != OkfProposalSensitivity::RepoSafe
            {
                continue;
            }
            let scope = candidate.scope.as_ref();
            let mut values = shared_values
                .iter()
                .map(|value: &RepositorySafetyValue| RepositorySafetyValue {
                    location: value.location.clone(),
                    kind: value.kind,
                    value: value.value.clone(),
                })
                .collect::<Vec<_>>();
            values.extend([
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
                safety_value(
                    format!("candidate[{index}].reason"),
                    SafetyFieldKind::Reason,
                    &candidate.reason,
                ),
            ]);
            for (tag_index, tag) in candidate.tags.iter().enumerate() {
                values.push(safety_value(
                    format!("candidate[{index}].tags[{tag_index}]"),
                    SafetyFieldKind::Text,
                    tag,
                ));
            }
            if authorize_repository_projection_batch(
                self.paths,
                RepositoryWriteRoute::ImportApply,
                candidate.sensitivity,
                scope.map_or(ScopeKind::Repo, |scope| scope.kind),
                scope.and_then(|scope| scope.id.as_deref()),
                Visibility::Repo,
                AuthorizationProof::ExplicitCommand {
                    operation: "import_plan",
                },
                explicit_repository_provenance(candidate.content_class, actor),
                &values,
                &[],
            )
            .is_err()
            {
                candidate.sensitivity = OkfProposalSensitivity::Unknown;
            }
        }
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.shared_conn);
        let (inventory, reserved_proposal_ids) = proposal_packets.planning_inventory()?;
        let existing = self.load_duplicates(&inventory.pending)?;
        let initial = import::build_plan(
            actor,
            &document,
            &existing,
            &self.paths.proposals_dir().join("pending"),
            &reserved_proposal_ids,
            &BTreeMap::new(),
        )?;
        let mut replays = BTreeMap::new();
        for candidate in &initial.candidates {
            if !import_origin_is_admissible(candidate) {
                continue;
            }
            let (identity, fingerprint) =
                import_origin_binding(self.paths, &initial.sources, candidate)?;
            match crate::lookup_origin(self.shared_conn, &identity, &fingerprint)? {
                crate::OriginLookup::Replay(outcome) => {
                    replays.insert(candidate.origin_key.clone(), outcome);
                }
                crate::OriginLookup::Prepared(_) => {
                    bail!("origin_operation_pending: import origin is already prepared")
                }
                crate::OriginLookup::Unseen => {}
            }
        }
        if replays.is_empty() {
            Ok(initial)
        } else {
            import::build_plan(
                actor,
                &document,
                &existing,
                &self.paths.proposals_dir().join("pending"),
                &reserved_proposal_ids,
                &replays,
            )
        }
    }

    pub(super) fn apply(
        &self,
        actor: &str,
        document: ImportDocument,
        expected_plan_id: &str,
    ) -> Result<ImportApplyResult> {
        let has_repo_candidates = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let has_runtime_candidates = document.candidates.iter().any(|candidate| {
            matches!(
                candidate.destination,
                MemoryDestination::Local | MemoryDestination::Session
            )
        });
        let _lifecycle_lock = (has_repo_candidates || has_runtime_candidates)
            .then(|| RepoLifecycleLock::acquire(self.paths))
            .transpose()?;
        if has_repo_candidates || has_runtime_candidates {
            shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)?;
        }
        let proposal_packets = ProposalPacketLifecycle::new(self.paths, self.shared_conn);
        if has_repo_candidates {
            proposal_packets.preflight_pending_root()?;
        }
        let plan = self.plan(actor, document.clone())?;
        if plan.plan_id != expected_plan_id {
            bail!(
                "stale import plan: expected {expected_plan_id}, recomputed {}",
                plan.plan_id
            );
        }
        if plan.candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                import::ImportCandidateAction::CreateProposal { .. }
            )
        }) {
            proposal_packets.prepare_pending_root()?;
        }
        let timestamp = expiry::format_timestamp(self.clock.now_utc())?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut planned = Vec::new();
        for candidate in &plan.candidates {
            let import::ImportCandidateAction::CreateProposal { proposal_id, .. } =
                &candidate.action
            else {
                continue;
            };
            let draft =
                import::proposal_draft(candidate, actor, &timestamp, proposal_id, &plan.sources)?;
            planned.push((
                candidate.index,
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
        let mut safety_values = Vec::new();
        for (index, source) in plan.sources.iter().enumerate() {
            for (name, value) in [
                ("path", source.path.as_deref()),
                ("url", source.url.as_deref()),
                ("ref", source.reference.as_deref()),
            ] {
                if let Some(value) = value {
                    safety_values.push(safety_value(
                        format!("sources[{index}].{name}"),
                        SafetyFieldKind::SourceReference,
                        value,
                    ));
                }
            }
        }
        for (candidate_index, proposal) in &planned {
            safety_values.extend(okf_proposal_safety_values(
                &format!("candidate[{candidate_index}]"),
                &proposal.parsed,
            ));
        }
        let repo_authorization = (!planned.is_empty())
            .then(|| {
                authorize_repository_projection_batch(
                    self.paths,
                    RepositoryWriteRoute::ImportApply,
                    OkfProposalSensitivity::RepoSafe,
                    ScopeKind::Repo,
                    None,
                    Visibility::Repo,
                    AuthorizationProof::ImportPlan {
                        plan_id: &plan.plan_id,
                    },
                    explicit_repository_provenance(
                        RepositoryContentClass::GeneralRepoKnowledge,
                        &plan.plan_id,
                    ),
                    &safety_values,
                    &repo_projections,
                )
            })
            .transpose()?;
        let import_origin_journal = if planned.is_empty() {
            None
        } else {
            let entries = planned
                .iter()
                .map(|(candidate_index, proposal)| {
                    let candidate = plan
                        .candidates
                        .iter()
                        .find(|candidate| candidate.index == *candidate_index)
                        .context("import journal candidate disappeared from the plan")?;
                    let (identity, fingerprint) =
                        import_origin_binding(self.paths, &plan.sources, candidate)?;
                    ImportOriginJournalEntry::new(
                        self.paths,
                        *candidate_index,
                        identity,
                        fingerprint,
                        proposal.proposal_id.clone(),
                        &proposal.path,
                        &proposal.markdown,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let journal = ImportOriginJournal::new(self.paths, &plan.plan_id, &timestamp, entries)?;
            Some(journal)
        };
        let reserved_runtime_ids = if has_runtime_candidates {
            reserved_runtime_record_ids(self.paths, self.conn)?
        } else {
            BTreeSet::new()
        };
        let mut writes = Vec::new();
        for candidate in &plan.candidates {
            let import::ImportCandidateAction::Replay {
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
                    writes.push(crate::ImportWrite::ProposalFile {
                        index: candidate.index,
                        proposal_id: proposal_id.clone(),
                        path: std::path::PathBuf::from(".memzoi/proposals/pending")
                            .join(format!("{proposal_id}.md")),
                    });
                }
                (
                    Some(destination @ (MemoryDestination::Local | MemoryDestination::Session)),
                    Some(record_id),
                    _,
                ) => {
                    writes.push(crate::ImportWrite::RuntimeRecord {
                        index: candidate.index,
                        record_id: record_id.clone(),
                        destination,
                    });
                }
                _ => bail!("recorded import origin outcome is missing its durable identifier"),
            }
        }
        if let Some(journal) = import_origin_journal.as_ref() {
            write_import_journal(self.paths, journal)?;
        }
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            let tx = self.shared_conn.unchecked_transaction()?;
            let mut prepared_origins = BTreeMap::new();
            for candidate in &plan.candidates {
                if matches!(
                    candidate.action,
                    import::ImportCandidateAction::Replay { .. }
                ) || !import_origin_is_admissible(candidate)
                {
                    continue;
                }
                let (identity, fingerprint) =
                    import_origin_binding(self.paths, &plan.sources, candidate)?;
                match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
                    crate::OriginPreparation::Acquired => {
                        prepared_origins.insert(candidate.index, (identity, fingerprint));
                    }
                    crate::OriginPreparation::Replay(_) => {
                        bail!(
                            "import origin finalized after planning; retry to receive its recorded outcome"
                        )
                    }
                    crate::OriginPreparation::Pending(_) => {
                        bail!("origin_operation_pending: import origin is already prepared")
                    }
                }
            }
            if !planned.is_empty() {
                proposal_packets
                    .ensure_planned_available(planned.iter().map(|(_, proposal)| proposal))?;
            }
            let created_files = match repo_authorization.as_ref() {
                Some(authorization) => create_authorized_repository_batch(
                    self.paths,
                    RepositoryWriteRoute::ImportApply,
                    authorization,
                    &repo_projections,
                )?,
                None => Vec::new(),
            };
            created.extend(created_files);
            for ((candidate_index, proposal), created_file) in planned.iter().zip(&created) {
                let candidate_plan = plan
                    .candidates
                    .iter()
                    .find(|candidate| candidate.index == *candidate_index)
                    .context("import proposal candidate should remain in the plan")?;
                let display_path = match &candidate_plan.action {
                    import::ImportCandidateAction::CreateProposal { path, .. } => path.clone(),
                    _ => bail!("import proposal candidate action changed before apply"),
                };
                writes.push(crate::ImportWrite::ProposalFile {
                    index: *candidate_index,
                    proposal_id: proposal.proposal_id.clone(),
                    path: display_path,
                });
                debug_assert_eq!(created_file.path(), proposal.path);
            }

            for candidate in &plan.candidates {
                let import::ImportCandidateAction::CreateRuntime { route } = candidate.action
                else {
                    continue;
                };
                let record = match route {
                    crate::MemoryWriteRoute::RuntimeLocal => RuntimeRecords::new(&tx)
                        .create_local_with_metadata_avoiding(
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                            crate::OriginDescriptor::new(
                                &candidate.origin_key,
                                crate::OriginRoute::Import,
                            ),
                            None,
                            &reserved_runtime_ids,
                        )?,
                    crate::MemoryWriteRoute::RuntimeSession => RuntimeRecords::new(&tx)
                        .create_checkpoint_with_metadata_avoiding(
                            actor,
                            &CheckpointInput {
                                task: candidate.title.clone(),
                                note: candidate.body.clone(),
                            },
                            &timestamp,
                            crate::OriginDescriptor::new(
                                &candidate.origin_key,
                                crate::OriginRoute::Import,
                            ),
                            None,
                            &reserved_runtime_ids,
                        )?,
                    _ => bail!("import runtime candidate has invalid route {route}"),
                };
                writes.push(crate::ImportWrite::RuntimeRecord {
                    index: candidate.index,
                    record_id: record.id,
                    destination: record.destination,
                });
            }
            for candidate in &plan.candidates {
                let Some((identity, fingerprint)) = prepared_origins.remove(&candidate.index)
                else {
                    continue;
                };
                let mut outcome = match &candidate.action {
                    import::ImportCandidateAction::CreateProposal { proposal_id, .. } => {
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::Created,
                            &timestamp,
                        )
                        .with_destination(MemoryDestination::Repo)
                        .with_proposal_id(proposal_id)
                    }
                    import::ImportCandidateAction::CreateRuntime { .. } => {
                        let (record_id, destination) = writes
                            .iter()
                            .find_map(|write| match write {
                                crate::ImportWrite::RuntimeRecord {
                                    index,
                                    record_id,
                                    destination,
                                } if *index == candidate.index => {
                                    Some((record_id.as_str(), *destination))
                                }
                                _ => None,
                            })
                            .context("import runtime outcome has no created record")?;
                        crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::Created,
                            &timestamp,
                        )
                        .with_destination(destination)
                        .with_record_id(record_id)
                    }
                    import::ImportCandidateAction::Duplicate { matches } => {
                        let mut outcome = crate::OriginOutcome::new(
                            identity,
                            fingerprint,
                            crate::OriginOutcomeKind::ExistingDuplicateNoWrite,
                            &timestamp,
                        );
                        if let Some(existing) = matches.first() {
                            outcome = match existing.kind {
                                import::ImportDuplicateKind::PendingProposal => {
                                    outcome.with_proposal_id(&existing.id)
                                }
                                import::ImportDuplicateKind::CanonicalRecord
                                | import::ImportDuplicateKind::RuntimeRecord => {
                                    outcome.with_record_id(&existing.id)
                                }
                                import::ImportDuplicateKind::EarlierCandidate => outcome,
                            };
                        }
                        outcome
                    }
                    import::ImportCandidateAction::NoWrite { .. } => crate::OriginOutcome::new(
                        identity,
                        fingerprint,
                        crate::OriginOutcomeKind::RejectedNoWrite,
                        &timestamp,
                    ),
                    import::ImportCandidateAction::Blocked { .. } => crate::OriginOutcome::new(
                        identity,
                        fingerprint,
                        crate::OriginOutcomeKind::NeedsReviewNoWrite,
                        &timestamp,
                    ),
                    import::ImportCandidateAction::Replay { .. } => {
                        bail!("replayed import origin was unexpectedly prepared")
                    }
                };
                if outcome.destination.is_none() {
                    outcome.destination = Some(candidate.classification.destination);
                }
                crate::finalize_origin(&tx, &outcome)?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(authorization) = repo_authorization.as_ref()
                && !created.is_empty()
            {
                if let Err(cleanup) = cleanup_authorized_import_proposals(
                    self.paths,
                    authorization,
                    &repo_projections,
                    &created,
                ) {
                    return Err(error)
                        .context(format!("import apply failed; cleanup failed: {cleanup}"));
                }
            } else if !created.is_empty() {
                return Err(error).context(
                    "import apply failed; created repository files have no matching authorization",
                );
            }
            if let Some(journal) = import_origin_journal.as_ref()
                && let Err(cleanup) = remove_import_journal(self.paths, journal)
            {
                return Err(error).context(format!(
                    "import apply failed; journal cleanup failed: {cleanup}"
                ));
            }
            return Err(error);
        }
        if let Some(journal) = import_origin_journal.as_ref() {
            remove_import_journal(self.paths, journal)
                .context("import writes committed but origin journal cleanup failed")?;
        }
        if has_runtime_candidates {
            shared_runtime::refresh_index_mirrors_locked(self.paths, self.shared_conn, self.conn)
                .context("import runtime writes committed but worktree index refresh failed")?;
        }
        writes.sort_by_key(|write| match write {
            crate::ImportWrite::ProposalFile { index, .. }
            | crate::ImportWrite::RuntimeRecord { index, .. } => *index,
        });
        Ok(ImportApplyResult { plan, writes })
    }

    fn load_duplicates(
        &self,
        pending_proposals: &[FileProposalInventoryEntry],
    ) -> Result<Vec<ExistingDuplicate>> {
        let mut entries = Vec::new();
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        let now = self.clock.now_utc();
        for record in okf::read_okf_record_files(self.paths.records_dir())? {
            let projected = okf::project_okf_record(&record);
            if !crate::evaluate_current_assertion(
                &projected.id,
                projected.status,
                projected.lane,
                &projected.retention,
                now,
                Vec::new(),
            )?
            .is_current
            {
                continue;
            }
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::CanonicalRecord,
                id: record.concept_id,
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&record.draft.body),
            });
        }
        for entry in pending_proposals {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::PendingProposal,
                id: entry.proposal.id.clone(),
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&entry.proposal.body),
            });
        }
        let runtime_records = RuntimeRecords::new(self.shared_conn).records_for_preservation()?;
        let mut runtime = Vec::new();
        for record in runtime_records {
            if crate::evaluate_current_assertion(
                &record.id,
                record.status,
                record.lane,
                &record.retention,
                now,
                Vec::new(),
            )?
            .is_current
            {
                runtime.push(record);
            }
        }
        runtime.sort_by(|a, b| a.id.cmp(&b.id));
        for record in runtime {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::RuntimeRecord,
                id: record.id,
                destination: Some(record.destination),
                hash: import::content_hash(&record.body),
            });
        }
        entries.sort_by(|a, b| (a.kind, a.id.as_str()).cmp(&(b.kind, b.id.as_str())));
        Ok(entries)
    }
}

fn import_origin_is_admissible(candidate: &import::ImportPlanCandidate) -> bool {
    !matches!(
        candidate.sensitivity,
        OkfProposalSensitivity::Unknown
            | OkfProposalSensitivity::Secret
            | OkfProposalSensitivity::RawTranscript
    )
}

fn import_origin_binding(
    paths: &MemoryPaths,
    sources: &[crate::OkfProposalSource],
    candidate: &import::ImportPlanCandidate,
) -> Result<(crate::OriginIdentity, String)> {
    let route = crate::OriginRoute::Import;
    let descriptor = crate::OriginDescriptor::new(&candidate.origin_key, route);
    let identity = crate::OriginIdentity::new(paths.repository_key(), descriptor);
    let fingerprint = crate::origin_input_fingerprint(
        route,
        &serde_json::json!({
            "origin_key": &candidate.origin_key,
            "sources": sources,
            "classification": &candidate.classification,
            "policy": &candidate.policy,
            "type": candidate.memory_type,
            "lane": candidate.lane,
            "title": &candidate.title,
            "body": &candidate.body,
            "sensitivity": candidate.sensitivity,
            "content_class": candidate.content_class,
            "scope": &candidate.scope,
            "tags": &candidate.tags,
        }),
    )?;
    Ok((identity, fingerprint))
}

fn cleanup_authorized_import_proposals(
    paths: &MemoryPaths,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    created: &[CreatedRepositoryFile],
) -> Result<()> {
    if created.len() != projections.len() {
        bail!("created import proposal batch does not match its authorized projections");
    }
    let mutation = RepositoryMutationAuthorization {
        route: RepositoryWriteRoute::ImportApply,
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
        bail!("{}", cleanup_errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_cleanup_preserves_a_concurrent_repository_replacement() -> Result<()> {
        let project = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().to_path_buf(),
            runtime.path().to_path_buf(),
        );
        let destination = paths.proposals_dir().join("pending/mem_import_cleanup.md");
        let authorized_bytes = b"authorized import proposal\n";
        let projections = vec![OwnedRepositoryProjection::from_absolute(
            &paths,
            &destination,
            authorized_bytes,
            None,
        )?];
        let authorization = authorize_repository_projection_batch(
            &paths,
            RepositoryWriteRoute::ImportApply,
            OkfProposalSensitivity::RepoSafe,
            ScopeKind::Repo,
            None,
            Visibility::Repo,
            AuthorizationProof::ImportPlan {
                plan_id: "import-cleanup-test",
            },
            explicit_repository_provenance(
                RepositoryContentClass::GeneralRepoKnowledge,
                "import-cleanup-test",
            ),
            &[],
            &projections,
        )?;
        let created = create_authorized_repository_batch(
            &paths,
            RepositoryWriteRoute::ImportApply,
            &authorization,
            &projections,
        )?;
        let replacement = b"concurrent human replacement\n";
        std::fs::write(&destination, replacement)?;

        let error =
            cleanup_authorized_import_proposals(&paths, &authorization, &projections, &created)
                .expect_err("cleanup must not delete bytes that were not authorized by the import");

        assert!(format!("{error:#}").contains("does not match"));
        assert_eq!(std::fs::read(&destination)?, replacement);
        Ok(())
    }

    #[test]
    fn import_cleanup_preserves_an_identical_recreated_file() -> Result<()> {
        let project = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().to_path_buf(),
            runtime.path().to_path_buf(),
        );
        let destination = paths
            .proposals_dir()
            .join("pending/mem_import_identical_cleanup.md");
        let authorized_bytes = b"authorized import proposal\n";
        let projections = vec![OwnedRepositoryProjection::from_absolute(
            &paths,
            &destination,
            authorized_bytes,
            None,
        )?];
        let authorization = authorize_repository_projection_batch(
            &paths,
            RepositoryWriteRoute::ImportApply,
            OkfProposalSensitivity::RepoSafe,
            ScopeKind::Repo,
            None,
            Visibility::Repo,
            AuthorizationProof::ImportPlan {
                plan_id: "import-identical-cleanup-test",
            },
            explicit_repository_provenance(
                RepositoryContentClass::GeneralRepoKnowledge,
                "import-identical-cleanup-test",
            ),
            &[],
            &projections,
        )?;
        let created = create_authorized_repository_batch(
            &paths,
            RepositoryWriteRoute::ImportApply,
            &authorization,
            &projections,
        )?;

        std::fs::remove_file(&destination)?;
        std::fs::write(&destination, authorized_bytes)?;

        let error =
            cleanup_authorized_import_proposals(&paths, &authorization, &projections, &created)
                .expect_err("cleanup must preserve an identical file recreated by another owner");

        assert!(format!("{error:#}").contains("no longer identifies"));
        assert_eq!(std::fs::read(&destination)?, authorized_bytes);
        Ok(())
    }
}
