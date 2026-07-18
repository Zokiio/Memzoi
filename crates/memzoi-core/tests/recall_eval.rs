use std::path::PathBuf;

use memzoi_core::{
    RECALL_EVAL_CORPUS_VERSION, RECALL_EVAL_METRIC_DEFINITIONS_VERSION, RECALL_EVAL_REPORT_VERSION,
    RecallEvalBaselineStatus, attach_recall_eval_baseline, run_recall_eval,
};

#[test]
fn checked_trust_corpus_passes_with_safety_and_integrity_metrics() -> anyhow::Result<()> {
    let mut report = run_recall_eval(checked_corpus())?;

    assert_eq!(report.version, RECALL_EVAL_REPORT_VERSION);
    assert_eq!(report.corpus.version, RECALL_EVAL_CORPUS_VERSION);
    assert_eq!(
        report.definitions.version,
        RECALL_EVAL_METRIC_DEFINITIONS_VERSION
    );
    assert_eq!(report.corpus.evaluated_at, "2026-07-10T12:00:00Z");
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.metrics.search.mean_recall_at_k, 1.0);
    assert_eq!(report.metrics.search.mean_mrr, 1.0);
    assert_eq!(report.metrics.precheck.precision.value, 1.0);
    assert_eq!(report.metrics.precheck.recall.value, 1.0);
    assert_eq!(report.metrics.leakage.stale.hits, 0);
    assert_eq!(report.metrics.leakage.expired.hits, 0);
    assert_eq!(report.metrics.leakage.scope.hits, 0);
    assert_eq!(report.metrics.leakage.forbidden.hits, 0);
    assert_eq!(report.metrics.citation_integrity.rate, 1.0);
    assert_eq!(report.metrics.provenance_integrity.rate, 1.0);
    assert_eq!(
        report.runtime.token_estimator,
        "context_pack/approx_words-v1"
    );
    assert_eq!(report.runtime.latency_sample_count, report.cases.len());

    let proposal = report
        .proposal_fixtures
        .iter()
        .find(|fixture| fixture.record_id == "proposal-evidence-round-trip")
        .expect("proposal evidence fixture report");
    assert!(proposal.passed);
    assert!(proposal.resolution_preserved);
    assert_eq!(
        proposal.source_ref,
        "docs/rfcs/0001-evidence-backed-capture.md"
    );
    assert!(proposal.lineage_separate_from_evidence);

    attach_recall_eval_baseline(&mut report, checked_baseline())?;
    let baseline = report.baseline.as_ref().expect("baseline comparison");
    assert_eq!(baseline.status, RecallEvalBaselineStatus::Match);
    assert!(baseline.deterministic_match);
    assert!(report.passed);

    let json = serde_json::to_string(&report)?;
    assert!(!json.contains("runtime-home"));
    assert!(!json.contains("memory.db"));
    Ok(())
}

fn checked_corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/quality/corpus.yaml")
}

fn checked_baseline() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/quality/baseline.json")
}
