use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    ContextPackInput, ExportFormat, ExportInput, InitRequest, LocalMemoryInput, MemoryDestination,
    MemoryDraft, MemoryLane, MemoryRecord, MemoryService, MemoryType, OkfProposalFile,
    PrecheckInput, Proposal, ProposalApprovalOverride, ProposalStatus, ProposalStatusFilter,
    ProposeOptions, ScopeKind, SearchInput, SearchResult, Visibility,
    apply_okf_create_proposal_file, discover_paths, parse_okf_proposal_file,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use crate::{
    cli::{
        Cli, Commands, DraftCommand, IntegrateCommands, LocalCommands, McpCommands,
        ProposalCommands, ProposalFileCommands,
    },
    integrate, mcp,
    output::print_json,
    update,
};

pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { force, json } => init_command(force, json),
        Commands::Propose {
            memory_type,
            scope_kind,
            visibility,
            title,
            body,
            actor,
            manual,
            auto_approve,
            apply,
            json,
        } => propose_command(
            DraftCommand {
                memory_type,
                scope_kind,
                visibility,
                title,
                body,
            },
            &actor,
            ProposeFlags {
                manual,
                auto_approve,
                apply,
            },
            json,
        ),
        Commands::Approve {
            proposal_id,
            actor,
            json,
        } => approve_command(&proposal_id, &actor, json),
        Commands::Reject {
            proposal_id,
            reason,
            actor,
            json,
        } => reject_command(&proposal_id, &reason, &actor, json),
        Commands::Apply {
            proposal_id,
            actor,
            json,
        } => apply_command(&proposal_id, &actor, json),
        Commands::Proposals { command } => match command {
            ProposalCommands::List { status, json } => proposals_list_command(&status, json),
            ProposalCommands::Show { proposal_id, json } => {
                proposals_show_command(&proposal_id, json)
            }
            ProposalCommands::Apply {
                all_approved,
                actor,
                json,
            } => proposals_apply_command(all_approved, &actor, json),
        },
        Commands::ProposalFiles { command } => match command {
            ProposalFileCommands::List { json } => proposal_files_list_command(json),
            ProposalFileCommands::Show { proposal_id, json } => {
                proposal_files_show_command(&proposal_id, json)
            }
            ProposalFileCommands::Validate { json } => proposal_files_validate_command(json),
            ProposalFileCommands::Apply { proposal_id, json } => {
                proposal_files_apply_command(&proposal_id, json)
            }
        },
        Commands::Local { command } => match command {
            LocalCommands::Add {
                memory_type,
                title,
                body,
                actor,
                json,
            } => local_add_command(&memory_type, title, body, &actor, json),
            LocalCommands::List { json } => local_list_command(json),
            LocalCommands::Search { query, limit, json } => {
                local_search_command(query, limit, json)
            }
        },
        Commands::Supersede {
            record_id,
            memory_type,
            scope_kind,
            visibility,
            title,
            body,
            actor,
            json,
        } => supersede_command(
            &record_id,
            DraftCommand {
                memory_type,
                scope_kind,
                visibility,
                title,
                body,
            },
            &actor,
            json,
        ),
        Commands::Tombstone {
            record_id,
            reason,
            actor,
            json,
        } => tombstone_command(&record_id, &reason, &actor, json),
        Commands::Search {
            query,
            scope_kind,
            memory_type,
            path,
            limit,
            json,
        } => search_command(query, scope_kind, memory_type, path, limit, json),
        Commands::Context {
            task,
            path,
            token_budget,
            json,
        } => context_command(task, path, token_budget, json),
        Commands::Precheck {
            path,
            action,
            command,
            scope_kind,
            json,
        } => precheck_command(path, action, command, scope_kind, json),
        Commands::Export {
            format,
            scope_kind,
            json,
        } => export_command(&format, &scope_kind, json),
        Commands::Rebuild { json } => rebuild_command(json),
        Commands::Doctor { project_root, json } => doctor_command(project_root, json),
        Commands::Quickstart { apply_sample, json } => quickstart_command(apply_sample, json),
        Commands::Update {
            check,
            reference,
            json,
        } => update::update_command(check, &reference, json),
        Commands::Mcp { command } => match command {
            McpCommands::Config { project_root } => mcp::mcp_config_command(project_root),
        },
        Commands::Integrate { command } => match command {
            IntegrateCommands::Prompt => integrate::integrate_prompt_command(),
            IntegrateCommands::Instructions { file, json } => {
                integrate::integrate_instructions_command(file, json)
            }
        },
    }
}

fn init_command(force: bool, as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let result = MemoryService::initialize(&cwd, InitRequest { force })?;
    let paths = result.paths;

    if as_json {
        print_json(&json!({
            "project_root": paths.project_root,
            "memory_dir": paths.memory_dir,
            "records_dir": paths.records_dir(),
            "runtime_dir": paths.runtime_dir,
            "config_path": paths.config_path,
            "db_path": paths.db_path,
            "exports_dir": paths.exports_dir,
        }))?;
    } else {
        println!("Initialized Memzoi bundle");
        println!("  memory: {}", paths.memory_dir.display());
        println!("  records: {}", paths.records_dir().display());
        println!("  runtime: {}", paths.runtime_dir.display());
        println!("  config: {}", paths.config_path.display());
        println!("  database: {}", paths.db_path.display());
        println!("  exports: {}", paths.exports_dir.display());
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ProposeFlags {
    manual: bool,
    auto_approve: bool,
    apply: bool,
}

fn propose_command(
    draft_args: DraftCommand,
    actor: &str,
    flags: ProposeFlags,
    as_json: bool,
) -> Result<()> {
    if flags.manual && flags.auto_approve {
        bail!("--manual and --auto-approve cannot be used together");
    }
    if flags.manual && flags.apply {
        bail!("--apply is incompatible with --manual");
    }

    let service = open_service()?;
    let draft = draft_from_args(
        &draft_args.memory_type,
        &draft_args.scope_kind,
        &draft_args.visibility,
        draft_args.title,
        draft_args.body,
    )?;
    let approval_override = match (flags.manual, flags.auto_approve || flags.apply) {
        (true, false) => Some(ProposalApprovalOverride::Manual),
        (false, true) => Some(ProposalApprovalOverride::Auto),
        (false, false) => None,
        (true, true) => unreachable!("manual/auto conflict is checked above"),
    };
    let result = service.propose_memory_with_options(
        actor,
        draft,
        ProposeOptions {
            approval_override,
            apply: flags.apply,
        },
    )?;
    if as_json {
        let record_id = result.record.as_ref().map(|record| record.id.as_str());
        let record_status = result.record.as_ref().map(|record| record.status.as_str());
        print_json(&json!({
            "proposal_id": result.proposal.id,
            "status": result.proposal.status.as_str(),
            "record_id": record_id,
            "record_status": record_status,
            "validation": result.validation,
            "applied": result.applied,
        }))
    } else {
        if let Some(record) = result.record {
            println!(
                "applied proposal {} as memory {}",
                result.proposal.id, record.id
            );
        } else if result.proposal.status == ProposalStatus::Approved {
            println!("approved proposal {}", result.proposal.id);
        } else {
            println!(
                "created {} proposal {}",
                result.proposal.status.as_str(),
                result.proposal.id
            );
        }
        if let Some(validation) = result.validation {
            for issue in validation.issues {
                println!("validation\t{}\t{}", issue.code, issue.message);
            }
        }
        Ok(())
    }
}

fn approve_command(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.approve_proposal(proposal_id, actor)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal.id,
            "status": proposal.status.as_str(),
        }))
    } else {
        println!("approved proposal {}", proposal.id);
        Ok(())
    }
}

fn reject_command(proposal_id: &str, reason: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.reject_proposal(proposal_id, actor, reason)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal.id,
            "status": proposal.status.as_str(),
        }))
    } else {
        println!("rejected proposal {}", proposal.id);
        Ok(())
    }
}

fn apply_command(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let record = service.apply_proposal(proposal_id, actor)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal_id,
            "record_id": record.id,
            "record_status": record.status.as_str(),
        }))
    } else {
        println!("applied proposal {proposal_id} as memory {}", record.id);
        Ok(())
    }
}

fn proposals_list_command(status: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let filter: ProposalStatusFilter = status.parse()?;
    let proposals = service.list_proposals(filter)?;
    if as_json {
        let proposals = proposals.iter().map(proposal_json).collect::<Vec<_>>();
        print_json(&json!({
            "status": status,
            "proposals": proposals,
        }))
    } else {
        for proposal in proposals {
            println!(
                "{}\t{}\t{}",
                proposal.status.as_str(),
                proposal.id,
                proposal.payload.title
            );
        }
        Ok(())
    }
}

fn proposals_show_command(proposal_id: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.show_proposal(proposal_id)?;
    if as_json {
        print_json(&proposal_json(&proposal))
    } else {
        println!("id:\t{}", proposal.id);
        println!("status:\t{}", proposal.status.as_str());
        println!("actor:\t{}", proposal.actor);
        println!("created:\t{}", proposal.created_at);
        println!("updated:\t{}", proposal.updated_at);
        println!("title:\t{}", proposal.payload.title);
        println!("body:\t{}", proposal.payload.body);
        if let Some(validation) = proposal.validation {
            println!(
                "validation:\t{}",
                if validation.is_valid {
                    "valid"
                } else {
                    "invalid"
                }
            );
            for issue in validation.issues {
                println!("validation_issue:\t{}\t{}", issue.code, issue.message);
            }
        }
        Ok(())
    }
}

fn proposals_apply_command(all_approved: bool, actor: &str, as_json: bool) -> Result<()> {
    if !all_approved {
        bail!("proposals apply requires --all-approved");
    }

    let service = open_service()?;
    let approved =
        service.list_proposals(ProposalStatusFilter::Status(ProposalStatus::Approved))?;
    let mut applied = Vec::new();
    let mut failed = None;
    for proposal in approved {
        match service.apply_proposal(&proposal.id, actor) {
            Ok(record) => {
                if !as_json {
                    println!("applied proposal {} as memory {}", proposal.id, record.id);
                }
                applied.push(json!({
                    "proposal_id": proposal.id,
                    "record_id": record.id,
                }));
            }
            Err(error) => {
                failed = Some(json!({
                    "proposal_id": proposal.id,
                    "error": error.to_string(),
                }));
                break;
            }
        }
    }

    let remaining_open_count: usize = service.open_proposal_counts()?.values().sum();
    if as_json {
        print_json(&json!({
            "applied": applied,
            "failed": failed,
            "remaining_open_count": remaining_open_count,
        }))?;
    }
    if let Some(failed) = failed {
        bail!(
            "failed to apply proposal {}: {}",
            failed["proposal_id"].as_str().unwrap_or("unknown"),
            failed["error"].as_str().unwrap_or("unknown error")
        );
    }
    Ok(())
}

fn proposal_json(proposal: &Proposal) -> serde_json::Value {
    json!({
        "id": proposal.id,
        "proposal_id": proposal.id,
        "operation": proposal.operation,
        "status": proposal.status.as_str(),
        "actor": proposal.actor,
        "created_at": proposal.created_at,
        "updated_at": proposal.updated_at,
        "payload": proposal.payload,
        "validation": proposal.validation,
    })
}

#[derive(Debug)]
struct ProposalFileScan {
    proposals_root: PathBuf,
    proposals: Vec<ProposalFileEntry>,
    errors: Vec<ProposalFileError>,
}

#[derive(Debug)]
struct ProposalFileEntry {
    path: PathBuf,
    proposal: OkfProposalFile,
}

#[derive(Debug)]
struct ProposalFileError {
    path: PathBuf,
    error: String,
}

fn proposal_files_list_command(as_json: bool) -> Result<()> {
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

fn proposal_files_show_command(proposal_id: &str, as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    let matches = scan
        .proposals
        .iter()
        .filter(|entry| entry.proposal.id == proposal_id || entry.proposal.file_id == proposal_id)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => {
            if scan.errors.is_empty() {
                bail!("proposal file not found: {proposal_id}");
            }
            bail!(
                "proposal file not found or invalid: {proposal_id}; run `memzoi proposal-files validate` for details"
            )
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

fn proposal_files_validate_command(as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    if as_json {
        print_json(&proposal_file_validation_json(&scan))?;
    } else {
        for entry in &scan.proposals {
            println!("valid\t{}\t{}", entry.path.display(), &entry.proposal.id);
        }
        for error in &scan.errors {
            println!("invalid\t{}\t{}", error.path.display(), &error.error);
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

fn proposal_files_apply_command(proposal_id: &str, as_json: bool) -> Result<()> {
    let scan = scan_pending_proposal_files()?;
    if !scan.errors.is_empty() {
        if as_json {
            print_json(&proposal_file_validation_json(&scan))?;
        }
        bail!("invalid proposal files found; run `memzoi proposal-files validate` for details");
    }

    let entry = require_proposal_file_entry(&scan, proposal_id)?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    let result = apply_okf_create_proposal_file(paths.records_dir(), &entry.proposal)?;
    let record_path = result
        .record_path
        .strip_prefix(&paths.project_root)
        .unwrap_or(&result.record_path);

    if as_json {
        print_json(&json!({
            "proposal_id": &entry.proposal.id,
            "file_id": &entry.proposal.file_id,
            "record_id": &result.record.id,
            "record_path": record_path,
            "action": entry.proposal.proposal.action.as_str(),
            "sensitivity": entry.proposal.sensitivity.as_str(),
            "title": &entry.proposal.title,
        }))?;
    } else {
        println!("applied\t{}\t{}", &result.record.id, record_path.display());
    }
    Ok(())
}

fn local_add_command(
    memory_type: &str,
    title: String,
    body: String,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let record = service.create_local_memory(
        actor,
        LocalMemoryInput {
            memory_type: parse_memory_type(memory_type)?,
            lane: MemoryLane::Semantic,
            title,
            body,
        },
    )?;
    if as_json {
        print_json(&local_record_json(&record))
    } else {
        println!("added\t{}\t{}", record.destination.as_str(), record.id);
        Ok(())
    }
}

fn local_list_command(as_json: bool) -> Result<()> {
    let service = open_service()?;
    let records = service.list_local_memory()?;
    if as_json {
        let records = records.iter().map(local_record_json).collect::<Vec<_>>();
        print_json(&json!({
            "destination": MemoryDestination::Local.as_str(),
            "records": records,
        }))
    } else {
        for record in records {
            println!(
                "{}\t{}\t{}\t{}",
                record.destination.as_str(),
                record.id,
                record.memory_type.as_str(),
                record.title
            );
        }
        Ok(())
    }
}

fn local_search_command(query: String, limit: usize, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let results = service.search_local_memory(query.clone(), limit)?;
    if as_json {
        print_json(&json!({
            "query": query,
            "destination": MemoryDestination::Local.as_str(),
            "records": results.iter().map(local_search_result_json).collect::<Vec<_>>(),
        }))
    } else {
        for result in results {
            println!(
                "{}\t{}\t{}\t{}",
                result.record.destination.as_str(),
                result.record.id,
                result.record.memory_type.as_str(),
                result.record.title
            );
        }
        Ok(())
    }
}

fn local_record_json(record: &MemoryRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "record_id": &record.id,
        "type": record.memory_type.as_str(),
        "lane": record.lane.as_str(),
        "destination": record.destination.as_str(),
        "scope_kind": record.scope_kind.as_str(),
        "visibility": record.visibility.as_str(),
        "status": record.status.as_str(),
        "title": &record.title,
        "body": &record.body,
        "source_kind": &record.source_kind,
        "source_ref": &record.source_ref,
        "created_at": &record.created_at,
        "updated_at": &record.updated_at,
    })
}

fn local_search_result_json(result: &SearchResult) -> serde_json::Value {
    json!({
        "record": local_record_json(&result.record),
        "score": result.score,
        "snippet": &result.snippet,
        "rationale": &result.rationale,
        "paths": &result.paths,
        "citations": &result.citations,
    })
}

fn require_proposal_file_entry<'a>(
    scan: &'a ProposalFileScan,
    proposal_id: &str,
) -> Result<&'a ProposalFileEntry> {
    let matches = scan
        .proposals
        .iter()
        .filter(|entry| entry.proposal.id == proposal_id || entry.proposal.file_id == proposal_id)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => bail!("proposal file not found: {proposal_id}"),
        [entry] => Ok(entry),
        _ => bail!("proposal file id {proposal_id:?} matched multiple files"),
    }
}

fn scan_pending_proposal_files() -> Result<ProposalFileScan> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    let proposals_root = paths.proposals_dir().join("pending");
    let mut files = Vec::new();
    if proposals_root.exists() {
        collect_markdown_files(&proposals_root, &mut files)?;
    }

    let mut proposals = Vec::new();
    let mut errors = Vec::new();
    for file in files {
        match parse_okf_proposal_file(&proposals_root, &file) {
            Ok(Some(proposal)) => proposals.push(ProposalFileEntry {
                path: file,
                proposal,
            }),
            Ok(None) => {}
            Err(error) => errors.push(ProposalFileError {
                path: file,
                error: error.to_string(),
            }),
        }
    }
    proposals.sort_by(|left, right| {
        left.proposal
            .id
            .cmp(&right.proposal.id)
            .then_with(|| left.path.cmp(&right.path))
    });
    errors.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ProposalFileScan {
        proposals_root,
        proposals,
        errors,
    })
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".")
}

fn print_proposal_file_summary(entry: &ProposalFileEntry) {
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
        &entry.proposal.id,
        entry.path.display(),
        entry.proposal.proposal.action.as_str(),
        entry.proposal.lane.as_str(),
        entry.proposal.memory_type.as_str(),
        entry.proposal.sensitivity.as_str(),
        &entry.proposal.title,
    );
}

fn print_proposal_file_detail(entry: &ProposalFileEntry) {
    println!("id:\t{}", &entry.proposal.id);
    println!("file_id:\t{}", &entry.proposal.file_id);
    println!("path:\t{}", entry.path.display());
    if let Some(kind) = &entry.proposal.kind {
        println!("kind:\t{kind}");
    }
    if let Some(version) = &entry.proposal.version {
        println!("version:\t{version}");
    }
    if let Some(profile) = &entry.proposal.profile {
        println!("profile:\t{profile}");
    }
    println!("status:\t{}", entry.proposal.status.as_str());
    println!("action:\t{}", entry.proposal.proposal.action.as_str());
    println!("proposed_by:\t{}", &entry.proposal.proposal.proposed_by);
    println!("proposed_at:\t{}", &entry.proposal.proposal.proposed_at);
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
    println!("timestamp:\t{}", &entry.proposal.timestamp);
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
    println!("sensitivity:\t{}", entry.proposal.sensitivity.as_str());
    println!("title:\t{}", &entry.proposal.title);
    println!("description:\t{}", &entry.proposal.description);
    println!("body:\t{}", &entry.proposal.body);
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
                "path": &error.path,
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
        "path": &entry.path,
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
        "sensitivity": proposal.sensitivity.as_str(),
    });
    if include_body {
        value["body"] = json!(&proposal.body);
    }
    value
}

fn supersede_command(
    record_id: &str,
    draft_args: DraftCommand,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let draft = draft_from_args(
        &draft_args.memory_type,
        &draft_args.scope_kind,
        &draft_args.visibility,
        draft_args.title,
        draft_args.body,
    )?;
    let result = service.supersede_record(record_id, actor, draft)?;
    if as_json {
        print_json(&json!({
            "superseded_record_id": result.previous.id,
            "superseded_record_status": result.previous.status.as_str(),
            "record_id": result.replacement.id,
            "record_status": result.replacement.status.as_str(),
        }))
    } else {
        println!(
            "superseded memory {} with {}",
            result.previous.id, result.replacement.id
        );
        Ok(())
    }
}

fn tombstone_command(record_id: &str, reason: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let record = service.tombstone_record(record_id, actor, reason)?;
    if as_json {
        print_json(&json!({
            "record_id": record.id,
            "record_status": record.status.as_str(),
        }))
    } else {
        println!("tombstoned memory {}", record.id);
        Ok(())
    }
}

fn search_command(
    query: String,
    scope_kind: Option<String>,
    memory_type: Option<String>,
    path: Option<String>,
    limit: usize,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let results = service.search_memory(SearchInput {
        query: query.clone(),
        scope_kind: scope_kind.as_deref().map(parse_scope_kind).transpose()?,
        scope_id: None,
        memory_type: memory_type.as_deref().map(parse_memory_type).transpose()?,
        destination: Some(MemoryDestination::Repo),
        path_prefix: path,
        limit,
        include_inactive: false,
    })?;

    if as_json {
        print_json(&json!({
            "query": query,
            "records": results,
        }))
    } else {
        for result in results {
            println!(
                "{}\t{}\t{}\t{}",
                result.record.id,
                result.record.memory_type.as_str(),
                result.record.scope_kind.as_str(),
                result.record.title
            );
        }
        Ok(())
    }
}

fn context_command(
    task: String,
    path: Option<String>,
    token_budget: Option<usize>,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let pack = service.build_context_pack(ContextPackInput {
        task,
        path_prefix: path,
        token_budget,
    })?;

    if as_json {
        print_json(&serde_json::to_value(&pack)?)
    } else {
        println!("{}", pack.prompt);
        Ok(())
    }
}

fn export_command(format: &str, scope_kind: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let format: ExportFormat = format.parse()?;
    let result = service.export(ExportInput {
        format,
        scope_kind: parse_scope_kind(scope_kind)?,
    })?;

    if as_json {
        print_json(&json!({
            "format": result.format.as_str(),
            "written_paths": result.written_paths,
        }))
    } else {
        for path in result.written_paths {
            println!("{}", path.display());
        }
        Ok(())
    }
}

fn rebuild_command(as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let result = MemoryService::rebuild_at(&cwd)?;
    if as_json {
        print_json(&json!({
            "records_root": result.records_root,
            "db_path": result.db_path,
            "record_ids": result.record_ids,
        }))
    } else {
        println!("rebuilt {} records", result.record_ids.len());
        println!("database: {}", result.db_path.display());
        Ok(())
    }
}

fn precheck_command(
    path: Option<String>,
    action: Option<String>,
    command: Option<String>,
    scope_kind: Option<String>,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let warnings = service.precheck(PrecheckInput {
        path: path.clone(),
        action: action.clone(),
        command: command.clone(),
        scope_kind: scope_kind.as_deref().map(parse_scope_kind).transpose()?,
    })?;

    if as_json {
        print_json(&json!({
            "path": path,
            "action": action,
            "command": command,
            "warnings": warnings,
        }))
    } else {
        if warnings.is_empty() {
            println!("No memory warnings.");
        } else {
            for warning in warnings {
                println!(
                    "{}\t{}\t{}\t{}",
                    warning.severity,
                    warning.record_id,
                    warning.message,
                    warning.suggested_next_step
                );
            }
        }
        Ok(())
    }
}

fn doctor_command(project_root: Option<PathBuf>, as_json: bool) -> Result<()> {
    let start = match project_root {
        Some(path) => path,
        None => std::env::current_dir().context("failed to read current directory")?,
    };
    let paths = discover_paths(start)?;
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();

    checks.push(check("binary", "ok", "memzoi is running"));
    checks.push(check(
        "project_root",
        "ok",
        paths.project_root.display().to_string(),
    ));
    if paths.records_dir().is_dir() {
        checks.push(check(
            "records",
            "ok",
            paths.records_dir().display().to_string(),
        ));
    } else {
        checks.push(check(
            "records",
            "warning",
            format!("{} missing", paths.records_dir().display()),
        ));
        push_next_step(&mut next_steps, "memzoi init");
    }

    if paths.config_path.is_file() {
        checks.push(check(
            "config",
            "ok",
            paths.config_path.display().to_string(),
        ));
    } else {
        checks.push(check(
            "config",
            "warning",
            format!("{} missing", paths.config_path.display()),
        ));
        push_next_step(&mut next_steps, "memzoi init");
    }

    let schema_is_ready = if paths.db_path.is_file() {
        checks.push(check("database", "ok", paths.db_path.display().to_string()));
        match schema_ready(&paths.db_path) {
            Ok(true) => {
                checks.push(check("schema", "ok", "memory schema is initialized"));
                true
            }
            Ok(false) => {
                checks.push(check(
                    "schema",
                    "warning",
                    "memory schema is missing tables",
                ));
                false
            }
            Err(error) => {
                checks.push(check("schema", "warning", error.to_string()));
                false
            }
        }
    } else {
        checks.push(check(
            "database",
            "warning",
            format!("{} missing", paths.db_path.display()),
        ));
        checks.push(check("schema", "skip", "database missing; run init first"));
        false
    };

    if paths.db_path.is_file() && schema_is_ready {
        match MemoryService::open_paths(paths.clone())
            .and_then(|service| service.open_proposal_counts())
        {
            Ok(counts) => {
                let total: usize = counts.values().sum();
                if total == 0 {
                    checks.push(check("proposals", "ok", "no open proposals"));
                } else {
                    let parts = counts
                        .iter()
                        .filter(|(_, count)| **count > 0)
                        .map(|(status, count)| format!("{}={count}", status.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    checks.push(check(
                        "proposals",
                        "warning",
                        format!("{total} open proposals ({parts})"),
                    ));
                    push_next_step(&mut next_steps, "memzoi proposals list --status open");
                    push_next_step(&mut next_steps, "memzoi proposals apply --all-approved");
                    push_next_step(
                        &mut next_steps,
                        "memzoi reject <proposal-id> --reason \"...\"",
                    );
                }
            }
            Err(error) => checks.push(check("proposals", "warning", error.to_string())),
        }
    }

    if paths.exports_dir.is_dir() {
        checks.push(check(
            "exports",
            "ok",
            paths.exports_dir.display().to_string(),
        ));
    } else {
        checks.push(check(
            "exports",
            "warning",
            format!("{} missing", paths.exports_dir.display()),
        ));
    }

    match Command::new("memzoi-mcp").arg("--help").output() {
        Ok(output) if output.status.success() => {
            checks.push(check("mcp", "ok", "memzoi-mcp is available"));
        }
        Ok(_) => checks.push(check("mcp", "warning", "memzoi-mcp --help failed")),
        Err(_) => checks.push(check("mcp", "warning", "memzoi-mcp not found on PATH")),
    }

    push_next_step(&mut next_steps, "memzoi mcp config --project-root .");

    let status = if checks
        .iter()
        .any(|check| check["status"].as_str() == Some("warning"))
    {
        "warning"
    } else {
        "ok"
    };

    if as_json {
        print_json(&json!({
            "project_root": paths.project_root,
            "status": status,
            "checks": checks,
            "next_steps": next_steps,
        }))
    } else {
        println!("Memzoi doctor");
        println!("project: {}", paths.project_root.display());
        println!();
        for check in checks {
            println!(
                "{}\t{}\t{}",
                check["status"].as_str().unwrap_or("unknown").to_uppercase(),
                check["name"].as_str().unwrap_or("unknown"),
                check["message"].as_str().unwrap_or("")
            );
        }
        if !next_steps.is_empty() {
            println!();
            println!("Next:");
            for step in next_steps {
                println!("  {step}");
            }
        }
        Ok(())
    }
}

fn quickstart_command(apply_sample: bool, as_json: bool) -> Result<()> {
    if !apply_sample {
        if as_json {
            return print_json(&json!({
                "next_steps": quickstart_steps(),
            }));
        }
        println!("Memzoi quickstart");
        println!();
        for (index, step) in quickstart_steps().iter().enumerate() {
            println!("{}. {step}", index + 1);
        }
        return Ok(());
    }

    let service = open_service()?;
    if !service.paths().config_path.is_file() {
        bail!("memory bundle is not initialized; run memzoi init first");
    }

    let sample_title = "Use Memzoi quickstart sample";
    let sample_body = "This repo has completed the Memzoi quickstart workflow.";
    let mut search = service.search_memory(SearchInput {
        query: "quickstart".to_string(),
        scope_kind: Some(ScopeKind::Repo),
        scope_id: None,
        memory_type: Some(MemoryType::Decision),
        destination: Some(MemoryDestination::Repo),
        path_prefix: None,
        limit: 10,
        include_inactive: false,
    })?;
    let existing_record = search
        .iter()
        .find(|result| {
            result.record.title == sample_title
                && result.record.source_kind.as_deref() == Some("quickstart")
        })
        .map(|result| result.record.id.clone());
    let (proposal_id, record_id, created) = if let Some(record_id) = existing_record {
        (None::<String>, record_id, false)
    } else {
        let draft = MemoryDraft {
            memory_type: MemoryType::Decision,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: sample_title.to_string(),
            body: sample_body.to_string(),
            tags: Vec::new(),
            source_kind: Some("quickstart".to_string()),
            source_ref: None,
            confidence: 1.0,
        };
        let proposal = service.propose_memory("quickstart", draft)?;
        service.approve_proposal(&proposal.id, "quickstart")?;
        let record = service.apply_proposal(&proposal.id, "quickstart")?;
        search = service.search_memory(SearchInput {
            query: "quickstart".to_string(),
            scope_kind: Some(ScopeKind::Repo),
            scope_id: None,
            memory_type: Some(MemoryType::Decision),
            destination: Some(MemoryDestination::Repo),
            path_prefix: None,
            limit: 10,
            include_inactive: false,
        })?;
        (Some(proposal.id), record.id, true)
    };
    let export = service.export(ExportInput {
        format: ExportFormat::AgentsMd,
        scope_kind: ScopeKind::Repo,
    })?;
    let next_steps = vec!["memzoi mcp config --project-root .".to_string()];

    if as_json {
        print_json(&json!({
            "created": created,
            "proposal_id": proposal_id,
            "record_id": record_id,
            "search_count": search.len(),
            "written_paths": export.written_paths,
            "next_steps": next_steps,
        }))
    } else {
        if created {
            println!("created sample memory {record_id}");
        } else {
            println!("sample memory already exists: {record_id}");
        }
        println!("search_count: {}", search.len());
        for path in export.written_paths {
            println!("exported: {}", path.display());
        }
        println!("next: memzoi mcp config --project-root .");
        Ok(())
    }
}

fn schema_ready(db_path: &PathBuf) -> Result<bool> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open database {}", db_path.display()))?;
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'memory_record')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
}

fn check(name: &str, status: &str, message: impl Into<String>) -> serde_json::Value {
    json!({
        "name": name,
        "status": status,
        "message": message.into(),
    })
}

fn push_next_step(next_steps: &mut Vec<String>, step: &str) {
    if !next_steps.iter().any(|existing| existing == step) {
        next_steps.push(step.to_owned());
    }
}

fn quickstart_steps() -> Vec<String> {
    vec![
        "memzoi init".to_string(),
        "memzoi quickstart --apply-sample".to_string(),
        "memzoi search quickstart".to_string(),
        "memzoi context --task \"remember quickstart setup\"".to_string(),
        "memzoi precheck --command \"rm -rf .memzoi\"".to_string(),
        "memzoi mcp config --project-root .".to_string(),
    ]
}

fn open_service() -> Result<MemoryService> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    MemoryService::open(&cwd)
}

fn draft_from_args(
    memory_type: &str,
    scope_kind: &str,
    visibility: &str,
    title: String,
    body: String,
) -> Result<MemoryDraft> {
    Ok(MemoryDraft {
        memory_type: parse_memory_type(memory_type)?,
        lane: MemoryLane::Semantic,
        scope_kind: parse_scope_kind(scope_kind)?,
        scope_id: None,
        visibility: parse_visibility(visibility)?,
        title,
        body,
        tags: Vec::new(),
        source_kind: Some("cli".to_string()),
        source_ref: None,
        confidence: 1.0,
    })
}

fn parse_memory_type(value: &str) -> Result<MemoryType> {
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_scope_kind(value: &str) -> Result<ScopeKind> {
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_visibility(value: &str) -> Result<Visibility> {
    value.parse().map_err(anyhow::Error::msg)
}
