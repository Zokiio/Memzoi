use std::{collections::BTreeSet, fs, path::Path, process::Command};

use anyhow::{Context, bail};
use memzoi_core::{
    ExpectedPriorRevision, InitRequest, MaterializationAction,
    MaterializationAuthorizationCapability, MaterializationOutputOutcome, MemoryDraft, MemoryLane,
    MemoryPaths, MemoryService, MemoryStatus, MemoryType, OkfProposalSensitivity, ProposalStatus,
    ProposeOptions, RepositoryMaterializationCandidate, RepositoryMaterializationCandidateRecord,
    RepositoryWriteRoute, ScopeKind, Visibility, build_repository_materialization_candidate,
    build_repository_materialization_decision, read_okf_record_files,
    repository_materialization_candidate_plan, repository_materialization_policy,
};
use tempfile::TempDir;

#[test]
fn every_route_has_a_unique_stable_identifier() {
    assert_eq!(RepositoryWriteRoute::ALL.len(), 14);
    let identifiers = RepositoryWriteRoute::ALL
        .iter()
        .map(|route| route.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(identifiers.len(), RepositoryWriteRoute::ALL.len());
    assert!(identifiers.contains("materialization"));
    assert!(identifiers.contains("recovery"));
}

#[test]
fn direct_structured_materialization_is_the_only_direct_attested_record_route() -> anyhow::Result<()>
{
    let (_temp, service) = initialized_git_service()?;
    let candidate = materialization_candidate(
        "direct-materialization",
        "Direct structured materialization",
    )?;
    let (plan, decision) = materialization_plan_and_decision(&candidate)?;

    let result = service.apply_repository_materialization(&plan, &decision, &candidate)?;

    assert_eq!(
        result.outputs[0].outcome,
        MaterializationOutputOutcome::Written
    );
    let records = read_okf_record_files(service.paths().records_dir())?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].concept_id, candidate.record.concept_id);
    assert_eq!(
        records[0]
            .materialization
            .as_ref()
            .map(|metadata| &metadata.plan_id),
        Some(&plan.plan_id),
        "only the direct structured route may mint a materialization attestation",
    );
    assert!(
        has_unstaged_record(&service.paths().project_root, &candidate.record.concept_id)?,
        "direct materialization must leave its canonical record in the reviewable working tree",
    );
    Ok(())
}

#[test]
fn legacy_direct_proposals_stay_proposal_compatible_without_direct_materialization()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let accepted = service.propose_memory_with_options(
        "agent:route-parity",
        memory_draft(
            "Accepted legacy direct proposal",
            "The compatibility route remains an approved proposal until explicit legacy apply.",
        ),
        ProposeOptions {
            approval_override: None,
            apply: false,
        },
    )?;

    assert_eq!(accepted.proposal.status, ProposalStatus::Approved);
    assert!(
        accepted
            .validation
            .as_ref()
            .is_some_and(|validation| validation.is_valid)
    );
    assert_eq!(accepted.record, None);
    assert!(!accepted.applied);
    assert_no_direct_canonical_record(&service, "accepted legacy direct proposal")?;

    for (label, sensitivity) in [
        ("local", OkfProposalSensitivity::LocalOnly),
        ("unsafe", OkfProposalSensitivity::Sensitive),
        ("unknown", OkfProposalSensitivity::Unknown),
    ] {
        let mut candidate = memory_draft(
            &format!("{label} legacy proposal"),
            "This candidate must remain outside repository canonical memory.",
        );
        candidate.sensitivity = sensitivity;

        let result = service.propose_memory_with_options(
            "agent:route-parity",
            candidate,
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;

        assert!(!result.applied, "{label} candidate unexpectedly applied");
        assert_eq!(result.record, None, "{label} candidate returned a record");
        assert_no_direct_canonical_record(&service, label)?;
    }
    Ok(())
}

#[test]
fn rebuild_and_open_recovery_preserve_existing_legacy_records_without_attestation()
-> anyhow::Result<()> {
    let (_temp, service) = initialized_git_service()?;
    let paths = service.paths().clone();
    let proposal = service.propose_memory(
        "agent:route-parity",
        memory_draft(
            "Legacy record for rebuild",
            "Rebuild and recovery may read this pre-existing compatibility record only.",
        ),
    )?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:route-parity")?;
    let record = service.apply_proposal(&proposal.id, "agent:route-parity")?;
    let record_path = paths.records_dir().join(format!("{}.md", record.id));
    let before_bytes = fs::read(&record_path)?;
    let status_before = git_status(&paths.project_root)?;
    assert!(
        read_okf_record_files(paths.records_dir())?[0]
            .materialization
            .is_none(),
        "legacy proposal apply must not invent a materialization decision before rebuild",
    );

    drop(service);
    MemoryService::rebuild_paths(paths.clone())?;
    drop(MemoryService::open_paths(paths.clone())?);

    assert_eq!(
        fs::read(&record_path)?,
        before_bytes,
        "rebuild or open-time recovery must not rewrite an existing legacy record",
    );
    assert_eq!(
        git_status(&paths.project_root)?,
        status_before,
        "rebuild or open-time recovery must not mint a new unstaged canonical record",
    );
    let records = read_okf_record_files(paths.records_dir())?;
    assert_eq!(records.len(), 1);
    assert!(
        records[0].materialization.is_none(),
        "rebuild or open-time recovery must not invent a materialization decision",
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
            "failed to initialize route-parity Git repository: {}",
            String::from_utf8_lossy(&output.stderr),
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

fn memory_draft(title: &str, body: &str) -> MemoryDraft {
    MemoryDraft {
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        visibility: Visibility::Repo,
        title: title.to_owned(),
        body: body.to_owned(),
        tags: vec!["route-parity".to_owned()],
        source_kind: Some("test".to_owned()),
        source_ref: Some("issue-100".to_owned()),
        sensitivity: OkfProposalSensitivity::RepoSafe,
        content_class: memzoi_core::RepositoryContentClass::GeneralRepoKnowledge,
        confidence: 0.82,
    }
}

fn materialization_candidate(
    concept_id: &str,
    title: &str,
) -> anyhow::Result<RepositoryMaterializationCandidate> {
    build_repository_materialization_candidate(
        RepositoryMaterializationCandidateRecord {
            concept_id: concept_id.to_owned(),
            draft: memory_draft(title, "A reviewed direct structured canonical record."),
            status: MemoryStatus::Active,
            applies_to: Vec::new(),
            created: "2026-07-16T00:00:00Z".to_owned(),
            updated: None,
            supersedes_id: None,
            retention: memzoi_core::retention_facts_for_creation(
                MemoryLane::Semantic,
                "2026-07-16T00:00:00Z",
                None,
                None,
            )?,
            origin: memzoi_core::OriginDescriptor::new(
                format!("repository-materialization:test:{concept_id}"),
                memzoi_core::OriginRoute::RepositoryMaterialization,
            ),
            lineage: None,
            proposal_id: None,
            capture: None,
        },
        MaterializationAction::Create,
        ExpectedPriorRevision::Absent,
        None,
        None,
    )
}

fn materialization_plan_and_decision(
    candidate: &RepositoryMaterializationCandidate,
) -> anyhow::Result<(
    memzoi_core::RepositoryMaterializationPlan,
    memzoi_core::RepositoryMaterializationDecision,
)> {
    let plan = repository_materialization_candidate_plan(candidate)?;
    let decision = build_repository_materialization_decision(
        &plan,
        "2026-07-16T00:00:00Z".to_owned(),
        repository_materialization_policy(),
        MaterializationAuthorizationCapability::ExplicitCli,
    )?;
    Ok((plan, decision))
}

fn assert_no_direct_canonical_record(service: &MemoryService, route: &str) -> anyhow::Result<()> {
    assert!(
        read_okf_record_files(service.paths().records_dir())?.is_empty(),
        "{route} unexpectedly created a canonical record",
    );
    assert!(
        !git_status(&service.paths().project_root)?
            .lines()
            .any(|line| line.contains(".memzoi/records/")),
        "{route} unexpectedly created an unstaged canonical record",
    );
    Ok(())
}

fn has_unstaged_record(project_root: &Path, record_id: &str) -> anyhow::Result<bool> {
    let expected = format!("?? .memzoi/records/{record_id}.md");
    Ok(git_status(project_root)?
        .lines()
        .any(|line| line == expected))
}

fn git_status(project_root: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(project_root)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to read route-parity Git status: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
    String::from_utf8(output.stdout).context("Git status was not valid UTF-8")
}
