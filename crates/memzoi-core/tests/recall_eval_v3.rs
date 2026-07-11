use memzoi_core::{
    ManifestDrivenRecallCandidate, RECALL_V3_CORPUS_VERSION, RECALL_V3_METRICS_VERSION,
    RECALL_V3_REPORT_VERSION, RECALL_V3_RUNNER_VERSION, RecallRetrievalCandidateManifest,
    RecallV3Candidate, RecallV3CandidateHit, RecallV3CandidateInput, RecallV3CandidateManifest,
    RecallV3CandidateOutput, RecallV3Corpus, RecallV3ForbiddenReason, run_recall_v3_eval,
    run_recall_v3_eval_with_candidates,
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

#[test]
fn recall_v3_rejects_cases_without_eligible_relevance() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("corpus.yaml"))?;
    let mut corpus: RecallV3Corpus = serde_yaml::from_slice(&bytes)?;
    for judgment in &mut corpus.cases[0].judgments {
        if judgment.eligible {
            judgment.relevance = 0;
        }
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("corpus.yaml");
    std::fs::write(&path, serde_yaml::to_string(&corpus)?)?;

    let error = run_recall_v3_eval(path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("must have at least one eligible relevant judgment")
    );
    Ok(())
}

#[test]
fn recall_v3_rejects_empty_slices_and_record_ids() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("corpus.yaml"))?;
    let original: RecallV3Corpus = serde_yaml::from_slice(&bytes)?;
    let mut invalid = Vec::new();
    let mut empty_slice = original.clone();
    empty_slice.cases[0].slices[0] = " ".into();
    invalid.push((empty_slice, "empty or duplicate slices"));
    let mut duplicate_slice = original.clone();
    let repeated_slice = duplicate_slice.cases[0].slices[0].clone();
    duplicate_slice.cases[0].slices.push(repeated_slice);
    invalid.push((duplicate_slice, "empty or duplicate slices"));
    let mut empty_record = original;
    empty_record.cases[0].judgments[0].record_id = " ".into();
    invalid.push((empty_record, "invalid judgment"));

    for (corpus, expected) in invalid {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("corpus.yaml");
        std::fs::write(&path, serde_yaml::to_string(&corpus)?)?;
        let error = run_recall_v3_eval(path).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
    Ok(())
}

#[test]
fn recall_v3_requires_every_staged_record_to_be_judged() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("corpus.yaml"))?;
    let mut corpus: RecallV3Corpus = serde_yaml::from_slice(&bytes)?;
    corpus.cases[0].judgments.pop();
    let dir = tempfile::tempdir()?;
    let records = dir.path().join("records");
    std::fs::create_dir(&records)?;
    for record in &corpus.records {
        std::fs::copy(root.join("records").join(record), records.join(record))?;
    }
    let path = dir.path().join("corpus.yaml");
    std::fs::write(&path, serde_yaml::to_string(&corpus)?)?;

    let error = run_recall_v3_eval(path).unwrap_err();
    assert!(error.to_string().contains("must judge every staged record"));
    Ok(())
}

#[test]
fn recall_v3_rejects_duplicate_record_paths() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("corpus.yaml"))?;
    let mut corpus: RecallV3Corpus = serde_yaml::from_slice(&bytes)?;
    corpus.records.push(corpus.records[0].clone());
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("corpus.yaml");
    std::fs::write(&path, serde_yaml::to_string(&corpus)?)?;

    let error = run_recall_v3_eval(path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("duplicate recall-v3 record path")
    );
    Ok(())
}

struct UnknownRecordCandidate;

impl RecallV3Candidate for UnknownRecordCandidate {
    fn manifest(&self) -> RecallV3CandidateManifest {
        RecallV3CandidateManifest {
            id: "unknown-record".into(),
            version: "1".into(),
            adapter: "test".into(),
            configuration_digest: "unknown-record".into(),
            offline: true,
        }
    }

    fn retrieve(
        &mut self,
        _input: &RecallV3CandidateInput,
    ) -> anyhow::Result<RecallV3CandidateOutput> {
        Ok(RecallV3CandidateOutput {
            hits: vec![RecallV3CandidateHit {
                record_id: "hallucinated-record".into(),
                score: 1.0,
                citations: Vec::new(),
                signals: Default::default(),
            }],
            fallback_reason: None,
            resource_observations: Default::default(),
        })
    }
}

#[test]
fn unknown_candidate_ids_count_as_forbidden_other() -> anyhow::Result<()> {
    let corpus = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/corpus.yaml");
    let mut candidate = UnknownRecordCandidate;
    let report = run_recall_v3_eval_with_candidates(&corpus, &mut [&mut candidate])?;
    let candidate = &report.candidates[1];

    assert_eq!(
        candidate.aggregate.forbidden_hits[&RecallV3ForbiddenReason::Other],
        3
    );
    assert!(!candidate.passed);
    Ok(())
}
