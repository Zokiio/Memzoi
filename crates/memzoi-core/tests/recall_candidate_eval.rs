use memzoi_core::{
    ManifestDrivenRecallCandidate, RECALL_DEVELOPMENT_LOG_VERSION, RecallCandidateArchitecture,
    RecallDevelopmentAttempt, RecallDevelopmentAttemptOutcome, RecallDevelopmentLog,
    RecallRetrievalCandidateManifest, RecallV3Candidate, RecallVectorArtifact,
    recall_vector_artifact_digest, run_recall_v3_eval_with_candidates, validate_development_log,
};

#[test]
fn fixture_vector_artifact_digest_is_stable() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/candidates/vectors/exact-union.json");
    let artifact: RecallVectorArtifact = serde_json::from_slice(&std::fs::read(root)?)?;
    let digest = recall_vector_artifact_digest(&artifact)?;
    assert_eq!(
        digest,
        "f86016fc537eef4d311e9656a51990eacd24d3e7e1165eaf3d793b78e274b1ee"
    );
    Ok(())
}

#[test]
fn modified_vector_artifacts_fall_back_without_using_unbound_scores() -> anyhow::Result<()> {
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/development");
    let mut manifest: RecallRetrievalCandidateManifest =
        serde_json::from_slice(&std::fs::read(root.join("candidates/exact-union.json"))?)?;
    manifest.id = "fixture-tampered-index".into();

    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("vectors"))?;
    std::fs::write(
        dir.path().join("candidate.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let mut vectors: serde_json::Value = serde_json::from_slice(&std::fs::read(
        root.join("candidates/vectors/exact-union.json"),
    )?)?;
    vectors["records"]["semantic-target"][0] = serde_json::json!(0.5);
    std::fs::write(
        dir.path().join("vectors/exact-union.json"),
        serde_json::to_vec_pretty(&vectors)?,
    )?;

    let mut candidate = ManifestDrivenRecallCandidate::load(dir.path().join("candidate.json"))?;
    let error = candidate
        .require_ready()
        .expect_err("a tampered vector artifact must not be ready for evaluation");
    assert!(error.to_string().contains("corrupt_index"));
    let report =
        run_recall_v3_eval_with_candidates(root.join("corpus.yaml"), &mut [&mut candidate])?;
    let candidate = &report.candidates[1];
    assert!(
        candidate
            .cases
            .iter()
            .all(|case| case.fallback_reason.as_deref() == Some("corrupt_index")),
        "{:#?}",
        candidate.cases
    );
    assert_eq!(candidate.aggregate.fallback_parity, 1.0);
    Ok(())
}

#[test]
fn published_manifest_can_resolve_a_separate_local_artifact_root() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/candidates");
    let manifest_dir = tempfile::tempdir()?;
    let artifact_dir = tempfile::tempdir()?;
    std::fs::copy(
        root.join("exact-union.json"),
        manifest_dir.path().join("candidate.json"),
    )?;
    std::fs::create_dir(artifact_dir.path().join("vectors"))?;
    std::fs::copy(
        root.join("vectors/exact-union.json"),
        artifact_dir.path().join("vectors/exact-union.json"),
    )?;

    let candidate = ManifestDrivenRecallCandidate::load_with_artifact_root(
        manifest_dir.path().join("candidate.json"),
        artifact_dir.path(),
    )?;
    candidate.require_ready()?;
    Ok(())
}

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
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/development");
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
        if architecture == RecallCandidateArchitecture::LexicalRerank {
            manifest.retrieval.fusion = memzoi_core::RecallFusionMethod::WeightedSum;
        }
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
    let candidate = |id: &str| {
        report
            .candidates
            .iter()
            .find(|candidate| candidate.manifest.id == id)
            .expect("candidate report exists")
    };
    let baseline = candidate("lexical-baseline");
    let semantic = candidate("semantic-only");
    let rerank = candidate("lexical-rerank");
    let union = candidate("lexical-union");
    assert!(semantic.aggregate.mean_ndcg_at_10 > baseline.aggregate.mean_ndcg_at_10);
    assert!(rerank.aggregate.mean_ndcg_at_10 < 1.0);
    assert_eq!(union.aggregate.mean_ndcg_at_10, 1.0);
    Ok(())
}

#[test]
fn approximate_search_manifest_is_rejected() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/candidates");
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
        .join("../../evals/recall/development/candidates");
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
fn unsupported_tie_breaking_and_rerank_fusion_are_rejected() -> anyhow::Result<()> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/recall/development/candidates");
    let bytes = std::fs::read(root.join("exact-union.json"))?;
    let base: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)?;
    let invalid = [
        ("tie-break", {
            let mut manifest = base.clone();
            manifest.retrieval.tie_break = "score_only".into();
            manifest
        }),
        ("rerank-fusion", {
            let mut manifest = base;
            manifest.retrieval.architecture = RecallCandidateArchitecture::LexicalRerank;
            manifest
        }),
    ];
    for (id, manifest) in invalid {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("candidate.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
        let error = match ManifestDrivenRecallCandidate::load(path) {
            Ok(_) => panic!("unsupported retrieval contract unexpectedly loaded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(if id == "tie-break" {
            "tie_break"
        } else {
            "weighted_sum"
        }));
    }
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
        .join("../../evals/recall/development/candidates");
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
