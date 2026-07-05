use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    ContextPackInput, ExportFormat, ExportInput, InitRequest, MemoryDraft, MemoryService,
    MemoryType, PrecheckInput, ScopeKind, SearchInput, Visibility, discover_paths,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use crate::{
    cli::{Cli, Commands, DraftCommand, IntegrateCommands, McpCommands},
    integrate, mcp,
    output::print_json,
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
            json,
        } => propose_command(
            &memory_type,
            &scope_kind,
            &visibility,
            title,
            body,
            &actor,
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

fn propose_command(
    memory_type: &str,
    scope_kind: &str,
    visibility: &str,
    title: String,
    body: String,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let draft = draft_from_args(memory_type, scope_kind, visibility, title, body)?;
    let proposal = service.propose_memory(actor, draft)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal.id,
            "status": proposal.status.as_str(),
        }))
    } else {
        println!("proposed memory {}", proposal.id);
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

    if paths.db_path.is_file() {
        checks.push(check("database", "ok", paths.db_path.display().to_string()));
        match schema_ready(&paths.db_path) {
            Ok(true) => checks.push(check("schema", "ok", "memory schema is initialized")),
            Ok(false) => checks.push(check(
                "schema",
                "warning",
                "memory schema is missing tables",
            )),
            Err(error) => checks.push(check("schema", "warning", error.to_string())),
        }
    } else {
        checks.push(check(
            "database",
            "warning",
            format!("{} missing", paths.db_path.display()),
        ));
        checks.push(check("schema", "skip", "database missing; run init first"));
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
