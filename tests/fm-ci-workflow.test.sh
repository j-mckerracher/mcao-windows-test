#!/usr/bin/env bash
# Contract tests for .github/workflows/ci.yml's runner-spend safeguards.
#
# Origin: the 2026-09-12 GitHub Actions starvation incident. firstmate CI had no
# concurrency deduplication, so every superseded PR head kept its full job
# fan-out, and four jobs carried no timeout at all. These tests hold both
# safeguards: PR runs supersede within one PR while main pushes are never
# cancelled, and every CI job carries a finite hang tripwire drawn from the
# three-tier timeout policy that docs/fm-test-portable-shards.md "Timeouts"
# owns (fast, normal, heavy), so no job drifts back to a one-off number.
#
# The workflow is parsed as YAML and its concurrency expressions are resolved
# against simulated pull_request and push contexts, so the assertions describe
# what GitHub would do, not how the file happens to be spelled.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CI_WORKFLOW="$ROOT/.github/workflows/ci.yml"

assert_present "$CI_WORKFLOW" ".github/workflows/ci.yml is missing"
command -v ruby >/dev/null 2>&1 \
  || fail "ruby is required to parse .github/workflows/ci.yml as YAML"

# Resolve the workflow's concurrency contract under one simulated event and
# print "<group><TAB><cancel-in-progress>". Only the two expression constructs
# this workflow uses are resolved: an `a || b` fallback and an `==` comparison.
resolve_concurrency() {
  local event=$1 pr_number=$2 run_id=$3
  ruby -ryaml -e '
doc = YAML.load_file(ARGV[0])
concurrency = doc.fetch("concurrency")
context = {
  "github.workflow" => doc.fetch("name"),
  "github.event_name" => ARGV[1],
  "github.event.pull_request.number" => ARGV[2],
  "github.run_id" => ARGV[3],
}

value = lambda do |token|
  token = token.strip
  next token[1..-2] if token.start_with?("\x27") && token.end_with?("\x27")
  raise "unresolvable context reference: #{token}" unless context.key?(token)
  context.fetch(token)
end

evaluate = lambda do |expression|
  expression = expression.strip
  if expression.include?("==")
    left, right = expression.split("==", 2)
    next value.call(left) == value.call(right) ? "true" : "false"
  end
  resolved = expression.split("||").map { |token| value.call(token) }.find { |v| !v.empty? }
  resolved.to_s
end

interpolate = lambda do |raw|
  raw.to_s.gsub(/\$\{\{(.+?)\}\}/) { evaluate.call(Regexp.last_match(1)) }
end

puts [interpolate.call(concurrency.fetch("group")),
      interpolate.call(concurrency.fetch("cancel-in-progress"))].join("\t")
' "$CI_WORKFLOW" "$event" "$pr_number" "$run_id"
}

job_timeout() {
  ruby -ryaml -e '
puts YAML.load_file(ARGV[0]).fetch("jobs").fetch(ARGV[1]).fetch("timeout-minutes", "none")
' "$CI_WORKFLOW" "$1"
}

# Tier membership is the executable inventory of the timeout policy: a new job
# must join a tier, and a job-level value outside these tiers is exactly the
# one-off number the policy removed.
FAST_TIER_JOBS='test-coverage invariants tests-timing-aggregate'
NORMAL_TIER_JOBS='lint tests-portable-parallel-1 tests-portable-parallel-2 tests-portable-serial macos-stock-bash windows-test windows-console-test'
HEAVY_TIER_JOBS='tests-herdr'

# Print the one timeout every listed job shares; fail on any disagreement.
tier_timeout() {  # <tier> <job>...
  local tier=$1 job first actual
  shift
  first=
  for job in "$@"; do
    actual=$(job_timeout "$job") || fail "could not read the $job timeout"
    case "$actual" in ''|*[!0-9]*) fail "$job ($tier tier) has no integer timeout, got $actual" ;; esac
    if [ -z "$first" ]; then
      first=$actual
    elif [ "$actual" != "$first" ]; then
      fail "$tier tier jobs must share one timeout, got $first and $actual ($job)"
    fi
  done
  printf '%s\n' "$first"
}

# Print every job id in the workflow, one per line.
workflow_jobs() {
  ruby -ryaml -e 'puts YAML.load_file(ARGV[0]).fetch("jobs").keys' "$CI_WORKFLOW"
}

group_of() { printf '%s\n' "$1" | cut -f1; }
cancel_of() { printf '%s\n' "$1" | cut -f2; }

test_pr_pushes_supersede_within_one_pr() {
  local first second
  first=$(resolve_concurrency pull_request 108 900001) || fail "could not resolve PR concurrency"
  second=$(resolve_concurrency pull_request 108 900002) || fail "could not resolve PR concurrency"
  [ "$(group_of "$first")" = "$(group_of "$second")" ] \
    || fail "two runs of one PR must share a concurrency group, got $(group_of "$first") and $(group_of "$second")"
  [ "$(cancel_of "$first")" = true ] \
    || fail "PR runs must cancel the in-progress run, got $(cancel_of "$first")"
  pass "a newer push to one PR supersedes that PR's in-flight CI"
}

test_separate_prs_do_not_cancel_each_other() {
  local one two
  one=$(resolve_concurrency pull_request 108 900001) || fail "could not resolve PR concurrency"
  two=$(resolve_concurrency pull_request 109 900003) || fail "could not resolve PR concurrency"
  [ "$(group_of "$one")" != "$(group_of "$two")" ] \
    || fail "distinct PRs must not share a concurrency group ($(group_of "$one"))"
  pass "distinct PRs get distinct concurrency groups"
}

test_main_pushes_are_never_cancelled() {
  local first second
  first=$(resolve_concurrency push '' 900010) || fail "could not resolve push concurrency"
  second=$(resolve_concurrency push '' 900011) || fail "could not resolve push concurrency"
  [ "$(group_of "$first")" != "$(group_of "$second")" ] \
    || fail "each main push must get its own concurrency group, got $(group_of "$first") twice"
  [ "$(cancel_of "$first")" = false ] \
    || fail "push runs must never cancel an in-progress run, got $(cancel_of "$first")"
  pass "every main push keeps its own group and is never cancelled"
}

test_every_job_has_a_finite_timeout() {
  local reported
  reported=$(ruby -ryaml -e '
YAML.load_file(ARGV[0]).fetch("jobs").each do |name, job|
  timeout = job["timeout-minutes"]
  next if timeout.is_a?(Integer) && timeout > 0
  puts "#{name}: #{timeout.inspect}"
end
' "$CI_WORKFLOW") || fail "could not read job timeouts from ci.yml"
  [ -z "$reported" ] || fail "these CI jobs have no finite hang tripwire:"$'\n'"$reported"
  pass "every ci.yml job carries a finite timeout"
}

# DPES-1011: a console dispatch must select only the isolated native console job.
test_manual_console_dispatch_isolated_from_workspace_matrix() {
  ruby -ryaml - "$CI_WORKFLOW" <<'RUBY' || fail "manual console dispatch must be isolated"
doc = YAML.load_file(ARGV[0])
triggers = doc.fetch("on") { doc.fetch(true) }
input = triggers.fetch("workflow_dispatch").fetch("inputs").fetch("windows_test_mode")
raise "manual test mode must be an explicit choice" unless input.fetch("type") == "choice"
raise "manual test mode must default to the existing workspace lane" unless input.fetch("default") == "workspace"
raise "manual test mode must offer only workspace and console" unless input.fetch("options") == %w[workspace console]
raise "manual test mode must be required" unless input.fetch("required") == true

jobs = doc.fetch("jobs")
workspace = jobs.fetch("windows-test")
console = jobs.fetch("windows-console-test")
raise "workspace matrix must skip console-only dispatches" unless
  workspace.fetch("if") == "github.event_name != 'workflow_dispatch' || inputs.windows_test_mode == 'workspace'"
raise "existing Windows targets must remain available to the workspace lane" unless
  workspace.fetch("strategy").fetch("matrix").fetch("target") ==
  %w[x86_64-pc-windows-msvc aarch64-pc-windows-msvc]
raise "console job must run only for console dispatches" unless
  console.fetch("if") == "github.event_name == 'workflow_dispatch' && inputs.windows_test_mode == 'console'"
raise "console job must not have a matrix" if console.key?("strategy")
raise "console job must not continue on error" if console.key?("continue-on-error")
raise "console job must use native Windows x86_64" unless console.fetch("runs-on") == "windows-2022"
raise "console job must not broaden permissions" unless console.fetch("permissions") == {"contents" => "read"}

unexpected = jobs.each_with_object([]) do |(name, job), failures|
  next if %w[windows-test windows-console-test].include?(name)
  condition = job.fetch("if", "").to_s
  expected_condition = if name == "tests-timing-aggregate"
    "always() && github.event_name != 'workflow_dispatch'"
  else
    "github.event_name != 'workflow_dispatch'"
  end
  failures << "#{name}: #{condition}" unless condition == expected_condition
end
raise "non-Windows jobs must use the exact manual-dispatch condition: #{unexpected.join(", ")}" unless unexpected.empty?

steps = console.fetch("steps")
raise "console job must default PATHEXT when the runner leaves it unset" unless
  steps.any? { |step|
    step["name"] == "Default PATHEXT when the runner leaves it unset" &&
    step["shell"] == "pwsh" &&
    step.fetch("run").include?("if (-not $env:PATHEXT)") &&
    step.fetch("run").include?("PATHEXT=.COM;.EXE;.BAT;.CMD")
  }
steps.each do |step|
  raise "console steps must not add conditions" if step.key?("if")
  raise "console steps must not continue on error" if step.key?("continue-on-error")
end
checkout = steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
raise "console job must check out the immutable event SHA without persisted credentials" unless
  checkout && checkout.fetch("with").fetch("ref") == "${{ github.sha }}" &&
  checkout.fetch("with").fetch("persist-credentials") == false
metadata = steps.find { |step| step["name"] == "Report checked-out SHA and platform" }
raise "console job must report its checked-out SHA and Windows platform" unless
  metadata && metadata.fetch("run").include?("git rev-parse HEAD") &&
  metadata.fetch("run").include?("Checked-out SHA:") &&
  metadata.fetch("run").include?("Platform:") &&
  metadata.fetch("run").include?("$ErrorActionPreference = 'Stop'") &&
  metadata.fetch("run").include?(
    "if ($head -ne $env:GITHUB_SHA) {\n  throw \"Checked-out HEAD does not match GITHUB_SHA: $head != $env:GITHUB_SHA\"\n}"
  ) &&
  metadata.fetch("run").include?("$env:RUNNER_OS -ne 'Windows'") &&
  metadata.fetch("run").include?("$env:RUNNER_ARCH -ne 'X64'")

test_step = steps.find { |step| step["name"] == "Run only the console integration tests" }
raise "console job must have one native test step" unless test_step && test_step["shell"] == "pwsh"
run_lines = test_step.fetch("run").lines.map(&:strip)
test_calls = run_lines.grep(/\AInvoke-NativeCargoTest\b/)
expected_calls = [
  "Invoke-NativeCargoTest 'console_pipe' @('test', '-p', 'mcao-backend-windows', '--locked', '--test', 'console_pipe')",
  "Invoke-NativeCargoTest 'pipe' @('test', '-p', 'mcao-backend-windows', '--locked', '--test', 'pipe')",
  "Invoke-NativeCargoTest 'console_server' @('test', '-p', 'mcao-backend-windows', '--locked', '--lib', 'console_server')",
]
raise "console job must run exactly the three scoped cargo tests: #{test_calls.inspect}" unless test_calls == expected_calls

native_test_helper = run_lines.take_while { |line| !line.start_with?("Invoke-NativeCargoTest ") }.join("\n")
raise "native cargo test helper must propagate cargo's exit code" unless
  native_test_helper.include?("$output = & cargo @Arguments") &&
  native_test_helper.include?("$exitCode = $LASTEXITCODE") &&
  native_test_helper.include?("if ($exitCode -ne 0) { exit $exitCode }")
raise "native cargo test helper must reject a zero-test match" unless
  native_test_helper.include?("$output -match '^running [1-9][0-9]* tests?$'") &&
  native_test_helper.include?('throw "$Name matched no native tests"')
RUBY
  pass "manual console selection runs three nonempty native test commands and propagates each exit code"
}

# Every job sits in exactly one tier, and the workflow carries exactly three
# distinct job-level timeouts: one per tier, no one-off numbers.
test_every_job_belongs_to_exactly_one_timeout_tier() {
  local expected actual distinct
  # shellcheck disable=SC2086
  expected=$(printf '%s\n' $FAST_TIER_JOBS $NORMAL_TIER_JOBS $HEAVY_TIER_JOBS | LC_ALL=C sort)
  [ "$(printf '%s\n' "$expected" | LC_ALL=C sort -u)" = "$expected" ] \
    || fail "a job is listed in more than one timeout tier:"$'\n'"$expected"
  actual=$(workflow_jobs | LC_ALL=C sort) || fail "could not list ci.yml jobs"
  [ "$actual" = "$expected" ] \
    || fail "ci.yml jobs and the timeout tiers disagree; every job must join one tier"$'\n'"workflow: $(printf '%s' "$actual" | tr '\n' ' ')"$'\n'"tiers: $(printf '%s' "$expected" | tr '\n' ' ')"
  distinct=$(for job in $expected; do job_timeout "$job"; done | LC_ALL=C sort -u | wc -l | tr -d ' ')
  [ "$distinct" = 3 ] \
    || fail "ci.yml must carry exactly three distinct job timeouts (fast, normal, heavy), got $distinct"
  pass "every ci.yml job belongs to one of the three timeout tiers"
}

# Fast tier: seconds-long checks share one short tripwire in the 5-10 minute band.
test_fast_tier_shares_one_short_tripwire() {
  local fast
  # shellcheck disable=SC2086
  fast=$(tier_timeout fast $FAST_TIER_JOBS) || exit 1
  [ "$fast" -ge 5 ] && [ "$fast" -le 10 ] \
    || fail "fast tier must be a 5-10 minute hang tripwire, got $fast"
  pass "fast tier jobs share one $fast minute tripwire"
}

# Normal tier: every test or lint lane shares ONE fixed 30-minute budget,
# above the fast tier. That budget is a hang tripwire, not a packing estimate.
test_normal_tier_shares_one_budget() {
  local fast normal
  # shellcheck disable=SC2086
  fast=$(tier_timeout fast $FAST_TIER_JOBS) || exit 1
  # shellcheck disable=SC2086
  normal=$(tier_timeout normal $NORMAL_TIER_JOBS) || exit 1
  [ "$normal" -gt "$fast" ] \
    || fail "normal tier ($normal) must exceed the fast tier ($fast)"
  [ "$normal" = 30 ] \
    || fail "normal tier must be the single 30-minute shared budget, got $normal"
  pass "normal tier jobs share one $normal minute budget"
}

# Heavy tier: Herdr alone carries a job-level last-resort backstop above the
# normal tier, while its family-run step owns a tighter tripwire so the
# always() cleanup and timing upload still run after a hang.
test_heavy_tier_keeps_a_step_tripwire_under_a_job_backstop() {
  local normal heavy step
  # shellcheck disable=SC2086
  normal=$(tier_timeout normal $NORMAL_TIER_JOBS) || exit 1
  # shellcheck disable=SC2086
  heavy=$(tier_timeout heavy $HEAVY_TIER_JOBS) || exit 1
  [ "$heavy" -gt "$normal" ] \
    || fail "heavy tier backstop ($heavy) must exceed the normal tier ($normal)"
  [ "$heavy" -ge 60 ] && [ "$heavy" -le 75 ] \
    || fail "heavy tier backstop must stay a 60-75 minute last resort, got $heavy"
  step=$(ruby -ryaml -e '
steps = YAML.load_file(ARGV[0]).fetch("jobs").fetch(ARGV[1]).fetch("steps")
index = steps.index { |s| s["id"] == "run-real-herdr-family" }
raise "no run-real-herdr-family step" unless index
teardown = steps.index { |s| s["id"] == "cleanup-herdr-lab-sessions" }
raise "no cleanup-herdr-lab-sessions step" unless teardown
raise "teardown must follow the family-run step" unless teardown > index
raise "teardown must run under always()" unless steps[teardown]["if"].to_s.strip == "always()"
puts steps[index].fetch("timeout-minutes", "none")
' "$CI_WORKFLOW" tests-herdr) || fail "could not read the Herdr family-run step"
  case "$step" in ''|*[!0-9]*) fail "the Herdr family-run step needs its own timeout-minutes, got $step" ;; esac
  [ "$step" = 20 ] \
    || fail "the Herdr family-run step must be the 20-minute tripwire, got $step"
  [ "$step" -lt "$heavy" ] \
    || fail "the Herdr step tripwire ($step) must stay below the job backstop ($heavy)"
  pass "Herdr keeps a $step minute step tripwire under a $heavy minute job backstop"
}

test_ci_matrices_match_executable_partitions() {
  ruby -ryaml -ropen3 - "$CI_WORKFLOW" "$ROOT" <<'RUBY' || fail "CI partition contract"
jobs = YAML.load_file(ARGV[0]).fetch("jobs")
root = ARGV[1]
serial = jobs.fetch("tests-portable-serial").fetch("strategy")
raise "serial failures must not cancel other shards" unless serial.fetch("fail-fast") == false
matrix = serial.fetch("matrix")
raise "unexpected serial dimensions" unless matrix.keys == ["shard"]
shards = matrix.fetch("shard")
lanes, status = Open3.capture2(File.join(root, "bin/fm-test-run.sh"), "--list-lanes")
raise "cannot list runner lanes" unless status.success?
actual = lanes.lines.map(&:strip).select { |l| l.match?(/\Aportable-serial-\d+of\d+\z/) }
expected = shards.map { |s| "portable-serial-#{s}of#{shards.length}" }
raise "CI matrix and runner disagree" unless actual.sort == expected.sort
lint = jobs.fetch("lint").fetch("strategy")
raise "lint failures must not cancel another partition" unless lint.fetch("fail-fast") == false
matrix = lint.fetch("matrix")
raise "unexpected lint dimensions" unless matrix.keys == ["partition"]
parts = matrix.fetch("partition")
roots = parts.flat_map do |p|
  output, result = Open3.capture2(File.join(root, "bin/fm-lint.sh"), "--partition", "#{p}of#{parts.length}", "--list-files")
  raise "unsupported lint partition" unless result.success?
  output.lines.map(&:strip)
end
canonical, result = Open3.capture2({"CI" => "true"}, File.join(root, "bin/fm-lint.sh"), "--list-files")
raise "lint matrix loses or duplicates canonical roots" unless result.success? && roots.sort == canonical.lines.map(&:strip).sort
RUBY
  pass "CI matrices cover every executable serial lane and canonical lint root exactly once"
}

test_live_setup_guard_cli_install_follows_shard_membership() {
  ruby -ryaml - "$CI_WORKFLOW" <<'RUBY' || fail "live setup guard CLI install must follow the shard that runs the guard"
steps = YAML.load_file(ARGV[0]).fetch("jobs").fetch("tests-portable-serial").fetch("steps")
run_index = steps.index { |s| s["name"].to_s.start_with?("Run portable serial shard") }
raise "no portable serial run step" unless run_index
resolver_index = steps.index { |s| s["id"] == "live-setup-guard" }
raise "no live-setup-guard resolver step" unless resolver_index
resolver = steps[resolver_index]
raise "resolver must list its own shard's lane" unless resolver.fetch("run").include?("bin/fm-test-run.sh --list --lane")
raise "resolver must look for the live setup guard" unless resolver.fetch("run").include?("tests/mcao-setup-hve-live-e2e.test.sh")
raise "resolver must read the same lane the shard runs" unless resolver.fetch("env").fetch("FM_SERIAL_LANE") == steps[run_index].fetch("env").fetch("FM_SERIAL_LANE")
install_index = steps.index { |s| s["name"].to_s.start_with?("Install the pinned Claude Code CLI") }
raise "no Claude Code CLI install step" unless install_index
raise "Claude CLI install must run after the resolver" unless install_index > resolver_index
raise "Claude CLI install must key on the resolver output, not a shard number" unless steps[install_index]["if"].to_s == "steps.live-setup-guard.outputs.needs_claude_cli == 'true'"
RUBY
  pass "the Claude CLI installs on exactly the shard the runner assigns the live setup guard"
}

test_ci_matrices_match_executable_partitions
test_live_setup_guard_cli_install_follows_shard_membership
test_pr_pushes_supersede_within_one_pr
test_separate_prs_do_not_cancel_each_other
test_main_pushes_are_never_cancelled
test_every_job_has_a_finite_timeout
test_manual_console_dispatch_isolated_from_workspace_matrix
test_every_job_belongs_to_exactly_one_timeout_tier
test_fast_tier_shares_one_short_tripwire
test_normal_tier_shares_one_budget
test_heavy_tier_keeps_a_step_tripwire_under_a_job_backstop
