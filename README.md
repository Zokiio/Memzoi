# Memzoi

**The unified memory provider for AI agents.**

AI coding agents are becoming interchangeable. Their memory should not be.

Every day I switch between AI providers, coding agents, CLIs, editors, and prompting workflows. Each one can discover valuable knowledge about a project, but that knowledge usually stays trapped inside the tool that created it.

When I move from Claude to Codex, or from Copilot to another agent, the new agent often feels dumber because it has no memory of everything that has already been learned.

Memzoi exists to solve that problem.

Instead of every AI tool maintaining its own isolated memory, Memzoi aims to provide one shared memory layer that every coding agent can use.

Today that starts with file-native, reviewable repository memory. Over time it will extend to personal memory, runtime memory, cross-agent memory, intelligent retrieval, and long-term knowledge that survives conversations, providers, and entire development environments.

The goal is simple:

> No matter which coding agent you use, you should only need one memory provider.

Memzoi, pronounced "mem-zoy", is currently a local-first v0 for dogfooding and early experimentation. It gives coding agents a safe way to recall durable project knowledge, propose new memory, run pre-action checks, and export reviewable agent instructions. The CLI and MCP server are available through release binaries or from a source checkout; package-manager installs are roadmap items.

## Why Memzoi?

Most AI memory systems optimize for one specific agent or provider. Many are cloud-hosted, opaque, difficult to review or version, and impossible to share through normal software engineering workflows.

Memzoi takes a different approach: repository knowledge should look and behave like the rest of your project.

If an AI discovers something important about the codebase, that knowledge should be reviewable in exactly the same way as code—not hidden inside another database, trapped inside another provider, or locked to a single model.

## Core Principles

### One memory model

Repository memory. Personal memory. Session memory. Future memory types.

Every kind of memory should follow the same lifecycle, provenance model, and retrieval APIs.

### Git-native where Git makes sense

Shared project knowledge belongs in Git. Repository memories should appear as normal file changes that are reviewed through `git diff`, staged changes, pull requests, and code review.

Developers should not need to remember to open a separate memory inbox before every commit. Git is already where durable project changes are reviewed; Memzoi should integrate with it instead of replacing it.

### Personal memory stays personal

Not everything belongs in Git. Personal preferences, working habits, private notes, and runtime memories should remain private while still being available across coding agents.

Repository memory and personal memory should work together without becoming the same thing.

### Agent-independent

The memory belongs to the developer—not to Claude, Codex, Copilot, Cursor, OpenAI, Anthropic, or any other provider.

Any agent should be able to contribute to the same memory and benefit from knowledge discovered by previous agents.

### Reviewable by default

Knowledge should never silently become canonical. Personal memory may be captured automatically, but shared repository memory should remain explicit and reviewable through normal software engineering workflows.

### Retrieval is replaceable

Lexical search, semantic search, embeddings, graph retrieval, and future retrieval methods are implementation details.

The canonical memory remains the same. Derived indexes should always be rebuildable.

## Vision

Memzoi is building a unified memory layer for AI agents.

Today:

- ✅ File-native repository memory
- ✅ Reviewable Git workflow
- ✅ Agent integrations
- ✅ Context generation
- ✅ Memory lifecycle
- ✅ Provenance

Tomorrow:

- Personal memory
- Runtime memory
- Cross-agent memory
- Intelligent retrieval
- Automatic capture
- Memory consolidation
- Knowledge promotion
- Organization memory

## Long-term Goal

The long-term goal is not to become another vector database, LLM framework, or agent.

The goal is to become the memory system every coding agent wants to use.

Whether the agent is Claude, Codex, Copilot, Cursor, Antigravity, or something that does not exist yet, they should all be able to share the same durable knowledge through Memzoi.

## Status

| Area | Status |
| --- | --- |
| CLI | Available via install script or source checkout |
| MCP server | Available via install script or source checkout |
| Documentation site | Available under `website/docs/` |
| Release binaries | Available on GitHub Releases for supported platforms |
| Self-update | Available for supported Mac/Linux release-binary installs |
| Homebrew and package-manager installs | Planned |

The primary binaries are `memzoi` and `memzoi-mcp`.

## Quickstart

Install Memzoi on Mac or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | sh
```

Install Memzoi on Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 | iex"
```

The install script downloads the latest release binary for supported platforms and verifies its SHA-256 checksum. On Mac/Linux it installs to `~/.local/bin` by default; Cargo is only required for source installs.

Or install both binaries from a source checkout:

```bash
make install
```

Verify the install:

```bash
memzoi --version
memzoi-mcp --version
```

Check for newer Memzoi releases:

```bash
memzoi update --check
```

Create a demo repo and run the first workflow:

```bash
mkdir -p /tmp/memzoi-demo
cd /tmp/memzoi-demo
git init
```

Initialize Memzoi memory and try recall, precheck, export, and MCP config:

```bash
memzoi init
memzoi doctor
memzoi quickstart --apply-sample
memzoi search quickstart
memzoi context --task "remember quickstart setup"
memzoi handoff --task "switch agents after quickstart"
memzoi precheck --command "rm -rf .memzoi"
memzoi export agents-md
memzoi mcp config --project-root .
```

For the full walkthrough, see [website/docs/quickstart.md](website/docs/quickstart.md).

## Documentation

The documentation site covers installation, memory lifecycle, recall, prechecks, exports, MCP integration, and the CLI reference.

- [Start here](website/docs/index.md)
- [Install](website/docs/install.md)
- [Quickstart](website/docs/quickstart.md)
- [Memory lifecycle](website/docs/memory-lifecycle.md)
- [Recall and precheck](website/docs/recall-and-precheck.md)
- [MCP and agent integration](website/docs/mcp-and-agent-integration.md)
- [Reference](website/docs/reference.md)
- [Development](website/docs/development.md)
- [Trust evaluation](docs/evaluation.md)
- [Product roadmap](docs/roadmap.md)

Run the docs site locally:

```bash
cd website
pnpm install
pnpm docs:start
```

Build the docs site:

```bash
cd website
pnpm install
pnpm docs:build
```

GitHub Pages deployment is configured in [.github/workflows/pages.yml](.github/workflows/pages.yml).

## What Works Now

- File-native canonical memory records under `.memzoi/records/`.
- Repository-shared local runtime state under `~/.memzoi/projects/<repository-key>/`, with a durable `shared.db` for local/session memory and DB-local proposals plus disposable indexes and exports under `worktrees/<worktree-key>/`.
- Safe memory lifecycle: propose, approve, reject, apply, supersede, and tombstone.
- Evidence-backed capture from explicit Markdown, agent-instruction, ADR, and Git-change sources: deterministic planning, complete human review, exact source replay, and stale-guarded apply. Repo-safe candidates become pending proposal files, private local/session candidates stay in runtime storage, and capture provenance survives later apply and rebuild. See the [capture reference](website/docs/reference.md#evidence-backed-capture).
- Deterministic planning for explicitly classified `memzoi/import-v1` candidates creates reviewable repo proposal files and private local/session runtime records on guarded apply. See the [import reference](website/docs/reference.md#classified-import).
- Rebuild from canonical records with `memzoi rebuild` when the derived runtime index needs to be regenerated.
- Text search, prompt-ready context packs, and CLI handoff packs.
- File-native recall v2 and capture v1 evaluation gates with isolated fixtures, stable reports, typed local baselines, capture quality and evidence metrics, prohibited-data hard gates, and measured review burden.
- Pre-action governance checks with citations and suggested next steps.
- Deterministic generated exports: OKF Markdown projections, `AGENTS.memory.md`, and `CLAUDE.memory.md`.
- Minimal stdio MCP server with safe tools:
  - `search_memory`
  - `inspect_memory_expiry`
  - `build_context_pack`
  - `plan_capture_v1`
  - `propose_memory`
  - `precheck_path`
  - `precheck_action`
  - `precheck_command`

## Project Layout

```text
crates/memzoi-core/  Core memory model, storage, lifecycle, search, context, precheck, and exports
crates/memzoi-cli/   `memzoi` command-line interface
crates/memzoi-mcp/   `memzoi-mcp` stdio MCP server
examples/            Example MCP config and memory files
scripts/             Install and smoke-test scripts
website/docs/        Docusaurus documentation source
```

## Development

Run the primary smoke checks:

```bash
make smoke
make onboarding-smoke
make eval
make capture-smoke
```

Run the underlying Rust checks directly:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run the binaries from source without installing:

```bash
cargo run -p memzoi-cli -- --help
cargo run -p memzoi-mcp -- --help
```

## Contributing

Contributions are welcome while the project is still in v0. Please keep changes small, reviewable, and aligned with the file-native memory model:

- Treat Markdown/YAML memory records as the source of truth.
- Keep generated indexes and exports disposable.
- Prefer typed, scoped records over large unstructured memory dumps.
- Do not store secrets or private personal data in repo-shared memory.
- Update or add docs for user-facing CLI, MCP, or schema changes.

Before opening a pull request, run the relevant checks from the [Development](#development) section. For documentation-only changes, run `pnpm docs:build` from `website/`.

## Security and Privacy

Repository-bound memory is protected by a shared, projection-bound safety capability. See [Repository-write safety](docs/repository-write-safety.md) for the detector policy, redacted diagnostics, and staged/range scan commands.

Memzoi is designed for project memory, not secret storage. Do not commit API keys, credentials, private personal data, raw chat logs, or temporary task progress into repo-shared memory records.

If you discover a security issue, please avoid posting sensitive details publicly. Open a minimal issue or contact the maintainer with enough context to coordinate a fix.

## License

Memzoi is licensed under the MIT license as declared in the Cargo workspace metadata.
