use std::{fs, io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use memzoi_core::{
    MemoryDestination, RepoMemoryExclusion, TWO_PLANE_MEMORY_POLICY, discover_paths,
};
use serde_json::json;

use crate::{cli::IntegrateProfile, output::print_json};

const MEMZOI_START: &str = "<!-- memzoi:start -->";
const MEMZOI_END: &str = "<!-- memzoi:end -->";

pub(crate) fn integrate_list_command(as_json: bool) -> Result<()> {
    let profiles = [
        profile_json(IntegrateProfile::Codex),
        profile_json(IntegrateProfile::Claude),
        profile_json(IntegrateProfile::Mcp),
    ];

    if as_json {
        print_json(&json!({ "profiles": profiles }))
    } else {
        for profile in [
            IntegrateProfile::Codex,
            IntegrateProfile::Claude,
            IntegrateProfile::Mcp,
        ] {
            println!(
                "{}\t{}\t{}",
                profile.as_str(),
                profile_label(profile),
                profile_kind(profile)
            );
        }
        Ok(())
    }
}

pub(crate) fn integrate_prompt_command(profile: IntegrateProfile) -> Result<()> {
    println!("{}", memzoi_protocol_prompt(profile));
    Ok(())
}

pub(crate) fn integrate_instructions_command(
    profile: IntegrateProfile,
    file: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let selection = resolve_instruction_file(profile, file)?;
    let (original, existed) = match fs::read_to_string(&selection.file) {
        Ok(original) => (original, true),
        Err(error) if error.kind() == ErrorKind::NotFound => (String::new(), false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", selection.file.display()));
        }
    };
    let updated = upsert_memzoi_block(&original, profile);
    if let Some(parent) = selection
        .file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&selection.file, updated)
        .with_context(|| format!("failed to write {}", selection.file.display()))?;

    let status = if existed { "updated" } else { "created" };
    if as_json {
        print_json(&json!({
            "file": selection.file,
            "profile": profile.as_str(),
            "status": status,
            "marker": "memzoi",
            "reason": selection.reason,
        }))
    } else {
        println!(
            "{} {} for profile {} ({})",
            status,
            selection.file.display(),
            profile.as_str(),
            selection.reason
        );
        Ok(())
    }
}

struct InstructionFileSelection {
    file: PathBuf,
    reason: &'static str,
}

fn resolve_instruction_file(
    profile: IntegrateProfile,
    file: Option<PathBuf>,
) -> Result<InstructionFileSelection> {
    if let Some(file) = file {
        return Ok(InstructionFileSelection {
            file,
            reason: "explicit_file",
        });
    }

    let root = default_instruction_root()?;
    let agents = root.join("AGENTS.md");
    let claude = root.join("CLAUDE.md");

    match profile {
        IntegrateProfile::Codex => Ok(InstructionFileSelection {
            file: agents,
            reason: "default_profile_file",
        }),
        IntegrateProfile::Claude => {
            if agents.exists() && file_contains_memzoi_block(&agents).unwrap_or(false) {
                Ok(InstructionFileSelection {
                    file: agents,
                    reason: "existing_memzoi_block",
                })
            } else {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "default_profile_file",
                })
            }
        }
        IntegrateProfile::Mcp => {
            if agents.exists() && file_is_readable_text(&agents) {
                Ok(InstructionFileSelection {
                    file: agents,
                    reason: "existing_instruction_file",
                })
            } else if claude.exists() && file_is_readable_text(&claude) {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "existing_instruction_file",
                })
            } else if agents.exists() {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "default_profile_file",
                })
            } else {
                Ok(InstructionFileSelection {
                    file: agents,
                    reason: "default_profile_file",
                })
            }
        }
    }
}

fn default_instruction_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    Ok(discover_paths(cwd)?.project_root)
}

fn file_contains_memzoi_block(file: &PathBuf) -> Result<bool> {
    let contents =
        fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
    Ok(contents
        .find(MEMZOI_START)
        .and_then(|start| contents[start..].find(MEMZOI_END))
        .is_some())
}

fn file_is_readable_text(file: &PathBuf) -> bool {
    fs::read_to_string(file).is_ok()
}

fn profile_json(profile: IntegrateProfile) -> serde_json::Value {
    json!({
        "profile": profile.as_str(),
        "label": profile_label(profile),
        "kind": profile_kind(profile),
        "default_files": profile_default_files(profile),
        "selection": profile_selection_policy(profile),
    })
}

fn profile_label(profile: IntegrateProfile) -> &'static str {
    match profile {
        IntegrateProfile::Codex => "Codex agent instructions",
        IntegrateProfile::Claude => "Claude agent instructions",
        IntegrateProfile::Mcp => "MCP setup and usage guidance",
    }
}

fn profile_kind(profile: IntegrateProfile) -> &'static str {
    match profile {
        IntegrateProfile::Codex | IntegrateProfile::Claude => "instruction-file",
        IntegrateProfile::Mcp => "setup-guidance",
    }
}

fn profile_default_files(profile: IntegrateProfile) -> &'static [&'static str] {
    match profile {
        IntegrateProfile::Codex => &["AGENTS.md"],
        IntegrateProfile::Claude => &["AGENTS.md", "CLAUDE.md"],
        IntegrateProfile::Mcp => &["AGENTS.md", "CLAUDE.md"],
    }
}

fn profile_selection_policy(profile: IntegrateProfile) -> &'static str {
    match profile {
        IntegrateProfile::Codex => "writes AGENTS.md unless --file is provided",
        IntegrateProfile::Claude => {
            "writes an existing AGENTS.md Memzoi block, otherwise writes CLAUDE.md"
        }
        IntegrateProfile::Mcp => {
            "writes a readable existing instruction file, preferring AGENTS.md, otherwise creates a profile file"
        }
    }
}

fn memzoi_protocol_prompt(profile: IntegrateProfile) -> String {
    let policy = memzoi_policy_block();
    match profile {
        IntegrateProfile::Codex => {
            format!("{CODEX_PROMPT_PREFIX}\n\n{policy}\n\n{CODEX_PROMPT_SUFFIX}")
        }
        IntegrateProfile::Claude => {
            format!("{CLAUDE_PROMPT_PREFIX}\n\n{policy}\n\n{CLAUDE_PROMPT_SUFFIX}")
        }
        IntegrateProfile::Mcp => format!("{MCP_PROMPT_PREFIX}\n\n{policy}\n\n{MCP_PROMPT_SUFFIX}"),
    }
}

fn memzoi_policy_block() -> String {
    let destinations = MemoryDestination::ALL
        .into_iter()
        .map(|destination| {
            let policy = destination.policy();
            format!(
                "  - `{}`: plane `{}`, route `{}`, review `{}`.",
                destination.as_str(),
                policy.plane.map(|plane| plane.as_str()).unwrap_or("none"),
                policy.write_route.as_str(),
                policy.review.as_str(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let repo_plane = MemoryDestination::Repo
        .policy()
        .plane
        .map(|plane| plane.as_str())
        .unwrap_or("none");
    let runtime_plane = MemoryDestination::Local
        .policy()
        .plane
        .map(|plane| plane.as_str())
        .unwrap_or("none");
    let exclusions = TWO_PLANE_MEMORY_POLICY
        .repo_exclusions
        .iter()
        .map(|exclusion: &RepoMemoryExclusion| exclusion.as_str().replace('_', " "))
        .collect::<Vec<_>>()
        .join(", ");
    let future_destinations = TWO_PLANE_MEMORY_POLICY.future_destinations.join(", ");

    format!(
        "Memzoi's canonical two-plane memory policy:\n\
- Git-plane repo memory (`{repo_plane}`) is reviewed, durable project truth in `{}`.\n\
- Runtime-plane local/session memory (`{runtime_plane}`) is local continuity under `{}` and is not canonical shared repo truth.\n\
- Destination policy (destination → plane, write route, review boundary):\n\
{destinations}\n\
- `memzoi propose` creates reviewable operational state; MCP is read-only and its plans are evidence, not canonical records.\n\
- Canonical repo writes require an explicit CLI apply route: DB proposals use `memzoi apply <proposal-id>` or `memzoi proposals apply --all-approved` after approval, or the one-shot `memzoi propose --apply` route; file-backed proposal packets require review followed by `memzoi proposal-files apply <proposal-id>`. DB proposal state and packet review alone are not canonical.\n\
- Do not commit {exclusions} to canonical repo records.\n\
- Future destinations not enabled by this integration: {future_destinations}.",
        TWO_PLANE_MEMORY_POLICY.canonical_records_glob,
        TWO_PLANE_MEMORY_POLICY.runtime_project_root_template,
    )
}

const CODEX_PROMPT_PREFIX: &str = "You are working in a repo that uses Memzoi.";

const CODEX_PROMPT_SUFFIX: &str = r#"Before non-trivial work:
- Run `memzoi context --task "<task>"` before non-trivial work, especially before broad repo scans.
- If editing specific files, include `--path <relative/path>`.

When switching agents or harnesses:
- Run `memzoi handoff --task "<task>"`.
- If the handoff is path-specific, include `--path <relative/path>`.
- Add `--include-local` or `--include-session` only when private runtime memory or explicit checkpoints should be included.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Search Memzoi memory before broad scans.
- Use `memzoi propose --type <type> --title "<title>" --body "<body>"` to create reviewable operational state.
- Use the policy block's supported route before durable knowledge becomes a canonical record.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer reviewable proposals over silent durable mutation.

The policy block defines the canonical route; DB-local proposal state is not canonical before an explicit CLI apply."#;

const CLAUDE_PROMPT_PREFIX: &str = "You are Claude working in a repo that uses Memzoi.";

const CLAUDE_PROMPT_SUFFIX: &str = r#"Before non-trivial work:
- Run `memzoi context --task "<task>"` before non-trivial work, especially before broad repo scans.
- If editing specific files, include `--path <relative/path>`.

When switching agents or harnesses:
- Run `memzoi handoff --task "<task>"`.
- If the handoff is path-specific, include `--path <relative/path>`.
- Add `--include-local` or `--include-session` only when private runtime memory or explicit checkpoints should be included.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Search Memzoi memory before broad scans.
- Use `memzoi propose --type <type> --title "<title>" --body "<body>"` to create reviewable operational state.
- Use the policy block's supported route before durable knowledge becomes a canonical record.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer reviewable proposals over silent durable mutation.

The policy block defines the canonical route; DB-local proposal state is not canonical before an explicit CLI apply."#;

const MCP_PROMPT_PREFIX: &str = r#"Memzoi MCP setup and usage guidance for this repo.

To configure an MCP client, generate a copy-pasteable server config with:
- `memzoi mcp config --project-root .`"#;

const MCP_PROMPT_SUFFIX: &str = r#"MCP clients should use Memzoi tools to:
- Search repository memory before broad repo scans.
- Build repository-only context packs for the current task.
- Run precheck tools before risky path, action, or command work.
- Build read-only capture and repository-maintenance plans.

MCP clients must not:
- Request or expose private local/session memory.
- Create or change proposal state.
- Apply proposals or write canonical repo records.
- Treat read-only planning artifacts as execution authority or canonical records.
- Commit excluded or runtime-only material into canonical repo records.
- Claim that MCP can apply canonical records; canonical writes require an explicit CLI apply route described in the policy block."#;

fn memzoi_instruction_block(profile: IntegrateProfile) -> String {
    format!(
        "{MEMZOI_START}\n## Memzoi\n\n{}\n{MEMZOI_END}\n",
        memzoi_protocol_prompt(profile)
    )
}

fn upsert_memzoi_block(original: &str, profile: IntegrateProfile) -> String {
    let block = memzoi_instruction_block(profile);

    if let Some(start_index) = original.find(MEMZOI_START)
        && let Some(relative_end) = original[start_index..].find(MEMZOI_END)
    {
        let end_index = start_index + relative_end + MEMZOI_END.len();
        let mut updated = String::new();
        updated.push_str(original[..start_index].trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block.trim_end());
        let tail = original[end_index..].trim_start();
        if !tail.is_empty() {
            updated.push_str("\n\n");
            updated.push_str(tail);
        }
        updated.push('\n');
        return updated;
    }

    if original.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{}", original.trim_end(), block)
    }
}
