use std::fs;

use memzoi_core::{
    ContextPackInput, InitRequest, MemoryDraft, MemoryLane, MemoryPaths, MemoryService,
    MemoryStatus, MemoryType, OkfProposalSensitivity, OkfRecordFile, PrecheckInput,
    RepositoryContentClass, ScopeKind, SearchInput, Visibility, render_okf_record_markdown,
};
use tempfile::TempDir;

#[test]
fn search_eval_recalls_path_scoped_decision_without_distractors() -> anyhow::Result<()> {
    let fixture = EvalFixture::with_corpus(64, 4)?;

    let results = fixture.service.search_memory(SearchInput {
        query: "routing recall token".to_owned(),
        scope_kind: Some(ScopeKind::Repo),
        memory_type: Some(MemoryType::Decision),
        path_prefix: Some("crates/memzoi-core/src".to_owned()),
        limit: 5,
        include_inactive: false,
        ..SearchInput::default()
    })?;

    assert!(
        results
            .iter()
            .any(|result| result.record.id == "target-routing-decision"),
        "search should recall the target decision before unrelated distractors: {results:?}"
    );
    assert!(
        results
            .iter()
            .all(|result| result.record.status == MemoryStatus::Active),
        "search should suppress inactive distractors by default: {results:?}"
    );
    assert!(
        results.iter().all(|result| result
            .paths
            .iter()
            .any(|path| path.path.starts_with("crates/memzoi-core/src"))),
        "path-scoped search should only return matching path metadata: {results:?}"
    );

    Ok(())
}

#[test]
fn context_eval_prioritizes_path_relevant_records_with_budget() -> anyhow::Result<()> {
    let fixture = EvalFixture::with_corpus(80, 3)?;

    let pack = fixture.service.build_context_pack(ContextPackInput {
        task: "Implement routing recall token for Rust context packs".to_owned(),
        path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
        token_budget: Some(80),
        include_local: false,
        include_session: false,
    })?;

    assert!(
        pack.records
            .first()
            .is_some_and(|result| result.record.id == "target-routing-decision"),
        "context pack should place the path-relevant target first: {pack:?}"
    );
    assert!(
        pack.prompt.split_whitespace().count() <= 120,
        "context prompt should stay close to the requested compact budget: {} words",
        pack.prompt.split_whitespace().count()
    );

    Ok(())
}

#[test]
fn precheck_eval_warns_for_governance_memory_and_ignores_facts() -> anyhow::Result<()> {
    let fixture = EvalFixture::with_corpus(48, 2)?;

    let warnings = fixture.service.precheck(PrecheckInput {
        path: Some("crates/memzoi-core/src/precheck.rs".to_owned()),
        action: Some("change risky precheck command handling".to_owned()),
        command: None,
        scope_kind: Some(ScopeKind::Repo),
    })?;

    assert!(
        warnings.iter().any(
            |warning| warning.record_id == "target-precheck-risk" && warning.severity == "high"
        ),
        "precheck should surface the matching governance risk: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|warning| warning.record_id != "target-precheck-fact"),
        "precheck should not turn matching non-governance facts into warnings: {warnings:?}"
    );

    Ok(())
}

struct EvalFixture {
    _temp: TempDir,
    service: MemoryService,
}

impl EvalFixture {
    fn with_corpus(records: usize, inactive_every: usize) -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            temp.path().canonicalize()?,
            temp.path().join(".memzoi-runtime"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: true })?;

        write_record(
            &paths,
            FixtureRecord {
                id: "target-routing-decision".to_owned(),
                memory_type: MemoryType::Decision,
                status: MemoryStatus::Active,
                title: "Routing recall token decision".to_owned(),
                body: "Use the routing recall token for Rust context packs and prioritize matching source paths.".to_owned(),
                path: "crates/memzoi-core/src/context.rs".to_owned(),
            },
        )?;
        write_record(
            &paths,
            FixtureRecord {
                id: "target-precheck-risk".to_owned(),
                memory_type: MemoryType::Risk,
                status: MemoryStatus::Active,
                title: "Risky precheck command handling".to_owned(),
                body: "Changing risky precheck command handling previously hid destructive command warnings.".to_owned(),
                path: "crates/memzoi-core/src/precheck.rs".to_owned(),
            },
        )?;
        write_record(
            &paths,
            FixtureRecord {
                id: "target-precheck-fact".to_owned(),
                memory_type: MemoryType::Fact,
                status: MemoryStatus::Active,
                title: "Risky precheck command handling fact".to_owned(),
                body: "Changing risky precheck command handling touches the same path but is informational only.".to_owned(),
                path: "crates/memzoi-core/src/precheck.rs".to_owned(),
            },
        )?;

        for index in 0..records {
            let status = if inactive_every != 0 && index % inactive_every == 0 {
                MemoryStatus::Superseded
            } else {
                MemoryStatus::Active
            };
            let memory_type = match index % 4 {
                0 => MemoryType::Fact,
                1 => MemoryType::Decision,
                2 => MemoryType::Procedure,
                _ => MemoryType::Warning,
            };
            let path = if index % 3 == 0 {
                format!("crates/memzoi-core/src/module_{index}.rs")
            } else {
                format!("apps/frontend/src/module_{index}.tsx")
            };
            write_record(
                &paths,
                FixtureRecord {
                    id: format!("distractor-{index}"),
                    memory_type,
                    status,
                    title: format!("Distractor memory {index}"),
                    body: format!(
                        "Distractor body {index} mentions routing recall token and precheck handling for ranking noise."
                    ),
                    path,
                },
            )?;
        }

        MemoryService::rebuild_paths(paths.clone())?;
        let service = MemoryService::open_paths(paths)?;
        Ok(Self {
            _temp: temp,
            service,
        })
    }
}

struct FixtureRecord {
    id: String,
    memory_type: MemoryType,
    status: MemoryStatus,
    title: String,
    body: String,
    path: String,
}

fn write_record(paths: &MemoryPaths, record: FixtureRecord) -> anyhow::Result<()> {
    let FixtureRecord {
        id,
        memory_type,
        status,
        title,
        body,
        path,
    } = record;
    let record = OkfRecordFile {
        concept_id: id.clone(),
        draft: MemoryDraft {
            memory_type,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title,
            body,
            tags: vec!["eval".to_owned()],
            source_kind: Some("eval".to_owned()),
            source_ref: Some(format!("eval://{path}")),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 0.91,
        },
        status,
        applies_to: vec![path],
        created: "2026-07-16T00:00:00Z".to_owned(),
        updated: None,
        supersedes_id: None,
        expires_at: None,
        proposal_id: None,
        capture: None,
        materialization: None,
    };
    fs::write(
        paths.records_dir().join(format!("{id}.md")),
        render_okf_record_markdown(&record)?,
    )?;
    Ok(())
}
