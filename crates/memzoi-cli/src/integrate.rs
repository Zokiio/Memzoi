use std::{fs, io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
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

    let agents = PathBuf::from("AGENTS.md");
    let claude = PathBuf::from("CLAUDE.md");

    match profile {
        IntegrateProfile::Codex => Ok(InstructionFileSelection {
            file: agents,
            reason: "default_profile_file",
        }),
        IntegrateProfile::Claude => {
            if agents.exists() && file_contains_memzoi_block(&agents)? {
                Ok(InstructionFileSelection {
                    file: agents,
                    reason: "existing_memzoi_block",
                })
            } else if claude.exists() {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "default_profile_file",
                })
            } else {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "default_profile_file",
                })
            }
        }
        IntegrateProfile::Mcp => {
            if agents.exists() {
                Ok(InstructionFileSelection {
                    file: agents,
                    reason: "existing_instruction_file",
                })
            } else if claude.exists() {
                Ok(InstructionFileSelection {
                    file: claude,
                    reason: "existing_instruction_file",
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

fn file_contains_memzoi_block(file: &PathBuf) -> Result<bool> {
    let contents =
        fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
    Ok(contents.contains(MEMZOI_START) && contents.contains(MEMZOI_END))
}

fn profile_json(profile: IntegrateProfile) -> serde_json::Value {
    json!({
        "profile": profile.as_str(),
        "label": profile_label(profile),
        "kind": profile_kind(profile),
        "default_file": profile_default_file(profile),
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

fn profile_default_file(profile: IntegrateProfile) -> &'static str {
    match profile {
        IntegrateProfile::Codex => "AGENTS.md",
        IntegrateProfile::Claude => "CLAUDE.md",
        IntegrateProfile::Mcp => "AGENTS.md",
    }
}

fn memzoi_protocol_prompt(profile: IntegrateProfile) -> &'static str {
    match profile {
        IntegrateProfile::Codex => CODEX_PROMPT,
        IntegrateProfile::Claude => CLAUDE_PROMPT,
        IntegrateProfile::Mcp => MCP_PROMPT,
    }
}

const CODEX_PROMPT: &str = r#"You are working in a repo that uses Memzoi.

Memzoi has two memory planes:
- Git-plane repo memory is reviewed durable project truth. Treat `.memzoi/records/*.md` as the canonical shared source.
- Runtime-plane local/session memory is for private or task continuity. Include it only with `--include-local` or `--include-session`.

Before non-trivial work:
- Run `memzoi context --task "<task>"` before broad repo scans.
- If editing specific files, include `--path <relative/path>`.

When switching agents or harnesses:
- Run `memzoi handoff --task "<task>"`.
- If the handoff is path-specific, include `--path <relative/path>`.
- Add `--include-local` or `--include-session` only when private runtime memory or explicit checkpoints should be included.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Propose it with `memzoi propose --type <type> --title "<title>" --body "<body>"`.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer proposals over silent durable mutation.

Do not commit secrets, raw chat logs, temporary task progress, private personal data, local-only memory, or session-only memory into repo records."#;

const CLAUDE_PROMPT: &str = r#"You are Claude working in a repo that uses Memzoi.

Memzoi has two memory planes:
- Git-plane repo memory is reviewed durable project truth. Treat `.memzoi/records/*.md` as the canonical shared source.
- Runtime-plane local/session memory is for private or task continuity. Include it only with `--include-local` or `--include-session`.

Before non-trivial work:
- Run `memzoi context --task "<task>"` before broad repo scans.
- If editing specific files, include `--path <relative/path>`.

When switching agents or harnesses:
- Run `memzoi handoff --task "<task>"`.
- If the handoff is path-specific, include `--path <relative/path>`.
- Add `--include-local` or `--include-session` only when private runtime memory or explicit checkpoints should be included.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Propose it with `memzoi propose --type <type> --title "<title>" --body "<body>"`.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer proposals over silent durable mutation.

Do not commit secrets, raw chat logs, temporary task progress, private personal data, local-only memory, or session-only memory into repo records."#;

const MCP_PROMPT: &str = r#"Memzoi MCP setup and usage guidance for this repo.

To configure an MCP client, generate a copy-pasteable server config with:
- `memzoi mcp config --project-root .`

Memzoi has two memory planes:
- Git-plane repo memory is reviewed durable project truth. Treat `.memzoi/records/*.md` as the canonical shared source.
- Runtime-plane local/session memory is for private or task continuity. Include it only with explicit local/session options.

MCP clients should use Memzoi tools to:
- Search repo memory before broad repo scans.
- Build context packs for the current task.
- Run precheck tools before risky path, action, or command work.
- Propose durable repo memories for review.

MCP clients must not:
- Apply proposals or write durable repo records directly.
- Commit secrets, raw chat logs, temporary task progress, private personal data, local-only memory, or session-only memory into repo records.
- Treat runtime/local/session memory as reviewed shared project truth."#;

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
