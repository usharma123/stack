#!/usr/bin/env bash
# Command receipt runner: rx <label> <cmd> [args...]
# Writes $RX_DIR/<seq>-<label>.{cmd,out,err,meta} and prints a one-line summary plus stdout tail.
# Optional: RX_TIMEOUT=<secs> bounds the command (process group killed via perl alarm).
set -u
: "${RX_DIR:?set RX_DIR}"
mkdir -p "$RX_DIR"
label=$1; shift
seq=$(( $(ls "$RX_DIR" 2>/dev/null | grep -c '\.meta$') + 1 ))
base=$(printf '%s/%03d-%s' "$RX_DIR" "$seq" "$label")
printf '%q ' "$@" >"$base.cmd"; echo >>"$base.cmd"
echo "cwd=$PWD" >>"$base.cmd"
start=$(python3 -c 'import time;print(time.time())')
if [ -n "${RX_TIMEOUT:-}" ]; then
  perl -e 'setpgrp(0,0); $SIG{ALRM}=sub{kill "KILL", -$$; exit 137}; alarm shift; $p=fork; if(!$p){exec @ARGV} waitpid($p,0); exit(($?&127) ? ($?&127)+128 : $?>>8)' "$RX_TIMEOUT" "$@" >"$base.out" 2>"$base.err" </dev/null
else
  "$@" >"$base.out" 2>"$base.err" </dev/null
fi
rc=$?
end=$(python3 -c 'import time;print(time.time())')
dur=$(python3 -c "print(round($end-$start,2))")
printf 'exit=%s secs=%s\n' "$rc" "$dur" >"$base.meta"
echo "[$seq $label] exit=$rc ${dur}s -> $base"
tail -c "${RX_SHOW:-1500}" "$base.out"
[ -s "$base.err" ] && { echo "--stderr--"; tail -c "${RX_SHOW:-1500}" "$base.err"; }
exit $rc
