# shellcheck shell=bash
# fm-verification-readiness-lib.sh - the admission gate for unattended and
# fan-out worker runs: whether a run must prove its project's verification
# capability is ready before it launches, and what the verdict is when it must.
#
# A run is attended only when the operator asked for that run while present and
# no away or quiet posture is active. Every other run is unattended, and a batch
# of two or more workers is fan-out whatever each would be alone. Attended runs
# pass without any check, so a project with no declaration still launches them.
# A relaunch of an existing run and a secondmate are not gated; bin/fm-spawn.sh
# owns that exemption and where the gate runs.
#
# This file is the single owner of the run classification, the declaration
# grammar, the reading of the assignment-profile decision, and the admission
# verdict. docs/configuration.md "Verification readiness" is the operator
# reference. A required check stops the run with the capability it could not
# prove; no retry and no waiver exist, and a stop preserves the record.
#
# Declaration: hve-verification.yml at the project root, committed with the
# project. The grammar is an exact subset of YAML. Keys appear once, in this
# order, and every key is present:
#   contract_version: 1
#   checks:
#     - id: <lowercase-slug>
#       command: <shell command, run from the project root>
#       evidence: <file name written under the evidence directory>
#   environment:
#     - <ENV_NAME>
#   fixtures:
#     - <path relative to the project root>
#   cleanup: driver | none
# An empty list is written `key: []`. Lines that start with `#` and blank lines
# are ignored. A check passes only with exit 0 and a non-empty evidence file.
# Exit 69, 126, and 127 mean the capability is unavailable, exit 75 means
# inconclusive, and any other nonzero exit means failed. Declared environment
# names are checked for presence only; values are never read into a record.
#
# Decision: the brief carries one assignment-profile fenced block, the decision
# record described by docs/assignment-profile-policy.md. Verification is
# required unless the decision is resolved and records tests not required. A
# missing or invalid decision refuses; it never grants permission to skip.

FM_VERIFY_DECLARATION=hve-verification.yml
FM_VERIFY_ERROR=
VERIFY_REQUIREMENT=
VERIFY_DECISION_JSON=null
VERIFY_CHECK_IDS=()
VERIFY_CHECK_CMDS=()
VERIFY_CHECK_EVIDENCE=()
VERIFY_ENV=()
VERIFY_FIXTURES=()
VERIFY_CLEANUP=
VERIFY_TASK=
VERIFY_PROJECT=
VERIFY_CLASS=
VERIFY_RECORD=
VERIFY_EVIDENCE=
VERIFY_RESULTS='[]'

# fm_verify_posture_active <state-dir>: an away or quiet record is present.
fm_verify_posture_active() {
  [ -e "$1/.afk" ] || [ -e "$1/.afk-contract" ]
}

# fm_verify_run_class <attended 0|1> <dispatch-size> <posture-active 0|1>
# Prints fanout, attended, or unattended.
fm_verify_run_class() {
  if [ "$2" -ge 2 ]; then
    echo fanout
  elif [ "$1" -eq 1 ] && [ "$3" -eq 0 ]; then
    echo attended
  else
    echo unattended
  fi
}

# fm_verify_decision_requirement <source-brief> <task-id>
# Sets VERIFY_REQUIREMENT to required or not-required and VERIFY_DECISION_JSON.
# Returns 1 with FM_VERIFY_ERROR when the decision is absent or invalid.
fm_verify_decision_requirement() {
  local brief=$1 id=$2 count json skip
  VERIFY_REQUIREMENT=
  VERIFY_DECISION_JSON=null
  count=$(grep -c '^```assignment-profile$' "$brief" 2>/dev/null) || count=0
  if [ "$count" -ne 1 ]; then
    FM_VERIFY_ERROR="the brief carries $count assignment-profile blocks; it needs exactly one decision record"
    return 1
  fi
  json=$(awk '/^```assignment-profile$/ { blk = 1; next } blk && /^```$/ { exit } blk { print }' "$brief")
  if ! printf '%s\n' "$json" | jq -e --arg id "$id" '
    (type == "object")
    and ((keys - [
      "contract_version", "decision_id", "task_id", "assignment_intent",
      "expected_surfaces", "pinned_policy_revision", "profile", "decision_stage",
      "classification_status", "risk_triggers", "change_coverage", "reasons",
      "required_stages", "evidence_obligations", "target_branch",
      "comparison_base_commit", "evaluated_change_identity", "supersedes"
    ]) == [])
    and (.contract_version == 1)
    and (.task_id == $id)
    and (.profile | IN("documentation-only", "small-low-risk", "standard", "high-risk"))
    and (.classification_status | IN("resolved", "unresolved"))
    and (.evidence_obligations.tests_required | type == "boolean")
  ' >/dev/null 2>&1; then
    FM_VERIFY_ERROR="the assignment-profile block is not valid JSON or does not satisfy docs/assignment-profile-decision.v1.schema.json for task $id"
    return 1
  fi
  VERIFY_DECISION_JSON=$(printf '%s\n' "$json" | jq -c .)
  skip=$(printf '%s\n' "$json" | jq -r '(.evidence_obligations.tests_required | not) and (.classification_status == "resolved")')
  if [ "$skip" = true ]; then
    VERIFY_REQUIREMENT=not-required
  else
    VERIFY_REQUIREMENT=required
  fi
  return 0
}

# fm_verify_declaration_error <line-number> <message>: sets FM_VERIFY_ERROR.
fm_verify_declaration_error() {
  FM_VERIFY_ERROR="line $1: $2"
  return 1
}

# fm_verify_declaration_load <declaration-file>
# Fills the VERIFY_* declaration arrays and VERIFY_CLEANUP. Returns 1 with
# FM_VERIFY_ERROR naming the first line the grammar refuses.
fm_verify_declaration_load() {
  local file=$1 line key value lineno=0 expect=contract_version list='' items=0 field=0
  local ids=" " evidence_names=" " env_names=" "
  VERIFY_CHECK_IDS=()
  VERIFY_CHECK_CMDS=()
  VERIFY_CHECK_EVIDENCE=()
  VERIFY_ENV=()
  VERIFY_FIXTURES=()
  VERIFY_CLEANUP=
  FM_VERIFY_ERROR=
  [ -f "$file" ] || {
    FM_VERIFY_ERROR="no declaration file at $file"
    return 1
  }
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    case "$line" in
    '' | '#'*) continue ;;
    esac
    case "$line" in
    [!\ ]*)
      if [ -n "$list" ]; then
        [ "$items" -gt 0 ] || { fm_verify_declaration_error "$lineno" "$list has no items; write '$list: []' when it is empty"; return 1; }
        [ "$field" -eq 0 ] || { fm_verify_declaration_error "$lineno" "the last check is incomplete; each check needs id, command, and evidence in that order"; return 1; }
        list=
      fi
      case "$line" in
      *:*) ;;
      *) fm_verify_declaration_error "$lineno" "expected 'key: value', found '$line'"; return 1 ;;
      esac
      key=${line%%:*}
      value=${line#*:}
      value=${value# }
      [ "$expect" != end ] || { fm_verify_declaration_error "$lineno" "unexpected key '$key' after cleanup"; return 1; }
      [ "$key" = "$expect" ] || { fm_verify_declaration_error "$lineno" "expected key '$expect', found '$key'; keys appear once, in the documented order"; return 1; }
      case "$key" in
      contract_version)
        [ "$value" = 1 ] || { fm_verify_declaration_error "$lineno" "contract_version must be 1"; return 1; }
        expect=checks
        ;;
      checks | environment | fixtures)
        case "$key" in
        checks) expect=environment ;;
        environment) expect=fixtures ;;
        fixtures) expect=cleanup ;;
        esac
        if [ "$value" = '[]' ]; then
          :
        elif [ -z "$value" ]; then
          list=$key
        else
          fm_verify_declaration_error "$lineno" "$key takes a list; write '$key:' followed by items, or '$key: []'"
          return 1
        fi
        items=0
        field=0
        ;;
      cleanup)
        case "$value" in
        driver | none) VERIFY_CLEANUP=$value ;;
        *) fm_verify_declaration_error "$lineno" "cleanup must be driver or none"; return 1 ;;
        esac
        expect=end
        ;;
      esac
      ;;
    '  - id: '*)
      [ "$list" = checks ] && [ "$field" -eq 0 ] || { fm_verify_declaration_error "$lineno" "a check starts with '  - id:' under checks, after the previous check is complete"; return 1; }
      value=${line#'  - id: '}
      case "$value" in
      '' | -* | *[!a-z0-9-]*) fm_verify_declaration_error "$lineno" "check id '$value' must be lowercase letters, digits, and hyphens"; return 1 ;;
      esac
      case "$ids" in *" $value "*) fm_verify_declaration_error "$lineno" "check id '$value' is declared twice"; return 1 ;; esac
      ids="$ids$value "
      VERIFY_CHECK_IDS+=("$value")
      items=$((items + 1))
      field=1
      ;;
    '    command: '*)
      [ "$list" = checks ] && [ "$field" -eq 1 ] || { fm_verify_declaration_error "$lineno" "a check's command line must follow its id line"; return 1; }
      value=${line#'    command: '}
      case "$value" in
      '' | *$'\t'*) fm_verify_declaration_error "$lineno" "check command must be a nonempty single-line command"; return 1 ;;
      esac
      VERIFY_CHECK_CMDS+=("$value")
      field=2
      ;;
    '    evidence: '*)
      [ "$list" = checks ] && [ "$field" -eq 2 ] || { fm_verify_declaration_error "$lineno" "a check's evidence line must follow its command line"; return 1; }
      value=${line#'    evidence: '}
      case "$value" in
      '' | .* | */* | *[!A-Za-z0-9._-]*) fm_verify_declaration_error "$lineno" "evidence '$value' must be a plain file name made of letters, digits, dot, underscore, and hyphen"; return 1 ;;
      esac
      case "$evidence_names" in *" $value "*) fm_verify_declaration_error "$lineno" "evidence file '$value' is declared twice"; return 1 ;; esac
      evidence_names="$evidence_names$value "
      VERIFY_CHECK_EVIDENCE+=("$value")
      field=0
      ;;
    '  - '*)
      value=${line#'  - '}
      case "$list" in
      environment)
        case "$value" in
        '' | [0-9]* | *[!A-Z0-9_]*) fm_verify_declaration_error "$lineno" "environment name '$value' must be an uppercase identifier"; return 1 ;;
        esac
        case "$env_names" in *" $value "*) fm_verify_declaration_error "$lineno" "environment name '$value' is declared twice"; return 1 ;; esac
        env_names="$env_names$value "
        VERIFY_ENV+=("$value")
        ;;
      fixtures)
        case "$value" in
        '' | /* | *$'\t'*) fm_verify_declaration_error "$lineno" "fixture '$value' must be a nonempty relative path"; return 1 ;;
        esac
        case "/$value/" in */../* | */./* | *//*) fm_verify_declaration_error "$lineno" "fixture '$value' must not contain empty, '.', or '..' segments"; return 1 ;; esac
        VERIFY_FIXTURES+=("$value")
        ;;
      *) fm_verify_declaration_error "$lineno" "a list item must follow environment or fixtures"; return 1 ;;
      esac
      items=$((items + 1))
      ;;
    *) fm_verify_declaration_error "$lineno" "unrecognized line '$line'"; return 1 ;;
    esac
  done <"$file"
  if [ -n "$list" ]; then
    [ "$items" -gt 0 ] || { FM_VERIFY_ERROR="$list has no items at end of file; write '$list: []' when it is empty"; return 1; }
    [ "$field" -eq 0 ] || { FM_VERIFY_ERROR="the last check is incomplete at end of file"; return 1; }
  fi
  [ "$expect" = end ] || { FM_VERIFY_ERROR="the declaration ends before key '$expect'; all five keys are required"; return 1; }
  return 0
}

# fm_verify_classify_exit <exit-code> <evidence-file>: prints one status.
fm_verify_classify_exit() {
  case "$1" in
  0) if [ -s "$2" ]; then echo pass; else echo failed; fi ;;
  69 | 126 | 127) echo unavailable ;;
  75) echo inconclusive ;;
  *) echo failed ;;
  esac
}

# fm_verify_write_record <outcome> <reason> <capability>
fm_verify_write_record() {
  local tmp="$VERIFY_RECORD.tmp.$$"
  mkdir -p "$(dirname "$VERIFY_RECORD")" &&
    jq -n --arg task "$VERIFY_TASK" --arg project "$VERIFY_PROJECT" \
      --arg run_class "$VERIFY_CLASS" --arg outcome "$1" --arg reason "$2" \
      --arg capability "$3" --arg evidence_dir "$VERIFY_EVIDENCE" --arg cleanup "$VERIFY_CLEANUP" \
      --argjson decision "${VERIFY_DECISION_JSON:-null}" \
      --argjson checks "$VERIFY_RESULTS" '
      {
        contract_version: 1, task_id: $task, project: $project, run_class: $run_class,
        outcome: $outcome, reason: $reason, capability: $capability, cleanup: $cleanup,
        decision: $decision, evidence_dir: $evidence_dir, checks: $checks
      }' >"$tmp" &&
    mv "$tmp" "$VERIFY_RECORD"
}

# fm_verify_refuse <reason> <capability> <action>: names the failed capability,
# preserves the record, and returns 1 so the caller stops before launch.
fm_verify_refuse() {
  printf 'error: spawn refused: project %s is not verification-ready for this %s run: %s\n' \
    "$VERIFY_PROJECT" "$VERIFY_CLASS" "$1" >&2
  printf '  capability: %s\n  action: %s\n' "$2" "$3" >&2
  fm_verify_write_record refused "$1" "$2" || echo "warning: could not write $VERIFY_RECORD" >&2
  return 1
}

# fm_verify_admit <task-id> <project-root> <source-brief> <run-class> <task-dir>
# Returns 0 when the run may launch and 1 after a named refusal. Attended runs
# return 0 without reading the project or the decision, and record an exempt
# outcome. Gated runs write <task-dir>/verification-readiness.json whatever the
# outcome: admitted, or refused with the failed capability.
fm_verify_admit() {
  local id=$1 root=$2 brief=$3 class=$4 task_dir=$5 i=0 name check_id cmd ev rc status
  VERIFY_TASK=$id
  VERIFY_PROJECT=$(basename "$root")
  VERIFY_CLASS=$class
  VERIFY_RECORD="$task_dir/verification-readiness.json"
  VERIFY_EVIDENCE="$task_dir/verification"
  VERIFY_RESULTS='[]'
  VERIFY_DECISION_JSON=null
  if [ "$class" = attended ]; then
    fm_verify_write_record exempt "attended run; the readiness gate does not apply" "" || echo "warning: could not write $VERIFY_RECORD" >&2
    return 0
  fi

  if [ ! -f "$root/$FM_VERIFY_DECLARATION" ]; then
    fm_verify_refuse "no $FM_VERIFY_DECLARATION declaration at the project root" \
      "verification declaration $FM_VERIFY_DECLARATION" \
      "commit $FM_VERIFY_DECLARATION to the project declaring its verification checks (docs/configuration.md \"Verification readiness\")"
    return 1
  fi
  if ! fm_verify_declaration_load "$root/$FM_VERIFY_DECLARATION"; then
    fm_verify_refuse "$FM_VERIFY_DECLARATION does not match the declaration grammar: $FM_VERIFY_ERROR" \
      "verification declaration $FM_VERIFY_DECLARATION" \
      "correct $FM_VERIFY_DECLARATION in the project to the documented grammar and retry the spawn"
    return 1
  fi
  if ! fm_verify_decision_requirement "$brief" "$id"; then
    fm_verify_refuse "$FM_VERIFY_ERROR" \
      "assignment-profile decision" \
      "scaffold the brief with a valid assignment-profile block (docs/assignment-profile-policy.md) and retry the spawn"
    return 1
  fi
  if [ "$VERIFY_REQUIREMENT" = not-required ]; then
    VERIFY_RESULTS='[]'
    fm_verify_write_record admitted "assignment-profile records tests not required; no driver ran" "" || echo "warning: could not write $VERIFY_RECORD" >&2
    return 0
  fi
  if [ "${#VERIFY_CHECK_IDS[@]}" -eq 0 ]; then
    fm_verify_refuse "verification is required but $FM_VERIFY_DECLARATION declares no checks" \
      "verification check" \
      "declare the project's verification check in $FM_VERIFY_DECLARATION (a driver that proves behavior, not a compile-only check) and retry the spawn"
    return 1
  fi
  while [ "$i" -lt "${#VERIFY_ENV[@]}" ]; do
    name=${VERIFY_ENV[$i]}
    i=$((i + 1))
    printenv "$name" >/dev/null 2>&1 || {
      fm_verify_refuse "required environment $name is not set" "environment $name" \
        "set $name in the environment that launches the spawn; its value is never recorded"
      return 1
    }
  done
  i=0
  while [ "$i" -lt "${#VERIFY_FIXTURES[@]}" ]; do
    name=${VERIFY_FIXTURES[$i]}
    i=$((i + 1))
    [ -e "$root/$name" ] || {
      fm_verify_refuse "required fixture $name is missing from the project" "fixture $name" \
        "restore $name in the project and retry the spawn"
      return 1
    }
  done
  mkdir -p "$VERIFY_EVIDENCE" || {
    fm_verify_refuse "the evidence directory $VERIFY_EVIDENCE cannot be created" "evidence directory" \
      "make $task_dir writable and retry the spawn"
    return 1
  }
  i=0
  while [ "$i" -lt "${#VERIFY_CHECK_IDS[@]}" ]; do
    check_id=${VERIFY_CHECK_IDS[$i]}
    cmd=${VERIFY_CHECK_CMDS[$i]}
    ev=${VERIFY_CHECK_EVIDENCE[$i]}
    i=$((i + 1))
    rm -f "$VERIFY_EVIDENCE/$ev"
    rc=0
    (cd "$root" && FM_VERIFY_EVIDENCE_DIR=$VERIFY_EVIDENCE bash -c "$cmd") >"$VERIFY_EVIDENCE/.driver-$check_id.log" 2>&1 || rc=$?
    status=$(fm_verify_classify_exit "$rc" "$VERIFY_EVIDENCE/$ev")
    VERIFY_RESULTS=$(jq -n --argjson acc "$VERIFY_RESULTS" --arg id "$check_id" --arg status "$status" \
      --argjson rc "$rc" --arg evidence "$VERIFY_EVIDENCE/$ev" --arg log "$VERIFY_EVIDENCE/.driver-$check_id.log" \
      '$acc + [{id: $id, status: $status, exit_code: $rc, evidence: $evidence, log: $log}]')
    case "$status" in
    pass) ;;
    unavailable)
      fm_verify_refuse "check $check_id could not run (exit $rc; its driver or tool is unavailable)" \
        "verification check $check_id" \
        "make the driver runnable from the project root, then spawn again; the run is not retried or waived; log $VERIFY_EVIDENCE/.driver-$check_id.log"
      return 1
      ;;
    inconclusive)
      fm_verify_refuse "check $check_id was inconclusive (exit $rc)" \
        "verification check $check_id" \
        "resolve the inconclusive result from $VERIFY_EVIDENCE/.driver-$check_id.log, then spawn again; the run is not retried or waived"
      return 1
      ;;
    failed)
      if [ "$rc" -eq 0 ]; then
        fm_verify_refuse "check $check_id exited 0 without its evidence file $ev" "verification check $check_id" \
          "make the driver write $ev under FM_VERIFY_EVIDENCE_DIR; the run is not retried or waived"
      else
        fm_verify_refuse "check $check_id failed (exit $rc)" "verification check $check_id" \
          "fix the project behavior the check proves, then spawn again; log $VERIFY_EVIDENCE/.driver-$check_id.log"
      fi
      return 1
      ;;
    esac
  done
  fm_verify_write_record admitted "" "" || echo "warning: could not write $VERIFY_RECORD" >&2
  return 0
}
