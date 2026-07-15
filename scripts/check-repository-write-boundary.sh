#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

scan_write_primitives() {
  local scan_root="$1"
  local output="$2"
  : >"$output"
  while IFS= read -r file; do
    while IFS='|' read -r primitive pattern; do
      count="$(grep -Eo "$pattern" "$file" | wc -l | tr -d ' ' || true)"
      if [ "$count" -gt 0 ]; then
        printf '%s|%s|%s\n' "$file" "$primitive" "$count" >>"$output"
      fi
    done <<'PATTERNS'
fs::write|(std::)?fs::write[[:space:]]*\(
fs::rename|(std::)?fs::rename[[:space:]]*\(
fs::hard_link|(std::)?fs::hard_link[[:space:]]*\(
fs::copy|(std::)?fs::copy[[:space:]]*\(
fs::remove_file|(std::)?fs::remove_file[[:space:]]*\(
fs::remove_dir_all|(std::)?fs::remove_dir_all[[:space:]]*\(
fs::remove_dir|(std::)?fs::remove_dir[[:space:]]*\(
fs::create_dir_all|(std::)?fs::create_dir_all[[:space:]]*\(
fs::create_dir|(std::)?fs::create_dir[[:space:]]*\(
fs::set_permissions|(std::)?fs::set_permissions[[:space:]]*\(
OpenOptions::constructor|OpenOptions::(new|default)[[:space:]]*\(
File::create|File::create[[:space:]]*\(
File::options|File::options[[:space:]]*\(
.write_all|\.write_all[[:space:]]*\(
.write_fmt|\.write_fmt[[:space:]]*\(
.set_len|\.set_len[[:space:]]*\(
open-write-mode|\.(write|append|truncate|create|create_new)[[:space:]]*\([[:space:]]*true[[:space:]]*\)
openat|(^|[^[:alnum:]_])openat[[:space:]]*\(
openat2|(^|[^[:alnum:]_])openat2[[:space:]]*\(
unlinkat|(^|[^[:alnum:]_])unlinkat[[:space:]]*\(
unlink|(^|[^[:alnum:]_])unlink[[:space:]]*\(
rmdir|(^|[^[:alnum:]_])rmdir[[:space:]]*\(
mkdirat|(^|[^[:alnum:]_])mkdirat[[:space:]]*\(
mkdir|(^|[^[:alnum:]_])mkdir[[:space:]]*\(
renameat_with|(^|[^[:alnum:]_])renameat_with[[:space:]]*\(
renameat2|(^|[^[:alnum:]_])renameat2[[:space:]]*\(
renameat|(^|[^[:alnum:]_])renameat[[:space:]]*\(
linkat|(^|[^[:alnum:]_])linkat[[:space:]]*\(
symlinkat|(^|[^[:alnum:]_])symlinkat[[:space:]]*\(
PATTERNS
  done < <(find "$scan_root" -type f -path '*/src/*.rs' | sort)
}

scan_mutation_imports() {
  local scan_root="$1"
  local output="$2"
  : >"$output"
  while IFS= read -r file; do
    # Alias imports hide the spelling that the primitive and entry-point audits
    # intentionally inventory. Accumulate complete `use` statements so rustfmt's
    # multiline form cannot evade this guard.
    awk -v file="$file" '
      BEGIN {
        mutation_symbol_count = split("write rename hard_link copy remove_file remove_dir_all remove_dir create_dir_all create_dir set_permissions OpenOptions File open openat openat2 unlinkat unlink rmdir mkdirat mkdir renameat_with renameat2 renameat linkat symlinkat", mutation_symbols, " ")
      }

      function inspect(statement, line, normalized, symbol_index, symbol, pattern) {
        normalized = statement
        gsub(/[[:space:]]+/, " ", normalized)

        if (normalized ~ /(^|[^A-Za-z0-9_])(write|rename|hard_link|copy|remove_file|remove_dir_all|remove_dir|create_dir_all|create_dir|set_permissions|OpenOptions|File|open|openat|openat2|unlinkat|unlink|rmdir|mkdirat|mkdir|renameat_with|renameat2|renameat|linkat|symlinkat)[ ]+as[ ]+[_A-Za-z][_A-Za-z0-9]*/) {
          print file "|" line "|mutation-primitive-alias"
        }

        if (normalized ~ /(^|[^A-Za-z0-9_])(std|rustix)::fs[ ]+as[ ]+[_A-Za-z][_A-Za-z0-9]*/ ||
            normalized ~ /(^|[^A-Za-z0-9_])(std|rustix)::[{][^;}]*fs[ ]+as[ ]+[_A-Za-z][_A-Za-z0-9]*/ ||
            normalized ~ /(^|[^A-Za-z0-9_])(std|rustix)::.*fs::[{][^;}]*self[ ]+as[ ]+[_A-Za-z][_A-Za-z0-9]*/) {
          print file "|" line "|filesystem-module-alias"
        }

        if (normalized ~ /(^|[^A-Za-z0-9_])(std|rustix)::.*fs::([*]|[{][^;}]*[*])/) {
          print file "|" line "|filesystem-glob-import"
        }

        # A direct import can make a mutator call look like an unrelated local
        # function (for example `remove_file(path)`). Inventory these imports
        # without line numbers so ordinary source movement does not churn the
        # reviewable allowlist.
        if (normalized !~ /(^|[^A-Za-z0-9_])(std|rustix)::.*fs/) {
          return
        }
        for (symbol_index = 1; symbol_index <= mutation_symbol_count; symbol_index++) {
          symbol = mutation_symbols[symbol_index]
          pattern = "(^|[^A-Za-z0-9_])" symbol "[ ]*([,};]|$)"
          if (normalized ~ pattern) {
            print file "|" symbol "|canonical-mutation-import"
          }
        }
      }

      {
        source = $0
        sub(/\/\/.*/, "", source)
        if (!collecting && source ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?use[[:space:]]/) {
          collecting = 1
          start_line = NR
          statement = source
        } else if (collecting) {
          statement = statement "\n" source
        }

        if (collecting && source ~ /;/) {
          inspect(statement, start_line)
          collecting = 0
          start_line = 0
          statement = ""
        }
      }
    ' "$file" >>"$output"
  done < <(find "$scan_root" -type f -path '*/src/*.rs' | sort)
}

scan_repository_io_entry_points() {
  local file="$1"
  local output="$2"
  # Emit a reviewable manifest of direct and delegated mutation surfaces:
  # function|visibility|mutation kinds|authorization parameter|verification call.
  awk '
    function flush() {
      if (name == "") {
        return
      }
      mutation = ""
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?(write|copy)[[:space:]]*[(]/ ||
          body ~ /OpenOptions::(new|default)[[:space:]]*[(]/ ||
          body ~ /File::(create|options)[[:space:]]*[(]/ ||
          body ~ /\.(write_all|write_fmt|set_len)[[:space:]]*[(]/ ||
          (body ~ /openat2?[[:space:]]*[(]/ &&
           body ~ /OFlags::(WRONLY|RDWR|CREATE|TRUNC|APPEND)/)) {
        mutation = "content"
      }
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?(remove_file|remove_dir|remove_dir_all)[[:space:]]*[(]/ ||
          body ~ /(^|[^A-Za-z0-9_])(unlinkat|unlink|rmdir)[[:space:]]*[(]/) {
        mutation = mutation == "" ? "delete" : mutation ",delete"
      }
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?(create_dir|create_dir_all)[[:space:]]*[(]/ ||
          body ~ /(^|[^A-Za-z0-9_])(mkdirat|mkdir)[[:space:]]*[(]/) {
        mutation = mutation == "" ? "directory" : mutation ",directory"
      }
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?rename[[:space:]]*[(]/ ||
          body ~ /(^|[^A-Za-z0-9_])(renameat|renameat2|renameat_with)[[:space:]]*[(]/) {
        mutation = mutation == "" ? "rename" : mutation ",rename"
      }
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?hard_link[[:space:]]*[(]/ ||
          body ~ /(^|[^A-Za-z0-9_])(linkat|symlinkat)[[:space:]]*[(]/) {
        mutation = mutation == "" ? "link" : mutation ",link"
      }
      if (body ~ /(^|[^A-Za-z0-9_])((std::)?fs::)?set_permissions[[:space:]]*[(]/) {
        mutation = mutation == "" ? "metadata" : mutation ",metadata"
      }
      visibility = "private"
      if (signature ~ /^pub\(crate\)[[:space:]]/) {
        visibility = "pub(crate)"
      } else if (signature ~ /^pub[[:space:]]/) {
        visibility = "pub"
      }
      if (body ~ /(create_authorized_repository_projection|remove_pinned_named_file|remove_created_repository_file|cleanup_created_file|rollback_repository_batch)[[:space:]]*[(]/) {
        mutation = mutation == "" ? "delegated" : mutation ",delegated"
      }
      if (mutation == "" && visibility != "private" &&
          name ~ /^(apply|backup|create|delete|install|link|mkdir|move|persist|quarantine|remove|rename|replace|restore|unlink|write)/) {
        mutation = "delegated"
      }
      if (mutation == "") {
        return
      }
      authorization = signature ~ /authorization:[[:space:]]*&AuthorizedRepositoryWriteBatch/ ? "authorized" : "unauthorized"
      verification = body ~ /verify_repository_batch(_for_identity)?[[:space:]]*[(]/ ? "verified" : "unverified"
      print name "|" visibility "|" mutation "|" authorization "|" verification
    }
    /^(pub(\([^)]*\))?[[:space:]]+)?((async|unsafe)[[:space:]]+)?fn [A-Za-z0-9_]+/ {
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

has_unsafe_exported_repository_io_entry_point() {
  awk -F'|' '
    $2 != "private" && ($4 != "authorized" || $5 != "verified") { found = 1 }
    END { exit found ? 0 : 1 }
  ' "$1"
}

actual_writers="$(mktemp)"
fixture_writers="$(mktemp)"
fixture_mutation_writers="$(mktemp)"
mutation_imports="$(mktemp)"
canonical_mutation_imports="$(mktemp)"
fixture_mutation_imports="$(mktemp)"
io_entry_points="$(mktemp)"
fixture_io_entry_points="$(mktemp)"
fixture_delete_entry_points="$(mktemp)"
trap 'rm -f "$actual_writers" "$fixture_writers" "$fixture_mutation_writers" "$mutation_imports" "$canonical_mutation_imports" "$fixture_mutation_imports" "$io_entry_points" "$fixture_io_entry_points" "$fixture_delete_entry_points"' EXIT

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

# Keep coverage for mutation families that previously escaped the inventory.
scan_write_primitives \
  scripts/fixtures/repository-write-boundary/unsafe-delete \
  "$fixture_mutation_writers"
for primitive in \
  'fs::remove_file' \
  'fs::remove_dir' \
  'fs::remove_dir_all' \
  'fs::create_dir' \
  'fs::create_dir_all' \
  'OpenOptions::constructor' \
  'File::options' \
  'open-write-mode' \
  'openat' \
  'unlinkat' \
  'mkdirat' \
  'renameat' \
  'renameat_with'; do
  if ! awk -F'|' -v primitive="$primitive" '$2 == primitive { found = 1 } END { exit found ? 0 : 1 }' \
    "$fixture_mutation_writers"; then
    echo "repository write boundary self-test missed mutation primitive: $primitive" >&2
    exit 1
  fi
done

# Aliases and glob imports obscure the spellings audited above, so reject them.
# Direct canonical imports remain visible in a stable, reviewable manifest.
scan_mutation_imports crates "$mutation_imports"
if grep -E '\|(mutation-primitive-alias|filesystem-module-alias|filesystem-glob-import)$' \
  "$mutation_imports" >&2; then
  echo "repository mutation imports must not alias or glob-import mutation surfaces" >&2
  exit 1
fi
awk -F'|' '$3 == "canonical-mutation-import" { print $1 "|" $2 }' \
  "$mutation_imports" | sort -u >"$canonical_mutation_imports"
if ! diff -u scripts/repository-mutation-imports.allow "$canonical_mutation_imports"; then
  echo "canonical repository mutation import inventory changed; review direct imports and update the allowlist" >&2
  exit 1
fi

scan_mutation_imports \
  scripts/fixtures/repository-write-boundary/unsafe-delete \
  "$fixture_mutation_imports"
for import_kind in \
  'mutation-primitive-alias' \
  'filesystem-module-alias' \
  'filesystem-glob-import' \
  'canonical-mutation-import'; do
  if ! awk -F'|' -v import_kind="$import_kind" '$3 == import_kind { found = 1 } END { exit found ? 0 : 1 }' \
    "$fixture_mutation_imports"; then
    echo "repository write boundary self-test missed mutation import kind: $import_kind" >&2
    exit 1
  fi
done

scan_repository_io_entry_points crates/memzoi-core/src/repository_io.rs "$io_entry_points"
if grep -Eq '^impl([[:space:]]|<)' crates/memzoi-core/src/repository_io.rs; then
  echo "repository I/O mutation entry points must remain auditable top-level functions" >&2
  exit 1
fi
if ! diff -u scripts/repository-io-mutation-entrypoints.allow "$io_entry_points"; then
  echo "repository I/O mutation surface changed; exported entry points require authorization and internal helpers require review" >&2
  exit 1
fi
if has_unsafe_exported_repository_io_entry_point "$io_entry_points"; then
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
if ! has_unsafe_exported_repository_io_entry_point "$fixture_io_entry_points"; then
  echo "repository write boundary self-test did not reject the unsafe repository I/O writer fixture" >&2
  exit 1
fi

# Deletion and directory mutation are repository writes too. An unauthenticated
# deleter inside the centralized I/O module must fail the same entry-point audit.
scan_repository_io_entry_points \
  scripts/fixtures/repository-write-boundary/unsafe-delete/src/repository_io.rs \
  "$fixture_delete_entry_points"
if ! grep -Fq '|delete|unauthorized|unverified' "$fixture_delete_entry_points"; then
  echo "repository write boundary self-test did not detect the unsafe repository deletion fixture" >&2
  exit 1
fi
if ! grep -Fq '|directory|unauthorized|unverified' "$fixture_delete_entry_points"; then
  echo "repository write boundary self-test did not detect the unsafe repository directory fixture" >&2
  exit 1
fi
if ! has_unsafe_exported_repository_io_entry_point "$fixture_delete_entry_points"; then
  echo "repository write boundary self-test did not reject the unsafe repository deletion fixture" >&2
  exit 1
fi

if grep -RFn \
  --include='*.rs' \
  --exclude='service.rs' \
  --exclude='tests.rs' \
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
grep -Fq 'authorization: &AuthorizedRepositoryProjectionBatch' \
  crates/memzoi-core/src/service/repository_mutation.rs
grep -Fq 'repository_io::verify_repository_batch' \
  crates/memzoi-core/src/service/repository_mutation.rs
grep -Fq 'expected_route: RepositoryWriteRoute' crates/memzoi-core/src/repository_io.rs
grep -Fq 'pub const ALL: [Self; 14]' crates/memzoi-core/src/repository_write_safety/policy.rs

echo "repository write boundary structural checks passed"
