use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use memzoi_core::{
    ADR_EXTRACTOR_PROFILE, CAPTURE_REQUEST_SCHEMA, CAPTURE_REVIEW_INPUT_SCHEMA, CaptureAction,
    CaptureCandidate, CaptureDataClass, CaptureEvidence, CaptureExtractorRequest,
    CaptureGitSourceContext, CapturePlan, CapturePlanStatus, CaptureRequest,
    CaptureReviewDecisionInput, CaptureReviewInput, CaptureReviewOutcome, CaptureSemanticLocation,
    CaptureSourceInputs, CaptureSourceLocator, CaptureSourceRequest, GIT_CHANGE_EXTRACTOR_PROFILE,
    INSTRUCTION_EXTRACTOR_PROFILE, InitRequest, MemoryDestination, MemoryPaths, MemoryService,
    OkfProposalSensitivity, SearchInput, build_capture_review, build_capture_review_with_inputs,
    plan_capture, plan_capture_with_inputs, read_okf_proposal_files, read_okf_record_files,
};
use tempfile::TempDir;

const REVIEWED_AT: &str = "2026-07-11T12:00:00Z";
const BASE: &str = "sha1:1111111111111111111111111111111111111111";
const HEAD: &str = "sha1:2222222222222222222222222222222222222222";

#[test]
fn instruction_profile_is_explicit_nested_feedback_safe_reviewed_and_cited() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let source = concat!(
        "# Agent instructions\n\n",
        "## Workflow\n\n",
        "Review durable changes before applying them.\n\n",
        "### Procedure: Nested capture rule\n\n",
        "Use nestedinstructionrecalltoken for durable repository work.\n\n",
        "<!-- memzoi:start -->\n",
        "### Procedure: Generated projection\n\n",
        "Never capture generatedinstructioncanary.\n",
        "<!-- memzoi:end -->\n\n",
        "### Explicitly rejected rule\n\n",
        "The rejectedinstructiontoken must not be retained.\n\n",
        "## Workflow\n\n",
        "Review durable changes before applying them.\n\n",
        "## Workflow\n\n",
        "Apply durable changes without review.\n\n",
        "## Temporary\n\n",
        "Keep this instruction only for the current task.\n",
    );
    fixture.write("nested/AGENTS.md", source)?;

    let request = project_path_request(
        "instructions",
        "nested/AGENTS.md",
        INSTRUCTION_EXTRACTOR_PROFILE,
    );
    let before = fixture.snapshot()?;
    let plan = plan_capture(&fixture.paths, request.clone())?;
    assert_eq!(
        before,
        fixture.snapshot()?,
        "instruction planning must be read-only"
    );
    assert_eq!(plan.status, CapturePlanStatus::Ready);
    assert_eq!(plan.data_class, CaptureDataClass::Private);
    assert!(!serde_json::to_string(&plan)?.contains("generatedinstructioncanary"));
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "generated_instruction_block_excluded"
            && diagnostic.source_id.as_deref() == Some("instructions")
    }));

    let workflow = candidates_named(&plan, "Workflow");
    assert_eq!(workflow.len(), 3);
    assert!(matches!(
        workflow[0].action,
        CaptureAction::CreateProposal { .. }
    ));
    assert!(matches!(
        workflow[1].action,
        CaptureAction::Duplicate { .. }
    ));
    assert!(matches!(workflow[2].action, CaptureAction::Conflict { .. }));

    let nested = candidate_named(&plan, "Nested capture rule");
    assert_eq!(nested.memory.scope.paths, ["nested"]);
    assert_eq!(nested.evidence.len(), 1);
    assert_eq!(
        nested.evidence[0].heading_path,
        [
            "Agent instructions",
            "Workflow",
            "Procedure: Nested capture rule"
        ]
    );
    assert!(matches!(
        nested.evidence[0].semantic_location,
        Some(CaptureSemanticLocation::Instruction)
    ));
    assert_exact_evidence(source.as_bytes(), &nested.evidence[0]);

    let temporary = candidate_named(&plan, "Temporary");
    assert_eq!(
        temporary.classification.destination,
        MemoryDestination::NeedsReview
    );
    assert!(matches!(
        temporary.action,
        CaptureAction::NoWrite { ref reason_code } if reason_code == "needs_review"
    ));

    let decisions: Vec<CaptureReviewDecisionInput> = plan
        .candidates
        .iter()
        .map(|candidate| {
            let (outcome, reason_code) = if candidate.candidate_id == workflow[0].candidate_id
                || candidate.candidate_id == nested.candidate_id
            {
                (CaptureReviewOutcome::Accept, None)
            } else if candidate.candidate_id == workflow[2].candidate_id {
                (
                    CaptureReviewOutcome::Defer,
                    Some("contradictory-instruction".to_owned()),
                )
            } else {
                (
                    CaptureReviewOutcome::Reject,
                    Some("not-approved-for-capture".to_owned()),
                )
            };
            CaptureReviewDecisionInput {
                candidate_id: candidate.candidate_id.clone(),
                outcome,
                reason_code,
                memory: None,
                requested_destination: None,
            }
        })
        .collect();

    fixture.write(
        "nested/AGENTS.md",
        &format!("{source}\n<!-- stale edit -->\n"),
    )?;
    let error = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions.clone()),
        "profile-reviewer",
        REVIEWED_AT,
    )
    .expect_err("an instruction source edit must stale the plan");
    assert!(error.to_string().contains("stale capture plan"));
    fixture.write("nested/AGENTS.md", source)?;

    let review = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions),
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    assert_eq!(
        review
            .decisions
            .iter()
            .filter(|decision| decision.outcome == CaptureReviewOutcome::Reject)
            .count(),
        3
    );
    let service = MemoryService::open_paths(fixture.paths.clone())?;
    let routed = service.apply_capture(
        "profile-reviewer",
        plan.clone(),
        review.clone(),
        &plan.plan_id,
        &review.review_id,
    )?;
    assert_eq!(routed.writes.len(), 2);
    assert!(read_okf_record_files(fixture.paths.records_dir())?.is_empty());
    assert_eq!(
        read_okf_proposal_files(fixture.paths.proposals_dir().join("pending"))?.len(),
        2
    );
    apply_pending_named(&service, &fixture.paths, "Nested capture rule")?;
    assert_cited_recall(
        &service,
        "nestedinstructionrecalltoken",
        MemoryDestination::Repo,
        &review.review_id,
    )?;
    assert!(
        search(
            &service,
            "rejectedinstructiontoken",
            MemoryDestination::Repo
        )?
        .is_empty(),
        "an explicitly rejected candidate must not be written"
    );

    fixture.write("guide.md", "# Workflow\nNever scan this implicitly.\n")?;
    let invalid = plan_capture(
        &fixture.paths,
        project_path_request("invalid", "guide.md", INSTRUCTION_EXTRACTOR_PROFILE),
    )?;
    assert_eq!(invalid.status, CapturePlanStatus::Blocked);
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "source_preflight_failed" })
    );
    Ok(())
}

#[test]
fn generated_instruction_projection_is_excluded_without_feedback() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "AGENTS.md",
        concat!(
            "<!-- Generated by memzoi. Do not edit directly. -->\n",
            "# Procedure: Exported memory\n",
            "Never reingest exportedprojectioncanary.\n",
        ),
    )?;
    let before = fixture.snapshot()?;
    let plan = plan_capture(
        &fixture.paths,
        project_path_request("projection", "AGENTS.md", INSTRUCTION_EXTRACTOR_PROFILE),
    )?;
    assert_eq!(before, fixture.snapshot()?);
    assert!(plan.candidates.is_empty());
    assert_eq!(plan.diagnostics[0].code, "generated_projection_excluded");
    assert!(!serde_json::to_string(&plan)?.contains("exportedprojectioncanary"));
    Ok(())
}

#[test]
fn instruction_heading_markers_force_review_routing() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(
        "AGENTS.md",
        concat!(
            "# Agent instructions\n\n",
            "## Temporary notes\n\n",
            "Use this override during migration.\n\n",
            "## Private instructions\n\n",
            "Keep this guidance out of shared memory.\n\n",
            "### Procedure: Nested shared rule\n\n",
            "Use a stable cache key for generated artifacts.\n\n",
            "## Temporarily stable\n\n",
            "This durable guidance has a near-miss heading.\n\n",
            "## Privateer workflow\n\n",
            "This durable workflow also has a near-miss heading.\n",
        ),
    )?;

    let plan = plan_capture(
        &fixture.paths,
        project_path_request("instructions", "AGENTS.md", INSTRUCTION_EXTRACTOR_PROFILE),
    )?;

    for title in [
        "Temporary notes",
        "Private instructions",
        "Nested shared rule",
    ] {
        let candidate = candidate_named(&plan, title);
        assert_eq!(
            candidate.classification.destination,
            MemoryDestination::NeedsReview
        );
        assert_eq!(
            candidate.classification.sensitivity,
            OkfProposalSensitivity::Unknown
        );
        assert!(matches!(
            candidate.action,
            CaptureAction::NoWrite { ref reason_code } if reason_code == "needs_review"
        ));
    }
    for title in ["Temporarily stable", "Privateer workflow"] {
        let candidate = candidate_named(&plan, title);
        assert_eq!(
            candidate.classification.destination,
            MemoryDestination::Repo
        );
        assert_eq!(
            candidate.classification.sensitivity,
            OkfProposalSensitivity::RepoSafe
        );
    }

    let decisions = plan
        .candidates
        .iter()
        .map(|candidate| CaptureReviewDecisionInput {
            candidate_id: candidate.candidate_id.clone(),
            outcome: CaptureReviewOutcome::Reject,
            reason_code: Some("review-routing-regression".to_owned()),
            memory: None,
            requested_destination: None,
        })
        .collect();
    let review = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions),
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    assert_eq!(review.decisions.len(), plan.candidates.len());
    Ok(())
}

#[test]
fn adr_directory_classifies_statuses_preserves_exact_lifecycle_evidence_and_cites_apply()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    fixture.write("docs/adr/.gitignore", "ignored.md\n")?;
    let accepted_source = concat!(
        "---\n",
        "title: Accepted cache\n",
        "status: accepted\n",
        "supersedes: adr-old-cache\n",
        "---\n",
        "# Accepted cache\n\n",
        "## Status\n\n",
        "Accepted\n\n",
        "## Context\n\n",
        "The acceptedcachecontexttoken explains the choice.\n\n",
        "## Decision\n\n",
        "Use acceptedadrrecalltoken for cache identity.\n\n",
        "## Consequences\n\n",
        "Cache keys remain deterministic.\n\n",
        "## Risks\n\n",
        "Cache invalidation remains hard.\n",
    );
    fixture.write("docs/adr/001-accepted.md", accepted_source)?;
    fixture.write(
        "docs/adr/002-draft.md",
        "# Draft cache\n\n## Status\n\nDraft\n\n## Decision\n\nUse draftadrtoken after review.\n",
    )?;
    fixture.write(
        "docs/adr/003-rejected.md",
        "# Rejected cache\n\n## Status\n\nRejected\n\n## Decision\n\nNever promote rejectedadrtoken.\n",
    )?;
    fixture.write(
        "docs/adr/004-superseded.md",
        "# Superseded cache\n\n## Status\n\nSuperseded\n\n## Decision\n\nDo not promote supersededadrtoken.\n",
    )?;
    fixture.write(
        "docs/adr/005-malformed.md",
        "# Malformed cache\n\n## Decision\n\nDo not promote malformedadrtoken.\n",
    )?;
    fixture.write(
        "docs/adr/006-contradictory.md",
        concat!(
            "---\n",
            "title: Contradictory cache\n",
            "status: accepted\n",
            "---\n",
            "# Contradictory cache\n\n",
            "## Status\n\n",
            "Draft\n\n",
            "## Decision\n\n",
            "Do not promote contradictoryadrtoken.\n",
        ),
    )?;
    fixture.write(
        "docs/adr/ignored.md",
        "# Ignored\n\n## Status\n\nAccepted\n\n## Decision\n\nignoredadrtoken\n",
    )?;

    let request = adr_directory_request();
    let before = fixture.snapshot()?;
    let plan = plan_capture(&fixture.paths, request)?;
    assert_eq!(
        before,
        fixture.snapshot()?,
        "ADR planning must be read-only"
    );
    assert_eq!(plan.status, CapturePlanStatus::Ready);
    assert_eq!(
        plan.sources[0]
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "docs/adr/001-accepted.md",
            "docs/adr/002-draft.md",
            "docs/adr/003-rejected.md",
            "docs/adr/004-superseded.md",
            "docs/adr/005-malformed.md",
            "docs/adr/006-contradictory.md",
        ]
    );
    assert_eq!(
        plan.sources[0]
            .policy_inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect::<Vec<_>>(),
        vec!["docs/adr/.gitignore"]
    );
    let rendered = serde_json::to_string(&plan)?;
    assert!(!rendered.contains("ignoredadrtoken"));
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "malformed_adr_metadata" })
    );
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "conflicting_adr_status" })
    );

    let accepted = candidate_named(&plan, "Accepted cache");
    assert!(matches!(
        accepted.action,
        CaptureAction::CreateProposal { .. }
    ));
    for required in ["title", "status", "decision"] {
        let evidence = adr_evidence(accepted, required, "accepted", None);
        assert_exact_evidence(accepted_source.as_bytes(), evidence);
        assert_eq!(
            evidence.locator,
            CaptureSourceLocator::ProjectPath {
                path: "docs/adr/001-accepted.md".to_owned()
            }
        );
    }
    for (title, field) in [
        ("Accepted cache — context", "context"),
        ("Accepted cache — consequences", "consequences"),
        ("Accepted cache — risks", "risk"),
    ] {
        let candidate = candidate_named(&plan, title);
        assert!(matches!(
            candidate.action,
            CaptureAction::CreateProposal { .. }
        ));
        assert_exact_evidence(
            accepted_source.as_bytes(),
            adr_evidence(candidate, field, "accepted", None),
        );
    }
    let supersession = candidate_named(&plan, "Accepted cache — supersession");
    assert_eq!(supersession.memory.body, "adr-old-cache");
    assert_eq!(
        supersession.classification.destination,
        MemoryDestination::NeedsReview
    );
    assert!(matches!(supersession.action, CaptureAction::NoWrite { .. }));
    let target_evidence = adr_evidence(
        supersession,
        "supersession",
        "accepted",
        Some("adr-old-cache"),
    );
    assert_exact_evidence(accepted_source.as_bytes(), target_evidence);

    for (title, status) in [
        ("Draft cache", "draft"),
        ("Rejected cache", "rejected"),
        ("Superseded cache", "superseded"),
        ("Contradictory cache", "unknown"),
    ] {
        let candidate = candidate_named(&plan, title);
        assert!(
            candidate
                .memory
                .tags
                .contains(&format!("adr-status:{status}"))
        );
        assert_eq!(
            candidate.classification.destination,
            MemoryDestination::NeedsReview
        );
        assert!(matches!(candidate.action, CaptureAction::NoWrite { .. }));
    }
    assert!(
        !plan
            .candidates
            .iter()
            .any(|candidate| { candidate.memory.body.contains("malformedadrtoken") })
    );

    let decisions = plan
        .candidates
        .iter()
        .map(|candidate| CaptureReviewDecisionInput {
            candidate_id: candidate.candidate_id.clone(),
            outcome: if matches!(candidate.action, CaptureAction::CreateProposal { .. }) {
                CaptureReviewOutcome::Accept
            } else if candidate.memory.title == "Contradictory cache" {
                CaptureReviewOutcome::Defer
            } else {
                CaptureReviewOutcome::Reject
            },
            reason_code: (!matches!(candidate.action, CaptureAction::CreateProposal { .. }))
                .then(|| "adr-not-active-authority".to_owned()),
            memory: None,
            requested_destination: None,
        })
        .collect::<Vec<_>>();

    fixture.write(
        "docs/adr/007-new.md",
        "# New ADR\n\n## Status\n\nAccepted\n\n## Decision\n\nnewadrtoken\n",
    )?;
    let error = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions.clone()),
        "profile-reviewer",
        REVIEWED_AT,
    )
    .expect_err("directory membership changes must stale the plan");
    assert!(error.to_string().contains("stale capture plan"));
    fs::remove_file(fixture.paths.project_root.join("docs/adr/007-new.md"))?;

    let review = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions),
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    let service = MemoryService::open_paths(fixture.paths.clone())?;
    let routed = service.apply_capture(
        "profile-reviewer",
        plan.clone(),
        review.clone(),
        &plan.plan_id,
        &review.review_id,
    )?;
    assert_eq!(routed.writes.len(), 4);
    assert!(read_okf_record_files(fixture.paths.records_dir())?.is_empty());
    assert_eq!(
        read_okf_proposal_files(fixture.paths.proposals_dir().join("pending"))?.len(),
        4
    );
    apply_pending_named(&service, &fixture.paths, "Accepted cache")?;
    let recalled = assert_cited_recall(
        &service,
        "acceptedadrrecalltoken",
        MemoryDestination::Repo,
        &review.review_id,
    )?;
    let capture = recalled.citations[0]
        .capture
        .as_ref()
        .expect("ADR recall must retain capture provenance");
    assert!(capture.evidence.iter().any(|evidence| matches!(
        evidence.semantic_location,
        Some(CaptureSemanticLocation::Adr { ref field, ref status, .. })
            if field == "decision" && status == "accepted"
    )));
    Ok(())
}

#[test]
fn supplied_diff_replay_is_exact_and_routes_only_reviewed_nonduplicate_candidates()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let diff = supplied_diff();
    let request = supplied_diff_request(diff);
    let mut inputs = CaptureSourceInputs::new();
    inputs.insert_supplied_bytes("reviewed-diff", diff.to_vec())?;
    let before = fixture.snapshot()?;
    let plan = plan_capture_with_inputs(&fixture.paths, request.clone(), &inputs)?;
    assert_eq!(
        before,
        fixture.snapshot()?,
        "supplied diff planning must be read-only"
    );
    assert_eq!(plan.status, CapturePlanStatus::Ready);
    assert_eq!(plan.sources[0].byte_length, diff.len() as u64);
    assert_eq!(
        plan.sources[0].source_content_hash,
        format!("blake3:{}", blake3::hash(diff).to_hex())
    );

    let review_candidates = candidates_named(&plan, "Review supplied changes");
    assert_eq!(review_candidates.len(), 3);
    assert!(matches!(
        review_candidates[0].action,
        CaptureAction::CreateProposal { .. }
    ));
    assert!(matches!(
        review_candidates[1].action,
        CaptureAction::Duplicate { .. }
    ));
    assert!(matches!(
        review_candidates[2].action,
        CaptureAction::Conflict { .. }
    ));
    let renamed = candidate_named(&plan, "Preserve renamed guidance");
    let rename_evidence = &renamed.evidence[0];
    assert_exact_evidence(diff, rename_evidence);
    assert!(matches!(
        rename_evidence.semantic_location,
        Some(CaptureSemanticLocation::GitChange {
            ref repository,
            ref base,
            ref head,
            ref old_blob,
            ref new_blob,
            ref old_path,
            ref new_path,
            ref change_kind,
            ref side,
            old_line_start: Some(1),
            old_line_end: Some(1),
            new_line_start: Some(2),
            new_line_end: Some(4),
            ..
        }) if repository == "."
            && base == BASE
            && head == HEAD
            && old_blob.as_deref() == Some("sha1:4444444444444444444444444444444444444444")
            && new_blob.as_deref() == Some("sha1:5555555555555555555555555555555555555555")
            && old_path.as_deref() == Some("docs/old.md")
            && new_path.as_deref() == Some("docs/new.md")
            && change_kind == "renamed"
            && side == "new"
    ));
    let deleted = candidate_named(&plan, "Deleted durable guidance");
    assert_eq!(
        deleted.classification.destination,
        MemoryDestination::NeedsReview
    );
    assert!(matches!(deleted.action, CaptureAction::NoWrite { .. }));
    assert_exact_evidence(diff, &deleted.evidence[0]);
    assert!(matches!(
        deleted.evidence[0].semantic_location,
        Some(CaptureSemanticLocation::GitChange {
            ref old_blob,
            new_blob: None,
            ref old_path,
            new_path: None,
            ref change_kind,
            ref side,
            old_line_start: Some(1),
            old_line_end: Some(2),
            new_line_start: None,
            new_line_end: None,
            ..
        }) if old_blob.as_deref() == Some("sha1:6666666666666666666666666666666666666666")
            && old_path.as_deref() == Some("docs/deleted.md")
            && change_kind == "deleted"
            && side == "old"
    ));
    assert!(!serde_json::to_string(&plan)?.contains("transientdiffcanary"));

    let decisions = plan
        .candidates
        .iter()
        .map(|candidate| CaptureReviewDecisionInput {
            candidate_id: candidate.candidate_id.clone(),
            outcome: if candidate.candidate_id == review_candidates[0].candidate_id {
                CaptureReviewOutcome::Accept
            } else if matches!(candidate.action, CaptureAction::Conflict { .. }) {
                CaptureReviewOutcome::Defer
            } else {
                CaptureReviewOutcome::Reject
            },
            reason_code: (candidate.candidate_id != review_candidates[0].candidate_id)
                .then(|| "not-selected-from-diff".to_owned()),
            memory: None,
            requested_destination: None,
        })
        .collect::<Vec<_>>();

    let mut wrong = CaptureSourceInputs::new();
    let mut changed = diff.to_vec();
    changed.extend_from_slice(b"\n# changed replay\n");
    wrong.insert_supplied_bytes("reviewed-diff", changed)?;
    let error = build_capture_review_with_inputs(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions.clone()),
        &wrong,
        "profile-reviewer",
        REVIEWED_AT,
    )
    .expect_err("changed supplied bytes must stale review");
    assert!(error.to_string().contains("stale capture plan"));

    let review = build_capture_review_with_inputs(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions),
        &inputs,
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    let service = MemoryService::open_paths(fixture.paths.clone())?;
    let before_failed_apply = fixture.snapshot()?;
    let error = service
        .apply_capture_with_inputs(
            "profile-reviewer",
            plan.clone(),
            review.clone(),
            &wrong,
            &plan.plan_id,
            &review.review_id,
        )
        .expect_err("changed supplied bytes must stale apply");
    assert!(error.to_string().contains("stale capture plan"));
    assert_eq!(
        before_failed_apply,
        fixture.snapshot()?,
        "stale apply must write nothing"
    );

    let routed = service.apply_capture_with_inputs(
        "profile-reviewer",
        plan.clone(),
        review.clone(),
        &inputs,
        &plan.plan_id,
        &review.review_id,
    )?;
    assert_eq!(routed.writes.len(), 1);
    assert!(read_okf_record_files(fixture.paths.records_dir())?.is_empty());
    assert_eq!(
        read_okf_proposal_files(fixture.paths.proposals_dir().join("pending"))?.len(),
        1
    );
    apply_pending_named(&service, &fixture.paths, "Review supplied changes")?;
    assert_cited_recall(
        &service,
        "supplieddiffrecalltoken",
        MemoryDestination::Repo,
        &review.review_id,
    )?;

    let mut duplicate_inputs = CaptureSourceInputs::new();
    duplicate_inputs.insert_supplied_bytes("reviewed-diff", diff.to_vec())?;
    let error = duplicate_inputs
        .insert_supplied_bytes("reviewed-diff", diff.to_vec())
        .expect_err("duplicate supplied source IDs must be rejected");
    assert!(error.to_string().contains("duplicated"));
    let mut extra_inputs = CaptureSourceInputs::new();
    extra_inputs.insert_supplied_bytes("reviewed-diff", diff.to_vec())?;
    extra_inputs.insert_supplied_bytes("ambient-extra", b"not requested".to_vec())?;
    let extra = plan_capture_with_inputs(&fixture.paths, request, &extra_inputs)?;
    assert_eq!(extra.status, CapturePlanStatus::Blocked);
    assert!(
        extra
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "source_preflight_failed"),
        "diagnostics: {:?}",
        extra.diagnostics
    );
    Ok(())
}

#[test]
fn git_range_is_pinned_read_only_reviewed_and_cited_with_delete_provenance() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    git(&fixture.paths.project_root, &["init", "-q"])?;
    fixture.write("docs/rules.md", "existing\n")?;
    fixture.write(
        "docs/deleted.md",
        "# Warning: Deleted guidance\nNever remove deleted guidance silently.\n",
    )?;
    fixture.write("docs/old-name.md", "unchanged rename body\n")?;
    fixture.write(
        "docs/replaced.md",
        "# Warning: Old warning\nOld warning body.\n",
    )?;
    git(&fixture.paths.project_root, &["add", "."])?;
    git_commit(&fixture.paths.project_root, "base")?;
    let base = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;

    fixture.write(
        "docs/rules.md",
        concat!(
            "existing\n",
            "# Procedure: Pinned Git guidance\n",
            "Use pinnedgitrecalltoken for range capture.\n\n",
            "# Procedure: Pinned Git guidance\n",
            "Use pinnedgitrecalltoken for range capture.\n\n",
            "# Warning: Rejected Git guidance\n",
            "Never retain rejectedgittoken after review.\n",
        ),
    )?;
    fixture.write(
        "docs/replaced.md",
        "# Warning: Replacement warning\nUse replacementwarningtoken.\n",
    )?;
    fs::remove_file(fixture.paths.project_root.join("docs/deleted.md"))?;
    fs::rename(
        fixture.paths.project_root.join("docs/old-name.md"),
        fixture.paths.project_root.join("docs/new-name.md"),
    )?;
    git(&fixture.paths.project_root, &["add", "-A"])?;
    git_commit(&fixture.paths.project_root, "capturable head")?;
    let head = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;
    let request = git_range_request(&base, &head, "base_to_head", true);

    let before = fixture.snapshot()?;
    let plan = plan_capture(&fixture.paths, request)?;
    assert_eq!(
        before,
        fixture.snapshot()?,
        "git_range planning must be read-only"
    );
    assert_eq!(plan.status, CapturePlanStatus::Ready);
    assert!(plan.sources[0].policy_inputs.iter().any(|input| {
        input
            .engine_version
            .starts_with("memzoi/git-unified-renderer-v1+git-")
    }));
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "git_rename_without_durable_candidate" })
    );
    let deleted = candidate_named(&plan, "Deleted guidance");
    assert_eq!(
        deleted.classification.destination,
        MemoryDestination::NeedsReview
    );
    assert!(matches!(deleted.action, CaptureAction::NoWrite { .. }));
    assert!(matches!(
        deleted.evidence[0].semantic_location,
        Some(CaptureSemanticLocation::GitChange {
            ref old_blob,
            new_blob: None,
            ref old_path,
            new_path: None,
            ref change_kind,
            ref side,
            old_line_start: Some(1),
            old_line_end: Some(2),
            new_line_start: None,
            new_line_end: None,
            ..
        }) if old_blob.as_deref().is_some_and(|value| value.starts_with("sha1:"))
            && old_path.as_deref() == Some("docs/deleted.md")
            && change_kind == "deleted"
            && side == "old"
    ));

    let pinned = candidates_named(&plan, "Pinned Git guidance");
    assert_eq!(pinned.len(), 2);
    assert!(matches!(
        pinned[0].action,
        CaptureAction::CreateProposal { .. }
    ));
    assert!(matches!(pinned[1].action, CaptureAction::Duplicate { .. }));
    let replacement = candidate_named(&plan, "Replacement warning");
    assert!(matches!(
        replacement.evidence[0].semantic_location,
        Some(CaptureSemanticLocation::GitChange {
            base: ref evidence_base,
            head: ref evidence_head,
            ref old_blob,
            ref new_blob,
            ref old_path,
            ref new_path,
            ref change_kind,
            ref side,
            old_line_start: Some(1),
            old_line_end: Some(2),
            new_line_start: Some(1),
            new_line_end: Some(2),
            ..
        }) if evidence_base == &format!("sha1:{base}")
            && evidence_head == &format!("sha1:{head}")
            && old_blob.as_deref().is_some_and(|value| value.starts_with("sha1:"))
            && new_blob.as_deref().is_some_and(|value| value.starts_with("sha1:"))
            && old_path.as_deref() == Some("docs/replaced.md")
            && new_path.as_deref() == Some("docs/replaced.md")
            && change_kind == "modified"
            && side == "new"
    ));

    fixture.write(
        "moving-head.txt",
        "moving HEAD must not alter pinned objects\n",
    )?;
    git(&fixture.paths.project_root, &["add", "moving-head.txt"])?;
    git_commit(&fixture.paths.project_root, "move HEAD")?;

    let decisions = plan
        .candidates
        .iter()
        .map(|candidate| CaptureReviewDecisionInput {
            candidate_id: candidate.candidate_id.clone(),
            outcome: if candidate.candidate_id == pinned[0].candidate_id {
                CaptureReviewOutcome::Accept
            } else {
                CaptureReviewOutcome::Reject
            },
            reason_code: (candidate.candidate_id != pinned[0].candidate_id)
                .then(|| "not-selected-from-range".to_owned()),
            memory: None,
            requested_destination: None,
        })
        .collect();
    let review = build_capture_review(
        &fixture.paths,
        &plan,
        review_input(&plan, decisions),
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    let service = MemoryService::open_paths(fixture.paths.clone())?;
    let routed = service.apply_capture(
        "profile-reviewer",
        plan.clone(),
        review.clone(),
        &plan.plan_id,
        &review.review_id,
    )?;
    assert_eq!(routed.writes.len(), 1);
    assert!(read_okf_record_files(fixture.paths.records_dir())?.is_empty());
    apply_pending_named(&service, &fixture.paths, "Pinned Git guidance")?;
    assert_cited_recall(
        &service,
        "pinnedgitrecalltoken",
        MemoryDestination::Repo,
        &review.review_id,
    )?;
    assert!(search(&service, "rejectedgittoken", MemoryDestination::Repo)?.is_empty());
    Ok(())
}

#[test]
fn git_range_first_parent_is_explicit_and_missing_or_different_repositories_stale()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    git(&fixture.paths.project_root, &["init", "-q"])?;
    fixture.write("README.md", "base\n")?;
    git(&fixture.paths.project_root, &["add", "README.md"])?;
    git_commit(&fixture.paths.project_root, "base")?;
    let base = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;
    let primary = git_stdout(
        &fixture.paths.project_root,
        &["symbolic-ref", "--short", "HEAD"],
    )?;

    git(
        &fixture.paths.project_root,
        &["checkout", "-q", "-b", "feature", &base],
    )?;
    fixture.write(
        "docs/merge.md",
        "# Decision: Selected merge parent\nUse selectedmergeparenttoken.\n",
    )?;
    git(&fixture.paths.project_root, &["add", "docs/merge.md"])?;
    git_commit(&fixture.paths.project_root, "feature")?;
    git(&fixture.paths.project_root, &["checkout", "-q", &primary])?;
    fixture.write("main.txt", "first parent\n")?;
    git(&fixture.paths.project_root, &["add", "main.txt"])?;
    git_commit(&fixture.paths.project_root, "first parent")?;
    let first_parent = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;
    git_merge(&fixture.paths.project_root, "feature", "merge feature")?;
    let merge = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;

    let request = git_range_request(&first_parent, &merge, "first_parent", true);
    let before = fixture.snapshot()?;
    let plan = plan_capture(&fixture.paths, request)?;
    assert_eq!(before, fixture.snapshot()?);
    assert_eq!(plan.candidates.len(), 1);
    assert!(
        candidate_named(&plan, "Selected merge parent")
            .memory
            .body
            .contains("selectedmergeparenttoken")
    );
    assert!(matches!(
        plan.sources[0].locator,
        CaptureSourceLocator::GitRange { ref merge_parent, .. }
            if merge_parent == "first_parent"
    ));
    let review = build_capture_review(
        &fixture.paths,
        &plan,
        reject_all(&plan),
        "profile-reviewer",
        REVIEWED_AT,
    )?;
    assert_eq!(review.decisions[0].outcome, CaptureReviewOutcome::Reject);

    let abbreviated = git_range_request(&first_parent[..12], &merge, "base_to_head", true);
    let abbreviated = plan_capture(&fixture.paths, abbreviated)?;
    assert_eq!(abbreviated.status, CapturePlanStatus::Blocked);
    assert!(
        abbreviated
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "source_preflight_failed" })
    );
    let missing = "3333333333333333333333333333333333333333";
    let missing_plan = plan_capture(
        &fixture.paths,
        git_range_request(&first_parent, missing, "base_to_head", true),
    )?;
    assert_eq!(missing_plan.status, CapturePlanStatus::Blocked);
    assert_eq!(missing_plan.data_class, CaptureDataClass::Blocked);
    assert!(!missing_plan.diagnostics.is_empty());

    let missing_repo_fixture = simple_git_plan_fixture()?;
    let (missing_plan, _) = pinned_plan(&missing_repo_fixture)?;
    fs::rename(
        missing_repo_fixture.paths.project_root.join(".git"),
        missing_repo_fixture.temp_path().join("missing.git"),
    )?;
    let before_review = missing_repo_fixture.snapshot()?;
    let error = build_capture_review(
        &missing_repo_fixture.paths,
        &missing_plan,
        accept_all(&missing_plan),
        "profile-reviewer",
        REVIEWED_AT,
    )
    .expect_err("a missing repository must stale review");
    assert!(error.to_string().contains("stale capture plan"));
    assert_eq!(before_review, missing_repo_fixture.snapshot()?);
    assert!(
        read_okf_proposal_files(missing_repo_fixture.paths.proposals_dir().join("pending"))?
            .is_empty()
    );

    let different_repo_fixture = simple_git_plan_fixture()?;
    let (different_plan, _) = pinned_plan(&different_repo_fixture)?;
    let clone = different_repo_fixture.temp_path().join("clone");
    git_clone(&different_repo_fixture.paths.project_root, &clone)?;
    fs::rename(
        different_repo_fixture.paths.project_root.join(".git"),
        different_repo_fixture.temp_path().join("original.git"),
    )?;
    fs::rename(
        clone.join(".git"),
        different_repo_fixture.paths.project_root.join(".git"),
    )?;
    let before_review = different_repo_fixture.snapshot()?;
    let error = build_capture_review(
        &different_repo_fixture.paths,
        &different_plan,
        accept_all(&different_plan),
        "profile-reviewer",
        REVIEWED_AT,
    )
    .expect_err("a different repository instance must stale review");
    assert!(error.to_string().contains("stale capture plan"));
    assert_eq!(before_review, different_repo_fixture.snapshot()?);
    assert!(
        read_okf_proposal_files(different_repo_fixture.paths.proposals_dir().join("pending"))?
            .is_empty()
    );
    Ok(())
}

struct Fixture {
    temp: TempDir,
    paths: MemoryPaths,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let root = temp.path().join("project");
        fs::create_dir_all(&root)?;
        let paths =
            MemoryPaths::with_runtime_home(root.canonicalize()?, temp.path().join("runtime"));
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        Ok(Self { temp, paths })
    }

    fn write(&self, relative: &str, body: &str) -> anyhow::Result<()> {
        let path = self.paths.project_root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, body)?;
        Ok(())
    }

    fn snapshot(&self) -> anyhow::Result<BTreeMap<PathBuf, Vec<u8>>> {
        file_snapshot(self.temp.path())
    }

    fn temp_path(&self) -> &Path {
        self.temp.path()
    }
}

fn project_path_request(source_id: &str, path: &str, profile: &str) -> CaptureRequest {
    CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: source_id.to_owned(),
            locator: CaptureSourceLocator::ProjectPath {
                path: path.to_owned(),
            },
            media_type: if profile == GIT_CHANGE_EXTRACTOR_PROFILE {
                "text/x-diff"
            } else {
                "text/markdown"
            }
            .to_owned(),
            git: None,
        }],
        extractor: CaptureExtractorRequest {
            profile: profile.to_owned(),
        },
    }
}

fn adr_directory_request() -> CaptureRequest {
    CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: "adrs".to_owned(),
            locator: CaptureSourceLocator::ProjectDirectory {
                path: "docs/adr".to_owned(),
                recursive: false,
                ignore_policy: "git-v1".to_owned(),
                include: vec!["*.md".to_owned()],
            },
            media_type: "text/markdown".to_owned(),
            git: None,
        }],
        extractor: CaptureExtractorRequest {
            profile: ADR_EXTRACTOR_PROFILE.to_owned(),
        },
    }
}

fn supplied_diff() -> &'static [u8] {
    concat!(
        "diff --git a/docs/review.md b/docs/review.md\n",
        "index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n",
        "--- a/docs/review.md\n",
        "+++ b/docs/review.md\n",
        "@@ -1 +1,10 @@\n",
        " existing\n",
        "+# Procedure: Review supplied changes\n",
        "+Use supplieddiffrecalltoken before applying changes.\n",
        "+\n",
        "+# Procedure: Review supplied changes\n",
        "+Use supplieddiffrecalltoken before applying changes.\n",
        "+\n",
        "+# Procedure: Review supplied changes\n",
        "+Apply changes without review.\n",
        "+\n",
        "diff --git a/docs/old.md b/docs/new.md\n",
        "similarity index 80%\n",
        "rename from docs/old.md\n",
        "rename to docs/new.md\n",
        "index 4444444444444444444444444444444444444444..5555555555555555555555555555555555555555 100644\n",
        "--- a/docs/old.md\n",
        "+++ b/docs/new.md\n",
        "@@ -1 +1,4 @@\n",
        " existing\n",
        "+# Decision: Preserve renamed guidance\n",
        "+Use renamedguidancetoken at the new path.\n",
        "+\n",
        "diff --git a/docs/deleted.md b/docs/deleted.md\n",
        "deleted file mode 100644\n",
        "index 6666666666666666666666666666666666666666..0000000000000000000000000000000000000000\n",
        "--- a/docs/deleted.md\n",
        "+++ /dev/null\n",
        "@@ -1,2 +0,0 @@\n",
        "-# Warning: Deleted durable guidance\n",
        "-Never remove this without review.\n",
        "diff --git a/src/transient.rs b/src/transient.rs\n",
        "new file mode 100644\n",
        "index 0000000000000000000000000000000000000000..7777777777777777777777777777777777777777\n",
        "--- /dev/null\n",
        "+++ b/src/transient.rs\n",
        "@@ -0,0 +1 @@\n",
        "+transientdiffcanary\n",
    )
    .as_bytes()
}

fn supplied_diff_request(bytes: &[u8]) -> CaptureRequest {
    CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: "reviewed-diff".to_owned(),
            locator: CaptureSourceLocator::SuppliedBytes {
                display_name: "reviewed.diff".to_owned(),
                media_type: "text/x-diff".to_owned(),
                byte_length: bytes.len() as u64,
                source_content_hash: format!("blake3:{}", blake3::hash(bytes).to_hex()),
            },
            media_type: "text/x-diff".to_owned(),
            git: Some(CaptureGitSourceContext {
                repository: ".".to_owned(),
                base: BASE.to_owned(),
                head: HEAD.to_owned(),
            }),
        }],
        extractor: CaptureExtractorRequest {
            profile: GIT_CHANGE_EXTRACTOR_PROFILE.to_owned(),
        },
    }
}

fn git_range_request(
    base: &str,
    head: &str,
    merge_parent: &str,
    rename_detection: bool,
) -> CaptureRequest {
    CaptureRequest {
        schema: CAPTURE_REQUEST_SCHEMA.to_owned(),
        sources: vec![CaptureSourceRequest {
            source_id: "range".to_owned(),
            locator: CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: merge_parent.to_owned(),
                rename_detection,
                diff_format: "git-unified-v1".to_owned(),
            },
            media_type: "text/x-diff".to_owned(),
            git: None,
        }],
        extractor: CaptureExtractorRequest {
            profile: GIT_CHANGE_EXTRACTOR_PROFILE.to_owned(),
        },
    }
}

fn candidates_named<'a>(plan: &'a CapturePlan, title: &str) -> Vec<&'a CaptureCandidate> {
    plan.candidates
        .iter()
        .filter(|candidate| candidate.memory.title == title)
        .collect()
}

fn candidate_named<'a>(plan: &'a CapturePlan, title: &str) -> &'a CaptureCandidate {
    let candidates = candidates_named(plan, title);
    assert_eq!(candidates.len(), 1, "expected one candidate named {title}");
    candidates[0]
}

fn adr_evidence<'a>(
    candidate: &'a CaptureCandidate,
    expected_field: &str,
    expected_status: &str,
    expected_target: Option<&str>,
) -> &'a CaptureEvidence {
    candidate
        .evidence
        .iter()
        .find(|evidence| {
            matches!(
                &evidence.semantic_location,
                Some(CaptureSemanticLocation::Adr { field, status, target })
                    if field == expected_field
                        && status == expected_status
                        && target.as_deref() == expected_target
            )
        })
        .unwrap_or_else(|| panic!("missing ADR {expected_field}/{expected_status} evidence"))
}

fn assert_exact_evidence(source: &[u8], evidence: &CaptureEvidence) {
    let start = usize::try_from(evidence.span.byte_start).expect("evidence offset fits usize");
    let end = usize::try_from(evidence.span.byte_end).expect("evidence offset fits usize");
    let exact = &source[start..end];
    assert_eq!(evidence.text.as_deref().map(str::as_bytes), Some(exact));
    assert_eq!(
        evidence.evidence_content_hash,
        format!("blake3:{}", blake3::hash(exact).to_hex())
    );
}

fn accept_all(plan: &CapturePlan) -> CaptureReviewInput {
    review_input(
        plan,
        plan.candidates
            .iter()
            .map(|candidate| CaptureReviewDecisionInput {
                candidate_id: candidate.candidate_id.clone(),
                outcome: CaptureReviewOutcome::Accept,
                reason_code: None,
                memory: None,
                requested_destination: None,
            })
            .collect(),
    )
}

fn reject_all(plan: &CapturePlan) -> CaptureReviewInput {
    review_input(
        plan,
        plan.candidates
            .iter()
            .map(|candidate| CaptureReviewDecisionInput {
                candidate_id: candidate.candidate_id.clone(),
                outcome: CaptureReviewOutcome::Reject,
                reason_code: Some("test-rejection".to_owned()),
                memory: None,
                requested_destination: None,
            })
            .collect(),
    )
}

fn review_input(
    plan: &CapturePlan,
    decisions: Vec<CaptureReviewDecisionInput>,
) -> CaptureReviewInput {
    CaptureReviewInput {
        schema: CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
        plan_id: plan.plan_id.clone(),
        prior_review_id: None,
        decisions,
    }
}

fn apply_pending_named(
    service: &MemoryService,
    paths: &MemoryPaths,
    title: &str,
) -> anyhow::Result<()> {
    let pending = paths.proposals_dir().join("pending");
    let proposal = read_okf_proposal_files(&pending)?
        .into_iter()
        .find(|proposal| proposal.title == title)
        .unwrap_or_else(|| panic!("missing pending proposal named {title}"));
    service.apply_file_proposal(
        pending.join(format!("{}.md", proposal.file_id)),
        "proposal-maintainer",
    )?;
    Ok(())
}

fn search(
    service: &MemoryService,
    query: &str,
    destination: MemoryDestination,
) -> anyhow::Result<Vec<memzoi_core::SearchResult>> {
    service.search_memory(SearchInput {
        query: query.to_owned(),
        destination: Some(destination),
        limit: 10,
        ..SearchInput::default()
    })
}

fn assert_cited_recall(
    service: &MemoryService,
    query: &str,
    destination: MemoryDestination,
    review_id: &str,
) -> anyhow::Result<memzoi_core::SearchResult> {
    let mut recalled = search(service, query, destination)?;
    assert_eq!(recalled.len(), 1, "expected exactly one result for {query}");
    let result = recalled.remove(0);
    assert_eq!(
        result
            .record
            .capture
            .as_ref()
            .expect("record capture provenance")
            .review_id,
        review_id
    );
    assert_eq!(result.citations.len(), 1);
    assert_eq!(
        result.citations[0]
            .capture
            .as_ref()
            .expect("citation capture provenance")
            .review_id,
        review_id
    );
    Ok(result)
}

fn simple_git_plan_fixture() -> anyhow::Result<Fixture> {
    let fixture = Fixture::new()?;
    git(&fixture.paths.project_root, &["init", "-q"])?;
    fixture.write("docs/rules.md", "existing\n")?;
    git(&fixture.paths.project_root, &["add", "docs/rules.md"])?;
    git_commit(&fixture.paths.project_root, "base")?;
    fixture.write(
        "docs/rules.md",
        "existing\n# Procedure: Repository identity\nUse repositoryidentitytoken.\n",
    )?;
    git(&fixture.paths.project_root, &["add", "docs/rules.md"])?;
    git_commit(&fixture.paths.project_root, "head")?;
    Ok(fixture)
}

fn pinned_plan(fixture: &Fixture) -> anyhow::Result<(CapturePlan, (String, String))> {
    let head = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD"])?;
    let base = git_stdout(&fixture.paths.project_root, &["rev-parse", "HEAD^"])?;
    let plan = plan_capture(
        &fixture.paths,
        git_range_request(&base, &head, "base_to_head", true),
    )?;
    Ok((plan, (base, head)))
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = git_command(root, args).status()?;
    anyhow::ensure!(status.success(), "git command failed: {args:?}");
    Ok(())
}

fn git_stdout(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = git_command(root, args).output()?;
    anyhow::ensure!(output.status.success(), "git command failed: {args:?}");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn git_commit(root: &Path, message: &str) -> anyhow::Result<()> {
    let status = git_identity_command(root, &["commit", "-q", "-m", message]).status()?;
    anyhow::ensure!(status.success(), "git commit failed");
    Ok(())
}

fn git_merge(root: &Path, branch: &str, message: &str) -> anyhow::Result<()> {
    let status =
        git_identity_command(root, &["merge", "-q", "--no-ff", branch, "-m", message]).status()?;
    anyhow::ensure!(status.success(), "git merge failed");
    Ok(())
}

fn git_clone(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(["clone", "-q", "--no-hardlinks"])
        .arg(source)
        .arg(destination)
        .status()?;
    anyhow::ensure!(status.success(), "git clone failed");
    Ok(())
}

fn git_identity_command(root: &Path, args: &[&str]) -> Command {
    let mut command = git_command(root, args);
    command
        .env("GIT_AUTHOR_NAME", "Memzoi Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Memzoi Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-07-11T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-07-11T12:00:00Z");
    command
}

fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    command
}

fn file_snapshot(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(
        root: &Path,
        current: &Path,
        snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, snapshot)?;
            } else if metadata.is_file() {
                snapshot.insert(path.strip_prefix(root)?.to_owned(), fs::read(&path)?);
            } else if metadata.file_type().is_symlink() {
                snapshot.insert(
                    path.strip_prefix(root)?.to_owned(),
                    fs::read_link(&path)?
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                );
            }
        }
        Ok(())
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot)?;
    Ok(snapshot)
}
