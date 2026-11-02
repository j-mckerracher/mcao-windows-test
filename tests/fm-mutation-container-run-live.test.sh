#!/usr/bin/env bash
# tests/fm-mutation-container-run-live.test.sh - the container-supervised mutation run
# against a real container runtime (opt-in). Off unless FM_LIVE_MUTATION_CONTAINER=1
# requests it; once requested, a missing runtime or image fails rather than skips.
# The fixture's oracle leaves a setsid-detached sleep behind, the escape the container
# exists to contain: the record must still publish, and no container and no host process
# may survive the run.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

fm_live_gate opt-in FM_LIVE_MUTATION_CONTAINER docker

IMAGE=node:24-bookworm
WRAP="$ROOT/bin/fm-mutation-container-run.sh"
CLASSIFY="$ROOT/bin/fm-mutation-classify.sh"
TMP=$(fm_test_tmproot fm-mutation-container-live)

docker info >/dev/null 2>&1 \
  || fail "FM_LIVE_MUTATION_CONTAINER=1 was requested but docker info failed; start the container runtime"
docker image inspect "$IMAGE" >/dev/null 2>&1 \
  || fail "FM_LIVE_MUTATION_CONTAINER=1 was requested but $IMAGE is not present locally; it is never pulled here"

FX="$TMP/fixture"
mkdir -p "$FX/bin" "$FX/tests"
cat >"$FX/bin/target.sh" <<'SH'
#!/usr/bin/env bash
if [ "$1" -eq 1 ]; then echo ok; fi
SH
cat >"$FX/tests/target.test.sh" <<'SH'
#!/usr/bin/env bash
setsid sh -c 'exec sleep 30173' </dev/null >/dev/null 2>&1 &
grep -q -- "-eq 1" bin/target.sh
SH
chmod +x "$FX/tests/target.test.sh"
git -C "$FX" init -q -b main
git -C "$FX" add -A
git -C "$FX" -c user.name='Firstmate Tests' -c user.email='tests@example.invalid' commit -qm fixture

(cd "$FX" && "$CLASSIFY" bin/target.sh) >"$TMP/classifier.tsv" 2>"$TMP/classify.err"
RC=$?
expect_code 0 "$RC" "classifier must succeed before the container run"

OUT=$(bash "$WRAP" --root "$FX" --container-timeout 240 --timeout 5 --budget 120 \
  <"$TMP/classifier.tsv" 2>"$TMP/stderr")
RC=$?
expect_code 0 "$RC" "the container run must publish a record; stderr: $(cat "$TMP/stderr")"
assert_contains "$OUT" $'summary\t-\t-\t-\tpublished\t' "the record must carry one published summary"
assert_contains "$OUT" "supervision=container" "the published summary must carry the supervision token"
assert_contains "$OUT" $'mutant\tbin/target.sh\tshell\tL2-eq-ne\tkilled' "the mutant must be killed by the oracle"
pass "a real container run publishes a record with the supervision token"

LEFT=$(docker ps -a --filter 'name=^fm-mutation-run-' --format '{{.Names}}')
[ -z "$LEFT" ] || fail "containers left behind after the run: $LEFT"
pass "no run container survives the run"

HOST=$(pgrep -fl '[s]leep 30173' || true)
[ -z "$HOST" ] || fail "the detached sleep escaped to the host: $HOST"
pass "the detached sleep never reached a host process"

echo "# fm-mutation-container-run-live.test.sh: all assertions passed"
