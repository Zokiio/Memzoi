use memzoi_core::{
    RECALL_OPERATIONAL_REPORT_VERSION, recall_synthetic_workload_digest,
    run_recall_operational_eval,
};

#[test]
fn complete_operational_evidence_passes_every_gate() -> anyhow::Result<()> {
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/operational/evidence.json");
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
fn operational_evidence_rejects_raw_unknown_trace_fields() {
    let evidence = r#"{
      "version":"memzoi-recall-operational-evidence/v1",
      "candidate_digest":"candidate",
      "release_build_digest":"release",
      "lexical_projection_digest":"lexical",
      "environment":{},
      "task_utility":[],
      "operational":[],
      "fallback":[],
      "performance":{},
      "cross_platform":{"mode":"exact_vectors"},
      "deterministic_ties":true,
      "policy_gates_preserved":true,
      "trace_counters":[{"reason_code":"failure","count":1,"raw_query":"secret"}],
      "warm_p95_limit_ms":200.0
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.json");
    std::fs::write(&path, evidence).unwrap();
    assert!(run_recall_operational_eval(path).is_err());
}
