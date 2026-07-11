use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use memzoi_core::discover_paths;

mod protocol;

#[derive(Debug, Parser)]
#[command(name = "memzoi-mcp", version, about = "MCP stdio adapter for memzoi")]
struct Cli {
    /// Project root used for Memzoi project discovery.
    #[arg(long, default_value = ".")]
    project_root: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = discover_paths(&cli.project_root).with_context(|| {
        format!(
            "failed to discover Memzoi project at {}",
            cli.project_root.display()
        )
    })?;
    protocol::serve_stdio(protocol::ProtocolState::new(paths))
}
