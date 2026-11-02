#!/usr/bin/env bash
# Contract tests for the failure artifacts of .github/workflows/rust-workspace-check.yml
# (DPES-759 AC3). Failure logs upload only as redacted copies plus _MANIFEST.txt, and the
# pwsh helper .github/scripts/rust-ci-redact-artifacts.ps1 removes FM_* values, credential-pattern
# lines and home credential paths without recording any value in its manifest or output.
#
# The workflow is parsed as YAML so the assertions describe what GitHub would run. The helper runs
# under a real pwsh; pwsh is required, so a missing pwsh fails this suite rather than skipping it.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

WORKFLOW="$ROOT/.github/workflows/rust-workspace-check.yml"
REDACT="$ROOT/.github/scripts/rust-ci-redact-artifacts.ps1"

assert_present "$WORKFLOW" ".github/workflows/rust-workspace-check.yml is missing"
assert_present "$REDACT" ".github/scripts/rust-ci-redact-artifacts.ps1 is missing"
command -v ruby >/dev/null 2>&1 || fail "ruby is required to parse the workflow as YAML"
command -v pwsh >/dev/null 2>&1 || fail "pwsh is required to run $REDACT"

# Every job that can fail must redact its failure logs in a pwsh step, then upload only that redacted
# directory, gated on the redaction step succeeding and never failing open on missing files.
# The Ruby program prints one fact per line, prefixed with its job, so each job is asserted on its own.
# shellcheck disable=SC2016  # The Ruby program is single-quoted so the literal ${{ }} expressions stay unexpanded.
structure=$(ruby -ryaml -e '
doc = YAML.load_file(ARGV[0])
jobs = doc.fetch("jobs")
logs_out = "${{ runner.temp }}/rust-ci-artifacts"
probe_out = "${{ runner.temp }}/probe-evidence-redacted"
redacting = ->(s) { s["run"].to_s.include?("rust-ci-redact-artifacts.ps1") }
uploading = ->(s) { s["uses"].to_s.start_with?("actions/upload-artifact@") }
label = ->(job, s) { "#{job} / #{s["name"] || s["uses"] || "run"}" }
# A step can reach a Windows leg unless its if excludes Windows; a matrix job or a windows-latest job can.
windows_possible = lambda do |job, s|
  next false if s["if"].to_s.include?("!= \x27Windows\x27")
  runs_on = job["runs-on"].to_s
  runs_on.include?("windows") || runs_on.include?("matrix.os") || s["if"].to_s.include?("== \x27Windows\x27")
end
%w[fmt check probe-windows].each do |name|
  steps = jobs.fetch(name).fetch("steps")
  redact = steps.each_index.select { |i| redacting.(steps[i]) && steps[i]["run"].to_s.include?("-OutDir \"#{logs_out}\"") }
  upload = steps.each_index.select { |i| uploading.(steps[i]) && steps[i]["with"]["path"] == logs_out }
  puts "#{name} logs-pair=#{redact.size}/#{upload.size}"
  if redact.size == 1 && upload.size == 1
    r = steps[redact[0]]
    u = steps[upload[0]]
    puts "#{name} logs-redact-if=#{r["if"]}"
    puts "#{name} logs-redact-shell=#{r["shell"]}"
    puts "#{name} logs-upload-after-redact=#{upload[0] > redact[0]}"
    puts "#{name} logs-upload-gated=#{u["if"].to_s == "${{ failure() && steps.#{r["id"]}.outcome == \x27success\x27 }}"}"
    puts "#{name} logs-upload-retention-days=#{u["with"]["retention-days"]}"
    puts "#{name} logs-upload-if-no-files-found=#{u["with"]["if-no-files-found"]}"
  end
  uploads = steps.select { |s| uploading.(s) }
  puts "#{name} raw-uploads=#{uploads.count { |s| ![logs_out, probe_out].include?(s["with"]["path"]) }}"
  puts "#{name} uploads-without-run-attempt=#{uploads.count { |s| !s["with"]["name"].to_s.include?("github.run_attempt") }}"
end
# Probe evidence is uploaded on every run that is not cancelled, so it goes through the same helper into its own redacted directory.
probe_steps = jobs.fetch("probe-windows").fetch("steps")
probe_redact = probe_steps.each_index.select { |i| redacting.(probe_steps[i]) && probe_steps[i]["run"].to_s.include?("-OutDir \"#{probe_out}\"") }
probe_upload = probe_steps.each_index.select { |i| uploading.(probe_steps[i]) && probe_steps[i]["with"]["path"] == probe_out }
puts "probe-windows probe-pair=#{probe_redact.size}/#{probe_upload.size}"
if probe_redact.size == 1 && probe_upload.size == 1
  r = probe_steps[probe_redact[0]]
  u = probe_steps[probe_upload[0]]
  puts "probe-windows probe-redact-not-cancelled=#{r["if"].to_s == "${{ !cancelled() }}"}"
  puts "probe-windows probe-redact-json=#{r["run"].to_s.include?("-Include \x27*.json\x27")}"
  puts "probe-windows probe-upload-after-redact=#{probe_upload[0] > probe_redact[0]}"
  puts "probe-windows probe-upload-gated=#{u["if"].to_s == "${{ !cancelled() && steps.#{r["id"]}.outcome == \x27success\x27 }}"}"
  puts "probe-windows probe-upload-retention-days=#{u["with"]["retention-days"]}"
  puts "probe-windows probe-upload-if-no-files-found=#{u["with"]["if-no-files-found"]}"
end
# A tee pipeline with no pipefail exits 0 when its command fails. A step that can run on a Windows leg
# must not pipe into tee at all, because bash is removed from PATH there; use Tee-Object under pwsh.
tee = /\|\s*tee\b/
jobs.each do |job_name, job|
  job.fetch("steps").each do |s|
    next unless s["run"].to_s.match?(tee)
    if windows_possible.(job, s)
      puts "windows-tee: #{label.(job_name, s)}"
    elsif s["shell"] != "bash" && !s["run"].to_s.include?("pipefail")
      puts "pipefail-missing: #{label.(job_name, s)}"
    end
  end
end
' "$WORKFLOW") || fail "could not parse $WORKFLOW as YAML"

# Each fact is one whole output line, so a mutated value cannot match inside another fact.
expect_fact() {
  printf '%s\n' "$structure" | grep -qxF -- "$1" || fail "$2"
}
for job in fmt check probe-windows; do
  expect_fact "$job logs-pair=1/1" "job $job must redact its failure logs once and upload that output once"
  expect_fact "$job logs-redact-if=failure()" "job $job failure-log redaction must run only on failure()"
  expect_fact "$job logs-redact-shell=pwsh" "job $job failure-log redaction must run under pwsh so Windows legs need no bash"
  expect_fact "$job logs-upload-after-redact=true" "job $job redacted upload must run after its redaction step"
  expect_fact "$job logs-upload-gated=true" "job $job redacted upload must run only when its redaction step succeeded"
  expect_fact "$job logs-upload-retention-days=7" "job $job redacted upload must keep 7 days"
  expect_fact "$job logs-upload-if-no-files-found=error" "job $job redacted upload must fail when the manifest is missing"
  expect_fact "$job raw-uploads=0" "job $job must not upload anything but its redacted directory"
  expect_fact "$job uploads-without-run-attempt=0" "job $job artifact names must include github.run_attempt so a re-run does not collide"
done
expect_fact "probe-windows probe-pair=1/1" "probe evidence must be redacted once and uploaded once from the redacted directory"
expect_fact "probe-windows probe-redact-not-cancelled=true" "probe evidence redaction must run on every run that is not cancelled"
expect_fact "probe-windows probe-redact-json=true" "probe evidence redaction must include the JSON evidence files"
expect_fact "probe-windows probe-upload-after-redact=true" "probe evidence upload must run after its redaction step"
expect_fact "probe-windows probe-upload-gated=true" "probe evidence upload must run on every run that is not cancelled and only when its redaction step succeeded"
expect_fact "probe-windows probe-upload-retention-days=7" "probe evidence upload must keep 7 days"
expect_fact "probe-windows probe-upload-if-no-files-found=error" "probe evidence upload must fail when the manifest is missing"
pipefail_missing=$(printf '%s\n' "$structure" | sed -n 's/^pipefail-missing: //p' | tr '\n' ';')
[ -z "$pipefail_missing" ] \
  || fail "tee pipeline runs without pipefail, so a failing command exits 0 through tee (${pipefail_missing%;}): set shell: bash, which runs with -o pipefail, or add set -o pipefail to its run"
windows_tee=$(printf '%s\n' "$structure" | sed -n 's/^windows-tee: //p' | tr '\n' ';')
[ -z "$windows_tee" ] \
  || fail "tee pipeline can run on a Windows leg, where bash is removed from PATH (${windows_tee%;}): use Tee-Object under pwsh"
pass "workflow redacts failure logs and probe evidence before every upload, from fmt, check and probe-windows"

# Fixture: a home with credential directories, and a process environment that carries FM_* and
# secret-named values. Every value below must be absent from the redacted output and the manifest.
TMP_ROOT=$(fm_test_tmproot rust-ci-redact)
FAKE_HOME="$TMP_ROOT/home"
IN_DIR="$TMP_ROOT/in"
OUT_DIR="$TMP_ROOT/out"
mkdir -p "$FAKE_HOME" "$IN_DIR"
FM_VALUE='fixture-fm-value-7731'
TOKEN_VALUE='fixture-token-value-4419'
GH_TOKEN_LINE='ghp_abcdefghijklmnopqrstuvwxyz0123456789'
export FM_TEST_REDACT="$FM_VALUE"
export FAKE_API_TOKEN="$TOKEN_VALUE"
# One fixture per rule: a secret-named variable for each env-name suffix, and a value that spans two lines.
FAKE_DB_PASSWORD='fixture-db-password-3307'
FAKE_DB_PWD='fixture-db-pwd-3308'
FAKE_PASSWD='fixture-passwd-3309'
FAKE_PASSPHRASE='fixture-passphrase-3310'
FAKE_CLIENT_SECRET='fixture-client-secret-3311'
FAKE_SIGNING_KEY='fixture-signing-key-3312'
FAKE_CREDENTIAL='fixture-credential-3313'
FM_MULTILINE=$'fixture-multi-a-5521\nfixture-multi-b-5522'
export FAKE_DB_PASSWORD FAKE_DB_PWD FAKE_PASSWD FAKE_PASSPHRASE FAKE_CLIENT_SECRET FAKE_SIGNING_KEY FAKE_CREDENTIAL FM_MULTILINE

cat > "$IN_DIR/rust-ci-clippy.log" <<LOG
   Compiling mcao-core v0.1.0
task-20260101-7 started, risk-based check passes
auth header Authorization: Bearer abc.def.ghi
push used $GH_TOKEN_LINE to origin
key sk-proj-XYZ123abc was rejected
oauth gho_ZZZ9876543210 leaked into output
config value $FM_VALUE printed
env value $TOKEN_VALUE printed
read $FAKE_HOME/.ssh/id_ed25519 failed
read $FAKE_HOME/.gitconfig ok
LOG
# Each credential shape has its own line, so a mutant that disables one rule fails on that line alone.
cat > "$IN_DIR/rust-ci-check.log" <<LOG
Authorization: Basic Zml4dHVyZS11c2VyOmZpeHR1cmUtcGFzcy05OTAx
"Authorization": "Basic FAKEb64userpass1111",
clone https://fixture-user:fixture-pass-8812@example.invalid/repo.git
token ghs_FIXTUREsrv0123456789abcdefgh
token github_pat_FIXTUREpat0123456789abcdef
token ghu_FIXTUREusr0123456789abcdefgh
token ghr_FIXTUREref0123456789abcdefgh
db password $FAKE_DB_PASSWORD
db pwd $FAKE_DB_PWD
passwd $FAKE_PASSWD
passphrase $FAKE_PASSPHRASE
client secret $FAKE_CLIENT_SECRET
signing key $FAKE_SIGNING_KEY
credential $FAKE_CREDENTIAL
multi-line FM value $FM_MULTILINE
read $FAKE_HOME/.aws/credentials failed
read $FAKE_HOME/.git/config failed
read $FAKE_HOME/.cargo/credentials.toml failed
read $FAKE_HOME/.netrc failed
read $FAKE_HOME/_netrc failed
read $FAKE_HOME/.cargo/config.toml ok
LOG
printf 'plain line that must survive\n' > "$IN_DIR/rust-ci-test.log"
# Windows legs write CRLF, so the multi-line value must also match across a CRLF line ending.
printf 'crlf FM value fixture-multi-a-5521\r\nfixture-multi-b-5522\r\n' >> "$IN_DIR/rust-ci-test.log"
printf 'not a ci log, must not be read\n' > "$IN_DIR/other.log"

# env -i keeps the suite's own FM_* exports out of the helper's environment, so the only
# sensitive values it sees are the fixture values above.
run_redact() {
  env -i HOME="$FAKE_HOME" PATH="$PATH" FM_TEST_REDACT="$FM_VALUE" FAKE_API_TOKEN="$TOKEN_VALUE" \
    FAKE_DB_PASSWORD="$FAKE_DB_PASSWORD" FAKE_DB_PWD="$FAKE_DB_PWD" FAKE_PASSWD="$FAKE_PASSWD" \
    FAKE_PASSPHRASE="$FAKE_PASSPHRASE" FAKE_CLIENT_SECRET="$FAKE_CLIENT_SECRET" \
    FAKE_SIGNING_KEY="$FAKE_SIGNING_KEY" FAKE_CREDENTIAL="$FAKE_CREDENTIAL" FM_MULTILINE="$FM_MULTILINE" \
    pwsh -NoProfile -NonInteractive -File "$REDACT" "$@"
}

run_out="$TMP_ROOT/run.out"
run_redact -InputDir "$IN_DIR" -OutDir "$OUT_DIR" >"$run_out" 2>&1 \
  || fail "redaction helper failed: $(cat "$run_out")"

[ -f "$OUT_DIR/rust-ci-clippy.log" ] || fail "redacted clippy log was not written"
[ -f "$OUT_DIR/rust-ci-test.log" ] || fail "redacted test log was not written"
[ -f "$OUT_DIR/_MANIFEST.txt" ] || fail "_MANIFEST.txt was not written"
[ ! -e "$OUT_DIR/other.log" ] || fail "a file not named rust-ci-*.log was copied"

clippy="$OUT_DIR/rust-ci-clippy.log"
assert_absent_text() {
  local needle=$1 file=$2
  if grep -qF -- "$needle" "$file"; then
    fail "redacted output still contains $3"
  fi
}
assert_absent_text "$GH_TOKEN_LINE" "$clippy" "a ghp_ token"
assert_absent_text "Bearer abc.def.ghi" "$clippy" "a Bearer credential"
assert_absent_text "sk-proj-XYZ123abc" "$clippy" "an sk- credential"
assert_absent_text "gho_ZZZ9876543210" "$clippy" "a gho_ credential"
assert_absent_text "$FM_VALUE" "$clippy" "an FM_* value"
assert_absent_text "$TOKEN_VALUE" "$clippy" "a *TOKEN* value"
assert_absent_text "$FAKE_HOME/.ssh" "$clippy" "a ~/.ssh path"

check="$OUT_DIR/rust-ci-check.log"
assert_absent_text "Zml4dHVyZS11c2VyOmZpeHR1cmUtcGFzcy05OTAx" "$check" "a Basic Authorization credential"
assert_absent_text "FAKEb64userpass1111" "$check" "a JSON-quoted Basic Authorization credential"
assert_absent_text "fixture-pass-8812" "$check" "a URL userinfo password"
assert_absent_text "ghs_FIXTUREsrv0123456789abcdefgh" "$check" "a ghs_ token"
assert_absent_text "github_pat_FIXTUREpat0123456789abcdef" "$check" "a github_pat_ token"
assert_absent_text "ghu_FIXTUREusr0123456789abcdefgh" "$check" "a ghu_ token"
assert_absent_text "ghr_FIXTUREref0123456789abcdefgh" "$check" "a ghr_ token"
assert_absent_text "$FAKE_DB_PASSWORD" "$check" "a *PASSWORD* env value"
assert_absent_text "$FAKE_DB_PWD" "$check" "a *_PWD env value"
assert_absent_text "$FAKE_PASSWD" "$check" "a *PASSWD* env value"
assert_absent_text "$FAKE_PASSPHRASE" "$check" "a *PASSPHRASE* env value"
assert_absent_text "$FAKE_CLIENT_SECRET" "$check" "a *SECRET* env value"
assert_absent_text "$FAKE_SIGNING_KEY" "$check" "a *KEY* env value"
assert_absent_text "$FAKE_CREDENTIAL" "$check" "a *CREDENTIAL* env value"
assert_absent_text "fixture-multi-a-5521" "$check" "the first line of a multi-line FM_* value"
assert_absent_text "fixture-multi-b-5522" "$check" "the second line of a multi-line FM_* value"
assert_absent_text "$FAKE_HOME/.aws" "$check" "a ~/.aws path"
assert_absent_text "$FAKE_HOME/.git/config" "$check" "a ~/.git path"
assert_absent_text "$FAKE_HOME/.cargo/credentials" "$check" "a ~/.cargo/credentials path"
assert_absent_text "$FAKE_HOME/.netrc" "$check" "a ~/.netrc path"
assert_absent_text "$FAKE_HOME/_netrc" "$check" "a ~/_netrc path"
grep -qF "$FAKE_HOME/.cargo/config.toml ok" "$check" \
  || fail "a file under ~/.cargo other than credentials must not be redacted"

grep -qF 'task-20260101-7 started, risk-based check passes' "$clippy" \
  || fail "a line with task- or risk- text must not be redacted by the sk- rule"
grep -qF "$FAKE_HOME/.gitconfig ok" "$clippy" \
  || fail "a file under home but not in .git/ must not be redacted"
grep -qF 'plain line that must survive' "$OUT_DIR/rust-ci-test.log" \
  || fail "a clean log must pass through unchanged"
assert_absent_text "fixture-multi-a-5521" "$OUT_DIR/rust-ci-test.log" "a multi-line FM_* value across CRLF line endings"
assert_absent_text "fixture-multi-b-5522" "$OUT_DIR/rust-ci-test.log" "a multi-line FM_* value across CRLF line endings"

for leak in "$FM_VALUE" "$TOKEN_VALUE" "$GH_TOKEN_LINE" "$FAKE_HOME/.ssh" "$FAKE_DB_PASSWORD" "$FAKE_DB_PWD" \
  "$FAKE_PASSWD" "$FAKE_PASSPHRASE" "$FAKE_CLIENT_SECRET" "$FAKE_SIGNING_KEY" "$FAKE_CREDENTIAL" \
  "fixture-multi-a-5521" "fixture-multi-b-5522" "fixture-pass-8812" "$FAKE_HOME/.netrc" "$FAKE_HOME/.aws" \
  "$FAKE_HOME/.git/config" "FAKEb64userpass1111"; do
  if grep -qF -- "$leak" "$OUT_DIR/_MANIFEST.txt" "$run_out"; then
    fail "manifest or helper output carries a redacted value or path"
  fi
done
grep -qF 'rust-ci-clippy.log: lines_matched=4' "$OUT_DIR/_MANIFEST.txt" \
  || fail "manifest must count the four credential-pattern lines in rust-ci-clippy.log"
grep -qF 'rust-ci-check.log: lines_matched=7 by_pattern=[Authorization Basic=2, URL userinfo=1, ghs_=1, github_pat_=1, ghu_=1, ghr_=1] env_values=8 home_paths=5' "$OUT_DIR/_MANIFEST.txt" \
  || fail "manifest must count each credential shape, env value and home path in rust-ci-check.log"
for name in FAKE_API_TOKEN FM_TEST_REDACT FAKE_DB_PASSWORD FAKE_DB_PWD FAKE_PASSWD FAKE_PASSPHRASE FAKE_CLIENT_SECRET FAKE_SIGNING_KEY FAKE_CREDENTIAL FM_MULTILINE; do
  grep '^env_names_redacted: ' "$OUT_DIR/_MANIFEST.txt" | grep -qF "$name" \
    || fail "manifest must name the redacted environment variable $name"
done
grep -qF 'home_paths=1' "$OUT_DIR/_MANIFEST.txt" \
  || fail "manifest must count the one redacted home credential path"
pass "helper redacts credential, env-name, multi-line and home credential shapes"

# Windows homes appear in logs as the native backslash form, a forward-slash form, and a JSON-escaped
# doubled-backslash form. The helper must redact the home credential path in every spelling.
WIN_HOME='C:\Users\fake'
WIN_IN="$TMP_ROOT/win-in"
WIN_OUT="$TMP_ROOT/win-out"
mkdir -p "$WIN_IN"
printf '%s\n' \
  'native C:\Users\fake\.netrc failed' \
  'forward C:/Users/fake/.ssh/id_fake failed' \
  'json "C:\\Users\\fake\\.cargo\\credentials.toml" failed' \
  'forward C:/Users/fake/.gitconfig ok' > "$WIN_IN/rust-ci-windows-home.log"
# A Windows-style HOME is relative on this host, and pwsh writes its profile cache under HOME, so the
# helper runs from the temp root rather than the working tree.
(cd "$TMP_ROOT" && env -i HOME="$WIN_HOME" PATH="$PATH" pwsh -NoProfile -NonInteractive -File "$REDACT" \
  -InputDir "$WIN_IN" -OutDir "$WIN_OUT") >"$TMP_ROOT/win-run.out" 2>&1 \
  || fail "redaction helper failed for a Windows home: $(cat "$TMP_ROOT/win-run.out")"
win="$WIN_OUT/rust-ci-windows-home.log"
assert_absent_text 'C:\Users\fake\.netrc' "$win" "a native Windows home ~/.netrc path"
assert_absent_text 'C:/Users/fake/.ssh' "$win" "a forward-slash Windows home ~/.ssh path"
assert_absent_text 'C:\\Users\\fake\\.cargo' "$win" "a JSON-escaped Windows home ~/.cargo/credentials path"
grep -qF 'forward C:/Users/fake/.gitconfig ok' "$win" \
  || fail "a file under a Windows home but not in .git/ must not be redacted"
grep -q 'rust-ci-windows-home.log: .*home_paths=3$' "$WIN_OUT/_MANIFEST.txt" \
  || fail "manifest must count the three Windows home credential paths"
pass "helper redacts Windows home credential paths in native, forward-slash and JSON-escaped forms"

# An empty run is still a valid upload: a failure before any log exists must leave a manifest.
EMPTY_IN="$TMP_ROOT/empty-in"
EMPTY_OUT="$TMP_ROOT/empty-out"
mkdir -p "$EMPTY_IN"
run_redact -InputDir "$EMPTY_IN" -OutDir "$EMPTY_OUT" >/dev/null 2>&1 \
  || fail "helper must succeed when no log exists"
grep -qF 'files: 0' "$EMPTY_OUT/_MANIFEST.txt" \
  || fail "helper must write a manifest that records zero logs"
pass "helper writes a manifest even when no log exists"

# A missing input directory is an error, not an empty success.
if run_redact -InputDir "$TMP_ROOT/absent" -OutDir "$TMP_ROOT/absent-out" >/dev/null 2>&1; then
  fail "helper must fail when the input directory is missing"
fi
pass "helper refuses a missing input directory"
