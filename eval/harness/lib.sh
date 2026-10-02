# Shared step recorder. Each step: name, exit code, wall ms, output tail -> results/<tool>.tsv + logs
: "${TOOL:?TOOL not set}"
RES=/results
mkdir -p "$RES/logs"
step() {
  local name=$1; shift
  local s e rc
  s=$(date +%s%N)
  timeout "${STEP_TIMEOUT:-900}" bash -c "$*" > "$RES/logs/$TOOL.$name.log" 2>&1 < /dev/null
  rc=$?
  e=$(date +%s%N)
  printf '%s\t%s\t%s\t%s\n' "$TOOL" "$name" "$rc" "$(( (e - s) / 1000000 ))" | tee -a "$RES/$TOOL.tsv"
  return $rc
}
# Wait until a probe succeeds; report ms to ready
wait_ready() {
  local name=$1 probe=$2 limit=${3:-120}
  local s e i
  s=$(date +%s%N)
  for ((i=0; i<limit*5; i++)); do
    if bash -c "$probe" >/dev/null 2>&1; then
      e=$(date +%s%N)
      printf '%s\t%s\t0\t%s\n' "$TOOL" "$name" "$(( (e - s) / 1000000 ))" | tee -a "$RES/$TOOL.tsv"
      return 0
    fi
    sleep 0.2
  done
  printf '%s\t%s\tTIMEOUT\t%s\n' "$TOOL" "$name" "$((limit*1000))" | tee -a "$RES/$TOOL.tsv"
  return 1
}
