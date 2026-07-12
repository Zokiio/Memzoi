use memzoi_core::{
    ManifestDrivenRecallCandidate, RECALL_DEVELOPMENT_LOG_VERSION, RecallCandidateArchitecture,
    RecallDevelopmentAttempt, RecallDevelopmentAttemptOutcome, RecallDevelopmentLog,
    RecallRetrievalCandidateManifest, RecallV3Candidate, run_recall_v3_eval_with_candidates,
    validate_development_log,
};

#[test]
fn development_log_retains_completed_rejected_and_failed_attempts() -> anyhow::Result<()> {
    let log = RecallDevelopmentLog {
        version: RECALL_DEVELOPMENT_LOG_VERSION.into(),
        corpus_digest: "corpus".into(),
        runner_digest: "runner".into(),
        attempts: vec![
            RecallDevelopmentAttempt {
                candidate_id: "completed".into(),
                candidate_digest: "candidate-a".into(),
                outcome: RecallDevelopmentAttemptOutcome::Completed,
                reason_code: None,
                report_digest: Some("report-a".into()),
            },
            RecallDevelopmentAttempt {
                candidate_id: "rejected".into(),
                candidate_digest: "candidate-b".into(),
                outcome: RecallDevelopmentAttemptOutcome::Rejected,
                reason_code: Some("quality_gate".into()),
                report_digest: None,
            },
            RecallDevelopmentAttempt {
                candidate_id: "failed".into(),
                candidate_digest: "candidate-c".into(),
                outcome: RecallDevelopmentAttemptOutcome::Failed,
                reason_code: Some("adapter_failure".into()),
                report_digest: None,
            },
        ],
    };
    validate_development_log(&log)?;
    Ok(())
}

#[test]
fn all_required_candidate_architectures_run_through_one_boundary() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
    let bytes = std::fs::read(root.join("candidates/exact-union.json"))?;
    let base: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("vectors"))?;
    std::fs::copy(
        root.join("candidates/vectors/exact-union.json"),
        dir.path().join("vectors/exact-union.json"),
    )?;
    let architectures = [
        ("semantic-only", RecallCandidateArchitecture::SemanticOnly),
        ("lexical-rerank", RecallCandidateArchitecture::LexicalRerank),
        (
            "lexical-union",
            RecallCandidateArchitecture::LexicalSemanticUnion,
        ),
    ];
    let mut paths = Vec::new();
    for (id, architecture) in architectures {
        let mut manifest = base.clone();
        manifest.id = id.into();
        manifest.retrieval.architecture = architecture;
        let path = dir.path().join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
        paths.push(path);
    }
    let mut candidates = paths
        .iter()
        .map(ManifestDrivenRecallCandidate::load)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut refs = candidates
        .iter_mut()
        .map(|candidate| candidate as &mut dyn RecallV3Candidate)
        .collect::<Vec<_>>();
    let report = run_recall_v3_eval_with_candidates(root.join("corpus.yaml"), &mut refs)?;

    assert_eq!(report.candidates.len(), 4);
    assert!(report.candidates.iter().all(|candidate| candidate.passed));
    assert_eq!(report.candidates[1].aggregate.mean_ndcg_at_10, 1.0);
    assert!(report.candidates[2].aggregate.mean_ndcg_at_10 < 1.0);
    assert_eq!(report.candidates[3].aggregate.mean_ndcg_at_10, 1.0);
    Ok(())
}

#[test]
fn approximate_search_manifest_is_rejected() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/candidates");
    let bytes = std::fs::read(root.join("exact-union.json"))?;
    let mut manifest: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    manifest.storage.exact_search = false;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("candidate.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;

    let error = match ManifestDrivenRecallCandidate::load(path) {
        Ok(_) => panic!("approximate search manifest unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("exact search"));
    Ok(())
}

#[test]
fn unsupported_structural_weights_are_rejected() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/candidates");
    let bytes = std::fs::read(root.join("exact-union.json"))?;
    let mut manifest: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    manifest.retrieval.path_weight = 0.25;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("candidate.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;

    let error = match ManifestDrivenRecallCandidate::load(path) {
        Ok(_) => panic!("unsupported structural weights unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("must remain zero"));
    Ok(())
}

#[test]
fn development_log_rejects_blank_attempt_metadata() {
    let invalid_attempts = [
        RecallDevelopmentAttempt {
            candidate_id: "".into(),
            candidate_digest: "digest".into(),
            outcome: RecallDevelopmentAttemptOutcome::Completed,
            reason_code: None,
            report_digest: Some("report".into()),
        },
        RecallDevelopmentAttempt {
            candidate_id: "completed".into(),
            candidate_digest: "digest".into(),
            outcome: RecallDevelopmentAttemptOutcome::Completed,
            reason_code: None,
            report_digest: Some(" ".into()),
        },
        RecallDevelopmentAttempt {
            candidate_id: "failed".into(),
            candidate_digest: "digest".into(),
            outcome: RecallDevelopmentAttemptOutcome::Failed,
            reason_code: Some(" ".into()),
            report_digest: None,
        },
    ];
    for attempt in invalid_attempts {
        let log = RecallDevelopmentLog {
            version: RECALL_DEVELOPMENT_LOG_VERSION.into(),
            corpus_digest: "corpus".into(),
            runner_digest: "runner".into(),
            attempts: vec![attempt],
        };
        assert!(validate_development_log(&log).is_err());
    }
}

#[test]
fn empty_vector_artifact_path_is_rejected() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/v3/candidates");
    let bytes = std::fs::read(root.join("exact-union.json"))?;
    let mut manifest: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    manifest.storage.vector_artifact = "".into();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("candidate.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;

    let error = match ManifestDrivenRecallCandidate::load(path) {
        Ok(_) => panic!("empty vector artifact unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("relative artifact"));
    Ok(())
}
