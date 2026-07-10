use std::{fs, path::Path};

use memzoi_core::{
    InitRequest, MemoryPaths, MemoryService, parse_import_document, read_okf_proposal_files,
};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

const MIXED_MANIFEST: &str = r#"
version: memzoi/import-v1
sources:
  - path: imports/not-read.yml
candidates:
  - destination: repo
    reason: durable project convention
    type: decision
    lane: semantic
    title: Repository convention
    body: The repository uses explicit review before durable memory changes.
    sensitivity: repo-safe
    scope:
      kind: repo
    tags: [workflow]
  - destination: local
    reason: private developer preference
    type: fact
    title: Local preference
    body: Keep this preference in local runtime memory only.
    sensitivity: local-only
  - destination: session
    reason: current handoff continuity
    type: episode
    lane: session
    title: Session continuity
    body: Resume the import review in the next session.
    sensitivity: local-only
  - destination: discard
    reason: stale transient note
    type: fact
    title: Transient note
    body: This note is no longer useful.
    sensitivity: unknown
  - destination: needs_review
    reason: ambiguous privacy boundary
    type: fact
    title: Ambiguous note
    body: Decide whether this content is safe to retain.
    sensitivity: unknown
"#;

fn initialized_service(temp: &TempDir) -> anyhow::Result<MemoryService> {
    let project_root = temp.path().join("project");
    fs::create_dir_all(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    MemoryService::open_paths(paths)
}

fn candidate(plan: &Value, index: usize) -> &Value {
    &plan["candidates"][index]
}

fn action_kind(candidate: &Value) -> &str {
    candidate["action"]["kind"]
        .as_str()
        .expect("import action should have a serialized kind")
}

fn file_names(dir: &Path) -> anyhow::Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = fs::read_dir(dir)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn mixed_manifest_is_review_first_and_respects_all_destination_boundaries() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let document = parse_import_document(MIXED_MANIFEST)?;

    let plan = service.plan_import("test", document.clone())?;
    let plan_again = service.plan_import("test", document.clone())?;
    let plan_json = serde_json::to_value(&plan)?;
    let plan_again_json = serde_json::to_value(&plan_again)?;

    assert_eq!(plan_json["plan_id"], plan_again_json["plan_id"]);
    assert_eq!(plan_json["candidates"].as_array().unwrap().len(), 5);
    assert_eq!(plan_json["summary"]["total"], 5);
    assert_eq!(plan_json["summary"]["create_proposals"], 1);
    assert_eq!(plan_json["summary"]["local_writes"], 1);
    assert_eq!(plan_json["summary"]["session_writes"], 1);
    assert_eq!(plan_json["summary"]["discarded"], 1);
    assert_eq!(plan_json["summary"]["needs_review"], 1);
    assert_eq!(
        candidate(&plan_json, 0)["classification"]["destination"],
        "repo"
    );
    assert_eq!(action_kind(candidate(&plan_json, 0)), "create_proposal");
    assert_eq!(
        candidate(&plan_json, 1)["classification"]["destination"],
        "local"
    );
    assert_eq!(action_kind(candidate(&plan_json, 1)), "create_runtime");
    assert_eq!(candidate(&plan_json, 1)["action"]["route"], "runtime_local");
    assert_eq!(
        candidate(&plan_json, 2)["classification"]["destination"],
        "session"
    );
    assert_eq!(action_kind(candidate(&plan_json, 2)), "create_runtime");
    assert_eq!(
        candidate(&plan_json, 2)["action"]["route"],
        "runtime_session"
    );
    assert_eq!(
        candidate(&plan_json, 3)["classification"]["destination"],
        "discard"
    );
    assert_eq!(action_kind(candidate(&plan_json, 3)), "no_write");
    assert_eq!(
        candidate(&plan_json, 4)["classification"]["destination"],
        "needs_review"
    );
    assert_eq!(action_kind(candidate(&plan_json, 4)), "blocked");

    assert!(file_names(&service.paths().records_dir())?.is_empty());
    assert!(file_names(&service.paths().proposals_dir())?.is_empty());

    let applied = service.apply_import("test", document, plan_json["plan_id"].as_str().unwrap())?;
    let applied_json = serde_json::to_value(&applied)?;
    let writes = applied_json["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0]["kind"], "proposal_file");
    assert_eq!(writes[0]["index"], 0);
    assert_eq!(writes[1]["kind"], "runtime_record");
    assert_eq!(writes[1]["index"], 1);
    assert_eq!(writes[1]["destination"], "local");
    assert_eq!(writes[2]["kind"], "runtime_record");
    assert_eq!(writes[2]["index"], 2);
    assert_eq!(writes[2]["destination"], "session");
    assert!(file_names(&service.paths().records_dir())?.is_empty());
    let proposal_files = file_names(&service.paths().proposals_dir())?;
    assert_eq!(proposal_files.len(), 1);

    let proposals = read_okf_proposal_files(service.paths().proposals_dir().join("pending"))?;
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].proposal.action.as_str(), "create");
    assert_eq!(
        proposals[0].body,
        "The repository uses explicit review before durable memory changes."
    );
    assert_eq!(proposals[0].status.as_str(), "proposed");

    let local = service.list_local_memory()?;
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].title, "Local preference");
    assert_eq!(
        local[0].body,
        "Keep this preference in local runtime memory only."
    );
    let session = service.list_checkpoints()?;
    assert_eq!(session.len(), 1);
    assert_eq!(session[0].title, "Session continuity");
    assert_eq!(
        session[0].body,
        "Resume the import review in the next session."
    );
    Ok(())
}

#[test]
fn serialized_repo_import_plan_uses_posix_relative_proposal_path() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let document = parse_import_document(MIXED_MANIFEST)?;

    let plan = service.plan_import("test", document)?;
    let plan_json = serde_json::to_value(&plan)?;
    let action = &plan_json["candidates"][0]["action"];
    let proposal_id = action["proposal_id"]
        .as_str()
        .expect("create-proposal action should serialize a proposal id");
    let path = action["path"]
        .as_str()
        .expect("create-proposal action should serialize a path");

    assert_eq!(action["kind"], "create_proposal");
    assert_eq!(path, format!(".memzoi/proposals/pending/{proposal_id}.md"));
    assert!(!path.contains('\\'));
    Ok(())
}

#[test]
fn wrong_or_stale_plan_id_is_a_zero_write_guard() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let document = parse_import_document(MIXED_MANIFEST)?;
    let plan = service.plan_import("test", document.clone())?;
    let plan_id = serde_json::to_value(&plan)?["plan_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let before_proposals = file_names(&service.paths().proposals_dir())?;
    let before_records = file_names(&service.paths().records_dir())?;
    let database_before = fs::read(&service.paths().db_path)?;
    let error = service
        .apply_import("test", document, &format!("{plan_id}-stale"))
        .expect_err("stale plan id must be rejected");
    assert!(error.to_string().to_lowercase().contains("plan"));
    assert_eq!(
        file_names(&service.paths().proposals_dir())?,
        before_proposals
    );
    assert_eq!(file_names(&service.paths().records_dir())?, before_records);
    assert_eq!(fs::read(&service.paths().db_path)?, database_before);
    Ok(())
}

#[test]
fn an_existing_pending_proposal_is_an_exact_duplicate() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let document = parse_import_document(MIXED_MANIFEST)?;
    let first_plan = service.plan_import("test", document.clone())?;
    let first_id = serde_json::to_value(&first_plan)?["plan_id"]
        .as_str()
        .unwrap()
        .to_owned();
    service.apply_import("test", document.clone(), &first_id)?;

    let second = service.plan_import("test", document)?;
    let second_json = serde_json::to_value(&second)?;
    for index in 0..=2 {
        assert_eq!(action_kind(candidate(&second_json, index)), "duplicate");
        assert!(
            !candidate(&second_json, index)["duplicates"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(file_names(&service.paths().proposals_dir())?.len(), 1);
    assert!(file_names(&service.paths().records_dir())?.is_empty());
    Ok(())
}

#[test]
fn malformed_documents_fail_and_non_safe_repo_candidates_are_explicitly_blocked()
-> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let before_proposals = file_names(&service.paths().proposals_dir())?;
    let before_records = file_names(&service.paths().records_dir())?;

    for manifest in [
        "version: memzoi/import-v1\ncandidates: [",
        "version: memzoi/import-v1\nsources:\n  - path: ../secret.yml\ncandidates: []",
        "version: memzoi/import-v1\nunknown: true\nsources: []\ncandidates: []",
    ] {
        assert!(parse_import_document(manifest).is_err());
    }

    let repo_unsafe = r#"
version: memzoi/import-v1
sources:
  - path: imports/IMPORT-SOURCE-SENTINEL.yml
candidates:
  - destination: repo
    reason: IMPORT-REASON-SENTINEL
    type: fact
    title: OMITTED-SENSITIVITY-TITLE-SENTINEL
    body: OMITTED-SENSITIVITY-BODY-SENTINEL
    scope:
      kind: repo
      paths: [src/IMPORT-PATH-SENTINEL/**]
  - destination: repo
    reason: durable project fact
    type: fact
    title: Safe repository candidate
    body: This repo-safe candidate may become a reviewed proposal.
    sensitivity: repo-safe
"#;
    let document = parse_import_document(repo_unsafe)?;
    let plan = service.plan_import("test", document.clone())?;
    let plan_json = serde_json::to_value(&plan)?;
    assert_eq!(candidate(&plan_json, 0)["sensitivity"], "unknown");
    assert_eq!(action_kind(candidate(&plan_json, 0)), "blocked");
    assert_eq!(action_kind(candidate(&plan_json, 1)), "create_proposal");
    let blocked_reason = candidate(&plan_json, 0)["action"]["reason"]
        .as_str()
        .expect("blocked import candidate should include a reason");
    assert!(blocked_reason.contains("got unknown"));
    assert!(!blocked_reason.contains("SENTINEL"));
    let rendered_plan = serde_json::to_string(&plan)?;
    for sentinel in [
        "IMPORT-SOURCE-SENTINEL",
        "IMPORT-REASON-SENTINEL",
        "OMITTED-SENSITIVITY-TITLE-SENTINEL",
        "OMITTED-SENSITIVITY-BODY-SENTINEL",
        "IMPORT-PATH-SENTINEL",
    ] {
        assert!(
            !rendered_plan.contains(sentinel),
            "blocked import output leaked {sentinel}: {rendered_plan}"
        );
    }

    let result = service.apply_import("test", document, &plan.plan_id)?;
    assert_eq!(result.writes.len(), 1);
    assert_eq!(result.plan.summary.create_proposals, 1);
    assert_eq!(result.plan.summary.needs_review, 1);
    let pending = service.paths().proposals_dir().join("pending");
    let proposals = read_okf_proposal_files(&pending)?;
    assert_eq!(proposals.len(), 1);
    let rendered = fs::read_to_string(pending.join(format!("{}.md", proposals[0].file_id)))?;
    assert!(!rendered.contains("SENTINEL"));
    assert_eq!(file_names(&service.paths().records_dir())?, before_records);
    assert_ne!(
        file_names(&service.paths().proposals_dir())?,
        before_proposals
    );
    Ok(())
}

#[test]
fn empty_or_whitespace_actor_is_rejected_before_import_planning_or_writes() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let service = initialized_service(&temp)?;
    let document = parse_import_document(MIXED_MANIFEST)?;

    for actor in ["", "   \t\n"] {
        let error = service
            .plan_import(actor, document.clone())
            .expect_err("an empty or whitespace actor must be rejected during planning");
        assert!(
            error.to_string().to_lowercase().contains("actor"),
            "actor validation error should identify the invalid actor: {error}"
        );
        assert!(file_names(&service.paths().proposals_dir())?.is_empty());
        assert!(file_names(&service.paths().proposals_dir().join("pending"))?.is_empty());
        assert!(file_names(&service.paths().records_dir())?.is_empty());
    }

    Ok(())
}
