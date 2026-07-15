use std::fs;

use anyhow::Context;
use tempfile::TempDir;
use uuid::Uuid;

use super::super::{InitRequest, MemoryService, proposal_packets::ProposalPacketLifecycle};
use super::journal::*;
use crate::MemoryPaths;

#[test]
fn open_rolls_back_uncommitted_capture_proposal_files_from_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe staged proposal\n";
    let journal = test_capture_apply_journal("mem_capture_uncommitted", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    assert!(
        !fs::read_to_string(capture_apply_journal_path(&paths))?
            .contains("repo-safe staged proposal"),
        "the durable journal must contain metadata and hashes, not proposal bodies"
    );
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    let destination = capture_apply_destination_path(&paths, entry);
    fs::write(&staged, contents)?;
    fs::hard_link(&staged, &destination)?;
    drop(service);

    let reopened = MemoryService::open_paths(paths.clone())?;

    assert!(!destination.exists());
    assert!(!staged.exists());
    assert!(!capture_apply_journal_path(&paths).exists());
    assert!(!capture_apply_commit_marker_exists(
        &reopened.conn,
        &journal
    )?);
    Ok(())
}

#[test]
fn open_rolls_back_committed_capture_stage_without_verifiable_authorization() -> anyhow::Result<()>
{
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe committed proposal\n";
    let journal = test_capture_apply_journal("mem_capture_committed", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    let destination = capture_apply_destination_path(&paths, entry);
    fs::write(&staged, contents)?;
    append_capture_apply_commit_marker(
        &service.conn,
        &journal,
        "agent:recovery-test",
        "2026-07-10T12:00:00Z",
    )?;
    drop(service);

    let reopened = MemoryService::open_paths(paths.clone())?;

    assert!(!destination.exists());
    assert!(!staged.exists());
    assert!(!capture_apply_journal_path(&paths).exists());
    assert!(capture_apply_commit_marker_exists(
        &reopened.conn,
        &journal
    )?);
    Ok(())
}

#[test]
fn recovery_preserves_mismatched_files_and_keeps_the_journal() -> anyhow::Result<()> {
    let (_temp, service) = initialized_service()?;
    let paths = service.paths.clone();
    let contents = b"repo-safe expected proposal\n";
    let journal = test_capture_apply_journal("mem_capture_mismatch", contents);
    ProposalPacketLifecycle::new(&paths, &service.conn).prepare_pending_root()?;
    write_capture_apply_journal(&paths, &journal)?;
    let entry = &journal.entries[0];
    let staged = capture_apply_stage_path(&paths, &journal, entry);
    let destination = capture_apply_destination_path(&paths, entry);
    fs::write(&staged, contents)?;
    fs::write(&destination, b"user replacement\n")?;
    drop(service);

    let error = MemoryService::open_paths(paths.clone())
        .err()
        .context("mismatched recovery file should block startup")?;

    assert!(format!("{error:#}").contains("refusing recovery deletion"));
    assert_eq!(fs::read(&destination)?, b"user replacement\n");
    assert_eq!(fs::read(&staged)?, contents);
    assert!(capture_apply_journal_path(&paths).is_file());
    Ok(())
}

fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
    let temp = TempDir::new()?;
    let project_root = temp.path().join("project");
    fs::create_dir(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    let service = MemoryService::open_paths(paths)?;
    Ok((temp, service))
}

fn test_capture_apply_journal(proposal_id: &str, contents: &[u8]) -> CaptureApplyJournal {
    CaptureApplyJournal {
        schema: CAPTURE_APPLY_JOURNAL_SCHEMA.to_owned(),
        safety_contract_version: crate::REPOSITORY_WRITE_SAFETY_VERSION.to_owned(),
        detector_policy_version: crate::REPOSITORY_WRITE_DETECTOR_POLICY_VERSION.to_owned(),
        route: crate::RepositoryWriteRoute::CaptureApply
            .as_str()
            .to_owned(),
        authorization_digest: "0".repeat(64),
        project_context_digest: "0".repeat(64),
        journal_id: Uuid::now_v7().to_string(),
        plan_id: "capture_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        review_id: "review_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .to_owned(),
        entries: vec![CaptureApplyJournalEntry {
            candidate_id: "candidate_test".to_owned(),
            proposal_id: proposal_id.to_owned(),
            content_bytes: contents.len() as u64,
            content_hash: blake3::hash(contents).to_hex().to_string(),
            projection_digest: "0".repeat(64),
        }],
    }
}
