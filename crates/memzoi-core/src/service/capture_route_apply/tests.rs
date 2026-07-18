use std::fs;

use anyhow::Context;
use tempfile::TempDir;
use uuid::Uuid;

use super::super::{
    InitRequest, MemoryService,
    proposal_packets::ProposalPacketLifecycle,
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection, RepositorySafetyValue,
        authorize_repository_projection_batch, explicit_repository_provenance,
        okf_proposal_safety_values, repository_transaction_path, stage_authorized_file,
    },
    runtime_records::RuntimeRecords,
};
use super::journal::*;
use crate::{
    AuthorizationProof, CAPTURE_REQUEST_SCHEMA, CAPTURE_REVIEW_INPUT_SCHEMA,
    CaptureExtractorRequest, CaptureMemoryScope, CaptureRequest, CaptureReviewDecisionInput,
    CaptureReviewInput, CaptureReviewOutcome, CaptureSourceLocator, CaptureSourceRequest,
    MARKDOWN_EXTRACTOR_PROFILE, MemoryDestination, MemoryLane, MemoryPaths, MemoryType,
    OkfProposalSensitivity, OriginDescriptor, OriginRoute, RepositoryContentClass,
    RepositoryWriteRoute, ScopeKind, Visibility, build_capture_review, okf, plan_capture,
};

#[test]
fn capture_apply_rejects_non_repo_proposal_scope() {
    let non_repo = CaptureMemoryScope {
        kind: ScopeKind::Team,
        id: Some("platform".to_owned()),
        paths: vec!["crates/memzoi-core/**".to_owned()],
    };
    let repo_with_id = CaptureMemoryScope {
        kind: ScopeKind::Repo,
        id: Some("unexpected".to_owned()),
        paths: Vec::new(),
    };
    let repo = CaptureMemoryScope {
        kind: ScopeKind::Repo,
        id: None,
        paths: Vec::new(),
    };

    for scope in [&non_repo, &repo_with_id] {
        assert!(
            super::validate_capture_proposal_policy(
                scope,
                crate::MemoryDestination::Repo,
                OkfProposalSensitivity::RepoSafe,
                RepositoryContentClass::GeneralRepoKnowledge,
            )
            .is_err()
        );
    }
    assert!(
        super::validate_capture_proposal_policy(
            &repo,
            crate::MemoryDestination::Repo,
            OkfProposalSensitivity::RepoSafe,
            RepositoryContentClass::GeneralRepoKnowledge,
        )
        .is_ok()
    );
    assert!(
        super::validate_capture_proposal_policy(
            &repo,
            crate::MemoryDestination::Repo,
            OkfProposalSensitivity::LocalOnly,
            RepositoryContentClass::LocalOnlyState,
        )
        .is_err()
    );
}

#[test]
fn committed_capture_fallback_completes_shared_sync_before_success() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    fs::write(
        paths.project_root.join("private-capture.md"),
        "# Fact: Recovered repository capture\n\nKeep the durable recovery rule in a reviewable proposal.\n\n# Preference: Recovered private capture\n\nKeep the recovery token in local runtime.\n",
    )?;
    let request = CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: "source-1".to_owned(),
            locator: CaptureSourceLocator::ProjectPath {
                path: "private-capture.md".to_owned(),
            },
            media_type: "text/markdown".to_owned(),
            git: None,
        }],
        extractor: CaptureExtractorRequest {
            profile: MARKDOWN_EXTRACTOR_PROFILE.to_owned(),
        },
    };
    let plan = plan_capture(&paths, request)?;
    assert_eq!(plan.candidates.len(), 2);
    assert!(plan.candidates.iter().any(|candidate| {
        candidate.memory.title == "Recovered repository capture"
            && candidate.classification.destination == MemoryDestination::NeedsReview
    }));
    assert!(
        plan.candidates
            .iter()
            .any(|candidate| { candidate.classification.destination == MemoryDestination::Local })
    );
    let review = build_capture_review(
        &paths,
        &plan,
        CaptureReviewInput {
            schema: CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
            plan_id: plan.plan_id.clone(),
            prior_review_id: None,
            decisions: plan
                .candidates
                .iter()
                .map(|candidate| {
                    let needs_repo_review =
                        candidate.classification.destination == MemoryDestination::NeedsReview;
                    CaptureReviewDecisionInput {
                        candidate_id: candidate.candidate_id.clone(),
                        outcome: if needs_repo_review {
                            CaptureReviewOutcome::Edit
                        } else {
                            CaptureReviewOutcome::Accept
                        },
                        reason_code: needs_repo_review
                            .then_some("explicit-contextual-classification".to_owned()),
                        memory: needs_repo_review.then_some(candidate.memory.clone()),
                        requested_destination: needs_repo_review.then_some(MemoryDestination::Repo),
                        content_class: needs_repo_review
                            .then_some(RepositoryContentClass::GeneralRepoKnowledge),
                    }
                })
                .collect(),
        },
        "capture-reviewer",
        "2026-07-16T12:00:00Z",
    )?;
    inject_before_committed_capture_recovery_hook(|| {
        anyhow::bail!("injected first committed capture recovery interruption")
    });

    service.apply_capture(
        "capture-reviewer",
        plan.clone(),
        review.clone(),
        &plan.plan_id,
        &review.review_id,
    )?;
    let record_id: String = service.conn.query_row(
        "SELECT id FROM memory_record WHERE destination = 'local'",
        [],
        |row| row.get(0),
    )?;
    let shared_record_count: i64 = service.shared_conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE id = ?1",
        [&record_id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_record_count, 1);
    assert!(
        !paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    drop(service);

    let recovered = MemoryService::open_paths(paths.clone())?;
    let recovered_record = recovered
        .list_local_memory()?
        .into_iter()
        .find(|record| record.id == record_id)
        .context("recovered capture runtime record is missing")?;
    assert!(recovered_record.capture.is_some());
    let shared_event_count: i64 = recovered.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE record_id = ?1 AND event_type = 'memory.capture_routed'",
        [&record_id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_event_count, 1);
    assert!(
        !paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    drop(recovered);

    let reopened = MemoryService::open_paths(paths)?;
    let shared_event_count: i64 = reopened.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE record_id = ?1 AND event_type = 'memory.capture_routed'",
        [&record_id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_event_count, 1);
    Ok(())
}

#[test]
fn capture_runtime_id_allocation_skips_an_unindexed_canonical_file_id() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal = service.propose_memory(
        "agent:unindexed-capture-collision-test",
        crate::MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: "Local unindexed capture collision".to_owned(),
            body: "This canonical file owns the first capture runtime identifier.".to_owned(),
            tags: vec!["capture".to_owned()],
            source_kind: Some("test".to_owned()),
            source_ref: Some("unindexed-capture-collision".to_owned()),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 1.0,
        },
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    let canonical = service.apply_proposal(&proposal.id, "agent:applier")?;
    assert_eq!(canonical.id, "local-unindexed-capture-collision");
    assert_eq!(
        service
            .conn
            .execute("DELETE FROM memory_record WHERE id = ?1", [&canonical.id])?,
        1
    );
    assert!(
        paths
            .records_dir()
            .join(format!("{}.md", canonical.id))
            .is_file()
    );
    drop(service);

    fs::write(
        paths.project_root.join("private-collision.md"),
        "# Preference: Unindexed capture collision\n\nKeep the unindexed capture collision token in local runtime.\n",
    )?;
    let request = CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: "source-1".to_owned(),
            locator: CaptureSourceLocator::ProjectPath {
                path: "private-collision.md".to_owned(),
            },
            media_type: "text/markdown".to_owned(),
            git: None,
        }],
        extractor: CaptureExtractorRequest {
            profile: MARKDOWN_EXTRACTOR_PROFILE.to_owned(),
        },
    };
    let plan = plan_capture(&paths, request)?;
    assert_eq!(plan.candidates.len(), 1);
    assert!(matches!(
        plan.candidates[0].action,
        crate::CaptureAction::CreateRuntime { .. }
    ));
    let review = build_capture_review(
        &paths,
        &plan,
        CaptureReviewInput {
            schema: CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
            plan_id: plan.plan_id.clone(),
            prior_review_id: None,
            decisions: vec![CaptureReviewDecisionInput {
                candidate_id: plan.candidates[0].candidate_id.clone(),
                outcome: CaptureReviewOutcome::Accept,
                reason_code: None,
                memory: None,
                requested_destination: None,
                content_class: None,
            }],
        },
        "capture-reviewer",
        "2026-07-16T12:00:00Z",
    )?;

    let service = MemoryService::open_paths(paths.clone())?;
    let result = service.apply_capture(
        "capture-reviewer",
        plan.clone(),
        review.clone(),
        &plan.plan_id,
        &review.review_id,
    )?;
    assert_eq!(result.writes.len(), 1);
    match &result.writes[0] {
        crate::CaptureWrite::RuntimeRecord { record_id, .. } => {
            assert_eq!(record_id, "local-unindexed-capture-collision-2")
        }
        write => panic!("expected a capture runtime write, got {write:?}"),
    }
    assert!(
        RuntimeRecords::new(&service.shared_conn)
            .get(&canonical.id)?
            .is_none()
    );
    Ok(())
}

#[test]
fn final_install_rejects_substituted_staged_bytes() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe staged proposal\n";
    let (journal, authorization, projections) =
        authorized_test_capture_apply(&paths, "mem_capture_swap", contents, &[])?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        std::str::from_utf8(contents)?,
        &journal.journal_id,
    )?;
    assert_eq!(staged, capture_apply_stage_path(&paths, &journal, entry));

    let authorized_stage = staged.with_extension("authorized");
    let error = install_capture_apply_proposals_with_hook(
        &paths,
        &journal,
        &authorization,
        &projections,
        |_, staged| {
            fs::rename(staged, &authorized_stage)?;
            fs::write(staged, b"substituted bytes\n")?;
            Ok(())
        },
    )
    .expect_err("final install must reject staged bytes substituted after authorization");

    assert!(
        format!("{error:#}").contains("authorized projection"),
        "{error:#}"
    );
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn uncommitted_recovery_preserves_an_exact_external_install_collision() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe external collision\n";
    let (journal, authorization, projections) =
        authorized_test_capture_apply(&paths, "mem_capture_external_collision", contents, &[])?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        std::str::from_utf8(contents)?,
        &journal.journal_id,
    )?;

    let install_error = install_capture_apply_proposals_with_hook(
        &paths,
        &journal,
        &authorization,
        &projections,
        |_, _| {
            fs::write(&destination, contents)?;
            Ok(())
        },
    )
    .expect_err("an exact external destination must win the exclusive-install race");
    assert!(format!("{install_error:#}").contains("without replacement"));

    let recovery_error = recover_capture_apply(&paths, &service.conn)
        .expect_err("recovery must not infer ownership from matching path and bytes");

    assert!(format!("{recovery_error:#}").contains("ownership"));
    assert_eq!(fs::read(&destination)?, contents);
    assert_eq!(fs::read(&staged)?, contents);
    assert!(capture_apply_journal_path(&paths).is_file());
    Ok(())
}

#[test]
fn uncommitted_recovery_preserves_a_crash_after_install_before_ownership() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe interrupted install\n";
    let (journal, authorization, projections) =
        authorized_test_capture_apply(&paths, "mem_capture_before_ownership", contents, &[])?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        std::str::from_utf8(contents)?,
        &journal.journal_id,
    )?;

    let install_error = install_capture_apply_proposals_with_hooks(
        &paths,
        &journal,
        &authorization,
        &projections,
        |_, _| Ok(()),
        |_, _| anyhow::bail!("injected crash before ownership persistence"),
    )
    .expect_err("the injected crash must interrupt ownership persistence");
    assert!(format!("{install_error:#}").contains("injected crash"));

    let recovery_error = recover_capture_apply(&paths, &service.conn)
        .expect_err("identity-less installed state must remain untouched for human review");

    assert!(format!("{recovery_error:#}").contains("ownership proof"));
    assert_eq!(fs::read(&destination)?, contents);
    assert_eq!(fs::read(&staged)?, contents);
    assert!(capture_apply_journal_path(&paths).is_file());
    assert!(!capture_apply_ownership_path(&paths).exists());
    Ok(())
}

#[test]
fn committed_recovery_rejects_substituted_staged_bytes() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal_id = "mem_capture_recovery_swap";
    let proposal = okf::plan_okf_create_proposal(
        &paths.proposals_dir().join("pending"),
        &okf::OkfCreateProposalDraft {
            proposal_id: proposal_id.to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            title: "Capture recovery authorization".to_owned(),
            body: "Committed recovery must install only the reviewed projection.".to_owned(),
            actor: "agent:recovery-test".to_owned(),
            timestamp: "2026-07-10T12:00:00Z".to_owned(),
            reason: Some("Exercise committed recovery".to_owned()),
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            applies_to: vec!["crates/memzoi-core/**".to_owned()],
            tags: vec!["capture".to_owned()],
            sources: Vec::new(),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            capture: None,
            retention: crate::retention_facts_for_creation(
                MemoryLane::Semantic,
                "2026-07-10T12:00:00Z",
                None,
                None,
            )?,
            origin: OriginDescriptor::new(
                format!("test:{proposal_id}"),
                OriginRoute::RepositoryProposal,
            ),
            lineage: None,
        },
    )?;
    let safety_values = okf_proposal_safety_values("candidate[candidate_test]", &proposal.parsed);
    let (journal, authorization, projections) = authorized_test_capture_apply(
        &paths,
        proposal_id,
        proposal.markdown.as_bytes(),
        &safety_values,
    )?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        &proposal.markdown,
        &journal.journal_id,
    )?;
    append_capture_apply_commit_marker(
        &service.conn,
        &journal,
        "agent:recovery-test",
        "2026-07-10T12:00:00Z",
    )?;

    let authorized_stage = staged.with_extension("authorized");
    let mut substitute = proposal.markdown.into_bytes();
    substitute[0] = if substitute[0] == b'X' { b'Y' } else { b'X' };
    let error = recover_capture_apply_with_hook(&paths, &service.conn, |_, staged| {
        fs::rename(staged, &authorized_stage)?;
        fs::write(staged, &substitute)?;
        Ok(())
    })
    .expect_err("committed recovery must reject substituted staged bytes");

    assert!(
        format!("{error:#}").contains("authorized projection"),
        "{error:#}"
    );
    assert!(!destination.exists());
    assert!(capture_apply_journal_path(&paths).is_file());
    Ok(())
}

#[test]
fn committed_recovery_rejects_a_substituted_exact_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal_id = "mem_capture_journal_substitution";
    let proposal = okf::plan_okf_create_proposal(
        &paths.proposals_dir().join("pending"),
        &okf::OkfCreateProposalDraft {
            proposal_id: proposal_id.to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            title: "Capture journal binding".to_owned(),
            body: "The commit marker must bind every exact journal field.".to_owned(),
            actor: "agent:recovery-test".to_owned(),
            timestamp: "2026-07-10T12:00:00Z".to_owned(),
            reason: Some("Exercise journal substitution".to_owned()),
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            applies_to: vec!["crates/memzoi-core/**".to_owned()],
            tags: vec!["capture".to_owned()],
            sources: Vec::new(),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            capture: None,
            retention: crate::retention_facts_for_creation(
                MemoryLane::Semantic,
                "2026-07-10T12:00:00Z",
                None,
                None,
            )?,
            origin: OriginDescriptor::new(
                format!("test:{proposal_id}"),
                OriginRoute::RepositoryProposal,
            ),
            lineage: None,
        },
    )?;
    let safety_values = okf_proposal_safety_values("candidate[candidate_test]", &proposal.parsed);
    let (journal, authorization, projections) = authorized_test_capture_apply(
        &paths,
        proposal_id,
        proposal.markdown.as_bytes(),
        &safety_values,
    )?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        &proposal.markdown,
        &journal.journal_id,
    )?;
    install_capture_apply_proposals(&paths, &journal, &authorization, &projections)?;
    append_capture_apply_commit_marker(
        &service.conn,
        &journal,
        "agent:recovery-test",
        "2026-07-10T12:00:00Z",
    )?;
    fs::remove_file(capture_apply_ownership_path(&paths))?;

    let mut substituted = journal.clone();
    substituted.entries[0].projection_digest = "f".repeat(64);
    let mut bytes = serde_json::to_vec_pretty(&substituted)?;
    bytes.push(b'\n');
    fs::write(capture_apply_journal_path(&paths), bytes)?;

    let recovery_error = recover_capture_apply(&paths, &service.conn)
        .expect_err("the DB commit marker must reject any exact journal substitution");

    assert!(format!("{recovery_error:#}").contains("does not match"));
    assert_eq!(fs::read(&destination)?, proposal.markdown.as_bytes());
    assert_eq!(fs::read(&staged)?, proposal.markdown.as_bytes());
    assert!(capture_apply_journal_path(&paths).is_file());
    Ok(())
}

#[test]
fn open_rolls_back_uncommitted_capture_proposal_files_from_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe staged proposal\n";
    let (journal, authorization, projections) =
        authorized_test_capture_apply(&paths, "mem_capture_uncommitted", contents, &[])?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    assert!(
        !fs::read_to_string(capture_apply_journal_path(&paths))?
            .contains("repo-safe staged proposal"),
        "the durable journal must contain metadata and hashes, not proposal bodies"
    );
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        std::str::from_utf8(contents)?,
        &journal.journal_id,
    )?;
    install_capture_apply_proposals(&paths, &journal, &authorization, &projections)?;
    assert!(capture_apply_ownership_path(&paths).is_file());
    drop(service);

    let reopened = MemoryService::open_paths(paths.clone())?;

    assert!(!destination.exists());
    assert!(!staged.exists());
    assert!(!capture_apply_journal_path(&paths).exists());
    assert!(!capture_apply_ownership_path(&paths).exists());
    assert!(!capture_apply_commit_marker_exists(
        &reopened.conn,
        &journal
    )?);
    Ok(())
}

#[test]
fn uncommitted_recovery_finishes_cleanup_after_crash_with_destination_already_backed_up()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe proposal interrupted during rollback\n";
    let (journal, authorization, projections) =
        authorized_test_capture_apply(&paths, "mem_capture_interrupted_rollback", contents, &[])?;
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let destination = capture_apply_destination_path(&paths, entry);
    let staged = stage_authorized_file(
        &paths,
        RepositoryWriteRoute::CaptureApply,
        &authorization,
        &projections,
        &destination,
        std::str::from_utf8(contents)?,
        &journal.journal_id,
    )?;
    install_capture_apply_proposals(&paths, &journal, &authorization, &projections)?;
    let recovery_backup =
        repository_transaction_path(&paths, &destination, &journal.review_id, "recovery-cleanup");

    inject_after_capture_recovery_backup_hook(|| {
        anyhow::bail!("injected crash after capture recovery backup")
    });
    let error = recover_capture_apply(&paths, &service.conn)
        .expect_err("the injected crash must interrupt recovery backup cleanup");
    assert!(format!("{error:#}").contains("injected crash"));
    assert!(!destination.exists());
    assert_eq!(fs::read(&recovery_backup)?, contents);
    assert!(capture_apply_journal_path(&paths).is_file());
    assert!(capture_apply_ownership_path(&paths).is_file());

    assert_eq!(
        recover_capture_apply(&paths, &service.conn)?,
        CaptureApplyRecoveryOutcome::RolledBack
    );
    assert!(!destination.exists());
    assert!(!staged.exists());
    assert!(!recovery_backup.exists());
    assert!(!capture_apply_journal_path(&paths).exists());
    assert!(!capture_apply_ownership_path(&paths).exists());
    Ok(())
}

#[test]
fn open_rolls_back_journal_written_before_proposal_staging() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let journal = test_capture_apply_journal(
        &paths,
        "mem_capture_before_staging",
        b"repo-safe proposal that was never staged\n",
    );
    write_capture_apply_journal(&paths, &journal)?;
    let destination = capture_apply_destination_path(&paths, &journal.entries[0]);
    drop(service);

    let _reopened = MemoryService::open_paths(paths.clone())?;

    assert!(!destination.exists());
    assert!(!capture_apply_journal_path(&paths).exists());
    Ok(())
}

#[test]
fn open_rejects_an_incompatible_capture_journal_without_mutating_artifacts() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    let proposal_id = "mem_incompatible_capture_rollback";
    let contents = b"Authorization: Bearer deterministic-test-fixture\n";
    let destination = paths
        .proposals_dir()
        .join("pending")
        .join(format!("{proposal_id}.md"));
    fs::write(&destination, contents)?;
    let mut incompatible_journal = test_capture_apply_journal(&paths, proposal_id, contents);
    incompatible_journal.schema = "incompatible/capture-apply-journal".to_owned();
    fs::write(
        capture_apply_journal_path(&paths),
        serde_json::to_vec_pretty(&incompatible_journal)?,
    )?;
    drop(service);

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("incompatible recovery without ownership must block startup")?;

    assert!(
        format!("{error:#}").contains("unsupported capture apply journal schema"),
        "{error:#}"
    );
    assert_eq!(fs::read(&destination)?, contents);
    assert!(capture_apply_journal_path(&paths).exists());
    Ok(())
}

#[test]
fn committed_recovery_fails_closed_without_verifiable_authorization() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe committed proposal\n";
    let journal = test_capture_apply_journal(&paths, "mem_capture_committed", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    let destination = capture_apply_destination_path(&paths, entry);
    fs::write(&staged, contents)?;
    append_capture_apply_commit_marker(
        &service.conn,
        &journal,
        "agent:recovery-test",
        "2026-07-10T12:00:00Z",
    )?;
    drop(service);

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("invalid committed recovery authorization should block startup")?;

    assert!(!destination.exists());
    assert!(staged.exists());
    assert!(capture_apply_journal_path(&paths).exists());
    assert!(format!("{error:#}").contains("refusing recovery mutation"));
    Ok(())
}

#[test]
fn recovery_preserves_mismatched_files_and_keeps_the_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe expected proposal\n";
    let journal = test_capture_apply_journal(&paths, "mem_capture_mismatch", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    let destination = capture_apply_destination_path(&paths, entry);
    fs::write(&staged, contents)?;
    fs::write(&destination, b"user replacement\n")?;
    drop(service);

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("mismatched recovery file should block startup")?;

    assert!(format!("{error:#}").contains("refusing recovery mutation"));
    assert_eq!(fs::read(&destination)?, b"user replacement\n");
    assert_eq!(fs::read(&staged)?, contents);
    assert!(capture_apply_journal_path(&paths).is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn recovery_rejects_a_replaced_project_root_before_repository_mutation() -> anyhow::Result<()> {
    let (temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe expected proposal\n";
    let journal = test_capture_apply_journal(&paths, "mem_capture_replaced_root", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    fs::write(&staged, contents)?;
    drop(service);

    let original_project = temp.path().join("original-project");
    fs::rename(&paths.project_root, &original_project)?;
    fs::create_dir(&paths.project_root)?;
    let destination = capture_apply_destination_path(&paths, entry);
    fs::create_dir_all(destination.parent().context("destination has no parent")?)?;
    fs::create_dir_all(paths.records_dir())?;
    fs::write(&destination, contents)?;

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("a replaced project root should block capture recovery")?;

    assert!(format!("{error:#}").contains("different repository root"));
    assert_eq!(fs::read(&destination)?, contents);
    assert!(staged.exists());
    assert!(capture_apply_journal_path(&paths).exists());
    Ok(())
}

fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    fs::create_dir(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    let service = MemoryService::open_paths(paths)?;
    Ok((temp, service))
}

fn test_capture_apply_journal(
    paths: &MemoryPaths,
    proposal_id: &str,
    contents: &[u8],
) -> CaptureApplyJournal {
    CaptureApplyJournal {
        schema: CAPTURE_APPLY_JOURNAL_SCHEMA.to_owned(),
        safety_contract_version: crate::REPOSITORY_WRITE_SAFETY_VERSION.to_owned(),
        detector_policy_version: crate::REPOSITORY_WRITE_DETECTOR_POLICY_VERSION.to_owned(),
        route: crate::RepositoryWriteRoute::CaptureApply
            .as_str()
            .to_owned(),
        authorization_digest: "0".repeat(64),
        project_context_digest: blake3::hash(
            &crate::repository_io::repository_project_identity(&paths.project_root).unwrap(),
        )
        .to_hex()
        .to_string(),
        journal_id: Uuid::now_v7().to_string(),
        plan_id: "capture_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        review_id: "review_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .to_owned(),
        entries: vec![CaptureApplyJournalEntry {
            candidate_id: "candidate_test".to_owned(),
            proposal_id: proposal_id.to_owned(),
            origin: OriginDescriptor::new("capture:test-journal", OriginRoute::Capture),
            input_fingerprint: "1".repeat(64),
            intended_outcome: crate::OriginOutcomeKind::Created,
            content_bytes: contents.len() as u64,
            content_hash: blake3::hash(contents).to_hex().to_string(),
            projection_digest: "0".repeat(64),
        }],
    }
}

fn authorized_test_capture_apply(
    paths: &MemoryPaths,
    proposal_id: &str,
    contents: &[u8],
    safety_values: &[RepositorySafetyValue],
) -> anyhow::Result<(
    CaptureApplyJournal,
    AuthorizedRepositoryProjectionBatch,
    Vec<OwnedRepositoryProjection>,
)> {
    let mut journal = test_capture_apply_journal(paths, proposal_id, contents);
    let destination = capture_apply_destination_path(paths, &journal.entries[0]);
    let projections = vec![OwnedRepositoryProjection::from_absolute(
        paths,
        &destination,
        contents,
        None,
    )?];
    let authorization = authorize_repository_projection_batch(
        paths,
        RepositoryWriteRoute::CaptureApply,
        OkfProposalSensitivity::RepoSafe,
        ScopeKind::Repo,
        None,
        Visibility::Repo,
        AuthorizationProof::CaptureReview {
            plan_id: &journal.plan_id,
            review_id: &journal.review_id,
        },
        explicit_repository_provenance(
            RepositoryContentClass::GeneralRepoKnowledge,
            &journal.review_id,
        ),
        safety_values,
        &projections,
    )?;
    journal.authorization_digest = authorization.digest();
    journal.project_context_digest = blake3::hash(
        &crate::repository_io::repository_project_identity(&paths.project_root)?,
    )
    .to_hex()
    .to_string();
    let entry = &mut journal.entries[0];
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi.capture.projection\0");
    hasher.update(destination.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    hasher.update(contents);
    entry.projection_digest = hasher.finalize().to_hex().to_string();
    Ok((journal, authorization, projections))
}
