#!/usr/bin/env bash
# Unified pre-PR gate: one command that runs every quality check this repo
# enforces, so /prep-pr, the pre-push hook, and humans all run the same set.
# Wired into cc-orchestrator's gate-runner.py via .gates.toml ([prep_pr]).
#
# Order mirrors CI (.github/workflows/ci.yml): non-cargo pre-commit hooks, then
# per-crate cargo fmt/clippy/test, then Python lint + unit tests.
set -euo pipefail

cd "$(dirname "$0")/.." || exit 1 # repo root, regardless of caller's cwd

# Rust toolchain may live in ~/.cargo/env on minimal shells (matches the
# pre-commit hooks' own sourcing).
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

section() { printf '\n========== %s ==========\n' "$1"; }

# 1) Non-cargo pre-commit hooks (markdownlint, gitleaks, actionlint, yaml,
#    whitespace). The cargo hooks are skipped here and run explicitly below so
#    each crate's failure is attributable -- this matches the CI split.
section "pre-commit (non-cargo hooks)"
SKIP="cargo-fmt,cargo-clippy,cargo-check,cargo-fmt-rabsody,cargo-clippy-rabsody,cargo-check-rabsody" \
  pre-commit run --all-files

# 2) Rust: one pass per workspace crate.
for crate in smpr rabsody; do
  section "cargo ($crate)"
  (
    cd "$crate" || exit 1
    cargo fmt -- --check
    cargo clippy -- -D warnings
    cargo test --verbose -- --test-threads=1
  )
done

# 3) Python (loose scripts under tools/). ruff is skip-if-absent; unittest
#    always runs. The scripts use sys.path-relative sibling imports, so each
#    test directory must be its own discovery start-dir -- a top-level
#    `discover -s tools` finds nothing.
section "python"
if [ ! -d tools ]; then
  echo "tools/ not found; skipping Python checks"
elif ! command -v ruff >/dev/null 2>&1; then
  echo "ruff not installed; skipping lint"
else
  ruff check tools/
fi

test_dirs=""
[ -d tools ] && test_dirs=$(find tools -name 'test_*.py' -exec dirname {} \; 2>/dev/null | sort -u)
if [ -z "$test_dirs" ]; then
  echo "no Python unittest files found; skipping tests"
else
  while IFS= read -r d; do
    echo "--- unittest: $d ---"
    python3 -m unittest discover -s "$d" -p 'test_*.py'
  done <<<"$test_dirs"
fi

section "gate: all checks passed"
