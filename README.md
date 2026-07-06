# Memzoi

File-native project memory for coding agents.


> **Motivation for building**
>
> I am constantly trying out different AI providers, agent harnesses, CLIs, and prompting workflows. Most of them have some form of memory, but that memory is usually siloed inside the tool that created it. As I move between tools, useful project knowledge gets fragmented across chats, prompts, provider state, and local runtimes.
>
> The result is that a new agent often feels dumber than it should because it cannot see the context another agent already helped uncover.
>
> Memzoi is a way to collect, review, and reuse that knowledge as file-native project memory: durable enough to survive tool switches, transparent enough for humans to review, and portable enough for different agents to build on.

Memzoi, pronounced "mem-zoy", gives AI coding agents a safe way to recall durable project knowledge, propose new memory, run pre-action checks, and export reviewable agent instructions. Canonical memory lives in human-readable files, while generated indexes and exports are disposable runtime state.

Memzoi is currently a local-first v0 for dogfooding and early experimentation. The CLI and MCP server are available through release binaries or from a source checkout; package-manager installs are roadmap items.

## Why Memzoi?

- Keep durable agent memory in Markdown/YAML files that humans can review and diff.
- Separate canonical repo memory from generated SQLite indexes and exports.
- Require proposed, reviewable writes before durable memory changes are applied.
- Build prompt-ready context packs for the task at hand.
- Check risky paths, actions, and shell commands against known warnings.
- Expose safe recall and proposal workflows through a minimal stdio MCP server.

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
- Local runtime state under `~/.memzoi/projects/<project-key>/` for derived SQLite indexes, generated exports, and DB-local open proposal state.
- Safe memory lifecycle: propose, approve, reject, apply, supersede, and tombstone.
- Rebuild from canonical records with `memzoi rebuild` when the derived runtime index needs to be regenerated.
- Text search and prompt-ready context packs.
- Pre-action governance checks with citations and suggested next steps.
- Deterministic generated exports: OKF Markdown projections, `AGENTS.memory.md`, and `CLAUDE.memory.md`.
- Minimal stdio MCP server with safe tools:
  - `search_memory`
  - `build_context_pack`
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

Memzoi is designed for project memory, not secret storage. Do not commit API keys, credentials, private personal data, raw chat logs, or temporary task progress into repo-shared memory records.

If you discover a security issue, please avoid posting sensitive details publicly. Open a minimal issue or contact the maintainer with enough context to coordinate a fix.

## License

Memzoi is licensed under the MIT license as declared in the Cargo workspace metadata.
