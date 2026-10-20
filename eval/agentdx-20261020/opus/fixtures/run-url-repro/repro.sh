#!/usr/bin/env bash
# usage: repro.sh <checkout copy of run-url-repro> ; needs an isolated env (env.sh) and stack on PATH.
set -u
cd "$1"
stack compile >/dev/null && stack up --ttl 10m >/dev/null || { echo "setup failed"; exit 2; }
exec_url=$(stack exec --require web -- sh -c 'printf %s "$WEB_URL"')
with=$(stack run with_dep 2>/dev/null | sed -n 's/^WEB_URL=//p')
without=$(stack run without_dep 2>/dev/null | sed -n 's/^WEB_URL=//p')
echo "exec:        $exec_url"
echo "run with:    $with"
echo "run without: $without"
stack down >/dev/null
rc=0
case $exec_url in http://127.0.0.1:*) ;; *) echo "FAIL: exec sees $exec_url, not a loopback URL"; rc=1 ;; esac
[ "$with" = "$exec_url" ] || { echo "FAIL: run (task lists service) sees $with, exec sees $exec_url"; rc=1; }
[ "$without" = "$exec_url" ] || { echo "FAIL: run (task lists no service) sees $without"; rc=1; }
exit $rc
