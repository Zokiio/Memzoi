use std::{fs, path::Path, process::Command};

use anyhow::{Context, bail};
use rusqlite::Connection;
use tempfile::TempDir;

use crate::{
    CANONICAL_REVISION_SCHEMA, CanonicalRevision, MATERIALIZATION_METADATA_SCHEMA,
    MaterializationAction, MaterializationMetadata, MemoryDraft, MemoryLane, MemoryPaths,
    MemoryRecord, MemoryType, ProposalStatus, ScopeKind, SearchInput, Visibility,
    canonical_revision_for_okf_record,
};

use super::super::{InitRequest, MemoryService, ProposalApprovalOverride, ProposeOptions};

#[test]
fn rebuild_preserves_shared_proposals_and_repairs_the_index_mirror() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Pending rebuild proposal", "Pending rebuild proposal body"),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let validated = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Validated rebuild proposal",
            "Validated rebuild proposal body",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    service.shared_conn.execute(
        "UPDATE proposal SET status = 'validated' WHERE id = ?1",
        [validated.proposal.id.as_str()],
    )?;
    let approved = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Approved rebuild proposal",
            "Approved rebuild proposal body",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    service.validate_proposal(&approved.proposal.id)?;
    service.approve_proposal(&approved.proposal.id, "reviewer:human")?;

    let paths = service.paths.clone();
    service.conn.execute_batch(
        "ALTER TABLE proposal RENAME TO disposable_proposal_mirror;
         CREATE TABLE proposal (
             id TEXT PRIMARY KEY,
             status TEXT NOT NULL
         );",
    )?;

    let result = service.rebuild()?;
    assert_eq!(result.db_path, paths.index_db_path);

    let reopened = MemoryService::open_paths(paths.clone())?;
    for (proposal_id, status) in [
        (pending.proposal.id.as_str(), ProposalStatus::Pending),
        (validated.proposal.id.as_str(), ProposalStatus::Validated),
        (approved.proposal.id.as_str(), ProposalStatus::Approved),
    ] {
        assert_eq!(reopened.show_proposal(proposal_id)?.status, status);
    }

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_count: i64 =
        index.query_row("SELECT COUNT(*) FROM proposal", [], |row| row.get(0))?;
    assert_eq!(mirrored_proposal_count, 3);
    let disposable_table_remains: bool = index.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'disposable_proposal_mirror'
         )",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !disposable_table_remains,
        "rebuild should replace the corrupted proposal mirror"
    );

    Ok(())
}

#[test]
fn rebuild_refuses_when_the_shared_runtime_cannot_be_read() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft("Pending shared proposal", "Pending shared proposal body"),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let paths = service.paths.clone();
    drop(service);
    fs::write(&paths.shared_db_path, b"not a SQLite database")?;

    let error = MemoryService::rebuild_paths(paths.clone())
        .expect_err("rebuild should fail closed when shared runtime cannot be read");
    let message = format!("{error:#}");
    assert!(
        message.contains("shared database") && message.contains("not a database"),
        "rebuild refusal should identify the unreadable shared database: {message}"
    );

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_remains: bool = index.query_row(
        "SELECT EXISTS(SELECT 1 FROM proposal WHERE id = ?1)",
        [pending.proposal.id.as_str()],
        |row| row.get(0),
    )?;
    assert!(
        mirrored_proposal_remains,
        "failed shared-runtime inspection must not replace the derived index"
    );

    Ok(())
}

#[test]
fn rebuild_rejects_incompatible_shared_schema_before_replacing_the_index() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let pending = service.propose_memory_with_options(
        "agent:red-tests",
        sample_memory_draft(
            "Pending proposal before schema damage",
            "The existing mirror must survive failed shared proposal inspection.",
        ),
        ProposeOptions {
            approval_override: Some(ProposalApprovalOverride::Manual),
            apply: false,
        },
    )?;
    let paths = service.paths.clone();
    drop(service);

    let shared = Connection::open(&paths.shared_db_path)?;
    shared.execute_batch(
        "ALTER TABLE proposal RENAME TO damaged_proposal;
         CREATE TABLE proposal (id TEXT PRIMARY KEY);",
    )?;
    drop(shared);

    let error = MemoryService::rebuild_paths(paths.clone())
        .expect_err("rebuild should inspect the shared schema before replacing the index");
    assert!(
        format!("{error:#}").contains("database does not match the current Memzoi format"),
        "unexpected rebuild error: {error:#}"
    );

    let index = Connection::open(&paths.index_db_path)?;
    let mirrored_proposal_remains: bool = index.query_row(
        "SELECT EXISTS(SELECT 1 FROM proposal WHERE id = ?1)",
        [pending.proposal.id],
        |row| row.get(0),
    )?;
    assert!(
        mirrored_proposal_remains,
        "failed shared proposal inspection must not replace the derived index"
    );
    Ok(())
}

#[test]
fn rebuild_scans_the_same_immutable_snapshot_that_it_imports() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft("Immutable rebuild snapshot", "Safe indexed baseline body."),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    let safe = fs::read_to_string(&record_path)?;
    let prohibited = safe
        .replace(
            "content_class: general_repo_knowledge",
            "content_class: raw_transcript",
        )
        .replace(
            "Safe indexed baseline body.",
            "Lexically harmless snapshotracesentinel payload.",
        );
    fs::write(&record_path, prohibited)?;

    let error = MemoryService::rebuild_paths_with_snapshot_hook(service.paths.clone(), || {
        fs::write(&record_path, &safe)?;
        Ok(())
    })
    .expect_err("the prohibited first snapshot must block after a safe replacement");
    assert!(format!("{error:#}").contains("raw_transcript"));
    assert!(
        service
            .search_memory(SearchInput {
                query: "snapshotracesentinel".to_owned(),
                limit: 10,
                ..SearchInput::default()
            })?
            .is_empty(),
        "the unscanned first snapshot reached runtime search"
    );
    Ok(())
}

#[test]
fn rebuild_admits_materialized_tracked_repository_record() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft(
            "Materialized repository admission",
            "Materialized repository records remain readable.",
        ),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    attach_valid_materialization(&record_path, &service.paths.records_dir())?;
    stage_git_path(&service.paths.project_root, &record_path)?;

    let rebuilt = service.rebuild()?;
    assert_eq!(rebuilt.record_ids, vec![record.id]);
    Ok(())
}

#[test]
fn rebuild_refuses_manual_semantic_edit_after_materialization() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft(
            "Materialized semantic edit",
            "Original materialized semantic content.",
        ),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    attach_valid_materialization(&record_path, &service.paths.records_dir())?;
    MemoryService::rebuild_paths(service.paths.clone())?;

    let manually_edited = fs::read_to_string(&record_path)?.replace(
        "Original materialized semantic content.",
        "Manually edited semantic content.",
    );
    fs::write(&record_path, manually_edited)?;

    let error = service
        .rebuild()
        .expect_err("manual semantic edits must invalidate materialization attestation");
    assert!(
        format!("{error:#}").contains("typed-record"),
        "unexpected materialization admission error: {error:#}"
    );
    Ok(())
}

#[test]
fn repository_record_admission_refuses_path_id_mismatch() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft("Path identity admission", "Path identity must be exact."),
    )?;
    let mut snapshot = crate::okf::read_okf_record_snapshots(service.paths.records_dir())?
        .into_iter()
        .find(|snapshot| snapshot.record.concept_id == record.id)
        .context("applied record snapshot is missing")?;
    snapshot.record.concept_id = "mismatched-record-id".to_owned();

    let error = super::admission::admit_repository_record_snapshot(&service.paths, &snapshot)
        .expect_err("admission must reject a canonical path/id mismatch");
    assert!(
        format!("{error:#}").contains("canonical-path-id"),
        "unexpected path/id admission error: {error:#}"
    );
    Ok(())
}

#[test]
fn rebuild_refuses_ignored_untracked_repository_record() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    apply_test_record(
        &service,
        sample_memory_draft(
            "Ignored record admission",
            "Ignored records are not review-visible.",
        ),
    )?;
    fs::write(
        service.paths.project_root.join(".gitignore"),
        ".memzoi/records/\n",
    )?;

    let error = service
        .rebuild()
        .expect_err("ignored canonical records must not enter repository reads");
    assert!(
        format!("{error:#}").contains("ignored-untracked"),
        "unexpected ignored-record admission error: {error:#}"
    );
    Ok(())
}

#[test]
fn rebuild_admits_unattested_record_without_materialization_attestation() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft(
            "Unattested record admission",
            "Canonical records created outside materialization have no materialization metadata.",
        ),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    let before = fs::read(&record_path)?;

    let rebuilt = service.rebuild()?;
    assert_eq!(rebuilt.record_ids, vec![record.id]);
    assert_eq!(
        fs::read(record_path)?,
        before,
        "admission must not synthesize a materialization attestation"
    );
    Ok(())
}

#[test]
fn rebuild_and_drift_refuse_inadmissible_records_without_replacing_the_index() -> anyhow::Result<()>
{
    let (_temp, service) = initialized_service()?;
    let record = apply_test_record(
        &service,
        sample_memory_draft(
            "Admission failure preserves index",
            "The prior derived index remains available.",
        ),
    )?;
    let record_path = service
        .paths
        .records_dir()
        .join(format!("{}.md", record.id));
    let inadmissible = fs::read_to_string(&record_path)?.replace(
        "content_class: general_repo_knowledge",
        "content_class: raw_transcript",
    );
    fs::write(&record_path, inadmissible)?;

    let rebuild_error = MemoryService::rebuild_paths(service.paths.clone())
        .expect_err("inadmissible records must fail rebuild before index replacement");
    assert!(
        format!("{rebuild_error:#}").contains("raw_transcript"),
        "unexpected rebuild admission error: {rebuild_error:#}"
    );
    let drift_error = service
        .repo_index_drift()
        .expect_err("inadmissible canonical files must be explicit drift errors");
    assert!(
        format!("{drift_error:#}").contains("raw_transcript"),
        "unexpected drift admission error: {drift_error:#}"
    );

    let indexed_body: String = service.conn.query_row(
        "SELECT body FROM memory_record WHERE id = ?1",
        [record.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(indexed_body, "The prior derived index remains available.");
    Ok(())
}

fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    fs::create_dir(&project_root)?;
    initialize_git_repository(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    let service = MemoryService::open_paths(paths)?;
    Ok((temp, service))
}

fn apply_test_record(service: &MemoryService, draft: MemoryDraft) -> anyhow::Result<MemoryRecord> {
    let proposal = service.propose_memory("agent:red-tests", draft)?;
    service.validate_proposal(&proposal.id)?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    service.apply_proposal(&proposal.id, "agent:applier")
}

fn sample_memory_draft(title: &str, body: &str) -> MemoryDraft {
    MemoryDraft {
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        visibility: Visibility::Repo,
        title: title.to_owned(),
        body: body.to_owned(),
        tags: vec!["rust".to_owned(), "tests".to_owned()],
        source_kind: Some("test".to_owned()),
        source_ref: Some("service-proposal-tests".to_owned()),
        sensitivity: crate::OkfProposalSensitivity::RepoSafe,
        content_class: crate::RepositoryContentClass::GeneralRepoKnowledge,
        confidence: 0.82,
    }
}

fn attach_valid_materialization(record_path: &Path, records_root: &Path) -> anyhow::Result<()> {
    let markdown = fs::read_to_string(record_path)?;
    let mut record = crate::okf::parse_okf_record_markdown(records_root, record_path, &markdown)?
        .context("record for materialization fixture was ignored")?;
    record.materialization = Some(crate::RepositoryMaterializationMetadata::Direct(
        MaterializationMetadata {
            schema: MATERIALIZATION_METADATA_SCHEMA.to_owned(),
            action: MaterializationAction::Create,
            plan_id: format!("blake3:{}", "a".repeat(64)),
            candidate_id: format!("blake3:{}", "b".repeat(64)),
            decision_id: format!("blake3:{}", "c".repeat(64)),
            decision_at: "2026-07-16T00:00:00Z".to_owned(),
            safety_contract: "test-repository-safety-contract".to_owned(),
            revision: CanonicalRevision {
                schema: CANONICAL_REVISION_SCHEMA.to_owned(),
                revision_hash: format!("blake3:{}", "0".repeat(64)),
            },
            target: None,
            reason: None,
        },
    ));
    let revision = canonical_revision_for_okf_record(&record)?;
    let crate::RepositoryMaterializationMetadata::Direct(metadata) = record
        .materialization
        .as_mut()
        .context("materialization fixture metadata disappeared")?
    else {
        bail!("materialization fixture must use direct metadata");
    };
    metadata.revision = revision;
    let crate::RepositoryMaterializationMetadata::Direct(metadata) = record
        .materialization
        .as_ref()
        .context("materialization fixture metadata is missing")?
    else {
        bail!("materialization fixture must use direct metadata");
    };
    let (frontmatter, body) = markdown
        .split_once("\n---\n")
        .context("canonical record fixture has no closing frontmatter delimiter")?;
    let materialization = format!(
        "materialization:\n  schema: {}\n  action: create\n  plan_id: {}\n  candidate_id: {}\n  decision_id: {}\n  decision_at: {}\n  safety_contract: {}\n  revision:\n    schema: {}\n    revision_hash: {}\n",
        metadata.schema,
        metadata.plan_id,
        metadata.candidate_id,
        metadata.decision_id,
        metadata.decision_at,
        metadata.safety_contract,
        metadata.revision.schema,
        metadata.revision.revision_hash,
    );
    fs::write(
        record_path,
        format!("{frontmatter}\n{materialization}---\n{body}"),
    )?;
    Ok(())
}

fn initialize_git_repository(path: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["init", "-q"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to initialize Git test repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn stage_git_path(repository: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path
        .strip_prefix(repository)
        .context("Git test path escaped its repository")?;
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["add", "--"])
        .arg(relative)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to stage Git test path: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
