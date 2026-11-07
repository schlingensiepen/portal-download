#!/usr/bin/env bash
# Proves that the smoke test catches what it is there for: each of its seven steps is
# run against an image with exactly that safeguard removed, and must fail at that step.
#
#   deploy/smoke-red.sh IMAGE REVISION
#
# IMAGE is an image that passes deploy/smoke.sh. Each mutation is a thin layer on top of
# it, so no Rust is compiled again.
set -euo pipefail

image=${1:?usage: smoke-red.sh IMAGE REVISION}
revision=${2:?usage: smoke-red.sh IMAGE REVISION}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# step | what is removed | command run in the derived image (as root)
mutations=(
    "1|Caddy is never started|sed -i 's|^caddy run .*|sleep 100000 \&|' /usr/local/bin/start.sh"
    "3|the service's log does not reach the container log|sed -i 's|^portal-download serve &|portal-download serve 2>/dev/null \&|' /usr/local/bin/start.sh"
    "4|the stop signal is not passed on|sed -i 's|^trap stop TERM INT|:|' /usr/local/bin/start.sh"
    "5|the state lies outside the volume|sed -i 's|^state_dir = .*|state_dir = \"/tmp/portal-download\"|' /etc/portal-download/config.toml"
    "5|the certificate is created anew at every start|sed -i 's|^if \[ ! -s \"\$TLS/cert.pem\" \]|if true \|\| [ ! -s \"\$TLS/cert.pem\" ]|' /usr/local/bin/start.sh"
    "6|the backup does not reach the service|awk '/reverse_proxy/ { print \"\\trespond /backup 404\" } 1' /etc/caddy/Caddyfile > /tmp/Caddyfile && cp /tmp/Caddyfile /etc/caddy/Caddyfile"
    "7|the check for a writable volume is gone|sed -i 's|^if ! { : > \"\$probe\"; } 2>/dev/null; then|if false; then|' /usr/local/bin/start.sh"
    "7|the check for SITE is gone|sed -i 's|^    exit 3\$|    :|' /usr/local/bin/start.sh"
)

failures=0
check() { # expected-step description image revision
    local expected=$1 what=$2 img=$3 rev=$4 out
    if out=$("$here/smoke.sh" "$img" "$rev" 2>&1); then
        echo "::error::smoke test passed although $what"
        failures=$((failures + 1))
    elif grep -q "^::error::step $expected " <<<"$out"; then
        echo "red at step $expected as expected: $what"
    else
        echo "::error::$what: expected a failure at step $expected, got: $(grep '^::error::' <<<"$out" | head -n 1)"
        failures=$((failures + 1))
    fi
}

check 2 "the expected revision differs from the built one" "$image" "0000000000000000000000000000000000000000"

n=0
for m in "${mutations[@]}"; do
    IFS='|' read -r expected what command <<<"$m"
    n=$((n + 1))
    tag="portal-download:red-$n"
    printf 'FROM %s\nUSER root\nRUN %s\nUSER 10001:10001\n' "$image" "$command" > "$work/Dockerfile.$n"
    if ! docker build --load -q -t "$tag" -f "$work/Dockerfile.$n" "$work" >/dev/null; then
        echo "::error::the mutation could not be built: $what"
        failures=$((failures + 1))
        continue
    fi
    # The mutation must really change the file; a sed that matches nothing proves nothing.
    if docker run --rm --entrypoint sh "$image" -c 'cat /usr/local/bin/start.sh /etc/portal-download/config.toml /etc/caddy/Caddyfile' >"$work/a" \
        && docker run --rm --entrypoint sh "$tag" -c 'cat /usr/local/bin/start.sh /etc/portal-download/config.toml /etc/caddy/Caddyfile' >"$work/b" \
        && cmp -s "$work/a" "$work/b"; then
        echo "::error::the mutation changed nothing: $what"
        failures=$((failures + 1))
        continue
    fi
    check "$expected" "$what" "$tag" "$revision"
done

[ "$failures" = 0 ] || { echo "::error::$failures safeguard(s) not proven"; exit 1; }
echo "every step of the smoke test was seen red with its safeguard removed"
