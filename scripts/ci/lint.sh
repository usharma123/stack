#!/usr/bin/env bash
# Lint the GitHub workflows, the Dependabot configuration and the shell scripts CI runs, with
# pinned tools, and fail on any finding:
#   scripts/ci/lint.sh
# - actionlint, with shellcheck on every `run:` script (config: .github/actionlint.yaml);
# - zizmor, offline, over .github (workflows and dependabot.yml);
# - shellcheck over tests/e2e, tests/fakes, verification, the verify skills and scripts/ci.
# Each tool's archive is kept under target/ci-tools and checked against its pinned SHA-256 on
# every use (scripts/ci/fetch-verified.sh). Linux x86_64 (CI) and macOS arm64.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

ACTIONLINT_VERSION=1.7.12
ZIZMOR_VERSION=1.32.0
SHELLCHECK_VERSION=0.11.0
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    actionlint_asset=linux_amd64 actionlint_sha=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
    zizmor_asset=x86_64-unknown-linux-gnu zizmor_sha=bb110cc765eca9cd7efb2e284a956be5974c60027b0970f16397d86208a9957b
    shellcheck_asset=linux.x86_64 shellcheck_sha=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 ;;
  Darwin-arm64)
    actionlint_asset=darwin_arm64 actionlint_sha=aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f
    zizmor_asset=aarch64-apple-darwin zizmor_sha=6efed18c4539a663fd3c851a2be9533fd8456c4e646d9416206da059154a0d3c
    shellcheck_asset=darwin.aarch64 shellcheck_sha=56affdd8de5527894dca6dc3d7e0a99a873b0f004d7aabc30ae407d3f48b0a79 ;;
  *) echo "no pinned linters for $(uname -sm)" >&2; exit 2 ;;
esac

tools="$root/target/ci-tools"
bin=$(mktemp -d "${TMPDIR:-/tmp}/stack-lint.XXXXXX")
trap 'rm -rf "$bin"' EXIT
fetch() { "$root/scripts/ci/fetch-verified.sh" "$@"; }

fetch "https://github.com/rhysd/actionlint/releases/download/v$ACTIONLINT_VERSION/actionlint_${ACTIONLINT_VERSION}_$actionlint_asset.tar.gz" \
  "$actionlint_sha" "$tools/actionlint-$ACTIONLINT_VERSION-$actionlint_asset.tar.gz"
tar -xzf "$tools/actionlint-$ACTIONLINT_VERSION-$actionlint_asset.tar.gz" -C "$bin" actionlint
fetch "https://github.com/zizmorcore/zizmor/releases/download/v$ZIZMOR_VERSION/zizmor-$zizmor_asset.tar.gz" \
  "$zizmor_sha" "$tools/zizmor-$ZIZMOR_VERSION-$zizmor_asset.tar.gz"
tar -xzf "$tools/zizmor-$ZIZMOR_VERSION-$zizmor_asset.tar.gz" -C "$bin" zizmor
fetch "https://github.com/koalaman/shellcheck/releases/download/v$SHELLCHECK_VERSION/shellcheck-v$SHELLCHECK_VERSION.$shellcheck_asset.tar.xz" \
  "$shellcheck_sha" "$tools/shellcheck-$SHELLCHECK_VERSION-$shellcheck_asset.tar.xz"
tar -xJf "$tools/shellcheck-$SHELLCHECK_VERSION-$shellcheck_asset.tar.xz" -C "$bin" --strip-components 1 "shellcheck-v$SHELLCHECK_VERSION/shellcheck"

"$bin/actionlint" -version | head -1
"$bin/zizmor" --version
"$bin/shellcheck" --version | sed -n 2p

failed=0
echo '== actionlint'
"$bin/actionlint" -shellcheck "$bin/shellcheck" -pyflakes '' -config-file .github/actionlint.yaml || failed=1
echo '== zizmor'
"$bin/zizmor" --offline --no-progress .github || failed=1
echo '== shellcheck'
scripts=()
while IFS= read -r script; do scripts+=("$script"); done < <(git ls-files 'tests/e2e/*.sh' 'tests/fakes/*.sh' 'verification/*.sh' '.cursor/skills/*.sh' 'scripts/ci/*.sh')
[ ${#scripts[@]} -gt 0 ] || { echo 'no scripts found to check' >&2; exit 1; }
"$bin/shellcheck" "${scripts[@]}" || failed=1
[ "$failed" -eq 0 ] && echo "lint passed: actionlint, zizmor and shellcheck (${#scripts[@]} scripts)"
exit "$failed"
