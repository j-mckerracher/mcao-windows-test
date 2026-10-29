#!/usr/bin/env bash
# tests/release.test.sh - release record and procedure: the root UPSTREAM file's format and git
# tracking, and that docs/RELEASE.md carries every maintainer step the release contract requires.
#
# Checks run against the committed files at the repository root. The UPSTREAM value itself changes
# only during a release, so these assertions pin its format rather than a specific commit.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

UPSTREAM_FILE="$ROOT/UPSTREAM"
RELEASE_DOC="$ROOT/docs/RELEASE.md"

test_upstream_record_is_one_full_sha_without_trailing_newline() {
  [ -f "$UPSTREAM_FILE" ] || fail "UPSTREAM is missing from the repository root"
  local bytes value
  bytes=$(wc -c < "$UPSTREAM_FILE" | tr -d ' ')
  assert_equals 40 "$bytes" "UPSTREAM must be exactly 40 bytes with no trailing newline or extra text"
  value=$(cat "$UPSTREAM_FILE")
  assert_equals 40 "${#value}" "UPSTREAM must hold one full 40-character commit SHA"
  case "$value" in
    *[!0-9a-f]*) fail "UPSTREAM must hold lowercase hexadecimal digits only" ;;
  esac
  pass "UPSTREAM holds one full lowercase commit SHA with no trailing newline"
}

test_upstream_record_is_tracked_in_git() {
  git -C "$ROOT" ls-files --error-unmatch -- UPSTREAM >/dev/null 2>&1 \
    || fail "UPSTREAM must be tracked in git, not ignored or untracked"
  pass "UPSTREAM is tracked in git"
}

test_release_doc_carries_required_maintainer_steps() {
  [ -f "$RELEASE_DOC" ] || fail "docs/RELEASE.md is missing"
  local required
  for required in \
    "git remote add upstream https://github.com/kunchenguid/firstmate" \
    "UPSTREAM" \
    "printf '%s'" \
    "git commit -m \"Release vX.Y.Z: upstream <short-SHA>\"" \
    "git tag -a vX.Y.Z" \
    "v0.1.0" \
    "git push origin main" \
    "git push origin vX.Y.Z" \
    "MOA_VERSION" \
    "ai-local-setup" \
    "## Rollback" \
    "PATCH" \
    "MINOR" \
    "MAJOR"; do
    assert_grep "$required" "$RELEASE_DOC" "docs/RELEASE.md lacks required step: $required"
  done
  pass "docs/RELEASE.md carries the upstream remote, UPSTREAM update, tagging, push order, notice, rollback, and versioning steps"
}

test_upstream_record_is_one_full_sha_without_trailing_newline
test_upstream_record_is_tracked_in_git
test_release_doc_carries_required_maintainer_steps
