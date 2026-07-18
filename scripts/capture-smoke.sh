#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DEFAULT_TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
if [[ "$DEFAULT_TARGET_DIR" != /* ]]; then
  DEFAULT_TARGET_DIR="$ROOT/$DEFAULT_TARGET_DIR"
fi
BIN_DIR=${1:-"$DEFAULT_TARGET_DIR/debug"}
if [[ "$BIN_DIR" != /* ]]; then
  BIN_DIR="$PWD/$BIN_DIR"
fi
MEMZOI_BIN=${MEMZOI_BIN:-"$BIN_DIR/memzoi"}
MEMZOI_MCP_BIN=${MEMZOI_MCP_BIN:-"$BIN_DIR/memzoi-mcp"}

if [[ "$MEMZOI_BIN" != /* ]]; then
  MEMZOI_BIN="$PWD/$MEMZOI_BIN"
fi
if [[ "$MEMZOI_MCP_BIN" != /* ]]; then
  MEMZOI_MCP_BIN="$PWD/$MEMZOI_MCP_BIN"
fi

for binary in "$MEMZOI_BIN" "$MEMZOI_MCP_BIN"; do
  if [[ ! -x "$binary" ]]; then
    printf 'capture smoke requires an executable binary: %s\n' "$binary" >&2
    exit 1
  fi
done

TMP=$(mktemp -d "${TMPDIR:-/tmp}/memzoi-capture-smoke-XXXXXX")
cleanup() {
  if [[ "${KEEP_MEMZOI_SMOKE:-0}" == "1" ]]; then
    printf 'kept capture smoke repo: %s\n' "$TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

export MEMZOI_HOME="$TMP/runtime-home"
mkdir -p "$TMP/repo/notes" "$TMP/output"
git -C "$TMP/repo" init -q 2>/dev/null || mkdir -p "$TMP/repo/.git"

cat >"$TMP/repo/notes/capture.md" <<'MARKDOWN'
# Capture smoke

## Procedure: Verify packaged capture binaries

Run the explicit source through both CLI and MCP planning surfaces.
MARKDOWN

"$MEMZOI_BIN" --version >"$TMP/output/memzoi-version.txt"
"$MEMZOI_MCP_BIN" --version >"$TMP/output/memzoi-mcp-version.txt"

cd "$TMP/repo"
"$MEMZOI_BIN" init --json >"$TMP/output/init.json"

CAPTURE_SMOKE_HOME="$MEMZOI_HOME" \
CAPTURE_SMOKE_STATE="$TMP/output/state-before.json" \
python3 - <<'PY'
import json
import os
import sqlite3

databases = []
for root, _, files in os.walk(os.environ['CAPTURE_SMOKE_HOME']):
    if 'shared.db' in files:
        databases.append(os.path.join(root, 'shared.db'))
assert len(databases) == 1, databases
connection = sqlite3.connect(databases[0])
counts = {
    table: connection.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0]
    for table in ['memory_record', 'origin_outcome', 'proposal', 'event_log']
}
with open(os.environ['CAPTURE_SMOKE_STATE'], 'w') as fh:
    json.dump(counts, fh, sort_keys=True)
PY

"$MEMZOI_BIN" capture plan \
  --source notes/capture.md \
  --source-id capture-smoke \
  --json >"$TMP/output/cli-plan.json"

CAPTURE_SMOKE_OUT="$TMP/output" python3 - <<'PY'
import json
import os

out = os.environ['CAPTURE_SMOKE_OUT']
with open(os.path.join(out, 'cli-plan.json')) as fh:
    plan = json.load(fh)
assert plan['schema'] == 'memzoi/capture-plan'
assert plan['status'] == 'ready'
assert plan['data_class'] == 'private'
assert len(plan['candidates']) == 1
candidate = plan['candidates'][0]
assert candidate['memory']['type'] == 'procedure'
assert candidate['classification']['destination'] == 'needs_review'
assert candidate['classification']['sensitivity'] == 'unknown'
assert candidate['evidence'][0]['locator']['path'] == 'notes/capture.md'
PY

CAPTURE_SMOKE_HOME="$MEMZOI_HOME" \
CAPTURE_SMOKE_BEFORE="$TMP/output/state-before.json" \
python3 - <<'PY'
import json
import os
import sqlite3

databases = []
for root, _, files in os.walk(os.environ['CAPTURE_SMOKE_HOME']):
    if 'shared.db' in files:
        databases.append(os.path.join(root, 'shared.db'))
assert len(databases) == 1, databases
connection = sqlite3.connect(databases[0])
after = {
    table: connection.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0]
    for table in ['memory_record', 'origin_outcome', 'proposal', 'event_log']
}
with open(os.environ['CAPTURE_SMOKE_BEFORE']) as fh:
    before = json.load(fh)
assert after == before, (before, after)
PY

CAPTURE_SMOKE_MCP_BIN="$MEMZOI_MCP_BIN" \
CAPTURE_SMOKE_OUT="$TMP/output" \
CAPTURE_SMOKE_REPO="$TMP/repo" \
python3 - <<'PY'
import json
import os
import queue
import subprocess
import threading
import time

command = [
    os.environ['CAPTURE_SMOKE_MCP_BIN'],
    '--project-root',
    os.environ['CAPTURE_SMOKE_REPO'],
]
process = subprocess.Popen(
    command,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1,
)
assert process.stdin is not None
assert process.stdout is not None
assert process.stderr is not None

lines = queue.Queue()

def read_stdout():
    for line in process.stdout:
        lines.put(line)

reader = threading.Thread(target=read_stdout, daemon=True)
reader.start()

messages = [
    {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {}},
    {'jsonrpc': '2.0', 'method': 'notifications/initialized', 'params': {}},
    {
        'jsonrpc': '2.0',
        'id': 2,
        'method': 'tools/call',
        'params': {
            'name': 'plan_capture',
            'arguments': {
                'schema': 'memzoi/capture-request',
                'sources': [{
                    'source_id': 'capture-smoke',
                    'locator': {'kind': 'project_path', 'path': 'notes/capture.md'},
                    'media_type': 'text/markdown',
                }],
                'extractor': {'profile': 'markdown-deterministic'},
            },
        },
    },
]
responses = {}
try:
    for message in messages:
        process.stdin.write(json.dumps(message, separators=(',', ':')) + '\n')
    process.stdin.flush()

    deadline = time.monotonic() + 30
    while 1 not in responses or 2 not in responses:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('timed out waiting for MCP capture response id=2')
        try:
            line = lines.get(timeout=remaining)
        except queue.Empty as error:
            raise TimeoutError('timed out waiting for MCP capture response id=2') from error
        response = json.loads(line)
        if response.get('id') in {1, 2}:
            responses[response['id']] = response
finally:
    try:
        process.stdin.close()
    except BrokenPipeError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)

stderr = process.stderr.read()
assert process.returncode == 0, stderr
ordered = [responses[1], responses[2]]
path = os.path.join(os.environ['CAPTURE_SMOKE_OUT'], 'mcp-plan.jsonl')
with open(path, 'w') as fh:
    for response in ordered:
        fh.write(json.dumps(response, separators=(',', ':')) + '\n')

assert ordered[0]['result']['serverInfo']['name'] == 'memzoi'
result = ordered[1]['result']
assert result['isError'] is True
assert result['content'][0]['text'] == (
    'private capture plans are not available to this MCP client'
)
assert 'structuredContent' not in result
PY

CAPTURE_SMOKE_HOME="$MEMZOI_HOME" \
CAPTURE_SMOKE_BEFORE="$TMP/output/state-before.json" \
python3 - <<'PY'
import json
import os
import sqlite3

databases = []
for root, _, files in os.walk(os.environ['CAPTURE_SMOKE_HOME']):
    if 'shared.db' in files:
        databases.append(os.path.join(root, 'shared.db'))
assert len(databases) == 1, databases
connection = sqlite3.connect(databases[0])
after = {
    table: connection.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0]
    for table in ['memory_record', 'origin_outcome', 'proposal', 'event_log']
}
with open(os.environ['CAPTURE_SMOKE_BEFORE']) as fh:
    before = json.load(fh)
assert after == before, (before, after)
PY

printf 'capture smoke OK: %s, %s\n' "$MEMZOI_BIN" "$MEMZOI_MCP_BIN"
