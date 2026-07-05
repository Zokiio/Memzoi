use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

use crate::output::print_json;

pub(crate) fn mcp_config_command(project_root: PathBuf) -> Result<()> {
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("failed to resolve project root {}", project_root.display()))?;
    print_json(&json!({
        "mcpServers": {
            "memzoi": {
                "command": "memzoi-mcp",
                "args": ["--project-root", project_root],
                "env": {},
            }
        }
    }))
}
