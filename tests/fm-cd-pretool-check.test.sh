#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016
# Behavior tests for the cd-guard PreToolUse seatbelt (docs/cd-guard.md).
#
# bin/fm-cd-command-policy.mjs is the single owner of the block/allow decision;
# it reuses the shell classifier owned by bin/fm-arm-command-policy.mjs.
# bin/fm-cd-pretool-check.sh is the stable transport: it scopes the guard to the
# real primary checkout, then drives all five harness entry forms. This suite
# proves the decision matrix, the harness-output shaping, the primary-checkout
# scoping (including the deliberate secondmate-home difference from the turn-end
# guard), the fail-open transport behavior, the prefilter fast path, the
# end-to-end cwd-leak regression, and the per-harness wiring. No harness is
# spawned; live per-harness evidence lives in docs/cd-guard.md.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

fm_git_identity fmtest fmtest@example.invalid
TMP_ROOT=$(fm_test_tmproot fm-cd-pretool-check)

# A primary-shaped checkout: plain (non-worktree) git repo, AGENTS.md, bin/ with
# the transport plus both policy files (fm-cd-command-policy.mjs imports the
# shared classifier from fm-arm-command-policy.mjs). This is what the transport's
# scoping treats as the real primary firstmate checkout.
install_cd_scripts() {
  local dir=$1
  mkdir -p "$dir/bin"
  cp "$ROOT/bin/fm-cd-pretool-check.sh" "$dir/bin/fm-cd-pretool-check.sh"
  cp "$ROOT/bin/fm-hook-host-lib.sh" "$dir/bin/fm-hook-host-lib.sh"
  cp "$ROOT/bin/fm-cd-command-policy.mjs" "$dir/bin/fm-cd-command-policy.mjs"
  cp "$ROOT/bin/fm-arm-command-policy.mjs" "$dir/bin/fm-arm-command-policy.mjs"
  chmod +x "$dir/bin/fm-cd-pretool-check.sh" "$dir/bin/fm-cd-command-policy.mjs"
}

make_primary_fixture() {
  local dir=$1
  git init -q "$dir"
  git -C "$dir" commit -q --allow-empty -m init
  : > "$dir/AGENTS.md"
  install_cd_scripts "$dir"
  printf '%s\n' "$dir"
}

# Same shape as primary plus the .fm-secondmate-home marker: a secondmate's own
# primary session, which the cd-guard DOES guard (unlike the turn-end guard).
make_secondmate_fixture() {
  local dir=$1
  make_primary_fixture "$dir" >/dev/null
  printf 'sm-cd-1\n' > "$dir/.fm-secondmate-home"
  printf '%s\n' "$dir"
}

# A genuine linked git worktree - the shape bin/fm-spawn.sh hands crewmate/scout
# tasks. git-dir and git-common-dir differ, so the guard must be inert.
make_child_worktree_fixture() {
  local base=$1 dir=$2
  fm_git_worktree "$base" "$dir" fm/cd-guard-test-branch
  : > "$dir/AGENTS.md"
  install_cd_scripts "$dir"
  printf '%s\n' "$dir"
}

PRIMARY=$(make_primary_fixture "$TMP_ROOT/primary")
CHECK="$PRIMARY/bin/fm-cd-pretool-check.sh"

# --- full cross-harness acceptance matrix ----------------------------------

MATRIX_IDS=()
MATRIX_EXPECTED=()
MATRIX_COMMANDS=()

matrix_case() {
  MATRIX_IDS+=("$1")
  MATRIX_EXPECTED+=("$2")
  MATRIX_COMMANDS+=("$3")
}

# BLOCK: a persistent top-level cwd change in the parent shell.
matrix_case B01 deny 'cd projects/foo'
matrix_case B02 deny 'cd ..'
matrix_case B03 deny 'cd'
matrix_case B04 deny 'cd -'
matrix_case B05 deny 'cd /abs/path'
matrix_case B06 deny 'pushd projects/foo'
matrix_case B07 deny 'popd'
matrix_case B08 deny 'X=1 cd projects/foo'
matrix_case B09 deny 'cd projects/foo && tasks-axi add x'
matrix_case B10 deny 'echo before; cd projects/foo'
matrix_case B11 deny 'true && cd projects/foo'
matrix_case B12 deny 'tasks-axi done x || cd projects/foo'
matrix_case B13 deny 'cd "projects/foo"'
matrix_case B14 deny '"cd" projects/foo'
matrix_case B15 deny 'sleep 1 & cd projects/foo'
matrix_case B16 deny 'command cd projects/foo'
matrix_case B17 deny 'cd projects/foo >/dev/null'
matrix_case B18 deny $'cd projects/foo\necho done'
matrix_case B19 deny "\$'\\143d' projects/foo"
matrix_case B20 deny "c'd' projects/foo"
matrix_case B21 deny 'c"d" projects/foo'
matrix_case B22 deny 'c\d projects/foo'
matrix_case B23 deny 'builtin cd projects/foo'
matrix_case B24 deny 'command builtin cd projects/foo'
matrix_case B25 deny 'builtin command cd projects/foo'
matrix_case B26 deny 'command -p cd projects/foo'
matrix_case B27 deny 'command -- cd projects/foo'

# ALLOW: not a persistent top-level cwd change (scoped, data, or non-cd).
matrix_case A01 allow 'git -C projects/foo status'
matrix_case A02 allow 'cat /abs/path/file'
matrix_case A03 allow 'ls projects/foo'
matrix_case A04 allow 'echo "cd projects/foo"'
matrix_case A05 allow 'grep cd file'
matrix_case A06 allow '(cd projects/foo && pwd)'
matrix_case A07 allow "bash -c 'cd projects/foo'"
matrix_case A08 allow 'env -C projects/foo make'
matrix_case A09 allow 'make -C projects/foo build'
matrix_case A10 allow 'find . -execdir cd {} \;'
matrix_case A11 allow 'cd projects/foo | cat'
matrix_case A12 allow 'cat foo | cd bar'
matrix_case A13 allow 'cd projects/foo &'
matrix_case A14 allow 'abcd project'
matrix_case A15 allow 'cdk deploy'
matrix_case A16 allow 'env cd projects/foo'
matrix_case A17 allow 'sudo cd projects/foo'
matrix_case A18 allow 'x=$(cd foo && pwd)'
matrix_case A19 allow 'dirs'
matrix_case A20 allow "echo 'pushd x'"
matrix_case A21 allow 'git checkout main'
matrix_case A22 allow "sh -c 'cd projects/foo && ls'"
matrix_case A23 allow "printf '%s\\n' 'cd projects/foo'"
matrix_case A24 allow 'ls -la'
matrix_case A25 allow './cd projects/foo'
matrix_case A26 allow '/tmp/cd projects/foo'
matrix_case A27 allow '/usr/bin/cd projects/foo'
matrix_case A28 allow './builtin cd projects/foo'
matrix_case A29 allow 'c\d\ projects/foo'
matrix_case A30 allow './command cd projects/foo'
matrix_case A31 allow '/usr/bin/command cd projects/foo'
matrix_case A32 allow '/tmp/builtin cd projects/foo'
matrix_case A33 allow 'command -v cd'
matrix_case A34 allow 'command -V cd'
matrix_case A35 allow 'command -pv cd'
matrix_case A36 allow 'command -vp cd'

MATRIX_TMP=$(mktemp -d "${TMPDIR:-/tmp}/fm-cd-policy-matrix.XXXXXX")
FM_TEST_CLEANUP_DIRS+=("$MATRIX_TMP")

run_matrix_entry() {
  local id=$1 expected=$2 entry=$3 cmd=$4 payload out_file err_file rc
  out_file="$MATRIX_TMP/$id-$entry.out"
  err_file="$MATRIX_TMP/$id-$entry.err"

  case "$entry" in
    codex)
      payload=$(jq -cn --arg command "$cmd" '{tool_name:"Bash",tool_input:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    claude)
      payload=$(jq -cn --arg command "$cmd" '{tool_name:"Bash",tool_input:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" --claude >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    grok)
      payload=$(jq -cn --arg command "$cmd" '{toolName:"run_terminal_command",toolInput:{command:$command}}')
      printf '%s' "$payload" | "$CHECK" >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    opencode|pi)
      "$CHECK" --command "$cmd" >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    stdin)
      printf '%s' "$cmd" | "$CHECK" --command-stdin >"$out_file" 2>"$err_file"
      rc=$?
      ;;
    *)
      fail "unknown matrix entry form: $entry"
      ;;
  esac

  if [ "$expected" = allow ]; then
    [ "$rc" -eq 0 ] || fail "$id via $entry must allow, got exit $rc: $(cat "$err_file")"
    [ ! -s "$out_file" ] || fail "$id via $entry allow must leave stdout empty: $(cat "$out_file")"
    [ ! -s "$err_file" ] || fail "$id via $entry allow must leave stderr empty: $(cat "$err_file")"
    return
  fi

  [ "$rc" -eq 2 ] || fail "$id via $entry must deny, got exit $rc"
  jq -e '.hookSpecificOutput.permissionDecision == "deny" and (.systemMessage | test("\\[persistent-cd\\]"))' "$err_file" >/dev/null 2>&1 \
    || fail "$id via $entry deny must carry the persistent-cd reason code on stderr: $(cat "$err_file")"
  if [ "$entry" = claude ]; then
    [ ! -s "$out_file" ] || fail "$id via claude deny must leave stdout empty: $(cat "$out_file")"
  elif [ "$entry" = grok ]; then
    jq -e '.decision == "deny"' "$out_file" >/dev/null 2>&1 \
      || fail "$id via grok deny must carry decision=deny on stdout: $(cat "$out_file")"
  fi
}

test_full_acceptance_matrix() {
  local i entry
  for ((i = 0; i < ${#MATRIX_IDS[@]}; i++)); do
    for entry in codex claude grok opencode pi stdin; do
      run_matrix_entry "${MATRIX_IDS[$i]}" "${MATRIX_EXPECTED[$i]}" "$entry" "${MATRIX_COMMANDS[$i]}"
    done
  done
  pass "cd-guard acceptance matrix: ${#MATRIX_IDS[@]} cases x 6 entry forms, block/allow all correct"
}

# --- primary-checkout scoping ----------------------------------------------

test_fires_in_secondmate_home() {
  local dir out rc
  dir=$(make_secondmate_fixture "$TMP_ROOT/secondmate")
  out=$("$dir/bin/fm-cd-pretool-check.sh" --claude --command 'cd projects/foo' 2>&1); rc=$?
  expect_code 2 "$rc" "cd-guard must fire in a secondmate's own primary session (unlike the turn-end guard)"
  assert_contains "$out" '[persistent-cd]' "secondmate-home block must carry the reason code"
  pass "cd-guard: fires in a secondmate home (its own primary session is a primary)"
}

test_inert_in_child_worktree() {
  local base dir out rc
  base="$TMP_ROOT/child-base"
  dir="$TMP_ROOT/child-wt"
  make_child_worktree_fixture "$base" "$dir" >/dev/null
  out=$("$dir/bin/fm-cd-pretool-check.sh" --claude --command 'cd projects/foo' 2>&1); rc=$?
  expect_code 0 "$rc" "cd-guard must be inert in a crewmate/scout linked worktree"
  [ -z "$out" ] || fail "cd-guard produced output in a child worktree: $out"
  pass "cd-guard: inert in a crewmate/scout task worktree (linked git worktree)"
}

test_inert_when_not_firstmate_repo() {
  local dir out rc
  dir="$TMP_ROOT/not-firstmate"
  git init -q "$dir"
  git -C "$dir" commit -q --allow-empty -m init
  install_cd_scripts "$dir"   # bin/ present but no AGENTS.md
  out=$("$dir/bin/fm-cd-pretool-check.sh" --claude --command 'cd projects/foo' 2>&1); rc=$?
  expect_code 0 "$rc" "cd-guard must be inert without AGENTS.md (not a firstmate checkout)"
  [ -z "$out" ] || fail "cd-guard produced output outside a firstmate checkout: $out"
  pass "cd-guard: inert in a non-firstmate repo (no AGENTS.md)"
}

test_inert_when_not_a_git_repo() {
  local dir out rc
  dir="$TMP_ROOT/no-git"
  mkdir -p "$dir"
  : > "$dir/AGENTS.md"
  install_cd_scripts "$dir"   # AGENTS.md + bin/ but no git repo
  out=$("$dir/bin/fm-cd-pretool-check.sh" --claude --command 'cd projects/foo' 2>&1); rc=$?
  expect_code 0 "$rc" "cd-guard must be inert when the checkout is not a git repo"
  [ -z "$out" ] || fail "cd-guard produced output in a non-git dir: $out"
  pass "cd-guard: inert when not inside a git repo"
}

# --- end-to-end cwd-leak regression ----------------------------------------

test_e2e_cwd_leak_regression() {
  local sandbox home home_updated leaked out rc
  sandbox="$TMP_ROOT/e2e"
  home="$sandbox/home"
  mkdir -p "$home/data" "$home/projects/clone/data"
  printf '## In flight\n' > "$home/data/backlog.md"

  # Without the guard, the persistent primary shell's cwd leaks: a stray
  # `cd projects/clone` makes the next firstmate-owned backlog write land in the
  # clone, and the home backlog is never updated.
  (
    cd "$home" || fail "cannot enter home"
    cd projects/clone || fail "cannot enter clone"
    printf -- '- [x] demo done\n' >> data/backlog.md
  )
  home_updated=0
  grep -q 'demo done' "$home/data/backlog.md" && home_updated=1
  leaked=0
  grep -q 'demo done' "$home/projects/clone/data/backlog.md" 2>/dev/null && leaked=1
  [ "$home_updated" -eq 0 ] || fail "baseline: home backlog was updated, cwd leak did not reproduce"
  [ "$leaked" -eq 1 ] || fail "baseline: backlog write did not leak into the clone"

  # With the guard, the exact stray command is denied before it can run, so the
  # real harness never lets cwd leave the home.
  out=$("$CHECK" --claude --command 'cd projects/clone' 2>&1); rc=$?
  expect_code 2 "$rc" "guard must deny the stray persistent cd that caused the leak"
  assert_contains "$out" '[persistent-cd]' "leak-preventing block must carry the reason code"
  pass "cd-guard: reproduces the cwd leak and denies the exact command that causes it"
}

# --- fail-open transport behavior ------------------------------------------

test_fail_open_empty_stdin() {
  local out rc
  out=$("$CHECK" < /dev/null 2>&1); rc=$?
  expect_code 0 "$rc" "transport must exit 0 on empty stdin"
  [ -z "$out" ] || fail "transport produced output on empty stdin: $out"
  pass "cd-guard: fails open on empty stdin"
}

# A closed fd 0 (not /dev/null) used to hang --command-stdin: the command substitution's pipe took fd 0
# and cat blocked on it. The watchdog kills a hung run so the suite cannot stall.
test_fail_open_closed_stdin() {
  local out_file pid waited rc
  out_file="$TMP_ROOT/closed-stdin.out"
  (exec 0<&-; exec "$CHECK" --command-stdin) >"$out_file" 2>&1 &
  pid=$!
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.5
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid"
    wait "$pid" 2>/dev/null
    fail "transport hung on a closed stdin with --command-stdin"
  fi
  wait "$pid"; rc=$?
  expect_code 0 "$rc" "transport must exit 0 on a closed stdin with --command-stdin"
  [ ! -s "$out_file" ] || fail "transport produced output on a closed stdin: $(cat "$out_file")"
  pass "cd-guard: fails open promptly on a closed stdin with --command-stdin"
}

test_fail_open_unparseable_json() {
  local out rc
  out=$(printf 'not json at all' | "$CHECK" 2>&1); rc=$?
  expect_code 0 "$rc" "transport must exit 0 on unparseable stdin JSON"
  [ -z "$out" ] || fail "transport produced output on unparseable JSON: $out"
  pass "cd-guard: fails open on unparseable stdin JSON"
}

test_fail_open_missing_node() {
  local fakebin tool tool_path out rc
  fakebin=$(fm_fakebin "$TMP_ROOT/nonode")
  for tool in bash sh git dirname cat printf sed tr jq; do
    tool_path=$(command -v "$tool") || continue
    ln -s "$tool_path" "$fakebin/$tool"
  done
  # node deliberately absent from this PATH.
  out=$(PATH="$fakebin" "$CHECK" --command 'cd projects/foo' 2>&1); rc=$?
  expect_code 0 "$rc" "transport must fail open when node is unavailable"
  [ -z "$out" ] || fail "transport produced output without node: $out"
  pass "cd-guard: fails open (never blocks) when node is missing"
}

test_fail_open_missing_jq_on_stdin() {
  local fakebin tool tool_path out rc
  fakebin=$(fm_fakebin "$TMP_ROOT/nojq")
  for tool in bash sh git dirname cat printf sed tr node; do
    tool_path=$(command -v "$tool") || continue
    ln -s "$tool_path" "$fakebin/$tool"
  done
  # jq deliberately absent: the stdin transport cannot extract the command.
  out=$(printf '{"tool_input":{"command":"cd projects/foo"}}' | PATH="$fakebin" "$CHECK" 2>&1); rc=$?
  expect_code 0 "$rc" "stdin transport must fail open when jq is unavailable"
  [ -z "$out" ] || fail "transport produced output without jq on the stdin path: $out"
  pass "cd-guard: fails open on the stdin path when jq is missing"
}

# --- prefilter fast path ----------------------------------------------------

test_prefilter_skips_node_without_cd_substring() {
  local dir fakebin marker tool tool_path out rc
  dir="$TMP_ROOT/prefilter"
  make_primary_fixture "$dir" >/dev/null
  fakebin=$(fm_fakebin "$TMP_ROOT/prefilter-fake")
  marker="$TMP_ROOT/prefilter-node-called"
  for tool in bash sh git dirname cat printf sed tr jq; do
    tool_path=$(command -v "$tool") || continue
    ln -s "$tool_path" "$fakebin/$tool"
  done
  cat > "$fakebin/node" <<EOF
#!/usr/bin/env bash
: > "$marker"
exit 0
EOF
  chmod +x "$fakebin/node"
  # No cd/pushd/popd substring: the prefilter must fast-allow before scoping or
  # the policy runtime is ever consulted.
  out=$(PATH="$fakebin" "$dir/bin/fm-cd-pretool-check.sh" --command 'git status' 2>&1); rc=$?
  expect_code 0 "$rc" "prefilter must fast-allow a command with no cd/pushd/popd substring"
  [ -z "$out" ] || fail "prefilter fast-allow produced output: $out"
  [ ! -e "$marker" ] || fail "prefilter fast-allow still invoked the node policy owner"
  pass "cd-guard: prefilter fast-allows (skips node) when no cd/pushd/popd substring is present"
}

# --- policy CLI contract ----------------------------------------------------

test_policy_cli_direct() {
  local policy
  policy="$ROOT/bin/fm-cd-command-policy.mjs"
  [ "$(node "$policy" --command 'cd projects/foo' | cut -f1)" = deny ] \
    || fail "policy CLI must deny a bare top-level cd"
  [ "$(printf '%s' 'cd projects/foo' | node "$policy" --command-stdin | cut -f1)" = deny ] \
    || fail "policy CLI must deny a bare top-level cd read from --command-stdin"
  [ "$(node "$policy" --command 'git -C projects/foo status')" = allow ] \
    || fail "policy CLI must allow git -C"
  [ "$(node "$policy" --command '(cd projects/foo && pwd)')" = allow ] \
    || fail "policy CLI must allow a subshell-local cd"
  [ "$(node "$policy")" = allow ] \
    || fail "policy CLI must allow when no command is supplied"
  pass "cd-guard: fm-cd-command-policy.mjs CLI honors the deny/allow output contract"
}

# --- oversized command transport (DPES-994) ---------------------------------

# A command larger than the process argument limit must still reach the policy owner on stdin.
test_oversized_command_stdin_transport() {
  local payload out err rc
  payload="$TMP_ROOT/oversized-stdin.txt"
  out="$TMP_ROOT/oversized-stdin.out"
  err="$TMP_ROOT/oversized-stdin.err"
  { printf 'cd projects/foo # '; head -c 1200000 /dev/zero | tr '\0' x; printf '\n'; } > "$payload"
  "$CHECK" --claude --command-stdin < "$payload" >"$out" 2>"$err"; rc=$?
  expect_code 2 "$rc" "oversized protected cd must deny when the command arrives on stdin"
  assert_contains "$(cat "$err")" '[persistent-cd]' "oversized protected deny must carry the reason code"
  { printf 'git -C projects/foo status # cd '; head -c 1200000 /dev/zero | tr '\0' x; printf '\n'; } > "$payload"
  "$CHECK" --claude --command-stdin < "$payload" >"$out" 2>"$err"; rc=$?
  expect_code 0 "$rc" "oversized benign command must allow when the command arrives on stdin"
  [ ! -s "$out" ] && [ ! -s "$err" ] || fail "oversized benign allow must be silent"
  pass "cd-guard: oversized (1.2 MB) protected command denies and benign allows over --command-stdin"
}

# Adapter fixtures are primary-shaped checkouts. Pi and omp resolve their root from their own
# extension location, so their extension trees are copied in and the root resolves to the fixture.
make_adapter_fixture() {
  local dir=$1
  make_primary_fixture "$dir" >/dev/null
  cp "$ROOT/bin/fm-arm-pretool-check.sh" "$dir/bin/fm-arm-pretool-check.sh"
  chmod +x "$dir/bin/fm-arm-pretool-check.sh"
  cp -R "$ROOT/.pi" "$dir/.pi"
  cp -R "$ROOT/.omp" "$dir/.omp"
  printf '%s\n' "$dir"
}

# Classifier-failure fixture: the policy owner records the bytes it received, then fails like a crashed runtime.
install_failing_classifier() {
  local dir=$1
  cat > "$dir/bin/fm-cd-command-policy.mjs" <<'EOF'
#!/usr/bin/env node
import { appendFileSync, readFileSync } from "node:fs";
const received = Buffer.byteLength(readFileSync(0, "utf8"));
appendFileSync(new URL("../classifier-received-bytes", import.meta.url), `${received}\n`);
process.stderr.write("simulated classifier failure\n");
process.exitCode = 1;
EOF
}

# Drives the real OpenCode, Pi, and omp cd-guard adapters against a primary fixture (real classifier) and a
# classifier-failure fixture. Prints each verdict; for the failure fixture also whether the classifier received
# the full command byte-for-byte.
test_adapters_send_cd_command_on_stdin() {
  local primary failing out expected
  command -v node >/dev/null 2>&1 || { pass "node not installed, skipping adapter cd-guard stdin check"; return; }
  primary=$(make_adapter_fixture "$TMP_ROOT/adapter-primary")
  failing=$(make_adapter_fixture "$TMP_ROOT/adapter-failing")
  install_failing_classifier "$failing"
  out=$(FM_PRIMARY="$primary" FM_FAILING="$failing" OPENCODE_CD="$ROOT/.opencode/plugins/fm-primary-cd-check.js" \
    node --input-type=module -e '
      import { pathToFileURL } from "node:url";
      import { existsSync, readFileSync, rmSync } from "node:fs";
      import { join } from "node:path";
      const pad = "x".repeat(1200000);
      const cases = [
        ["protected_oversized", `cd projects/foo # ${pad}`],
        ["benign_oversized", `git -C projects/foo status # cd ${pad}`],
        ["protected_small", "cd projects/foo"],
      ];
      const roots = [["primary", process.env.FM_PRIMARY], ["failing", process.env.FM_FAILING]];
      const { FmPrimaryCdCheck } = await import(pathToFileURL(process.env.OPENCODE_CD).href);
      const extensionHandlers = new Map();
      const extensionAdapter = (name) => async (root, command) => {
        const key = `${name}:${root}`;
        if (!extensionHandlers.has(key)) {
          const mod = await import(pathToFileURL(`${root}/.${name}/extensions/fm-primary-turnend-guard.ts`).href);
          const handlers = {};
          mod.default({ on: (event, handler) => { handlers[event] = handler; }, sendMessage() {} });
          extensionHandlers.set(key, handlers.tool_call);
        }
        const result = await extensionHandlers.get(key)({ type: "tool_call", toolName: "bash", input: { command } });
        return result?.block ? "deny" : "allow";
      };
      const adapters = {
        opencode: async (root, command) => {
          const hooks = await FmPrimaryCdCheck({ worktree: root });
          try {
            await hooks["tool.execute.before"]({ tool: "bash" }, { args: { command } });
            return "allow";
          } catch (error) {
            return error.code ? `throws:${error.code}` : "deny";
          }
        },
        pi: extensionAdapter("pi"),
        omp: extensionAdapter("omp"),
      };
      const lines = [];
      for (const [adapter, run] of Object.entries(adapters)) {
        for (const [label, root] of roots) {
          for (const [caseName, command] of cases) {
            const received = join(root, "classifier-received-bytes");
            rmSync(received, { force: true });
            let verdict;
            try {
              verdict = await run(root, command);
            } catch (error) {
              verdict = `throws:${error.code ?? "error"}`;
            }
            let detail = "";
            if (label === "failing") {
              const seen = existsSync(received) ? readFileSync(received, "utf8").trim().split("\n").at(-1) : "none";
              detail = Number(seen) === Buffer.byteLength(command) ? " full" : ` received:${seen}`;
            }
            lines.push(`${adapter} ${label} ${caseName} ${verdict}${detail}`);
          }
        }
      }
      console.log(lines.join("\n"));
    ' 2>&1) || fail "cd-guard adapter driver failed to run: $out"
  expected=$(cat <<'EOF'
opencode primary protected_oversized deny
opencode primary benign_oversized allow
opencode primary protected_small deny
opencode failing protected_oversized allow full
opencode failing benign_oversized allow full
opencode failing protected_small allow full
pi primary protected_oversized deny
pi primary benign_oversized allow
pi primary protected_small deny
pi failing protected_oversized allow full
pi failing benign_oversized allow full
pi failing protected_small allow full
omp primary protected_oversized deny
omp primary benign_oversized allow
omp primary protected_small deny
omp failing protected_oversized allow full
omp failing benign_oversized allow full
omp failing protected_small allow full
EOF
)
  [ "$out" = "$expected" ] || fail "cd-guard adapters must deny an oversized protected command, allow benign ones, and fail open on classifier failure. got:
$out"
  pass "cd-guard adapters (OpenCode, Pi, omp): 1.2 MB protected denies, 1.2 MB benign allows, classifier failure fails open with the full command received"
}

# A synchronous throw from spawn (not an async error event) must fail open like a classifier crash. A module
# hook makes only the cd checker's spawn throw; the protected command would otherwise deny.
test_omp_spawn_throw_fails_open() {
  local primary verdict
  command -v node >/dev/null 2>&1 || { pass "node not installed, skipping omp spawn-throw check"; return; }
  node -e 'process.exit(typeof require("node:module").registerHooks === "function" ? 0 : 1)' 2>/dev/null \
    || { pass "node lacks module.registerHooks, skipping omp spawn-throw check"; return; }
  primary=$(make_adapter_fixture "$TMP_ROOT/omp-spawn-throw-primary")
  verdict=$(FM_PRIMARY="$primary" node --input-type=module -e '
    import { registerHooks } from "node:module";
    import { pathToFileURL } from "node:url";
    const stub = `const real = process.getBuiltinModule("node:child_process");
export const spawnSync = real.spawnSync;
export const spawn = (file, args, options) => {
  if (String(file).includes("fm-cd-pretool-check")) throw new TypeError("simulated synchronous spawn failure");
  return real.spawn(file, args, options);
};`;
    registerHooks({
      load(url, context, nextLoad) {
        if (url === "node:child_process") return { format: "module", source: stub, shortCircuit: true };
        return nextLoad(url, context);
      },
    });
    const mod = await import(pathToFileURL(`${process.env.FM_PRIMARY}/.omp/extensions/fm-primary-turnend-guard.ts`).href);
    const handlers = {};
    mod.default({ on: (event, handler) => { handlers[event] = handler; }, sendMessage() {} });
    let verdict;
    try {
      const result = await handlers.tool_call({ type: "tool_call", toolName: "bash", input: { command: "cd projects/foo" } });
      verdict = result?.block ? "deny" : "allow";
    } catch (error) {
      verdict = `throws:${error.message}`;
    }
    console.log(verdict);
  ' 2>&1) || fail "omp spawn-throw driver failed to run: $verdict"
  [ "$verdict" = allow ] || fail "omp must fail open when the cd checker's spawn throws synchronously, got: $verdict"
  pass "cd-guard omp adapter: a synchronous spawn throw in the cd checker fails open"
}

# --- per-harness wiring -----------------------------------------------------

# Delegated to bin/fm-lint.sh, the single owner of the lint definition including
# --external-sources; calling the linter directly here would be a second copy of
# that definition, and would disagree the moment this checker sourced a shared
# library.
test_scripts_are_shellcheck_clean() {
  local out
  command -v shellcheck >/dev/null 2>&1 || { pass "shellcheck not installed, skipping"; return; }
  out=$("$ROOT/bin/fm-lint.sh" "$ROOT/bin/fm-cd-pretool-check.sh" 2>&1) \
    || fail "bin/fm-cd-pretool-check.sh is not lint-clean under the pinned definition: $out"
  pass "bin/fm-cd-pretool-check.sh is clean under bin/fm-lint.sh"
}

test_full_acceptance_matrix
test_fires_in_secondmate_home
test_inert_in_child_worktree
test_inert_when_not_firstmate_repo
test_inert_when_not_a_git_repo
test_e2e_cwd_leak_regression
test_fail_open_empty_stdin
test_fail_open_closed_stdin
test_fail_open_unparseable_json
test_fail_open_missing_node
test_fail_open_missing_jq_on_stdin
test_prefilter_skips_node_without_cd_substring
test_policy_cli_direct
test_oversized_command_stdin_transport
test_adapters_send_cd_command_on_stdin
test_omp_spawn_throw_fails_open
test_scripts_are_shellcheck_clean
