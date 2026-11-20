#!/usr/bin/env bash
# Resolver cases for fresh-install, install.sh input validation, plus broken
# release assets on the strict path.
# Hostile inputs such as '$(id)' are meant literally, never expanded.
# shellcheck disable=SC2016
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
resolve=$root/scripts/fresh-install-version.sh
work=$(mktemp -d)
server_pid=
trap 'if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; fi; rm -rf "$work"' EXIT

fail() {
  printf 'fresh-install-version-test: %s\n' "$1" >&2
  exit 1
}

expect_version() {
  local want=$1
  shift
  local got
  got=$("$resolve" "$@")
  if [ "$got" != "$want" ]; then
    fail "expected $want, got $got ($*)"
  fi
}

expect_strict_mismatch() {
  local err code
  set +e
  err=$("$resolve" "$@" 2>&1 >/dev/null)
  code=$?
  set -e
  if [ "$code" -eq 0 ]; then
    fail "strict path was skipped ($*)"
  fi
  if ! printf '%s\n' "$err" | grep -q 'does not match'; then
    fail "strict path failed without the mismatch error ($*): $err"
  fi
}

expect_version 0.3.2 pull_request 0.9.9 v0.3.2 no refs/pull/7/merge
expect_version 0.3.2 pull_request 0.3.2 v0.3.2 yes refs/pull/7/merge
expect_version 0.3.2 push 0.3.2 v0.3.2 yes refs/heads/main
expect_version 0.3.2 push 0.3.2 v0.3.2 yes refs/tags/v0.3.2

expect_strict_mismatch push 0.9.9 v0.3.2 no refs/heads/main
expect_strict_mismatch push 0.9.9 v0.3.2 no refs/tags/v0.9.9
expect_strict_mismatch pull_request 0.9.9 v0.3.2 no refs/tags/v0.9.9
expect_strict_mismatch pull_request 0.3.1 v0.3.2 yes refs/pull/7/merge

# The resolver never prints anything but a release version, whatever the
# checkout version or the latest tag contain.
expect_resolver_refuses() {
  local out code
  set +e
  out=$("$resolve" "$@" 2>/dev/null)
  code=$?
  set -e
  if [ "$code" -eq 0 ] || [ -n "$out" ]; then
    fail "resolver accepted bad input ($*): $out"
  fi
}

for bad in '../0.3.2' '0.3.2/../../x' '0.3.2;rm -rf /' '$(id)' '0.3' '0.3.2-' \
  'https://evil.example/0.3.2' "0.3.2
0.3.3" 'v0.3.2'; do
  expect_resolver_refuses pull_request "$bad" v0.3.2 no refs/pull/7/merge
  expect_resolver_refuses push "$bad" "v$bad" yes refs/heads/main
done
for bad in '0.3.2' 'v../0.3.2' 'v0.3.2/../../x' 'v0.3.2;rm' 'vhttps://evil.example' 'v' \
  "v0.3.2
v0.3.3"; do
  expect_resolver_refuses pull_request 0.9.9 "$bad" no refs/pull/7/merge
done

if grep -q 'test "$latest" = "v${version}"' "$root/.github/workflows/ci.yml"; then
  fail "workflow still asserts latest == Cargo.toml on every event"
fi
if ! grep -q 'fresh-install-version.sh' "$root/.github/workflows/ci.yml"; then
  fail "workflow does not resolve the install version"
fi
if ! grep -q 'OMACORTEX_INSTALL_VERSION' "$root/.github/workflows/ci.yml"; then
  fail "workflow does not pass the resolved version to install.sh"
fi

# install.sh validates every input that reaches a URL before it downloads.
# A fake curl records each URL and fails, so nothing leaves the machine.
fakebin="$work/fakebin"
mkdir -p "$fakebin" "$work/install-home"
cat > "$fakebin/curl" <<'SH'
#!/bin/sh
for arg in "$@"; do last=$arg; done
printf '%s\n' "$last" >> "$FAKE_CURL_LOG"
exit 22
SH
chmod +x "$fakebin/curl"
curl_log="$work/curl.log"

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) fail "no fixture arch for $(uname -m)" ;;
esac

# Runs install.sh --download with only the given variables set.
# Sets install_out and install_code.
run_install() {
  : > "$curl_log"
  set +e
  install_out=$(env -i PATH="$fakebin:$PATH" HOME="$work/install-home" \
    FAKE_CURL_LOG="$curl_log" "$@" "$root/install.sh" --download 2>&1)
  install_code=$?
  set -e
}

expect_install_refused() {
  local want=$1
  shift
  run_install "$@"
  if [ "$install_code" -eq 0 ]; then
    fail "install.sh accepted bad input ($*)"
  fi
  if ! printf '%s\n' "$install_out" | grep -qF "$want"; then
    fail "install.sh refused ($*) without '$want': $install_out"
  fi
  if [ -s "$curl_log" ]; then
    fail "install.sh fetched before refusing ($*): $(cat "$curl_log")"
  fi
}

expect_install_fetches() {
  local url=$1
  shift
  run_install "$@"
  if [ "$install_code" -eq 0 ]; then
    fail "install.sh succeeded against a failing curl ($*)"
  fi
  if ! printf '%s\n' "$install_out" | grep -qF "download failed"; then
    fail "install.sh did not reach the download ($*): $install_out"
  fi
  if [ "$(head -n 1 "$curl_log")" != "$url" ]; then
    fail "install.sh fetched $(head -n 1 "$curl_log"), want $url ($*)"
  fi
}

gh_release=https://github.com/anonb3ll/omaCortex/releases/download
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

for good in 1.2.3 v1.2.3 0.3.2-rc.1 v10.20.30-beta-2.x; do
  plain=${good#v}
  expect_install_fetches \
    "$gh_release/v$plain/omacortex-$plain-$arch-linux.tar.gz" \
    OMACORTEX_INSTALL_VERSION="$good"
done
expect_install_fetches \
  "$gh_release/v$cargo_version/omacortex-$cargo_version-$arch-linux.tar.gz"
expect_install_fetches \
  "https://127.0.0.1:8443/releases/download/v1.2.3/omacortex-1.2.3-$arch-linux.tar.gz" \
  OMACORTEX_INSTALL_VERSION=1.2.3 \
  OMACORTEX_RELEASE_BASE=https://127.0.0.1:8443/releases/download/v1.2.3
expect_install_fetches \
  "https://localhost/r/omacortex-1.2.3-$arch-linux.tar.gz" \
  OMACORTEX_INSTALL_VERSION=1.2.3 OMACORTEX_RELEASE_BASE=https://localhost/r

for bad in '' ../ ../../etc/passwd '1.2.3;rm' '1.2.3;rm -rf /' '1.2.3/../../x' \
  '$(id)' '`id`' '1.2.3 ' ' 1.2.3' '1.2' '1.2.3-' '1.2.3-a..b' '1.2.3?x=y' \
  '1.2.3#x' '1.2.3%2f..' vv1.2.3 https://evil.example/1.2.3 \
  'http://127.0.0.1/1.2.3' "1.2.3
4.5.6" "1.2.3$(printf '\t')"; do
  expect_install_refused "OMACORTEX_INSTALL_VERSION must be a release version" \
    OMACORTEX_INSTALL_VERSION="$bad"
done

for bad in '' http://127.0.0.1/r https://evil.example/r https://127.0.0.1.evil.example/r \
  https://localhost.evil.example/r 'https://127.0.0.1@evil.example/r' \
  https://127.0.0.1/../r https://127.0.0.1/r/.. https://127.0.0.1/r/./s \
  'https://127.0.0.1/r?x=y' 'https://127.0.0.1/r;id' 'https://127.0.0.1/r s' \
  file:///etc/passwd 'https://127.0.0.1/r$(id)'; do
  expect_install_refused "OMACORTEX_RELEASE_BASE must be an https URL" \
    OMACORTEX_INSTALL_VERSION=1.2.3 OMACORTEX_RELEASE_BASE="$bad"
done

# Strict path (main) selects the checkout version, then the asset fails checksum.
expected=$("$resolve" push 9.9.9 v9.9.9 yes refs/heads/main)
if [ "$expected" != "9.9.9" ]; then
  fail "strict path did not keep the checkout version"
fi

name="omacortex-${expected}-${arch}-linux"
rel="$work/www/releases/download/v${expected}"
mkdir -p "$rel"
printf 'not a release archive\n' > "$rel/$name.tar.gz"
printf '%s  ./%s.tar.gz\n' \
  "0000000000000000000000000000000000000000000000000000000000000000" \
  "$name" > "$rel/checksums.txt"

# Overridden versions get no shortcut past the checksum: a missing entry, a
# duplicated entry, a malformed sum, or an entry for a look-alike name each
# stop the install, even when one of them holds the archive's real sum.
checksum_case() {
  local version=$1 body=$2
  local case_name="omacortex-${version}-${arch}-linux"
  local dir="$work/www/releases/download/v${version}"
  mkdir -p "$dir"
  printf 'not a release archive\n' > "$dir/$case_name.tar.gz"
  local sum
  sum=$(sha256sum "$dir/$case_name.tar.gz" | cut -d' ' -f1)
  body=${body//@SUM@/$sum}
  body=${body//@NAME@/$case_name}
  printf '%s' "$body" > "$dir/checksums.txt"
}
checksum_case 9.9.8 "@SUM@  ./omacortex-other-${arch}-linux.tar.gz
"
checksum_case 9.9.7 "@SUM@  ./@NAME@.tar.gz
@SUM@  ./@NAME@.tar.gz
"
checksum_case 9.9.6 "@SUM@00  ./@NAME@.tar.gz
"
checksum_case 9.9.5 "@SUM@  ./omacortex-9x9x5-${arch}-linux.tar.gz
"

openssl req -x509 -newkey rsa:2048 -keyout "$work/key.pem" -out "$work/cert.pem" \
  -days 1 -nodes -subj "/CN=127.0.0.1" -addext "subjectAltName=IP:127.0.0.1" \
  >/dev/null 2>&1
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
cat > "$work/server.py" <<'PY'
import os
import ssl
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

port, root, cert, key = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
os.chdir(root)

class Quiet(SimpleHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return

httpd = ThreadingHTTPServer(("127.0.0.1", port), Quiet)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(cert, key)
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
PY
python3 "$work/server.py" "$port" "$work/www" "$work/cert.pem" "$work/key.pem" &
server_pid=$!

base="https://127.0.0.1:${port}/releases/download/v${expected}"
ready=no
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if curl -fsS --proto '=https' --cacert "$work/cert.pem" "$base/checksums.txt" >/dev/null 2>&1; then
    ready=yes
    break
  fi
  sleep 0.1
done
if [ "$ready" != yes ]; then
  fail "fixture server did not start"
fi

home="$work/home"
mkdir -p "$home"
set +e
out=$(
  HOME="$home" \
    OMACORTEX_INSTALL_VERSION="$expected" \
    OMACORTEX_RELEASE_BASE="$base" \
    SSL_CERT_FILE="$work/cert.pem" \
    CURL_CA_BUNDLE="$work/cert.pem" \
    "$root/install.sh" --download 2>&1
)
code=$?
set -e
printf '%s\n' "$out"
if [ "$code" -eq 0 ]; then
  fail "strict install of a broken release succeeded"
fi
if ! printf '%s\n' "$out" | grep -q "checksum mismatch for $name.tar.gz"; then
  fail "broken release did not fail the checksum check (exit $code)"
fi

for version in 9.9.8 9.9.7 9.9.6 9.9.5; do
  case_name="omacortex-${version}-${arch}-linux"
  set +e
  out=$(
    HOME="$home" \
      OMACORTEX_INSTALL_VERSION="v$version" \
      OMACORTEX_RELEASE_BASE="https://127.0.0.1:${port}/releases/download/v${version}" \
      SSL_CERT_FILE="$work/cert.pem" \
      CURL_CA_BUNDLE="$work/cert.pem" \
      "$root/install.sh" --download 2>&1
  )
  code=$?
  set -e
  if [ "$code" -eq 0 ]; then
    fail "install of v$version with a bad checksums.txt succeeded"
  fi
  if ! printf '%s\n' "$out" | grep -qF "no single sha256 checksum for $case_name.tar.gz"; then
    fail "v$version did not fail on its checksum entry (exit $code): $out"
  fi
done

printf 'fresh-install-version-test: ok\n'
