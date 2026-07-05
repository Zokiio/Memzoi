#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TMP=$(mktemp -d "${TMPDIR:-/tmp}/memzoi-onboarding-XXXXXX")
cleanup() {
  if [[ "${KEEP_MEMZOI_SMOKE:-0}" == "1" ]]; then
    printf 'kept onboarding smoke repo: %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

cd "$ROOT"
cargo build --workspace

cd "$TMP"
git init -q 2>/dev/null || mkdir -p .git

OUT="$TMP/output"
mkdir -p "$OUT"

cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- init --json >"$OUT/init.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- doctor --json >"$OUT/doctor.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- quickstart --apply-sample --json >"$OUT/quickstart.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- search quickstart --json >"$OUT/search.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- context --task "remember quickstart" --json >"$OUT/context.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- precheck --command "rm -rf .memzoi" --json >"$OUT/precheck.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- export agents-md --json >"$OUT/export.json"
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-cli --bin memzoi -- mcp config --project-root . >"$OUT/mcp-config.json"

MEMZOI_ONBOARDING_OUT="$OUT" python3 - <<'PY'
import json
import os
out = os.environ['MEMZOI_ONBOARDING_OUT']
for name in ['init', 'doctor', 'quickstart', 'search', 'context', 'precheck', 'export', 'mcp-config']:
    path = os.path.join(out, f'{name}.json')
    with open(path) as fh:
        data = json.load(fh)
    assert data, path
assert json.load(open(os.path.join(out, 'doctor.json')))['status'] in {'ok', 'warning'}
assert json.load(open(os.path.join(out, 'quickstart.json')))['search_count'] >= 1
assert json.load(open(os.path.join(out, 'mcp-config.json')))['mcpServers']['memzoi']['command'] == 'memzoi-mcp'
PY

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | cargo run -q --manifest-path "$ROOT/Cargo.toml" -p memzoi-mcp --bin memzoi-mcp -- --project-root "$TMP" \
  >"$OUT/mcp-smoke.jsonl"

MEMZOI_ONBOARDING_OUT="$OUT" python3 - <<'PY'
import json
import os
lines = open(os.path.join(os.environ['MEMZOI_ONBOARDING_OUT'], 'mcp-smoke.jsonl')).read().splitlines()
assert len(lines) == 2, lines
responses = [json.loads(line) for line in lines]
assert responses[0]['result']['serverInfo']['name'] == 'memzoi'
tools = {tool['name'] for tool in responses[1]['result']['tools']}
expected = {'search_memory', 'build_context_pack', 'propose_memory', 'precheck_path', 'precheck_action', 'precheck_command'}
assert tools == expected, tools
print('onboarding smoke OK')
PY
