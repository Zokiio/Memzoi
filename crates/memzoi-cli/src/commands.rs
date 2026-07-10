use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    CheckpointInput, ContextPackInput, ExportFormat, ExportInput, FileProposalInventoryEntry,
    FileProposalInventoryError, FileProposalResolutionResult, HandoffInput, ImportApplyResult,
    ImportPlan, InitRequest, LocalMemoryInput, MemoryDestination, MemoryDraft, MemoryLane,
    MemoryRecord, MemoryService, MemoryType, OkfProposalOutcome, OkfProposalSensitivity,
    OkfProposalStatus, PrecheckInput, Proposal, ProposalApprovalOverride, ProposalInboxSummary,
    ProposalStatus, ProposalStatusFilter, ProposeOptions, ScopeKind, SearchInput, SearchResult,
    SessionEndResult, SessionEndWrite, Visibility, discover_paths,
    lifecycle_transaction_artifact_count, okf_proposal_matches_identity, parse_import_document,
    parse_session_end_document, scan_file_proposal_inventory,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use crate::{
    cli::{
        CheckpointCommands, Cli, Commands, DraftCommand, EventCommands, ImportCommands,
        IntegrateCommands, LocalCommands, McpCommands, ProposalCommands, ProposalFileCommands,
    },
    integrate, mcp,
    output::{print_json, print_jsonl_row},
    update,
};

pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { force, json } => init_command(force, json),
        Commands::Propose {
            memory_type,
            scope_kind,
            visibility,
            source_kind,
            source_ref,
            sensitivity,
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
                source_kind,
                source_ref,
                sensitivity,
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
        Commands::Import { command } => match command {
            ImportCommands::Plan {
                from_file,
                actor,
                json,
            } => import_plan_command(from_file, &actor, json),
            ImportCommands::Apply {
                from_file,
                plan_id,
                actor,
                json,
            } => import_apply_command(from_file, &plan_id, &actor, json),
        },
        Commands::ProposalFiles { command } => match command {
            ProposalFileCommands::List { json } => proposal_files_list_command(json),
            ProposalFileCommands::Show { proposal_id, json } => {
                proposal_files_show_command(&proposal_id, json)
            }
            ProposalFileCommands::Validate { json } => proposal_files_validate_command(json),
            ProposalFileCommands::Apply {
                proposal_id,
                actor,
                json,
            } => proposal_files_apply_command(&proposal_id, &actor, json),
            ProposalFileCommands::Reject {
                proposal_id,
                reason,
                actor,
                json,
            } => proposal_files_reject_command(&proposal_id, &reason, &actor, json),
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
        Commands::Checkpoint { command } => match command {
            CheckpointCommands::Add {
                task,
                note,
                from_file,
                actor,
                json,
            } => checkpoint_add_command(task, note, from_file, &actor, json),
            CheckpointCommands::List { json } => checkpoint_list_command(json),
        },
        Commands::Events { command } => match command {
            EventCommands::Export { jsonl } => events_export_command(jsonl),
        },
        Commands::SessionEnd {
            from_file,
            from_checkpoint,
            actor,
            json,
        } => session_end_command(from_file, from_checkpoint, &actor, json),
        Commands::Supersede {
            record_id,
            memory_type,
            scope_kind,
            visibility,
            source_kind,
            source_ref,
            sensitivity,
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
                source_kind,
                source_ref,
                sensitivity,
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
        Commands::Expiry { record_id, json } => expiry_command(&record_id, json),
        Commands::Context {
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        } => context_command(
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        ),
        Commands::Handoff {
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        } => handoff_command(
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        ),
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
            IntegrateCommands::List { json } => integrate::integrate_list_command(json),
            IntegrateCommands::Prompt { profile } => integrate::integrate_prompt_command(profile),
            IntegrateCommands::Instructions {
                profile,
                file,
                json,
            } => integrate::integrate_instructions_command(profile, file, json),
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

fn import_plan_command(from_file: PathBuf, actor: &str, as_json: bool) -> Result<()> {
    let manifest = fs::read_to_string(&from_file).with_context(|| {
        format!(
            "failed to read import manifest from {}",
            from_file.display()
        )
    })?;
    let document = parse_import_document(&manifest)
        .with_context(|| format!("failed to parse import manifest {}", from_file.display()))?;
    let service = open_service()?;
    let plan = service.plan_import(actor, document)?;
    if as_json {
        let output = import_plan_json(
            &plan,
            &from_file,
            service.paths().project_root.as_path(),
            actor,
        )?;
        print_json(&output)
    } else {
        println!("plan\t{}", plan.plan_id);
        print_import_plan_human(&plan);
        Ok(())
    }
}

fn import_apply_command(
    from_file: PathBuf,
    plan_id: &str,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    if plan_id.trim().is_empty() {
        bail!("import apply requires --plan-id");
    }
    let manifest = fs::read_to_string(&from_file).with_context(|| {
        format!(
            "failed to read import manifest from {}",
            from_file.display()
        )
    })?;
    let document = parse_import_document(&manifest)
        .with_context(|| format!("failed to parse import manifest {}", from_file.display()))?;
    let service = open_service()?;
    let result = service.apply_import(actor, document, plan_id)?;
    if as_json {
        let output = import_apply_json(
            &result,
            &from_file,
            service.paths().project_root.as_path(),
            actor,
            plan_id,
        )?;
        print_json(&output)
    } else {
        println!("applied\t{}", result.plan.plan_id);
        print_import_plan_human(&result.plan);
        Ok(())
    }
}

fn import_plan_json(
    plan: &ImportPlan,
    from_file: &Path,
    project_root: &Path,
    actor: &str,
) -> Result<serde_json::Value> {
    let mut output = serde_json::to_value(plan).context("failed to serialize import plan")?;
    if let serde_json::Value::Object(fields) = &mut output {
        fields.insert("mode".to_owned(), json!("plan"));
        fields.insert("actor".to_owned(), json!(actor));
        fields.insert(
            "source_file".to_owned(),
            json!(safe_import_source_file(from_file, project_root)),
        );
    }
    Ok(output)
}

fn import_apply_json(
    result: &ImportApplyResult,
    from_file: &Path,
    project_root: &Path,
    actor: &str,
    expected_plan_id: &str,
) -> Result<serde_json::Value> {
    let mut output =
        serde_json::to_value(&result.plan).context("failed to serialize import apply plan")?;
    if let serde_json::Value::Object(fields) = &mut output {
        fields.insert("mode".to_owned(), json!("apply"));
        fields.insert("actor".to_owned(), json!(actor));
        fields.insert(
            "source_file".to_owned(),
            json!(safe_import_source_file(from_file, project_root)),
        );
        fields.insert("expected_plan_id".to_owned(), json!(expected_plan_id));
        fields.insert("writes".to_owned(), json!(result.writes));
    }
    Ok(output)
}

fn safe_import_source_file(from_file: &Path, project_root: &Path) -> Option<PathBuf> {
    let manifest = from_file.canonicalize().ok()?;
    let root = project_root.canonicalize().ok()?;
    manifest.strip_prefix(root).ok().map(Path::to_path_buf)
}

fn print_import_plan_human(plan: &ImportPlan) {
    println!(
        "summary\t{}",
        serde_json::to_string(&plan.summary).unwrap_or_default()
    );
    for candidate in &plan.candidates {
        println!(
            "candidate\t{}\t{}\t{}\t{}",
            candidate.index,
            candidate.title,
            candidate.classification.destination.as_str(),
            serde_json::to_string(&candidate.action).unwrap_or_default()
        );
        println!("body\t{}", candidate.body);
    }
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
    let draft = draft_from_args(draft_args)?;
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
            "sensitivity": result.proposal.payload.sensitivity.as_str(),
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
    let proposal = service.show_proposal(proposal_id)?;
    if proposal.payload.sensitivity != OkfProposalSensitivity::RepoSafe {
        return blocked_repo_sensitivity_error("apply", proposal.payload.sensitivity, as_json);
    }
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

type ProposalFileEntry = FileProposalInventoryEntry;
type ProposalFileError = FileProposalInventoryError;

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

fn proposal_files_validate_command(as_json: bool) -> Result<()> {
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

fn proposal_files_apply_command(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
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
        && entry.proposal.sensitivity != OkfProposalSensitivity::RepoSafe
    {
        return blocked_repo_sensitivity_error(
            "proposal_files_apply",
            entry.proposal.sensitivity,
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

fn proposal_files_reject_command(
    proposal_id: &str,
    reason: &str,
    actor: &str,
    as_json: bool,
) -> Result<()> {
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
        print_json(&runtime_record_json(&record))
    } else {
        println!("added\t{}\t{}", record.destination.as_str(), record.id);
        Ok(())
    }
}

fn local_list_command(as_json: bool) -> Result<()> {
    let service = open_service()?;
    let records = service.list_local_memory()?;
    if as_json {
        let records = records.iter().map(runtime_record_json).collect::<Vec<_>>();
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

fn checkpoint_add_command(
    task: String,
    note: Option<String>,
    from_file: Option<PathBuf>,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let note = checkpoint_note_from_args(note, from_file)?;
    let service = open_service()?;
    let record = service.create_checkpoint(actor, CheckpointInput { task, note })?;
    if as_json {
        print_json(&runtime_record_json(&record))
    } else {
        println!("checkpoint\t{}\t{}", record.destination.as_str(), record.id);
        Ok(())
    }
}

fn checkpoint_list_command(as_json: bool) -> Result<()> {
    let service = open_service()?;
    let records = service.list_checkpoints()?;
    if as_json {
        let records = records.iter().map(runtime_record_json).collect::<Vec<_>>();
        print_json(&json!({
            "destination": MemoryDestination::Session.as_str(),
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

fn checkpoint_note_from_args(note: Option<String>, from_file: Option<PathBuf>) -> Result<String> {
    match (note, from_file) {
        (Some(_), Some(_)) => bail!("use either --note or --from-file, not both"),
        (Some(note), None) => Ok(note),
        (None, Some(path)) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read checkpoint note from {}", path.display())),
        (None, None) => bail!("checkpoint add requires --note or --from-file"),
    }
}

fn session_end_command(
    from_file: Option<PathBuf>,
    from_checkpoint: Option<String>,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let (document, source) = match (from_file, from_checkpoint) {
        (Some(_), Some(_)) => bail!("use either --from-file or --from-checkpoint, not both"),
        (Some(path), None) => {
            let body = fs::read_to_string(&path).with_context(|| {
                format!("failed to read session-end input from {}", path.display())
            })?;
            (
                parse_session_end_document(&body)?,
                json!({
                    "kind": "file",
                    "path": path,
                }),
            )
        }
        (None, Some(record_id)) => {
            let checkpoint = service.show_checkpoint(&record_id)?;
            (
                parse_session_end_document(&checkpoint.body)?,
                json!({
                    "kind": "checkpoint",
                    "record_id": record_id,
                }),
            )
        }
        (None, None) => bail!("session-end requires --from-file or --from-checkpoint"),
    };

    let result = service.promote_session_end(actor, document)?;
    if as_json {
        let project_root = service.paths().project_root.as_path();
        print_json(&session_end_result_json(&result, source, project_root))
    } else {
        for candidate in &result.candidates {
            match &candidate.write {
                Some(SessionEndWrite::ProposalFile { proposal_id, path }) => {
                    let path = path
                        .strip_prefix(service.paths().project_root.as_path())
                        .unwrap_or(path);
                    println!(
                        "{}\t{}\t{}\t{}",
                        candidate.status.as_str(),
                        candidate.destination.as_str(),
                        proposal_id,
                        path.display()
                    );
                }
                Some(SessionEndWrite::RuntimeRecord {
                    record_id,
                    destination,
                }) => {
                    println!(
                        "{}\t{}\t{}\t{}",
                        candidate.status.as_str(),
                        destination.as_str(),
                        record_id,
                        candidate.title
                    );
                }
                None => {
                    println!(
                        "{}\t{}\t{}",
                        candidate.status.as_str(),
                        candidate.destination.as_str(),
                        candidate.title
                    );
                }
            }
        }
        Ok(())
    }
}

fn runtime_record_json(record: &MemoryRecord) -> serde_json::Value {
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
        "proposal_id": &record.proposal_id,
        "created_at": &record.created_at,
        "updated_at": &record.updated_at,
    })
}

fn local_search_result_json(result: &SearchResult) -> serde_json::Value {
    json!({
        "record": runtime_record_json(&result.record),
        "score": result.score,
        "snippet": &result.snippet,
        "rationale": &result.rationale,
        "paths": &result.paths,
        "citations": &result.citations,
    })
}

fn session_end_result_json(
    result: &SessionEndResult,
    source: serde_json::Value,
    project_root: &Path,
) -> serde_json::Value {
    json!({
        "task": &result.task,
        "source": source,
        "candidates": result.candidates.iter().map(|candidate| {
            let write = candidate.write.as_ref().map(|write| match write {
                SessionEndWrite::ProposalFile { proposal_id, path } => {
                    let path = path.strip_prefix(project_root).unwrap_or(path);
                    json!({
                        "kind": "proposal_file",
                        "proposal_id": proposal_id,
                        "path": path,
                    })
                }
                SessionEndWrite::RuntimeRecord { record_id, destination } => {
                    json!({
                        "kind": "runtime_record",
                        "record_id": record_id,
                        "destination": destination.as_str(),
                    })
                }
            });
            json!({
                "index": candidate.index,
                "destination": candidate.destination.as_str(),
                "type": candidate.memory_type.as_str(),
                "lane": candidate.lane.as_str(),
                "title": &candidate.title,
                "sensitivity": candidate.sensitivity.as_str(),
                "status": candidate.status.as_str(),
                "reason": &candidate.reason,
                "write": write,
            })
        }).collect::<Vec<_>>(),
    })
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
        entry.proposal.sensitivity.as_str(),
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
    if let Some(version) = &entry.proposal.version {
        println!("version:\t{version}");
    }
    if let Some(profile) = &entry.proposal.profile {
        println!("profile:\t{profile}");
    }
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
    println!("sensitivity:\t{}", entry.proposal.sensitivity.as_str());
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
        "sensitivity": proposal.sensitivity.as_str(),
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

fn supersede_command(
    record_id: &str,
    draft_args: DraftCommand,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let draft = draft_from_args(draft_args)?;
    if draft.sensitivity != OkfProposalSensitivity::RepoSafe {
        return blocked_repo_sensitivity_error("supersede", draft.sensitivity, as_json);
    }
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

fn expiry_command(record_id: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let diagnostic = service.inspect_expiry(record_id)?;

    if as_json {
        print_json(&serde_json::to_value(&diagnostic)?)
    } else {
        println!("record_id:\t{}", diagnostic.record.id);
        println!("title:\t{}", diagnostic.record.title);
        println!("status:\t{}", diagnostic.record.status.as_str());
        println!(
            "expires_at:\t{}",
            diagnostic.record.expires_at.as_deref().unwrap_or("none")
        );
        println!("evaluated_at:\t{}", diagnostic.evaluated_at);
        println!("expired:\t{}", diagnostic.expired);
        println!(
            "excluded_from_normal_reads:\t{}",
            diagnostic.excluded_from_normal_reads
        );
        println!("reason:\t{}", diagnostic.reason);
        println!();
        println!("{}", diagnostic.record.body);
        Ok(())
    }
}

fn context_command(
    task: String,
    path: Option<String>,
    token_budget: Option<usize>,
    include_local: bool,
    include_session: bool,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let pack = service.build_context_pack(ContextPackInput {
        task,
        path_prefix: path,
        token_budget,
        include_local,
        include_session,
    })?;

    if as_json {
        print_json(&serde_json::to_value(&pack)?)
    } else {
        println!("{}", pack.prompt);
        Ok(())
    }
}

fn handoff_command(
    task: Option<String>,
    path: Option<String>,
    token_budget: Option<usize>,
    include_local: bool,
    include_session: bool,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let pack = service.build_handoff_pack(HandoffInput {
        task,
        path_prefix: path,
        token_budget,
        include_local,
        include_session,
    })?;

    if as_json {
        print_json(&serde_json::to_value(&pack)?)
    } else {
        println!("# Memzoi Handoff");
        println!();
        println!("Task: {}", pack.task);
        if let Some(path_prefix) = pack.path_prefix.as_deref() {
            println!("Path: {path_prefix}");
        }
        println!(
            "Proposal inbox: {}",
            proposal_inbox_text(&pack.proposal_inbox)
        );
        println!();
        println!("{}", pack.context.prompt);
        Ok(())
    }
}

fn proposal_inbox_text(proposal_inbox: &ProposalInboxSummary) -> String {
    if proposal_inbox.open_total == 0 {
        return "0 open DB proposals".to_owned();
    }
    format!(
        "{} open DB proposals (pending={}, validated={}, approved={})",
        proposal_inbox.open_total,
        proposal_inbox.pending,
        proposal_inbox.validated,
        proposal_inbox.approved
    )
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

fn events_export_command(jsonl: bool) -> Result<()> {
    let service = open_service()?;

    if jsonl {
        service.for_each_event(|event| {
            print_jsonl_row(&event)?;
            Ok(())
        })?;
    } else {
        service.for_each_event(|event| {
            println!(
                "{}\t{}\t{}\t{}",
                event.created_at, event.id, event.event_type, event.actor
            );
            Ok(())
        })?;
    }

    Ok(())
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

    let proposal_inventory = if paths.config_path.is_file() && paths.db_path.is_file() {
        MemoryService::open_paths(paths.clone())
            .and_then(|service| service.validate_file_proposal_inventory())
    } else {
        scan_file_proposal_inventory(&paths)
    };
    match proposal_inventory {
        Ok(inventory) => {
            let invalid = inventory.errors.len();
            if invalid > 0 {
                checks.push(check(
                    "proposal_files",
                    "warning",
                    format!(
                        "{invalid} invalid proposal packet{}: {}",
                        if invalid == 1 { "" } else { "s" },
                        inventory
                            .errors
                            .first()
                            .map(|error| error.error.as_str())
                            .unwrap_or("unknown proposal inventory error")
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi proposal-files validate");
            } else if inventory.pending.is_empty() {
                let applied = inventory
                    .resolved
                    .iter()
                    .filter(|entry| entry.proposal.status == OkfProposalStatus::Applied)
                    .count();
                let rejected = inventory.resolved.len() - applied;
                checks.push(check(
                    "proposal_files",
                    "ok",
                    format!(
                        "no pending file proposals (resolved: applied={applied}, rejected={rejected})"
                    ),
                ));
            } else {
                checks.push(check(
                    "proposal_files",
                    "warning",
                    format!(
                        "{} pending file proposal{}",
                        inventory.pending.len(),
                        if inventory.pending.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi proposal-files list");
            }
        }
        Err(error) => {
            checks.push(check("proposal_files", "warning", error.to_string()));
        }
    }

    match lifecycle_transaction_artifact_count(&paths) {
        Ok(0) => checks.push(check(
            "lifecycle_transactions",
            "ok",
            "no hidden lifecycle transaction artifacts",
        )),
        Ok(count) => {
            checks.push(check(
                "lifecycle_transactions",
                "warning",
                format!(
                    "{} hidden lifecycle transaction artifact{} require inspection under the Memzoi records/proposals roots",
                    count,
                    if count == 1 { "" } else { "s" },
                ),
            ));
            push_next_step(
                &mut next_steps,
                "inspect hidden .memzoi lifecycle transaction artifacts before retrying",
            );
        }
        Err(error) => checks.push(check(
            "lifecycle_transactions",
            "warning",
            error.to_string(),
        )),
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

        match MemoryService::open_paths(paths.clone())
            .and_then(|service| service.repo_index_drift())
        {
            Ok(drift) if drift.is_current() => {
                checks.push(check("repo_index", "ok", "runtime repo index is current"));
            }
            Ok(drift) => {
                checks.push(check(
                    "repo_index",
                    "warning",
                    format!(
                        "runtime repo index is stale (missing={}, stale={}, changed={}, fts_out_of_sync={})",
                        drift.missing_from_index.len(),
                        drift.stale_in_index.len(),
                        drift.changed_in_index.len(),
                        drift.fts_out_of_sync,
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi rebuild");
            }
            Err(error) => checks.push(check("repo_index", "warning", error.to_string())),
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
            sensitivity: OkfProposalSensitivity::RepoSafe,
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

fn draft_from_args(args: DraftCommand) -> Result<MemoryDraft> {
    let DraftCommand {
        memory_type,
        scope_kind,
        visibility,
        source_kind,
        source_ref,
        sensitivity,
        title,
        body,
    } = args;
    Ok(MemoryDraft {
        memory_type: parse_memory_type(&memory_type)?,
        lane: MemoryLane::Semantic,
        scope_kind: parse_scope_kind(&scope_kind)?,
        scope_id: None,
        visibility: parse_visibility(&visibility)?,
        title,
        body,
        tags: Vec::new(),
        source_kind: normalize_optional_metadata(source_kind, "source-kind")?,
        source_ref: normalize_optional_metadata(source_ref, "source-ref")?,
        sensitivity: sensitivity.parse().map_err(anyhow::Error::msg)?,
        confidence: 1.0,
    })
}

fn blocked_repo_sensitivity_error(
    operation: &str,
    sensitivity: OkfProposalSensitivity,
    as_json: bool,
) -> Result<()> {
    let next_step = repo_sensitivity_guidance(sensitivity);
    let message = if operation == "proposal_files_apply" {
        format!(
            "OKF proposal sensitivity {} cannot be applied into repo records; {next_step}",
            sensitivity.as_str()
        )
    } else {
        format!(
            "canonical repo apply requires sensitivity repo-safe; got {}; {next_step}",
            sensitivity.as_str()
        )
    };
    if as_json {
        print_json(&json!({
            "ok": false,
            "error": {
                "code": "repo_sensitivity_required",
                "operation": operation,
                "sensitivity": sensitivity.as_str(),
                "message": message,
                "next_step": next_step,
            }
        }))?;
    }
    bail!(message)
}

fn repo_sensitivity_guidance(sensitivity: OkfProposalSensitivity) -> &'static str {
    match sensitivity {
        OkfProposalSensitivity::RepoSafe => "repo-safe proposals may be applied after review",
        OkfProposalSensitivity::LocalOnly => {
            "local-only proposals belong in the future local/runtime memory plane"
        }
        OkfProposalSensitivity::Sensitive => {
            "classify or sanitize sensitive content before applying it to the repo plane"
        }
        OkfProposalSensitivity::Secret => "secret proposals must not become repo-shared memory",
        OkfProposalSensitivity::RawTranscript => {
            "raw transcripts must not become repo-shared memory"
        }
        OkfProposalSensitivity::PrivatePersonalData => {
            "private personal data must not become repo-shared memory"
        }
        OkfProposalSensitivity::TemporaryState => {
            "temporary task state belongs in local or session memory, not canonical repo memory"
        }
        OkfProposalSensitivity::Unknown => {
            "classify the proposal sensitivity before applying it to repo records"
        }
    }
}

fn normalize_optional_metadata(value: Option<String>, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("--{label} cannot be empty");
    }
    Ok(Some(value.to_owned()))
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
