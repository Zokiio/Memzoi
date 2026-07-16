use std::fs;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::{
    MemoryDraft, MemoryLane, MemoryPaths, MemoryRecord, MemoryType, ProposalStatus, ScopeKind,
    SearchInput, Visibility,
};

use super::super::{InitRequest, MemoryService, ProposalApprovalOverride, ProposeOptions};

#[test]
fn rebuild_preserves_shared_proposals_and_repairs_the_index_mirror() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Pending rebuild proposal", "Pending rebuild proposal body"),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let validated = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Validated rebuild proposal",
            "Validated rebuild proposal body",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    service.shared_conn.execute(
        "UPDATE proposal SET status = 'validated' WHERE id = ?1",
        [validated.proposal.id.as_str()],
    )?;
    let approved = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Approved rebuild proposal",
            "Approved rebuild proposal body",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    service.validate_proposal(&approved.proposal.id)?;
    service.approve_proposal(&approved.proposal.id, "reviewer:human")?;

    let paths = service.paths.clone();
    service.conn.execute_batch(
        "ALTER TABLE proposal RENAME TO disposable_proposal_mirror;
         CREATE TABLE proposal (
             id TEXT PRIMARY KEY,
             status TEXT NOT NULL
         );",
    )?;

    let result = service.rebuild()?;
    assert_eq!(result.db_path, paths.index_db_path);

    let reopened = MemoryService::open_paths(paths.clone())?;
    for (proposal_id, status) in [
        (pending.proposal.id.as_str(), ProposalStatus::Pending),
        (validated.proposal.id.as_str(), ProposalStatus::Validated),
        (approved.proposal.id.as_str(), ProposalStatus::Approved),
    ] {
        assert_eq!(reopened.show_proposal(proposal_id)?.status, status);
    }

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_count: i64 =
        index.query_row("SELECT COUNT(*) FROM proposal", [], |row| row.get(0))?;
    assert_eq!(mirrored_proposal_count, 3);
    let disposable_table_remains: bool = index.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'disposable_proposal_mirror'
         )",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !disposable_table_remains,
        "rebuild should replace the corrupted proposal mirror"
    );

    Ok(())
}

#[test]
fn rebuild_refuses_when_the_shared_runtime_cannot_be_read() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Pending shared proposal", "Pending shared proposal body"),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let paths = service.paths.clone();
    drop(service);
    fs::write(&paths.shared_db_path, b"not a SQLite database")?;

    let error = MemoryService::rebuild_paths(paths.clone())
        .expect_err("rebuild should fail closed when shared runtime cannot be read");
    let message = format!("{error:#}");
    assert!(
        message.contains("shared database") && message.contains("not a database"),
        "rebuild refusal should identify the unreadable shared database: {message}"
    );

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_remains: bool = index.query_row(
        "SELECT EXISTS(SELECT 1 FROM proposal WHERE id = ?1)",
        [pending.proposal.id.as_str()],
        |row| row.get(0),
    )?;
    assert!(
        mirrored_proposal_remains,
        "failed shared-runtime inspection must not replace the derived index"
    );

    Ok(())
}

#[test]
fn rebuild_validates_shared_proposals_before_replacing_the_index() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Pending proposal before schema damage",
            "The existing mirror must survive failed shared proposal inspection.",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let paths = service.paths.clone();
    drop(service);

    let shared = Connection::open(&paths.shared_db_path)?;
    shared.execute_batch(
        "ALTER TABLE proposal RENAME TO damaged_proposal;
         CREATE TABLE proposal (id TEXT PRIMARY KEY);",
    )?;
    drop(shared);

    let error = MemoryService::rebuild_paths(paths.clone())
        .expect_err("rebuild should inspect shared proposals before replacing the index");
    assert!(
        format!("{error:#}").contains("shared database proposals"),
        "unexpected rebuild error: {error:#}"
    );

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_remains: bool = index.query_row(
        "SELECT EXISTS(SELECT 1 FROM proposal WHERE id = ?1)",
        [pending.proposal.id],
        |row| row.get(0),
    )?;
    assert!(
        mirrored_proposal_remains,
        "failed shared proposal inspection must not replace the derived index"
    );
    Ok(())
}

#[test]
fn rebuild_scans_the_same_immutable_snapshot_that_it_imports() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft("Immutable rebuild snapshot", "Safe indexed baseline body."),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    let safe = fs::read_to_string(&record_path)?;
    let prohibited = safe
        .replace(
            "content_class: general_repo_knowledge",
            "content_class: raw_transcript",
        )
        .replace(
            "Safe indexed baseline body.",
            "Lexically harmless snapshotracesentinel payload.",
        );
    fs::write(&record_path, prohibited)?;

    let error = MemoryService::rebuild_paths_with_snapshot_hook(service.paths.clone(), || {
        fs::write(&record_path, &safe)?;
        Ok(())
    })
    .expect_err("the prohibited first snapshot must block after a safe replacement");
    assert!(format!("{error:#}").contains("raw_transcript"));
    assert!(
        service
            .search_memory(SearchInput {
                query: "snapshotracesentinel".to_owned(),
                limit: 10,
                ..SearchInput::default()
            })?
            .is_empty(),
        "the unscanned first snapshot reached runtime search"
    );
    Ok(())
}

fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    std::fs::create_dir(&project_root)?;
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
