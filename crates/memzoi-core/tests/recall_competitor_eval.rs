use memzoi_core::{
    RECALL_COMPETITOR_REPORT_VERSION, RECALL_INTERNAL_GATE_STATEMENT, run_recall_competitor_eval,
};

#[test]
fn two_track_competitor_fixture_is_complete_and_reproducible() -> anyhow::Result<()> {
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/competitors/fixture-evidence.json");
    let report = run_recall_competitor_eval(evidence)?;

    assert_eq!(report.version, RECALL_COMPETITOR_REPORT_VERSION);
    assert!(report.passed, "{report:#?}");
    assert!(!report.eligible_for_ship_decision);
    assert_eq!(report.retrieval_results.len(), 1);
    assert_eq!(report.end_to_end_results.len(), 1);
    assert_eq!(report.internal_release_gate, RECALL_INTERNAL_GATE_STATEMENT);
    assert!(report.unsupported_by_product["fixture-ranked-memory"].contains(&"citations".into()));
    Ok(())
}

#[test]
fn competitor_evidence_digest_ignores_json_formatting() -> anyhow::Result<()> {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/competitors/fixture-evidence.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture)?)?;
    let dir = tempfile::tempdir()?;
    let compact = dir.path().join("compact.json");
    std::fs::write(&compact, serde_json::to_vec(&value)?)?;

    assert_eq!(
        run_recall_competitor_eval(fixture)?.evidence_digest,
        run_recall_competitor_eval(compact)?.evidence_digest
    );
    Ok(())
}

#[test]
fn competitor_schema_rejects_hidden_fields() {
    let evidence = r#"{
      "version":"memzoi-recall-competitor-evidence/v2",
      "evidence_kind":"contract_fixture",
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

#[test]
fn competitor_citation_support_and_integrity_must_agree() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/competitors/fixture-evidence.json");
    let bytes = std::fs::read(fixture).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let invalid = [
        (true, serde_json::Value::Null),
        (false, serde_json::json!(1.0)),
    ];
    for (supported, integrity) in invalid {
        let mut evidence = original.clone();
        evidence["retrieval_results"][0]["citations_supported"] = serde_json::json!(supported);
        evidence["retrieval_results"][0]["citation_integrity"] = integrity;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("competitors.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
        let error = run_recall_competitor_eval(path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exactly when citations are supported")
        );
    }
}

#[test]
fn observed_label_without_executable_verification_is_not_ship_evidence() -> anyhow::Result<()> {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/competitors/fixture-evidence.json");
    let mut evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture)?)?;
    evidence["evidence_kind"] = serde_json::json!("observed_bakeoff");
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("observed.json");
    std::fs::write(&path, serde_json::to_vec(&evidence)?)?;

    let report = run_recall_competitor_eval(path)?;
    assert!(report.passed);
    assert!(!report.eligible_for_ship_decision);
    Ok(())
}
