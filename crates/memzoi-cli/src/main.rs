use anyhow::Result;
use clap::Parser;

mod cli;
mod commands;
mod integrate;
mod mcp;
mod output;

use cli::Cli;

fn main() -> Result<()> {
    commands::run(Cli::parse())
}
