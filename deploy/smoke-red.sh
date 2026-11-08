#!/usr/bin/env bash
# Proves that the smoke test catches what it is there for: each of its nine steps is
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
    "1|Caddy is never started|sed -i 's|^    caddy run .*|    sleep 100000 \&|' /usr/local/bin/start.sh"
    "1|the ready block is never printed|sed -i 's|^    ready_block \"https://.*|    :|' /usr/local/bin/start.sh"
    "1|Caddy logs at level INFO|sed -i '/level WARN/d' /etc/caddy/Caddyfile"
    "1|another name ends the handshake or gets an empty answer|sed -i 's|redir https://.*|respond 200|' /etc/caddy/Caddyfile"
    "3|the ready block names another setup secret than the service|sed -i 's|Setup secret: \$(window secret)|Setup secret: pds_other|' /usr/local/bin/start.sh"
    "3|the service's log does not reach the container log|sed -i 's|^portal-download serve &|portal-download serve 2>/dev/null \&|' /usr/local/bin/start.sh"
    "4|the stop signal is not passed on|sed -i 's|^trap stop TERM INT|:|' /usr/local/bin/start.sh"
    "5|the state lies outside the volume|sed -i 's|^state_dir = .*|state_dir = \"/tmp/portal-download\"|' /etc/portal-download/config.toml"
    "5|the block says not set up although somebody is|sed -i 's|^    if \[ \"\$users\" = 0 \]; then|    if true; then|' /usr/local/bin/start.sh"
    "5|the certificate is created anew at every start|sed -i 's|^    certificate start\$|    rm -f \"\$TLS/cert.pem\"; certificate start|' /usr/local/bin/start.sh"
    "6|the backup does not reach the service|awk '/reverse_proxy/ { print \"\\trespond /backup 404\" } 1' /etc/caddy/Caddyfile > /tmp/Caddyfile && cp /tmp/Caddyfile /etc/caddy/Caddyfile"
    "7|the running container never checks the certificate|sed -i 's|^    sleep \"\$CHECK_SECONDS\" &|    sleep 100000 \&|' /usr/local/bin/start.sh"
    "7|Caddy keeps the old certificate after a renewal|sed -i 's|^            if \[ \$? = 10 \]; then|            if false; then|' /usr/local/bin/start.sh"
    "7|a certificate of the operator's is replaced like the image's own|sed -i 's|^    elif \[ \"\$(cat \"\$TLS/generated\" 2>/dev/null)\" = \"\$(fingerprint)\" \]; then|    elif true; then|' /usr/local/bin/start.sh"
    "7|the certificate is valid for ten years|sed -i 's|^CERT_DAYS=120|CERT_DAYS=3650|' /usr/local/bin/start.sh"
    "8|PROXY=external still starts Caddy|sed -i 's|^if \[ \"\$PROXY\" = external \]; then|if false; then|' /usr/local/bin/start.sh"
    "8|PROXY=external prints no ready block|sed -i 's|^    ready_block \"port 8080 .*|    :|' /usr/local/bin/start.sh"
    "8|PROXY=external listens only on loopback|sed -i 's|^listen = .*|listen = \"127.0.0.1:8080\"|' /etc/portal-download/config-external.toml"
    # Assumes the smoke test reaches the container from the bridge gateway, not from
    # loopback, as with Docker's userland proxy on the CI runners.
    "8|PROXY=external ignores the proxy's headers|sed -i 's|^trust_forwarded_headers = true|trust_forwarded_headers = false|' /etc/portal-download/config-external.toml"
    "9|the check for a writable volume is gone|sed -i 's|^if ! { : > \"\$probe\"; } 2>/dev/null; then|if false; then|' /usr/local/bin/start.sh"
    "9|the check for a missing SITE is gone|sed -i '/SITE is not set/{n;s/exit 3/:/}' /usr/local/bin/start.sh"
    "9|the check of the form of SITE is gone|sed -i '/is neither a host name/{n;s/exit 3/:/}' /usr/local/bin/start.sh"
    "9|the check of the interval is gone|sed -i 's|^    if ! \[\[ \"\$CHECK_SECONDS\" =~ .*|    if false; then|' /usr/local/bin/start.sh"
    "9|the check of PROXY is gone|sed -i '/is not a mode/{n;s/exit 3/:/}' /usr/local/bin/start.sh"
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
    if docker run --rm --entrypoint sh "$image" -c 'cat /usr/local/bin/start.sh /etc/portal-download/config.toml /etc/portal-download/config-external.toml /etc/caddy/Caddyfile' >"$work/a" \
        && docker run --rm --entrypoint sh "$tag" -c 'cat /usr/local/bin/start.sh /etc/portal-download/config.toml /etc/portal-download/config-external.toml /etc/caddy/Caddyfile' >"$work/b" \
        && cmp -s "$work/a" "$work/b"; then
        echo "::error::the mutation changed nothing: $what"
        failures=$((failures + 1))
        continue
    fi
    check "$expected" "$what" "$tag" "$revision"
done

[ "$failures" = 0 ] || { echo "::error::$failures safeguard(s) not proven"; exit 1; }
echo "every step of the smoke test was seen red with its safeguard removed"
