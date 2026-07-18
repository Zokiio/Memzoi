use super::*;

#[test]
fn rebuild_refuses_to_delete_unreadable_runtime_db() {
    let isolated_home = tempfile::tempdir().expect("isolated memzoi home");
    let repo = initialized_temp_repo_with_home(isolated_home.path());
    let repo = repo.path();
    let db_path = MemoryPaths::with_runtime_home(
        repo.canonicalize().expect("canonical repo path"),
        isolated_home.path().to_path_buf(),
    )
    .shared_db_path;
    let original_bytes = b"not a sqlite database with local runtime memory";
    fs::write(&db_path, original_bytes).expect("corrupt runtime db");

    let stderr =
        run_command_failure_stderr_with_home(repo, &["rebuild", "--json"], isolated_home.path());
    assert!(
        stderr.contains("local/session runtime memory could not be"),
        "rebuild should explain that local runtime memory could not be preserved: {stderr}"
    );
    assert_eq!(
        fs::read(&db_path).expect("runtime db should remain after failed rebuild"),
        original_bytes,
        "failed rebuild must not delete an unreadable runtime db"
    );
}

#[test]
fn context_json_returns_prompt_ready_pack_records_and_citations() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let matching = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI context procedure",
        "Path-bound zircon context should be included in prompt-ready output for context.rs.",
    );
    attach_memory_path(repo, &matching, "crates/memzoi-core/src/context.rs");
    update_record_source_ref(repo, &matching, "issue://cli-context#procedure");

    let unrelated_path = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI unrelated path",
        "This memory matches zircon text but belongs to a different source path.",
    );
    attach_memory_path(repo, &unrelated_path, "crates/memzoi-cli/src/main.rs");
    update_record_source_ref(repo, &unrelated_path, "issue://cli-context#global");

    let tombstoned = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI context tombstoned",
        "Inactive context memory must not be rendered into the prompt.",
    );
    attach_memory_path(repo, &tombstoned, "crates/memzoi-core/src/context.rs");
    update_record_source_ref(repo, &tombstoned, "issue://cli-context#old");
    run_json_command(
        repo,
        &[
            "tombstone",
            tombstoned.as_str(),
            "--reason",
            "obsolete context",
            "--json",
        ],
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "Need zircon context procedure while editing context.rs",
            "--path",
            "crates/memzoi-core/src/context.rs",
            "--token-budget",
            "60",
            "--json",
        ],
    );

    let ids = record_ids_from_json(&pack);
    assert!(
        ids.contains(&matching.as_str()),
        "context JSON should include the path-relevant active memory record: {pack}"
    );
    assert!(
        !ids.contains(&tombstoned.as_str()),
        "context JSON should suppress inactive records: {pack}"
    );
    assert_eq!(
        ids.first().copied(),
        Some(matching.as_str()),
        "path-relevant memory should rank first in context records when --path is supplied: {pack}"
    );

    let prompt = prompt_text(&pack)
        .unwrap_or_else(|| panic!("context JSON should include prompt-ready text: {pack}"));
    assert!(
        prompt.contains("Path-bound zircon context")
            || prompt.contains("Zircon CLI context procedure"),
        "prompt-ready text should include the relevant active memory: {prompt:?}"
    );
    assert!(
        !prompt.contains("Inactive context memory"),
        "prompt-ready text should not include tombstoned memory: {prompt:?}"
    );
    assert!(
        prompt.split_whitespace().count() <= 80,
        "context --token-budget should cap prompt-ready output approximately: {prompt:?}"
    );

    assert_eq!(pack["budget"]["requested"].as_u64(), Some(60));
    assert_eq!(pack["budget"]["effective"].as_u64(), Some(60));
    assert_eq!(
        pack["budget"]["estimate_unit"].as_str(),
        Some("approx_words")
    );
    assert!(
        pack["budget"]["estimated_used"]
            .as_u64()
            .is_some_and(|used| used > 0),
        "context JSON should expose estimated budget use: {pack}"
    );
    let included = pack["included"]
        .as_array()
        .unwrap_or_else(|| panic!("context JSON should expose included metadata: {pack}"));
    assert!(
        included.iter().any(|item| {
            item.get("record_id").and_then(Value::as_str) == Some(matching.as_str())
                && item.get("type").and_then(Value::as_str) == Some("procedure")
                && item.get("provenance").and_then(Value::as_str) == Some("git")
                && item.get("destination").and_then(Value::as_str) == Some("repo")
        }),
        "context JSON should expose included record provenance metadata: {pack}"
    );
    assert!(
        pack["next_queries"].as_array().is_some_and(Vec::is_empty),
        "context JSON should include an empty next_queries array for now: {pack}"
    );

    let citation = citation_for_record(&pack, &matching)
        .unwrap_or_else(|| panic!("context JSON should cite {matching}: {pack}"));
    assert_json_string_field(citation, &["record_id", "id"], &matching);
    assert_json_string_field(citation, &["type", "memory_type"], "procedure");
    assert_json_string_field(citation, &["scope", "scope_kind"], "repo");
    assert_json_string_field(citation, &["destination"], "repo");
    assert_json_string_field(citation, &["visibility"], "repo");
    assert_eq!(citation["source_kind"], Value::Null);
    assert_json_string_field(citation, &["source_ref"], "issue://cli-context#procedure");
    let first_record = pack["records"]
        .as_array()
        .and_then(|records| records.first())
        .unwrap_or_else(|| panic!("context JSON should include selected records: {pack}"));
    assert!(
        first_record.get("ranking").is_some(),
        "context JSON should expose ranking metadata per selected record: {pack}"
    );
    assert_eq!(
        pack["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );
    assert_eq!(
        pack["budget"]["selected_records"].as_u64(),
        Some(ids.len() as u64)
    );
}

#[test]
fn context_include_local_and_session_flags_are_explicit_opt_ins() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Layered omega repo decision",
        "Layered omega context should include repo memory by default.",
    );
    let local = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Layered omega local preference",
            "--body",
            "Layered omega context should include local memory only with explicit opt-in.",
            "--json",
        ],
    );
    let local_id = json_string(&local, "record_id").to_owned();
    let checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Layered omega session checkpoint",
            "--note",
            "Layered omega context should include session memory only with explicit opt-in.",
            "--operation-id",
            "layered-omega-checkpoint",
            "--json",
        ],
    );
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();

    let default_pack = run_json_command(
        repo,
        &["context", "--task", "layered omega context", "--json"],
    );
    let default_ids = record_ids_from_json(&default_pack);
    assert_eq!(
        default_ids,
        vec![repo_record.as_str()],
        "context should be repo-only by default: {default_pack}"
    );
    assert_json_does_not_reference_records(
        &default_pack,
        &[local_id.clone(), checkpoint_id.clone()],
    );
    assert_eq!(
        default_pack["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );

    let layered_pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "layered omega context",
            "--include-local",
            "--include-session",
            "--json",
        ],
    );
    let layered_ids = record_ids_from_json(&layered_pack);
    assert!(layered_ids.contains(&repo_record.as_str()));
    assert!(layered_ids.contains(&local_id.as_str()));
    assert!(layered_ids.contains(&checkpoint_id.as_str()));
    assert_eq!(
        layered_pack["policy"]["requested_destinations"],
        serde_json::json!(["repo", "local", "session"])
    );
    let prompt = prompt_text(&layered_pack)
        .unwrap_or_else(|| panic!("context JSON should include prompt text: {layered_pack}"));
    assert!(
        prompt.contains("destination=local") && prompt.contains("destination=session"),
        "prompt should label non-repo memory provenance: {prompt:?}"
    );
    let local_citation = citation_for_record(&layered_pack, &local_id)
        .unwrap_or_else(|| panic!("layered context should cite local memory: {layered_pack}"));
    assert_json_string_field(local_citation, &["destination"], "local");
    assert_json_string_field(local_citation, &["visibility"], "private");
    assert_json_string_field(local_citation, &["source_kind"], "memzoi-local");
}

#[test]
fn context_json_excludes_runtime_memory_without_leaking_content_or_counts() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Runtime zircon repo decision",
        "Repo runtime zircon memory may appear in global context.",
    );
    run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "fact",
            "--title",
            "Runtime zircon local private title",
            "--body",
            "Runtime zircon local private body must not leak.",
            "--json",
        ],
    );
    run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Runtime zircon session private title",
            "--note",
            "Runtime zircon session private body must not leak.",
            "--operation-id",
            "runtime-zircon-checkpoint",
            "--json",
        ],
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "runtime zircon",
            "--token-budget",
            "120",
            "--json",
        ],
    );
    assert_eq!(
        record_ids_from_json(&pack),
        vec![repo_record.as_str()],
        "global context records should remain repo-only: {pack}"
    );
    let rendered = serde_json::to_string(&pack).expect("serialize context JSON");
    assert!(
        !rendered.contains("local private")
            && !rendered.contains("session private")
            && !rendered.contains("must not leak"),
        "context JSON should not leak local/session titles or bodies: {pack}"
    );

    let warnings = pack["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("context JSON should include warnings: {pack}"));
    assert!(
        warnings.is_empty(),
        "context JSON must not count or expose local/session memory unless explicitly opted in: {pack}"
    );
}

#[test]
fn handoff_json_wraps_context_and_reports_proposal_inbox() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Handoff delta repo decision",
        "Handoff delta context should be wrapped under the handoff JSON context field.",
    );
    let proposal = run_json_command(
        repo,
        &[
            "propose",
            "--type",
            "fact",
            "--title",
            "Handoff delta pending proposal",
            "--body",
            "Handoff delta proposal inbox count should come from the DB inbox.",
            "--manual",
            "--json",
        ],
    );
    assert_eq!(proposal_status(&proposal), Some("pending"));

    let handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--task",
            "handoff delta context",
            "--token-budget",
            "100",
            "--json",
        ],
    );

    assert_eq!(json_string(&handoff, "task"), "handoff delta context");
    assert_eq!(handoff["proposal_inbox"]["source"], "db");
    assert_eq!(handoff["proposal_inbox"]["open_total"].as_u64(), Some(1));
    assert_eq!(handoff["proposal_inbox"]["pending"].as_u64(), Some(1));
    assert_eq!(
        record_ids_from_json(&handoff["context"]),
        vec![record.as_str()],
        "handoff should wrap selected context records under context: {handoff}"
    );
    assert!(
        handoff["context"]["included"].as_array().is_some(),
        "handoff context should expose included metadata: {handoff}"
    );
    assert!(
        handoff["context"]["omitted"].as_array().is_some(),
        "handoff context should expose omitted metadata: {handoff}"
    );
    assert_eq!(
        handoff["context"]["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );
}

#[test]
fn handoff_path_only_uses_stable_task_fallback() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let matching = create_applied_memory(
        repo,
        "warning",
        "repo",
        "Handoff path-only warning",
        "Path-only handoff should include this path-scoped memory.",
    );
    attach_memory_path(repo, &matching, "crates/memzoi-core/src/handoff.rs");

    let handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--path",
            "crates/memzoi-core/src/handoff.rs",
            "--token-budget",
            "90",
            "--json",
        ],
    );

    assert_eq!(
        json_string(&handoff, "task"),
        "Handoff for path crates/memzoi-core/src/handoff.rs"
    );
    assert_eq!(
        handoff["context"]["task"].as_str(),
        Some("Handoff for path crates/memzoi-core/src/handoff.rs")
    );
    assert_eq!(
        record_ids_from_json(&handoff["context"]),
        vec![matching.as_str()],
        "path-only handoff should include path-scoped records: {handoff}"
    );
}

#[test]
fn handoff_runtime_memory_requires_explicit_opt_in() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Layered handoff repo decision",
        "Layered handoff should include repo memory by default.",
    );
    let local = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Layered handoff local preference",
            "--body",
            "Layered handoff should include local memory only with explicit opt-in.",
            "--json",
        ],
    );
    let local_id = json_string(&local, "record_id").to_owned();
    let checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Layered handoff session checkpoint",
            "--note",
            "Layered handoff should include session memory only with explicit opt-in.",
            "--operation-id",
            "layered-handoff-checkpoint",
            "--json",
        ],
    );
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();

    let default_handoff =
        run_json_command(repo, &["handoff", "--task", "layered handoff", "--json"]);
    assert_eq!(
        record_ids_from_json(&default_handoff["context"]),
        vec![repo_record.as_str()],
        "handoff should be repo-only by default: {default_handoff}"
    );
    assert_json_does_not_reference_records(
        &default_handoff,
        &[local_id.clone(), checkpoint_id.clone()],
    );

    let layered_handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--task",
            "layered handoff",
            "--include-local",
            "--include-session",
            "--json",
        ],
    );
    let layered_ids = record_ids_from_json(&layered_handoff["context"]);
    assert!(layered_ids.contains(&repo_record.as_str()));
    assert!(layered_ids.contains(&local_id.as_str()));
    assert!(layered_ids.contains(&checkpoint_id.as_str()));
    assert_eq!(
        layered_handoff["context"]["policy"]["requested_destinations"],
        serde_json::json!(["repo", "local", "session"])
    );
}

#[test]
fn handoff_text_labels_proposal_inbox_and_stays_repo_only_by_default() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Text handoff repo procedure",
        "Text handoff should render this repo memory.",
    );
    run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "fact",
            "--title",
            "Text handoff local private title",
            "--body",
            "Text handoff local private body must not leak.",
            "--json",
        ],
    );

    let stdout = run_command_stdout(repo, &["handoff", "--task", "text handoff"]);
    assert!(stdout.contains("# Memzoi Handoff"), "{stdout}");
    assert!(
        stdout.contains("Proposal inbox: 0 open DB proposals"),
        "{stdout}"
    );
    assert!(stdout.contains("Text handoff repo procedure"), "{stdout}");
    assert!(
        !stdout.contains("Text handoff local private"),
        "default text handoff should not leak local memory: {stdout}"
    );
}

#[test]
fn handoff_requires_task_or_path_at_cli_boundary() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let stderr = run_command_failure_stderr(repo, &["handoff"]);
    assert!(
        stderr.contains("handoff requires --task or --path"),
        "handoff should explain missing required task/path input: {stderr}"
    );
}

#[test]
fn precheck_json_warns_for_path_only_governance_and_cites_memory() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let risk = create_applied_memory(
        repo,
        "risk",
        "repo",
        "Preserve settlement invariants",
        "Changing the rounding order previously broke tax calculation.",
    );
    attach_memory_path(repo, &risk, "apps/api/src/billing/invoice.rs");
    update_record_source_ref(repo, &risk, "issue://billing-risk#invoice");

    let unrelated = create_applied_memory(
        repo,
        "warning",
        "repo",
        "Auth command warning",
        "Do not run auth migrations while smoke tests are active.",
    );
    attach_memory_path(repo, &unrelated, "apps/api/src/auth/mod.rs");

    let precheck = run_json_command(
        repo,
        &[
            "precheck",
            "--path",
            "apps/api/src/billing/invoice.rs",
            "--json",
        ],
    );

    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert_eq!(
        warnings.len(),
        1,
        "precheck should only warn for matching risky path: {precheck}"
    );
    let warning = &warnings[0];
    assert_json_string_field(warning, &["record_id"], &risk);
    assert_json_string_field(warning, &["severity"], "high");
    assert!(
        warning
            .to_string()
            .contains("Preserve settlement invariants")
            || warning.to_string().contains("rounding order"),
        "warning should explain the matching memory: {warning}"
    );
    assert_json_does_not_reference_records(&precheck, &[unrelated]);
}

#[test]
fn precheck_json_warns_for_risky_command_without_path() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let warning = create_applied_memory(
        repo,
        "warning",
        "repo",
        "npm install warning",
        "Running npm install mutates lockfiles; use the package manager already configured by the repo.",
    );
    attach_memory_path(repo, &warning, "package.json");

    let precheck = run_json_command(repo, &["precheck", "--command", "npm install", "--json"]);

    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert_eq!(
        warnings.len(),
        1,
        "command-only precheck should warn: {precheck}"
    );
    assert_json_string_field(&warnings[0], &["record_id"], &warning);
    assert_json_string_field(&warnings[0], &["severity"], "warning");
}

#[test]
fn rebuild_json_restores_search_context_and_precheck_from_canonical_records() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let records_dir = test_paths(repo).records_dir().join("core");
    fs::create_dir_all(&records_dir).expect("create canonical records directory");
    fs::write(
        records_dir.join("canonical-rebuild-decision.md"),
        r#"---
id: core/canonical-rebuild-decision
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: test-record:canonical-rebuild-decision
  route: repository_materialization
type: decision
lane: semantic
title: Rebuild sentinel routing decision
description: Restores context packs from canonical records.
timestamp: 2026-07-05T00:00:00Z
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 0.93
source: test
source_ref: test://rebuild-decision
applies_to:
  - crates/memzoi-core/src/context.rs
---

# Rebuild sentinel routing decision

Use the rebuild sentinel routing recall token when restoring context packs from canonical records.
"#,
    )
    .expect("write canonical decision record");
    fs::write(
        records_dir.join("canonical-rebuild-risk.md"),
        r#"---
id: core/canonical-rebuild-risk
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: test-record:canonical-rebuild-risk
  route: repository_materialization
type: risk
lane: semantic
title: Rebuild sentinel precheck risk
description: Restores precheck warnings from canonical records.
timestamp: 2026-07-05T00:00:00Z
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 0.97
source: test
source_ref: test://rebuild-risk
applies_to:
  - crates/memzoi-core/src/precheck.rs
---

# Rebuild sentinel precheck risk

Changing rebuild sentinel precheck command handling previously hid destructive command warnings.
"#,
    )
    .expect("write canonical risk record");

    let rebuild = run_json_command(repo, &["rebuild", "--json"]);
    assert_json_array_contains(&rebuild, "record_ids", "core/canonical-rebuild-decision");
    assert_json_array_contains(&rebuild, "record_ids", "core/canonical-rebuild-risk");

    let search = run_json_command(
        repo,
        &[
            "search",
            "rebuild sentinel routing",
            "--scope-kind",
            "repo",
            "--type",
            "decision",
            "--path",
            "crates/memzoi-core/src",
            "--json",
        ],
    );
    assert_eq!(
        record_ids_from_json(&search),
        vec!["core/canonical-rebuild-decision"],
        "rebuilt DB should make canonical decision searchable: {search}"
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "Need rebuild sentinel routing for context packs",
            "--path",
            "crates/memzoi-core/src/context.rs",
            "--json",
        ],
    );
    let context_ids = record_ids_from_json(&pack);
    assert!(
        context_ids.contains(&"core/canonical-rebuild-decision"),
        "rebuilt DB should make canonical decision available to context packs: {pack}"
    );

    let precheck = run_json_command(
        repo,
        &[
            "precheck",
            "--path",
            "crates/memzoi-core/src/precheck.rs",
            "--action",
            "change rebuild sentinel precheck command handling",
            "--json",
        ],
    );
    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert!(
        warnings.iter().any(|warning| {
            warning.get("record_id").and_then(Value::as_str) == Some("core/canonical-rebuild-risk")
        }),
        "rebuilt DB should make canonical risk available to precheck: {precheck}"
    );
}

#[test]
fn rebuild_preserves_open_shared_proposals_with_ids_statuses_and_next_steps() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let pending = run_json_command(
        repo,
        &[
            "propose",
            "--manual",
            "--type",
            "decision",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--sensitivity",
            "repo-safe",
            "--content-class",
            "general_repo_knowledge",
            "--title",
            "Pending rebuild protection",
            "--body",
            "Rebuild should not silently discard pending proposals.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let pending_id = json_string(&pending, "proposal_id").to_owned();

    let approved = run_json_command(
        repo,
        &[
            "propose",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--sensitivity",
            "repo-safe",
            "--content-class",
            "general_repo_knowledge",
            "--title",
            "Approved rebuild protection",
            "--body",
            "Rebuild should not silently discard approved proposals waiting to apply.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let approved_id = json_string(&approved, "proposal_id").to_owned();

    run_json_command(repo, &["rebuild", "--json"]);

    let open = run_json_command(repo, &["proposals", "list", "--status", "open", "--json"]);
    let proposals = proposals_from_json(&open);
    assert!(
        proposals.iter().any(|proposal| {
            proposal_id_from_value(proposal) == pending_id
                && proposal_status(proposal) == Some("pending")
        }),
        "rebuild should preserve the pending shared proposal: {open}"
    );
    assert!(
        proposals.iter().any(|proposal| {
            proposal_id_from_value(proposal) == approved_id
                && proposal_status(proposal) == Some("approved")
        }),
        "rebuild should preserve the approved shared proposal: {open}"
    );

    let applied = run_json_command(
        repo,
        &[
            "proposals",
            "apply",
            "--all-approved",
            "--actor",
            "agent:rebuild-test",
            "--json",
        ],
    );
    assert!(
        applied_proposals_from_json(&applied)
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == approved_id),
        "the preserved approved proposal should remain actionable: {applied}"
    );
    let pending_after_apply = run_json_command(
        repo,
        &["proposals", "list", "--status", "pending", "--json"],
    );
    assert!(
        proposals_from_json(&pending_after_apply)
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == pending_id),
        "applying approved proposals should leave the preserved pending proposal open: {pending_after_apply}"
    );
}
