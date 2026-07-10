---
title: Development
---

# Development

This repo contains the Rust workspace, the Docusaurus documentation site, examples, and smoke scripts.

## Rust workspace

Workspace crates:

- `memzoi-core`: memory model, storage, lifecycle, search, context, precheck, and exports.
- `memzoi-cli`: `memzoi` command-line interface.
- `memzoi-mcp`: stdio MCP server.

Common checks:

```bash
make smoke
make onboarding-smoke
```

Underlying commands:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Documentation site

The docs are built with Docusaurus 3 from the committed `website/docs/` directory.

```bash
cd website
pnpm install
pnpm docs:start
pnpm docs:build
pnpm docs:serve
```

Use `pnpm docs:build` before opening a documentation PR. The Docusaurus config treats broken links as build errors and broken Markdown links as warnings.

## GitHub Pages

GitHub Pages deployment is configured in `.github/workflows/pages.yml`.

The workflow:

- runs on pushes to `main` and manual dispatch
- installs pnpm 11.10.0
- installs dependencies in `website/` with `pnpm install --frozen-lockfile`
- builds docs in `website/` with `pnpm docs:build`
- uploads the generated `website/build/` directory as the Pages artifact

The generated `website/build/` and `website/.docusaurus/` directories are ignored and should not be committed.

## Local verification checklist

For documentation-only changes:

```bash
cd website
pnpm docs:build
```

When Rust tooling is available, also check:

```bash
memzoi --help
memzoi propose --help
memzoi search --help
memzoi context --help
memzoi precheck --help
make onboarding-smoke
```

Use the existing `target/debug/memzoi` binary for local help comparisons if Cargo is unavailable but a debug binary has already been built.
