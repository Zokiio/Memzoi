---
title: Install
---

# Install

Memzoi v0 is installed through release binaries or from a source checkout. Homebrew and crates.io packages are roadmap items.

## Install methods

| Method | Status | Command | Notes |
| --- | --- | --- | --- |
| Install script | Available now | `curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh \| sh` | Downloads the latest release binary for supported platforms. |
| Windows install script | Available now | `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1 \| iex"` | Downloads the latest release binary for supported platforms. |
| Source checkout | Available now | `make install` | Requires Cargo; installs `memzoi` and `memzoi-mcp`. |
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

By default, the install script downloads the latest GitHub release binary, verifies its SHA-256 checksum, and installs into `~/.local/bin` on Mac/Linux. On Windows, it installs under `%LOCALAPPDATA%\Programs\Memzoi\bin` and updates the user `Path`.

The v0.5.0 Windows binaries support MCP and most CLI surfaces. CLI/MCP capture
file operations and private lifecycle artifact I/O fail closed on Windows
because they require Unix handle-relative, no-symlink, or atomic no-clobber
primitives. For private lifecycle, `authorize`, `apply`, and `plan --output`
are unavailable; `plan` without `--output`, `inspect`, and `revoke` remain
available.

Install a pinned release by tag. Replace `vX.Y.Z` with a tag from the
[latest GitHub release](https://github.com/Zokiio/Memzoi/releases/latest):

```bash
curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | MEMZOI_REF=vX.Y.Z sh
```

Set `MEMZOI_REF=main` to install from the current main branch instead; that source install path requires Cargo.

## Staying Up To Date

For supported Mac/Linux release-binary installs:

```bash
memzoi update
```

To check without changing files:

```bash
memzoi update --check
```

`memzoi update` only applies updates to release-binary installs where `memzoi` and `memzoi-mcp` are sibling binaries in a writable, non-package-managed directory. Source checkouts should use `git pull && make install`; Cargo and future package-manager installs should use their installer or package manager. Windows supports `memzoi update --check` and prints the PowerShell install command for manual updates.

Release downloads are verified against the release `.sha256` sidecar for download integrity. This is not a cryptographic signature or proof of artifact authenticity.

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

## Release Packaging Note

When validating or publishing crates, handle `memzoi-core` first. `memzoi-cli` and `memzoi-mcp` declare versioned dependencies on `memzoi-core`, so their `cargo package` checks can fail until the matching `memzoi-core` version is available in the crates.io index.

## Verify Install

```bash
memzoi --version
memzoi-mcp --version
memzoi doctor
```

If the Mac/Linux binaries install successfully but the shell cannot find them, add the install directory to `PATH`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

If `MEMZOI_INSTALL_DIR` is set, use that directory instead.

## Developer Mode

You can run the binaries from source without installing:

```bash
cargo run -p memzoi-cli -- --help
cargo run -p memzoi-mcp -- --help
```

Developer mode is useful while editing the repo. The installed CLI path is better for testing real agent integrations because MCP clients usually expect a stable binary on `PATH`.
