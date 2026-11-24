#!/usr/bin/env bash
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
root=$(git -C "$skill" rev-parse --show-toplevel)
mkdir -p "$root/target/verification"
evidence=$(mktemp -d "$root/target/verification/pgtemplate.XXXXXX")
{
  git -C "$root" rev-parse HEAD
  git -C "$root" status --porcelain
} >"$evidence/source.log"
jar="$root/target/verification/tla2tools.jar"
checksum=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
if [ ! -f "$jar" ]; then
  curl -fsSL https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar -o "$evidence/tla2tools.jar"
  printf '%s  %s\n' "$checksum" "$evidence/tla2tools.jar" | shasum -a 256 -c -
  mv "$evidence/tla2tools.jar" "$jar"
fi
printf '%s  %s\n' "$checksum" "$jar" | shasum -a 256 -c -
printf 'Evidence: %s\n' "$evidence"
cd "$skill"
java -cp "$jar" tlc2.TLC -workers 1 -metadir "$evidence/baseline" -config FALSE.cfg Generation.tla >"$evidence/baseline.log" 2>&1
rg 'No error has been found' "$evidence/baseline.log"
rg 'distinct states found' "$evidence/baseline.log"
status=0
java -cp "$jar" tlc2.TLC -workers 1 -metadir "$evidence/mutant" -config TRUE.cfg Generation.tla >"$evidence/mutant.log" 2>&1 || status=$?
[ "$status" -eq 12 ]
rg 'Invariant AcceptedRecordMatchesObservedDirectory is violated' "$evidence/mutant.log"
cd "$root"
test_name=pgtemplate::tests::a_template_replaced_while_its_record_is_read_is_read_again_under_the_lock
cargo test --locked --target-dir "$root/target/verification/build" \
  --lib "$test_name" -- --exact 2>&1 | tee "$evidence/rust.log"
rg -Fx "test $test_name ... ok" "$evidence/rust.log"
