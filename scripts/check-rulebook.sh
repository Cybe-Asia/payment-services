#!/usr/bin/env bash
# Rulebook ratchet (docs/engineering/RUST_RULES_OF_THUMB.md §31): counts known violations in src/
# and fails when any count rises above scripts/rulebook-baseline.txt. Counts may only go down.
# Usage: scripts/check-rulebook.sh [--write-baseline]
# shortcut: the same script is copied into each Rust service; move it into the shared service crate's tooling once it exists.
set -euo pipefail
cd "$(dirname "$0")/.."
baseline=scripts/rulebook-baseline.txt

# Non-test Rust source: skip test files and everything after the first #[cfg(test)].
prod_src() {
  find -H src -name '*.rs' ! -name '*test*.rs' ! -path '*/tests/*' -print0 |
    xargs -0 awk 'FNR==1{skip=0} /#\[cfg\(test\)\]/{skip=1} !skip{print FILENAME": "$0}'
}

count() { prod_src | grep -E -c "$1" || true; }

counts() {
  # §21: secrets never have a fallback value
  echo "secretFallback=$(count 'env::var\("[A-Z_]*(SECRET|PASSWORD|TOKEN|KEY)[A-Z_]*"\)[^;]*unwrap_or')"
  # §21: environment read only in the config module
  echo "envReadOutsideConfig=$(prod_src | grep -E 'env::var\(' | grep -v -E '^src/(config|main\.rs)' | wc -l | tr -d ' ')"
  # §11: no driver or client error text in responses
  echo "errorTextToClient=$(count '(ApiResponse::[a-z_]+|error_response|failure)\(&?[a-z_]*\.to_string\(\)')"
  # §11: typed errors in new code
  echo "resultString=$(count 'Result<[^;]*, ?String>')"
  # §11: no unwrap in production paths
  echo "unwrap=$(count '\.unwrap\(\)')"
  # §3: domain code does not import infrastructure
  echo "domainInfraImport=$(prod_src | grep -E '^src/([^:]*/)?domain/' | grep -E -c 'use (axum|neo4rs|sqlx|reqwest)' || true)"
  # §18: no blocking file I/O in async code outside startup
  echo "blockingFs=$(prod_src | grep -v -E '^src/(config|main\.rs)' | grep -E -c 'std::fs::' || true)"
}

current=$(counts)
if [[ "${1:-}" == "--write-baseline" ]]; then
  if [[ -f $baseline ]]; then
    while IFS='=' read -r key value; do
      old=$(grep -E "^$key=" "$baseline" | cut -d= -f2 || true)
      if [[ -n "$old" && "$value" -gt "$old" ]]; then echo "Refusing to raise the baseline for $key ($old -> $value)" >&2; exit 1; fi
    done <<<"$current"
  fi
  echo "$current" >"$baseline"
  echo "Baseline written."
  exit 0
fi

failed=0
while IFS='=' read -r key value; do
  max=$(grep -E "^$key=" "$baseline" 2>/dev/null | cut -d= -f2 || true)
  max=${max:-0}
  if (( value > max )); then echo "✗ $key: $value (baseline $max)" >&2; failed=1
  elif (( value < max )); then echo "↓ $key: $value (baseline $max); run with --write-baseline to lock it in"; fi
done <<<"$current"
if (( failed )); then echo "Rulebook ratchet failed: see docs/engineering/RUST_RULES_OF_THUMB.md §31." >&2; exit 1; fi
echo "Rulebook ratchet passed."
