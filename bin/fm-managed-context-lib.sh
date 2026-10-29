# shellcheck shell=bash
# Managed-context contract: the single owner of the fenced `managed-context`
# block that a Motif-managed ship launch carries in its launch brief.
# Usage: . bin/fm-managed-context-lib.sh
#
# bin/fm-brief.sh scaffolds the block into a ship brief, bin/fm-promote.sh writes
# it into a promoted scout's ship instructions, and bin/fm-spawn.sh validates it
# before any endpoint, worktree, or record exists, binds the real worktree path
# into the launch brief, and records the fields in state/<id>.meta. Every
# consumer reads the same field list, closed value sets, and refusal wording
# from here. docs/managed-context.md is the human-readable contract.
#
# The block is a fenced section whose body is one `field: value` line per field.
# Each refusal names the field it refuses, and none of them starts a standalone
# hve launch: a ship without a valid block does not launch at all.

FM_MANAGED_CONTEXT_OWNER=motif
FM_MANAGED_CONTEXT_FIELDS='ownership task_id worktree delivery_mode helper_allowance'
FM_MANAGED_CONTEXT_MODES='no-mistakes direct-PR'
FM_MANAGED_CONTEXT_HELPERS='read-only-checks isolated-scratch'
FM_MANAGED_CONTEXT_PLACEHOLDER='{WORKTREE}'

# Trim leading and trailing blanks from stdin's single line.
fm_managed_context_trim() {  # <text>
  local s=$1
  s=${s#"${s%%[![:space:]]*}"}
  s=${s%"${s##*[![:space:]]}"}
  printf '%s' "$s"
}

# Print the Managed context section a ship brief carries, with the worktree
# left as the placeholder that the launch replaces. The helper allowance is the
# only helper execution this contract grants: read-only checks and isolated
# scratch output. A second real checkout is reported to Motif, never run.
fm_managed_context_section() {  # <task-id> <mode>
  printf '%s\n' \
    '# Managed context' \
    'This block is the assignment record Motif routes by. Firstmate binds the worktree line when it launches this worker; the helper allowance covers read-only checks and isolated scratch output only.' \
    '```managed-context' \
    "ownership: $FM_MANAGED_CONTEXT_OWNER" \
    "task_id: $1" \
    "worktree: $FM_MANAGED_CONTEXT_PLACEHOLDER" \
    "delivery_mode: $2" \
    'helper_allowance: read-only-checks, isolated-scratch' \
    '```'
}

# Refuse a delivery mode that the managed-context contract does not support for
# ship launches. The two retained modes are the only ones accepted; local-only
# and any other value are named in the refusal.
fm_managed_context_mode_valid() {  # <mode> <caller>
  local mode=$1 caller=$2
  case " $FM_MANAGED_CONTEXT_MODES " in
    *" $mode "*) return 0 ;;
  esac
  echo "error: $caller: delivery mode '$mode' is not supported for ship launches under the managed-context contract; the supported modes are no-mistakes and direct-PR (docs/managed-context.md)" >&2
  return 1
}

# Print the body lines of the single managed-context block in <file>.
# Exit 1 when the file carries no block, 2 when it carries more than one.
fm_managed_context_block_lines() {  # <file>
  awk '
    /^```managed-context$/ {
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

# Print the value of one field of the single managed-context block in <file>.
# Empty output when the field is absent or the block is missing or repeated.
fm_managed_context_field() {  # <file> <field>
  local body line key val
  body=$(fm_managed_context_block_lines "$1") || return 0
  while IFS= read -r line; do
    key=${line%%:*}
    [ "$key" != "$line" ] || continue
    key=$(fm_managed_context_trim "$key")
    [ "$key" = "$2" ] || continue
    val=${line#*:}
    fm_managed_context_trim "$val"
    printf '\n'
    return 0
  done <<EOF
$body
EOF
}

# Validate the managed-context block in <file> against the task this spawn
# launches and the mode it launches with. The refusal names the field that is
# absent, invalid, or contradictory. The worktree field must be the placeholder:
# the launch fills in the worktree it allocates (fm_managed_context_bind), so a
# literal path in the brief is refused as contradicting that allocation.
fm_managed_context_validate() {  # <file> <task-id> <mode> <caller>
  local file=$1 task=$2 mode=$3 caller=$4
  local body rc line key val
  local owner='' task_field='' worktree='' delivery='' helpers='' seen=' '
  body=$(fm_managed_context_block_lines "$file")
  rc=$?
  case $rc in
    0) ;;
    1)
      echo "error: $caller: $file has no managed-context block; a ship launch needs the block the contract requires, and no standalone hve launch stands in for it (docs/managed-context.md)" >&2
      return 1 ;;
    *)
      echo "error: $caller: $file carries more than one managed-context block; contradictory managed context, keep exactly one" >&2
      return 1 ;;
  esac
  while IFS= read -r line; do
    [ -n "$(fm_managed_context_trim "$line")" ] || continue
    case "$line" in
      *:*) ;;
      *)
        echo "error: $caller: managed-context line '$line' is not a 'field: value' pair" >&2
        return 1 ;;
    esac
    key=$(fm_managed_context_trim "${line%%:*}")
    val=$(fm_managed_context_trim "${line#*:}")
    case " $seen " in
      *" $key "*)
        echo "error: $caller: managed-context field '$key' appears more than once; contradictory managed context" >&2
        return 1 ;;
    esac
    seen="$seen$key "
    case "$key" in
      ownership) owner=$val ;;
      task_id) task_field=$val ;;
      worktree) worktree=$val ;;
      delivery_mode) delivery=$val ;;
      helper_allowance) helpers=$val ;;
      *)
        echo "error: $caller: managed-context field '$key' is not a managed-context field (expected: $FM_MANAGED_CONTEXT_FIELDS)" >&2
        return 1 ;;
    esac
  done <<EOF
$body
EOF
  local field fields nl=$'\n'
  fields=${FM_MANAGED_CONTEXT_FIELDS// /$nl}
  while IFS= read -r field; do
    case " $seen " in
      *" $field "*) ;;
      *)
        echo "error: $caller: managed-context is missing field '$field'" >&2
        return 1 ;;
    esac
  done <<EOF
$fields
EOF
  if [ "$owner" != "$FM_MANAGED_CONTEXT_OWNER" ]; then
    echo "error: $caller: managed-context field 'ownership' is '$owner'; the only supported ownership is $FM_MANAGED_CONTEXT_OWNER" >&2
    return 1
  fi
  if [ "$task_field" != "$task" ]; then
    echo "error: $caller: managed-context field 'task_id' is '$task_field' but this spawn launches task '$task'; contradictory managed context" >&2
    return 1
  fi
  if [ "$worktree" != "$FM_MANAGED_CONTEXT_PLACEHOLDER" ]; then
    echo "error: $caller: managed-context field 'worktree' is '$worktree' but the brief must carry $FM_MANAGED_CONTEXT_PLACEHOLDER; the spawn binds the worktree it allocates, so a literal path contradicts that allocation" >&2
    return 1
  fi
  fm_managed_context_mode_valid "$delivery" "$caller" || return 1
  if [ "$delivery" != "$mode" ]; then
    echo "error: $caller: managed-context field 'delivery_mode' is '$delivery' but this spawn launches mode=$mode; contradictory managed context" >&2
    return 1
  fi
  fm_managed_context_helpers_valid "$helpers" "$caller" || return 1
  return 0
}

# Validate a helper allowance: a comma-separated list, nonempty, without
# repeats, drawn only from the read-only checks and isolated scratch set. The
# caller passes the field value; a valid list prints nothing.
fm_managed_context_helpers_valid() {  # <value> <caller>
  local value=$1 caller=$2 item seen=' '
  [ -n "$value" ] || {
    echo "error: $caller: managed-context field 'helper_allowance' is empty; grant read-only-checks and isolated-scratch, or both" >&2
    return 1
  }
  while IFS= read -r item; do
    item=$(fm_managed_context_trim "$item")
    [ -n "$item" ] || {
      echo "error: $caller: managed-context field 'helper_allowance' has an empty entry; list read-only-checks and isolated-scratch separated by commas" >&2
      return 1
    }
    case "$item" in
      *[[:space:]]*)
        echo "error: $caller: managed-context field 'helper_allowance' entry '$item' contains a space; separate the allowance entries with commas" >&2
        return 1 ;;
    esac
    case " $FM_MANAGED_CONTEXT_HELPERS " in
      *" $item "*) ;;
      *)
        echo "error: $caller: managed-context field 'helper_allowance' names '$item', which is not allowed; the allowance is read-only-checks and isolated-scratch only" >&2
        return 1 ;;
    esac
    case " $seen " in
      *" $item "*)
        echo "error: $caller: managed-context field 'helper_allowance' repeats '$item'" >&2
        return 1 ;;
    esac
    seen="$seen$item "
  done <<EOF
$(printf '%s\n' "$value" | tr ',' '\n')
EOF
  return 0
}

# Helper classes a worker may request, each paired with the one allowance token
# that grants it, as class:token pairs. This is the host-side contract: a class
# with no pair is unknown, and a class whose token the brief's helper_allowance
# does not grant is denied. Nothing else grants helper execution.
FM_MANAGED_CONTEXT_HELPER_CLASSES='grounding:read-only-checks design-critique:read-only-checks bounded-check-execution:read-only-checks design-sketch:isolated-scratch candidate-scratch-output:isolated-scratch'

# Print the allowance token a helper class requires. Exit 1 for an unknown class.
fm_managed_context_helper_token() {  # <helper-class>
  local pair
  for pair in $FM_MANAGED_CONTEXT_HELPER_CLASSES; do
    if [ "${pair%%:*}" = "$1" ]; then
      printf '%s\n' "${pair#*:}"
      return 0
    fi
  done
  return 1
}

# Allow a helper class only when the brief's helper_allowance grants the token the
# class requires. An absent allowance, an allowance without that token, and an
# unknown class all deny, and the refusal names the class and the token it lacks.
fm_managed_context_helper_check() {  # <helper_allowance> <helper-class> <caller>
  local allowance=$1 class=$2 caller=$3 token item
  token=$(fm_managed_context_helper_token "$class") || {
    echo "error: $caller: helper class '$class' is unknown; the contract maps only: ${FM_MANAGED_CONTEXT_HELPER_CLASSES// /, } - a helper of any other class is denied" >&2
    return 1
  }
  while IFS= read -r item; do
    [ "$(fm_managed_context_trim "$item")" = "$token" ] && return 0
  done <<EOF
$(printf '%s\n' "$allowance" | tr ',' '\n')
EOF
  echo "error: $caller: helper class '$class' needs the '$token' allowance, which the brief's helper_allowance does not grant; the helper is denied" >&2
  return 1
}

# Bind the allocated worktree into a launch brief copy. A placeholder worktree
# line is replaced with the allocated path. A literal path must already equal
# it, because a brief that names another worktree contradicts this launch. The
# rewrite goes through a sibling temp file and an atomic rename, so an
# interrupted bind never leaves a half-written launch brief.
fm_managed_context_bind() {  # <launch-brief> <worktree> <caller>
  local file=$1 worktree=$2 caller=$3 current tmp
  case "$worktree" in
    /*) ;;
    *)
      echo "error: $caller: the allocated worktree '$worktree' is not an absolute path; refusing to bind managed context" >&2
      return 1 ;;
  esac
  current=$(fm_managed_context_field "$file" worktree)
  if [ "$current" = "$worktree" ]; then
    return 0
  fi
  if [ "$current" != "$FM_MANAGED_CONTEXT_PLACEHOLDER" ]; then
    echo "error: $caller: managed-context field 'worktree' is '$current' but this launch allocated '$worktree'; contradictory managed context" >&2
    return 1
  fi
  tmp="$file.bind.$$"
  FM_MC_WORKTREE=$worktree awk '
    /^```managed-context$/ { blk = 1; print; next }
    blk && /^```$/ { blk = 0; print; next }
    blk && /^[[:space:]]*worktree[[:space:]]*:/ { print "worktree: " ENVIRON["FM_MC_WORKTREE"]; next }
    { print }
  ' "$file" >"$tmp" || {
    rm -f -- "$tmp"
    echo "error: $caller: could not bind the worktree into $file" >&2
    return 1
  }
  if ! mv "$tmp" "$file"; then
    rm -f -- "$tmp"
    echo "error: $caller: could not publish the bound launch brief $file" >&2
    return 1
  fi
  return 0
}
