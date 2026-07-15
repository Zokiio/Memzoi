use std::fs;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::{
    MemoryDraft, MemoryLane, MemoryPaths, MemoryRecord, MemoryType, ScopeKind, SearchInput,
    Visibility,
};

use super::super::{InitRequest, MemoryService, ProposalApprovalOverride, ProposeOptions};

#[test]
fn rebuild_refuses_to_discard_open_proposals_with_actionable_details() -> anyhow::Result<()> {
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
    service.conn.execute(
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
            approval_override: None,
            apply: false,
        },
    )?;

    let error = service
        .rebuild()
        .expect_err("rebuild should not discard open proposals");
    let message = error.to_string();

    assert!(
        message.contains("rebuild refused because 3 open proposals would be discarded"),
        "rebuild refusal should include the open proposal count: {message}"
    );
    for (proposal_id, status) in [
        (pending.proposal.id.as_str(), "pending"),
        (validated.proposal.id.as_str(), "validated"),
        (approved.proposal.id.as_str(), "approved"),
    ] {
        let summary = format!("{proposal_id} ({status})");
        assert!(
            message.contains(&summary),
            "rebuild refusal should include open proposal summary {summary}: {message}"
        );
    }
    assert!(
        message.contains("memzoi proposals list --status open"),
        "rebuild refusal should suggest listing open proposals: {message}"
    );
    assert!(
        message.contains("memzoi proposals apply --all-approved"),
        "rebuild refusal should suggest applying approved proposals: {message}"
    );
    assert!(
        message.contains("memzoi reject <proposal-id> --reason"),
        "rebuild refusal should suggest rejecting proposals before rebuild: {message}"
    );

    Ok(())
}

#[test]
fn rebuild_refuses_when_open_proposals_cannot_be_inspected() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let db_path = service.paths.db_path.clone();
    service.conn.execute_batch(
        "ALTER TABLE proposal RENAME TO proposal_with_expected_schema;
         CREATE TABLE proposal (
             id TEXT PRIMARY KEY,
             status TEXT NOT NULL
         );",
    )?;

    let error = service
        .rebuild()
        .expect_err("rebuild should fail closed when proposal inspection fails");
    let message = error.to_string();
    assert!(
        message.contains("rebuild refused because open proposals could not be inspected"),
        "rebuild refusal should explain the failed proposal inspection: {message}"
    );

    let conn = Connection::open(db_path)?;
    let original_proposal_table_remains: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'proposal_with_expected_schema'
         )",
        [],
        |row| row.get(0),
    )?;
    assert!(
        original_proposal_table_remains,
        "failed proposal inspection must not replace the derived database"
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
