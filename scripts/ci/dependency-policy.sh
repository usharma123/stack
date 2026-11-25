#!/usr/bin/env bash
# Check Cargo.lock against deny.toml with a pinned cargo-deny:
#   scripts/ci/dependency-policy.sh pinned   bans, licenses, sources and advisories, offline,
#                                            against the RustSec database at ADVISORY_DB_COMMIT
#                                            (every pull request: its result depends on its
#                                            content alone, so yanked crates are not judged here)
#   scripts/ci/dependency-policy.sh latest   advisories and yanked crates against the newest
#                                            database and crates.io index (audit.yml)
# cargo-deny's archive is kept under target/ci-tools and checked against its pinned SHA-256 on
# every use. Its advisory databases are kept there too (deny.toml's db-path is replaced in a
# copy of it), so a developer's ~/.cargo/advisory-dbs is never touched. The crate graph comes
# from `cargo metadata --locked`; CARGO overrides the cargo command (e.g. CARGO="cargo +1.93.1").
# Linux x86_64 (CI) and macOS arm64. Needs git and curl.
set -euo pipefail
mode=${1-}
case "$mode" in pinned|latest) ;; *) echo "usage: $0 pinned|latest" >&2; exit 2 ;; esac
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

CARGO_DENY_VERSION=0.20.2
# Move forward in the pull request that takes a new advisory into account (see deny.toml).
ADVISORY_DB_COMMIT=10db6998b5d74ec438bcf0763125bd425a068a8f
ADVISORY_DB_URL=https://github.com/rustsec/advisory-db
# Where cargo-deny 0.20.2 keeps the database of that URL under its db-path.
ADVISORY_DB_DIR=advisory-db-3157b0e258782691
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) asset=x86_64-unknown-linux-musl sha=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
  Darwin-arm64) asset=aarch64-apple-darwin sha=fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3 ;;
  *) echo "no pinned cargo-deny for $(uname -sm)" >&2; exit 2 ;;
esac

tools="$root/target/ci-tools"
archive="$tools/cargo-deny-$CARGO_DENY_VERSION-$asset.tar.gz"
"$root/scripts/ci/fetch-verified.sh" \
  "https://github.com/EmbarkStudios/cargo-deny/releases/download/$CARGO_DENY_VERSION/cargo-deny-$CARGO_DENY_VERSION-$asset.tar.gz" "$sha" "$archive"
work=$(mktemp -d "${TMPDIR:-/tmp}/stack-deny.XXXXXX")
trap 'rm -rf "$work"' EXIT
tar -xzf "$archive" -C "$work" --strip-components 1 "cargo-deny-$CARGO_DENY_VERSION-$asset/cargo-deny"
"$work/cargo-deny" --version

read -r -a cargo <<<"${CARGO:-cargo}"
"${cargo[@]}" metadata --locked --format-version 1 >"$work/metadata.json"
# cargo-deny runs cargo itself, taking CARGO for its path: give it the plain name, and the
# toolchain a `cargo +<toolchain>` named.
deny=(env -u CARGO)
if [ ${#cargo[@]} -eq 2 ] && [[ "${cargo[1]}" == +* ]]; then deny+=(RUSTUP_TOOLCHAIN="${cargo[1]#+}"); fi
deny+=("$work/cargo-deny")

dbs="$tools/advisory-dbs-$mode"
case "$dbs" in *\"*|*\|*|*\\*) echo "cannot name $dbs in a TOML string" >&2; exit 2 ;; esac
grep -q '^db-path = ' deny.toml || { echo 'deny.toml has no db-path line to replace' >&2; exit 1; }
sed "s|^db-path = .*|db-path = \"$dbs\"|" deny.toml >"$work/deny.toml"
db="$dbs/$ADVISORY_DB_DIR"

if [ "$mode" = pinned ]; then
  if [ "$(git -C "$db" rev-parse HEAD 2>/dev/null)" != "$ADVISORY_DB_COMMIT" ]; then
    rm -rf "$db"
    mkdir -p "$db"
    git -C "$db" init -q
    # A transfer slower than 1 KB/s for a minute is abandoned.
    git -C "$db" -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60 fetch -q --depth 1 "$ADVISORY_DB_URL" "$ADVISORY_DB_COMMIT"
    git -C "$db" checkout -q --detach FETCH_HEAD
  fi
  # cargo-deny reads the working tree: it must be exactly the pinned commit's.
  [ "$(git -C "$db" rev-parse HEAD)" = "$ADVISORY_DB_COMMIT" ] || { echo "advisory database is not at $ADVISORY_DB_COMMIT" >&2; exit 1; }
  [ -z "$(git -C "$db" status --porcelain)" ] || { echo "the advisory database at $db was modified; delete it" >&2; exit 1; }
  echo "RustSec advisory database: $ADVISORY_DB_COMMIT ($(git -C "$db" show -s --format=%cI HEAD))"
  # Offline, with no index to read whether a crate was yanked since: `latest` judges that.
  "${deny[@]}" --offline --config "$work/deny.toml" --metadata-path "$work/metadata.json" check --show-stats --allow yanked --allow index-failure
else
  "${deny[@]}" --config "$work/deny.toml" --metadata-path "$work/metadata.json" check --show-stats advisories
  echo "RustSec advisory database checked: $(git -C "$db" rev-parse HEAD) ($(git -C "$db" show -s --format=%cI HEAD))"
fi
