#!/bin/sh
set -eu

REPO_URL="${MEMZOI_REPO_URL:-https://github.com/Zokiio/Memzoi.git}"
REF="${MEMZOI_REF:-v0.1.0}"
DOWNLOAD_BASE="${MEMZOI_DOWNLOAD_BASE:-https://github.com/Zokiio/Memzoi/releases/download}"

INSTALL_ROOT=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
BIN_DIR="$INSTALL_ROOT/bin"
REPO_ROOT=""

if [ -d "crates/memzoi-cli" ] && [ -d "crates/memzoi-mcp" ]; then
  REPO_ROOT=$(pwd)
else
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || printf '.')
  if [ -d "$SCRIPT_DIR/../crates/memzoi-cli" ] && [ -d "$SCRIPT_DIR/../crates/memzoi-mcp" ]; then
    REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
  fi
fi

need_cargo() {
  if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' 'error: cargo is required for source installs' >&2
    printf '%s\n' 'Install Rust from https://rustup.rs/ and re-run this script, or use a release tag with binary assets.' >&2
    exit 1
  fi
}

install_git_package() {
  package=$1
  need_cargo
  if [ "$REF" = "main" ] || [ "$REF" = "master" ]; then
    printf '%s\n' "+ cargo install --git $REPO_URL --branch $REF $package --locked"
    cargo install --git "$REPO_URL" --branch "$REF" "$package" --locked
  else
    printf '%s\n' "+ cargo install --git $REPO_URL --tag $REF $package --locked"
    cargo install --git "$REPO_URL" --tag "$REF" "$package" --locked
  fi
}

install_path_package() {
  package_path=$1
  need_cargo
  printf '%s\n' "+ cargo install --path $package_path --locked"
  cargo install --path "$package_path" --locked
}

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)

  case "$os:$arch" in
    Linux:x86_64 | Linux:amd64)
      printf '%s\n' 'x86_64-unknown-linux-gnu'
      ;;
    Darwin:x86_64 | Darwin:amd64)
      printf '%s\n' 'x86_64-apple-darwin'
      ;;
    Darwin:arm64 | Darwin:aarch64)
      printf '%s\n' 'aarch64-apple-darwin'
      ;;
    *)
      printf '%s\n' "unsupported platform: $os $arch" >&2
      return 1
      ;;
  esac
}

download_file() {
  url=$1
  output=$2

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$output"
  else
    printf '%s\n' 'error: curl or wget is required to download Memzoi release binaries' >&2
    exit 1
  fi
}

install_release_binaries() {
  target=$(detect_target) || return 1
  archive="memzoi-$REF-$target.tar.gz"
  url="$DOWNLOAD_BASE/$REF/$archive"
  tmp_dir=$(mktemp -d)

  printf '%s\n' "+ download $url"
  if ! download_file "$url" "$tmp_dir/$archive"; then
    rm -rf "$tmp_dir"
    return 1
  fi

  tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"
  mkdir -p "$BIN_DIR"
  cp "$tmp_dir/memzoi" "$BIN_DIR/memzoi"
  cp "$tmp_dir/memzoi-mcp" "$BIN_DIR/memzoi-mcp"
  chmod +x "$BIN_DIR/memzoi" "$BIN_DIR/memzoi-mcp"
  rm -rf "$tmp_dir"
}

if [ -n "$REPO_ROOT" ]; then
  install_path_package "$REPO_ROOT/crates/memzoi-cli"
  install_path_package "$REPO_ROOT/crates/memzoi-mcp"
elif [ "$REF" = "main" ] || [ "$REF" = "master" ]; then
  install_git_package memzoi-cli
  install_git_package memzoi-mcp
elif ! install_release_binaries; then
  printf '%s\n' "error: no release binary found for $REF on this platform" >&2
  printf '%s\n' "Try installing from source with: MEMZOI_REF=main sh -c 'curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | sh'" >&2
  exit 1
fi

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
