use std::{fs, path::PathBuf};

use memzoi_core::{
    CAPTURE_EVAL_BASELINE_VERSION, CAPTURE_EVAL_CORPUS_VERSION,
    CAPTURE_EVAL_METRIC_DEFINITIONS_VERSION, CAPTURE_EVAL_REPORT_VERSION,
    CaptureEvalBaselineStatus, attach_capture_eval_baseline, run_capture_eval,
    write_capture_eval_baseline,
};

#[test]
fn checked_capture_corpus_passes_quality_safety_and_review_burden_gates() -> anyhow::Result<()> {
    let mut report = run_capture_eval(checked_corpus())?;

    assert_eq!(report.version, CAPTURE_EVAL_REPORT_VERSION);
    assert_eq!(report.corpus.version, CAPTURE_EVAL_CORPUS_VERSION);
    assert_eq!(
        report.definitions.version,
        CAPTURE_EVAL_METRIC_DEFINITIONS_VERSION
    );
    assert!(report.gates_passed, "{report:#?}");
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.metrics.candidate_precision.value, 1.0);
    assert_eq!(report.metrics.candidate_recall.value, 1.0);
    assert_eq!(report.metrics.evidence_validity.rate, 1.0);
    assert_eq!(report.metrics.destination_accuracy.rate, 1.0);
    assert_eq!(report.metrics.sensitivity_accuracy.rate, 1.0);
    assert_eq!(report.metrics.action_accuracy.rate, 1.0);
    assert_eq!(report.metrics.forbidden_hits.hits, 0);
    assert_eq!(report.metrics.forbidden_hits.opportunities, 4);
    assert_eq!(report.metrics.unsupported_outcome_accuracy.rate, 1.0);
    assert_eq!(report.metrics.case_pass_rate.value, 1.0);
    assert!(report.observations.provider.is_none());
    assert!(report.observations.model.is_none());
    assert!(report.observations.token_usage.is_none());
    assert!(report.observations.cost.is_none());
    assert!(report.hard_gates.passed());
    assert_eq!(report.metrics.review_burden.proposed, 25);
    assert_eq!(report.metrics.review_burden.accepted, 11);
    assert_eq!(report.metrics.review_burden.rejected, 9);
    assert_eq!(report.metrics.review_burden.edited, 1);
    assert_eq!(report.metrics.review_burden.deferred, 4);
    assert_eq!(report.metrics.review_burden.duplicates, 5);
    assert_eq!(report.metrics.review_burden.conflicts, 4);
    assert_eq!(report.metrics.review_burden.needs_review, 8);
    assert_eq!(report.hard_gates.review_workflow_violations, 0);
    assert_eq!(report.hard_gates.provenance_violations, 0);
    assert_eq!(report.hard_gates.unnamed_source_reads, 0);
    assert_eq!(report.hard_gates.stale_review_acceptance_violations, 0);
    assert_eq!(report.hard_gates.stale_apply_acceptance_violations, 0);
    assert_eq!(report.hard_gates.stale_write_violations, 0);
    assert_eq!(report.hard_gates.direct_canonical_write_violations, 0);
    for case in &report.cases {
        for assertion in [
            "review_workflow",
            "apply_workflow",
            "provenance_and_citations",
            "repo_candidates_proposal_only",
            "policy_inputs_declared",
        ] {
            assert_eq!(
                case.assertions.get(assertion),
                Some(&true),
                "{} failed {assertion}",
                case.id
            );
        }
    }
    for (profile, metrics) in &report.profile_metrics {
        assert!(
            metrics.forbidden_hits.opportunities > 0,
            "{profile} has no forbidden-candidate opportunity"
        );
    }
    let stale = report
        .cases
        .iter()
        .find(|case| case.id == "stale-source-rejected-at-review-and-apply")
        .expect("stale workflow case");
    for assertion in [
        "stale_review_rejected",
        "stale_apply_rejected",
        "stale_no_write",
    ] {
        assert_eq!(stale.assertions.get(assertion), Some(&true));
    }

    if std::env::var_os("MEMZOI_UPDATE_CAPTURE_EVAL_BASELINE").is_some() {
        write_capture_eval_baseline(&report, checked_baseline())?;
    }
    attach_capture_eval_baseline(&mut report, checked_baseline())?;
    let baseline = report.baseline.as_ref().expect("baseline comparison");
    assert_eq!(baseline.status, CaptureEvalBaselineStatus::Match);
    assert!(baseline.deterministic_match);
    assert!(report.passed);

    let json = serde_json::to_string(&report)?;
    for prohibited in [
        "capture-eval-redaction-canary",
        "capture-policy-redaction-canary",
        "notes/synthetic-secret.md",
        "runtime-home",
        "memory.db",
    ] {
        assert!(!json.contains(prohibited), "report leaked {prohibited}");
    }
    Ok(())
}

#[test]
fn compatible_but_changed_capture_baseline_fails_exact_match() -> anyhow::Result<()> {
    let report = run_capture_eval(checked_corpus())?;
    let temp = tempfile::tempdir()?;
    let baseline = temp.path().join("baseline.json");
    write_capture_eval_baseline(&report, &baseline)?;

    let mut changed = report;
    changed.metrics.case_count += 1;
    attach_capture_eval_baseline(&mut changed, &baseline)?;

    let comparison = changed.baseline.as_ref().expect("baseline comparison");
    assert_eq!(comparison.status, CaptureEvalBaselineStatus::Changed);
    assert!(!comparison.deterministic_match);
    assert!(!changed.passed);
    Ok(())
}

#[test]
fn failing_capture_report_cannot_replace_a_baseline() -> anyhow::Result<()> {
    let mut report = run_capture_eval(checked_corpus())?;
    report.gates_passed = false;
    report.passed = false;
    let temp = tempfile::tempdir()?;
    let baseline = temp.path().join("baseline.json");
    fs::write(&baseline, "baseline sentinel")?;

    let error = write_capture_eval_baseline(&report, &baseline)
        .expect_err("failed capture gates must not update a baseline");

    assert!(error.to_string().contains("gates failed"));
    assert_eq!(fs::read_to_string(baseline)?, "baseline sentinel");
    Ok(())
}

#[test]
fn capture_baseline_schema_version_is_explicit() -> anyhow::Result<()> {
    let report = run_capture_eval(checked_corpus())?;
    let temp = tempfile::tempdir()?;
    let baseline_path = temp.path().join("baseline.json");
    write_capture_eval_baseline(&report, &baseline_path)?;
    let baseline: serde_json::Value = serde_json::from_slice(&fs::read(baseline_path)?)?;
    assert_eq!(
        baseline.get("version").and_then(serde_json::Value::as_str),
        Some(CAPTURE_EVAL_BASELINE_VERSION)
    );
    Ok(())
}

#[test]
fn required_profiles_without_cases_cannot_run_or_replace_the_baseline() -> anyhow::Result<()> {
    let (_temp, corpus) = copied_corpus()?;
    replace_once(
        &corpus,
        "thresholds:\n",
        concat!(
            "  - profile: empty-required-profile\n",
            "    required: true\n",
            "    extractor_kind: deterministic\n",
            "    extractor_id: memzoi-empty\n",
            "    extractor_version: 1.0.0\n",
            "thresholds:\n",
        ),
    )?;
    let baseline = corpus
        .parent()
        .expect("corpus parent")
        .join("baseline.json");
    let before = fs::read(&baseline)?;

    let error = run_capture_eval(&corpus)
        .expect_err("a required profile without cases must fail corpus validation");

    assert!(error.to_string().contains("must define at least one case"));
    assert_eq!(fs::read(baseline)?, before);
    Ok(())
}

#[test]
fn required_profiles_without_forbidden_opportunities_cannot_run() -> anyhow::Result<()> {
    let (_temp, corpus) = copied_corpus()?;
    replace_once(
        &corpus,
        "coverage: [feedback_loop, forbidden]",
        "coverage: [feedback_loop]",
    )?;

    let error = run_capture_eval(&corpus)
        .expect_err("every required profile must declare a forbidden opportunity");

    assert!(error.to_string().contains("missing Forbidden coverage"));
    Ok(())
}

#[test]
fn locator_hash_and_semantic_evidence_mismatches_fail_exact_candidate_matching()
-> anyhow::Result<()> {
    let mutations = [
        (
            "locator: {kind: project_path, path: notes/typed.md}\n              source_content_hash: blake3:dd96c45e68baa7b3e15f89c5cfa17fb6ddcbff7b35445e0471dfba80fbdf856b\n              span: {byte_start: 22",
            "locator: {kind: project_path, path: notes/mixed.md}\n              source_content_hash: blake3:dd96c45e68baa7b3e15f89c5cfa17fb6ddcbff7b35445e0471dfba80fbdf856b\n              span: {byte_start: 22",
            "typed-markdown-candidates",
            "deterministic-fact",
        ),
        (
            "source_content_hash: blake3:dd96c45e68baa7b3e15f89c5cfa17fb6ddcbff7b35445e0471dfba80fbdf856b\n              span: {byte_start: 22",
            "source_content_hash: blake3:dd96c45e68baa7b3e15f89c5cfa17fb6ddcbff7b35445e0471dfba80fbdf856a\n              span: {byte_start: 22",
            "typed-markdown-candidates",
            "deterministic-fact",
        ),
        (
            "semantic_location: {kind: adr, field: decision, status: accepted}",
            "semantic_location: {kind: adr, field: context, status: accepted}",
            "accepted-adr-decision",
            "adr-evidence-decision",
        ),
        (
            "hunk: blake3:cc457e8ae69846a22e3109b6bc4ecf8a1dbdb834610d903a0e22923ebff1b92c",
            "hunk: blake3:ac457e8ae69846a22e3109b6bc4ecf8a1dbdb834610d903a0e22923ebff1b92c",
            "explicit-git-diff-guidance",
            "git-preserve-evidence-warning",
        ),
    ];

    for (from, to, case_id, candidate_id) in mutations {
        let (_temp, corpus) = copied_corpus()?;
        replace_once(&corpus, from, to)?;
        let report = run_capture_eval(&corpus)?;
        let case = report
            .cases
            .iter()
            .find(|case| case.id == case_id)
            .expect("mutated case report");
        assert!(!case.passed, "{case_id} unexpectedly passed");
        assert!(
            case.missing_candidates
                .iter()
                .any(|candidate| candidate == candidate_id),
            "{case_id} did not reject the mismatched evidence"
        );
        assert!(!report.gates_passed);
    }
    Ok(())
}

fn checked_corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/capture/v1/corpus.yaml")
}

fn checked_baseline() -> PathBuf {
    if let Some(path) = std::env::var_os("MEMZOI_CAPTURE_EVAL_BASELINE_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/capture/v1/baseline.json")
}

fn copied_corpus() -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let destination = temp.path().join("capture-v1");
    copy_directory(
        checked_corpus().parent().expect("checked corpus parent"),
        &destination,
    )?;
    Ok((temp, destination.join("corpus.yaml")))
}

fn copy_directory(source: &std::path::Path, destination: &std::path::Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn replace_once(path: &std::path::Path, from: &str, to: &str) -> anyhow::Result<()> {
    let original = fs::read_to_string(path)?;
    assert_eq!(original.matches(from).count(), 1, "mutation must be unique");
    fs::write(path, original.replacen(from, to, 1))?;
    Ok(())
}
