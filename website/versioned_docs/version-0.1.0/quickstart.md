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
curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | MEMZOI_REF=v0.1.0 sh
```

Windows:

```powershell
$env:MEMZOI_REF = "v0.1.0"
irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 | iex
```

The install script downloads the v0.1.0 release binary and does not require Cargo.

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

It also creates local runtime state under `~/.memzoi/projects/<project-key>/`:

- `config.toml`
- `memory.db`
- `exports/`

Project root discovery prefers an ancestor with `.memzoi/records/`, then an ancestor Git root, then the current directory.

## Health check

```bash
memzoi doctor --json
```

`doctor` checks the project root, records directory, runtime config, database, schema, exports directory, and whether `memzoi-mcp` is available on `PATH`. Missing records, config, or database usually means the repo has not run `memzoi init` yet.
