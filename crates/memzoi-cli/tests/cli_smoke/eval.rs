use super::*;

#[test]
fn eval_recall_help_requires_an_explicit_corpus() {
    let mut cmd = memzoi();

    cmd.args(["eval", "recall", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Evaluate recall quality")
                .and(predicate::str::contains("--corpus <CORPUS>"))
                .and(predicate::str::contains("--baseline <BASELINE>"))
                .and(predicate::str::contains("--update-baseline"))
                .and(predicate::str::contains("--json")),
        );
}

#[test]
fn eval_capture_help_requires_an_explicit_corpus() {
    let mut cmd = memzoi();

    cmd.args(["eval", "capture", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Evaluate capture quality")
                .and(predicate::str::contains("--corpus <CORPUS>"))
                .and(predicate::str::contains("--baseline <BASELINE>"))
                .and(predicate::str::contains("--update-baseline"))
                .and(predicate::str::contains("--json")),
        );
}

#[test]
fn eval_recall_v3_help_exposes_local_locked_commitment_controls() {
    let mut cmd = memzoi();

    cmd.args(["eval", "recall-v3", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--corpus <CORPUS>")
                .and(predicate::str::contains("--candidate <CANDIDATES>"))
                .and(predicate::str::contains("--artifact-root <ARTIFACT_ROOTS>"))
                .and(predicate::str::contains("--prepare-locked-commitment"))
                .and(predicate::str::contains("--verify-locked-commitment"))
                .and(predicate::str::contains("--require-ready-candidates"))
                .and(predicate::str::contains("--commitment <COMMITMENT>")),
        );
}

#[test]
fn eval_recall_v3_help_exposes_complete_development_workflow() {
    let mut candidate = memzoi();
    candidate
        .args(["eval", "recall-v3", "candidate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("build"));

    let mut development = memzoi();
    development
        .args(["eval", "recall-v3", "development", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("run")
                .and(predicate::str::contains("freeze"))
                .and(predicate::str::contains("publish")),
        );

    let mut freeze = memzoi();
    freeze
        .args(["eval", "recall-v3", "development", "freeze", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--run <RUN>")
                .and(predicate::str::contains("--corpus <CORPUS>"))
                .and(predicate::str::contains("--matrix <MATRIX>"))
                .and(predicate::str::contains("--profile-root <PROFILE_ROOT>")),
        );
}

#[test]
fn candidate_build_without_model_feature_fails_explicitly() {
    let mut cmd = memzoi();
    cmd.args([
        "eval",
        "recall-v3",
        "candidate",
        "build",
        "--profile",
        "profile.json",
        "--matrix",
        "matrix.json",
        "--corpus",
        "corpus.yaml",
        "--model-root",
        "models",
        "--template",
        "title_body/v1",
        "--output",
        "output",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "candidate build requires --features recall-models",
    ));
}

#[test]
fn eval_recall_json_uses_isolated_state_and_reports_citations() {
    let repo = tempfile::tempdir().expect("temp repo");
    let sentinel = repo.path().join(".memzoi/records/user-record.md");
    let baseline_path = checked_recall_baseline();
    let baseline_before = fs::read(&baseline_path).expect("read checked baseline");
    fs::create_dir_all(sentinel.parent().expect("sentinel parent")).expect("create sentinel root");
    fs::write(&sentinel, "user canonical bytes").expect("write sentinel");

    let mut cmd = memzoi();
    let assert = cmd
        .args(["eval", "recall", "--corpus"])
        .arg(checked_recall_corpus())
        .arg("--baseline")
        .arg(&baseline_path)
        .arg("--json")
        .current_dir(repo.path())
        .assert()
        .success();
    let report = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(
        report.get("version").and_then(Value::as_str),
        Some("memzoi-recall-report/v2")
    );
    assert_eq!(
        report.pointer("/corpus/version").and_then(Value::as_str),
        Some("memzoi-recall-corpus/v2")
    );
    assert_eq!(report.get("passed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        report
            .pointer("/metrics/search/mean_recall_at_k")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        report
            .pointer("/metrics/precheck/precision/value")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        report
            .pointer("/metrics/leakage/forbidden/rate")
            .and_then(Value::as_f64),
        Some(0.0)
    );
    assert_eq!(
        report
            .pointer("/metrics/citation_integrity/rate")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        report
            .pointer("/metrics/provenance_integrity/rate")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        report.pointer("/baseline/status").and_then(Value::as_str),
        Some("match")
    );
    assert_eq!(
        report
            .pointer("/baseline/deterministic_match")
            .and_then(Value::as_bool),
        Some(true)
    );
    let citation = report
        .get("cases")
        .and_then(Value::as_array)
        .and_then(|cases| {
            cases
                .iter()
                .find(|case| case.get("id").and_then(Value::as_str) == Some("citation-integrity"))
        })
        .and_then(|case| case.pointer("/retrieved/0/citations/0"))
        .expect("citation-bearing retrieved result");
    assert_eq!(
        citation.get("source_ref").and_then(Value::as_str),
        Some("issue://47")
    );
    assert_eq!(
        citation.get("path").and_then(Value::as_str),
        Some("docs/roadmap.md")
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("read sentinel"),
        "user canonical bytes",
        "evaluation must not mutate normal canonical state"
    );
    assert_eq!(
        fs::read(&baseline_path).expect("read checked baseline after comparison"),
        baseline_before,
        "ordinary baseline comparison must be read-only"
    );
}

#[test]
fn eval_capture_json_uses_isolated_state_and_matches_the_exact_baseline() {
    let repo = tempfile::tempdir().expect("temp repo");
    let sentinel = repo.path().join(".memzoi/records/user-record.md");
    let baseline_path = checked_capture_baseline();
    let baseline_before = fs::read(&baseline_path).expect("read checked capture baseline");
    fs::create_dir_all(sentinel.parent().expect("sentinel parent")).expect("create sentinel root");
    fs::write(&sentinel, "user canonical bytes").expect("write sentinel");

    let mut cmd = memzoi();
    let assert = cmd
        .args(["eval", "capture", "--corpus"])
        .arg(checked_capture_corpus())
        .arg("--baseline")
        .arg(&baseline_path)
        .arg("--json")
        .current_dir(repo.path())
        .assert()
        .success();
    let report = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(
        report.get("version").and_then(Value::as_str),
        Some("memzoi/capture-eval-report")
    );
    assert_eq!(
        report
            .pointer("/corpus/profile_count")
            .and_then(Value::as_u64),
        Some(4)
    );
    assert_eq!(
        report
            .pointer("/metrics/candidate_precision/value")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(
        report.get("gates_passed").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        report.pointer("/baseline/status").and_then(Value::as_str),
        Some("match")
    );
    assert_eq!(report.get("passed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        fs::read_to_string(&sentinel).expect("read sentinel"),
        "user canonical bytes",
        "capture evaluation must not mutate normal canonical state"
    );
    assert_eq!(
        fs::read(&baseline_path).expect("read checked capture baseline after comparison"),
        baseline_before,
        "ordinary capture baseline comparison must be read-only"
    );
}

#[test]
fn eval_capture_can_update_and_compare_a_local_baseline() {
    let repo = tempfile::tempdir().expect("temp repo");
    let baseline_path = repo.path().join("capture-baseline.json");
    fs::write(&baseline_path, "existing baseline bytes").expect("seed baseline sentinel");
    let mut cmd = memzoi();

    let assert = cmd
        .args(["eval", "capture", "--corpus"])
        .arg(checked_capture_corpus())
        .arg("--baseline")
        .arg(&baseline_path)
        .args(["--update-baseline", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();
    let report = json_from_stdout(&assert.get_output().stdout);
    let baseline = serde_json::from_str::<Value>(
        &fs::read_to_string(&baseline_path).expect("read updated capture baseline"),
    )
    .expect("updated capture baseline is JSON");

    assert_eq!(report.get("passed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        report.pointer("/baseline/status").and_then(Value::as_str),
        Some("match")
    );
    assert_eq!(
        baseline.get("version").and_then(Value::as_str),
        Some("memzoi/capture-eval-baseline")
    );
    assert_eq!(
        baseline.pointer("/corpus/digest"),
        report.pointer("/corpus/digest")
    );
}

#[test]
fn eval_recall_can_update_and_compare_a_local_baseline() {
    let repo = tempfile::tempdir().expect("temp repo");
    let baseline_path = repo.path().join("baseline.json");
    fs::write(&baseline_path, "existing baseline bytes").expect("seed baseline sentinel");
    let mut cmd = memzoi();

    let assert = cmd
        .args(["eval", "recall", "--corpus"])
        .arg(checked_recall_corpus())
        .arg("--baseline")
        .arg("baseline.json")
        .args(["--update-baseline", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();
    let report = json_from_stdout(&assert.get_output().stdout);
    let baseline = serde_json::from_str::<Value>(
        &fs::read_to_string(&baseline_path).expect("read updated baseline"),
    )
    .expect("updated baseline is JSON");

    assert_eq!(report.get("passed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        report.pointer("/baseline/status").and_then(Value::as_str),
        Some("match")
    );
    assert_eq!(
        report
            .pointer("/baseline/deterministic_match")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        baseline.get("version").and_then(Value::as_str),
        Some("memzoi-recall-baseline/v1")
    );
    assert_eq!(
        baseline.pointer("/corpus/version").and_then(Value::as_str),
        Some("memzoi-recall-corpus/v2")
    );
    assert_eq!(
        baseline.pointer("/corpus/digest"),
        report.pointer("/corpus/digest")
    );
}

#[test]
fn eval_recall_refuses_to_update_a_failing_baseline() {
    let (_fixture, corpus) = failing_recall_corpus();
    let repo = tempfile::tempdir().expect("temp repo");
    let baseline_root = tempfile::tempdir().expect("baseline root");
    let baseline_path = baseline_root.path().join("baseline.json");
    fs::write(&baseline_path, "existing baseline bytes").expect("seed baseline sentinel");
    let mut cmd = memzoi();

    cmd.args(["eval", "recall", "--corpus"])
        .arg(&corpus)
        .arg("--baseline")
        .arg(&baseline_path)
        .args(["--update-baseline", "--json"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "thresholds failed; baseline was not modified",
        ));

    assert_eq!(
        fs::read_to_string(&baseline_path).expect("read baseline sentinel"),
        "existing baseline bytes",
        "a failing threshold run must not replace its baseline"
    );
}

#[test]
fn eval_recall_regression_keeps_json_report_on_stdout_and_exits_nonzero() {
    let (_fixture, corpus) = failing_recall_corpus();
    let repo = tempfile::tempdir().expect("temp repo");
    let mut cmd = memzoi();
    let assert = cmd
        .args(["eval", "recall", "--corpus"])
        .arg(&corpus)
        .arg("--json")
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "recall evaluation thresholds or baseline compatibility failed",
        ));
    let report = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(report.get("passed").and_then(Value::as_bool), Some(false));
    let mean_recall = report
        .pointer("/metrics/search/mean_recall_at_k")
        .and_then(Value::as_f64)
        .expect("v2 report has search mean recall metric");
    assert!(
        mean_recall < 1.0,
        "intentional regression should lower search recall: {report}"
    );
    assert_eq!(
        report
            .pointer("/threshold_results/min_mean_recall_at_k")
            .and_then(Value::as_bool),
        Some(false)
    );
}
