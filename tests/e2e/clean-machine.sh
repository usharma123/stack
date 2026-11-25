#!/usr/bin/env bash
# Clean-machine smoke of a packaged Stack binary (macOS or Linux): a new HOME, no Stack, mise or
# XDG state from the caller, and no mise anywhere on PATH. `stack setup` must install the pinned
# mise itself (the managed path), then
#   setup -> doctor -> compile -> compile --locked -> up -> exec and run against the real
#   Postgres endpoint -> down
# with every service process gone and its port closed afterwards.
#
#   tests/e2e/clean-machine.sh <path-to-stack-binary>
#
# Needs bash, git, tar, jq and curl on the caller's PATH; Stack runs with only git and tar (and
# the system directories) on its PATH. STACK_SMOKE_SECONDS (default 900) bounds each Stack command:
# one still running then is stopped, its output shown, and the run fails. STACK_SMOKE_EVIDENCE
# can name an existing directory for every command's output. The work directory is kept when
# the run fails.
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
[ $# -eq 1 ] || { echo "usage: $0 <path-to-stack-binary>" >&2; exit 2; }
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[ -x "$binary" ] || { echo "$binary is not executable" >&2; exit 2; }
for tool in git tar jq; do
  command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 2; }
done
seconds=${STACK_SMOKE_SECONDS:-900}
# A short path: Postgres and Stack keep sockets and locks under HOME.
work=$(mktemp -d /tmp/ssmoke.XXXXXX)
work=$(cd "$work" && pwd -P)
home="$work/home" app="$work/app" out="$work/out" sysbin="$work/sysbin"
mkdir -p "$home" "$app" "$out" "$sysbin"
for tool in git tar; do ln -s "$(command -v "$tool")" "$sysbin/$tool"; done
clean_path="$sysbin:/usr/bin:/bin:/usr/sbin:/sbin"
managed="$home/.local/share/stack/bin/mise"

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "--- $*"; }

# Fail before anything runs if this machine has a mise Stack could find without setup.
IFS=: read -r -a dirs <<<"$clean_path"
for dir in "${dirs[@]}"; do
  if [ -e "$dir/mise" ]; then
    fail "$dir/mise exists: this runner is not a clean machine for the managed setup path"
  fi
done

# The clean environment: nothing inherited but what is named here.
clean_env=(HOME="$home" PATH="$clean_path" USER="${USER:-runner}" LOGNAME="${LOGNAME:-runner}"
  TMPDIR="$work/tmp" LANG=C.UTF-8 GIT_CONFIG_GLOBAL="$home/.gitconfig" GIT_CONFIG_NOSYSTEM=1)
# clean COMMAND...: COMMAND in the clean environment.
clean() { env -i "${clean_env[@]}" "$@"; }
mkdir -p "$work/tmp"

# stack NAME ARGS...: `stack ARGS...` in the app, cleanly, bounded by $seconds, with its output
# in $out/NAME.out. Returns its status; on a stall, stops it and returns 124. The background job
# is Stack itself (the subshell execs env, which execs Stack), so what is stopped is Stack, and
# no wrapper is left behind it.
running() { jobs -r -p | grep -qx "$1"; }
# stop PID: TERM, up to 5s for it to end, then KILL; reaped either way before it returns.
stop() {
  local pid=$1 tenths=0
  kill -TERM "$pid" 2>/dev/null || true
  while running "$pid" && [ "$tenths" -lt 50 ]; do
    sleep 0.1
    tenths=$((tenths + 1))
  done
  if running "$pid"; then kill -KILL "$pid" 2>/dev/null || true; fi
  wait "$pid" 2>/dev/null || true
}
current=
stack() {
  local name=$1 tenths=0 status=0
  shift
  (cd "$app" && exec env -i "${clean_env[@]}" "$binary" "$@") </dev/null >"$out/$name.out" 2>"$out/$name.err" &
  current=$!
  while running "$current"; do
    if [ "$tenths" -ge $((seconds * 10)) ]; then
      stop "$current"
      current=
      echo "stack $*: still running after ${seconds}s, stopped" >&2
      tail -c 4000 "$out/$name.err" >&2 || true
      return 124
    fi
    sleep 0.1
    tenths=$((tenths + 1))
  done
  wait "$current" || status=$?
  current=
  return "$status"
}
# ok NAME ARGS...: `stack --json ARGS...`, which must succeed with "ok": true.
ok() {
  local name=$1 status=0
  shift
  stack "$name" --json "$@" || status=$?
  if [ "$status" -ne 0 ] || ! jq -e '.ok == true' "$out/$name.out" >/dev/null 2>&1; then
    echo "stack --json $* exited $status:" >&2
    cat "$out/$name.out" >&2 || true
    tail -c 4000 "$out/$name.err" >&2 || true
    fail "stack $* did not succeed"
  fi
}
assert_json() { jq -e "$1" "$out/$2.out" >/dev/null || fail "$2: expected $1 in $(cat "$out/$2.out")"; }
accepting() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

up=false
cleanup() {
  local status=$?
  # A Stack command still running (the run interrupted) is stopped and reaped first.
  if [ -n "$current" ]; then
    stop "$current"
    current=
    status=1
  fi
  if [ "$up" = true ]; then
    stack cleanup-down --json down || status=1
  fi
  if [ -n "${STACK_SMOKE_EVIDENCE:-}" ]; then cp -R "$out/." "$STACK_SMOKE_EVIDENCE/" 2>/dev/null || true; fi
  if [ "$status" -eq 0 ]; then
    chmod -R u+w "$work" 2>/dev/null || true
    rm -rf "$work"
    echo "clean-machine smoke passed"
  else
    for log in "$out"/*.err; do
      [ -s "$log" ] || continue
      echo "=== $(basename "$log")" >&2
      tail -c 2000 "$log" >&2
    done
    echo "clean-machine smoke failed; work dir kept: $work" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

step "binary: $("$binary" --version) on $(uname -sm)"
step 'no mise is found before setup'
if clean sh -c 'command -v mise' >/dev/null 2>&1; then fail "a mise is on the clean PATH"; fi

step 'stack setup installs the pinned mise into the isolated data directory'
ok setup setup
assert_json '.data.installed == true and .data.first_on_path == true' setup
[ "$(jq -r .data.mise "$out/setup.out")" = "$managed" ] || fail "setup installed $(jq -r .data.mise "$out/setup.out"), not $managed"
[ -x "$managed" ] || fail "$managed is not an executable file"
pinned=$(sed -n 's/^pub const MISE_VERSION: &str = "\(.*\)";$/\1/p' "$(dirname "$0")/../../src/setup.rs")
[ -n "$pinned" ] || fail "cannot read MISE_VERSION from src/setup.rs"
case "$(jq -r .data.version "$out/setup.out")" in
  "$pinned "*|"$pinned") ;;
  *) fail "setup installed mise $(jq -r .data.version "$out/setup.out"), not the pinned $pinned" ;;
esac
ok setup-again setup
assert_json '.data.installed == false and .data.first_on_path == true' setup-again
[ "$(jq -r .data.mise "$out/setup-again.out")" = "$managed" ] || fail "a second setup found $(jq -r .data.mise "$out/setup-again.out")"

step 'a project, checked by doctor'
cat >"$app/stack.toml" <<'TOML'
[services.postgres]
preset = "postgres"
version = "17"

[tasks.smoke]
run = "psql \"$DATABASE_URL\" -X -Atc 'select 40 + 2'"
services = ["postgres"]
TOML
(cd "$app" && clean git init -q -b main && clean git config user.email smoke@example.invalid && clean git config user.name smoke)
ok doctor doctor

step 'compile, then compile --locked from the committed lock'
ok compile compile
[ -f "$app/stack.lock" ] || fail "compile wrote no stack.lock"
(cd "$app" && clean git add -A && clean git commit -qm smoke)
ok compile-locked compile --locked
(cd "$app" && clean git diff --exit-code --quiet) || fail "compile --locked changed committed files"

step 'up starts a verified Postgres'
up=true
ok up up --timeout 10m
assert_json '(.data.checks | length) == 1 and .data.checks[0].service == "postgres" and .data.checks[0].ready and .data.checks[0].identity == "instance"' up
assert_json '.data.checks[0].pid | type == "number" and . == floor and . > 0' up
port=$(jq -r '.data.checks[0].port' "$out/up.out")
pid=$(jq -r '.data.checks[0].pid' "$out/up.out")
[[ $pid =~ ^[1-9][0-9]*$ ]] || fail "up reported Postgres' PID as $pid, not a positive integer"
accepting "$port" || fail "nothing accepts on Postgres' port $port"

step 'exec reaches the endpoint with the managed mise first on PATH'
ok exec-query exec --require postgres -- sh -c 'psql "$DATABASE_URL" -X -Atc "select 40 + 2"; printf "%s\n" "$DATABASE_URL"'
assert_json '.data.exit_code == 0' exec-query
query=$(jq -r .data.stdout "$out/exec-query.out")
[ "$(printf '%s\n' "$query" | sed -n 1p)" = 42 ] || fail "the query returned: $query"
case "$(printf '%s\n' "$query" | sed -n 2p)" in
  *":$port/"*) ;;
  *) fail "DATABASE_URL does not name the verified port $port: $query" ;;
esac
ok exec-mise exec -- sh -c 'command -v mise'
[ "$(jq -r '.data.stdout | rtrimstr("\n")' "$out/exec-mise.out")" = "$managed" ] || fail "the command found $(jq -r .data.stdout "$out/exec-mise.out")"

step 'run runs the declared task against it'
stack run run smoke || { cat "$out/run.out" "$out/run.err" >&2; fail "stack run smoke failed"; }
grep -qx 42 "$out/run.out" || { cat "$out/run.out" "$out/run.err" >&2; fail "stack run smoke did not print 42"; }

step 'down stops everything'
ok down down
up=false
assert_json '.data.confirmed == true' down
if kill -0 "$pid" 2>/dev/null; then fail "Postgres ($pid) survived down"; fi
if accepting "$port"; then fail "port $port still accepts after down"; fi
# Whether status exits zero with nothing running is not the question; what it reports is.
stack status --json status || true
assert_json '.data.session == null and .data.healthy == false' status
