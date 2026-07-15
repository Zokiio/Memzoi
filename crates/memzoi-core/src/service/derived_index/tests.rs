use tempfile::TempDir;

use crate::{MemoryDraft, MemoryLane, MemoryPaths, MemoryType, ScopeKind, Visibility};

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

fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let paths = MemoryPaths::with_runtime_home(
        temp.path().canonicalize()?,
        temp.path().join(".memzoi-runtime"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    let service = MemoryService::open_paths(paths)?;
    Ok((temp, service))
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
        confidence: 0.82,
    }
}
