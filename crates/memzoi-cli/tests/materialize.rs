use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use assert_cmd::Command;
use memzoi_core::{
    CANONICAL_REVISION_SCHEMA, CanonicalRevision, ExpectedPriorRevision, MaterializationAction,
    MaterializationTarget, MemoryDraft, MemoryLane, MemoryPaths, MemoryStatus, MemoryType,
    OkfProposalSensitivity, RepositoryContentClass, RepositoryMaterializationCandidate,
    RepositoryMaterializationCandidateRecord, ScopeKind, Visibility,
    build_repository_materialization_candidate,
};
use serde::Serialize;
use serde_json::Value;

fn memzoi(home: &Path) -> Command {
    let mut command = Command::cargo_bin("memzoi").expect("memzoi binary");
    command.env("MEMZOI_HOME", home);
    command
}

fn initialized_repo() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().expect("temp repository");
    let home = tempfile::tempdir().expect("temporary Memzoi home");
    run_git(repo.path(), &["init", "-q"]);

    let mut init = memzoi(home.path());
    init.current_dir(repo.path())
        .args(["init", "--json"])
        .assert()
        .success();
    (repo, home)
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = StdCommand::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run Git assertion command");
    assert!(
        output.status.success(),
        "Git assertion command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Git stdout is UTF-8")
}

fn materialization_paths(repo: &Path, home: &Path) -> MemoryPaths {
    MemoryPaths::with_runtime_home(
        repo.canonicalize().expect("canonical repository root"),
        home.to_path_buf(),
    )
}

fn create_candidate(concept_id: &str) -> RepositoryMaterializationCandidate {
    build_repository_materialization_candidate(
        candidate_record(concept_id, MemoryStatus::Active, None),
        MaterializationAction::Create,
        ExpectedPriorRevision::Absent,
        None,
        None,
    )
    .expect("build strict create candidate")
}

fn supersede_candidate(concept_id: &str) -> RepositoryMaterializationCandidate {
    let expected_revision = CanonicalRevision {
        schema: CANONICAL_REVISION_SCHEMA.to_owned(),
        revision_hash: blake3_identity('a'),
    };
    build_repository_materialization_candidate(
        candidate_record(
            concept_id,
            MemoryStatus::Active,
            Some("existing-record".to_owned()),
        ),
        MaterializationAction::Supersede,
        ExpectedPriorRevision::Absent,
        Some(MaterializationTarget {
            record_id: "existing-record".to_owned(),
            expected_revision,
        }),
        Some("replace the existing canonical record".to_owned()),
    )
    .expect("build strict supersede candidate")
}

fn candidate_record(
    concept_id: &str,
    status: MemoryStatus,
    supersedes_id: Option<String>,
) -> RepositoryMaterializationCandidateRecord {
    RepositoryMaterializationCandidateRecord {
        concept_id: concept_id.to_owned(),
        draft: MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: "Direct materialization fixture".to_owned(),
            body: "A direct materialization candidate remains reviewable before apply.".to_owned(),
            tags: vec!["materialization".to_owned()],
            source_kind: Some("issue".to_owned()),
            source_ref: Some("issue://100".to_owned()),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 0.9,
        },
        status,
        applies_to: vec!["crates/memzoi-cli/src/commands.rs".to_owned()],
        created: "2026-07-16T12:00:00Z".to_owned(),
        updated: None,
        supersedes_id,
        retention: memzoi_core::retention_facts_for_creation(
            MemoryLane::Semantic,
            "2026-07-16T12:00:00Z",
            None,
            None,
        )
        .expect("valid materialization fixture retention"),
        origin: memzoi_core::OriginDescriptor::new(
            format!("repository-materialization:test:{concept_id}"),
            memzoi_core::OriginRoute::RepositoryMaterialization,
        ),
        lineage: None,
        proposal_id: None,
        capture: None,
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    let mut bytes = serde_json::to_vec_pretty(value).expect("serialize JSON fixture");
    bytes.push(b'\n');
    fs::write(path, bytes).expect("write JSON fixture");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read JSON artifact"))
        .expect("parse JSON artifact")
}

fn run_json(repo: &Path, home: &Path, args: &[&str]) -> Value {
    let mut command = memzoi(home);
    let assert = command.args(args).current_dir(repo).assert().success();
    serde_json::from_slice(&assert.get_output().stdout).expect("parse JSON command output")
}

fn run_stdout(repo: &Path, home: &Path, args: &[&str]) -> String {
    let mut command = memzoi(home);
    let assert = command.args(args).current_dir(repo).assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).expect("command stdout is UTF-8")
}

fn run_failure_stderr(repo: &Path, home: &Path, args: &[&str]) -> String {
    let mut command = memzoi(home);
    let assert = command.args(args).current_dir(repo).assert().failure();
    String::from_utf8(assert.get_output().stderr.clone()).expect("command stderr is UTF-8")
}

fn materialization_artifacts(
    repo: &Path,
    home: &Path,
    candidate_path: &Path,
    stem: &str,
) -> (Value, PathBuf, Value, PathBuf) {
    let artifact_dir = repo.join("artifacts");
    fs::create_dir_all(&artifact_dir).expect("create explicit artifact directory");
    let plan_path = artifact_dir.join(format!("{stem}-plan.json"));
    let decision_path = artifact_dir.join(format!("{stem}-decision.json"));
    let candidate_path = candidate_path.to_str().expect("candidate path is UTF-8");
    let plan_path_text = plan_path.to_str().expect("plan path is UTF-8");
    let decision_path_text = decision_path.to_str().expect("decision path is UTF-8");

    let plan = run_json(
        repo,
        home,
        &[
            "materialize",
            "plan",
            "--candidate-file",
            candidate_path,
            "--output",
            plan_path_text,
            "--json",
        ],
    );
    let decision = run_json(
        repo,
        home,
        &[
            "materialize",
            "decide",
            "--candidate-file",
            candidate_path,
            "--plan-file",
            plan_path_text,
            "--decision-at",
            "2026-07-16T13:00:00Z",
            "--output",
            decision_path_text,
            "--json",
        ],
    );
    (plan, plan_path, decision, decision_path)
}

#[test]
fn materialize_plan_and_decide_only_write_explicit_artifacts() {
    let (repo, home) = initialized_repo();
    let paths = materialization_paths(repo.path(), home.path());
    let candidate_path = repo.path().join("candidate.json");
    write_json(&candidate_path, &create_candidate("materialize-plan"));
    let before = managed_state_snapshot(&paths);

    let (plan, plan_path, decision, decision_path) =
        materialization_artifacts(repo.path(), home.path(), &candidate_path, "read-only");

    assert_eq!(
        managed_state_snapshot(&paths),
        before,
        "plan and decide must not alter records, proposal packets, or runtime state"
    );
    assert_eq!(plan["schema"], "memzoi/repository-materialization-plan");
    assert_eq!(
        decision["schema"],
        "memzoi/repository-materialization-decision"
    );
    assert_eq!(read_json(&plan_path), plan);
    assert_eq!(read_json(&decision_path), decision);
    let forbidden = paths.memory_dir.join("forbidden-plan.json");
    let forbidden_error = run_failure_stderr(
        repo.path(),
        home.path(),
        &[
            "materialize",
            "plan",
            "--candidate-file",
            candidate_path.to_str().expect("candidate path is UTF-8"),
            "--output",
            forbidden.to_str().expect("forbidden path is UTF-8"),
        ],
    );
    assert!(
        forbidden_error.contains("cannot be saved inside .memzoi"),
        "{forbidden_error}"
    );
    assert!(!forbidden.exists());
    assert_eq!(managed_state_snapshot(&paths), before);
}

#[test]
fn materialize_apply_writes_an_unstaged_canonical_record_and_reports_review_data() {
    let (repo, home) = initialized_repo();
    let paths = materialization_paths(repo.path(), home.path());
    let concept_id = "materialize-apply";
    let candidate_path = repo.path().join("candidate.json");
    write_json(&candidate_path, &create_candidate(concept_id));
    let (plan, plan_path, decision, decision_path) =
        materialization_artifacts(repo.path(), home.path(), &candidate_path, "apply");
    let candidate = read_json(&candidate_path);
    let candidate_path_text = candidate_path.to_str().expect("candidate path is UTF-8");
    let plan_path_text = plan_path.to_str().expect("plan path is UTF-8");
    let decision_path_text = decision_path.to_str().expect("decision path is UTF-8");
    let candidate_id = candidate["candidate_id"]
        .as_str()
        .expect("candidate identity");
    let plan_id = plan["plan_id"].as_str().expect("plan identity");
    let decision_id = decision["decision_id"].as_str().expect("decision identity");
    let apply_args = [
        "materialize",
        "apply",
        "--candidate-file",
        candidate_path_text,
        "--plan-file",
        plan_path_text,
        "--decision-file",
        decision_path_text,
        "--candidate-id",
        candidate_id,
        "--plan-id",
        plan_id,
        "--decision-id",
        decision_id,
        "--json",
    ];

    let applied = run_json(repo.path(), home.path(), &apply_args);
    assert_eq!(
        applied["schema"],
        "memzoi/repository-materialization-apply-report"
    );
    let record_path = paths.records_dir().join(format!("{concept_id}.md"));
    assert!(
        record_path.is_file(),
        "apply must create the canonical record"
    );
    assert_eq!(
        applied["result"]["schema"],
        "memzoi/repository-materialization-result"
    );
    assert_eq!(
        applied["result"]["outputs"][0]["path"],
        format!(".memzoi/records/{concept_id}.md")
    );
    assert_eq!(applied["result"]["outputs"][0]["action"], "create");
    assert!(
        applied["result"]["outputs"][0]["semantic_revision"]["revision_hash"]
            .as_str()
            .is_some_and(|identity| identity.starts_with("blake3:"))
    );
    assert_eq!(
        applied["review"][0]["command_argv"],
        serde_json::json!([
            "git",
            "diff",
            "--no-index",
            "--",
            "/dev/null",
            format!(".memzoi/records/{concept_id}.md"),
        ])
    );
    assert_eq!(
        git_stdout(
            repo.path(),
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                &format!(".memzoi/records/{concept_id}.md"),
            ],
        ),
        format!("?? .memzoi/records/{concept_id}.md\n"),
        "apply must leave the canonical record unstaged"
    );

    let human_args = &apply_args[..apply_args.len() - 1];
    let human = run_stdout(repo.path(), home.path(), human_args);
    assert!(human.contains(&format!("changed: .memzoi/records/{concept_id}.md")));
    assert!(human.contains("action: create"));
    assert!(human.contains("record_id: materialize-apply"));
    assert!(human.contains("semantic_revision: blake3:"));
    assert!(
        human.contains(&format!(
            "review: git diff --no-index -- /dev/null .memzoi/records/{concept_id}.md"
        )),
        "human output must provide an exact review command: {human}"
    );
}

#[test]
fn materialize_apply_rejects_tampered_artifacts_and_supplied_ids_without_writes() {
    let (repo, home) = initialized_repo();
    let paths = materialization_paths(repo.path(), home.path());
    let candidate_path = repo.path().join("candidate.json");
    write_json(&candidate_path, &create_candidate("materialize-reject"));
    let (plan, plan_path, decision, decision_path) =
        materialization_artifacts(repo.path(), home.path(), &candidate_path, "reject");
    let candidate = read_json(&candidate_path);
    let before = managed_state_snapshot(&paths);
    let candidate_path_text = candidate_path.to_str().expect("candidate path is UTF-8");
    let plan_path_text = plan_path.to_str().expect("plan path is UTF-8");
    let decision_path_text = decision_path.to_str().expect("decision path is UTF-8");
    let plan_id = plan["plan_id"].as_str().expect("plan identity");
    let decision_id = decision["decision_id"].as_str().expect("decision identity");

    let mismatch = run_failure_stderr(
        repo.path(),
        home.path(),
        &[
            "materialize",
            "apply",
            "--candidate-file",
            candidate_path_text,
            "--plan-file",
            plan_path_text,
            "--decision-file",
            decision_path_text,
            "--candidate-id",
            &blake3_identity('0'),
            "--plan-id",
            plan_id,
            "--decision-id",
            decision_id,
            "--json",
        ],
    );
    assert!(mismatch.contains("--candidate-id"), "{mismatch}");
    assert_eq!(managed_state_snapshot(&paths), before);

    let mut tampered = decision.clone();
    tampered["decision_id"] = Value::String(blake3_identity('f'));
    let tampered_path = repo.path().join("artifacts/tampered-decision.json");
    write_json(&tampered_path, &tampered);
    let tampered_path_text = tampered_path.to_str().expect("tampered path is UTF-8");
    let tampered_error = run_failure_stderr(
        repo.path(),
        home.path(),
        &[
            "materialize",
            "apply",
            "--candidate-file",
            candidate_path_text,
            "--plan-file",
            plan_path_text,
            "--decision-file",
            tampered_path_text,
            "--candidate-id",
            candidate["candidate_id"]
                .as_str()
                .expect("candidate identity"),
            "--plan-id",
            plan_id,
            "--decision-id",
            tampered["decision_id"].as_str().expect("tampered identity"),
            "--json",
        ],
    );
    assert!(
        tampered_error.contains("decision artifact failed core validation"),
        "{tampered_error}"
    );
    assert_eq!(managed_state_snapshot(&paths), before);
    assert!(
        !paths.records_dir().join("materialize-reject.md").exists(),
        "failed applies must not create canonical records"
    );
}

#[test]
fn materialize_apply_rejects_lifecycle_actions_without_creating_a_record() {
    let (repo, home) = initialized_repo();
    let paths = materialization_paths(repo.path(), home.path());
    let concept_id = "materialize-replacement";
    let candidate_path = repo.path().join("candidate.json");
    write_json(&candidate_path, &supersede_candidate(concept_id));
    let (plan, plan_path, decision, decision_path) =
        materialization_artifacts(repo.path(), home.path(), &candidate_path, "unsupported");
    let candidate = read_json(&candidate_path);
    let before = managed_state_snapshot(&paths);
    let candidate_path_text = candidate_path.to_str().expect("candidate path is UTF-8");
    let plan_path_text = plan_path.to_str().expect("plan path is UTF-8");
    let decision_path_text = decision_path.to_str().expect("decision path is UTF-8");

    let error = run_failure_stderr(
        repo.path(),
        home.path(),
        &[
            "materialize",
            "apply",
            "--candidate-file",
            candidate_path_text,
            "--plan-file",
            plan_path_text,
            "--decision-file",
            decision_path_text,
            "--candidate-id",
            candidate["candidate_id"]
                .as_str()
                .expect("candidate identity"),
            "--plan-id",
            plan["plan_id"].as_str().expect("plan identity"),
            "--decision-id",
            decision["decision_id"].as_str().expect("decision identity"),
            "--json",
        ],
    );
    assert!(
        error.contains("supports only create and update actions"),
        "{error}"
    );
    assert_eq!(managed_state_snapshot(&paths), before);
    assert!(
        !paths
            .records_dir()
            .join(format!("{concept_id}.md"))
            .exists()
    );
}

fn blake3_identity(fill: char) -> String {
    format!("blake3:{}", fill.to_string().repeat(64))
}

fn managed_state_snapshot(paths: &MemoryPaths) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    snapshot_tree(&paths.memory_dir, Path::new("repo"), &mut snapshot);
    snapshot_tree(&paths.runtime_dir, Path::new("runtime"), &mut snapshot);
    snapshot
}

fn snapshot_tree(root: &Path, label: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
    if !root.exists() {
        return;
    }
    snapshot.insert(label.to_path_buf(), b"directory".to_vec());
    snapshot_tree_entries(root, root, label, snapshot);
}

fn snapshot_tree_entries(
    root: &Path,
    directory: &Path,
    label: &Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let mut entries = fs::read_dir(directory)
        .expect("read managed state directory")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("collect managed state entries");
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("managed state entry lies beneath root");
        let key = label.join(relative);
        let metadata = fs::symlink_metadata(&path).expect("inspect managed state entry");
        if metadata.is_dir() {
            snapshot.insert(key, b"directory".to_vec());
            snapshot_tree_entries(root, &path, label, snapshot);
        } else if metadata.is_file() {
            let mut bytes = b"file\0".to_vec();
            bytes.extend(fs::read(&path).expect("read managed state file"));
            snapshot.insert(key, bytes);
        } else if metadata.file_type().is_symlink() {
            let mut bytes = b"symlink\0".to_vec();
            bytes.extend(
                fs::read_link(&path)
                    .expect("read managed state symlink")
                    .as_os_str()
                    .as_encoded_bytes(),
            );
            snapshot.insert(key, bytes);
        } else {
            snapshot.insert(key, b"special".to_vec());
        }
    }
}
