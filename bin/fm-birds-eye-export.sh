#!/usr/bin/env bash
# fm-birds-eye-export.sh - writes one self-contained, metadata-only bird's-eye snapshot.
#
# Usage:
#   fm-birds-eye-export.sh [--root <home-dir>]... [--interval <seconds>] --output <path>
#                          [--now <epoch>] [--fixture-dir <dir>]
#   fm-birds-eye-export.sh -h | --help
#
# The configured home (FM_HOME, else this checkout) is always read first, and each --root
# adds an explicit root, through bin/fm-birds-eye-model.sh. The fm-birds-eye-view.v1
# document is projected through an explicit allowlist and embedded with the dashboard in
# assets/birds-eye-view/. The result is one HTML file that renders offline in the
# dashboard's embedded-snapshot mode: no fetches, no timers, no live indicator.
#
# Excluded from the snapshot: raw pane or worker output, prompts, tool payloads,
# environment, credentials, worktree and report paths, and free-form activity text.
# Free text the dashboard needs (blocker and decision summaries, attention reasons) is
# kept clipped to 160 characters. Root errors become fixed categories, because producer
# stderr can carry environment values.
#
# Embedded JSON escapes <, >, &, U+2028 and U+2029, so hostile metadata cannot close a
# script or style element. The page's Content-Security-Policy admits only the exact inline
# script and style blocks it carries, by SHA-256 hash, and allows no connections.
#
# The one side effect is the producer's own state/secondmate-summary-cache refresh, which
# the model owns. Nothing else is written except the --output file.
#
# --fixture-dir <dir> is test-only and passes through to the model.
# --now <epoch> pins the captured time; it defaults to the current time.
#
# Exit status: 0 when the snapshot is written, 2 on a usage error (including an interval
# below 5 seconds), 1 when the export cannot run.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ASSETS="$SCRIPT_DIR/../assets/birds-eye-view"
MODEL="$SCRIPT_DIR/fm-birds-eye-model.sh"
VIEW_SCHEMA="fm-birds-eye-view.v1"
CLIP=160

usage() {
  cat <<'EOF'
Usage: fm-birds-eye-export.sh [--root <home-dir>]... [--interval <seconds>] --output <path>
                              [--now <epoch>] [--fixture-dir <dir>]
       fm-birds-eye-export.sh -h | --help

Writes one self-contained HTML snapshot of the bird's-eye view. The configured home is
always read; each --root adds an explicit root. The snapshot is metadata only and renders
offline. Exit 0 when written, 2 on a usage error, 1 when the export cannot run.
EOF
}

die() {
  printf '%s\n' "$1" >&2
  exit "${2:-2}"
}

ROOTS=()
INTERVAL=10
OUTPUT=""
NOW=""
FIXTURE_DIR=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --root)
      [ "$#" -ge 2 ] || die "--root needs a directory"
      ROOTS+=("$2"); shift 2 ;;
    --interval)
      [ "$#" -ge 2 ] || die "--interval needs a number of seconds"
      INTERVAL=$2; shift 2 ;;
    --output)
      [ "$#" -ge 2 ] || die "--output needs a path"
      OUTPUT=$2; shift 2 ;;
    --now)
      [ "$#" -ge 2 ] || die "--now needs an epoch"
      NOW=$2; shift 2 ;;
    --fixture-dir)
      [ "$#" -ge 2 ] || die "--fixture-dir needs a directory"
      FIXTURE_DIR=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$OUTPUT" ] || die "--output is required"
[ -n "$NOW" ] || NOW=$(date +%s)

TMP=$(mktemp -d "${TMPDIR:-/tmp}/fm-birds-eye-export.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

# The model validates --interval (refusing below 5 seconds) and --now; its exit status is
# forwarded unchanged so a usage refusal stays a usage refusal.
model_args=(--interval "$INTERVAL" --now "$NOW")
if [ -n "$FIXTURE_DIR" ]; then
  model_args+=(--fixture-dir "$FIXTURE_DIR")
fi
i=0
while [ "$i" -lt "${#ROOTS[@]}" ]; do
  model_args+=(--root "${ROOTS[$i]}")
  i=$((i + 1))
done

rc=0
bash "$MODEL" "${model_args[@]}" >"$TMP/view.json" 2>"$TMP/model.err" || rc=$?
if [ "$rc" -ne 0 ]; then
  cat "$TMP/model.err" >&2
  exit "$rc"
fi
jq -e --arg s "$VIEW_SCHEMA" '.schema == $s' "$TMP/view.json" >/dev/null 2>&1 \
  || die "model output is not a $VIEW_SCHEMA document" 1

# Explicit allowlist. A key survives only when it is listed at its level; anything else,
# at any depth, is dropped. Free text is clipped. Root errors become fixed categories.
read -r -d '' PROJECT <<'JQ' || true
def keep($keys):
  if type == "object" then with_entries(select(.key as $k | $keys | any(. == $k))) else null end;
def clip:
  if type == "string" and length > $clip then .[0:$clip] + "..." else . end;
def maybe(f): if . == null then null else f end;
def safe_link:
  if type == "string" and test("^https://[^\\s<>\"]+$") then . else null end;
def decision_obj:
  maybe(keep(["label","recipient","resolver","key","age_seconds","age_basis","summary","ref"])
    | .summary |= clip);
def entity:
  keep(["key","kind","name","home_id","task_id","parent_key","child_keys","contextual_in",
        "project","role","classification","spawn_gen","prerequisites","state","observation",
        "artifacts","latest_activity","blocker","decision","decisions","execution","timing"])
  | .name |= clip
  | .state |= maybe(keep(["raw","display","source","wait_reason","observed_at"])
      | .raw |= clip | .wait_reason |= clip)
  | .observation |= maybe(keep(["quality","collected_at","source_event_at","root_id",
      "collection_age_seconds","collection_age_basis","source_event_age_seconds",
      "source_event_age_basis","stale"]))
  | .artifacts |= maybe(keep(["branch","pr_url","output_available"])
      | .branch |= clip | .pr_url |= safe_link)
  | .latest_activity |= maybe(keep(["at","reported_by_agent"]))
  | .blocker |= maybe(keep(["reason","age_seconds","age_basis"]) | .reason |= clip)
  | .decision |= decision_obj
  | .decisions |= ((. // []) | map(decision_obj))
  | .execution |= maybe(keep(["harness","requested_model","requested_effort","observed_model",
      "observed_effort","playbook","workflow_stage"]) | map_values(clip))
  | .timing |= maybe(keep(["elapsed_seconds","elapsed_basis","state_seconds","state_basis",
      "observed_state","observed_since","observed_transition"]));
def root_error($r):
  if $r.status == "ok" or $r.status == null then null
  elif $r.status == "malformed" then "producer output was malformed"
  elif $r.status == "unsupported_schema" then "producer schema is not supported"
  elif (($r.error // "") | startswith("not reached")) then "parent root could not be read"
  elif (($r.error // "") | test("timed out")) then "producer timed out"
  elif $r.status == "failed" then "producer could not be read"
  else "root could not be read" end;
def root:
  . as $r
  | keep(["root_id","home_path","source","parent_root_id","status","stale","collected_at",
          "last_good_at","last_good_age_seconds","age_basis"])
  | .error = root_error($r);
def attention_reason($doc; $a):
  # Reasons are rebuilt from fixed wording, never copied from producer text: a failure
  # reason is fixed, and a root reason is the sanitized root category.
  if $a.class == "failure" then "work failed"
  elif $a.class == "observation" and (($a.key // "") | startswith("root:")) then
    ([ ($doc.roots // [])[] | select(("root:" + .root_id) == $a.key) ] | .[0]) as $r
    | if $r == null then "root could not be read"
      else (($r | root | .error) // "last-good records are stale") end
  else ($a.reason | clip) end;
def attention_item($doc):
  . as $a
  | keep(["class","label","priority","key","project","reason","resolver","age_seconds"])
  | .label |= clip
  | .reason = attention_reason($doc; $a);
def history_summary:
  keep(["key","generation","last_state","last_seen_at","historical","parent_key"]);
. as $doc
| {
  schema: .schema,
  generated_at: .generated_at,
  generated_epoch: .generated_epoch,
  refresh: (.refresh | maybe(keep(["interval_seconds","stale_after_seconds"]))),
  counts: .counts,
  roots: ((.roots // []) | map(root)),
  attention: ((.attention // []) | map(attention_item($doc))),
  unsupported: ((.unsupported // []) | map(keep(["category","reason","count"]))),
  history: (.history | maybe(keep(["limit","retained","evicted","limit_reached","restart_disclosure","summaries"])
    | .summaries = ((.summaries // []) | map(history_summary)))),
  entities: ((.entities // []) | map(entity))
}
JQ

jq -c --argjson clip "$CLIP" "$PROJECT" "$TMP/view.json" >"$TMP/snapshot.json" \
  || die "could not project the view document" 1

# Escape the characters that could close or reopen a script element or change its parsing.
# JSON structure never contains these characters, so the escapes stay valid JSON.
printf 'window.BIRDS_EYE_DATA = %s;\n' \
  "$(jq -r '
      tojson
      | gsub("<"; "\\u003c") | gsub(">"; "\\u003e") | gsub("&"; "\\u0026")
      | gsub("\u2028"; "\\u2028") | gsub("\u2029"; "\\u2029")' "$TMP/snapshot.json")" \
  >"$TMP/data.js"

for asset in app.js app.css index.html; do
  [ -f "$ASSETS/$asset" ] || die "missing dashboard asset: $ASSETS/$asset" 1
done
if grep -q '</script' "$ASSETS/app.js" || grep -q '</style' "$ASSETS/app.css"; then
  die "dashboard asset contains a closing script or style tag; refusing to embed" 1
fi
for marker in 'http-equiv="Content-Security-Policy"' '<link rel="stylesheet" href="app.css">' \
    '<script src="app.js"></script>'; do
  count=$(grep -c -F "$marker" "$ASSETS/index.html" || true)
  [ "$count" -eq 1 ] || die "dashboard index.html must contain exactly one: $marker" 1
done

sha256_b64() {
  openssl dgst -sha256 -binary "$1" | openssl base64 -A
}
DATA_HASH=$(sha256_b64 "$TMP/data.js")
APP_HASH=$(sha256_b64 "$ASSETS/app.js")
CSS_HASH=$(sha256_b64 "$ASSETS/app.css")
CSP="default-src 'none'; script-src 'sha256-$DATA_HASH' 'sha256-$APP_HASH'; style-src 'sha256-$CSS_HASH'; img-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'"

while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    *'http-equiv="Content-Security-Policy"'*)
      printf '  <meta http-equiv="Content-Security-Policy" content="%s">\n' "$CSP" ;;
    *'<link rel="stylesheet" href="app.css">'*)
      printf '<style>'
      cat "$ASSETS/app.css"
      printf '</style>\n' ;;
    *'<script src="app.js"></script>'*)
      printf '<script>'
      cat "$TMP/data.js"
      printf '</script>\n<script>'
      cat "$ASSETS/app.js"
      printf '</script>\n' ;;
    *) printf '%s\n' "$line" ;;
  esac
done <"$ASSETS/index.html" >"$TMP/page.html"

mkdir -p "$(dirname "$OUTPUT")"
cp "$TMP/page.html" "$OUTPUT.tmp.$$"
mv -f "$OUTPUT.tmp.$$" "$OUTPUT"

jq -r --arg out "$OUTPUT" '
  "wrote " + $out
  + " | roots=" + (.roots | length | tostring)
  + " entities=" + (.entities | length | tostring)
  + " attention=" + (.attention | length | tostring)
  + " unsupported=" + ([.unsupported[].category] | join(",") | if . == "" then "none" else . end)
  + " captured_at=" + .generated_at' "$TMP/snapshot.json"
