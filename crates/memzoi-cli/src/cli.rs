use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "memzoi",
    version,
    about = "Local-first memory governance for coding agents"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Initialize repo .memzoi memory and local runtime state.
    Init {
        /// Overwrite an existing config.toml.
        #[arg(long)]
        force: bool,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Propose a new memory record.
    Propose {
        #[arg(long = "type")]
        memory_type: String,
        #[arg(long = "scope-kind", alias = "scope", default_value = "repo")]
        scope_kind: String,
        #[arg(long, default_value = "repo")]
        visibility: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        manual: bool,
        #[arg(long = "auto-approve")]
        auto_approve: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },

    /// Approve a proposed memory.
    Approve {
        proposal_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Reject a proposed memory.
    Reject {
        proposal_id: String,
        #[arg(long, default_value = "rejected by user")]
        reason: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Apply an approved memory proposal.
    Apply {
        proposal_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Inspect and apply proposal inbox state.
    Proposals {
        #[command(subcommand)]
        command: ProposalCommands,
    },

    /// Inspect OKF proposal files under .memzoi/proposals/pending.
    ProposalFiles {
        #[command(subcommand)]
        command: ProposalFileCommands,
    },

    /// Manage local-only runtime memory records.
    Local {
        #[command(subcommand)]
        command: LocalCommands,
    },

    /// Manage runtime session checkpoints.
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCommands,
    },

    /// Promote explicit session-end candidates into proposals or runtime memory.
    SessionEnd {
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long = "from-checkpoint")]
        from_checkpoint: Option<String>,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Supersede an active memory record with new content.
    Supersede {
        record_id: String,
        #[arg(long = "type")]
        memory_type: String,
        #[arg(long = "scope-kind", alias = "scope", default_value = "repo")]
        scope_kind: String,
        #[arg(long, default_value = "repo")]
        visibility: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Tombstone an active memory record.
    Tombstone {
        record_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Search active memory records.
    Search {
        /// Search query.
        query: String,
        #[arg(long = "scope-kind", alias = "scope")]
        scope_kind: Option<String>,
        #[arg(long = "type")]
        memory_type: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Build a prompt-ready context pack for a task.
    Context {
        #[arg(long)]
        task: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        token_budget: Option<usize>,
        #[arg(long)]
        include_local: bool,
        #[arg(long)]
        include_session: bool,
        #[arg(long)]
        json: bool,
    },

    /// Check planned work against risky memories before acting.
    Precheck {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long = "scope-kind", alias = "scope")]
        scope_kind: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Export active repo memory into reviewable files.
    Export {
        /// Export format: okf, agents-md, or claude-md.
        format: String,
        #[arg(long = "scope-kind", alias = "scope", default_value = "repo")]
        scope_kind: String,
        #[arg(long)]
        json: bool,
    },

    /// Rebuild the derived SQLite database from canonical memory records.
    Rebuild {
        #[arg(long)]
        json: bool,
    },

    /// Check installation and repo memory readiness.
    Doctor {
        #[arg(long = "project-root")]
        project_root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Print or run a tiny first-run workflow.
    Quickstart {
        #[arg(long)]
        apply_sample: bool,
        #[arg(long)]
        json: bool,
    },

    /// Check for or apply a Memzoi release update.
    Update {
        /// Only check whether an update is available.
        #[arg(long)]
        check: bool,

        /// Release tag to install, or latest.
        #[arg(long = "ref", default_value = "latest")]
        reference: String,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// MCP integration helpers.
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },

    /// Generate or install agent integration prompts/instructions.
    Integrate {
        #[command(subcommand)]
        command: IntegrateCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProposalCommands {
    /// List proposals by status.
    List {
        #[arg(long, default_value = "open")]
        status: String,
        #[arg(long)]
        json: bool,
    },

    /// Show one proposal.
    Show {
        proposal_id: String,
        #[arg(long)]
        json: bool,
    },

    /// Apply all approved proposals.
    Apply {
        #[arg(long = "all-approved")]
        all_approved: bool,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProposalFileCommands {
    /// List valid proposal files.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Show one proposal file.
    Show {
        proposal_id: String,
        #[arg(long)]
        json: bool,
    },

    /// Validate proposal files and report every parsing error.
    Validate {
        #[arg(long)]
        json: bool,
    },

    /// Apply one repo-safe create proposal file into canonical records.
    Apply {
        proposal_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LocalCommands {
    /// Add a local-only runtime memory record.
    Add {
        #[arg(long = "type")]
        memory_type: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// List local-only runtime memory records.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Search local-only runtime memory records.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CheckpointCommands {
    /// Add a runtime session checkpoint.
    Add {
        #[arg(long)]
        task: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// List runtime session checkpoints.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommands {
    /// Print copy-pasteable MCP server configuration JSON.
    Config {
        #[arg(long = "project-root", default_value = ".")]
        project_root: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum IntegrateCommands {
    /// Print a one-shot prompt that teaches an agent to use Memzoi.
    Prompt,

    /// Create or update a marked Memzoi block in an instruction file.
    Instructions {
        #[arg(long, default_value = "AGENTS.md")]
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) struct DraftCommand {
    pub(crate) memory_type: String,
    pub(crate) scope_kind: String,
    pub(crate) visibility: String,
    pub(crate) title: String,
    pub(crate) body: String,
}
