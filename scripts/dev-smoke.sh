#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- --help >/tmp/memzoi-cli-help.txt
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-mcp --bin memzoi-mcp -- --help >/tmp/memzoi-mcp-help.txt

SMOKE_REPO=$(mktemp -d "${TMPDIR:-/tmp}/memzoi-dev-smoke-XXXXXX")
cleanup() {
  rm -rf "$SMOKE_REPO"
}
trap cleanup EXIT

git -C "$SMOKE_REPO" init -q 2>/dev/null || mkdir -p "$SMOKE_REPO/.git"
(cd "$SMOKE_REPO" && cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- init --json >/tmp/memzoi-dev-smoke-init.json)

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-mcp --bin memzoi-mcp -- --project-root "$SMOKE_REPO" \
  >/tmp/memzoi-mcp-smoke.jsonl

python3 - <<'PY'
import json
lines = open('/tmp/memzoi-mcp-smoke.jsonl').read().splitlines()
assert len(lines) == 2, lines
responses = [json.loads(line) for line in lines]
assert responses[0]['result']['serverInfo']['name'] == 'memzoi'
tools = {tool['name'] for tool in responses[1]['result']['tools']}
expected = {
    'search_memory',
    'build_context_pack',
    'precheck_path',
    'precheck_action',
    'precheck_command',
    'inspect_memory_expiry',
    'plan_capture',
    'plan_maintenance',
}
assert tools == expected, tools
print('MCP smoke OK:', ','.join(sorted(tools)))
PY
