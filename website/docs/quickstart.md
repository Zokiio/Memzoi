---
title: Quickstart
---

# Quickstart

This workflow creates a throwaway Git repo, initializes repo `.memzoi/` memory plus local runtime
state, writes the built-in sample memory, recalls it, checks a risky command, exports an agent
projection, and prints MCP configuration.

## Install Memzoi

Mac or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 | iex"
```

The install script downloads the latest release binary and does not require Cargo.

Or install from a source checkout:

```bash
make install
```

Then verify the binaries:

```bash
memzoi --version
memzoi-mcp --version
```

## Run the first workflow

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

`memzoi quickstart --apply-sample` proposes, approves, and applies one sample repo memory. It is idempotent: running it again should reuse the existing sample record instead of creating duplicates.

## Preview the workflow without writing the sample

```bash
memzoi quickstart
```

Use this when you want the command sequence before letting Memzoi write a sample memory.

## What `init` creates

`memzoi init` creates the canonical memory directory in the discovered project root:

- `.memzoi/records/`

It also creates local runtime state under `~/.memzoi/projects/<repository-key>/`:

- `config.toml`
- `shared.db` for local/session memory and DB-local proposals shared by linked worktrees
- `worktrees/<worktree-key>/index.db` for the active worktree's disposable canonical index
- `worktrees/<worktree-key>/exports/`

The repository key is derived from Git's canonical common directory and is therefore stable across linked worktrees. Each worktree receives its own index and generated exports. Opening a newly linked worktree builds its index from that checkout's `.memzoi/records/`; it does not require another `memzoi init`.

Project root discovery prefers an ancestor with `.memzoi/records/`, then an ancestor Git root, then the current directory.

## Health check

```bash
memzoi doctor --json
```

`doctor` checks the project root, records directory, shared runtime, current worktree index, legacy path-keyed runtimes, exports directory, and whether `memzoi-mcp` is available on `PATH`. Missing canonical records and missing shared runtime are reported separately.
