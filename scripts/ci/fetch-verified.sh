#!/usr/bin/env bash
# Download a pinned file and keep it only if its SHA-256 is the pinned one:
#   scripts/ci/fetch-verified.sh URL SHA256 OUTPUT
# The download is bounded (connect 10s, 300s each try, three retries) and HTTPS only. An OUTPUT
# already there is checked the same way on every use; one that does not match stops the run
# (delete it to download it again). Needs curl and sha256sum or shasum.
set -euo pipefail
[ $# -eq 3 ] || { echo "usage: $0 URL SHA256 OUTPUT" >&2; exit 2; }
url=$1 want=$2 out=$3
case "$url" in https://*) ;; *) echo "not an HTTPS URL: $url" >&2; exit 2 ;; esac
[[ "$want" =~ ^[0-9a-f]{64}$ ]] || { echo "not a SHA-256: $want" >&2; exit 2; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

if [ -e "$out" ]; then
  got=$(sha256 "$out")
  [ "$got" = "$want" ] || { echo "$out has SHA-256 $got, not the pinned $want; delete it to download it again" >&2; exit 1; }
  exit 0
fi
mkdir -p "$(dirname "$out")"
partial=$(mktemp "$out.XXXXXX")
trap 'rm -f "$partial"' EXIT
curl --proto '=https' --tlsv1.2 --connect-timeout 10 --max-time 300 --retry 3 --retry-delay 5 \
  --retry-max-time 900 -fsSL -o "$partial" "$url"
got=$(sha256 "$partial")
[ "$got" = "$want" ] || { echo "$url has SHA-256 $got, not the pinned $want" >&2; exit 1; }
mv "$partial" "$out"
trap - EXIT
