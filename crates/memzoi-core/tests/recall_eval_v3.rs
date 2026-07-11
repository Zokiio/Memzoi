use memzoi_core::{
    ManifestDrivenRecallCandidate, RECALL_V3_CORPUS_VERSION, RECALL_V3_METRICS_VERSION,
    RECALL_V3_REPORT_VERSION, RECALL_V3_RUNNER_VERSION, RecallRetrievalCandidateManifest,
    RecallV3Candidate, RecallV3CandidateInput, RecallV3CandidateManifest, RecallV3CandidateOutput,
    run_recall_v3_eval, run_recall_v3_eval_with_candidates,
};

#[test]
fn recall_v3_runs_lexical_baseline_in_isolated_state() -> anyhow::Result<()> {
    let corpus = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/corpus.yaml");
    let first = run_recall_v3_eval(&corpus)?;
    let second = run_recall_v3_eval(&corpus)?;

    assert_eq!(first.version, RECALL_V3_REPORT_VERSION);
    assert_eq!(first.corpus.version, RECALL_V3_CORPUS_VERSION);
    assert!(first.isolated_state);
    assert!(!first.network_required);
    assert!(first.passed, "{first:#?}");
    assert_eq!(first.candidates[0].manifest.id, "lexical-baseline");
    assert!(first.candidates[0].aggregate.mean_ndcg_at_10 < 1.0);
    assert_eq!(first.candidates[0].aggregate.citation_integrity, 1.0);
    assert_eq!(first.commitment.metrics_digest, first.digests.metrics);
    assert_eq!(first.commitment.runner_digest, first.digests.runner);
    assert_ne!(first.digests.metrics, first.digests.runner);
    assert!(!first.digests.report.is_empty());
    assert_eq!(first.candidates[0].paired_vs_lexical.lower_bound, 0.0);
    assert_eq!(first.digests.corpus, second.digests.corpus);
    assert_eq!(first.digests.judgments, second.digests.judgments);
    assert_eq!(first.digests.report, second.digests.report);
    assert_eq!(RECALL_V3_METRICS_VERSION, "memzoi-recall-metrics/v3");
    assert_eq!(RECALL_V3_RUNNER_VERSION, "memzoi-recall-runner/v3");
    Ok(())
}

#[test]
fn manifest_driven_exact_union_preserves_signals_and_citations() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let mut candidate =
        ManifestDrivenRecallCandidate::load(root.join("candidates/exact-union.json"))?;
    let report =
        run_recall_v3_eval_with_candidates(root.join("corpus.yaml"), &mut [&mut candidate])?;

    assert!(report.passed, "{report:#?}");
    let candidate = &report.candidates[1];
    assert_eq!(candidate.manifest.id, "fixture-exact-union");
    assert_eq!(candidate.aggregate.mean_ndcg_at_10, 1.0);
    assert_eq!(candidate.aggregate.citation_integrity, 1.0);
    assert_eq!(
        candidate.resource_observations["exact_distance_comparisons"],
        9.0
    );
    assert!(
        candidate
            .cases
            .iter()
            .all(|case| case.fallback_reason.is_none())
    );
    Ok(())
}

#[test]
fn missing_manifest_artifact_normalizes_to_lexical_fallback() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("candidates/exact-union.json"))?;
    let mut manifest: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    manifest.id = "fixture-missing-index".into();
    manifest.storage.vector_artifact = "missing-index.json".into();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("candidate.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    let mut candidate = ManifestDrivenRecallCandidate::load(path)?;
    let report =
        run_recall_v3_eval_with_candidates(root.join("corpus.yaml"), &mut [&mut candidate])?;

    let candidate = &report.candidates[1];
    assert_eq!(candidate.aggregate.fallback_parity, 1.0);
    assert!(
        candidate
            .cases
            .iter()
            .all(|case| case.fallback_reason.as_deref() == Some("missing_index"))
    );
    assert!(candidate.passed);
    Ok(())
}

struct BrokenFallback;

impl RecallV3Candidate for BrokenFallback {
    fn manifest(&self) -> RecallV3CandidateManifest {
        RecallV3CandidateManifest {
            id: "broken-fallback".into(),
            version: "1".into(),
            adapter: "test".into(),
            configuration_digest: "synthetic".into(),
            offline: true,
        }
    }

    fn retrieve(
        &mut self,
        input: &RecallV3CandidateInput,
    ) -> anyhow::Result<RecallV3CandidateOutput> {
        assert!(
            input
                .eligible_records
                .iter()
                .all(|record| record.id != "scope-distractor")
        );
        Ok(RecallV3CandidateOutput {
            hits: Vec::new(),
            fallback_reason: Some("missing_index".into()),
            resource_observations: Default::default(),
        })
    }
}

#[test]
fn recall_v3_candidates_share_eligibility_and_fallback_boundaries() -> anyhow::Result<()> {
    let corpus = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/corpus.yaml");
    let mut broken = BrokenFallback;
    let report = run_recall_v3_eval_with_candidates(&corpus, &mut [&mut broken])?;

    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.candidates[1].manifest.id, "broken-fallback");
    assert!(report.candidates[1].aggregate.fallback_parity < 1.0);
    assert!(!report.candidates[1].passed);
    assert!(!report.passed);
    Ok(())
}

#[test]
fn recall_v3_rejects_unknown_schema_fields() {
    let invalid = "version: memzoi-recall-corpus/v3\nname: invalid\nkind: development\nevaluated_at: 2026-07-10T12:00:00Z\nrecords: [x.md]\nthresholds: {}\ncases: []\nunknown: true\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("corpus.yaml"), invalid).unwrap();
    let error = run_recall_v3_eval(dir.path().join("corpus.yaml")).unwrap_err();
    assert!(error.to_string().contains("failed to parse"));
}
