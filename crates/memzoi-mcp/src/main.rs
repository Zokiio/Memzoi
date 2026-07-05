use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use memzoi_core::MemoryService;

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
    let service = MemoryService::open(&cli.project_root).with_context(|| {
        format!(
            "failed to open memory service at {}",
            cli.project_root.display()
        )
    })?;
    protocol::serve_stdio(service)
}
