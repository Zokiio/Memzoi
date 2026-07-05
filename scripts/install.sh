#!/bin/sh
set -eu

REPO_URL="${MEMZOI_REPO_URL:-https://github.com/Zokiio/Memzoi.git}"
REF="${MEMZOI_REF:-latest}"
DOWNLOAD_BASE="${MEMZOI_DOWNLOAD_BASE:-https://github.com/Zokiio/Memzoi/releases/download}"
RELEASE_API_BASE="${MEMZOI_RELEASE_API_BASE:-https://api.github.com/repos/Zokiio/Memzoi/releases}"

if [ -n "${MEMZOI_INSTALL_DIR:-}" ]; then
  BIN_DIR="$MEMZOI_INSTALL_DIR"
elif [ -n "${CARGO_INSTALL_ROOT:-}" ]; then
  BIN_DIR="$CARGO_INSTALL_ROOT/bin"
else
  BIN_DIR="$HOME/.local/bin"
fi

REPO_ROOT=""
RESOLVED_REF=""

if [ -d "crates/memzoi-cli" ] && [ -d "crates/memzoi-mcp" ]; then
  REPO_ROOT=$(pwd)
else
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || printf '.')
  if [ -d "$SCRIPT_DIR/../crates/memzoi-cli" ] && [ -d "$SCRIPT_DIR/../crates/memzoi-mcp" ]; then
    REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
  fi
fi

usage() {
  cat <<'HELP'
Usage: install.sh [--ref REF] [--install-dir DIR]

Environment:
  MEMZOI_REF          Release tag to install. Defaults to latest.
  MEMZOI_INSTALL_DIR  Directory for installed binaries. Defaults to ~/.local/bin.
  MEMZOI_REPO_URL     Git repository for source installs.
  MEMZOI_DOWNLOAD_BASE
                      Release download base URL.
HELP
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --ref)
      if [ "$#" -lt 2 ]; then
        printf '%s\n' 'error: --ref requires a value' >&2
        exit 1
      fi
      REF=$2
      shift
      ;;
    --install-dir)
      if [ "$#" -lt 2 ]; then
        printf '%s\n' 'error: --install-dir requires a value' >&2
        exit 1
      fi
      BIN_DIR=$2
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      printf '%s\n' "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

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
  if [ "$RESOLVED_REF" = "main" ] || [ "$RESOLVED_REF" = "master" ]; then
    printf '%s\n' "+ cargo install --git $REPO_URL --branch $RESOLVED_REF $package --locked"
    cargo install --git "$REPO_URL" --branch "$RESOLVED_REF" "$package" --locked
  else
    printf '%s\n' "+ cargo install --git $REPO_URL --tag $RESOLVED_REF $package --locked"
    cargo install --git "$REPO_URL" --tag "$RESOLVED_REF" "$package" --locked
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

download_text() {
  url=$1

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O -
  else
    printf '%s\n' 'error: curl or wget is required to download Memzoi release metadata' >&2
    exit 1
  fi
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

file_sha256() {
  path=$1

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print tolower($1)}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$path" | sed 's/^.*= //' | awk '{print tolower($1)}'
  else
    printf '%s\n' 'error: sha256sum, shasum, or openssl is required to verify downloads' >&2
    exit 1
  fi
}

expected_sha256() {
  manifest=$1

  awk '
    $1 ~ /^[0-9a-fA-F]{64}$/ {
      print tolower($1)
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$manifest"
}

verify_sha256() {
  archive_path=$1
  manifest_path=$2
  expected=$(expected_sha256 "$manifest_path") || {
    printf '%s\n' "error: could not read SHA-256 checksum from $manifest_path" >&2
    exit 1
  }
  actual=$(file_sha256 "$archive_path")

  if [ "$actual" != "$expected" ]; then
    printf '%s\n' 'error: downloaded Memzoi archive checksum did not match' >&2
    printf '%s\n' "expected: $expected" >&2
    printf '%s\n' "actual:   $actual" >&2
    exit 1
  fi
}

resolve_ref() {
  case "$REF" in
    main | master)
      RESOLVED_REF="$REF"
      return
      ;;
    "" | latest)
      release_json=$(download_text "$RELEASE_API_BASE/latest") || {
        printf '%s\n' 'error: could not fetch latest Memzoi release metadata' >&2
        exit 1
      }
      RESOLVED_REF=$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
      if [ -z "$RESOLVED_REF" ]; then
        printf '%s\n' 'error: could not resolve latest Memzoi release tag' >&2
        exit 1
      fi
      ;;
    v*)
      RESOLVED_REF="$REF"
      ;;
    [0-9]*)
      RESOLVED_REF="v$REF"
      ;;
    *)
      RESOLVED_REF="$REF"
      ;;
  esac
}

install_release_binaries() {
  target=$(detect_target) || return 1
  archive="memzoi-$RESOLVED_REF-$target.tar.gz"
  url="$DOWNLOAD_BASE/$RESOLVED_REF/$archive"
  checksum_url="$url.sha256"
  tmp_dir=$(mktemp -d)

  printf '%s\n' "+ download $url"
  if ! download_file "$url" "$tmp_dir/$archive"; then
    rm -rf "$tmp_dir"
    return 1
  fi

  printf '%s\n' "+ download $checksum_url"
  download_file "$checksum_url" "$tmp_dir/$archive.sha256"
  verify_sha256 "$tmp_dir/$archive" "$tmp_dir/$archive.sha256"

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
else
  resolve_ref
  if [ "$RESOLVED_REF" = "main" ] || [ "$RESOLVED_REF" = "master" ]; then
    install_git_package memzoi-cli
    install_git_package memzoi-mcp
  elif ! install_release_binaries; then
    printf '%s\n' "error: no release binary found for $RESOLVED_REF on this platform" >&2
    printf '%s\n' "Try installing from source with: MEMZOI_REF=main sh -c 'curl -fsSL https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh | sh'" >&2
    exit 1
  fi
fi

printf '%s\n' '+ memzoi --version'
"$BIN_DIR/memzoi" --version

printf '%s\n' '+ memzoi-mcp --version'
"$BIN_DIR/memzoi-mcp" --version

if ! command -v memzoi >/dev/null 2>&1; then
  printf '\n%s\n' "Note: $BIN_DIR is not on PATH in this shell."
  printf '%s\n' "Add it with: export PATH=\"$BIN_DIR:\$PATH\""
fi

cat <<'NEXT'

Installed Memzoi.

Next:
  memzoi init
  memzoi doctor
  memzoi quickstart --apply-sample
NEXT
