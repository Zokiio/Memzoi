use std::path::PathBuf;

use memzoi_core::{RECALL_EVAL_CORPUS_VERSION, RECALL_EVAL_REPORT_VERSION, run_recall_eval};

#[test]
fn checked_recall_corpus_passes_with_observable_citations() -> anyhow::Result<()> {
    let report = run_recall_eval(checked_corpus())?;

    assert_eq!(report.version, RECALL_EVAL_REPORT_VERSION);
    assert_eq!(report.corpus_version, RECALL_EVAL_CORPUS_VERSION);
    assert_eq!(report.evaluated_at, "2026-07-10T12:00:00Z");
    assert!(report.aggregate.passed, "{report:#?}");
    assert_eq!(report.aggregate.mean_recall_at_k, 1.0);
    assert_eq!(report.aggregate.mean_mrr, 1.0);
    assert_eq!(report.aggregate.total_forbidden_hits, 0);

    let citation_case = report
        .cases
        .iter()
        .find(|case| case.id == "citation-round-trip")
        .expect("citation case");
    let citation = citation_case
        .retrieved
        .first()
        .and_then(|retrieved| retrieved.citations.first())
        .expect("retrieved citation");
    assert_eq!(citation.record_id, "citation-target");
    assert_eq!(citation.source_ref.as_deref(), Some("issue://44"));
    assert_eq!(citation.path.as_deref(), Some("docs/roadmap.md"));

    let json = serde_json::to_string(&report)?;
    assert!(!json.contains("runtime-home"));
    assert!(!json.contains("memory.db"));
    Ok(())
}

fn checked_corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v1/corpus.yaml")
}
