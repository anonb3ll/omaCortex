#!/usr/bin/env bash
# Resolver cases for fresh-install, plus a broken release asset on the strict path.
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

if grep -q 'test "$latest" = "v${version}"' "$root/.github/workflows/ci.yml"; then
  fail "workflow still asserts latest == Cargo.toml on every event"
fi
if ! grep -q 'fresh-install-version.sh' "$root/.github/workflows/ci.yml"; then
  fail "workflow does not resolve the install version"
fi
if ! grep -q 'OMACORTEX_INSTALL_VERSION' "$root/.github/workflows/ci.yml"; then
  fail "workflow does not pass the resolved version to install.sh"
fi

# Strict path (main) selects the checkout version, then the asset fails checksum.
expected=$("$resolve" push 9.9.9 v9.9.9 yes refs/heads/main)
if [ "$expected" != "9.9.9" ]; then
  fail "strict path did not keep the checkout version"
fi

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) fail "no fixture arch for $(uname -m)" ;;
esac
name="omacortex-${expected}-${arch}-linux"
rel="$work/www/releases/download/v${expected}"
mkdir -p "$rel"
printf 'not a release archive\n' > "$rel/$name.tar.gz"
printf '%s  ./%s.tar.gz\n' \
  "0000000000000000000000000000000000000000000000000000000000000000" \
  "$name" > "$rel/checksums.txt"

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

printf 'fresh-install-version-test: ok\n'
