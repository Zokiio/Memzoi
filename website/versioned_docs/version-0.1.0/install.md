---
title: Install
---

# Install

Memzoi v0 is installed through the install script or from a source checkout. Homebrew is a roadmap item.

## Install methods

| Method | Status | Command | Notes |
| --- | --- | --- | --- |
| Install script | Available now | `curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh \| sh` | Downloads release binaries for supported platforms. |
| Windows install script | Available now | `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 \| iex"` | Downloads release binaries for supported platforms. |
| Source checkout | Available now | `make install` | Installs `memzoi`, `memzoi-mcp`, and v0 compatibility aliases. |
| crates.io | Planned | `cargo install memzoi-cli` and `cargo install memzoi-mcp` | Publish `memzoi-core` first; dependent package checks need it in the crates.io index. |
| GitHub release binary | Available now | [Download from GitHub Releases](https://github.com/Zokiio/Memzoi/releases) | The install scripts select the matching release asset automatically. |
| Homebrew | Future | TBD | Defer until at least one GitHub release exists. |

## Install Script

Mac or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 | iex"
```

By default, the install script downloads `v0.1.0` release binaries from GitHub. Set `MEMZOI_REF=main` to install from the current main branch instead; that source install path requires Cargo.

## Local Cargo Install

From the Memzoi repo:

```bash
make install
```

The install script runs:

```bash
cargo install --path crates/memzoi-cli --locked
cargo install --path crates/memzoi-mcp --locked
```

It installs the primary binaries:

- `memzoi`
- `memzoi-mcp`

It also installs compatibility aliases for v0-era scripts and MCP configs:

- `agent-memory`
- `agent-memory-mcp`

## Release Packaging Note

When validating or publishing crates, handle `memzoi-core` first. `memzoi-cli` and `memzoi-mcp` declare versioned dependencies on `memzoi-core`, so their `cargo package` checks can fail until the matching `memzoi-core` version is available in the crates.io index.

## Verify Install

```bash
memzoi --version
memzoi-mcp --version
memzoi doctor
```

If the binaries install successfully but the shell cannot find them, add Cargo's bin directory to `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

If `CARGO_INSTALL_ROOT` or `CARGO_HOME` is set, use that install root's `bin` directory instead.

## Developer Mode

You can run the binaries from source without installing:

```bash
cargo run -p memzoi-cli -- --help
cargo run -p memzoi-mcp -- --help
```

Developer mode is useful while editing the repo. The installed CLI path is better for testing real agent integrations because MCP clients usually expect a stable binary on `PATH`.
