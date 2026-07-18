use memzoi_core::{
    RECALL_OPERATIONAL_REPORT_VERSION, recall_synthetic_workload_digest,
    run_recall_operational_eval,
};

#[test]
fn complete_operational_evidence_passes_every_gate() -> anyhow::Result<()> {
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/operational/evidence.json");
    let report = run_recall_operational_eval(evidence)?;

    assert_eq!(report.version, RECALL_OPERATIONAL_REPORT_VERSION);
    assert!(report.passed, "{report:#?}");
    assert_eq!(report.task_utility_pass_rate, 1.0);
    assert_eq!(report.operational_pass_rate, 1.0);
    assert_eq!(report.fallback_parity, 1.0);
    assert_eq!(report.performance.record_count, 10_000);
    assert_eq!(
        report.workload_digest,
        recall_synthetic_workload_digest(10_000, 1_592_594_996)
    );
    assert_ne!(
        report.workload_digest,
        recall_synthetic_workload_digest(10_000, 1_592_594_997)
    );
    assert!(report.trace_counters.contains_key("safe_suppression"));
    Ok(())
}

#[test]
fn operational_evidence_digest_ignores_json_formatting() -> anyhow::Result<()> {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/operational/evidence.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture)?)?;
    let dir = tempfile::tempdir()?;
    let compact = dir.path().join("compact.json");
    std::fs::write(&compact, serde_json::to_vec(&value)?)?;

    assert_eq!(
        run_recall_operational_eval(fixture)?.evidence_digest,
        run_recall_operational_eval(compact)?.evidence_digest
    );
    Ok(())
}

#[test]
fn operational_evidence_rejects_raw_unknown_trace_fields() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/operational/evidence.json");
    let bytes = std::fs::read(fixture).unwrap();
    let mut evidence: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    evidence["trace_counters"][0]["raw_query"] = serde_json::json!("secret");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    let error = run_recall_operational_eval(path).unwrap_err();
    assert!(format!("{error:#}").contains("unknown field `raw_query`"));
}

#[test]
fn operational_evidence_rejects_unregistered_trace_reason_codes() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/operational/evidence.json");
    let bytes = std::fs::read(fixture).unwrap();
    let mut evidence: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    evidence["trace_counters"][0]["reason_code"] = serde_json::json!("private_identifier_123");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();

    let error = run_recall_operational_eval(path).unwrap_err();
    assert!(format!("{error:#}").contains("unknown variant `private_identifier_123`"));
}

#[test]
fn operational_evidence_requires_complete_environment_identity() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/operational/evidence.json");
    let bytes = std::fs::read(fixture).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let invalid = [
        ("os", serde_json::json!(" ")),
        ("architecture", serde_json::json!("")),
        ("cpu", serde_json::json!(" ")),
        ("timer", serde_json::json!("")),
        ("memory_bytes", serde_json::json!(0)),
    ];
    for (field, value) in invalid {
        let mut evidence = original.clone();
        evidence["environment"][field] = value;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evidence.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
        let error = run_recall_operational_eval(path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid operational evidence identity")
        );
    }
}
