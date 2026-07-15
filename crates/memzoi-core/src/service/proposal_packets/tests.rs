use super::super::safe_files::RepoLifecycleLock;
use super::*;
use crate::{
    ImportCandidateInput, MemoryLane, MemoryType, OkfProposalSensitivity, OkfProposalSource,
    SessionEndCandidate, SessionEndWrite,
};
use tempfile::TempDir;

#[test]
fn repo_packet_writers_share_the_lifecycle_lock() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let _first = RepoLifecycleLock::acquire(&service.paths)?;

    let session_error = service
        .promote_session_end(
            "agent:red-tests",
            repo_session_document("Locked session packet"),
        )
        .expect_err("session-end repo writes must contend on the lifecycle lock");
    assert!(
        session_error
            .to_string()
            .contains("another repo lifecycle operation is in progress")
    );

    let import_error = service
        .apply_import(
            "agent:red-tests",
            repo_import_document("Locked import packet"),
            "unused-plan-id",
        )
        .expect_err("import repo writes must contend on the lifecycle lock");
    assert!(
        import_error
            .to_string()
            .contains("another repo lifecycle operation is in progress")
    );
    assert!(
        scan_file_proposal_inventory(&service.paths)?
            .pending
            .is_empty()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn repo_packet_writers_refuse_symlinked_pending_root() -> anyhow::Result<()> {
    for writer in ["session-end", "import"] {
        let (_temp, service) = initialized_service()?;
        let outside = TempDir::new()?;
        fs::create_dir_all(service.paths.proposals_dir())?;
        std::os::unix::fs::symlink(
            outside.path(),
            service.paths.proposals_dir().join("pending"),
        )?;

        let error = match writer {
            "session-end" => service
                .promote_session_end(
                    "agent:red-tests",
                    repo_session_document("Escaped session packet"),
                )
                .expect_err("session-end must refuse the symlinked pending root"),
            "import" => service
                .apply_import(
                    "agent:red-tests",
                    repo_import_document("Escaped import packet"),
                    "unused-plan-id",
                )
                .expect_err("import must refuse the symlinked pending root"),
            _ => unreachable!(),
        };
        assert!(
            format!("{error:#}").contains("ancestor must be a real directory"),
            "unexpected {writer} containment error: {error:#}"
        );
        assert_eq!(fs::read_dir(outside.path())?.count(), 0);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_readers_refuse_symlinked_records_root_before_reading_outside() -> anyhow::Result<()> {
    for operation in ["doctor", "rebuild"] {
        let (_temp, service) = initialized_service()?;
        let outside = TempDir::new()?;
        let sentinel = "OUTSIDE-CANONICAL-CONTENT-SENTINEL";
        fs::write(outside.path().join("outside.md"), sentinel)?;
        fs::remove_dir_all(service.paths.records_dir())?;
        std::os::unix::fs::symlink(outside.path(), service.paths.records_dir())?;

        let error = match operation {
            "doctor" => service
                .repo_index_drift()
                .expect_err("index drift must refuse a symlinked canonical root"),
            "rebuild" => service
                .rebuild()
                .expect_err("rebuild must refuse a symlinked canonical root"),
            _ => unreachable!(),
        };
        let rendered = format!("{error:#}");
        assert!(rendered.contains("must be a real directory"), "{rendered}");
        assert!(!rendered.contains(sentinel), "{rendered}");
    }
    Ok(())
}

#[test]
fn repo_packet_planning_reserves_metadata_identities_from_resolved_packets() -> anyhow::Result<()> {
    let (_session_temp, session_service) = initialized_service()?;
    let session_pending = write_test_pending_proposal_with_id(
        &session_service,
        "mem_session_reused-title",
        "Previously reviewed session packet",
        OkfProposalSensitivity::RepoSafe,
    )?;
    let session_renamed = session_pending.with_file_name("different-session-file.md");
    fs::rename(&session_pending, &session_renamed)?;
    session_service.reject_file_proposal(
        &session_renamed,
        "reviewer:human",
        "Terminal identity fixture",
    )?;
    let session = session_service
        .promote_session_end("agent:red-tests", repo_session_document("Reused title"))?;
    assert!(matches!(
        session.candidates[0].write.as_ref(),
        Some(SessionEndWrite::ProposalFile { proposal_id, .. })
            if proposal_id == "mem_session_reused-title-2"
    ));

    let (_import_temp, import_service) = initialized_service()?;
    let import_pending = write_test_pending_proposal_with_id(
        &import_service,
        "mem_import_reused-title",
        "Previously reviewed import packet",
        OkfProposalSensitivity::RepoSafe,
    )?;
    let import_renamed = import_pending.with_file_name("different-import-file.md");
    fs::rename(&import_pending, &import_renamed)?;
    import_service.reject_file_proposal(
        &import_renamed,
        "reviewer:human",
        "Terminal identity fixture",
    )?;
    let document = repo_import_document("Reused title");
    let plan = import_service.plan_import("agent:red-tests", document.clone())?;
    assert!(matches!(
        &plan.candidates[0].action,
        crate::ImportCandidateAction::CreateProposal { proposal_id, .. }
            if proposal_id == "mem_import_reused-title-2"
    ));
    let applied = import_service.apply_import("agent:red-tests", document, &plan.plan_id)?;
    assert!(matches!(
        &applied.writes[0],
        crate::ImportWrite::ProposalFile { proposal_id, .. }
            if proposal_id == "mem_import_reused-title-2"
    ));
    Ok(())
}

#[test]
fn repo_packet_planning_reserves_hash_only_receipt_aliases_and_database_proposal_ids()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let raw_id = "mem_session_hash-reserved";
    let pending = write_test_pending_proposal_with_id(
        &service,
        raw_id,
        "Previously rejected unsafe packet",
        OkfProposalSensitivity::Secret,
    )?;
    service.reject_file_proposal(
        &pending,
        "reviewer:human",
        "Hash-only terminal identity fixture",
    )?;

    let session =
        service.promote_session_end("agent:red-tests", repo_session_document("Hash reserved"))?;
    assert!(matches!(
        session.candidates[0].write.as_ref(),
        Some(SessionEndWrite::ProposalFile { proposal_id, .. })
            if proposal_id == "mem_session_hash-reserved-2"
    ));

    service.conn.execute(
        "INSERT INTO proposal (id, operation, payload_json, status, actor)
         VALUES ('mem_import_db-reserved', 'create', '{}', 'pending', 'agent:red-tests')",
        [],
    )?;
    let plan = service.plan_import("agent:red-tests", repo_import_document("Db reserved"))?;
    assert!(matches!(
        &plan.candidates[0].action,
        crate::ImportCandidateAction::CreateProposal { proposal_id, .. }
            if proposal_id == "mem_import_db-reserved-2"
    ));
    Ok(())
}

#[test]
fn file_create_refuses_to_replace_a_local_runtime_row() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record_id = "runtime-collision-create";
    insert_runtime_record_with_id(&service, record_id, MemoryDestination::Local)?;
    let pending = write_test_pending_proposal_with_id(
        &service,
        "mem_runtime_collision_create",
        "Runtime collision create",
        OkfProposalSensitivity::RepoSafe,
    )?;

    let error = service
        .apply_file_proposal(&pending, "agent:applier")
        .expect_err("file apply must not replace local memory");
    assert!(error.to_string().contains("owned by non-repo memory"));
    assert!(pending.is_file());
    assert!(
        !service
            .paths
            .records_dir()
            .join(format!("{record_id}.md"))
            .exists()
    );
    let runtime = RuntimeRecords::new(&service.conn)
        .get(record_id)?
        .context("runtime row survived")?;
    assert_eq!(runtime.destination, MemoryDestination::Local);
    assert_eq!(runtime.body, "Runtime collision sentinel");
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn file_supersede_runtime_collision_rolls_back_target_and_replacement() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft("Supersede collision target", "Original target body"),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let target_before = fs::read(&target_path)?;
    let replacement_id = "supersede-collision-replacement";
    insert_runtime_record_with_id(&service, replacement_id, MemoryDestination::Session)?;
    let pending = write_test_supersede_proposal(
        &service,
        "mem_supersede_runtime_collision",
        "Supersede collision replacement",
        &target.id,
    )?;

    let error = service
        .apply_file_proposal(&pending, "agent:applier")
        .expect_err("supersede must not replace session memory");
    assert!(error.to_string().contains("owned by non-repo memory"));
    assert_eq!(fs::read(&target_path)?, target_before);
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&target.id)?
            .context("target row survived")?
            .status,
        MemoryStatus::Active
    );
    let runtime = RuntimeRecords::new(&service.conn)
        .get(replacement_id)?
        .context("session collision row survived")?;
    assert_eq!(runtime.destination, MemoryDestination::Session);
    assert!(pending.is_file());
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn file_apply_refuses_database_proposal_identity_collision() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let proposal_id = "mem_db_proposal_collision";
    service.conn.execute(
        "INSERT INTO proposal (id, operation, payload_json, status, actor)
         VALUES (?1, 'create', '{}', 'pending', 'agent:red-tests')",
        [proposal_id],
    )?;
    let pending = write_test_pending_proposal_with_id(
        &service,
        proposal_id,
        "Database proposal collision",
        OkfProposalSensitivity::RepoSafe,
    )?;

    let error = service
        .apply_file_proposal(&pending, "agent:applier")
        .expect_err("ambiguous proposal lineage must be rejected");
    assert!(
        error
            .to_string()
            .contains("conflicts with a database proposal")
    );
    assert!(!error.to_string().contains(proposal_id));
    assert!(pending.is_file());
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn apply_revalidates_pending_bytes_immediately_before_resolution() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = write_test_pending_proposal(
        &service,
        "Pending apply race",
        OkfProposalSensitivity::RepoSafe,
    )?;
    let mutated = b"concurrent human edit";

    let error = service
        .apply_file_proposal_with_hooks(
            &pending,
            "agent:applier",
            |path| fs::write(path, mutated).map_err(Into::into),
            |_| Ok(()),
        )
        .expect_err("changed pending bytes must abort apply");
    assert!(error.to_string().contains("changed after validation"));
    assert_eq!(fs::read(&pending)?, mutated);
    assert!(
        !service
            .paths
            .records_dir()
            .join("pending-apply-race.md")
            .exists()
    );
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn unsafe_reject_revalidates_pending_bytes_without_leaking_raw_identity() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let original = write_test_pending_proposal_with_id(
        &service,
        "mem_unsafe_race",
        "Unsafe race packet",
        OkfProposalSensitivity::Secret,
    )?;
    let raw_id = "SECRET-RAW-ID-SENTINEL";
    let raw_file = service
        .paths
        .proposals_dir()
        .join("pending/SECRET-RAW-FILE-SENTINEL.md");
    let markdown = fs::read_to_string(&original)?
        .replace("id: mem_unsafe_race", &format!("id: {raw_id}"))
        .replace(
            "Lifecycle finalization must never hide cleanup failures.",
            "SECRET-BODY-SENTINEL",
        );
    fs::write(&raw_file, markdown)?;
    fs::remove_file(&original)?;
    let mutated = b"SECRET-CONCURRENT-EDIT-SENTINEL";

    let error = service
        .reject_file_proposal_with_hooks(
            &raw_file,
            "reviewer:human",
            "Reject unsafe race",
            |path| fs::write(path, mutated).map_err(Into::into),
            |_| Ok(()),
        )
        .expect_err("changed unsafe pending bytes must abort rejection");
    let rendered = format!("{error:#}");
    assert!(rendered.contains("changed after validation"), "{rendered}");
    for forbidden in [
        raw_id,
        "SECRET-RAW-FILE-SENTINEL",
        "SECRET-BODY-SENTINEL",
        "SECRET-CONCURRENT-EDIT-SENTINEL",
    ] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }
    assert_eq!(fs::read(&raw_file)?, mutated);
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn unsafe_reject_reports_original_contextual_classification() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = write_test_pending_proposal_with_content_class(
        &service,
        "Unsafe contextual classification",
        OkfProposalSensitivity::Secret,
        RepositoryContentClass::RawTranscript,
    )?;

    let result = service.reject_file_proposal(
        &pending,
        "reviewer:human",
        "Reject unsafe contextual classification",
    )?;

    assert_eq!(result.proposal.sensitivity, OkfProposalSensitivity::Secret);
    assert_eq!(
        result.proposal.content_class,
        RepositoryContentClass::RawTranscript
    );
    Ok(())
}

#[test]
fn overwrite_install_never_replaces_a_file_recreated_after_backup() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft("Backup race target", "Original target body"),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let mut replacement = target.clone();
    replacement.status = MemoryStatus::Superseded;
    replacement.updated_at = "2026-07-10T12:00:00Z".to_owned();
    let session = CanonicalWriteSession::begin(&service.paths)?;
    let write = service.prepare_record_file_write_with_conn(
        &session,
        &service.conn,
        &replacement,
        FileWriteMode::Overwrite,
    )?;
    let tx = service.conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
        [&target.id],
    )?;
    let projections = canonical_write_projections(&service.paths, std::slice::from_ref(&write))?;
    let values = memory_draft_safety_values("replacement", &write.record_file.draft);
    let authorization = authorize_repository_projection_batch(
        &service.paths,
        RepositoryWriteRoute::Supersede,
        OkfProposalSensitivity::RepoSafe,
        write.record_file.draft.scope_kind,
        write.record_file.draft.scope_id.as_deref(),
        write.record_file.draft.visibility,
        AuthorizationProof::LifecycleOperation {
            target_id: &target.id,
        },
        explicit_repository_provenance(write.record_file.draft.content_class, &target.id),
        &values,
        &projections,
    )?;

    let error = session
        .commit_with_backup_hook(
            RepositoryWriteRoute::Supersede,
            &authorization,
            tx,
            &[write],
            |_| Ok(()),
            |_, path| fs::write(path, "fresh editor bytes").map_err(Into::into),
            |_| Ok(()),
        )
        .expect_err("no-replace overwrite must refuse the recreated target");
    assert!(format!("{error:#}").contains("without replacement"));
    assert_eq!(fs::read_to_string(&target_path)?, "fresh editor bytes");
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&target.id)?
            .context("target row survived")?
            .status,
        MemoryStatus::Active
    );
    let artifacts = lifecycle_transaction_artifacts(&service.paths)?;
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    assert!(
        artifacts[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".canonical.tmp"))
    );
    Ok(())
}

#[test]
fn canonical_rollback_preserves_an_identical_recreated_install() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let target = apply_test_record(
        &service,
        sample_memory_draft("Canonical rollback identity", "Original target body"),
    )?;
    let target_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", target.id));
    let mut replacement = target.clone();
    replacement.status = MemoryStatus::Superseded;
    replacement.updated_at = "2026-07-10T12:00:00Z".to_owned();
    let session = CanonicalWriteSession::begin(&service.paths)?;
    let write = service.prepare_record_file_write_with_conn(
        &session,
        &service.conn,
        &replacement,
        FileWriteMode::Overwrite,
    )?;
    let replacement_bytes = write.markdown.as_bytes().to_vec();
    let tx = service.conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
        [&target.id],
    )?;
    let projections = canonical_write_projections(&service.paths, std::slice::from_ref(&write))?;
    let values = memory_draft_safety_values("replacement", &write.record_file.draft);
    let authorization = authorize_repository_projection_batch(
        &service.paths,
        RepositoryWriteRoute::Supersede,
        OkfProposalSensitivity::RepoSafe,
        write.record_file.draft.scope_kind,
        write.record_file.draft.scope_id.as_deref(),
        write.record_file.draft.visibility,
        AuthorizationProof::LifecycleOperation {
            target_id: &target.id,
        },
        explicit_repository_provenance(write.record_file.draft.content_class, &target.id),
        &values,
        &projections,
    )?;
    let recreated_path = target_path.clone();
    let recreated_bytes = replacement_bytes.clone();

    let error = session
        .commit_with_hooks(
            RepositoryWriteRoute::Supersede,
            &authorization,
            tx,
            &[write],
            |_| Ok(()),
            move |_| {
                fs::remove_file(&recreated_path)?;
                fs::write(&recreated_path, &recreated_bytes)?;
                Err(anyhow::anyhow!("injected pre-commit failure"))
            },
        )
        .expect_err("rollback must not delete an identical file recreated by another owner");

    assert!(format!("{error:#}").contains("pre-commit"));
    assert_eq!(fs::read(&target_path)?, replacement_bytes);
    assert_eq!(
        RuntimeRecords::new(&service.conn)
            .get(&target.id)?
            .context("target row survived")?
            .status,
        MemoryStatus::Active
    );
    Ok(())
}

#[test]
fn rejection_finalization_failure_restores_raw_pending_packet() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = write_test_pending_proposal(
        &service,
        "Rejected cleanup packet",
        OkfProposalSensitivity::Secret,
    )?;
    let original = fs::read(&pending)?;

    let error = service
        .reject_file_proposal_with_finalize_hook(
            &pending,
            "reviewer:human",
            "Unsafe content must remain pending when cleanup fails.",
            |_| Err(anyhow::anyhow!("injected raw-backup cleanup failure")),
        )
        .expect_err("rejection cleanup failure must be observable");
    assert!(error.to_string().contains("finalization was interrupted"));
    assert_eq!(fs::read(&pending)?, original);
    assert!(
        !service
            .paths
            .proposals_dir()
            .join("resolved/rejected/rejected-cleanup-packet.md")
            .exists()
    );
    assert!(lifecycle_transaction_artifacts(&service.paths)?.is_empty());
    Ok(())
}

#[test]
fn rejection_rollback_preserves_an_identical_recreated_receipt() -> anyhow::Result<()> {
    use std::{cell::RefCell, rc::Rc};

    let (_temp, service) = initialized_service()?;
    let pending = write_test_pending_proposal(
        &service,
        "Rejected identical replacement",
        OkfProposalSensitivity::RepoSafe,
    )?;
    let snapshot = service.load_pending_file_proposal_snapshot(&pending)?;
    let resolved_path = service
        .paths
        .proposals_dir()
        .join("resolved/rejected")
        .join(format!("{}.md", snapshot.proposal.file_id));
    let recreated_path = resolved_path.clone();
    let recreated_bytes = Rc::new(RefCell::new(Vec::new()));
    let observed_bytes = Rc::clone(&recreated_bytes);

    let error = service
        .reject_file_proposal_with_finalize_hook(
            &pending,
            "reviewer:human",
            "Exercise exact rollback ownership",
            move |_| {
                let bytes = fs::read(&recreated_path)?;
                fs::remove_file(&recreated_path)?;
                fs::write(&recreated_path, &bytes)?;
                *observed_bytes.borrow_mut() = bytes;
                Err(anyhow::anyhow!("injected rejection finalization failure"))
            },
        )
        .expect_err("rollback must preserve an identical receipt recreated by another owner");

    assert!(format!("{error:#}").contains("finalization"));
    assert!(pending.exists());
    assert_eq!(fs::read(&resolved_path)?, *recreated_bytes.borrow());
    Ok(())
}

#[test]
fn unsafe_rejection_cleanup_failure_never_exposes_raw_backup_path() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let original = write_test_pending_proposal_with_id(
        &service,
        "mem_cleanup_leak",
        "Unsafe cleanup leak packet",
        OkfProposalSensitivity::Secret,
    )?;
    let raw_id = "SECRET-CLEANUP-ID-SENTINEL";
    let raw_file = service
        .paths
        .proposals_dir()
        .join("pending/SECRET-CLEANUP-FILE-SENTINEL.md");
    let markdown = fs::read_to_string(&original)?
        .replace("id: mem_cleanup_leak", &format!("id: {raw_id}"))
        .replace(
            "Lifecycle finalization must never hide cleanup failures.",
            "SECRET-CLEANUP-BODY-SENTINEL",
        );
    fs::write(&raw_file, markdown)?;
    fs::remove_file(&original)?;

    let error = service
        .reject_file_proposal_with_finalize_hook(
            &raw_file,
            "reviewer:human",
            "Exercise cleanup diagnostics",
            |backup| {
                fs::remove_file(backup)?;
                fs::create_dir(backup)?;
                Ok(())
            },
        )
        .expect_err("backup cleanup failure must be reported");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("rejection cleanup rollback"),
        "{rendered}"
    );
    for forbidden in [
        raw_id,
        "SECRET-CLEANUP-FILE-SENTINEL",
        "SECRET-CLEANUP-BODY-SENTINEL",
    ] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }
    Ok(())
}

#[test]
fn applied_finalization_failure_is_reported_and_discoverable() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = write_test_pending_proposal(
        &service,
        "Applied cleanup packet",
        OkfProposalSensitivity::RepoSafe,
    )?;

    let error = service
        .apply_file_proposal_with_finalize_hook(&pending, "agent:applier", |_| {
            Err(anyhow::anyhow!("injected committed cleanup failure"))
        })
        .expect_err("committed cleanup failure must be observable");
    assert!(
        error
            .to_string()
            .contains("apply committed but finalization was interrupted")
    );
    assert!(!pending.exists());
    assert!(
        service
            .paths
            .records_dir()
            .join("applied-cleanup-packet.md")
            .is_file()
    );
    assert!(
        service
            .paths
            .proposals_dir()
            .join("resolved/applied/applied-cleanup-packet.md")
            .is_file()
    );
    let artifacts = lifecycle_transaction_artifacts(&service.paths)?;
    assert_eq!(artifacts.len(), 3, "unexpected artifacts: {artifacts:?}");
    assert!(
        artifacts
            .iter()
            .all(|artifact| artifact.starts_with(repository_transaction_root(&service.paths)))
    );
    assert!(
        artifacts
            .iter()
            .all(|artifact| !artifact.starts_with(&service.paths.project_root))
    );
    let names = artifacts
        .iter()
        .filter_map(|artifact| artifact.file_name()?.to_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.ends_with(".pending.tmp"))
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.ends_with(".write.tmp"))
            .count(),
        2
    );
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

fn repo_session_document(title: &str) -> SessionEndDocument {
    SessionEndDocument {
        task: "Exercise guarded proposal creation".to_owned(),
        candidates: vec![SessionEndCandidate {
            destination: MemoryDestination::Repo,
            memory_type: MemoryType::Decision,
            lane: MemoryLane::Semantic,
            title: title.to_owned(),
            body: "The pending packet must remain inside the guarded repository root.".to_owned(),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            reason: Some("Lifecycle regression coverage".to_owned()),
            scope: None,
            tags: vec!["lifecycle".to_owned()],
        }],
    }
}

fn repo_import_document(title: &str) -> ImportDocument {
    ImportDocument {
        version: "memzoi/import-v1".to_owned(),
        sources: vec![OkfProposalSource {
            path: Some("src/lifecycle.rs".to_owned()),
            url: None,
            reference: None,
        }],
        candidates: vec![ImportCandidateInput {
            destination: MemoryDestination::Repo,
            reason: "Lifecycle regression coverage".to_owned(),
            memory_type: Some(MemoryType::Decision),
            lane: Some(MemoryLane::Semantic),
            title: title.to_owned(),
            body: "The imported pending packet must remain inside the guarded repository root."
                .to_owned(),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            scope: None,
            tags: vec!["lifecycle".to_owned()],
        }],
    }
}

fn write_test_pending_proposal(
    service: &MemoryService,
    title: &str,
    sensitivity: OkfProposalSensitivity,
) -> anyhow::Result<PathBuf> {
    write_test_pending_proposal_with_content_class(
        service,
        title,
        sensitivity,
        RepositoryContentClass::GeneralRepoKnowledge,
    )
}

fn write_test_pending_proposal_with_content_class(
    service: &MemoryService,
    title: &str,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
) -> anyhow::Result<PathBuf> {
    let proposal_id = proposals::title_to_concept_slug(title)
        .context("test proposal title should produce a slug")?;
    write_test_pending_proposal_with_id_and_content_class(
        service,
        &proposal_id,
        title,
        sensitivity,
        content_class,
    )
}

fn write_test_pending_proposal_with_id(
    service: &MemoryService,
    proposal_id: &str,
    title: &str,
    sensitivity: OkfProposalSensitivity,
) -> anyhow::Result<PathBuf> {
    write_test_pending_proposal_with_id_and_content_class(
        service,
        proposal_id,
        title,
        sensitivity,
        RepositoryContentClass::GeneralRepoKnowledge,
    )
}

fn write_test_pending_proposal_with_id_and_content_class(
    service: &MemoryService,
    proposal_id: &str,
    title: &str,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
) -> anyhow::Result<PathBuf> {
    prepare_pending_proposal_root(&service.paths)?;
    let draft = okf::OkfCreateProposalDraft {
        proposal_id: proposal_id.to_owned(),
        memory_type: MemoryType::Decision,
        lane: MemoryLane::Semantic,
        title: title.to_owned(),
        body: "Lifecycle finalization must never hide cleanup failures.".to_owned(),
        actor: "agent:red-tests".to_owned(),
        timestamp: "2026-07-10T00:00:00Z".to_owned(),
        reason: Some("Lifecycle regression coverage".to_owned()),
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        applies_to: vec!["crates/memzoi-core/**".to_owned()],
        tags: vec!["lifecycle".to_owned()],
        sources: vec![OkfProposalSource {
            path: Some("crates/memzoi-core/src/service.rs".to_owned()),
            url: None,
            reference: None,
        }],
        sensitivity: OkfProposalSensitivity::RepoSafe,
        content_class,
        capture: None,
    };
    let plan =
        okf::plan_okf_create_proposal(&service.paths.proposals_dir().join("pending"), &draft)?;
    if sensitivity == OkfProposalSensitivity::RepoSafe {
        okf::create_okf_proposal_file(&plan)
    } else {
        let markdown = plan.markdown.replacen(
            "sensitivity: repo-safe",
            &format!("sensitivity: {}", sensitivity.as_str()),
            1,
        );
        fs::write(&plan.path, markdown)?;
        Ok(plan.path)
    }
}

fn write_test_supersede_proposal(
    service: &MemoryService,
    proposal_id: &str,
    title: &str,
    target_id: &str,
) -> anyhow::Result<PathBuf> {
    let path = write_test_pending_proposal_with_id(
        service,
        proposal_id,
        title,
        OkfProposalSensitivity::RepoSafe,
    )?;
    let markdown = fs::read_to_string(&path)?
        .replace("action: create", "action: supersede")
        .replace("supersedes: []", &format!("supersedes:\n- {target_id}"))
        .replace("2026-07-10T00:00:00Z", "2099-07-10T00:00:00Z");
    fs::write(&path, markdown)?;
    Ok(path)
}

fn insert_runtime_record_with_id(
    service: &MemoryService,
    record_id: &str,
    destination: MemoryDestination,
) -> anyhow::Result<()> {
    let record = MemoryRecord {
        id: record_id.to_owned(),
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        destination,
        scope_kind: ScopeKind::Personal,
        scope_id: None,
        visibility: Visibility::Private,
        title: "Runtime collision sentinel".to_owned(),
        body: "Runtime collision sentinel".to_owned(),
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind: Some("test-runtime".to_owned()),
        source_ref: None,
        proposal_id: None,
        capture: None,
        content_hash: blake3::hash(b"Runtime collision sentinel")
            .to_hex()
            .to_string(),
        created_at: "2026-07-10T00:00:00Z".to_owned(),
        updated_at: "2026-07-10T00:00:00Z".to_owned(),
        supersedes_id: None,
        expires_at: None,
    };
    RuntimeRecords::new(&service.conn).insert_for_test(&record)
}

fn apply_test_record(service: &MemoryService, draft: MemoryDraft) -> anyhow::Result<MemoryRecord> {
    let proposal = service.propose_memory("agent:red-tests", draft)?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    service.apply_proposal(&proposal.id, "agent:applier")
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
        source_ref: Some("proposal-packet-tests".to_owned()),
        sensitivity: OkfProposalSensitivity::RepoSafe,
        content_class: RepositoryContentClass::GeneralRepoKnowledge,
        confidence: 0.82,
    }
}
