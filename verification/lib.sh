# Shared by the TLA+ verify skills under .cursor/skills/verify-stack-*/: source it, then call
# `verify_model <skill dir> <spec>` and `verify_rust <test>...` (or `verify_rust_list <file>`).
# Needs bash, Java 11+, git, curl, and sha256sum or shasum.
set -euo pipefail

VERIFY_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
TLA_URL=https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
TLA_JAR="$VERIFY_ROOT/target/verification/tla2tools.jar"

# Whether the file $1 has the pinned TLA+ v1.7.4 checksum.
tla_checksum_ok() {
  local actual
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$1" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$1" | cut -d' ' -f1)
  fi
  [ "$actual" = "$TLA_SHA256" ]
}

# The TLA+ tools jar, downloaded on first use, and checked against the pinned checksum on every
# use, a cached copy included: a jar that does not match stops the run.
tla_jar() {
  mkdir -p "$(dirname "$TLA_JAR")"
  if [ ! -f "$TLA_JAR" ]; then
    local partial
    partial=$(mktemp "$TLA_JAR.XXXXXX")
    curl -fsSL "$TLA_URL" -o "$partial"
    if ! tla_checksum_ok "$partial"; then
      rm -f "$partial"
      echo "downloaded tla2tools.jar does not match sha256 $TLA_SHA256" >&2
      return 1
    fi
    mv "$partial" "$TLA_JAR"
  fi
  if ! tla_checksum_ok "$TLA_JAR"; then
    echo "$TLA_JAR does not match sha256 $TLA_SHA256; delete it to download it again" >&2
    return 1
  fi
}

# A fresh evidence directory for the skill named $1, recording the source it checks.
verify_evidence() {
  mkdir -p "$VERIFY_ROOT/target/verification"
  EVIDENCE=$(mktemp -d "$VERIFY_ROOT/target/verification/$1.XXXXXX")
  {
    git -C "$VERIFY_ROOT" rev-parse HEAD
    git -C "$VERIFY_ROOT" status --porcelain
  } >"$EVIDENCE/source.log"
  printf 'Evidence: %s\n' "$EVIDENCE"
}

# Check the spec $2 in the skill directory $1 under every config `expected.tsv` lists, each
# with one worker so counts are reproducible, and compare the exit code, the violated invariant
# (or `-` for a pass) and the distinct states with the table. Every config file in the directory
# must be listed. Writes `<config>.log` and a `summary.tsv` to $EVIDENCE.
verify_model() {
  local dir=$1 spec=$2 failed=0 cfg exit invariant states
  tla_jar
  for cfg in "$dir"/*.cfg; do
    if ! cut -f1 "$dir/expected.tsv" | grep -qx "$(basename "$cfg")"; then
      echo "$(basename "$cfg") is not listed in $dir/expected.tsv" >&2
      failed=1
    fi
  done
  printf 'config\texit\tviolated\tdistinct_states\tverdict\n' >"$EVIDENCE/summary.tsv"
  while IFS=$'\t' read -r cfg exit invariant states; do
    case "$cfg" in ''|'#'*|config) continue ;; esac
    local name=${cfg%.cfg} log status=0 got_inv got_states verdict=ok
    log="$EVIDENCE/$name.log"
    (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC -workers 1 -metadir "$EVIDENCE/$name.states" \
      -config "$cfg" "$spec" >"$log" 2>&1) || status=$?
    got_inv=$(sed -n 's/^Error: Invariant \([A-Za-z0-9_]*\) is violated\.$/\1/p' "$log" | head -1)
    got_states=$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' "$log" | tail -1)
    [ -n "$got_inv" ] || got_inv=-
    if [ "$status" != "$exit" ] || [ "$got_inv" != "$invariant" ] || [ "$got_states" != "$states" ]; then
      verdict=MISMATCH
      failed=1
    fi
    if [ "$invariant" = - ] && ! grep -q '^Model checking completed. No error has been found.$' "$log"; then
      verdict=MISMATCH
      failed=1
    fi
    printf '%s\t%s\t%s\t%s\t%s\n' "$cfg" "$status" "$got_inv" "$got_states" "$verdict" >>"$EVIDENCE/summary.tsv"
    printf '%-44s exit %-3s %-34s %10s distinct states  %s\n' "$cfg" "$status" "$got_inv" "$got_states" "$verdict"
    if [ "$verdict" != ok ]; then
      echo "  expected exit $exit, $invariant, $states distinct states; see $log" >&2
    fi
  done <"$dir/expected.tsv"
  return "$failed"
}

# Run the named Rust tests, each exactly, and require each to pass. A name is a library test
# (`module::tests::name`), or an integration test as `tests/<crate>.rs <name>`; each target is
# built and run once. Set VERIFY_SKIP_RUST=1 to check only the models; CARGO overrides the
# command (e.g. CARGO="cargo +1.93.1"), and CARGO_BUILD_JOBS bounds the build as usual.
verify_rust() {
  if [ "${VERIFY_SKIP_RUST:-0}" = 1 ]; then
    echo "Rust tests skipped (VERIFY_SKIP_RUST=1)"
    return 0
  fi
  local cargo test target targets=() names=() name entry
  read -r -a cargo <<<"${CARGO:-cargo}"
  : >"$EVIDENCE/rust.log"
  for entry in "$@"; do
    case "$entry" in
      "tests/"*" "*) target=${entry%% *}; target=${target#tests/}; target=${target%.rs} ;;
      *) target=lib ;;
    esac
    case " ${targets[*]-} " in *" $target "*) ;; *) targets+=("$target") ;; esac
  done
  for target in "${targets[@]}"; do
    names=()
    for entry in "$@"; do
      case "$entry" in
        "tests/$target.rs "*) names+=("${entry#* }") ;;
        "tests/"*) ;;
        *) [ "$target" = lib ] && names+=("$entry") ;;
      esac
    done
    local select=(--lib)
    [ "$target" = lib ] || select=(--test "$target")
    (cd "$VERIFY_ROOT" && "${cargo[@]}" test --locked --target-dir "$VERIFY_ROOT/target/verification/build" "${select[@]}" -- --exact "${names[@]}") \
      >>"$EVIDENCE/rust.log" 2>&1 || { tail -40 "$EVIDENCE/rust.log" >&2; return 1; }
  done
  for entry in "$@"; do
    name=${entry#tests/* }
    grep -Fxq "test $name ... ok" "$EVIDENCE/rust.log" || { echo "Rust test $entry did not pass; see $EVIDENCE/rust.log" >&2; return 1; }
  done
  echo "Rust: $# tests passed (${targets[*]}; $EVIDENCE/rust.log)"
}

# verify_rust with the tests listed in the file $1, one per line (`#` comments and blank lines
# skipped).
verify_rust_list() {
  local tests=() test
  while IFS= read -r test; do
    case "$test" in ''|'#'*) ;; *) tests+=("$test") ;; esac
  done <"$1"
  verify_rust "${tests[@]}"
}
