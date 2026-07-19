use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use memzoi_core::read_okf_record_files;

#[test]
fn maintenance_documentation_examples_reference_checked_in_records() -> anyhow::Result<()> {
    let repository_root = repository_root();
    let canonical_record_ids = read_okf_record_files(repository_root.join(".memzoi/records"))?
        .into_iter()
        .map(|record| record.concept_id)
        .collect::<BTreeSet<_>>();

    let reference_path = repository_root.join("website/docs/reference.md");
    let reference = fs::read_to_string(&reference_path)?;
    let cli_record_ids = maintenance_cli_literal_record_ids(&reference);
    assert!(
        !cli_record_ids.is_empty(),
        "{} should contain a literal maintenance --record-id example",
        reference_path.display()
    );

    let mcp_path = repository_root.join("website/docs/mcp-and-agent-integration.md");
    let mcp = fs::read_to_string(&mcp_path)?;
    let mcp_record_ids = maintenance_mcp_literal_record_ids(&mcp)?;
    assert!(
        !mcp_record_ids.is_empty(),
        "{} should contain a memzoi/maintenance-request record_ids example",
        mcp_path.display()
    );

    for (path, record_id) in cli_record_ids
        .into_iter()
        .map(|record_id| (&reference_path, record_id))
        .chain(
            mcp_record_ids
                .into_iter()
                .map(|record_id| (&mcp_path, record_id)),
        )
    {
        assert!(
            canonical_record_ids.contains(&record_id),
            "{} references maintenance record ID {record_id:?}, which is not checked in under .memzoi/records",
            path.display()
        );
    }

    Ok(())
}

#[test]
fn private_lifecycle_documentation_examples_are_copy_safe() -> anyhow::Result<()> {
    let reference_path = repository_root().join("website/docs/reference.md");
    let reference = fs::read_to_string(&reference_path)?;
    let section = markdown_h2_section(&reference, "Owner-authorized private lifecycle");
    let bash_blocks = fenced_blocks(section, "bash");
    assert!(
        !bash_blocks.is_empty(),
        "{} should contain private lifecycle bash examples",
        reference_path.display()
    );

    let angle_bracket_placeholders = bash_blocks
        .iter()
        .flat_map(|block| block.split_whitespace())
        .filter(|token| {
            token
                .find('<')
                .is_some_and(|open| token[open + 1..].contains('>'))
        })
        .collect::<Vec<_>>();
    assert!(
        angle_bracket_placeholders.is_empty(),
        "{} lifecycle bash examples contain shell-unsafe angle-bracket placeholders: {angle_bracket_placeholders:?}",
        reference_path.display()
    );

    let mut direct_request_templates = 0;
    for block in fenced_blocks(section, "json") {
        if !block.contains("memzoi/private-lifecycle-request") {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(&block)?;
        if request.get("schema").and_then(serde_json::Value::as_str)
            == Some("memzoi/private-lifecycle-request")
            && request
                .pointer("/source/kind")
                .and_then(serde_json::Value::as_str)
                == Some("direct")
        {
            direct_request_templates += 1;
        }
    }
    assert!(
        direct_request_templates > 0,
        "{} should contain a direct private-lifecycle request JSON template",
        reference_path.display()
    );

    let lifecycle_commands = bash_commands(section)
        .into_iter()
        .filter(|command| command.starts_with("memzoi lifecycle "))
        .collect::<Vec<_>>();
    let plan_commands = lifecycle_commands
        .iter()
        .filter(|command| command.starts_with("memzoi lifecycle plan "))
        .collect::<Vec<_>>();
    assert!(
        !plan_commands.is_empty(),
        "{} should contain a lifecycle plan example",
        reference_path.display()
    );
    for command in plan_commands {
        assert!(
            !command_has_option(command, "--evaluated-at"),
            "{} lifecycle plan examples must use the service clock instead of a stale --evaluated-at override: {command}",
            reference_path.display()
        );
    }

    let authorize_commands = lifecycle_commands
        .iter()
        .filter(|command| command.starts_with("memzoi lifecycle authorize "))
        .collect::<Vec<_>>();
    assert!(
        !authorize_commands.is_empty(),
        "{} should contain a lifecycle authorize example",
        reference_path.display()
    );
    for command in authorize_commands {
        assert!(
            !command_has_option(command, "--expires-at"),
            "{} lifecycle authorize examples must use the bounded default instead of a stale --expires-at override: {command}",
            reference_path.display()
        );
        assert!(
            !command_has_option(command, "--plan-file"),
            "{} direct-request lifecycle authorize examples must not bind an unrelated --plan-file: {command}",
            reference_path.display()
        );
    }

    let apply_commands = lifecycle_commands
        .iter()
        .filter(|command| command.starts_with("memzoi lifecycle apply "))
        .collect::<Vec<_>>();
    assert!(
        !apply_commands.is_empty(),
        "{} should contain a lifecycle apply example",
        reference_path.display()
    );
    for command in apply_commands {
        assert!(
            !command_has_option(command, "--plan-file"),
            "{} direct-request lifecycle apply examples must not bind an unrelated --plan-file: {command}",
            reference_path.display()
        );
    }

    Ok(())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn maintenance_cli_literal_record_ids(markdown: &str) -> Vec<String> {
    let mut record_ids = Vec::new();
    for command in bash_commands(markdown) {
        collect_maintenance_cli_record_ids(&command, &mut record_ids);
    }
    record_ids
}

fn collect_maintenance_cli_record_ids(command: &str, record_ids: &mut Vec<String>) {
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    if tokens.get(..3) != Some(&["memzoi", "maintenance", "plan"]) {
        return;
    }

    for (index, token) in tokens.iter().enumerate() {
        let value = if *token == "--record-id" {
            tokens.get(index + 1).copied()
        } else {
            token.strip_prefix("--record-id=")
        };
        let Some(value) = value else {
            continue;
        };
        let value = value.trim_matches(['\'', '"']);
        if !value.starts_with('<') && !value.starts_with('$') {
            record_ids.push(value.to_owned());
        }
    }
}

fn maintenance_mcp_literal_record_ids(markdown: &str) -> anyhow::Result<Vec<String>> {
    let mut record_ids = Vec::new();
    for block in fenced_blocks(markdown, "json") {
        if !block.contains("memzoi/maintenance-request") {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(&block)?;
        if request.get("schema").and_then(serde_json::Value::as_str)
            != Some("memzoi/maintenance-request")
        {
            continue;
        }
        let Some(ids) = request
            .get("record_ids")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for record_id in ids {
            let record_id = record_id
                .as_str()
                .expect("maintenance-request record_ids entries should be strings");
            record_ids.push(record_id.to_owned());
        }
    }
    Ok(record_ids)
}

fn fenced_blocks(markdown: &str, language: &str) -> Vec<String> {
    let opening = format!("```{language}");
    let mut blocks = Vec::new();
    let mut current = None;
    for line in markdown.lines() {
        if current.is_none() && line.trim() == opening {
            current = Some(String::new());
        } else if line.trim() == "```" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
        } else if let Some(block) = current.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    blocks
}

fn bash_commands(markdown: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for block in fenced_blocks(markdown, "bash") {
        let mut command = String::new();
        for line in block.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let continued = line.ends_with('\\');
            let fragment = line.strip_suffix('\\').unwrap_or(line).trim_end();
            if !command.is_empty() {
                command.push(' ');
            }
            command.push_str(fragment);
            if !continued {
                commands.push(std::mem::take(&mut command));
            }
        }
        if !command.is_empty() {
            commands.push(command);
        }
    }
    commands
}

fn markdown_h2_section<'a>(markdown: &'a str, heading: &str) -> &'a str {
    let marker = format!("## {heading}");
    let section = markdown
        .split_once(&marker)
        .unwrap_or_else(|| panic!("documentation is missing {marker:?}"))
        .1;
    section
        .split_once("\n## ")
        .map_or(section, |(section, _)| section)
}

fn command_has_option(command: &str, option: &str) -> bool {
    command
        .split_whitespace()
        .any(|token| token == option || token.starts_with(&format!("{option}=")))
}
