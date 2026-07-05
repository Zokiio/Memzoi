#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  printf '%s\n' 'error: cargo is required to install memzoi' >&2
  printf '%s\n' 'Install Rust from https://rustup.rs/ and re-run this script.' >&2
  exit 1
fi

INSTALL_ROOT=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
BIN_DIR="$INSTALL_ROOT/bin"

printf '%s\n' '+ cargo install --path crates/memzoi-cli --locked'
cargo install --path crates/memzoi-cli --locked

printf '%s\n' '+ cargo install --path crates/memzoi-mcp --locked'
cargo install --path crates/memzoi-mcp --locked

printf '%s\n' '+ memzoi --version'
"$BIN_DIR/memzoi" --version

printf '%s\n' '+ memzoi-mcp --version'
"$BIN_DIR/memzoi-mcp" --version

ln -sf memzoi "$BIN_DIR/agent-memory"
ln -sf memzoi-mcp "$BIN_DIR/agent-memory-mcp"

printf '%s\n' '+ agent-memory compatibility alias --version'
"$BIN_DIR/agent-memory" --version

printf '%s\n' '+ agent-memory-mcp compatibility alias --version'
"$BIN_DIR/agent-memory-mcp" --version

if ! command -v memzoi >/dev/null 2>&1; then
  printf '\n%s\n' "Note: $BIN_DIR is not on PATH in this shell."
  printf '%s\n' "Add it with: export PATH=\"$BIN_DIR:\$PATH\""
fi

cat <<'NEXT'

Installed Memzoi.

Compatibility aliases are also installed: agent-memory, agent-memory-mcp.

Next:
  memzoi init
  memzoi doctor
  memzoi quickstart --apply-sample
NEXT
