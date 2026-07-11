use memzoi_core::{
    RECALL_COMPETITOR_REPORT_VERSION, RECALL_INTERNAL_GATE_STATEMENT, run_recall_competitor_eval,
};

#[test]
fn two_track_competitor_fixture_is_complete_and_reproducible() -> anyhow::Result<()> {
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/competitors/fixture-evidence.json");
    let report = run_recall_competitor_eval(evidence)?;

    assert_eq!(report.version, RECALL_COMPETITOR_REPORT_VERSION);
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.retrieval_results.len(), 1);
    assert_eq!(report.end_to_end_results.len(), 1);
    assert_eq!(report.internal_release_gate, RECALL_INTERNAL_GATE_STATEMENT);
    assert!(report.unsupported_by_product["fixture-ranked-memory"].contains(&"citations".into()));
    Ok(())
}

#[test]
fn competitor_schema_rejects_hidden_fields() {
    let evidence = r#"{
      "version":"memzoi-recall-competitor-evidence/v1",
      "protocol":{},
      "products":[],
      "retrieval_results":[],
      "end_to_end_results":[],
      "limitations":[],
      "artifacts":[],
      "locked_test_accessed":false,
      "product_specific_tuning":false,
      "hidden_product_difference":"omitted"
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("competitors.json");
    std::fs::write(&path, evidence).unwrap();
    assert!(run_recall_competitor_eval(path).is_err());
}
