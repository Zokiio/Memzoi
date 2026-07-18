use std::{
    fs,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
};

use tempfile::TempDir;

use super::*;

#[test]
fn open_recovers_installed_checkpoint_proposal_before_origin_finalization() -> Result<()> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    fs::create_dir(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    super::super::MemoryService::initialize_paths(
        paths.clone(),
        super::super::InitRequest { force: false },
    )?;
    let clock = crate::FixedClock::from_rfc3339("2026-07-18T10:00:00Z")?;
    let service = super::super::MemoryService::open_paths_with_clock(paths.clone(), clock)?;
    let checkpoint_body = r#"task: Recover checkpoint promotion
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Recovered checkpoint proposal
    body: The exact installed proposal must finalize through journal recovery.
    sensitivity: repo-safe
    content_class: general_repo_knowledge
"#;
    let created = service.create_checkpoint_command(
        "agent:test",
        super::super::CreateCheckpointCommand {
            operation_id: "create-recovery-source".to_owned(),
            input: CheckpointInput {
                task: "Recovery source".to_owned(),
                note: checkpoint_body.to_owned(),
            },
        },
    )?;
    let document = crate::parse_session_end_document(checkpoint_body)?;
    let command = SessionEndFromCheckpointCommand {
        operation_id: "recover-installed-session-end".to_owned(),
        checkpoint_id: created.checkpoint_id.clone(),
        expected_version: created.record_version.clone(),
        document: document.clone(),
    };

    inject_after_repository_install_hook(|| panic!("simulated process interruption"));
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        service.promote_session_end_from_checkpoint("agent:test", command.clone())
    }));
    assert!(interrupted.is_err());
    assert_eq!(read_pending(&paths)?.len(), 1);
    drop(service);

    let recovered = super::super::MemoryService::open_paths_with_clock(
        paths.clone(),
        crate::FixedClock::from_rfc3339("2026-07-19T10:00:00Z")?,
    )?;
    assert!(read_pending(&paths)?.is_empty());
    assert!(
        recovered
            .list_checkpoints()?
            .iter()
            .all(|record| record.id != created.checkpoint_id)
    );
    let replayed = recovered.promote_session_end_from_checkpoint("agent:test", command)?;
    assert!(
        replayed
            .closure
            .as_ref()
            .is_some_and(|value| value.replayed)
    );
    assert_eq!(
        replayed.promotion.candidates[0]
            .write
            .as_ref()
            .and_then(|write| match write {
                SessionEndWrite::ProposalFile { proposal_id, .. } => Some(proposal_id.as_str()),
                SessionEndWrite::RuntimeRecord { .. } => None,
            }),
        Some("mem_session_recovered-checkpoint-proposal")
    );
    Ok(())
}

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
        None,
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

    let error =
        cleanup_authorized_session_end_proposals(&paths, &authorization, &projections, &created)
            .expect_err("cleanup must not delete bytes not authorized by session-end");

    assert!(format!("{error:#}").contains("does not match"));
    assert_eq!(std::fs::read(&destination)?, replacement);
    Ok(())
}
