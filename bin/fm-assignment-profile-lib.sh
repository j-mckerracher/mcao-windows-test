# shellcheck shell=bash
# Assignment-profile contract, producer side: the single bash owner for the
# decision record a ship launch carries. docs/assignment-profile-policy.md owns
# the contract, and bin/fm-assignment-profile.py owns schema evaluation and the
# canonical form. Usage: . bin/fm-assignment-profile-lib.sh
#
# A ship brief carries the record as one fenced `assignment-profile` block beside
# its `managed-context` block. bin/fm-brief.sh renders it with
# fm_assignment_profile_section; bin/fm-spawn.sh validates it before any worktree
# or endpoint is bound, pins its target and base after the base is resolved,
# persists the canonical record, and writes the decision keys into the task's
# meta. A relaunch restores that record and refuses on any mismatch.
#
# The trusted code root is this checkout, never a worker's worktree, so the
# revision registry and the policy files it hashes are read from the firstmate
# code tree. Every function that refuses prints one line naming the reason on
# stderr and returns non-zero.

FM_ASSIGNMENT_PROFILE_FENCE='assignment-profile'
FM_ASSIGNMENT_PROFILE_FILE='assignment-profile.json'
# The two lead-ins that carry the rendered obligations into a brief. This file owns
# them: fm_assignment_profile_section and fm_assignment_profile_dod_copy write them,
# and fm_assignment_profile_brief_check reads them back, so the copies cannot drift.
FM_ASSIGNMENT_PROFILE_SECTION_LEAD='The decision below was made at launch and pins the policy revision it names.
Your obligations are exactly the ones it lists, and a smaller diff never lowers them.'
FM_ASSIGNMENT_PROFILE_DOD_LEAD='Assignment profile obligations, from the decision record this launch carries. They add to the definition of done above and a smaller diff never lowers them:'
FM_ASSIGNMENT_PROFILE_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
FM_ASSIGNMENT_PROFILE_PY="$FM_ASSIGNMENT_PROFILE_ROOT/bin/fm-assignment-profile.py"

# Print the body of the single assignment-profile block in <file>.
# Exit 1 when the file carries no block, 2 when it carries more than one.
fm_assignment_profile_block_lines() {  # <file>
  awk '
    /^```assignment-profile$/ {
      if (seen) multi = 1
      seen = 1
      blk = 1
      next
    }
    blk && /^```$/ { blk = 0; next }
    blk { print }
    END {
      if (!seen) exit 1
      if (multi) exit 2
    }
  ' "$1"
}

# Write the record body carried by <brief> to <out>. <caller> names the refuser.
fm_assignment_profile_extract() {  # <brief> <out> <caller>
  local rc=0
  fm_assignment_profile_block_lines "$1" >"$2" || rc=$?
  case $rc in
    0) return 0 ;;
    1) echo "error: $3: $1 carries no assignment-profile block; a ship launch needs the decision record that bin/fm-brief.sh --assignment-profile renders" >&2 ;;
    *) echo "error: $3: $1 carries more than one assignment-profile block; exactly one is allowed" >&2 ;;
  esac
  rm -f -- "$2"
  return 1
}

# Validate the record at <record> (a path, or - for stdin) and print its canonical
# form. Pins are optional: --task <id>, --target-branch <b>, --base-commit <sha>,
# and --require-current. bin/fm-assignment-profile.py owns every refusal reason.
fm_assignment_profile_validate() {  # <record|-> <caller> [pin args...]
  local record=$1 caller=$2 rc=0
  shift 2
  python3 -I "$FM_ASSIGNMENT_PROFILE_PY" validate --code-root "$FM_ASSIGNMENT_PROFILE_ROOT" \
    --record "$record" "$@" || rc=$?
  [ "$rc" -eq 0 ] || { echo "error: $caller: assignment profile refused" >&2; return 1; }
}

# Extract and validate the record a brief carries for task <task>. Pass
# <require-current> 1 for a new launch, so the record must pin the current revision.
# Prints the canonical record.
fm_assignment_profile_load() {  # <brief> <caller> <task> <require-current 0|1>
  local brief=$1 caller=$2 task=$3 require=$4 tmp canon rc=0
  tmp=$(mktemp "${TMPDIR:-/tmp}/fm-assignment-profile.XXXXXX") || return 1
  fm_assignment_profile_extract "$brief" "$tmp" "$caller" || { rm -f -- "$tmp"; return 1; }
  if [ "$require" = 1 ]; then
    canon=$(fm_assignment_profile_validate "$tmp" "$caller" --task "$task" --require-current) || rc=1
  else
    canon=$(fm_assignment_profile_validate "$tmp" "$caller" --task "$task") || rc=1
  fi
  rm -f -- "$tmp"
  [ "$rc" -eq 0 ] || return 1
  printf '%s\n' "$canon"
}

# Check a launch's canonical record against the pins the host resolved: the
# target branch and the comparison-base commit it will start from.
fm_assignment_profile_pin_check() {  # <canonical> <target-branch> <base-commit> <caller>
  printf '%s\n' "$1" | fm_assignment_profile_validate - "$4" --target-branch "$2" --base-commit "$3" >/dev/null
}

# Print the obligations the validated record lists, rendered by
# bin/fm-assignment-profile.py render. The brief and the shared definition of done
# both carry this text, so they cannot disagree with each other or the record.
# <require-current> 1 (the default) is a new launch; 0 is a relaunch, which stays
# pinned to the revision it launched under.
fm_assignment_profile_obligations() {  # <record> <caller> <task> [require-current 0|1]
  local canon
  if [ "${4:-1}" = 1 ]; then
    canon=$(fm_assignment_profile_validate "$1" "$2" --task "$3" --require-current) || return 1
  else
    canon=$(fm_assignment_profile_validate "$1" "$2" --task "$3") || return 1
  fi
  printf '%s\n' "$canon" | python3 -I "$FM_ASSIGNMENT_PROFILE_PY" render \
    --code-root "$FM_ASSIGNMENT_PROFILE_ROOT" --record -
}

# Print the Markdown section bin/fm-brief.sh writes: the obligations rendered from
# the validated record, then the canonical record as the carried block.
fm_assignment_profile_section() {  # <record> <caller> <task>
  local canon obligations
  canon=$(fm_assignment_profile_validate "$1" "$2" --task "$3" --require-current) || return 1
  obligations=$(fm_assignment_profile_obligations "$1" "$2" "$3") || return 1
  cat <<EOF
# Assignment profile

$FM_ASSIGNMENT_PROFILE_SECTION_LEAD

$obligations
\`\`\`$FM_ASSIGNMENT_PROFILE_FENCE
$canon
\`\`\`
EOF
}

# Print the paragraph the shared definition of done carries: its lead-in, then the
# obligations the validated record lists. bin/fm-brief.sh appends it to the DoD.
fm_assignment_profile_dod_copy() {  # <record> <caller> <task>
  local obligations
  obligations=$(fm_assignment_profile_obligations "$1" "$2" "$3") || return 1
  printf '%s\n\n%s\n' "$FM_ASSIGNMENT_PROFILE_DOD_LEAD" "$obligations"
}

# Print <brief> without the text inside its fenced blocks. A fence opens on a line that
# starts with three or more backticks or tildes and closes on a line of the same character,
# at least as long, with nothing after it, as CommonMark does. A backtick fence's info
# string may not hold a backtick, so a line such as "```x``` note" is prose, not a fence.
# Obligation text inside a fenced example is an example, not the brief's own statement, so
# it must not count.
fm_assignment_profile_unfenced() {  # <brief>
  awk '
    function fence(s,   t, n, r) {  # sets FC to the fence character and returns the run length, or 0
      t = s; n = 0
      while (n < 3 && substr(t, 1, 1) == " ") { t = substr(t, 2); n++ }
      FC = substr(t, 1, 1)
      if (FC != "`" && FC != "~") return 0
      r = 0
      while (substr(t, r + 1, 1) == FC) r++
      if (r < 3) return 0
      REST = substr(t, r + 1)
      if (FC == "`" && index(REST, "`")) return 0
      return r
    }
    {
      n = fence($0)
      if (!inside) {
        if (n) { inside = n; ch = FC; next }
        print
      } else if (n >= inside && FC == ch && REST ~ /^[ \t]*$/) {
        inside = 0
      }
    }
  ' "$1"
}

# Check that a brief states the obligations its canonical record renders, in its
# Assignment profile section and, when it carries one, its definition-of-done copy. Only
# text outside fenced blocks counts. A copy that disagrees with the record refuses before
# any endpoint, so the worker never reads an obligation the decision does not carry.
# A fresh launch passes <require-current> 1 and needs both copies. A relaunch passes 0
# (see obligations) and accepts the section alone, the shape a brief has when a promotion
# rewrote it or it launched before the definition-of-done copy existed; the section is
# still checked, and any copy present must still agree.
fm_assignment_profile_brief_check() {  # <brief> <canonical> <task> <caller> [require-current 0|1]
  local tmp obligations text
  tmp=$(mktemp "${TMPDIR:-/tmp}/fm-assignment-profile.XXXXXX") || return 1
  printf '%s\n' "$2" >"$tmp"
  if ! obligations=$(fm_assignment_profile_obligations "$tmp" "$4" "$3" "${5:-1}"); then
    rm -f -- "$tmp"
    return 1
  fi
  rm -f -- "$tmp"
  text=$(fm_assignment_profile_unfenced "$1") || return 1
  case $text in
    *"$FM_ASSIGNMENT_PROFILE_SECTION_LEAD"$'\n\n'"$obligations"*) ;;
    *) echo "error: $4: the brief's Assignment profile section does not state the obligations its decision record lists; refusing to launch" >&2
       return 1 ;;
  esac
  if [ "${5:-1}" = 0 ]; then
    case $text in
      *"$FM_ASSIGNMENT_PROFILE_DOD_LEAD"*) ;;
      *) return 0 ;;
    esac
  fi
  case $text in
    *"$FM_ASSIGNMENT_PROFILE_DOD_LEAD"$'\n\n'"$obligations"*) ;;
    *) echo "error: $4: the brief's definition-of-done copy does not state the obligations its decision record lists; refusing to launch" >&2
       return 1 ;;
  esac
}

# Print the persisted record path inside task data directory <task-dir>.
fm_assignment_profile_path() {  # <task-dir>
  printf '%s/%s\n' "$1" "$FM_ASSIGNMENT_PROFILE_FILE"
}

fm_assignment_profile_sha() {  # <file>
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else sha256sum "$1" | awk '{print $1}'
  fi
}

# Persist canonical record <canonical> to <dest>, atomically, so a relaunch never
# reads a partial file.
fm_assignment_profile_persist() {  # <dest> <canonical>
  local dest=$1 tmp="$1.tmp.${BASHPID:-$$}"
  if printf '%s\n' "$2" >"$tmp" && mv -f -- "$tmp" "$dest"; then return 0; fi
  rm -f -- "$tmp"
  echo "error: could not persist the assignment profile to $dest" >&2
  return 1
}

# Print the task-record lines for canonical record <canonical> persisted under
# hash <sha>.
fm_assignment_profile_meta() {  # <canonical> <sha>
  printf '%s\n' "$1" | python3 -I "$FM_ASSIGNMENT_PROFILE_PY" meta \
    --code-root "$FM_ASSIGNMENT_PROFILE_ROOT" --record - || return 1
  echo "profile_record_sha256=$2"
}

# Restore a relaunch's record. Refuses when the persisted file is missing, when
# its hash differs from the task record, or when the brief carries a record that
# is not byte-identical to the persisted one. Prints the canonical record.
fm_assignment_profile_restore_check() {  # <persisted> <expected-sha> <brief> <task> <caller>
  local persisted=$1 want=$2 brief=$3 task=$4 caller=$5 got canon brief_canon tmp
  if [ ! -f "$persisted" ] || [ -L "$persisted" ]; then
    echo "error: $caller: task $task has no persisted assignment profile at $persisted; a ship without its decision record stays blocked" >&2
    return 1
  fi
  got=$(fm_assignment_profile_sha "$persisted") || return 1
  if [ -z "$want" ] || [ "$got" != "$want" ]; then
    echo "error: $caller: the persisted assignment profile for $task no longer matches its recorded hash; refusing to relaunch" >&2
    return 1
  fi
  brief_canon=$(fm_assignment_profile_load "$brief" "$caller" "$task" 0) || return 1
  tmp=$(mktemp "${TMPDIR:-/tmp}/fm-assignment-profile.XXXXXX") || return 1
  printf '%s\n' "$brief_canon" >"$tmp"
  if ! cmp -s -- "$tmp" "$persisted"; then
    rm -f -- "$tmp"
    echo "error: $caller: the launch brief carries an assignment profile that differs from the one task $task launched with; refusing to relaunch" >&2
    return 1
  fi
  rm -f -- "$tmp"
  canon=$(cat -- "$persisted") || return 1
  printf '%s\n' "$canon"
}
