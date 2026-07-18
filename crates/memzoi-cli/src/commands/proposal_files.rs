use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    FileProposalInventoryEntry, FileProposalInventoryError, FileProposalResolutionResult,
    OkfProposalOutcome, OkfProposalSensitivity, discover_paths, okf_proposal_matches_identity,
    scan_file_proposal_inventory,
};
use serde_json::json;

use super::{blocked_repo_sensitivity_error, open_service};
use crate::output::print_json;

#[derive(Debug)]
struct ProposalFileScan {
    proposals_root: PathBuf,
    proposals: Vec<ProposalFileEntry>,
    errors: Vec<ProposalFileError>,
}

type ProposalFileEntry = FileProposalInventoryEntry;
type ProposalFileError = FileProposalInventoryError;

pub(super) fn list(as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    if as_json {
        print_json(&proposal_file_scan_json(&scan))?;
    } else {
        for entry in &scan.proposals {
            print_proposal_file_summary(entry);
        }
    }

    if !scan.errors.is_empty() {
        bail!("invalid proposal files found; run `memzoi proposal-files validate` for details");
    }
    Ok(())
}

pub(super) fn show(proposal_id: &str, as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    let matches = scan
        .proposals
        .iter()
        .filter(|entry| okf_proposal_matches_identity(&entry.proposal, proposal_id))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] if !scan.errors.is_empty() => bail!(
            "proposal file not found or invalid: {proposal_id}; run `memzoi proposal-files validate` for details"
        ),
        [] => {
            let resolved = scan_resolved_proposal_files()?;
            let entry = require_proposal_file_entry(&resolved, proposal_id)?;
            if as_json {
                print_json(&proposal_file_json(entry, true))
            } else {
                print_proposal_file_detail(entry);
                Ok(())
            }
        }
        [entry] => {
            if as_json {
                print_json(&proposal_file_json(entry, true))
            } else {
                print_proposal_file_detail(entry);
                Ok(())
            }
        }
        _ => bail!("proposal file id {proposal_id:?} matched multiple files"),
    }
}

pub(super) fn validate(as_json: bool) -> Result<()> {
    let mut scan = scan_pending_proposal_files()?;
    validate_pending_proposal_scan(&mut scan)?;
    if as_json {
        print_json(&proposal_file_validation_json(&scan))?;
    } else {
        for entry in &scan.proposals {
            println!(
                "valid\t{}\t{}",
                entry.display_path.display(),
                entry.proposal.id
            );
        }
        for error in &scan.errors {
            println!("invalid\t{}\t{}", error.display_path.display(), error.error);
        }
    }

    if !scan.errors.is_empty() {
        bail!(
            "{} invalid proposal file{}",
            scan.errors.len(),
            if scan.errors.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn validate_pending_proposal_scan(scan: &mut ProposalFileScan) -> Result<()> {
    let service = open_service()?;
    let inventory = service.validate_file_proposal_inventory()?;
    scan.proposals = inventory.pending;
    scan.errors = inventory.errors;
    Ok(())
}

pub(super) fn apply(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    if !scan.errors.is_empty() {
        if as_json {
            print_json(&proposal_file_validation_json(&scan))?;
        }
        bail!(
            "invalid proposal files found: {}; run `memzoi proposal-files validate` for details",
            scan.errors
                .first()
                .map(|error| error.error.as_str())
                .unwrap_or("unknown proposal inventory error")
        );
    }

    let entry = optional_proposal_file_entry(&scan, proposal_id)?;
    if let Some(entry) = entry
        && entry.source_sensitivity != OkfProposalSensitivity::RepoSafe
    {
        return blocked_repo_sensitivity_error(
            "proposal_files_apply",
            entry.source_sensitivity,
            as_json,
        );
    }
    let service = open_service()?;
    let result = match entry {
        Some(entry) => service.apply_file_proposal_inventory_entry(entry, actor)?,
        None => service.replay_file_proposal(proposal_id, OkfProposalOutcome::Applied, actor)?,
    };
    print_file_resolution_result(&result, as_json)
}

pub(super) fn reject(proposal_id: &str, reason: &str, actor: &str, as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    if !scan.errors.is_empty() {
        if as_json {
            print_json(&proposal_file_validation_json(&scan))?;
        }
        bail!(
            "invalid proposal files found: {}; run `memzoi proposal-files validate` for details",
            scan.errors
                .first()
                .map(|error| error.error.as_str())
                .unwrap_or("unknown proposal inventory error")
        );
    }
    let service = open_service()?;
    let result = match optional_proposal_file_entry(&scan, proposal_id)? {
        Some(entry) => service.reject_file_proposal_inventory_entry(entry, actor, reason)?,
        None => service.replay_file_proposal(proposal_id, OkfProposalOutcome::Rejected, actor)?,
    };
    print_file_resolution_result(&result, as_json)
}

fn require_proposal_file_entry<'a>(
    scan: &'a ProposalFileScan,
    proposal_id: &str,
) -> Result<&'a ProposalFileEntry> {
    optional_proposal_file_entry(scan, proposal_id)?
        .with_context(|| format!("proposal file not found: {proposal_id}"))
}

fn optional_proposal_file_entry<'a>(
    scan: &'a ProposalFileScan,
    proposal_id: &str,
) -> Result<Option<&'a ProposalFileEntry>> {
    let matches = scan
        .proposals
        .iter()
        .filter(|entry| okf_proposal_matches_identity(&entry.proposal, proposal_id))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Ok(None),
        [entry] => Ok(Some(entry)),
        _ => bail!("proposal file id {proposal_id:?} matched multiple files"),
    }
}

fn scan_pending_proposal_files() -> Result<ProposalFileScan> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    scan_pending_proposal_files_at(&paths)
}

fn scan_pending_proposal_files_at(paths: &memzoi_core::MemoryPaths) -> Result<ProposalFileScan> {
    let proposals_root = paths.proposals_dir().join("pending");
    let inventory = scan_file_proposal_inventory(paths)?;

    Ok(ProposalFileScan {
        proposals_root,
        proposals: inventory.pending,
        errors: inventory.errors,
    })
}

fn scan_resolved_proposal_files() -> Result<ProposalFileScan> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    scan_resolved_proposal_files_at(&paths)
}

fn scan_resolved_proposal_files_at(paths: &memzoi_core::MemoryPaths) -> Result<ProposalFileScan> {
    let proposals_root = paths.proposals_dir().join("resolved");
    let inventory = scan_file_proposal_inventory(paths)?;
    Ok(ProposalFileScan {
        proposals_root,
        proposals: inventory.resolved,
        errors: inventory.errors,
    })
}

fn print_proposal_file_summary(entry: &ProposalFileEntry) {
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
        entry.proposal.id,
        entry.display_path.display(),
        entry.proposal.proposal.action.as_str(),
        entry.proposal.lane.as_str(),
        entry.proposal.memory_type.as_str(),
        entry.source_sensitivity.as_str(),
        entry.proposal.title,
    );
}

fn print_proposal_file_detail(entry: &ProposalFileEntry) {
    println!("id:\t{}", entry.proposal.id);
    println!("file_id:\t{}", entry.proposal.file_id);
    println!("path:\t{}", entry.display_path.display());
    if let Some(kind) = &entry.proposal.kind {
        println!("kind:\t{kind}");
    }
    println!("version:\t{}", entry.proposal.version);
    println!("profile:\t{}", entry.proposal.profile);
    println!("status:\t{}", entry.proposal.status.as_str());
    println!("action:\t{}", entry.proposal.proposal.action.as_str());
    println!("proposed_by:\t{}", entry.proposal.proposal.proposed_by);
    println!("proposed_at:\t{}", entry.proposal.proposal.proposed_at);
    if let Some(reason) = &entry.proposal.proposal.reason {
        println!("reason:\t{reason}");
    }
    if let Some(confidence) = &entry.proposal.proposal.confidence {
        println!("confidence:\t{confidence}");
    }
    if let Some(target) = &entry.proposal.proposal.target {
        println!("target:\t{target}");
    }
    println!("lane:\t{}", entry.proposal.lane.as_str());
    println!("type:\t{}", entry.proposal.memory_type.as_str());
    println!("scope_kind:\t{}", entry.proposal.scope_kind.as_str());
    if let Some(scope_id) = &entry.proposal.scope_id {
        println!("scope_id:\t{scope_id}");
    }
    if !entry.proposal.applies_to.is_empty() {
        println!("applies_to:\t{}", entry.proposal.applies_to.join(", "));
    }
    if !entry.proposal.tags.is_empty() {
        println!("tags:\t{}", entry.proposal.tags.join(", "));
    }
    println!("timestamp:\t{}", entry.proposal.timestamp);
    if let Some(created_by) = &entry.proposal.created_by {
        println!("created_by:\t{created_by}");
    }
    if !entry.proposal.sources.is_empty() {
        println!(
            "sources:\t{}",
            entry
                .proposal
                .sources
                .iter()
                .map(|source| {
                    source
                        .path
                        .as_deref()
                        .or(source.url.as_deref())
                        .or(source.reference.as_deref())
                        .unwrap_or("(empty)")
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !entry.proposal.supersedes.is_empty() {
        println!("supersedes:\t{}", entry.proposal.supersedes.join(", "));
    }
    println!("sensitivity:\t{}", entry.source_sensitivity.as_str());
    println!("content_class:\t{}", entry.source_content_class.as_str());
    println!(
        "retention:\t{}",
        serde_json::to_string(&entry.proposal.retention)
            .expect("typed retention facts must serialize")
    );
    println!(
        "origin:\t{}",
        serde_json::to_string(&entry.proposal.origin)
            .expect("typed origin descriptor must serialize")
    );
    if let Some(lineage) = &entry.proposal.lineage {
        println!(
            "lineage:\t{}",
            serde_json::to_string(lineage).expect("typed record lineage must serialize")
        );
    }
    println!("title:\t{}", entry.proposal.title);
    println!("description:\t{}", entry.proposal.description);
    println!("body:\t{}", entry.proposal.body);
}

fn proposal_file_scan_json(scan: &ProposalFileScan) -> serde_json::Value {
    json!({
        "proposals_root": &scan.proposals_root,
        "proposals": scan.proposals
            .iter()
            .map(|entry| proposal_file_json(entry, false))
            .collect::<Vec<_>>(),
        "errors": proposal_file_errors_json(&scan.errors),
    })
}

fn proposal_file_validation_json(scan: &ProposalFileScan) -> serde_json::Value {
    json!({
        "valid": scan.errors.is_empty(),
        "valid_count": scan.proposals.len(),
        "invalid_count": scan.errors.len(),
        "proposals_root": &scan.proposals_root,
        "proposals": scan.proposals
            .iter()
            .map(|entry| proposal_file_json(entry, false))
            .collect::<Vec<_>>(),
        "errors": proposal_file_errors_json(&scan.errors),
    })
}

fn proposal_file_errors_json(errors: &[ProposalFileError]) -> Vec<serde_json::Value> {
    errors
        .iter()
        .map(|error| {
            json!({
                "path": &error.display_path,
                "error": &error.error,
            })
        })
        .collect()
}

fn proposal_file_json(entry: &ProposalFileEntry, include_body: bool) -> serde_json::Value {
    let proposal = &entry.proposal;
    let mut value = json!({
        "id": &proposal.id,
        "file_id": &proposal.file_id,
        "path": &entry.display_path,
        "kind": &proposal.kind,
        "version": &proposal.version,
        "profile": &proposal.profile,
        "type": proposal.memory_type.as_str(),
        "lane": proposal.lane.as_str(),
        "title": &proposal.title,
        "description": &proposal.description,
        "status": proposal.status.as_str(),
        "action": proposal.proposal.action.as_str(),
        "proposal": {
            "action": proposal.proposal.action.as_str(),
            "proposed_by": &proposal.proposal.proposed_by,
            "proposed_at": &proposal.proposal.proposed_at,
            "reason": &proposal.proposal.reason,
            "confidence": &proposal.proposal.confidence,
            "target": &proposal.proposal.target,
        },
        "scope_kind": proposal.scope_kind.as_str(),
        "scope_id": &proposal.scope_id,
        "applies_to": &proposal.applies_to,
        "tags": &proposal.tags,
        "timestamp": &proposal.timestamp,
        "created_by": &proposal.created_by,
        "sources": proposal.sources.iter().map(|source| {
            json!({
                "path": &source.path,
                "url": &source.url,
                "ref": &source.reference,
            })
        }).collect::<Vec<_>>(),
        "supersedes": &proposal.supersedes,
        "sensitivity": entry.source_sensitivity.as_str(),
        "content_class": entry.source_content_class.as_str(),
        "retention": &proposal.retention,
        "origin": &proposal.origin,
        "lineage": &proposal.lineage,
        "resolution": &proposal.resolution,
    });
    if include_body {
        value["body"] = json!(&proposal.body);
    }
    value
}

fn print_file_resolution_result(
    result: &FileProposalResolutionResult,
    as_json: bool,
) -> Result<()> {
    let paths =
        discover_paths(std::env::current_dir().context("failed to read current directory")?)?;
    let relative = |path: &Path| {
        path.strip_prefix(&paths.project_root)
            .unwrap_or(path)
            .to_path_buf()
    };
    let resolved_path = relative(&result.resolved_path);
    let record_path = result.record_path.as_deref().map(relative);
    let record_id = result
        .record
        .as_ref()
        .map(|record| record.id.as_str())
        .or(result.resolution.record_id.as_deref());
    let record_status = result
        .record
        .as_ref()
        .map(|record| record.status.as_str())
        .or(
            (result.resolution.outcome == OkfProposalOutcome::Applied).then_some({
                match result.proposal.proposal.action {
                    memzoi_core::OkfProposalAction::Create
                    | memzoi_core::OkfProposalAction::Supersede => "active",
                    memzoi_core::OkfProposalAction::Tombstone => "tombstoned",
                }
            }),
        );

    if as_json {
        print_json(&json!({
            "proposal_id": &result.proposal.id,
            "file_id": &result.proposal.file_id,
            "action": result.proposal.proposal.action.as_str(),
            "status": result.resolution.outcome.as_str(),
            "outcome": result.resolution.outcome.as_str(),
            "sensitivity": result.proposal.sensitivity.as_str(),
            "title": &result.proposal.title,
            "record_id": record_id,
            "record_status": record_status,
            "record_path": record_path,
            "target_id": &result.resolution.target_id,
            "resolved_path": resolved_path,
            "resolution": &result.resolution,
            "already_resolved": result.already_resolved,
            "runtime_index_updated": result.runtime_index_updated,
        }))
    } else {
        println!(
            "{}\t{}\t{}",
            result.resolution.outcome.as_str(),
            result.proposal.id,
            resolved_path.display()
        );
        if let Some(record_id) = record_id {
            println!("record\t{record_id}");
        }
        if result.already_resolved {
            println!("idempotent\talready resolved");
        }
        Ok(())
    }
}
