#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

scan_write_primitives() {
  local scan_root="$1"
  local output="$2"
  : >"$output"
  while IFS= read -r file; do
    for primitive in \
      'fs::write' \
      'fs::rename' \
      'fs::hard_link' \
      'fs::copy' \
      'OpenOptions::new' \
      'File::create' \
      '.write_all'; do
      count="$(grep -Fo "$primitive" "$file" | wc -l | tr -d ' ' || true)"
      if [ "$count" -gt 0 ]; then
        printf '%s|%s|%s\n' "$file" "$primitive" "$count" >>"$output"
      fi
    done
  done < <(find "$scan_root" -type f -path '*/src/*.rs' | sort)
}

scan_repository_io_entry_points() {
  local file="$1"
  local output="$2"
  awk '
    function flush() {
      if (name == "" || body !~ /(fs::write|fs::rename|fs::hard_link|fs::copy|OpenOptions::new|File::create|\.write_all)/) {
        return
      }
      authorization = signature ~ /authorization: *&AuthorizedRepositoryWriteBatch/ ? "authorized" : "unauthorized"
      verification = body ~ /verify_repository_batch/ ? "verified" : "unverified"
      print name "|" authorization "|" verification
    }
    /^(pub(\([^)]*\))? )?fn [A-Za-z0-9_]+/ {
      flush()
      name = $0
      sub(/^.*fn /, "", name)
      sub(/\(.*/, "", name)
      signature = $0
      body = ""
      next
    }
    /^#\[cfg\(test\)\]/ {
      flush()
      name = ""
      signature = ""
      body = ""
      next
    }
    name != "" {
      if (signature !~ /\{/) {
        signature = signature "\n" $0
      }
      body = body "\n" $0
    }
    END { flush() }
  ' "$file" >"$output"
}

actual_writers="$(mktemp)"
fixture_writers="$(mktemp)"
io_entry_points="$(mktemp)"
fixture_io_entry_points="$(mktemp)"
trap 'rm -f "$actual_writers" "$fixture_writers" "$io_entry_points" "$fixture_io_entry_points"' EXIT

scan_write_primitives crates "$actual_writers"
if ! diff -u scripts/repository-write-primitives.allow "$actual_writers"; then
  echo "repository mutation primitive inventory changed; centralize the writer or review and update the allowlist" >&2
  exit 1
fi

# An unsafe writer in another production crate must be detected.
scan_write_primitives scripts/fixtures/repository-write-boundary/unsafe-cli "$fixture_writers"
if diff -u /dev/null "$fixture_writers" >/dev/null; then
  echo "repository write boundary self-test did not detect the unsafe CLI writer fixture" >&2
  exit 1
fi

scan_repository_io_entry_points crates/memzoi-core/src/repository_io.rs "$io_entry_points"
if grep -Eq '^impl([[:space:]]|<)' crates/memzoi-core/src/repository_io.rs; then
  echo "repository I/O mutation entry points must remain auditable top-level functions" >&2
  exit 1
fi
if ! diff -u scripts/repository-io-mutation-entrypoints.allow "$io_entry_points"; then
  echo "repository I/O mutation entry points changed; every writer must accept and verify authorization" >&2
  exit 1
fi
if grep -Ev '\|authorized\|verified$' "$io_entry_points" >/dev/null; then
  echo "repository I/O contains a mutation entry point without capability verification" >&2
  exit 1
fi

# An unauthenticated writer inside the centralized I/O module must also fail.
scan_repository_io_entry_points \
  scripts/fixtures/repository-write-boundary/unsafe-io/repository_io.rs \
  "$fixture_io_entry_points"
if ! grep -Fq '|unauthorized|unverified' "$fixture_io_entry_points"; then
  echo "repository write boundary self-test did not detect the unsafe repository I/O writer fixture" >&2
  exit 1
fi

if grep -RFn \
  --include='*.rs' \
  --exclude='service.rs' \
  --exclude='okf.rs' \
  'okf::create_okf_proposal_file(' \
  crates/memzoi-core/src >/dev/null; then
  echo "deprecated direct OKF proposal writer remains reachable" >&2
  exit 1
fi
awk '
  previous == "#[cfg(test)]" && index($0, "pub(crate) fn create_okf_proposal_file") == 1 {
    found = 1
  }
  { previous = $0 }
  END { exit found ? 0 : 1 }
' crates/memzoi-core/src/okf.rs

grep -Fq 'authorization: &AuthorizedRepositoryWriteBatch' crates/memzoi-core/src/repository_io.rs
grep -Fq 'authorization: &AuthorizedRepositoryProjectionBatch' crates/memzoi-core/src/service.rs
grep -Fq 'repository_io::verify_repository_batch' crates/memzoi-core/src/service.rs
grep -Fq 'expected_route: RepositoryWriteRoute' crates/memzoi-core/src/repository_io.rs
grep -Fq 'pub const ALL: [Self; 14]' crates/memzoi-core/src/repository_write_safety/policy.rs

echo "repository write boundary structural checks passed"
