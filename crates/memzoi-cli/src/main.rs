use anyhow::Result;
use clap::Parser;

mod cli;
mod commands;
mod eval;
mod integrate;
mod mcp;
mod output;
mod update;

use cli::Cli;

fn main() -> Result<()> {
    commands::run(Cli::parse())
}
