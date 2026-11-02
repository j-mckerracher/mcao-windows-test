#!/usr/bin/env bash
# Single owner of the pstack worker workflow's availability resolution and its
# deterministic proof-record format. Sourced by no script: every consumer calls
# it as a command, so its stdout contract below is the interface.
#
# The pstack workflow dimension (`standard` default | `pstack`) is orthogonal
# to delivery mode: a pstack-equipped ship worker owns investigation,
# implementation, direct proof, and the outer no-mistakes lifecycle, while the
# pipeline's gate agents stay isolated validators and the pipeline remains the
# only publisher. bin/fm-dod-lib.sh owns what the workflow changes for the
# WORKER (fm_pstack_workflow_block, the proof obligations); this script owns
# what the workflow RESOLVES from and the exact proof format that records it.
#
# Availability matrix (v1): harnesses `claude` and `codex`, from config/pstack-plugin.
# Firstmate never installs pstack, never runs `setup-pstack`, and never writes
# user-global harness configuration; the plugin must already exist on this
# machine and config/pstack-plugin must name it.
#
# config/pstack-plugin holds ONE absolute path to a pstack Claude plugin root
# that itself contains `.claude-plugin/plugin.json` (not a folder of plugins),
# resolved to a real path. The file is inheritable through FM_INHERITABLE_CONFIG
# like the rest of config/ (bin/fm-config-inherit-lib.sh owns that list; an
# inherited path that does not exist in the secondmate home is refused rather
# than guessed).
#
# Commands and exit codes (refusals print an `error:` line and a `next:` line):
#   resolve --harness <h>     resolve pstack availability for harness <h>.
#                             On success prints one `key=value` report line per
#                             line: workflow=pstack, harness=, plugin_dir=,
#                             plugin=, entry=<plugin>:poteto-mode,
#                             load=plugin-dir, hooks=<present|none>,
#                             mcp=<present|none>.
#                             Exit 2 for wrong usage. Exit 3 for the plugin
#                             name missing or the config or plugin.json being
#                             malformed (an unusable config path, unparseable
#                             JSON, no plugin name). Exit 4 for a missing
#                             config file, plugin dir, .claude-plugin/plugin.json,
#                             or entry skill, an entry skill that is not
#                             model-invocable
#                             (`disable-model-invocation: true`; point at a
#                             harness-neutral pstack port instead, whose copy
#                             of the upstream Cursor entry is model-invocable),
#                             or jq being missing (the plugin.json name is
#                             read with jq and never guessed otherwise).
#                             Exit 5 for an unsupported harness (matrix above).
#   supported-harnesses       print the harness matrix, one name per line.
#   template <id>             print the proof-record skeleton for task <id> to
#                             data/<id>/moa-proof.md's format. Resolves the
#                             config first (same exit codes and refusals as
#                             resolve) so the entry line carries the real
#                             entry. This is the single owner of the format:
#                             nothing else in the repo restates it.
#   render <id>               print the deterministic PR body for task <id>'s
#                             data/<id>/moa-proof.md, the single owner of the
#                             body format: same record in, byte-identical body
#                             out, no model, clock or network involved. Sections
#                             in fixed order: verdict and source lines (plus a
#                             `Refs <owner>/<repo>#<N>` line, always in that
#                             full form, when the source is a GitHub Issue URL),
#                             `### Read first` (NOT VERIFIED criteria and the
#                             Not run `Risk:` lines; omitted when there are
#                             none), Acceptance criteria table, Checks, AI
#                             review, Scope, and a collapsed pointer to the full
#                             record, which the worker posts as a separate
#                             comment. Reads only the record, so it needs no
#                             task record. Exit 0 prints the body; exit 1
#                             prints an `error:` line and a `next:` line for a
#                             missing record or one without task, candidate or
#                             source; exit 2 for wrong usage.
#   proof-check <id> [--rerun] [--require-mutation-row] [--check-body <file>] [--check-comment <file>]
#                             deterministic self-check that task <id>'s
#                             data/<id>/moa-proof.md record is well-formed
#                             and anchored: requires state/<id>.meta with
#                             kind=ship and workflow=pstack; the task's ship
#                             branch (meta `branch=`, default `fm/<id>`);
#                             valid fields (the proof-record header line, task
#                             match, a `<plugin>:<skill>` entry, a
#                             whitespace-free playbook, and 40-or-64-hex
#                             base/candidate/supersedes values, a trailing
#                             `# comment` stripped before validation because
#                             the template line carries one); candidate == the
#                             worktree HEAD; the worktree ON the ship branch;
#                             no tracked uncommitted changes; base a strict
#                             ancestor of candidate; supersedes, when the
#                             field carries a sha, a commit this copy has that
#                             is NOT an ancestor of candidate (a complete
#                             invalidation rebuilt from base discarded it); the
#                             `## Reproduction or baseline`, `## Direct proof`,
#                             and `## Not run and remaining uncertainty`
#                             sections non-empty; and NO active no-mistakes
#                             run on the branch (fm_nm_run_checked `axi` +
#                             fm_nm_select_run: pending/running -> refuse;
#                             absent/terminal -> pass; an unreadable, capped,
#                             or empty overview, or a missing binary, fails
#                             closed with a `next:`). Exit 0 prints
#                             `ok candidate=<sha>`; exit 1 prints ONE `error:`
#                             line per failure, listing every failure, and a
#                             `next:` when the run-state read failed closed.
#                             --check-body adds one more check: <file> must be
#                             byte-identical to `render <id>` output (every
#                             whitespace difference fails, and the error names
#                             the first differing line). --check-comment adds
#                             another: <file> must be byte-identical to the
#                             record. Without the flags neither is checked.
#                             --rerun runs after every check above passes and
#                             re-executes each cheap Checks row in the task
#                             worktree: a row whose check name or command names
#                             lint or a type checker (lint, eslint, ruff, clippy,
#                             mypy, pylint, tsc, cargo check, type) is cheap, and
#                             its command's exit status is compared with the
#                             recorded `pass:` or `fail:` result. Other rows are
#                             reported as not re-run and `info:` rows are skipped.
#                             A command the shell cannot run is a failed
#                             comparison, and the rest still run. The record is
#                             only read, never written. Exit 0 when every compared
#                             row matches, 1 on any mismatch, 3 when the task
#                             record names no usable worktree. The flag may also
#                             come before the task id.
#                             --require-mutation-row adds the mutation-evidence
#                             gate (off by default, so the mutation row stays
#                             report-only). The change's paths come from git diff
#                             base...candidate and are classified by
#                             bin/fm-mutation-classify.sh; a classifier refusal
#                             fails. When any path has a mutate surface, the
#                             Checks table needs exactly one `mutation` row whose
#                             command is bin/fm-mutation-verdict.sh and whose
#                             result starts `passed: ` or `not verified: `, bound
#                             to the candidate by the Checks check. A passed row
#                             is refused when the change holds an unsupported
#                             path. No score floor applies: a not-verified row is
#                             valid. Proof-check cannot tell where a row was
#                             written, so the exact-head Mutation verdict check
#                             run remains the record that a verdict was measured.
# The `## Pipeline outcome` section is deliberately NOT checked by proof-check:
# it is filled only after validation, per the worker contract.
#
# hooks=/mcp= report what the plugin would load beyond the entry skill, so the
# caller can see what side effects a plugin carries: hooks are present when
# plugin.json carries a non-empty `hooks` value or the plugin root has a
# `hooks/` directory; MCP is present when the plugin root has a `.mcp.json` or
# plugin.json carries a non-empty `mcpServers` value. Neither gates v1
# availability; they are surfaced because a plugin that hooks the harness or
# adds MCP servers is a bigger surface than the entry skill alone.
#
# Usage: fm-pstack.sh resolve --harness <harness>
#        fm-pstack.sh supported-harnesses
#        fm-pstack.sh template <task-id>
#        fm-pstack.sh render <task-id>
#        fm-pstack.sh proof-check <task-id> [--rerun] [--require-mutation-row] [--check-body <file>] [--check-comment <file>]
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"
DATA="${FM_DATA_OVERRIDE:-$FM_HOME/data}"
FM_PSTACK_CONFIG="${FM_CONFIG_OVERRIDE:-$FM_HOME/config}"

# shellcheck source=bin/fm-branch-prefix-lib.sh
. "$SCRIPT_DIR/fm-branch-prefix-lib.sh"
# shellcheck source=bin/fm-pr-lib.sh
. "$SCRIPT_DIR/fm-pr-lib.sh"
# shellcheck source=bin/fm-nm-run-lib.sh
. "$SCRIPT_DIR/fm-nm-run-lib.sh"
# shellcheck source=bin/fm-backend.sh
. "$SCRIPT_DIR/fm-backend.sh"

fm_pstack_supported_harnesses() {
  printf '%s\n' claude codex
}

fm_pstack_refuse() {  # <exit-code> <error-line> <next-line>
  printf 'error: %s\n' "$2"
  printf 'next: %s\n' "$3"
  exit "$1"
}

fm_pstack_trim() {
  local s=${1:-}
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}

# First scalar field `<key>: <value>` in <proof-file>; empty when the field is
# missing or empty. A trailing ` # comment` is stripped before the value is
# returned, because the proof template carries one on its optional field.
fm_pstack_proof_field() {  # <proof-file> <key>
  local v
  v=$(sed -n "s/^$2:[[:space:]]*\(.*\)$/\1/p" "$1" 2>/dev/null | head -1)
  v=$(printf '%s\n' "$v" | sed 's/#.*$//')
  fm_pstack_trim "$v"
}

# Content of the proof section whose heading line is exactly <heading>, from
# that line to the next `## ` heading or end of file, joined with newlines.
fm_pstack_proof_section() {  # <proof-file> <heading-line>
  awk -v sec="$2" '
    $0 == sec { inh = 1; next }
    /^## /    { inh = 0; next }
    inh { buf = buf $0 "\n" }
    END { sub(/\n$/, "", buf); printf "%s", buf }
  ' "$1"
}

# 0 when <SKILL.md> carries a model-invocable entry: its YAML frontmatter exists
# and does not carry `disable-model-invocation: true`. 1 when the frontmatter is
# absent or disables invocation; the caller writes the refusal either way, so a
# malformed skill fails to the same actionable place.
fm_pstack_entry_model_invocable() {  # <SKILL.md>
  awk '
    NR == 1 && $0 == "---" { infront = 1; saw = 1; next }
    infront && $0 == "---" { infront = 0; next }
    infront {
      s = $0
      sub(/^[[:space:]]+/, "", s)
      sub(/[[:space:]]+$/, "", s)
      if (s ~ /^disable-model-invocation:/) {
        v = substr(s, index(s, ":") + 1)
        gsub(/[[:space:]]/, "", v)
        gsub(/^"|"$/, "", v)
        if (v == "true" || v == "True" || v == "TRUE") disabled = 1
      }
    }
    END { if (saw && !disabled) exit 0; exit 1 }
  ' "$1"
}

# Core resolver: validates config/pstack-plugin and the plugin it names, and
# sets FM_PSTACK_PLUGIN_DIR, FM_PSTACK_PLUGIN, FM_PSTACK_ENTRY,
# FM_PSTACK_HOOKS and FM_PSTACK_MCP. Prints nothing; a failure refuses by exit
# code, never silently. The harness argument is the already-validated, already
# supported harness. Every jq side read is guarded, because the JSON parse
# check above is what already proved the file readable.
fm_pstack_resolve_core() {  # <harness>
  FM_PSTACK_PLUGIN=
  FM_PSTACK_PLUGIN_DIR=
  FM_PSTACK_ENTRY=
  FM_PSTACK_HOOKS=none
  FM_PSTACK_MCP=none

  local cfg="$FM_PSTACK_CONFIG/pstack-plugin" raw plugin_file plugin
  if [ ! -f "$cfg" ]; then
    fm_pstack_refuse 4 \
      "no pstack plugin is configured: $cfg does not exist" \
      "write the absolute path of a pstack Claude plugin root (one containing .claude-plugin/plugin.json) into $cfg; firstmate never installs pstack itself"
  fi
  raw=$(fm_pstack_trim "$(cat "$cfg")")
  if [ -z "$raw" ]; then
    fm_pstack_refuse 3 \
      "the pstack plugin configuration at $cfg is empty" \
      "write exactly one absolute path to a pstack Claude plugin root into $cfg"
  fi
  # The command substitution above strips trailing newlines only, so any
  # interior newline means the configuration holds more than one path.
  case "$raw" in
    *$'\n'*)
      fm_pstack_refuse 3 \
        "the pstack plugin configuration at $cfg must hold exactly one path" \
        "keep exactly one absolute path to a pstack Claude plugin root in $cfg and remove any extra lines"
      ;;
  esac
  case "$raw" in
    /*) ;;
    *)
      fm_pstack_refuse 3 \
        "the pstack plugin configuration at $cfg holds \"$raw\", which is not an absolute path" \
        "write exactly one absolute path to a pstack Claude plugin root into $cfg"
      ;;
  esac
  if [ ! -d "$raw" ]; then
    fm_pstack_refuse 4 \
      "the pstack plugin directory at $raw does not exist" \
      "correct the path in $cfg or check out the harness-neutral pstack Claude plugin at that path"
  fi
  FM_PSTACK_PLUGIN_DIR="$(cd "$raw" && pwd -P)"
  plugin_file="$FM_PSTACK_PLUGIN_DIR/.claude-plugin/plugin.json"
  if [ ! -f "$plugin_file" ]; then
    fm_pstack_refuse 4 \
      "$FM_PSTACK_PLUGIN_DIR is not a Claude plugin root: .claude-plugin/plugin.json is missing there" \
      "point $cfg at the plugin root itself (the directory containing .claude-plugin/plugin.json), not a folder of plugins"
  fi
  if ! command -v jq >/dev/null 2>&1; then
    fm_pstack_refuse 4 \
      "jq is required to read the pstack plugin's .claude-plugin/plugin.json and none is installed" \
      "install jq so the plugin name can be read the same way everywhere"
  fi
  if ! plugin=$(jq -r 'if (.name|type)=="string" then .name else "" end' "$plugin_file" 2>/dev/null); then
    fm_pstack_refuse 3 \
      "$plugin_file is not parseable JSON" \
      "fix the plugin.json in the pstack plugin check-out this configuration names"
  fi
  if [ -z "$plugin" ]; then
    fm_pstack_refuse 3 \
      "$plugin_file carries no usable \"name\" string" \
      "give the pstack plugin a name in its plugin.json so the entry can be formed as <plugin>:poteto-mode"
  fi
  FM_PSTACK_PLUGIN=$plugin
  FM_PSTACK_ENTRY="$plugin:poteto-mode"

  local skill="$FM_PSTACK_PLUGIN_DIR/skills/poteto-mode/SKILL.md"
  if [ ! -f "$skill" ]; then
    fm_pstack_refuse 4 \
      "the pstack entry skill is missing: $skill does not exist" \
      "point $cfg at a pstack plugin that ships the poteto-mode entry skill"
  fi
  if ! fm_pstack_entry_model_invocable "$skill"; then
    fm_pstack_refuse 4 \
      "the pstack entry at $skill is not model-invocable (its SKILL.md frontmatter is absent or sets disable-model-invocation: true)" \
      "use a harness-neutral pstack port such as michael-denyer/pstack-claude or ericlitman/open-pstack, whose copy of the upstream Cursor entry is model-invocable; the upstream cursor/plugins/pstack copy is Cursor-only"
  fi

  local hooks_mcp
  hooks_mcp=$(jq -r '.hooks // empty' "$plugin_file" 2>/dev/null || true)
  if [ -n "$hooks_mcp" ] || [ -d "$FM_PSTACK_PLUGIN_DIR/hooks" ]; then
    FM_PSTACK_HOOKS=present
  fi
  if [ -f "$FM_PSTACK_PLUGIN_DIR/.mcp.json" ] \
    || [ -n "$(jq -r '.mcpServers // empty' "$plugin_file" 2>/dev/null || true)" ]; then
    FM_PSTACK_MCP=present
  fi
}

do_resolve() {  # --harness <h>
  [ "${1:-}" = "--harness" ] || fm_pstack_refuse 2 \
    "usage: fm-pstack.sh resolve --harness <harness>" \
    "call resolve with exactly one --harness argument naming the worker runtime"
  [ "$#" -eq 2 ] || fm_pstack_refuse 2 \
    "usage: fm-pstack.sh resolve --harness <harness>" \
    "resolve takes exactly one --harness and one harness name"
  [ -n "$2" ] || fm_pstack_refuse 2 \
    "usage: fm-pstack.sh resolve --harness <harness>" \
    "--harness needs a value naming the worker runtime"
  # The harness matrix is checked before any local configuration reads, so an
  # unsupported harness is reported even on a machine with no plugin configured.
  case "$2" in
    claude|codex)
      fm_pstack_resolve_core "$2"
      ;;
    *)
      fm_pstack_refuse 5 \
        "workflow=pstack is not supported on harness \"$2\"" \
        "pstack ships on claude and codex only in v1 (fm-pstack.sh supported-harnesses prints the matrix); dispatch this task on a standard workflow instead"
      ;;
  esac
  printf 'workflow=pstack\n'
  printf 'harness=%s\n' "$2"
  printf 'plugin_dir=%s\n' "$FM_PSTACK_PLUGIN_DIR"
  printf 'plugin=%s\n' "$FM_PSTACK_PLUGIN"
  printf 'entry=%s\n' "$FM_PSTACK_ENTRY"
  printf 'load=plugin-dir\n'
  printf 'hooks=%s\n' "$FM_PSTACK_HOOKS"
  printf 'mcp=%s\n' "$FM_PSTACK_MCP"
}

do_supported_harnesses() {
  fm_pstack_supported_harnesses
}

do_template() {  # <id>
  [ "$#" -eq 1 ] || fm_pstack_refuse 2 \
    "usage: fm-pstack.sh template <task-id>" \
    "call template with exactly the task id the proof record is written for"
  local id=$1
  case "$id" in
    ""|.*|*/*) fm_pstack_refuse 2 \
      "usage: fm-pstack.sh template <task-id>" \
      "the proof record is written for a firstmate task id" ;;
  esac
  # The entry line is filled from the same resolution a spawn performs, so the
  # template cannot be rendered for a plugin that is not actually present; a
  # failure refuses with the resolve script's own exit codes and refusals.
  fm_pstack_resolve_core claude
  cat <<EOF
moa-proof: v2
task: $id
entry: $FM_PSTACK_ENTRY
playbook: <playbook>
base: <40-hex>
candidate: <40-hex>
supersedes: <40-hex>   # optional, only after complete invalidation
brief: <path-to-brief>
source: <url or 'pasted'>
no-mistakes: <pass|failed|skipped: <reason>|off>
## Reproduction or baseline
## Checks
## Acceptance criteria
## AI review
## Scope
## Not run remaining uncertainty
EOF
}

# Data rows of the markdown table in <section>, one row per line, each cell
# trimmed and the cells joined with the unit separator (\037). The header is the
# row directly above the separator row and is skipped; a table with no separator
# row has no header. An escaped pipe (`\|`) stays inside its cell, escape
# included, so a cell is never split on it.
fm_proof_table_rows() {  # <section>
  local line row cell out pending=
  local -a f
  while IFS= read -r line; do
    line=$(fm_pstack_trim "${line%$'\r'}")
    case "$line" in '|'*) ;; *) continue ;; esac
    if [ -z "$(printf '%s' "$line" | tr -d '|: -')" ]; then
      pending=  # the separator row: the row above it was the header
      continue
    fi
    [ -z "$pending" ] || printf '%s\n' "$pending"
    row=${line//\\|/$'\036'}
    row=${row#|}
    row=${row%|}
    IFS='|' read -ra f <<< "$row"
    out=
    for cell in ${f[@]+"${f[@]}"}; do
      cell=$(fm_pstack_trim "$cell")
      out="$out${out:+$'\037'}${cell//$'\036'/\\|}"
    done
    pending=$out
  done <<< "$1"
  [ -z "$pending" ] || printf '%s\n' "$pending"
}

# Sets FM_PROOF_SRC_KEY, FM_PROOF_SRC_URL and FM_PROOF_SRC_REFS from the record's
# source field: `pasted`, a `.../browse/<KEY-N>` Jira URL, a GitHub Issue URL
# (key `#<N>`, and the Refs line in the always-full `<owner>/<repo>#<N>` form so
# render never needs to know the current repository), any other https URL (key
# and link are the URL), or any other text (key only, no link).
fm_proof_source_parts() {  # <source>
  local source=$1 re
  FM_PROOF_SRC_KEY=$source
  FM_PROOF_SRC_URL=
  FM_PROOF_SRC_REFS=
  re='^https://github\.com/([^/]+)/([^/]+)/issues/([0-9]+)([/?].*)?$'
  if [[ $source =~ $re ]]; then
    FM_PROOF_SRC_KEY="#${BASH_REMATCH[3]}"
    FM_PROOF_SRC_URL=$source
    FM_PROOF_SRC_REFS="Refs ${BASH_REMATCH[1]}/${BASH_REMATCH[2]}#${BASH_REMATCH[3]}"
    return 0
  fi
  re='^https://[^/]+/browse/([A-Z][A-Z0-9]*-[0-9]+)([/?].*)?$'
  if [[ $source =~ $re ]]; then
    FM_PROOF_SRC_KEY=${BASH_REMATCH[1]}
    FM_PROOF_SRC_URL=$source
    return 0
  fi
  case "$source" in
    https://*) FM_PROOF_SRC_URL=$source ;;
  esac
}

# The Checks section as one inline line: `<icon> <check> <result>` per row,
# joined with ` · `. The result text is the record's own, never reworded. The icon
# is ✅ for a result whose first word is pass, passed, ok or clean, ❌ for fail,
# failed or error, and absent for anything else (a bare number such as coverage),
# so an unrecognized result is never shown as a pass.
# The check is the first cell and the result the second to last, the same ends
# fm_proof_validate_checks_table reads, so a command holding a pipe is harmless.
fm_proof_render_checks() {  # <checks-section>
  local row check result lower icon out=
  local -a f
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    IFS=$'\037' read -ra f <<< "$row"
    [ "${#f[@]}" -ge 4 ] || continue
    check=${f[0]}
    result=${f[$((${#f[@]} - 2))]}
    lower=$(printf '%s' "$result" | tr '[:upper:]' '[:lower:]')
    icon=
    if [[ $lower =~ ^(pass|passed|ok|clean)([^a-z]|$) ]]; then
      icon='✅ '
    elif [[ $lower =~ ^(fail|failed|error)([^a-z]|$) ]]; then
      icon='❌ '
    fi
    out="$out${out:+ · }$icon$check $result"
  done <<< "$(fm_proof_table_rows "$1")"
  [ -n "$out" ] || out="No checks recorded."
  printf '%s\n' "$out"
}

# The AI review section as one summary line: the finding count, the tally of
# dispositions, the deferred tracking links, then the highest-severity finding
# (the first one in record order on a tie) as `<Severity> finding <text>, <how>.`.
# Every non-blank line other than `none` is a finding, the same reading
# fm_proof_validate_ai_review uses; a finding is `- <id> (<severity>) <text>` ending
# in `fixed`, `rejected: <reason>` or `deferred: <link>`.
fm_proof_render_ai() {  # <ai-section>
  local line lower text sev summ disp link rank best_rank=0 best='' links=''
  local count=0 fixed=0 rejected=0 deferred=0 out
  while IFS= read -r line; do
    line=$(fm_pstack_trim "${line%$'\r'}")
    [ -n "$line" ] || continue
    lower=$(printf '%s' "$line" | tr '[:upper:]' '[:lower:]')
    case "$lower" in none|'- none') continue ;; esac
    count=$((count + 1))
    text=$line
    disp=
    if [[ $line =~ ^(.*)[[:space:]]deferred:[[:space:]]+([^[:space:]]+)$ ]]; then
      disp=deferred
      text=${BASH_REMATCH[1]}
      link=${BASH_REMATCH[2]}
      deferred=$((deferred + 1))
      case ", $links, " in
        *", $link, "*) ;;
        *) links="$links${links:+, }$link" ;;
      esac
    elif [[ $line =~ ^(.*)rejected:[[:space:]]+[^[:space:]] ]]; then
      disp=rejected
      text=${line%%rejected:*}
      rejected=$((rejected + 1))
    elif [[ $line =~ (^|[^[:alnum:]])fixed$ ]]; then
      disp=fixed
      text=${line%fixed}
      fixed=$((fixed + 1))
    fi
    sev=
    summ=${text#[-*]}
    summ=$(fm_pstack_trim "$summ")
    if [[ $text =~ ^[-*][[:space:]]+[^[:space:]]+[[:space:]]+\(([A-Za-z]+)\)[[:space:]]+(.*)$ ]]; then
      sev=$(printf '%s' "${BASH_REMATCH[1]}" | tr '[:upper:]' '[:lower:]')
      summ=${BASH_REMATCH[2]}
    fi
    summ=$(fm_pstack_trim "$summ")
    while :; do
      case "$summ" in
        *[,\;:.]|*[[:space:]]) summ=${summ%?} ;;
        *) break ;;
      esac
    done
    case "$sev" in
      critical) rank=4 ;;
      high) rank=3 ;;
      med|medium) rank=2 ;;
      low) rank=1 ;;
      *) rank=0 ;;
    esac
    if [ "$rank" -gt "$best_rank" ]; then
      best_rank=$rank
      best="$(printf '%s' "${sev:0:1}" | tr '[:lower:]' '[:upper:]')${sev:1} finding $summ${disp:+, $disp}."
    fi
  done <<< "$1"
  if [ "$count" -eq 0 ]; then
    printf '0 findings.\n'
    return 0
  fi
  out="$count findings: $fixed fixed, $rejected rejected, $deferred deferred"
  [ -z "$links" ] || out="$out ($links)"
  out="$out."
  [ -z "$best" ] || out="$out $best"
  printf '%s\n' "$out"
}

# The Scope section as one summary line from its `actual: <n> files, +<added>
# -<removed>` line (the line proof-check holds equal to git diff --numstat), then
# either the verbatim text of an `outside-scope: <text>` line or, when that line
# is absent or says `none`, the sentence that every change is inside the brief's
# declared paths.
fm_proof_render_scope() {  # <scope-section>
  local line lower actual='' outside='' files plus minus noun=files out
  while IFS= read -r line; do
    line=$(fm_pstack_trim "${line%$'\r'}")
    lower=$(printf '%s' "$line" | tr '[:upper:]' '[:lower:]')
    case "$lower" in
      *actual:*) [ -n "$actual" ] || actual=$line ;;
      outside-scope:*) [ -n "$outside" ] || outside=$(fm_pstack_trim "${line#*:}") ;;
    esac
  done <<< "$1"
  if [[ $actual =~ ([0-9]+)[[:space:]]+files?.*\+([0-9]+).*[[:space:]]-([0-9]+) ]]; then
    files=${BASH_REMATCH[1]}
    plus=${BASH_REMATCH[2]}
    minus=${BASH_REMATCH[3]}
    [ "$files" -ne 1 ] || noun='file'
    out="$files $noun, +$plus −$minus."
  else
    out="No actual line recorded."
  fi
  case "$(printf '%s' "$outside" | tr '[:upper:]' '[:lower:]')" in
    ""|none) out="$out All inside brief's declared paths." ;;
    *) out="$out Outside-scope changes: $outside" ;;
  esac
  printf '%s\n' "$out"
}

# Deterministic PR body for a moa-proof v2 record: the same record always yields
# byte-identical output (no clock, randomness or locale; the record is the only
# input, never the repository or the network), and no model writes it. Sections
# in fixed order, a blank line between them: the verdict and source lines (plus a
# Refs line for a GitHub Issue source), Read first (omitted when nothing needs
# it), Acceptance criteria, Checks, AI review, Scope, and the collapsed pointer to
# the full record, which the worker posts as a separate comment. Returns 1 with an
# `error:` line on stderr when the record is missing or has no task, candidate or
# source.
fm_proof_render() {  # <proof-file>
  local -x LC_ALL=C
  local proof=$1 task candidate source no_mistakes short
  [ -f "$proof" ] || { echo "error: proof file not found: $proof" >&2; return 1; }
  task=$(fm_pstack_proof_field "$proof" task)
  candidate=$(fm_pstack_proof_field "$proof" candidate)
  source=$(fm_pstack_proof_field "$proof" source)
  no_mistakes=$(fm_pstack_proof_field "$proof" no-mistakes)
  [ -n "$task" ] || { echo "error: the proof record has no task field to render" >&2; return 1; }
  [ -n "$candidate" ] || { echo "error: the proof record has no candidate field to render" >&2; return 1; }
  [ -n "$source" ] || { echo "error: the proof record has no source field to render" >&2; return 1; }
  [ -n "$no_mistakes" ] || no_mistakes=off
  short=${candidate:0:7}

  local ac_sec checks_sec ai_sec scope_sec notrun_sec
  ac_sec=$(fm_pstack_proof_section "$proof" "## Acceptance criteria")
  checks_sec=$(fm_pstack_proof_section "$proof" "## Checks")
  ai_sec=$(fm_pstack_proof_section "$proof" "## AI review")
  scope_sec=$(fm_pstack_proof_section "$proof" "## Scope")
  notrun_sec=$(fm_pstack_proof_section "$proof" "## Not run remaining uncertainty")

  # Acceptance criteria rows and the unverified list in one pass.
  local row id crit evidence mark i n verified=0 unverified=0 ac_rows='' nv_lines='' reason
  local -a f
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    IFS=$'\037' read -ra f <<< "$row"
    n=${#f[@]}
    id=${f[0]}
    crit=
    evidence=
    if [ "$n" -ge 3 ]; then
      evidence=${f[$((n - 1))]}
      for ((i = 1; i < n - 1; i++)); do
        if [ "$i" -eq 1 ]; then crit=${f[$i]}; else crit="$crit\\|${f[$i]}"; fi
      done
    elif [ "$n" -eq 2 ]; then
      crit=${f[1]}
    fi
    case "$evidence" in
      *"NOT VERIFIED"*)
        unverified=$((unverified + 1))
        mark='❌'
        reason=
        [ -z "$id" ] || reason=$(printf '%s\n' "$notrun_sec" | grep -Fw -m1 -- "$id" || true)
        reason=$(fm_pstack_trim "$reason")
        case "$reason" in '- '*|'* '*) reason=${reason#??} ;; esac
        [ -n "$reason" ] || reason=$evidence
        nv_lines="$nv_lines- ⚠️ **$id not verified:** $reason"$'\n'
        evidence='not verified'
        ;;
      *)
        verified=$((verified + 1))
        mark='✅'
        ;;
    esac
    ac_rows="$ac_rows| $mark | $id${crit:+ $crit} | $evidence |"$'\n'
  done <<< "$(fm_proof_table_rows "$ac_sec")"

  # Risks are the Not run lines that open with `Risk:`, with or without a bullet.
  local line lower risk_lines=
  while IFS= read -r line; do
    line=$(fm_pstack_trim "${line%$'\r'}")
    lower=$(printf '%s' "$line" | tr '[:upper:]' '[:lower:]')
    case "$lower" in
      risk:*|'- risk:'*|'* risk:'*)
        case "$line" in '- '*|'* '*) line=${line#??} ;; esac
        risk_lines="$risk_lines- ⚠️ $line"$'\n'
        ;;
    esac
  done <<< "$notrun_sec"

  fm_proof_source_parts "$source"
  # shellcheck disable=SC2016  # the backticks are literal markdown code spans
  printf '**Verified locally at `%s`** · %s of %s acceptance criteria proven · %s not verified · no-mistakes: %s\n' \
    "$short" "$verified" "$((verified + unverified))" "$unverified" "$no_mistakes"
  if [ -n "$FM_PROOF_SRC_URL" ]; then
    printf 'Source: [%s](%s)' "$FM_PROOF_SRC_KEY" "$FM_PROOF_SRC_URL"
  else
    printf 'Source: %s' "$FM_PROOF_SRC_KEY"
  fi
  # shellcheck disable=SC2016  # literal markdown code span
  printf ' · Proof record: `data/%s/moa-proof.md` (attached)\n' "$task"
  [ -z "$FM_PROOF_SRC_REFS" ] || printf '%s\n' "$FM_PROOF_SRC_REFS"
  if [ -n "$nv_lines$risk_lines" ]; then
    printf '\n### Read first\n%s%s' "$nv_lines" "$risk_lines"
  fi
  printf '\n### Acceptance criteria\n| | criterion | evidence |\n|---|---|---|\n%s' "$ac_rows"
  # shellcheck disable=SC2016  # literal markdown code span
  printf '\n### Checks (all at `%s`)\n' "$short"
  fm_proof_render_checks "$checks_sec"
  printf '\n### AI review (fresh-session reviewer)\n'
  fm_proof_render_ai "$ai_sec"
  printf '\n### Scope\n'
  fm_proof_render_scope "$scope_sec"
  printf '\n<details><summary>Full proof record</summary>\n\n(Attached as separate PR comment.)\n\n</details>\n'
}

do_render() {  # <task-id>
  [ "$#" -eq 1 ] || fm_pstack_refuse 2 \
    "usage: fm-pstack.sh render <task-id>" \
    "call render with exactly the task id whose proof record is rendered"
  local id=$1 proof body
  case "$id" in
    ""|.*|*/*) fm_pstack_refuse 2 \
      "usage: fm-pstack.sh render <task-id>" \
      "the proof record is rendered for a firstmate task id" ;;
  esac
  proof="$DATA/$id/moa-proof.md"
  [ -f "$proof" ] || fm_pstack_refuse 1 \
    "no proof record at $proof" \
    "write it first with the template command, then run render again"
  # Rendered in full before anything is printed, so a record that cannot be
  # rendered never leaves a half-written body on stdout.
  if ! body=$(fm_proof_render "$proof" 2>&1); then
    fm_pstack_refuse 1 "${body#error: }" "fix the proof record, then run render again"
  fi
  printf '%s\n' "$body"
}

# Proof-check failure collector: each failure is one newline-terminated line,
# and every failure is printed before exit 1 so the worker fixes all of them in
# one round.
FM_PSTACK_FAILURES=''
FM_PSTACK_NEXT=
fm_pstack_fail() {  # <reason>
  FM_PSTACK_FAILURES="$FM_PSTACK_FAILURES$1
"
}

# Absolute path of the brief a proof record names; a relative path is read from
# the operational home, where data/<id>/brief.md lives.
fm_pstack_brief_path() {  # <brief-field>
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s' "$FM_HOME/$1" ;;
  esac
}

# The brief's acceptance-criteria ids, one per line, sorted numerically, read
# from the acceptance_criteria list of the fenced moa-intake block. A failure
# prints its reason on stdout and returns 1.
fm_proof_load_brief_acs() {  # <brief-path>
  local brief=$1 start end ids sorted expect=1 n
  [ -f "$brief" ] || { echo "brief file not found"; return 1; }
  start=$(grep -n '^```moa-intake$' "$brief" | head -1 | cut -d: -f1)
  [ -n "$start" ] || { echo "intake block not found in brief"; return 1; }
  end=$(awk -v s="$start" 'NR > s && $0 == "```" { print NR; exit }' "$brief")
  [ -n "$end" ] || { echo "intake block not properly closed"; return 1; }
  ids=$(sed -n "$((start + 1)),$((end - 1))p" "$brief" | awk '
    /^acceptance_criteria:/ { inac = 1; next }
    /^[a-z_]+:/ { inac = 0 }
    inac && /^- AC[0-9]+ \[(confirmed|drafted)\]: / { print $2 }')
  [ -n "$ids" ] || { echo "no acceptance criteria in the intake block"; return 1; }
  sorted=$(printf '%s\n' "$ids" | sed 's/^AC//' | sort -n)
  for n in $sorted; do
    if [ "$n" -ne "$expect" ]; then
      echo "acceptance criteria ids are not sequential from AC1 (expected AC$expect, found AC$n)"
      return 1
    fi
    expect=$((expect + 1))
  done
  printf '%s\n' "$sorted" | sed 's/^/AC/'
}

# Check 1: every acceptance criterion in the brief has a row in the proof's
# Acceptance criteria table.
fm_proof_validate_ac_coverage() {  # (no args, uses proof variable)
  local brief brief_path acs ac table
  brief=$(fm_pstack_proof_field "$proof" brief)
  case "$brief" in ""|'<'*) return 0 ;; esac  # the brief-field check already failed
  brief_path=$(fm_pstack_brief_path "$brief")
  [ -f "$brief_path" ] || return 0            # the brief-exists check already failed
  if ! acs=$(fm_proof_load_brief_acs "$brief_path"); then
    fm_pstack_fail "the brief's acceptance criteria cannot be read: $acs"
    return 0
  fi
  table=$(fm_pstack_proof_section "$proof" "## Acceptance criteria")
  for ac in $acs; do
    printf '%s\n' "$table" | grep -q "^| *$ac *|" \
      || fm_pstack_fail "acceptance criterion $ac from brief is not in the proof record's AC table"
  done
}

# Check 2: an acceptance criterion marked NOT VERIFIED is named in the Not run
# section. It never fails the proof on its own.
fm_proof_validate_not_verified() {  # (no args, uses proof variable)
  local table not_run line ac
  table=$(fm_pstack_proof_section "$proof" "## Acceptance criteria")
  not_run=$(fm_pstack_proof_section "$proof" "## Not run remaining uncertainty")
  while IFS= read -r line; do
    case "$line" in *"NOT VERIFIED"*) ;; *) continue ;; esac
    ac=$(printf '%s\n' "$line" | sed -n 's/^| *\(AC[0-9][0-9]*\) *|.*/\1/p')
    [ -n "$ac" ] || continue
    printf '%s\n' "$not_run" | grep -qw "$ac" \
      || fm_pstack_fail "acceptance criterion $ac is marked NOT VERIFIED but not mentioned in Not run"
  done <<< "$table"
}

# Check 3: every Checks row names a check, a command and a result, and was run
# at exactly the candidate commit. The command may itself contain a pipe, so the
# row is read from both ends.
fm_proof_validate_checks_table() {  # (no args, uses proof variable and candidate)
  local section line row n check cmd result at_sha seen_header=0
  local -a f
  section=$(fm_pstack_proof_section "$proof" "## Checks")
  while IFS= read -r line; do
    line=$(fm_pstack_trim "$line")
    case "$line" in '|'*) ;; *) continue ;; esac
    [ -n "$(printf '%s' "$line" | tr -d '|: -')" ] || continue  # separator row
    if [ "$seen_header" -eq 0 ]; then seen_header=1; continue; fi
    row=${line#|}
    row=${row%|}
    IFS='|' read -ra f <<< "$row"
    n=${#f[@]}
    if [ "$n" -lt 4 ]; then
      fm_pstack_fail "Checks table row is not check | command | result | at: $line"
      continue
    fi
    check=$(fm_pstack_trim "${f[0]}")
    result=$(fm_pstack_trim "${f[$((n - 2))]}")
    at_sha=$(fm_pstack_trim "${f[$((n - 1))]}")
    cmd=$(IFS='|'; fm_pstack_trim "${f[*]:1:$((n - 3))}")
    [ -n "$check" ] || fm_pstack_fail "Checks table has empty check name"
    [ -n "$cmd" ] || fm_pstack_fail "Checks table row for $check has empty command"
    [ -n "$result" ] || fm_pstack_fail "Checks table row for $check has empty result"
    [ "$at_sha" = "$candidate" ] \
      || fm_pstack_fail "Checks table row for $check shows 'at' $at_sha, not candidate $candidate"
  done <<< "$section"
}

# Check 4: the Scope section's `actual:` line matches git diff --numstat
# base..candidate for files, added lines and removed lines.
fm_proof_validate_scope() {  # (no args, uses proof variable and wt/base/candidate)
  local section actual r_files r_plus r_minus numstat a_files a_plus a_minus
  [ -n "$wt" ] && [ -n "$base" ] && [ -n "$candidate" ] || return 0  # anchoring already failed
  section=$(fm_pstack_proof_section "$proof" "## Scope")
  actual=$(printf '%s\n' "$section" | grep -i 'actual:' | head -1)
  r_files=$(printf '%s\n' "$actual" | sed -En 's/.*[^0-9]([0-9]+) files?.*/\1/p')
  r_plus=$(printf '%s\n' "$actual" | sed -En 's/.*\+([0-9]+).*/\1/p')
  r_minus=$(printf '%s\n' "$actual" | sed -En 's/.* -([0-9]+).*/\1/p')
  if [ -z "$r_files" ] || [ -z "$r_plus" ] || [ -z "$r_minus" ]; then
    fm_pstack_fail "the Scope section needs an 'actual:' line reading '<n> files, +<added> -<removed>'"
    return 0
  fi
  numstat=$(git -C "$wt" diff --numstat "$base..$candidate" 2>/dev/null) || numstat=
  a_files=$(printf '%s\n' "$numstat" | grep -c . || true)
  a_plus=$(printf '%s\n' "$numstat" | awk '$1 ~ /^[0-9]+$/ { s += $1 } END { print s + 0 }')
  a_minus=$(printf '%s\n' "$numstat" | awk '$2 ~ /^[0-9]+$/ { s += $2 } END { print s + 0 }')
  [ "$r_files" -eq "$a_files" ] || fm_pstack_fail "Scope section reports $r_files files, but git diff shows $a_files"
  [ "$r_plus" -eq "$a_plus" ] || fm_pstack_fail "Scope section reports +$r_plus lines, but git diff shows +$a_plus"
  [ "$r_minus" -eq "$a_minus" ] || fm_pstack_fail "Scope section reports -$r_minus lines, but git diff shows -$a_minus"
}

# Check 5: every AI review finding ends in a disposition: fixed, rejected with a
# reason, or deferred with a link. A review with no findings says `none`.
fm_proof_validate_ai_review() {  # (no args, uses proof variable)
  local section line
  section=$(fm_pstack_proof_section "$proof" "## AI review")
  while IFS= read -r line; do
    line=$(fm_pstack_trim "$line")
    [ -n "$line" ] || continue
    case "$(printf '%s' "$line" | tr '[:upper:]' '[:lower:]')" in none|'- none') continue ;; esac
    printf '%s\n' "$line" | grep -qE '(^|[^[:alnum:]])fixed$|rejected:[[:space:]]+[^[:space:]].*$|deferred:[[:space:]]+[^[:space:]]+$' \
      || fm_pstack_fail "AI review finding does not have a disposition: $line"
  done <<< "$section"
}

# One line naming the first line at which <actual-file> departs from
# <expected-file>, or the length difference when one is a prefix of the other.
# Only the line text is compared here; identical lines with a differing final
# newline or CR are reported as a whitespace-only difference.
fm_proof_first_difference() {  # <expected-file> <actual-file>
  awk '
    NR == FNR { a[NR] = $0; na = NR; next }
    { b[FNR] = $0; nb = FNR }
    END {
      n = (na > nb) ? na : nb
      for (i = 1; i <= n; i++) {
        if (i > na) { printf "the file has extra content from line %d", i; exit }
        if (i > nb) { printf "the file ends before line %d", i; exit }
        if (a[i] != b[i]) {
          e = substr(a[i], 1, 80); g = substr(b[i], 1, 80)
          printf "line %d expected \"%s\" but found \"%s\"", i, e, g
          exit
        }
      }
      printf "only line endings or the final newline differ"
    }
  ' "$1" "$2"
}

# Check 6 (--check-body): the PR body file equals render(record) byte for byte.
fm_proof_validate_body() {  # <body-file> (uses proof variable)
  local file=$1 expected rendered
  if [ ! -f "$file" ]; then
    fm_pstack_fail "the PR body file $file does not exist"
    return 0
  fi
  if ! rendered=$(fm_proof_render "$proof" 2>&1); then
    fm_pstack_fail "the record cannot be rendered to check the PR body: ${rendered#error: }"
    return 0
  fi
  expected=$(mktemp "${TMPDIR:-/tmp}/fm-pstack-render.XXXXXX")
  printf '%s\n' "$rendered" > "$expected"
  cmp -s "$expected" "$file" \
    || fm_pstack_fail "PR body does not match render(record): $(fm_proof_first_difference "$expected" "$file")"
  rm -f "$expected"
}

# Check 7 (--check-comment): the posted comment file equals the record byte for
# byte.
fm_proof_validate_comment() {  # <comment-file> (uses proof variable)
  local file=$1
  if [ ! -f "$file" ]; then
    fm_pstack_fail "the PR comment file $file does not exist"
    return 0
  fi
  cmp -s "$proof" "$file" || fm_pstack_fail "PR comment does not match record file"
}

# Check 9 (only under --require-mutation-row): a change with a mutation surface carries
# exactly one mutation row, written by bin/fm-mutation-verdict.sh and bound to the
# candidate by the Checks check above. Presence and validity are required, and the
# verdict is not scored: passed and not verified are both valid rows, so no score floor
# applies. A passed row is refused when the change holds a path the verdict never passes.
fm_proof_validate_mutation_row() {  # (no args, uses proof, wt, base, candidate)
  local section line row n paths classified counts applicable unsupported rc
  local count=0 mut_cmd='' mut_result='' seen_header=0
  local -a f
  [ -n "$wt" ] && [ -n "$base" ] && [ -n "$candidate" ] || return 0  # anchoring already failed
  paths=$(git -C "$wt" diff --name-only --diff-filter=ACMRT "$base...$candidate" 2>/dev/null) \
    || { fm_pstack_fail "the mutation gate could not list the candidate's changed paths"; return 0; }
  [ -n "$paths" ] || return 0
  # set -e would end the script on a failing classifier before its reason is reported, so the
  # exit status is captured here rather than tested afterwards.
  rc=0
  classified=$(printf '%s\n' "$paths" | "$SCRIPT_DIR/fm-mutation-classify.sh" 2>/dev/null) || rc=$?
  if [ "$rc" -ne 0 ]; then
    fm_pstack_fail "the mutation gate could not classify every changed path (fm-mutation-classify.sh exited $rc), so it cannot tell whether a mutation row is required"
    return 0
  fi
  counts=$(printf '%s\n' "$classified" | awk -F '\t' '$3 == "mutate" { m++ } $3 == "unsupported" { u++ } END { printf "%d %d\n", m + 0, u + 0 }')
  read -r applicable unsupported <<< "$counts"
  [ "$applicable" -gt 0 ] || return 0

  section=$(fm_pstack_proof_section "$proof" "## Checks")
  while IFS= read -r line; do
    line=$(fm_pstack_trim "$line")
    case "$line" in '|'*) ;; *) continue ;; esac
    [ -n "$(printf '%s' "$line" | tr -d '|: -')" ] || continue  # separator row
    if [ "$seen_header" -eq 0 ]; then seen_header=1; continue; fi
    row=${line#|}
    row=${row%|}
    IFS='|' read -ra f <<< "$row"
    n=${#f[@]}
    [ "$n" -ge 4 ] || continue  # a malformed row is reported by the Checks table check
    [ "$(fm_pstack_trim "${f[0]}")" = mutation ] || continue
    count=$((count + 1))
    mut_cmd=$(IFS='|'; fm_pstack_trim "${f[*]:1:$((n - 3))}")
    mut_result=$(fm_pstack_trim "${f[$((n - 2))]}")
  done <<< "$section"

  if [ "$count" -eq 0 ]; then
    fm_pstack_fail "the change has a mutation surface, so the Checks table needs one mutation row from bin/fm-mutation-verdict.sh --checks-row; none is present"
    return 0
  fi
  if [ "$count" -gt 1 ]; then
    fm_pstack_fail "the Checks table has $count mutation rows; it needs exactly one"
    return 0
  fi
  case "$mut_cmd" in
    "bin/fm-mutation-verdict.sh"|"bin/fm-mutation-verdict.sh "*) ;;
    *) fm_pstack_fail "the mutation row's command is not bin/fm-mutation-verdict.sh, so the verdict was not written by it: $mut_cmd" ;;
  esac
  case "$mut_result" in
    "passed: "*)
      [ "$unsupported" -eq 0 ] \
        || fm_pstack_fail "the mutation row says passed, but $unsupported changed path(s) have no mutation surface, and the verdict never passes such a change"
      ;;
    "not verified: "*) ;;
    *) fm_pstack_fail "the mutation row's result is not a verdict (it must start 'passed: ' or 'not verified: '): $mut_result" ;;
  esac
}

# A check is cheap when its name or command names a lint or type tool, so a
# `--rerun` re-executes it. eslint and pylint contain "lint", so they need no
# entry of their own.
fm_proof_is_cheap_check() {  # <check> <command>
  local text
  text=$(printf '%s %s' "$1" "$2" | tr '[:upper:]' '[:lower:]')
  case "$text" in
    *lint*|*type*|*ruff*|*mypy*|*clippy*|*tsc*|*'cargo check'*) return 0 ;;
    *) return 1 ;;
  esac
}

# Runs one check command in <worktree> with its output discarded and stdin
# closed, so it cannot read the caller's input. Prints `pass` for exit 0,
# `not executed` when the shell could not run the command (126 or 127), and
# `fail` for any other exit.
fm_proof_rerun_status() {  # <worktree> <command>
  local rc=0
  (cd "$1" && bash -c "$2") </dev/null >/dev/null 2>&1 || rc=$?
  case "$rc" in
    0) printf 'pass' ;;
    126|127) printf 'not executed' ;;
    *) printf 'fail' ;;
  esac
}

# --rerun: re-executes each cheap Checks row of <proof> in <worktree> and
# compares its outcome with the recorded result. Prints a block per row, then
# the summary line, and returns 1 on any mismatch. Read-only: the record is
# never written.
fm_proof_rerun() {  # <worktree> <proof>
  local wt=$1 section row check cmd result rec status n i
  local us=$'\037' checked=0 passed=0 failed=0
  local -a f
  section=$(fm_pstack_proof_section "$2" "## Checks")
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    IFS=$us read -ra f <<< "$row"
    n=${#f[@]}
    [ "$n" -ge 4 ] || continue
    check=${f[0]}
    # Cells between check and result rejoin into the command; a markdown `\|`
    # is a literal pipe in the command, so unescape it before running.
    cmd=${f[1]}
    for ((i = 2; i <= n - 3; i++)); do cmd="$cmd | ${f[i]}"; done
    cmd=${cmd//\\|/|}
    result=${f[$((n - 2))]}
    if ! fm_proof_is_cheap_check "$check" "$cmd"; then
      printf 'Check: %s\n  Note:     not re-run (not a lint or type check)\n' "$check"
      continue
    fi
    case "$result" in
      pass:*) rec=pass ;;
      fail:*) rec=fail ;;
      info:*)
        printf 'Check: %s\n  Note:     skipped (informational, not compared)\n' "$check"
        continue
        ;;
      *) rec=unrecognized ;;
    esac
    checked=$((checked + 1))
    status=$(fm_proof_rerun_status "$wt" "$cmd")
    printf 'Check: %s\n  Command:  %s\n  Recorded: %s\n  Re-run:   %s\n' "$check" "$cmd" "$rec" "$status"
    [ "$status" != "not executed" ] || printf 'error: command not found or failed to execute: %s\n' "$cmd"
    [ "$rec" != unrecognized ] || printf 'error: result field format unrecognized: %s\n' "$result"
    if [ "$rec" = "$status" ]; then
      printf '  Result:   ✅ match\n'
      passed=$((passed + 1))
    else
      printf '  Result:   ❌ mismatch\n'
      failed=$((failed + 1))
    fi
  done <<< "$(fm_proof_table_rows "$section")"
  [ "$checked" -gt 0 ] || printf 'info: no cheap checks to re-run\n'
  printf -- '--rerun summary: %s checked, %s passed, %s failed\n' "$checked" "$passed" "$failed"
  [ "$failed" -eq 0 ]
}

do_proof_check() {  # <id> [--rerun] [--require-mutation-row] [--check-body <file>] [--check-comment <file>]
  local usage="usage: fm-pstack.sh proof-check <task-id> [--rerun] [--require-mutation-row] [--check-body <file>] [--check-comment <file>]"
  [ "$#" -ge 1 ] || fm_pstack_refuse 2 "$usage" \
    "call proof-check with exactly the task id whose proof record is being checked"
  local check_body='' check_comment='' require_mutation=0 rerun=0
  if [ "$1" = --rerun ]; then
    rerun=1
    shift
    [ "$#" -ge 1 ] || fm_pstack_refuse 2 "$usage" \
      "proof-check --rerun needs the task id after the flag"
  fi
  local id=$1
  shift
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --rerun)
        rerun=1
        shift
        ;;
      --require-mutation-row)
        require_mutation=1
        shift
        ;;
      --check-body|--check-comment)
        [ "$#" -ge 2 ] && [ -n "$2" ] || fm_pstack_refuse 2 "$usage" \
          "$1 needs the path of the file to compare"
        if [ "$1" = --check-body ]; then check_body=$2; else check_comment=$2; fi
        shift 2
        ;;
      *) fm_pstack_refuse 2 "$usage" \
          "proof-check takes one task id and optionally --rerun, --require-mutation-row, --check-body and --check-comment, the last two each with a file" ;;
    esac
  done
  local wt branch branch_head wt_branch nm_out sel run_id run_status
  local head_sha candidate base supersedes sec proof meta brief_path
  case "$id" in
    ""|.*|*/*) fm_pstack_refuse 2 \
      "usage: fm-pstack.sh proof-check <task-id>" \
      "the proof record is checked for a firstmate task id" ;;
  esac
  meta="$STATE/$id.meta"
  proof="$DATA/$id/moa-proof.md"
  if [ ! -f "$meta" ]; then
    fm_pstack_refuse 1 \
      "no task record at $meta, so no pstack proof can be checked for $id" \
      "proof-check applies to a dispatched ship task's record"
  fi
  [ "$(fm_meta_get "$meta" kind)" = ship ] \
    || fm_pstack_fail "the task record for $id is not kind=ship, so there is no proof candidate to check"
  [ "$(fm_meta_get "$meta" workflow)" = pstack ] \
    || fm_pstack_fail "the task record for $id does not carry workflow=pstack, so the proof gate does not apply"
  branch=$(fm_meta_get "$meta" branch)
  [ -n "$branch" ] || branch="$FM_LEGACY_BRANCH_PREFIX$id"
  wt=$(fm_meta_get "$meta" worktree)
  if [ -z "$wt" ] || [ ! -d "$wt" ]; then
    fm_pstack_fail "the task record for $id names no existing worktree (worktree=$wt), so the candidate cannot be anchored"
    wt=
  elif ! git -C "$wt" rev-parse --git-dir >/dev/null 2>&1; then
    fm_pstack_fail "the task record for $id names $wt, which is not a git copy"
    wt=
  fi
  if [ "$rerun" -eq 1 ] && [ -z "$wt" ]; then
    fm_pstack_refuse 3 "worktree not available; skipping re-run" \
      "the task record must name an existing git copy of the ship branch; restore that worktree, then run proof-check --rerun again"
  fi

  if [ ! -f "$proof" ]; then
    fm_pstack_refuse 1 \
      "no proof record at $proof" \
      "write it first with the exact template command from your Definition of done, then run proof-check again"
  fi

  entry=$(fm_pstack_proof_field "$proof" entry)
  case "$entry" in
    ""|*[[:space:]]*|'<'*) fm_pstack_fail "the proof record's entry field is missing, contains whitespace, or is an unfilled placeholder: \"$entry\"" ;;
  esac
  case "$entry" in
    *:*) ;;
    *) fm_pstack_fail "the proof record's entry field \"$entry\" is not a <plugin>:<skill> entry" ;;
  esac
  case "$(fm_pstack_proof_field "$proof" playbook)" in
    ""|*[[:space:]]*|'<'*) fm_pstack_fail "the proof record's playbook field is missing, contains whitespace, or is an unfilled placeholder" ;;
  esac
  case "$(fm_pstack_proof_field "$proof" task)" in
    "$id") ;;
    *) fm_pstack_fail "the proof record's task field does not name $id" ;;
  esac
  case "$(fm_pstack_proof_field "$proof" moa-proof)" in
    v2) ;;
    *) fm_pstack_fail "the proof record does not open with moa-proof: v2" ;;
  esac

  # Extract v2 fields
  brief=$(fm_pstack_proof_field "$proof" brief)
  source=$(fm_pstack_proof_field "$proof" source)
  no_mistakes=$(fm_pstack_proof_field "$proof" no-mistakes)
  case "$brief" in
    "") fm_pstack_fail "the proof record's brief field is empty" ;;
    '<'*) fm_pstack_fail "the proof record's brief field is an unfilled placeholder" ;;
    *)
      brief_path=$(fm_pstack_brief_path "$brief")
      [ -f "$brief_path" ] || fm_pstack_fail "the proof record's brief $brief does not exist at $brief_path"
      ;;
  esac
  case "$source" in
    "") fm_pstack_fail "the proof record's source field is empty" ;;
    '<'*) fm_pstack_fail "the proof record's source field is an unfilled placeholder" ;;
  esac
  case "$no_mistakes" in
    "") fm_pstack_fail "the proof record's no-mistakes field is empty" ;;
    '<'*) fm_pstack_fail "the proof record's no-mistakes field is an unfilled placeholder" ;;
    pass|failed|off) ;;
    skipped:*)
      nm_reason=$(fm_pstack_trim "${no_mistakes#skipped:}")
      [ -n "$nm_reason" ] || fm_pstack_fail "the proof record's no-mistakes field says skipped with no reason; write skipped: <reason>" ;;
    *) fm_pstack_fail "the proof record's no-mistakes field must be pass, failed, skipped: <reason>, or off (got: $no_mistakes)" ;;
  esac

  head_sha=
  candidate=$(fm_pstack_proof_field "$proof" candidate)
  base=$(fm_pstack_proof_field "$proof" base)
  supersedes=$(fm_pstack_proof_field "$proof" supersedes)
  case "$supersedes" in
    '<'*) supersedes= ;;  # the template's unfilled placeholder line counts as no field
  esac
  if [ -n "$wt" ]; then
    head_sha=$(git -C "$wt" rev-parse HEAD 2>/dev/null) || head_sha=
  fi

  if ! fm_pr_head_valid "$candidate"; then
    fm_pstack_fail "the proof record's candidate is missing or not a full commit sha: \"$candidate\""
  elif [ -n "$head_sha" ] && [ "$candidate" != "$head_sha" ]; then
    fm_pstack_fail "the proof record's candidate $candidate is not the worktree HEAD $head_sha; commit the exact proof candidate first"
  fi

  if [ -n "$wt" ] && [ -n "$head_sha" ]; then
    wt_branch=$(git -C "$wt" rev-parse --symbolic-full-name HEAD 2>/dev/null) || wt_branch=
    branch_head=$(git -C "$wt" rev-parse --verify --quiet "refs/heads/$branch" 2>/dev/null) || branch_head=
    if [ "$wt_branch" = "refs/heads/$branch" ] && [ -n "$branch_head" ] && [ "$branch_head" = "$head_sha" ]; then
      :
    else
      fm_pstack_fail "the worktree is not on the ship branch $branch with its tip at HEAD; commit the exact proof candidate on $branch"
    fi
    if [ -n "$(git -C "$wt" status --porcelain -uno 2>/dev/null || true)" ]; then
      fm_pstack_fail "the worktree has tracked uncommitted changes; commit them so the proof candidate is exact"
    fi
    if ! fm_pr_head_valid "$base"; then
      fm_pstack_fail "the proof record's base is missing or not a full commit sha: \"$base\""
    elif [ -z "$(git -C "$wt" rev-parse --verify --quiet "$base^{commit}" 2>/dev/null)" ]; then
      fm_pstack_fail "the proof record's base $base is not a commit this copy has"
    elif [ "$base" = "$candidate" ]; then
      fm_pstack_fail "the proof record's base $base is the candidate itself; a baseline is a strict ancestor of the change"
    elif ! git -C "$wt" merge-base --is-ancestor "$base" "$candidate" 2>/dev/null; then
      fm_pstack_fail "the proof record's base $base is not an ancestor of the candidate"
    fi
    if [ -n "$supersedes" ]; then
      if [ -z "$(git -C "$wt" rev-parse --verify --quiet "$supersedes^{commit}" 2>/dev/null)" ]; then
        fm_pstack_fail "the proof record's supersedes sha $supersedes is not a commit this copy has"
      elif git -C "$wt" merge-base --is-ancestor "$supersedes" "$candidate" 2>/dev/null; then
        fm_pstack_fail "the proof record's supersedes sha $supersedes is an ancestor of the candidate, so the invalidated work was never discarded"
      fi
    fi
  fi

  for sec in "## Reproduction or baseline" "## Checks" "## Acceptance criteria" "## AI review" "## Scope" "## Not run remaining uncertainty"; do
    if [ -z "$(fm_pstack_trim "$(fm_pstack_proof_section "$proof" "$sec")")" ]; then
      fm_pstack_fail "the proof record's \"$sec\" section is empty"
    fi
  done

  # No active no-mistakes run may hold the branch when the proof is checked:
  # the proof record describes the candidate's own work, and a pending or
  # running run means validation is already under way.
  sel=
  if [ -n "$wt" ] && nm_out=$(fm_nm_run_checked "$wt" 10 axi) && [ -n "$nm_out" ]; then
    sel=$(fm_nm_select_run "$branch" "$nm_out" "$wt" 10)
  fi
  if [ -z "$sel" ]; then
    fm_pstack_fail "the no-mistakes run state could not be read from the worktree (the axi overview failed or was empty)"
    FM_PSTACK_NEXT="rule 7 owns the daemon checks that decide when a pipeline block is real; confirm \`no-mistakes daemon status\` and \`no-mistakes axi status\`, then re-run proof-check"
  else
    case "$sel" in
      absent)
        ;;
      selected\|*)
        run_id=$(printf '%s' "$sel" | cut -d'|' -f2)
        run_status=$(printf '%s' "$sel" | cut -d'|' -f3)
        case "$run_status" in
          pending|running)
            fm_pstack_fail "a no-mistakes run ($run_id, $run_status) is already active on branch $branch; proof-check runs before validation, not alongside it"
            ;;
        esac
        ;;
      *)
        fm_pstack_fail "the no-mistakes run state for branch $branch could not be read ($sel)"
        FM_PSTACK_NEXT="confirm the run state with \`no-mistakes axi status\` and re-run proof-check, or stop and report why the state cannot be read"
        ;;
    esac
  fi

  fm_proof_validate_ac_coverage
  fm_proof_validate_not_verified
  fm_proof_validate_checks_table
  [ "$require_mutation" -eq 0 ] || fm_proof_validate_mutation_row
  fm_proof_validate_scope
  fm_proof_validate_ai_review
  [ -z "$check_body" ] || fm_proof_validate_body "$check_body"
  [ -z "$check_comment" ] || fm_proof_validate_comment "$check_comment"

  if [ -n "$FM_PSTACK_FAILURES" ]; then
    printf '%s\n' "$FM_PSTACK_FAILURES" | while IFS= read -r line; do
      [ -n "$line" ] || continue
      printf 'error: %s\n' "$line"
    done
    if [ -n "$FM_PSTACK_NEXT" ]; then
      printf 'next: %s\n' "$FM_PSTACK_NEXT"
    fi
    exit 1
  fi
  if [ "$rerun" -eq 0 ]; then
    printf 'ok candidate=%s\n' "$candidate"
  elif fm_proof_rerun "$wt" "$proof"; then
    printf 'ok candidate=%s (--rerun: all cheap checks match recorded)\n' "$candidate"
  else
    printf 'error: --rerun detected discrepancies between recorded and re-run results\n'
    exit 1
  fi
}

case "${1:-}" in
  resolve)
    shift
    do_resolve "$@"
    ;;
  supported-harnesses)
    do_supported_harnesses
    ;;
  template)
    shift
    do_template "$@"
    ;;
  render)
    shift
    do_render "$@"
    ;;
  proof-check)
    shift
    do_proof_check "$@"
    ;;
  "")
    fm_pstack_refuse 2 \
      "usage: fm-pstack.sh resolve --harness <harness> | supported-harnesses | template <task-id> | render <task-id> | proof-check <task-id>" \
      "pick one of the five commands; the script's header owns each contract"
    ;;
  *)
    fm_pstack_refuse 2 \
      "unknown command \"$1\" (expected resolve, supported-harnesses, template, render, or proof-check)" \
      "see the usage comment in this script's header for each command's contract"
    ;;
esac