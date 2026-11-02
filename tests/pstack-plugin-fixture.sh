#!/usr/bin/env bash
# Fixture builders shared by the pstack worker-workflow suites
# (tests/fm-pstack.test.sh and tests/fm-pstack-lifecycle.test.sh).
#
# Every builder writes a self-contained tree or fake binary under a fixture
# temp root; nothing here reaches the real FM_HOME, the real no-mistakes
# daemon, or any harness installation. fm-pstack.sh resolves the plugin from
# FM_CONFIG_OVERRIDE, so the tests point that variable at the fixture home's
# config directory, exactly as an inherited secondmate home would.

# pstack_write_plugin <dir> [<name>]: write a valid pstack Claude plugin root
# whose plugin.json is parseable JSON with a "name" and whose poteto-mode entry
# skill has model-invocable frontmatter (no disable-model-invocation key).
# Echoes the directory.
pstack_write_plugin() {
  local dir=$1 name=${2:-pstack}
  mkdir -p "$dir/.claude-plugin" "$dir/skills/poteto-mode"
  printf '{"name":"%s"}\n' "$name" > "$dir/.claude-plugin/plugin.json"
  printf -- '%s\n' \
    '---' \
    'name: poteto-mode' \
    'description: the pstack debugging loop' \
    '---' \
    'Run the pstack loop: reproduce, form one hypothesis, change one thing.' \
    > "$dir/skills/poteto-mode/SKILL.md"
  printf '%s\n' "$dir"
}

# pstack_write_plugin_disabled <dir> [<name>]: like pstack_write_plugin, but the
# poteto-mode skill frontmatter sets disable-model-invocation: true, which the
# resolver must refuse as not model-invocable.
pstack_write_plugin_disabled() {
  local dir=$1 name=${2:-pstack}
  mkdir -p "$dir/.claude-plugin" "$dir/skills/poteto-mode"
  printf '{"name":"%s"}\n' "$name" > "$dir/.claude-plugin/plugin.json"
  printf -- '%s\n' \
    '---' \
    'name: poteto-mode' \
    'description: the pstack debugging loop' \
    'disable-model-invocation: true' \
    '---' \
    'Run the pstack loop by asking the operator to invoke it.' \
    > "$dir/skills/poteto-mode/SKILL.md"
  printf '%s\n' "$dir"
}

# pstack_write_plugin_malformed <dir> [<name>]: a plugin root whose plugin.json
# is not parseable JSON.
pstack_write_plugin_malformed() {
  local dir=$1 name=${2:-pstack}
  mkdir -p "$dir/.claude-plugin" "$dir/skills/poteto-mode"
  printf '{oops\n' > "$dir/.claude-plugin/plugin.json"
  printf '%s\n' "$dir"
}

# pstack_write_plugin_no_name <dir>: a parseable plugin.json with no "name".
pstack_write_plugin_no_name() {
  local dir=$1
  mkdir -p "$dir/.claude-plugin" "$dir/skills/poteto-mode"
  printf '{"description":"no name here"}\n' > "$dir/.claude-plugin/plugin.json"
  printf '%s\n' "$dir"
}

# pstack_plugins_folder <dir> [<name>]: a folder that CONTAINS one valid plugin
# at <dir>/a rather than being a plugin root itself - the layout the resolver
# must refuse, because a folder of plugins has no plugin.json to name the entry.
pstack_plugins_folder() {
  local dir=$1
  mkdir -p "$dir"
  pstack_write_plugin "$dir/a" "${2:-pstack}" >/dev/null
  printf '%s\n' "$dir"
}

# pstack_write_plugin_hooks <dir> [<name>]: valid plugin with a hooks/ directory
# in the plugin root (hooks=present in the resolve report).
pstack_write_plugin_hooks() {
  local dir=$1 name=${2:-pstack}
  pstack_write_plugin "$dir" "$name" >/dev/null
  mkdir -p "$dir/hooks"
  printf '%s\n' "$dir"
}

# pstack_write_plugin_mcp_file <dir> [<name>]: valid plugin with a .mcp.json in
# the plugin root (mcp=present via the file form).
pstack_write_plugin_mcp_file() {
  local dir=$1 name=${2:-pstack}
  pstack_write_plugin "$dir" "$name" >/dev/null
  printf '{"mcpServers":{"x":{"command":"true"}}}\n' > "$dir/.mcp.json"
  printf '%s\n' "$dir"
}

# pstack_write_plugin_mcp_json <dir> [<name>]: valid plugin with mcpServers in
# plugin.json itself (mcp=present via the plugin.json form).
pstack_write_plugin_mcp_json() {
  local dir=$1 name=${2:-pstack}
  mkdir -p "$dir/.claude-plugin" "$dir/skills/poteto-mode"
  printf '{"name":"%s","mcpServers":{"x":{"command":"true"}}}\n' "$name" \
    > "$dir/.claude-plugin/plugin.json"
  printf -- '%s\n' '---' 'name: poteto-mode' 'description: the pstack loop' '---' \
    'body' > "$dir/skills/poteto-mode/SKILL.md"
  printf '%s\n' "$dir"
}

# pstack_fake_axi <fakebin> <worktree> <count> <row...>: canonical fake that
# answers `fm_nm_run_checked <wt> 10 axi` (i.e. `no-mistakes axi`) with a
# complete TOON runs overview. Rows are comma-joined id,branch,status,head,pr.
pstack_fake_axi() {
  local fakebin=$1 worktree=$2 count=$3 row rows='' n
  shift 3
  n=0
  for row in "$@"; do
    rows="${rows}  $row
"
    n=$((n + 1))
  done
  {
    # The emitters below write shell source, so their own format strings keep
    # the target script's quotes and expansions literal.
    # shellcheck disable=SC2016
    printf '#!/bin/bash\n'
    # shellcheck disable=SC2016
    printf 'case "$1" in\n'
    printf '  axi)\n'
    printf '    printf "repo: %%s\\n" "%s"\n' "$worktree"
    printf '    printf "count: %%s\\n" "%s"\n' "$count"
    printf '    printf "runs[%%d]{id,branch,status,head,pr}:\\n" "%d"\n' "$n"
    if [ -n "$rows" ]; then
      printf -- '    cat <<'"'"'ROWS'"'"'\n%sROWS\n' "$rows"
    fi
    printf '    ;;\n'
    printf '  *) printf "unsupported args: %%s\\n" "$*"; exit 64 ;;\n'
    printf 'esac\n'
  } > "$fakebin/no-mistakes"
  chmod +x "$fakebin/no-mistakes"
}

# pstack_proof_check_run <home> <task-id> [PATH=...]: run bin/fm-pstack.sh
# proof-check against a fixture home, printing stdout+stderr with exit status
# captured after. Call as: out=$(pstack_proof_check_run ...); rc=$?
pstack_proof_check_run() {
  env FM_ROOT_OVERRIDE="$ROOT" FM_HOME="$1" "$ROOT/bin/fm-pstack.sh" proof-check "$2" 2>&1
}