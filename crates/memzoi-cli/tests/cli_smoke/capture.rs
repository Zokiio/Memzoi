use super::*;

#[test]
fn capture_help_exposes_explicit_plan_review_and_apply_boundaries() {
    let mut root = memzoi();
    root.args(["capture", "--help"]).assert().success().stdout(
        predicate::str::contains("plan")
            .and(predicate::str::contains("review"))
            .and(predicate::str::contains("apply")),
    );

    let mut plan = memzoi();
    plan.args(["capture", "plan", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--source <SOURCE>")
                .and(predicate::str::contains("--source-id <SOURCE_ID>"))
                .and(predicate::str::contains("--output <OUTPUT>"))
                .and(predicate::str::contains("--json")),
        );

    let mut review = memzoi();
    review
        .args(["capture", "review", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--plan-file <PLAN_FILE>")
                .and(predicate::str::contains(
                    "--decisions-file <DECISIONS_FILE>",
                ))
                .and(predicate::str::contains("--reviewed-by <REVIEWED_BY>"))
                .and(predicate::str::contains("--reviewed-at <REVIEWED_AT>")),
        );

    let mut apply = memzoi();
    apply
        .args(["capture", "apply", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--plan-file <PLAN_FILE>")
                .and(predicate::str::contains("--review-file <REVIEW_FILE>"))
                .and(predicate::str::contains("--plan-id <PLAN_ID>"))
                .and(predicate::str::contains("--review-id <REVIEW_ID>")),
        );
}

#[test]
fn capture_plan_requires_exactly_one_explicit_source_argument() {
    let repo = tempfile::tempdir().expect("temp repo");

    let mut missing = memzoi();
    missing
        .args(["capture", "plan", "--json"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--source <SOURCE>"));

    let mut repeated = memzoi();
    repeated
        .args([
            "capture", "plan", "--source", "one.md", "--source", "two.md", "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--source"));
}

#[test]
fn capture_plan_does_not_create_an_absent_reserved_memory_root_for_private_output() {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);
    fs::write(
        repo.path().join("capture-source.md"),
        "# Fact: Reserved output boundary\nNever create managed state while planning.\n",
    )
    .expect("write capture source");
    let forbidden = repo.path().join(".memzoi");

    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "capture-source.md",
            "--output",
            forbidden.to_str().expect("reserved path utf-8"),
        ],
    );
    assert!(error.contains("private runtime directory"), "{error}");
    assert!(!forbidden.exists());
}

#[test]
fn capture_plan_json_human_and_explicit_artifact_are_deterministic_and_read_only() {
    let repo = initialized_temp_repo();
    let source = repo.path().join("capture-source.md");
    fs::write(&source, capture_markdown_fixture()).expect("write capture source");
    let paths = test_paths(repo.path());
    for directory in [
        paths.records_dir(),
        paths.proposals_dir().join("pending"),
        paths.exports_dir.clone(),
    ] {
        fs::create_dir_all(directory).expect("create managed artifact boundary fixture");
    }
    let before = managed_state_snapshot(&paths);

    let planned = run_json_command(
        repo.path(),
        &["capture", "plan", "--source", "capture-source.md", "--json"],
    );
    let repeated = run_json_command(
        repo.path(),
        &["capture", "plan", "--source", "capture-source.md", "--json"],
    );
    assert_eq!(planned, repeated, "capture planning must be deterministic");
    assert_json_string_field(&planned, &["schema"], "memzoi/capture-plan-v2");
    assert_json_string_field(&planned, &["status"], "ready");
    assert_json_string_field(&planned, &["data_class"], "private");
    assert!(json_string(&planned, "plan_id").starts_with("capture_"));
    let candidates = planned["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("capture plan should include candidates: {planned}"));
    assert_eq!(
        candidates.len(),
        2,
        "typed sections should be extracted: {planned}"
    );
    for candidate in candidates {
        assert!(json_string(candidate, "claim_id").starts_with("claim_"));
        assert!(json_string(candidate, "candidate_id").starts_with("candidate_"));
        assert!(candidate["confidence"].is_string() || candidate["confidence"].is_number());
        assert_json_string_field(
            &candidate["classification"],
            &["destination"],
            "needs_review",
        );
        assert_json_string_field(&candidate["classification"], &["sensitivity"], "unknown");
        assert!(
            candidate["classification"]["destination_reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        let evidence = &candidate["evidence"][0];
        assert_json_string_field(evidence, &["source_id"], "source");
        assert_json_string_field(&evidence["locator"], &["path"], "capture-source.md");
        assert!(json_string(evidence, "source_content_hash").starts_with("blake3:"));
        assert!(json_string(evidence, "evidence_content_hash").starts_with("blake3:"));
        assert!(evidence["span"]["byte_end"].as_u64().unwrap() > 0);
        assert!(
            evidence["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
        assert_json_string_field(&candidate["extraction"], &["id"], "memzoi-markdown");
    }

    let human = run_command_stdout(
        repo.path(),
        &["capture", "plan", "--source", "capture-source.md"],
    );
    assert!(human.starts_with("capture-plan\n"), "{human}");
    assert!(human.contains(json_string(&planned, "plan_id")), "{human}");
    for candidate in candidates {
        assert!(
            human.contains(json_string(candidate, "candidate_id")),
            "{human}"
        );
        assert!(
            human.contains(json_string(
                &candidate["evidence"][0],
                "evidence_content_hash"
            )),
            "{human}"
        );
    }

    assert_eq!(
        managed_state_snapshot(&paths),
        before,
        "stdout-only planning must not mutate memory state"
    );
    let artifact = paths.runtime_dir.join("capture-plan.json");
    let emitted = run_json_command(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "capture-source.md",
            "--output",
            artifact.to_str().expect("artifact path utf-8"),
            "--json",
        ],
    );
    let saved: Value = serde_json::from_slice(&fs::read(&artifact).expect("read plan artifact"))
        .expect("parse plan artifact");
    assert_eq!(emitted, planned);
    assert_eq!(saved, planned);

    let saved_before = fs::read(&artifact).expect("read immutable plan artifact");
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "capture-source.md",
            "--output",
            artifact.to_str().expect("artifact path utf-8"),
        ],
    );
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(
        fs::read(&artifact).expect("reread plan artifact"),
        saved_before
    );

    for forbidden in [
        paths.records_dir().join("capture-plan.json"),
        paths
            .proposals_dir()
            .join("pending")
            .join("capture-plan.json"),
        paths.exports_dir.join("capture-plan.json"),
    ] {
        let error = run_command_failure_stderr(
            repo.path(),
            &[
                "capture",
                "plan",
                "--source",
                "capture-source.md",
                "--output",
                forbidden.to_str().expect("managed path utf-8"),
            ],
        );
        assert!(
            error.contains("Memzoi-managed state")
                || error.contains("private runtime directory")
                || error.contains("generated exports"),
            "{error}"
        );
        assert!(!forbidden.exists());
    }
}

#[test]
fn capture_plan_enforces_private_and_blocked_artifact_storage_boundaries() {
    let repo = initialized_temp_repo();
    fs::write(
        repo.path().join("private-source.md"),
        "# Preference: Keep this local\n\nPrivate capture plans stay outside the repository.\n",
    )
    .expect("write private capture source");
    let forbidden = repo.path().join("private-plan.json");
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "private-source.md",
            "--output",
            forbidden.to_str().expect("private path utf-8"),
            "--json",
        ],
    );
    assert!(error.contains("private runtime directory"), "{error}");
    assert!(!forbidden.exists());

    let allowed = test_paths(repo.path())
        .runtime_dir
        .join("private-plan.json");
    let planned = run_json_command(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "private-source.md",
            "--output",
            allowed.to_str().expect("runtime path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&planned, &["data_class"], "private");
    let saved: Value = serde_json::from_slice(&fs::read(&allowed).expect("read private plan"))
        .expect("parse private plan");
    assert_eq!(saved, planned);

    let exports = test_paths(repo.path()).exports_dir;
    if exports.exists() {
        fs::remove_dir_all(&exports).expect("remove generated exports fixture");
    }
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "private-source.md",
            "--output",
            exports.to_str().expect("exports path utf-8"),
            "--json",
        ],
    );
    assert!(error.contains("generated exports"), "{error}");
    assert!(!exports.exists());

    fs::write(
        repo.path().join("blocked-source.md"),
        "# Fact: Redacted\n\napi_key = blocked-artifact-secret\n",
    )
    .expect("write blocked capture source");
    let blocked_output = test_paths(repo.path())
        .runtime_dir
        .join("blocked-plan.json");
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--source",
            "blocked-source.md",
            "--output",
            blocked_output.to_str().expect("blocked path utf-8"),
            "--json",
        ],
    );
    assert!(
        error.contains("blocked capture artifacts may only be emitted to stdout"),
        "{error}"
    );
    assert!(!blocked_output.exists());
}

#[test]
fn capture_review_binds_every_decision_without_mutating_memory_state() {
    let repo = initialized_temp_repo();
    let (source, plan_path, review_path, plan, review) = capture_plan_review_fixture(repo.path());
    let paths = test_paths(repo.path());
    let before = managed_state_snapshot(&paths);

    assert!(source.is_file());
    assert!(plan_path.is_file());
    assert!(review_path.is_file());
    assert_json_string_field(&review, &["schema"], "memzoi/capture-review-v2");
    assert_json_string_field(&review, &["plan_id"], json_string(&plan, "plan_id"));
    assert_json_string_field(&review, &["data_class"], "private");
    assert!(json_string(&review, "review_id").starts_with("review_"));
    assert_eq!(
        review["decisions"].as_array().map(Vec::len),
        plan["candidates"].as_array().map(Vec::len)
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(&review_path).expect("read review artifact"))
            .expect("parse review artifact");
    assert_eq!(saved, review);
    assert_eq!(managed_state_snapshot(&paths), before);

    let decisions_path = paths.runtime_dir.join("capture-decisions.json");
    let human = run_command_stdout(
        repo.path(),
        &[
            "capture",
            "review",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--decisions-file",
            decisions_path.to_str().expect("decisions path utf-8"),
            "--reviewed-by",
            "maintainer:test",
            "--reviewed-at",
            "2026-07-10T18:00:00Z",
        ],
    );
    assert!(human.starts_with("capture-review\n"), "{human}");
    assert!(human.contains(json_string(&review, "review_id")), "{human}");

    let forbidden = paths.memory_dir.join("capture-review.json");
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "review",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--decisions-file",
            decisions_path.to_str().expect("decisions path utf-8"),
            "--reviewed-by",
            "maintainer:test",
            "--reviewed-at",
            "2026-07-10T18:00:00Z",
            "--output",
            forbidden.to_str().expect("managed review path utf-8"),
        ],
    );
    assert!(error.contains("private runtime directory"), "{error}");
    assert!(!forbidden.exists());
    assert_eq!(managed_state_snapshot(&paths), before);
}

#[test]
fn capture_apply_rejects_stale_source_with_zero_memory_writes() {
    let repo = initialized_temp_repo();
    let (source, plan_path, review_path, plan, review) = capture_plan_review_fixture(repo.path());
    fs::write(
        &source,
        format!(
            "{}\nSource bytes changed after review.\n",
            capture_markdown_fixture()
        ),
    )
    .expect("change reviewed source");
    let paths = test_paths(repo.path());
    let before = managed_state_snapshot(&paths);

    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "apply",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--review-file",
            review_path.to_str().expect("review path utf-8"),
            "--plan-id",
            json_string(&plan, "plan_id"),
            "--review-id",
            json_string(&review, "review_id"),
            "--json",
        ],
    );
    assert!(error.contains("stale capture plan"), "{error}");
    assert_eq!(
        managed_state_snapshot(&paths),
        before,
        "stale apply must write nothing"
    );
}

#[test]
fn capture_apply_routes_repo_candidates_to_pending_proposals_only() {
    let repo = initialized_temp_repo();
    let (_source, plan_path, review_path, plan, review) = capture_plan_review_fixture(repo.path());
    let applied = run_json_command(
        repo.path(),
        &[
            "capture",
            "apply",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--review-file",
            review_path.to_str().expect("review path utf-8"),
            "--plan-id",
            json_string(&plan, "plan_id"),
            "--review-id",
            json_string(&review, "review_id"),
            "--json",
        ],
    );
    assert_json_string_field(&applied, &["schema"], "memzoi/capture-apply-result-v2");
    assert_json_string_field(&applied, &["plan_id"], json_string(&plan, "plan_id"));
    assert_json_string_field(&applied, &["review_id"], json_string(&review, "review_id"));
    let writes = applied["writes"]
        .as_array()
        .unwrap_or_else(|| panic!("capture apply should report writes: {applied}"));
    assert_eq!(writes.len(), plan["candidates"].as_array().unwrap().len());
    assert!(writes.iter().all(|write| write["kind"] == "proposal_file"));

    let paths = test_paths(repo.path());
    assert_eq!(
        fs::read_dir(paths.records_dir())
            .expect("read canonical records")
            .count(),
        0,
        "capture route apply must not create canonical memory"
    );
    let pending = paths.proposals_dir().join("pending");
    assert_eq!(
        fs::read_dir(&pending)
            .expect("read pending proposals")
            .count(),
        writes.len()
    );
    let validated = run_json_command(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated["valid"], true, "{validated}");
}

#[test]
fn capture_request_file_requires_and_replays_explicit_supplied_bytes() {
    let repo = initialized_temp_repo();
    let diff = concat!(
        "diff --git a/docs/cli.md b/docs/cli.md\n",
        "new file mode 100644\n",
        "index 0000000000000000000000000000000000000000..2222222222222222222222222222222222222222\n",
        "--- /dev/null\n",
        "+++ b/docs/cli.md\n",
        "@@ -0,0 +1,2 @@\n",
        "+# Decision: Replay supplied capture bytes\n",
        "+Use suppliedclicapturetoken only with exact replay.\n",
    );
    let bytes_path = repo.path().join("reviewed.diff");
    fs::write(&bytes_path, diff).expect("write supplied diff");
    let request_path = repo.path().join("capture-request.json");
    fs::write(
        &request_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "memzoi/capture-request-v2",
            "sources": [{
                "source_id": "reviewed-diff",
                "locator": {
                    "kind": "supplied_bytes",
                    "display_name": "reviewed.diff",
                    "media_type": "text/x-diff",
                    "byte_length": diff.len(),
                    "source_content_hash": format!("blake3:{}", blake3::hash(diff.as_bytes()).to_hex()),
                },
                "media_type": "text/x-diff",
                "git": {
                    "repository": ".",
                    "base": "sha1:1111111111111111111111111111111111111111",
                    "head": "sha1:2222222222222222222222222222222222222222",
                },
            }],
            "extractor": { "profile": "git-change-deterministic" },
        }))
        .expect("serialize capture request"),
    )
    .expect("write capture request");

    let missing = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "plan",
            "--request-file",
            request_path.to_str().expect("request path utf-8"),
            "--json",
        ],
    );
    assert!(missing.contains("requires --source-bytes"), "{missing}");

    let mut implicit_stdin = memzoi();
    implicit_stdin
        .args(["capture", "plan", "--request-file"])
        .arg(&request_path)
        .arg("--json")
        .write_stdin(diff)
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "supplied_bytes capture requires --source-bytes",
        ));

    let mut explicit_stdin = memzoi();
    let stdin_assert = explicit_stdin
        .args(["capture", "plan", "--request-file"])
        .arg(&request_path)
        .args(["--source-bytes", "-", "--json"])
        .write_stdin(diff)
        .current_dir(repo.path())
        .assert()
        .success();
    let stdin_plan = json_from_stdout(&stdin_assert.get_output().stdout);
    assert_json_string_field(&stdin_plan, &["status"], "ready");
    assert_eq!(stdin_plan["candidates"].as_array().map(Vec::len), Some(1));

    let plan_path = repo.path().join("supplied-plan.json");
    let plan = run_json_command(
        repo.path(),
        &[
            "capture",
            "plan",
            "--request-file",
            request_path.to_str().expect("request path utf-8"),
            "--source-bytes",
            bytes_path.to_str().expect("source bytes path utf-8"),
            "--output",
            plan_path.to_str().expect("plan path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&plan, &["status"], "ready");
    assert_eq!(plan["candidates"].as_array().map(Vec::len), Some(1));

    let decisions_path = repo.path().join("supplied-decisions.json");
    fs::write(
        &decisions_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "memzoi/capture-review-input-v2",
            "plan_id": json_string(&plan, "plan_id"),
            "decisions": [{
                "candidate_id": json_string(&plan["candidates"][0], "candidate_id"),
                "outcome": "accept",
            }],
        }))
        .expect("serialize supplied capture decisions"),
    )
    .expect("write supplied capture decisions");
    let review = run_json_command(
        repo.path(),
        &[
            "capture",
            "review",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--decisions-file",
            decisions_path.to_str().expect("decisions path utf-8"),
            "--source-bytes",
            bytes_path.to_str().expect("source bytes path utf-8"),
            "--reviewed-by",
            "maintainer:test",
            "--reviewed-at",
            "2026-07-11T12:00:00Z",
            "--json",
        ],
    );
    assert_json_string_field(&review, &["plan_id"], json_string(&plan, "plan_id"));

    fs::write(&bytes_path, "changed").expect("change supplied bytes");
    let stale = run_command_failure_stderr(
        repo.path(),
        &[
            "capture",
            "review",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--decisions-file",
            decisions_path.to_str().expect("decisions path utf-8"),
            "--source-bytes",
            bytes_path.to_str().expect("source bytes path utf-8"),
            "--reviewed-by",
            "maintainer:test",
            "--reviewed-at",
            "2026-07-11T12:00:00Z",
            "--json",
        ],
    );
    assert!(stale.contains("stale capture plan"), "{stale}");
}
