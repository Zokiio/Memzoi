use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use crate::output::print_json;

pub(crate) fn integrate_prompt_command() -> Result<()> {
    println!("{}", memzoi_protocol_prompt());
    Ok(())
}

pub(crate) fn integrate_instructions_command(file: PathBuf, as_json: bool) -> Result<()> {
    let original = fs::read_to_string(&file).unwrap_or_default();
    let updated = upsert_memzoi_block(&original);
    if let Some(parent) = file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&file, updated).with_context(|| format!("failed to write {}", file.display()))?;

    if as_json {
        print_json(&json!({
            "file": file,
            "status": "updated",
        }))
    } else {
        println!("updated {}", file.display());
        Ok(())
    }
}

fn memzoi_protocol_prompt() -> &'static str {
    r#"You are working in a repo that uses Memzoi.

Before non-trivial work:
- Run `memzoi context --task "<task>"`.
- If editing specific files, include `--path <relative/path>`.

Before risky actions:
- Run `memzoi precheck --path <relative/path>`.
- Run `memzoi precheck --command "<command>"` before destructive or broad commands.

When durable repo knowledge is discovered:
- Propose it with `memzoi propose --type <type> --title "<title>" --body "<body>"`.
- Use types like fact, decision, procedure, preference, warning, risk, or failed_attempt.
- Prefer proposals over silent durable mutation.

Do not store secrets, raw chat logs, temporary task progress, or private user facts in repo memory."#
}

fn memzoi_instruction_block() -> String {
    format!(
        "<!-- memzoi:start -->\n## Memzoi\n\n{}\n<!-- memzoi:end -->\n",
        memzoi_protocol_prompt()
    )
}

fn upsert_memzoi_block(original: &str) -> String {
    let start = "<!-- memzoi:start -->";
    let end = "<!-- memzoi:end -->";
    let block = memzoi_instruction_block();

    if let Some(start_index) = original.find(start) {
        if let Some(relative_end) = original[start_index..].find(end) {
            let end_index = start_index + relative_end + end.len();
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
    }

    if original.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{}", original.trim_end(), block)
    }
}
