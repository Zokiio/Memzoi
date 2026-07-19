use std::{cell::Cell, collections::BTreeSet, fs, process::Command, rc::Rc};

use super::*;
use crate::repository_io;
use crate::{
    CanonicalRevision, ExpectedPriorRevision, MaterializationTarget, MemoryLane, MemoryStatus,
    MemoryType, ProposalStatus, RepositoryMaterializationCandidate,
    RepositoryMaterializationCandidateRecord, ScopeKind, SessionEndCandidate,
    SessionEndCandidateStatus, Visibility, build_repository_materialization_candidate,
    build_repository_materialization_decision, build_repository_materialization_plan,
    repository_materialization_candidate_plan, repository_materialization_policy,
};
use tempfile::TempDir;

fn assert_opaque_private_record_id(id: &str, prefix: &str) {
    let uuid = id
        .strip_prefix(&format!("{prefix}-"))
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(|| panic!("private record ID is not an opaque {prefix} UUID: {id}"));
    assert_eq!(uuid.get_version_num(), 4, "private record ID is not random");
}

#[test]
fn unresolved_git_identity_blocks_service_lifecycles_before_writes() -> anyhow::Result<()> {
    for operation in ["open", "initialize", "rebuild"] {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git"))?;
        let paths = MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );

        let result = match operation {
            "open" => MemoryService::open_paths(paths.clone()).map(|_| ()),
            "initialize" => {
                MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })
                    .map(|_| ())
            }
            "rebuild" => MemoryService::rebuild_paths(paths.clone()).map(|_| ()),
            _ => unreachable!(),
        };
        let error = result
            .err()
            .with_context(|| format!("{operation} should fail closed"))?;

        assert!(
            format!("{error:#}").contains("Git repository identity"),
            "{operation} returned the wrong error: {error:#}"
        );
        assert!(
            !paths.repository_runtime_dir.exists(),
            "{operation} created runtime state despite unresolved Git identity"
        );
    }
    Ok(())
}

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
fn open_recovers_applied_proposal_and_event_from_shared_sync_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal = service.propose_memory(
        "agent:shared-sync-test",
        sample_memory_draft(
            "Recover committed proposal apply",
            "The shared proposal transition must survive an interrupted post-commit sync.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:shared-sync-test")?;
    shared_runtime::inject_before_shared_sync_recovery_hook(|| {
        anyhow::bail!("injected post-commit shared-sync interruption")
    });

    let error = service
        .apply_proposal(&proposal.id, "agent:shared-sync-test")
        .expect_err("injected shared-sync interruption must surface after canonical commit");
    assert!(format!("{error:#}").contains("injected post-commit shared-sync interruption"));
    assert_eq!(
        proposals::load_proposal_public(&service.shared_conn, &proposal.id)?.status,
        ProposalStatus::Approved
    );
    let (record_id, indexed_status): (String, String) = service.conn.query_row(
        "SELECT id, status FROM memory_record WHERE proposal_id = ?1",
        [&proposal.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(indexed_status, "active");
    assert!(
        paths
            .records_dir()
            .join(format!("{record_id}.md"))
            .is_file()
    );
    assert!(
        paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .is_file()
    );
    drop(service);

    let recovered = MemoryService::open_paths(paths.clone())?;
    assert_eq!(
        recovered.show_proposal(&proposal.id)?.status,
        ProposalStatus::Applied
    );
    let shared_apply_events: i64 = recovered.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_apply_events, 1);
    assert!(
        !paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    drop(recovered);

    let reopened = MemoryService::open_paths(paths)?;
    let shared_apply_events: i64 = reopened.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_apply_events, 1);
    Ok(())
}

#[test]
fn open_finishes_proposal_sync_after_marker_cleanup_interruption() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal = service.propose_memory(
        "agent:shared-sync-test",
        sample_memory_draft(
            "Recover marker cleanup interruption",
            "An already-applied shared payload must make journal cleanup idempotent.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:shared-sync-test")?;
    shared_runtime::inject_after_shared_sync_marker_cleanup_hook(|| {
        anyhow::bail!("injected interruption after shared-sync marker cleanup")
    });

    let error = service
        .apply_proposal(&proposal.id, "agent:shared-sync-test")
        .expect_err("the cleanup interruption must surface after the shared payload is applied");
    assert!(format!("{error:#}").contains("injected interruption"));
    assert_eq!(
        proposals::load_proposal_public(&service.shared_conn, &proposal.id)?.status,
        ProposalStatus::Applied
    );
    let marker_count: i64 = service.conn.query_row(
        "SELECT COUNT(*) FROM event_log WHERE event_type = ?1",
        ["memzoi.shared_sync.index_committed"],
        |row| row.get(0),
    )?;
    assert_eq!(marker_count, 0);
    assert!(
        paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .is_file()
    );
    drop(service);

    let recovered = MemoryService::open_paths(paths.clone())?;
    assert_eq!(
        recovered.show_proposal(&proposal.id)?.status,
        ProposalStatus::Applied
    );
    let indexed_record_count: i64 = recovered.conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE proposal_id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_record_count, 1);
    let shared_apply_events: i64 = recovered.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_apply_events, 1);
    assert!(
        !paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    Ok(())
}

#[test]
fn reject_recovers_interrupted_apply_before_mutating_shared_proposal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let proposal = service.propose_memory(
        "agent:shared-sync-test",
        sample_memory_draft(
            "Reject after interrupted apply",
            "Recovery must publish the committed apply before a reject is evaluated.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:shared-sync-test")?;
    shared_runtime::inject_before_shared_sync_recovery_hook(|| {
        anyhow::bail!("injected post-commit shared-sync interruption")
    });
    service
        .apply_proposal(&proposal.id, "agent:shared-sync-test")
        .expect_err("injected shared-sync interruption must surface");

    let error = service
        .reject_proposal(
            &proposal.id,
            "reviewer:shared-sync-test",
            "must not replace a committed apply",
        )
        .expect_err("the recovered applied proposal cannot be rejected");
    assert!(format!("{error:#}").contains("applied proposal"));
    assert_eq!(
        service.show_proposal(&proposal.id)?.status,
        ProposalStatus::Applied
    );
    assert!(
        !service
            .paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    Ok(())
}

#[test]
fn refresh_refuses_shared_id_owned_by_inactive_repo_record() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let repo_record = apply_test_record(
        &service,
        sample_memory_draft(
            "Inactive repository collision",
            "An inactive repository row still owns its identifier.",
        ),
    )?;
    service.conn.execute(
        "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
        [&repo_record.id],
    )?;
    let mut local_record = repo_record.clone();
    local_record.destination = MemoryDestination::Local;
    local_record.scope_kind = ScopeKind::Personal;
    local_record.visibility = Visibility::Private;
    local_record.status = MemoryStatus::Active;
    local_record.title = "Colliding local runtime".to_owned();
    RuntimeRecords::new(&service.shared_conn).insert_for_test(&local_record)?;

    let error =
        shared_runtime::refresh_index_mirrors(&service.paths, &service.shared_conn, &service.conn)
            .expect_err("inactive repository identifiers must remain collision-protected");
    assert!(format!("{error:#}").contains(&repo_record.id));
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&repo_record.id)?
            .context("repository record disappeared")?
            .destination,
        MemoryDestination::Repo
    );
    Ok(())
}

#[test]
fn shared_runtime_ids_are_opaque_and_do_not_reuse_title_derived_repository_ids()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let repo_local = apply_test_record(
        &service,
        sample_memory_draft(
            "Local runtime collision",
            "This canonical record owns the first local runtime candidate identifier.",
        ),
    )?;
    let repo_session = apply_test_record(
        &service,
        sample_memory_draft(
            "Session runtime collision",
            "This canonical record owns the first session runtime candidate identifier.",
        ),
    )?;
    assert_eq!(repo_local.id, "local-runtime-collision");
    assert_eq!(repo_session.id, "session-runtime-collision");

    let local = service.create_local_memory(
        "agent:collision-test",
        LocalMemoryInput {
            memory_type: MemoryType::Preference,
            lane: MemoryLane::Semantic,
            title: "Runtime collision".to_owned(),
            body: "The local write must allocate an opaque private identifier.".to_owned(),
        },
    )?;
    let checkpoint = service.create_checkpoint(
        "agent:collision-test",
        CheckpointInput {
            task: "Runtime collision".to_owned(),
            note: "The session write must allocate an opaque private identifier.".to_owned(),
        },
    )?;

    assert_opaque_private_record_id(&local.id, "local");
    assert_opaque_private_record_id(&checkpoint.id, "session");
    assert_ne!(local.id, repo_local.id);
    assert_ne!(checkpoint.id, repo_session.id);
    for (repo, runtime) in [(&repo_local, &local), (&repo_session, &checkpoint)] {
        assert_eq!(
            RuntimeRecords::new(&service.conn)
                .get(&repo.id)?
                .context("canonical record disappeared during runtime allocation")?
                .destination,
            MemoryDestination::Repo
        );
        assert_eq!(
            RuntimeRecords::new(&service.shared_conn)
                .get(&runtime.id)?
                .context("runtime record was not committed to shared authority")?
                .destination,
            runtime.destination
        );
    }
    Ok(())
}

#[test]
fn direct_runtime_ids_are_opaque_and_do_not_reuse_unindexed_canonical_ids() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let canonical_local = apply_test_record(
        &service,
        sample_memory_draft(
            "Local unindexed canonical collision",
            "This canonical file owns the first local runtime candidate identifier.",
        ),
    )?;
    let canonical_session = apply_test_record(
        &service,
        sample_memory_draft(
            "Session unindexed canonical collision",
            "This canonical file owns the first session runtime candidate identifier.",
        ),
    )?;
    assert_eq!(canonical_local.id, "local-unindexed-canonical-collision");
    assert_eq!(
        canonical_session.id,
        "session-unindexed-canonical-collision"
    );

    for canonical in [&canonical_local, &canonical_session] {
        assert_eq!(
            service
                .conn
                .execute("DELETE FROM memory_record WHERE id = ?1", [&canonical.id])?,
            1
        );
        assert!(
            service
                .paths
                .records_dir()
                .join(format!("{}.md", canonical.id))
                .is_file()
        );
        assert!(
            RuntimeRecords::new(&service.conn)
                .get(&canonical.id)?
                .is_none()
        );
    }

    let local = service.create_local_memory(
        "agent:unindexed-collision-test",
        LocalMemoryInput {
            memory_type: MemoryType::Preference,
            lane: MemoryLane::Semantic,
            title: "Unindexed canonical collision".to_owned(),
            body: "The local write must not derive its identifier from this title.".to_owned(),
        },
    )?;
    let checkpoint = service.create_checkpoint(
        "agent:unindexed-collision-test",
        CheckpointInput {
            task: "Unindexed canonical collision".to_owned(),
            note: "The session write must not derive its identifier from this task.".to_owned(),
        },
    )?;

    assert_opaque_private_record_id(&local.id, "local");
    assert_opaque_private_record_id(&checkpoint.id, "session");
    assert_ne!(local.id, canonical_local.id);
    assert_ne!(checkpoint.id, canonical_session.id);
    let canonical_ids = okf::read_okf_record_files(service.paths.records_dir())?
        .into_iter()
        .map(|record| record.concept_id)
        .collect::<BTreeSet<_>>();
    assert!(canonical_ids.contains(&canonical_local.id));
    assert!(canonical_ids.contains(&canonical_session.id));
    assert!(
        RuntimeRecords::new(&service.shared_conn)
            .get(&canonical_local.id)?
            .is_none()
    );
    assert!(
        RuntimeRecords::new(&service.shared_conn)
            .get(&canonical_session.id)?
            .is_none()
    );
    Ok(())
}

#[test]
fn open_rolls_forward_exact_canonical_create_after_precommit_crash() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal = service.propose_memory(
        "agent:markerless-recovery-test",
        sample_memory_draft(
            "Markerless canonical recovery",
            "Exact durable canonical bytes authorize source-index roll-forward.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:markerless-recovery-test")?;
    canonical_write::inject_after_canonical_install_hook(|| {
        panic!("injected crash after canonical install before index commit")
    });

    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.apply_proposal(&proposal.id, "agent:markerless-recovery-test")
    }));
    assert!(crashed.is_err(), "the crash hook must unwind the apply");
    assert_eq!(
        service.show_proposal(&proposal.id)?.status,
        ProposalStatus::Approved
    );
    let indexed_record_count: i64 = service.conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE proposal_id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_record_count, 0);
    let canonical = okf::read_okf_record_files(paths.records_dir())?;
    assert_eq!(canonical.len(), 1);
    assert_eq!(
        canonical[0].proposal_id.as_deref(),
        Some(proposal.id.as_str())
    );
    assert!(
        paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .is_file()
    );
    drop(service);

    let recovered = MemoryService::open_paths(paths.clone())?;
    assert_eq!(
        recovered.show_proposal(&proposal.id)?.status,
        ProposalStatus::Applied
    );
    let recovered_record_count: i64 = recovered.conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE proposal_id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(recovered_record_count, 1);
    let apply_event_count: i64 = recovered.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(apply_event_count, 1);
    assert!(
        !paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .exists()
    );
    drop(recovered);

    let reopened = MemoryService::open_paths(paths)?;
    let apply_event_count: i64 = reopened.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(apply_event_count, 1);
    Ok(())
}

#[test]
fn markerless_recovery_rejects_substituted_canonical_bytes() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let proposal = service.propose_memory(
        "agent:markerless-recovery-test",
        sample_memory_draft(
            "Substituted markerless canonical",
            "Only the exact authorized canonical bytes may roll the index forward.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:markerless-recovery-test")?;
    canonical_write::inject_after_canonical_install_hook(|| {
        panic!("injected crash after canonical install before index commit")
    });
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.apply_proposal(&proposal.id, "agent:markerless-recovery-test")
    }));
    assert!(crashed.is_err(), "the crash hook must unwind the apply");

    let canonical_path = fs::read_dir(paths.records_dir())?
        .next()
        .context("crashed apply did not install a canonical record")??
        .path();
    let mut substituted = fs::read(&canonical_path)?;
    substituted[0] ^= 1;
    fs::write(&canonical_path, substituted)?;
    drop(service);

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("substituted canonical bytes must block markerless recovery")?;
    assert!(
        format!("{error:#}").contains("bytes do not match the authorized projection"),
        "unexpected recovery error: {error:#}"
    );
    assert!(
        paths
            .repository_runtime_dir
            .join("shared-sync.json")
            .is_file(),
        "failed-closed recovery must retain its journal"
    );
    let index = db::open_database(&paths.index_db_path)?;
    db::init_database(&index)?;
    let indexed_record_count: i64 = index.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE proposal_id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_record_count, 0);
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
fn shared_proposals_are_authoritative_during_reads_and_open() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let read_only_proposal = proposals::propose_memory(
        &service.conn,
        "agent:index-fixture",
        sample_memory_draft("Index-only proposal", "Disposable index state"),
    )?;

    service.search_memory(SearchInput {
        query: "unrelated".to_owned(),
        ..SearchInput::default()
    })?;

    assert!(
        proposals::load_proposal_public(&service.shared_conn, &read_only_proposal.id).is_err(),
        "a read path must not promote an index-only proposal into shared state"
    );
    assert!(
        proposals::load_proposal_public(&service.conn, &read_only_proposal.id).is_err(),
        "shared state must overwrite stale disposable proposal mirrors"
    );

    let startup_proposal = proposals::propose_memory(
        &service.conn,
        "agent:index-fixture",
        sample_memory_draft("Startup index-only proposal", "Disposable startup state"),
    )?;
    let paths = service.paths.clone();
    drop(service);

    let reopened = MemoryService::open_paths(paths)?;
    assert!(reopened.show_proposal(&startup_proposal.id).is_err());
    assert!(
        proposals::load_proposal_public(&reopened.conn, &startup_proposal.id).is_err(),
        "opening must refresh from shared state without copying index proposals back"
    );
    Ok(())
}

#[test]
fn open_rejects_an_incompatible_disposable_index_without_rebuilding_it() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let _record = apply_test_record(
        &service,
        sample_memory_draft(
            "Disposable index rebuild",
            "Canonical records must survive replacement of an incompatible index.",
        ),
    )?;
    let paths = service.paths.clone();
    drop(service);

    let incompatible = Connection::open(&paths.index_db_path)?;
    incompatible.execute("CREATE TABLE obsolete_index_layout(id INTEGER)", [])?;
    drop(incompatible);

    let before = std::fs::read(&paths.index_db_path)?;
    let error = match MemoryService::open_paths(paths.clone()) {
        Ok(_) => anyhow::bail!("an existing old-schema index must fail current-only open"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("unsupported SQLite schema"));
    assert_eq!(std::fs::read(&paths.index_db_path)?, before);

    let unchanged = Connection::open(&paths.index_db_path)?;
    let obsolete_exists: bool = unchanged.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema
           WHERE type = 'table' AND name = 'obsolete_index_layout'
         )",
        [],
        |row| row.get(0),
    )?;
    assert!(
        obsolete_exists,
        "incompatible index was unexpectedly replaced"
    );
    Ok(())
}

#[test]
fn unchanged_search_does_not_open_the_repository_lifecycle_lock() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let _lifecycle_lock = RepoLifecycleLock::acquire(&service.paths)?;

    let results = service.search_memory(SearchInput {
        query: "absent mirror freshness sentinel".to_owned(),
        destination: Some(MemoryDestination::Repo),
        ..SearchInput::default()
    })?;

    assert!(results.is_empty());
    Ok(())
}

#[test]
fn unscoped_search_retries_when_private_mirror_generation_changes() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let hook_calls = Rc::new(Cell::new(0_usize));
    let observed_calls = Rc::clone(&hook_calls);
    inject_after_private_mirror_read_hook(move |shared| {
        let call = observed_calls.get();
        observed_calls.set(call + 1);
        if call == 0 {
            shared.execute(
                "UPDATE private_lifecycle_generation
                 SET generation = generation + 1
                 WHERE singleton = 1",
                [],
            )?;
        }
        Ok(())
    });

    let result = service.search_memory(SearchInput {
        query: "absent unscoped mirror freshness sentinel".to_owned(),
        ..SearchInput::default()
    });
    clear_after_private_mirror_read_hook();

    assert!(result?.is_empty());
    assert_eq!(hook_calls.get(), 2, "unscoped search did not retry");
    assert!(shared_runtime::lifecycle_generations_match(
        &service.shared_conn,
        &service.conn
    )?);
    Ok(())
}

#[test]
fn benchmark_read_helpers_are_repository_only() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;

    let search_error = service
        .search_memory_for_benchmark(SearchInput {
            query: "private benchmark sentinel".to_owned(),
            destination: Some(MemoryDestination::Local),
            ..SearchInput::default()
        })
        .expect_err("benchmark search must reject private destinations");
    assert_eq!(
        search_error.to_string(),
        "benchmark search is repository-only"
    );

    let context_error = service
        .build_context_pack_for_benchmark(ContextPackInput {
            task: "private benchmark sentinel".to_owned(),
            include_session: true,
            ..ContextPackInput::default()
        })
        .expect_err("benchmark context building must reject private destinations");
    assert_eq!(
        context_error.to_string(),
        "benchmark context building is repository-only"
    );
    Ok(())
}

#[test]
fn private_mirror_read_retries_after_a_generation_change() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let local = service.create_local_memory(
        "agent:red-tests",
        LocalMemoryInput {
            memory_type: MemoryType::Preference,
            lane: MemoryLane::Semantic,
            title: "Mirror retry sentinel".to_owned(),
            body: "Private mirror retry result".to_owned(),
        },
    )?;
    let mut injected = false;
    inject_after_private_mirror_read_hook(move |shared| {
        if !injected {
            shared.execute(
                "UPDATE private_lifecycle_generation
                 SET generation = generation + 1
                 WHERE singleton = 1",
                [],
            )?;
            injected = true;
        }
        Ok(())
    });

    let result = service.build_context_pack(ContextPackInput {
        task: "Mirror retry sentinel".to_owned(),
        include_local: true,
        ..ContextPackInput::default()
    });
    clear_after_private_mirror_read_hook();
    let pack = result?;
    assert!(
        pack.records
            .iter()
            .any(|result| result.record.id == local.id),
        "fresh retry omitted the private record"
    );
    assert!(shared_runtime::lifecycle_generations_match(
        &service.shared_conn,
        &service.conn
    )?);
    Ok(())
}

#[test]
fn private_mirror_read_fails_safely_after_two_unstable_generations() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    service.create_local_memory(
        "agent:red-tests",
        LocalMemoryInput {
            memory_type: MemoryType::Preference,
            lane: MemoryLane::Semantic,
            title: "Persistent mirror race".to_owned(),
            body: "This result must not be returned from an unstable mirror.".to_owned(),
        },
    )?;
    inject_after_private_mirror_read_hook(|shared| {
        shared.execute(
            "UPDATE private_lifecycle_generation
             SET generation = generation + 1
             WHERE singleton = 1",
            [],
        )?;
        Ok(())
    });

    let result = service.build_context_pack(ContextPackInput {
        task: "Persistent mirror race".to_owned(),
        include_local: true,
        ..ContextPackInput::default()
    });
    clear_after_private_mirror_read_hook();
    let error = result.expect_err("an unstable private mirror must not return a context pack");
    assert_eq!(error.to_string(), "mirror refresh required");
    Ok(())
}

#[test]
fn canonical_lifecycle_rejects_runtime_targets_without_canonical_leaks() -> anyhow::Result<()> {
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
        let error = service
            .inspect_expiry(&record.id)
            .expect_err("ordinary expiry inspection must not expose private runtime history");
        assert!(error.to_string().contains("repository-only"));
        let stored = RuntimeRecords::new(&service.shared_conn)
            .get(&record.id)?
            .context("private runtime target disappeared after rejected canonical lifecycle")?;
        assert_eq!(stored.status, MemoryStatus::Active);
        assert_eq!(stored.destination, record.destination);
    }
    Ok(())
}

#[test]
fn quarantined_checkpoint_history_is_available_only_through_lifecycle_inspection()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let checkpoint = service.create_checkpoint(
        "agent:red-tests",
        CheckpointInput {
            task: "Quarantined checkpoint".to_owned(),
            note: "Private checkpoint history sentinel".to_owned(),
        },
    )?;
    service.shared_conn.execute(
        "UPDATE private_lifecycle_state
         SET quarantined = 1,
             quarantine_reason_code = 'owner_quarantine',
             quarantine_event_id = 'event-owner-quarantine'
         WHERE record_id = ?1",
        [&checkpoint.id],
    )?;

    assert!(
        service.inspect_checkpoint(&checkpoint.id).is_err(),
        "ordinary checkpoint inspection exposed quarantined history"
    );
    assert!(
        service.show_checkpoint(&checkpoint.id).is_err(),
        "ordinary checkpoint show exposed quarantined history"
    );
    assert!(
        service
            .checkpoint_for_owner_operation(&checkpoint.id)
            .is_err(),
        "owner command accessor exposed quarantined history"
    );
    assert!(
        service
            .list_checkpoints()?
            .iter()
            .all(|record| record.id != checkpoint.id),
        "ordinary checkpoint list exposed quarantined history"
    );

    let inspected = service.inspect_private_lifecycle_record(&checkpoint.id)?;
    assert_eq!(inspected.record.body, "Private checkpoint history sentinel");
    assert!(inspected.state.quarantined);
    Ok(())
}

#[test]
fn owner_checkpoint_commands_can_render_closed_history_but_not_superseded_history()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let closed = service.create_checkpoint(
        "agent:red-tests",
        CheckpointInput {
            task: "Closed owner command".to_owned(),
            note: "Closed history remains available to its exact owner command.".to_owned(),
        },
    )?;
    let closed_version = service.checkpoint_record_version(&closed.id)?;
    service.close_checkpoint(
        "agent:red-tests",
        CloseCheckpointCommand {
            operation_id: "close-owner-command-history".to_owned(),
            checkpoint_id: closed.id.clone(),
            expected_version: closed_version,
        },
    )?;
    assert!(service.inspect_checkpoint(&closed.id).is_err());
    assert_eq!(
        service.checkpoint_for_owner_operation(&closed.id)?.id,
        closed.id
    );

    let superseded = service.create_checkpoint(
        "agent:red-tests",
        CheckpointInput {
            task: "Superseded owner command".to_owned(),
            note: "Superseded history remains lifecycle-inspection-only.".to_owned(),
        },
    )?;
    service.shared_conn.execute(
        "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
        [&superseded.id],
    )?;
    assert!(
        service
            .checkpoint_for_owner_operation(&superseded.id)
            .is_err(),
        "owner command accessor exposed superseded history"
    );
    Ok(())
}

#[test]
fn caller_selected_private_ids_are_confined_to_trusted_recall_evaluation() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let fixture_id = "local-trusted-recall-eval-fixture";
    let input = LocalMemoryInput {
        memory_type: MemoryType::Preference,
        lane: MemoryLane::Semantic,
        title: "Trusted recall fixture".to_owned(),
        body: "Deterministic fixture identity is isolated from production writers.".to_owned(),
    };

    let error = service
        .create_local_memory_with_id_for_trusted_recall_eval(
            "memzoi-eval",
            fixture_id,
            input.clone(),
        )
        .expect_err("ordinary services must reject caller-selected private ids");
    assert!(error.to_string().contains("trusted recall evaluation"));
    assert!(
        RuntimeRecords::new(&service.shared_conn)
            .get(fixture_id)?
            .is_none()
    );
    drop(service);

    let trusted = MemoryService::open_paths_with_clock_for_trusted_recall_eval(
        paths,
        crate::FixedClock::from_rfc3339("2026-07-19T12:00:00Z")?,
    )?;
    let created = trusted.create_local_memory_with_id_for_trusted_recall_eval(
        "memzoi-eval",
        fixture_id,
        input.clone(),
    )?;
    assert_eq!(created.id, fixture_id);
    let error = trusted
        .create_local_memory_with_id_for_trusted_recall_eval("memzoi-eval", fixture_id, input)
        .expect_err("trusted fixture ids must remain collision-free");
    assert!(format!("{error:#}").contains("collides"));
    assert_eq!(
        RuntimeRecords::new(&trusted.shared_conn)
            .get(fixture_id)?
            .context("trusted fixture disappeared")?
            .id,
        fixture_id
    );
    Ok(())
}

#[test]
fn canonical_lifecycle_rejects_private_and_inactive_repo_targets() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let private_draft = sample_memory_draft(
        "Private repo target",
        "Private visibility must not be rewritten through a canonical lifecycle route.",
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
    assert!(
        private_error.to_string().contains("visibility private"),
        "{private_error:#}"
    );
    assert_eq!(fs::read(&private_path)?, private_before);
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&private.id)?
            .context("private record must remain indexed")?
            .status,
        MemoryStatus::Active
    );
    // Restore the deliberately stale test fixture before exercising a second
    // public lifecycle route, which now requires a current repository index.
    service.conn.execute(
        "UPDATE memory_record SET visibility = 'repo' WHERE id = ?1",
        [&private.id],
    )?;

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
    assert!(
        inactive_error
            .to_string()
            .contains("not a current assertion"),
        "{inactive_error:#}"
    );
    assert_eq!(fs::read(&active_path)?, active_before);
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&active.id)?
            .context("inactive record must remain indexed")?
            .status,
        MemoryStatus::Superseded
    );
    Ok(())
}

#[test]
fn direct_supersede_rejects_cross_scope_replacements_before_mutation() -> anyhow::Result<()> {
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
fn direct_supersede_rolls_back_db_and_files_when_second_install_fails() -> anyhow::Result<()> {
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

    assert_direct_supersede_unchanged(
        &service,
        &target,
        &target_path,
        &target_before,
        "second-write-rollback-replacement",
    )?;
    Ok(())
}

#[test]
fn direct_supersede_rolls_back_installed_files_when_db_commit_fails() -> anyhow::Result<()> {
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

    assert_direct_supersede_unchanged(
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

#[test]
fn session_end_runtime_records_and_events_commit_to_shared_authority() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let result = service.promote_session_end(
        "agent:shared-runtime-test",
        SessionEndDocument {
            task: "Preserve shared session-end runtime".to_owned(),
            candidates: vec![SessionEndCandidate {
                destination: MemoryDestination::Local,
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Shared session-end preference".to_owned(),
                body: "Session-end runtime writes and events belong in shared authority."
                    .to_owned(),
                sensitivity: OkfProposalSensitivity::LocalOnly,
                content_class: RepositoryContentClass::LocalOnlyState,
                reason: Some("private runtime continuity".to_owned()),
                scope: None,
                tags: Vec::new(),
            }],
        },
    )?;
    assert_eq!(
        result.candidates[0].status,
        SessionEndCandidateStatus::Written
    );
    let record_id = service.list_local_memory()?[0].id.clone();
    let shared_event_count: i64 = service.shared_conn.query_row(
        "SELECT COUNT(*) FROM event_log
         WHERE record_id = ?1 AND event_type = 'memory.local_created'",
        [&record_id],
        |row| row.get(0),
    )?;
    assert_eq!(shared_event_count, 1);
    let indexed_count: i64 = service.conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE id = ?1",
        [&record_id],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_count, 1);
    Ok(())
}

#[test]
fn materialization_create_installs_one_pinned_canonical_record() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate = materialization_candidate("materialized-create", "Materialized create")?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;

    let result = service.apply_repository_materialization(&plan, &decision, &candidate)?;

    assert_eq!(
        result.outputs[0].outcome,
        crate::MaterializationOutputOutcome::Written
    );
    let path = service.paths.records_dir().join("materialized-create.md");
    let markdown = fs::read_to_string(&path)?;
    let record = okf::parse_okf_record_markdown(service.paths.records_dir(), &path, &markdown)?
        .context("materialization create must write a canonical record")?;
    assert_eq!(record.concept_id, candidate.record.concept_id);
    assert_eq!(
        record.materialization.as_ref().map(|value| &value.plan_id),
        Some(&plan.plan_id)
    );
    Ok(())
}

#[test]
fn materialization_rolls_back_file_and_index_when_index_commit_fails() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate =
        materialization_candidate("materialized-commit-failure", "Failed materialization")?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;
    let path = service
        .paths
        .records_dir()
        .join("materialized-commit-failure.md");
    service.conn.execute_batch(
        "CREATE TABLE materialization_commit_parent (id INTEGER PRIMARY KEY);
         CREATE TABLE materialization_commit_guard (
             record_id TEXT NOT NULL,
             parent_id INTEGER NOT NULL,
             FOREIGN KEY (parent_id) REFERENCES materialization_commit_parent(id)
                 DEFERRABLE INITIALLY DEFERRED
         );
         CREATE TRIGGER materialization_commit_failure
         AFTER INSERT ON memory_record
         WHEN NEW.id = 'materialized-commit-failure'
         BEGIN
             INSERT INTO materialization_commit_guard (record_id, parent_id)
             VALUES (NEW.id, 1);
         END;",
    )?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("a failed derived-index commit must fail materialization");

    assert!(
        format!("{error:#}").contains("FOREIGN KEY constraint failed"),
        "unexpected materialization commit error: {error:#}"
    );
    assert!(
        !path.exists(),
        "a failed derived-index commit must roll back the canonical file"
    );
    let indexed: bool = service.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
        [&candidate.record.concept_id],
        |row| row.get(0),
    )?;
    assert!(
        !indexed,
        "a failed derived-index commit must roll back its row"
    );
    assert!(service.repo_index_drift()?.is_current());
    Ok(())
}

#[test]
fn deleting_materialized_worktree_bytes_removes_them_on_rebuild_without_hidden_state()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let paths = service.paths.clone();
    let candidate = materialization_candidate("materialized-deleted", "Deleted materialization")?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;
    service.apply_repository_materialization(&plan, &decision, &candidate)?;
    let path = paths.records_dir().join("materialized-deleted.md");
    assert!(path.exists());

    assert_eq!(
        service
            .search_memory(SearchInput {
                query: "Deleted materialization".to_owned(),
                limit: 10,
                ..SearchInput::default()
            })?
            .len(),
        1,
        "a completed materialization must be locally active before rebuild"
    );
    fs::remove_file(&path)?;
    let stale_error = service
        .search_memory(SearchInput {
            query: "Deleted materialization".to_owned(),
            limit: 10,
            ..SearchInput::default()
        })
        .expect_err("deleted canonical bytes must make repository reads unavailable until rebuild");
    assert!(format!("{stale_error:#}").contains("repository derived index is stale"));

    drop(service);
    let rebuilt = MemoryService::rebuild_paths(paths.clone())?;
    assert!(rebuilt.record_ids.is_empty());
    let reopened = MemoryService::open_paths(paths)?;
    assert!(
        reopened
            .search_memory(SearchInput {
                query: "Deleted materialization".to_owned(),
                limit: 10,
                ..SearchInput::default()
            })?
            .is_empty(),
        "rebuild must not restore deleted materialized bytes from hidden approval state"
    );
    Ok(())
}

#[test]
fn materialization_update_refuses_stale_prior_revision_without_writing() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let original = materialization_candidate("materialized-stale", "Original materialized record")?;
    let (create_plan, create_decision) = materialization_plan_and_decision(&original)?;
    service.apply_repository_materialization(&create_plan, &create_decision, &original)?;
    let path = service.paths.records_dir().join("materialized-stale.md");
    let before = fs::read(&path)?;

    let mut replacement_record = original.record.clone();
    replacement_record.draft.body =
        "Changed bytes with a deliberately stale prior revision.".to_owned();
    let stale_prior = CanonicalRevision {
        schema: crate::CANONICAL_REVISION_SCHEMA.to_owned(),
        revision_hash: format!("blake3:{}", "f".repeat(64)),
    };
    let replacement = build_repository_materialization_candidate(
        replacement_record,
        crate::MaterializationAction::Update,
        ExpectedPriorRevision::Revision(stale_prior),
        None,
        None,
    )?;
    let (update_plan, update_decision) = materialization_plan_and_decision(&replacement)?;

    let error = service
        .apply_repository_materialization(&update_plan, &update_decision, &replacement)
        .expect_err("stale updates must not install canonical bytes");

    assert!(format!("{error:#}").contains("materialization_update_stale"));
    assert_eq!(fs::read(path)?, before);
    Ok(())
}

#[test]
fn materialization_retries_identical_final_bytes_idempotently() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate =
        materialization_candidate("materialized-idempotent", "Idempotent materialization")?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;
    service.apply_repository_materialization(&plan, &decision, &candidate)?;

    let retry = service.apply_repository_materialization(&plan, &decision, &candidate)?;

    assert_eq!(
        retry.outputs[0].outcome,
        crate::MaterializationOutputOutcome::AlreadyCurrent
    );
    Ok(())
}

#[test]
fn materialization_rejects_unsafe_local_and_private_candidates() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let valid = materialization_candidate("materialized-local", "Local materialization")?;
    let (plan, decision) = materialization_plan_and_decision(&valid)?;
    let mut local = valid.clone();
    local.record.draft.scope_kind = ScopeKind::Personal;
    let mut unsafe_candidate = valid.clone();
    unsafe_candidate.record.draft.sensitivity = crate::OkfProposalSensitivity::Sensitive;
    let mut private = valid.clone();
    private.record.draft.visibility = Visibility::Private;

    for candidate in [local, unsafe_candidate, private] {
        let error = service
            .apply_repository_materialization(&plan, &decision, &candidate)
            .expect_err("non-repository candidates must fail before install");
        assert!(
            format!("{error:#}").contains("repository materialization candidate"),
            "unexpected materialization candidate error: {error:#}"
        );
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{}.md", candidate.record.concept_id))
                .exists()
        );
    }
    Ok(())
}

#[test]
fn materialization_rejects_malformed_capture_before_writing() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let mut record =
        materialization_candidate_record("materialized-malformed-capture", "Malformed capture");
    record.capture = Some(materialization_capture());
    let mut candidate = build_repository_materialization_candidate(
        record,
        crate::MaterializationAction::Create,
        ExpectedPriorRevision::Absent,
        None,
        None,
    )?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;
    candidate
        .record
        .capture
        .as_mut()
        .expect("valid materialization fixture has capture provenance")
        .confidence = "not-a-number".to_owned();
    let path = service
        .paths
        .records_dir()
        .join("materialized-malformed-capture.md");

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("malformed capture provenance must fail before a canonical write");

    assert!(
        format!("{error:#}").contains("capture provenance confidence is invalid"),
        "unexpected malformed capture error: {error:#}"
    );
    assert!(
        !path.exists(),
        "malformed capture provenance wrote a canonical record before failing"
    );
    Ok(())
}

#[test]
fn materialization_rejects_a_plan_not_derived_from_its_direct_candidate() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate =
        materialization_candidate("materialized-candidate-plan", "Candidate plan mismatch")?;
    let (derived_plan, _) = materialization_plan_and_decision(&candidate)?;
    let plan = build_repository_materialization_plan(
        format!("blake3:{}", "a".repeat(64)),
        derived_plan.outputs,
    )?;
    let decision = build_repository_materialization_decision(
        &plan,
        "2026-07-16T00:00:00Z".to_owned(),
        repository_materialization_policy(),
        crate::MaterializationAuthorizationCapability::ExplicitCli,
    )?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("a direct candidate must pin its derived plan");

    assert!(format!("{error:#}").contains("materialization_candidate_plan_mismatch"));
    Ok(())
}

#[test]
fn materialization_rejects_a_decision_pinned_to_another_plan() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate = materialization_candidate("materialized-decision", "Decision mismatch")?;
    let (plan, _) = materialization_plan_and_decision(&candidate)?;
    let alternate_plan = build_repository_materialization_plan(
        format!("blake3:{}", "e".repeat(64)),
        plan.outputs.clone(),
    )?;
    let decision = build_repository_materialization_decision(
        &alternate_plan,
        "2026-07-16T00:00:00Z".to_owned(),
        repository_materialization_policy(),
        crate::MaterializationAuthorizationCapability::ExplicitCli,
    )?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("a decision for another plan must not authorize materialization");

    assert!(format!("{error:#}").contains("materialization_plan_decision_identity_mismatch"));
    assert!(
        !service
            .paths
            .records_dir()
            .join("materialized-decision.md")
            .exists()
    );
    Ok(())
}

#[test]
fn materialization_rejects_a_decision_with_a_noncanonical_policy() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let candidate = materialization_candidate("materialized-policy", "Policy mismatch")?;
    let (plan, _) = materialization_plan_and_decision(&candidate)?;
    let decision = build_repository_materialization_decision(
        &plan,
        "2026-07-16T00:00:00Z".to_owned(),
        crate::MaterializationPolicy {
            policy_id: "untrusted-policy".to_owned(),
            safety_contract: "untrusted-contract".to_owned(),
        },
        crate::MaterializationAuthorizationCapability::ExplicitCli,
    )?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("a caller-provided policy must not authorize materialization");

    assert!(format!("{error:#}").contains("materialization_policy_mismatch"));
    assert!(
        !service
            .paths
            .records_dir()
            .join("materialized-policy.md")
            .exists()
    );
    Ok(())
}

#[test]
fn materialization_rejects_git_ignored_targets() -> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    fs::write(
        service.paths.project_root.join(".gitignore"),
        ".memzoi/records/*.md\n",
    )?;
    let candidate = materialization_candidate("materialized-ignored", "Ignored materialization")?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("ignored materialization targets must fail closed");

    assert!(format!("{error:#}").contains("materialization_git_review_visibility_required"));
    assert!(
        !service
            .paths
            .records_dir()
            .join("materialized-ignored.md")
            .exists()
    );
    Ok(())
}

#[test]
fn materialization_requires_a_multi_record_transaction_for_lifecycle_actions() -> anyhow::Result<()>
{
    let (_temp, service) = initialized_git_service()?;
    let transaction_artifacts_before = crate::lifecycle_transaction_artifact_count(&service.paths)?;
    let revision = CanonicalRevision {
        schema: crate::CANONICAL_REVISION_SCHEMA.to_owned(),
        revision_hash: format!("blake3:{}", "f".repeat(64)),
    };
    let target = MaterializationTarget {
        record_id: "prior-record".to_owned(),
        expected_revision: revision,
    };
    let mut record =
        materialization_candidate_record("materialized-supersede", "Unsupported lifecycle");
    record.supersedes_id = Some(target.record_id.clone());
    let candidate = build_repository_materialization_candidate(
        record,
        crate::MaterializationAction::Supersede,
        ExpectedPriorRevision::Absent,
        Some(target),
        Some("requires a paired lifecycle transaction".to_owned()),
    )?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;

    let error = service
        .apply_repository_materialization(&plan, &decision, &candidate)
        .expect_err("single-record lifecycle materialization must fail before writes");

    assert!(format!("{error:#}").contains("multi_record_transaction_required"));
    assert!(
        !service
            .paths
            .records_dir()
            .join("materialized-supersede.md")
            .exists()
    );
    assert_eq!(
        crate::lifecycle_transaction_artifact_count(&service.paths)?,
        transaction_artifacts_before
    );
    Ok(())
}

fn initialized_git_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    let output = Command::new("git")
        .args(["init", "-q"])
        .arg(&project_root)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to initialize materialization Git test repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    let service = MemoryService::open_paths(paths)?;
    Ok((temp, service))
}

fn materialization_candidate_record(
    concept_id: &str,
    title: &str,
) -> RepositoryMaterializationCandidateRecord {
    RepositoryMaterializationCandidateRecord {
        concept_id: concept_id.to_owned(),
        draft: sample_memory_draft(title, "A compact shared canonical record."),
        status: MemoryStatus::Active,
        applies_to: Vec::new(),
        created: "2026-07-16T00:00:00Z".to_owned(),
        updated: None,
        supersedes_id: None,
        retention: crate::retention_facts_for_creation(
            MemoryLane::Semantic,
            "2026-07-16T00:00:00Z",
            None,
            None,
        )
        .expect("valid test retention"),
        origin: crate::OriginDescriptor::new(
            format!("repository-materialization:test:{concept_id}"),
            crate::OriginRoute::RepositoryMaterialization,
        ),
        lineage: None,
        proposal_id: None,
        capture: None,
    }
}

fn materialization_capture() -> crate::CaptureProvenance {
    crate::CaptureProvenance {
        schema: crate::CAPTURE_PROVENANCE_SCHEMA.to_owned(),
        plan_id: "capture-plan".to_owned(),
        review_id: "capture-review".to_owned(),
        claim_id: "capture-claim".to_owned(),
        reviewed_claim_id: "reviewed-capture-claim".to_owned(),
        candidate_id: "capture-candidate".to_owned(),
        reviewed_candidate_id: "reviewed-capture-candidate".to_owned(),
        extraction: crate::CaptureExtractorIdentity {
            kind: "markdown".to_owned(),
            id: "markdown".to_owned(),
            implementation_digest: format!("blake3:{}", "1".repeat(64)),
        },
        evidence: vec![crate::CaptureEvidence {
            source_id: "source-1".to_owned(),
            locator: crate::CaptureSourceLocator::ProjectPath {
                path: "notes.md".to_owned(),
            },
            source_content_hash: format!("blake3:{}", "2".repeat(64)),
            span: crate::CaptureEvidenceSpan {
                byte_start: 0,
                byte_end: 12,
                line_start: 1,
                line_end: 1,
            },
            evidence_content_hash: format!("blake3:{}", "3".repeat(64)),
            text: None,
            heading_path: vec!["Capture".to_owned()],
            section_kind: "fact".to_owned(),
            semantic_location: None,
        }],
        confidence: "0.82".to_owned(),
        classification: crate::CaptureClassification {
            destination: crate::MemoryDestination::Repo,
            destination_reason: "repository-safe evidence".to_owned(),
            sensitivity: crate::OkfProposalSensitivity::RepoSafe,
            sensitivity_reason: "reviewed".to_owned(),
            content_class: crate::RepositoryContentClass::GeneralRepoKnowledge,
            policy: crate::MemoryDestination::Repo.policy(),
        },
        destination: crate::MemoryDestination::Repo,
        sensitivity: crate::OkfProposalSensitivity::RepoSafe,
        review_outcome: crate::CaptureReviewOutcome::Accept,
        review_reason_code: None,
        reviewed_by: "reviewer".to_owned(),
        reviewed_at: "2026-07-16T12:00:00Z".to_owned(),
        routed_by: "test".to_owned(),
    }
}

fn materialization_candidate(
    concept_id: &str,
    title: &str,
) -> anyhow::Result<RepositoryMaterializationCandidate> {
    build_repository_materialization_candidate(
        materialization_candidate_record(concept_id, title),
        crate::MaterializationAction::Create,
        ExpectedPriorRevision::Absent,
        None,
        None,
    )
}

fn materialization_plan_and_decision(
    candidate: &RepositoryMaterializationCandidate,
) -> anyhow::Result<(
    crate::RepositoryMaterializationPlan,
    crate::RepositoryMaterializationDecision,
)> {
    let plan = repository_materialization_candidate_plan(candidate)?;
    let decision = build_repository_materialization_decision(
        &plan,
        "2026-07-16T00:00:00Z".to_owned(),
        repository_materialization_policy(),
        crate::MaterializationAuthorizationCapability::ExplicitCli,
    )?;
    Ok((plan, decision))
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

fn assert_direct_supersede_unchanged(
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
