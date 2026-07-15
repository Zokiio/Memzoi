use std::fs;

use super::*;
use crate::repository_io;
use crate::{
    MemoryLane, MemoryStatus, MemoryType, ProposalStatus, ScopeKind, SessionEndCandidate,
    SessionEndCandidateStatus, Visibility,
};
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn create_only_write_and_sync_failures_leave_no_partial_repository_file() -> anyhow::Result<()> {
    for failure in [
        repository_io::InjectedCreateFileFailure::Write,
        repository_io::InjectedCreateFileFailure::Sync,
    ] {
        let (_temp, service) = initialized_service()?;
        let token = format!("createfailure{}", Uuid::now_v7());
        repository_io::inject_repository_create_failure(failure);

        let error = service
            .propose_memory_with_options(
                "agent:failure-test",
                sample_memory_draft("Injected create failure", &token),
                ProposeOptions {
                    approval_override: None,
                    apply: true,
                },
            )
            .expect_err("injected create persistence failure must abort the write");

        assert!(format!("{error:#}").contains("injected repository"));
        assert!(okf::read_okf_record_files(service.paths.records_dir())?.is_empty());
        assert!(
            service
                .search_memory(SearchInput {
                    query: token,
                    ..SearchInput::default()
                })?
                .is_empty()
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn overwrite_write_and_sync_failures_restore_the_original_repository_file() -> anyhow::Result<()> {
    for failure in [
        repository_io::InjectedCreateFileFailure::Write,
        repository_io::InjectedCreateFileFailure::Sync,
    ] {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft("Injected overwrite target", "Original durable body."),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let original_markdown = fs::read(&target_path)?;
        repository_io::inject_repository_create_failure(failure);

        let error = service
            .supersede_record(
                &target.id,
                "agent:failure-test",
                sample_memory_draft(
                    "Injected overwrite replacement",
                    "Replacement durable body.",
                ),
            )
            .expect_err("injected overwrite persistence failure must abort the write");

        assert!(format!("{error:#}").contains("injected repository"));
        assert_eq!(fs::read(&target_path)?, original_markdown);
        assert_eq!(
            RuntimeRecords::new(&service.conn)
                .get(&target.id)?
                .context("original record must remain indexed")?
                .status,
            MemoryStatus::Active
        );
        let records = okf::read_okf_record_files(service.paths.records_dir())?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].concept_id, target.id);
        assert_eq!(records[0].status, MemoryStatus::Active);
    }
    Ok(())
}

#[test]
fn propose_with_options_auto_approves_unique_proposals_by_default() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let result = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Default auto proposal", "Unique default auto proposal body"),
        ProposeOptions {
            approval_override: None,
            apply: false,
        },
    )?;

    assert_eq!(result.proposal.status, ProposalStatus::Approved);
    assert_eq!(
        result
            .validation
            .as_ref()
            .map(|validation| validation.is_valid),
        Some(true)
    );
    assert_eq!(result.record, None);
    assert!(!result.applied);
    let approved =
        service.list_proposals(ProposalStatusFilter::Status(ProposalStatus::Approved))?;
    assert_eq!(
        approved
            .iter()
            .map(|proposal| proposal.id.as_str())
            .collect::<Vec<_>>(),
        vec![result.proposal.id.as_str()]
    );

    Ok(())
}

#[test]
fn auto_approval_and_apply_cannot_bypass_unknown_sensitivity() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let mut draft = sample_memory_draft(
        "Unknown sensitivity proposal",
        "Content must remain outside canonical memory until classified.",
    );
    draft.sensitivity = crate::OkfProposalSensitivity::Unknown;

    let result = service.propose_memory_with_options(
        "agent:red-tests",
        draft,
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Auto),
            apply: true,
        },
    )?;

    assert_eq!(result.proposal.status, ProposalStatus::Pending);
    assert!(!result.applied);
    assert_eq!(result.record, None);
    assert!(result.validation.as_ref().is_some_and(|validation| {
        validation
            .issues
            .iter()
            .any(|issue| issue.code == "repo_sensitivity_required")
    }));
    let record_count: i64 =
        service
            .conn
            .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
    assert_eq!(record_count, 0);

    Ok(())
}

#[test]
fn propose_with_options_manual_override_leaves_proposal_pending() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let result = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Manual proposal", "Manual proposal body"),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;

    assert_eq!(result.proposal.status, ProposalStatus::Pending);
    assert_eq!(result.validation, None);
    assert_eq!(result.record, None);
    assert!(!result.applied);
    let counts = service.open_proposal_counts()?;
    assert_eq!(counts.get(&ProposalStatus::Pending), Some(&1));

    Ok(())
}

#[test]
fn propose_with_options_apply_writes_canonical_record_file() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let result = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Applied proposal", "Applied proposal body"),
        ProposeOptions {
            approval_override: None,
            apply: true,
        },
    )?;

    let record = result
        .record
        .as_ref()
        .expect("apply mode should return the canonical record");
    assert!(result.applied);
    assert_eq!(result.proposal.status, ProposalStatus::Applied);
    assert_eq!(record.status, MemoryStatus::Active);
    assert_eq!(record.title, "Applied proposal");
    assert_eq!(record.source_ref.as_deref(), Some("service-proposal-tests"));
    assert_eq!(record.source_kind.as_deref(), Some("test"));
    assert_eq!(
        record.proposal_id.as_deref(),
        Some(result.proposal.id.as_str())
    );

    let mut applied_event_proposal_ids = Vec::new();
    service.for_each_event(|event| {
        if event.event_type == "memory.applied" {
            applied_event_proposal_ids.push(event.proposal_id);
        }
        Ok(())
    })?;
    assert_eq!(
        applied_event_proposal_ids,
        vec![Some(result.proposal.id.clone())]
    );

    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    let canonical = fs::read_to_string(&record_path)?;
    assert!(
        canonical.contains("status: active\n"),
        "canonical record should be written as an active OKF record: {canonical}"
    );
    assert!(
        canonical.contains("# Applied proposal"),
        "canonical record should include the approved title: {canonical}"
    );
    assert!(
        canonical.contains("Applied proposal body"),
        "canonical record should include the approved body: {canonical}"
    );
    assert!(
        canonical.contains("source_ref: service-proposal-tests\n"),
        "canonical record should preserve the evidence reference: {canonical}"
    );
    assert!(
        canonical.contains(&format!("proposal_id: {}\n", result.proposal.id)),
        "canonical record should store proposal lineage separately: {canonical}"
    );

    Ok(())
}

#[test]
fn evidence_provenance_and_proposal_lineage_survive_rebuild_and_export() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let mut draft = sample_memory_draft(
        "Lineage survives rebuild",
        "\n  Evidence-backed zircon lineage.  \n",
    );
    draft.source_kind = Some("  test  ".to_owned());
    draft.source_ref = Some("  service-proposal-tests  ".to_owned());
    let applied = service.propose_memory_with_options(
        "agent:red-tests",
        draft,
        ProposeOptions {
            approval_override: None,
            apply: true,
        },
    )?;
    let proposal_id = applied.proposal.id.clone();
    let applied_record = applied.record.expect("record should be applied");
    let record_id = applied_record.id.clone();
    let expected_hash = blake3::hash("Evidence-backed zircon lineage.".as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(applied_record.content_hash, expected_hash);
    assert!(
        service.repo_index_drift()?.is_current(),
        "normalized proposal apply must agree with its canonical file immediately"
    );

    service.rebuild()?;
    let rebuilt = MemoryService::open_paths(paths)?;
    let results = rebuilt.search_memory(SearchInput {
        query: "zircon lineage".to_owned(),
        scope_kind: Some(ScopeKind::Repo),
        scope_id: None,
        memory_type: None,
        lane: None,
        destination: Some(MemoryDestination::Repo),
        path_prefix: None,
        limit: 10,
        include_inactive: false,
    })?;
    let result = results
        .iter()
        .find(|result| result.record.id == record_id)
        .expect("rebuilt recall should return the applied record");
    assert_eq!(result.record.source_kind.as_deref(), Some("test"));
    assert_eq!(
        result.record.source_ref.as_deref(),
        Some("service-proposal-tests")
    );
    assert_eq!(
        result.record.proposal_id.as_deref(),
        Some(proposal_id.as_str())
    );
    assert_eq!(result.record.content_hash, expected_hash);
    assert_eq!(
        result.citations[0].source_ref.as_deref(),
        Some("service-proposal-tests"),
        "recall citations must point at original evidence, not the review packet"
    );

    let exported = rebuilt.export(ExportInput {
        format: ExportFormat::Okf,
        scope_kind: ScopeKind::Repo,
    })?;
    let markdown = fs::read_to_string(&exported.written_paths[0])?;
    assert!(markdown.contains("source_kind: \"test\""));
    assert!(markdown.contains("source_ref: \"service-proposal-tests\""));
    assert!(markdown.contains(&format!("proposal_id: \"{proposal_id}\"")));

    Ok(())
}

#[test]
fn propose_with_options_rejects_manual_apply_without_creating_proposal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let error = service
        .propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Manual apply proposal", "Manual apply proposal body"),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: true,
            },
        )
        .expect_err("manual apply should fail before creating an unappliable proposal");

    let message = error.to_string();
    assert!(
        message.contains("proposal apply mode requires auto approval"),
        "manual apply error should explain the auto-approval requirement: {message}"
    );
    assert!(
        message.contains("manual proposals must be approved before apply"),
        "manual apply error should tell callers how manual proposals progress: {message}"
    );
    assert!(
        service
            .list_proposals(ProposalStatusFilter::All)?
            .is_empty(),
        "manual apply refusal should not leave an unreviewed proposal behind"
    );

    Ok(())
}

#[test]
fn duplicate_propose_with_apply_remains_unapproved_and_unapplied() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let draft = sample_memory_draft("Duplicate proposal", "Duplicate proposal body");
    let original = service.propose_memory_with_options(
        "agent:red-tests",
        draft.clone(),
        ProposeOptions {
            approval_override: None,
            apply: true,
        },
    )?;
    let original_record = original
        .record
        .as_ref()
        .expect("initial unique proposal should apply");

    let duplicate = service.propose_memory_with_options(
        "agent:red-tests",
        draft,
        ProposeOptions {
            approval_override: None,
            apply: true,
        },
    )?;

    let validation = duplicate
        .validation
        .as_ref()
        .expect("duplicate proposal should be validated before approval");
    assert!(!validation.is_valid);
    assert!(
        validation.issues.iter().any(|issue| {
            issue.code == "duplicate_content_hash"
                && issue.record_id.as_deref() == Some(original_record.id.as_str())
        }),
        "duplicate validation should name the conflicting canonical record: {validation:?}"
    );
    assert_eq!(duplicate.proposal.status, ProposalStatus::Pending);
    assert_eq!(duplicate.record, None);
    assert!(!duplicate.applied);
    assert_eq!(
        service
            .show_proposal(duplicate.proposal.id.as_str())?
            .status,
        ProposalStatus::Pending
    );

    Ok(())
}

#[test]
fn legacy_lifecycle_rejects_runtime_targets_without_canonical_leaks() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let local = service.create_local_memory(
        "agent:red-tests",
        LocalMemoryInput {
            memory_type: MemoryType::Preference,
            lane: MemoryLane::Semantic,
            title: "Private runtime preference".to_owned(),
            body: "This local-only body must never become canonical.".to_owned(),
        },
    )?;
    let checkpoint = service.create_checkpoint(
        "agent:red-tests",
        CheckpointInput {
            task: "Private runtime checkpoint".to_owned(),
            note: "This session-only body must never become canonical.".to_owned(),
        },
    )?;

    let local_error = service
        .tombstone_record(&local.id, "agent:red-tests", "must stay local")
        .expect_err("local targets must not be written as canonical tombstones");
    assert!(local_error.to_string().contains("destination local"));

    let session_error = service
        .supersede_record(
            &checkpoint.id,
            "agent:red-tests",
            sample_memory_draft(
                "Replacement for session checkpoint",
                "A repo replacement must not promote the private target.",
            ),
        )
        .expect_err("session targets must not be written as canonical supersedes");
    assert!(session_error.to_string().contains("destination session"));

    for record in [&local, &checkpoint] {
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{}.md", record.id))
                .exists(),
            "runtime target {} leaked into canonical records",
            record.id
        );
        let stored = service.inspect_expiry(&record.id)?.record;
        assert_eq!(stored.status, MemoryStatus::Active);
        assert_eq!(stored.destination, record.destination);
    }
    Ok(())
}

#[test]
fn legacy_lifecycle_rejects_private_and_inactive_repo_targets() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let private_draft = sample_memory_draft(
        "Private repo target",
        "Private visibility must not be rewritten through a legacy lifecycle route.",
    );
    let private = apply_test_record(&service, private_draft)?;
    service.conn.execute(
        "UPDATE memory_record SET visibility = 'private' WHERE id = ?1",
        [&private.id],
    )?;
    let private_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", private.id));
    let private_before = fs::read(&private_path)?;

    let private_error = service
        .tombstone_record(&private.id, "agent:red-tests", "must stay private")
        .expect_err("private targets must not be rewritten canonically");
    assert!(private_error.to_string().contains("visibility private"));
    assert_eq!(fs::read(&private_path)?, private_before);
    assert_eq!(
        service.inspect_expiry(&private.id)?.record.status,
        MemoryStatus::Active
    );

    let active = apply_test_record(
        &service,
        sample_memory_draft(
            "Inactive lifecycle target",
            "Inactive targets must be rejected before file mutation.",
        ),
    )?;
    let active_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", active.id));
    let active_before = fs::read(&active_path)?;
    service.conn.execute(
        "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
        [active.id.as_str()],
    )?;

    let inactive_error = service
        .tombstone_record(&active.id, "agent:red-tests", "already inactive")
        .expect_err("inactive targets must be rejected");
    assert!(inactive_error.to_string().contains("status superseded"));
    assert_eq!(fs::read(&active_path)?, active_before);
    assert_eq!(
        service.inspect_expiry(&active.id)?.record.status,
        MemoryStatus::Superseded
    );
    Ok(())
}

#[test]
fn legacy_supersede_rejects_cross_scope_replacements_before_mutation() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft(
            "Same-scope lifecycle target",
            "The target must remain active when replacement scope differs.",
        ),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let target_before = fs::read(&target_path)?;
    let mut replacement = sample_memory_draft(
        "Cross-scope lifecycle replacement",
        "A team-scoped replacement cannot supersede a repo-scoped target.",
    );
    replacement.scope_kind = ScopeKind::Team;
    replacement.scope_id = Some("platform".to_owned());

    let error = service
        .supersede_record(&target.id, "agent:red-tests", replacement)
        .expect_err("cross-scope replacements must be rejected");
    assert!(error.to_string().contains("cross-scope"));
    assert_eq!(fs::read(&target_path)?, target_before);
    assert_eq!(
        service.inspect_expiry(&target.id)?.record.status,
        MemoryStatus::Active
    );
    assert!(
        !service
            .paths
            .records_dir()
            .join("cross-scope-lifecycle-replacement.md")
            .exists()
    );
    Ok(())
}

#[test]
fn legacy_supersede_rolls_back_db_and_files_when_second_install_fails() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft(
            "Second-write rollback target",
            "The original canonical body must survive a second-write failure.",
        ),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let target_before = fs::read(&target_path)?;
    let replacement = sample_memory_draft(
        "Second-write rollback replacement",
        "This replacement must disappear when its install is interrupted.",
    );

    let error = service
        .supersede_record_with_hooks(
            &target.id,
            "agent:red-tests",
            replacement,
            |index| {
                if index == 1 {
                    return Err(anyhow::anyhow!("injected second-write install failure"));
                }
                Ok(())
            },
            |_| Ok(()),
        )
        .expect_err("second-write failure must abort the lifecycle transaction");
    assert!(error.to_string().contains("second-write install failure"));

    assert_legacy_supersede_unchanged(
        &service,
        &target,
        &target_path,
        &target_before,
        "second-write-rollback-replacement",
    )?;
    Ok(())
}

#[test]
fn legacy_supersede_rolls_back_installed_files_when_db_commit_fails() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft(
            "Commit rollback target",
            "The original canonical body must survive a database commit failure.",
        ),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let target_before = fs::read(&target_path)?;
    let replacement = sample_memory_draft(
        "Commit rollback replacement",
        "This replacement must disappear when SQLite refuses commit.",
    );

    let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                replacement,
                |_| Ok(()),
                |tx| {
                    tx.pragma_update(None, "defer_foreign_keys", "ON")?;
                    tx.execute(
                        "INSERT INTO memory_tag(record_id, tag) VALUES ('missing-record', 'deferred-commit-failure')",
                        [],
                    )?;
                    Ok(())
                },
            )
            .expect_err("deferred foreign-key violation must fail commit");
    assert!(
        error
            .to_string()
            .contains("failed to commit memory lifecycle transaction"),
        "expected actual commit failure, got: {error:#}"
    );

    assert_legacy_supersede_unchanged(
        &service,
        &target,
        &target_path,
        &target_before,
        "commit-rollback-replacement",
    )?;
    Ok(())
}

#[test]
fn show_proposal_reports_missing_ids() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let error = service
        .show_proposal("prop_missing")
        .expect_err("missing proposal show should fail");

    assert!(
        error
            .to_string()
            .contains("proposal not found: prop_missing"),
        "missing proposal show error should include the requested id: {error:#}"
    );

    Ok(())
}

#[test]
fn blocked_session_end_result_redacts_task_and_every_candidate_title() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let task_sentinel = "ghp_SESSION_END_TASK_SENTINEL_0123456789abcdefghijklmnop";
    let local_title_sentinel = "BLOCKED-LOCAL-TITLE-SENTINEL";
    let repo_title_sentinel = "BLOCKED-REPO-TITLE-SENTINEL";
    let result = service.promote_session_end(
        "agent:red-tests",
        SessionEndDocument {
            task: task_sentinel.to_owned(),
            candidates: vec![
                SessionEndCandidate {
                    destination: MemoryDestination::Local,
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    title: local_title_sentinel.to_owned(),
                    body: "This candidate must not be written when the repo candidate blocks."
                        .to_owned(),
                    sensitivity: OkfProposalSensitivity::RepoSafe,
                    content_class: RepositoryContentClass::GeneralRepoKnowledge,
                    reason: None,
                    scope: None,
                    tags: Vec::new(),
                },
                SessionEndCandidate {
                    destination: MemoryDestination::Repo,
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    title: repo_title_sentinel.to_owned(),
                    body: "This otherwise safe candidate must block because of the task."
                        .to_owned(),
                    sensitivity: OkfProposalSensitivity::RepoSafe,
                    content_class: RepositoryContentClass::GeneralRepoKnowledge,
                    reason: None,
                    scope: None,
                    tags: Vec::new(),
                },
            ],
        },
    )?;

    assert_eq!(result.task, "Redacted blocked session-end task");
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| candidate.status == SessionEndCandidateStatus::Blocked)
    );
    let rendered = serde_json::to_string(&result)?;
    for sentinel in [task_sentinel, local_title_sentinel, repo_title_sentinel] {
        assert!(
            !rendered.contains(sentinel),
            "blocked result leaked {sentinel}: {rendered}"
        );
    }
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

fn apply_test_record(service: &MemoryService, draft: MemoryDraft) -> anyhow::Result<MemoryRecord> {
    let proposal = service.propose_memory("agent:red-tests", draft)?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    service.apply_proposal(&proposal.id, "agent:applier")
}

fn assert_legacy_supersede_unchanged(
    service: &MemoryService,
    target: &MemoryRecord,
    target_path: &Path,
    target_before: &[u8],
    replacement_id: &str,
) -> anyhow::Result<()> {
    assert_eq!(fs::read(target_path)?, target_before);
    assert_eq!(
        service.inspect_expiry(&target.id)?.record.status,
        MemoryStatus::Active
    );
    assert!(
        service.inspect_expiry(replacement_id).is_err(),
        "replacement row {replacement_id} survived rollback"
    );
    assert!(
        !service
            .paths
            .records_dir()
            .join(format!("{replacement_id}.md"))
            .exists(),
        "replacement file {replacement_id} survived rollback"
    );
    let transaction_files = fs::read_dir(service.paths.records_dir())?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with('.') && name.ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(
        transaction_files.is_empty(),
        "staged transaction files survived rollback: {transaction_files:?}"
    );
    Ok(())
}

fn sample_memory_draft(title: &str, body: &str) -> MemoryDraft {
    MemoryDraft {
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        visibility: Visibility::Repo,
        title: title.to_owned(),
        body: body.to_owned(),
        tags: vec!["rust".to_owned(), "tests".to_owned()],
        source_kind: Some("test".to_owned()),
        source_ref: Some("service-proposal-tests".to_owned()),
        sensitivity: crate::OkfProposalSensitivity::RepoSafe,
        content_class: crate::RepositoryContentClass::GeneralRepoKnowledge,
        confidence: 0.82,
    }
}
