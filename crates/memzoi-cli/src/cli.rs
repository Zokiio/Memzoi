use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
        /// Optional evidence source kind, such as issue, PR, commit, or document.
        #[arg(long = "source-kind")]
        source_kind: Option<String>,
        /// Optional durable evidence locator kept separate from proposal lineage.
        #[arg(long = "source-ref")]
        source_ref: Option<String>,
        /// Repo sharing classification. Canonical apply requires repo-safe.
        #[arg(long, default_value = "unknown")]
        sensitivity: String,
        /// Contextual repository-content classification. Canonical apply requires general repo knowledge.
        #[arg(long = "content-class", default_value = "unknown")]
        content_class: String,
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

    /// Plan and apply classified imports.
    Import {
        #[command(subcommand)]
        command: ImportCommands,
    },

    /// Plan, review, and route evidence-backed capture.
    Capture {
        #[command(subcommand)]
        command: CaptureCommands,
    },

    /// Build immutable, repository-only maintenance evidence plans.
    Maintenance {
        #[command(subcommand)]
        command: MaintenanceCommands,
    },

    /// Plan and execute exact owner-authorized private lifecycle actions.
    Lifecycle {
        #[command(subcommand)]
        command: LifecycleCommands,
    },

    /// Plan, decide, and apply direct Git-native record materialization.
    Materialize {
        #[command(subcommand)]
        command: MaterializeCommands,
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

    /// Inspect runtime event log rows.
    Events {
        #[command(subcommand)]
        command: EventCommands,
    },

    /// Promote explicit session-end candidates into proposals or runtime memory.
    SessionEnd {
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long = "from-checkpoint")]
        from_checkpoint: Option<String>,
        /// Caller-controlled idempotency identity for checkpoint promotion. Required with --json.
        #[arg(long = "operation-id")]
        operation_id: Option<String>,
        /// Source checkpoint version observed by the caller. Required with --json.
        #[arg(long = "expected-version")]
        expected_version: Option<String>,
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
        /// Optional evidence source kind for the replacement record.
        #[arg(long = "source-kind")]
        source_kind: Option<String>,
        /// Optional durable evidence locator for the replacement record.
        #[arg(long = "source-ref")]
        source_ref: Option<String>,
        /// Repo sharing classification. Canonical replacement requires repo-safe.
        #[arg(long, default_value = "unknown")]
        sensitivity: String,
        /// Contextual repository-content classification. Canonical replacement requires general repo knowledge.
        #[arg(long = "content-class", default_value = "unknown")]
        content_class: String,
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

    /// Inspect a record's expiry and explain normal-read eligibility.
    Expiry {
        /// Record ID to inspect, including records excluded from normal reads.
        record_id: String,
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

    /// Build a compact context pack for switching agents or harnesses.
    Handoff {
        #[arg(long)]
        task: Option<String>,
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

    /// Scan repository memory blobs for prohibited content without modifying Git.
    Safety {
        #[command(subcommand)]
        command: SafetyCommands,
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

    /// Run file-native evaluation suites in isolated state.
    Eval {
        #[command(subcommand)]
        command: EvalCommands,
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
pub(crate) enum SafetyCommands {
    /// Scan a staged index, a branch range head, or one working-tree file.
    Scan {
        #[arg(long, conflicts_with_all = ["range", "file"])]
        staged: bool,
        #[arg(long, value_name = "BASE...HEAD", conflicts_with_all = ["staged", "file"])]
        range: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with_all = ["staged", "range"])]
        file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum EvalCommands {
    /// Validate task-utility, fallback, lifecycle, and performance evidence.
    RecallOperational {
        /// Strict operational evidence JSON file.
        #[arg(long)]
        evidence: PathBuf,
        /// Emit the stable machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Validate a frozen two-track competitor bakeoff evidence package.
    RecallCompetitors {
        /// Strict competitor evidence JSON file.
        #[arg(long)]
        evidence: PathBuf,
        /// Emit the stable machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate candidate-neutral recall v3 quality and emit release evidence.
    RecallV3 {
        #[command(subcommand)]
        command: Option<Box<RecallV3Commands>>,
        /// Explicit path to the strict recall-v3 corpus YAML file.
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Strict candidate manifest JSON files to evaluate after the lexical baseline.
        #[arg(long = "candidate")]
        candidates: Vec<PathBuf>,
        /// Local vector-artifact roots paired positionally with --candidate values.
        #[arg(long = "artifact-root", requires = "candidates")]
        artifact_roots: Vec<PathBuf>,
        /// Write the digest commitment artifact to this path.
        #[arg(long)]
        commitment: Option<PathBuf>,
        /// Prepare a commitment for a local locked-test corpus without running retrieval.
        #[arg(long, conflicts_with_all = ["commitment", "verify_locked_commitment"])]
        prepare_locked_commitment: Option<PathBuf>,
        /// Verify a local locked-test corpus and candidate set before evaluating it.
        #[arg(long)]
        verify_locked_commitment: Option<PathBuf>,
        /// Fail if a non-lexical candidate falls back instead of executing.
        #[arg(long)]
        require_ready_candidates: bool,
        /// Emit the stable machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate recall quality against a versioned YAML corpus.
    Recall {
        /// Explicit path to the recall corpus YAML file.
        #[arg(long)]
        corpus: PathBuf,
        /// Compare deterministic results with this local baseline artifact.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Replace the selected baseline before comparing it.
        #[arg(long, requires = "baseline")]
        update_baseline: bool,
        /// Emit the stable machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate capture quality and review burden against a versioned corpus.
    Capture {
        /// Explicit path to the capture corpus YAML file.
        #[arg(long)]
        corpus: PathBuf,
        /// Compare deterministic results with this local baseline artifact.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Replace the selected baseline after every safety and quality gate passes.
        #[arg(long, requires = "baseline")]
        update_baseline: bool,
        /// Emit the stable machine-readable report.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecallV3Commands {
    /// Manage explicitly installed offline embedding models.
    Model {
        #[command(subcommand)]
        command: RecallV3ModelCommands,
    },
    /// Freeze the development-only candidate process.
    Development {
        #[command(subcommand)]
        command: RecallV3DevelopmentCommands,
    },
    /// Build immutable vector artifacts and candidate manifests.
    Candidate {
        #[command(subcommand)]
        command: RecallV3CandidateCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecallV3CandidateCommands {
    /// Build one profile/template artifact and its three architecture manifests.
    Build {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        matrix: PathBuf,
        #[arg(long)]
        corpus: PathBuf,
        #[arg(long)]
        model_root: PathBuf,
        #[arg(long)]
        template: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "development-generation-1")]
        generation: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecallV3DevelopmentCommands {
    /// Build and evaluate the complete checked-in candidate matrix.
    Run {
        #[arg(long)]
        matrix: PathBuf,
        #[arg(long)]
        corpus: PathBuf,
        #[arg(long)]
        model_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        attempted_at: String,
        #[arg(long, default_value = "development-generation-1")]
        generation: String,
        #[arg(long)]
        json: bool,
    },
    /// Freeze the lexical baseline and best trust-safe candidate per architecture.
    Freeze {
        #[arg(long)]
        run: PathBuf,
        #[arg(long)]
        corpus: PathBuf,
        #[arg(long)]
        matrix: PathBuf,
        #[arg(long)]
        profile_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        frozen_at: String,
        #[arg(long)]
        json: bool,
    },
    /// Verify and publish compact evidence without model or vector files.
    Publish {
        #[arg(long)]
        run: PathBuf,
        #[arg(long)]
        corpus: PathBuf,
        #[arg(long)]
        matrix: PathBuf,
        #[arg(long)]
        profile_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecallV3ModelCommands {
    /// Download, verify, and atomically install one pinned profile.
    Install {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        model_root: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Verify an installed model without network access.
    Inspect {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        model_root: PathBuf,
        #[arg(long)]
        json: bool,
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
pub(crate) enum ImportCommands {
    /// Classify an import manifest without writing.
    Plan {
        #[arg(long = "from-file")]
        from_file: PathBuf,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Apply a previously reviewed import plan.
    Apply {
        #[arg(long = "from-file")]
        from_file: PathBuf,
        #[arg(long = "plan-id")]
        plan_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CaptureCommands {
    /// Extract a deterministic, mutation-free plan from one explicit source.
    Plan {
        /// Explicit POSIX project-relative Markdown source path.
        #[arg(
            long,
            conflicts_with = "request_file",
            required_unless_present = "request_file"
        )]
        source: Option<String>,
        /// Complete capture-request JSON or YAML artifact for an extension profile.
        #[arg(long = "request-file", conflicts_with = "source")]
        request_file: Option<PathBuf>,
        /// Explicit supplied-bytes transport path, or '-' for explicitly selected stdin.
        #[arg(long = "source-bytes", requires = "request_file")]
        source_bytes: Option<PathBuf>,
        /// Stable source identifier included in evidence references.
        #[arg(long = "source-id", default_value = "source")]
        source_id: String,
        /// Explicit destination for the complete JSON plan artifact.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Emit the complete plan as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Bind reviewed decisions to an immutable capture plan.
    Review {
        /// Complete capture-plan JSON artifact.
        #[arg(long = "plan-file")]
        plan_file: PathBuf,
        /// Strict capture-review-input JSON artifact.
        #[arg(long = "decisions-file")]
        decisions_file: PathBuf,
        /// Prior capture-review artifact when replacing deferred decisions.
        #[arg(long = "prior-review-file")]
        prior_review_file: Option<PathBuf>,
        /// Exact supplied bytes used by the plan, or '-' for explicitly selected stdin.
        #[arg(long = "source-bytes")]
        source_bytes: Option<PathBuf>,
        /// Reviewer identity recorded in the review artifact.
        #[arg(long = "reviewed-by")]
        reviewed_by: String,
        /// Explicit RFC 3339 review time recorded in the review artifact.
        #[arg(long = "reviewed-at")]
        reviewed_at: String,
        /// Explicit destination for the complete JSON review artifact.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Emit the complete review as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Route one complete, pinned capture plan and review.
    Apply {
        /// Complete capture-plan JSON artifact.
        #[arg(long = "plan-file")]
        plan_file: PathBuf,
        /// Complete capture-review JSON artifact.
        #[arg(long = "review-file")]
        review_file: PathBuf,
        /// Immediate predecessor review when applying a later deferred-decision review.
        #[arg(long = "prior-review-file")]
        prior_review_file: Option<PathBuf>,
        /// Exact supplied bytes used by the plan, or '-' for explicitly selected stdin.
        #[arg(long = "source-bytes")]
        source_bytes: Option<PathBuf>,
        /// Expected plan identity pinned by the reviewer.
        #[arg(long = "plan-id")]
        plan_id: String,
        /// Expected review identity pinned by the reviewer.
        #[arg(long = "review-id")]
        review_id: String,
        /// Actor recorded on routed writes.
        #[arg(long, default_value = "cli")]
        actor: String,
        /// Emit the complete apply result as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MaintenanceCommands {
    /// Evaluate repository records without applying or authorizing maintenance.
    Plan {
        /// Restrict targets to these record IDs while retaining their comparison neighbourhood.
        #[arg(long = "record-id", value_name = "ID")]
        record_ids: Vec<String>,
        /// Explicit RFC 3339 evaluation instant for deterministic replay.
        #[arg(long = "evaluated-at", value_name = "RFC3339")]
        evaluated_at: Option<String>,
        /// Existing-parent destination outside the Git worktree and Memzoi runtime.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Emit the complete repository-safe plan as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LifecycleCommands {
    /// Build read-only maintenance evidence over private runtime records.
    Plan {
        /// Restrict targets to these record IDs while retaining their comparison neighbourhood.
        #[arg(long = "record-id", value_name = "ID")]
        record_ids: Vec<String>,
        /// Explicit RFC 3339 evaluation instant for deterministic evidence.
        #[arg(long = "evaluated-at", value_name = "RFC3339")]
        evaluated_at: Option<String>,
        /// Existing-parent destination outside the worktree and managed runtime state.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Emit the complete content-free private plan as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Persist one time-bounded, one-shot owner grant for an exact request.
    Authorize {
        #[arg(long = "request-file", value_name = "PATH")]
        request_file: PathBuf,
        #[arg(long = "plan-file", value_name = "PATH")]
        plan_file: Option<PathBuf>,
        /// Optional upper bound that may only shorten the default authority window.
        #[arg(long = "expires-at", value_name = "RFC3339")]
        expires_at: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Revoke an active owner grant without changing private lifecycle state.
    Revoke {
        #[arg(long = "grant-id", value_name = "ID")]
        grant_id: String,
        #[arg(long)]
        json: bool,
    },

    /// Explicitly inspect private lifecycle history or owner authority.
    Inspect {
        #[command(subcommand)]
        command: LifecycleInspectCommands,
    },

    /// Atomically consume one active grant and apply its exact action group.
    Apply {
        #[arg(long = "request-file", value_name = "PATH")]
        request_file: PathBuf,
        #[arg(long = "grant-id", value_name = "ID")]
        grant_id: String,
        #[arg(long = "plan-file", value_name = "PATH")]
        plan_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LifecycleInspectCommands {
    /// Inspect one private record, including quarantined or superseded history.
    Record {
        record_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect the authoritative stored grant row.
    Grant {
        grant_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MaterializeCommands {
    /// Derive a deterministic materialization plan from one candidate artifact.
    Plan {
        /// Complete repository-materialization-candidate JSON artifact.
        #[arg(long = "candidate-file")]
        candidate_file: PathBuf,
        /// Explicit destination for the complete JSON plan artifact.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Emit the complete plan as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Bind the repository materialization policy to a reviewed plan.
    Decide {
        /// Complete repository-materialization-candidate JSON artifact.
        #[arg(long = "candidate-file")]
        candidate_file: PathBuf,
        /// Complete repository-materialization-plan JSON artifact.
        #[arg(long = "plan-file")]
        plan_file: PathBuf,
        /// Explicit RFC 3339 decision timestamp.
        #[arg(long = "decision-at")]
        decision_at: String,
        /// Explicit destination for the complete JSON decision artifact.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Emit the complete decision as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Install one fully pinned canonical record without staging or committing it.
    Apply {
        /// Complete repository-materialization-candidate JSON artifact.
        #[arg(long = "candidate-file")]
        candidate_file: PathBuf,
        /// Complete repository-materialization-plan JSON artifact.
        #[arg(long = "plan-file")]
        plan_file: PathBuf,
        /// Complete repository-materialization-decision JSON artifact.
        #[arg(long = "decision-file")]
        decision_file: PathBuf,
        /// BLAKE3 identity expected in the candidate artifact.
        #[arg(long = "candidate-id")]
        candidate_id: String,
        /// BLAKE3 identity expected in the plan artifact.
        #[arg(long = "plan-id")]
        plan_id: String,
        /// BLAKE3 identity expected in the decision artifact.
        #[arg(long = "decision-id")]
        decision_id: String,
        /// Emit the core result and changed record summary as machine-readable JSON.
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

    /// Apply one repo-safe proposal file and archive its resolved packet.
    Apply {
        proposal_id: String,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Reject one proposal file with a reviewable reason.
    Reject {
        proposal_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "cli")]
        actor: String,
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
        /// Closed or expired checkpoint continued by this new session generation.
        #[arg(long = "successor-of")]
        successor_of: Option<String>,
        /// Caller-controlled idempotency identity. Required with --json.
        #[arg(long = "operation-id")]
        operation_id: Option<String>,
        /// Version of --successor-of observed by the caller. Required with --json.
        #[arg(long = "expected-version")]
        expected_version: Option<String>,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Extend the 24-hour lease of an active checkpoint.
    Continue {
        checkpoint_id: String,
        /// Caller-controlled idempotency identity. Required with --json.
        #[arg(long = "operation-id")]
        operation_id: Option<String>,
        /// Checkpoint version observed by the caller. Required with --json.
        #[arg(long = "expected-version")]
        expected_version: Option<String>,
        #[arg(long, default_value = "cli")]
        actor: String,
        #[arg(long)]
        json: bool,
    },

    /// Permanently close a checkpoint.
    Close {
        checkpoint_id: String,
        /// Caller-controlled idempotency identity. Required with --json.
        #[arg(long = "operation-id")]
        operation_id: Option<String>,
        /// Checkpoint version observed by the caller. Required with --json.
        #[arg(long = "expected-version")]
        expected_version: Option<String>,
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
pub(crate) enum EventCommands {
    /// Export runtime event log rows.
    Export {
        /// Emit compact JSON Lines: one JSON object per event row.
        #[arg(long)]
        jsonl: bool,
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
    /// List supported agent integration profiles.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Print a one-shot prompt that teaches an agent to use Memzoi.
    Prompt {
        /// Agent or integration profile to generate.
        #[arg(long, value_enum)]
        profile: IntegrateProfile,
    },

    /// Create or update a marked Memzoi block in an instruction file.
    Instructions {
        /// Agent or integration profile to generate.
        #[arg(long, value_enum)]
        profile: IntegrateProfile,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum IntegrateProfile {
    Codex,
    Claude,
    Mcp,
}

impl IntegrateProfile {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Mcp => "mcp",
        }
    }
}

pub(crate) struct DraftCommand {
    pub(crate) memory_type: String,
    pub(crate) scope_kind: String,
    pub(crate) visibility: String,
    pub(crate) source_kind: Option<String>,
    pub(crate) source_ref: Option<String>,
    pub(crate) sensitivity: String,
    pub(crate) content_class: String,
    pub(crate) title: String,
    pub(crate) body: String,
}
