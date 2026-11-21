#!/usr/bin/env bash
# Smoke test of a built image, run before it is published.
#
#   deploy/smoke.sh IMAGE REVISION
#
# Exits 0 when the image passes all ten steps, and 1 with an ::error:: line naming
# the step otherwise. Needs docker, curl, jq and openssl on the machine that runs it.
set -euo pipefail

image=${1:?usage: smoke.sh IMAGE REVISION}
revision=${2:?usage: smoke.sh IMAGE REVISION}
name=pd-smoke
volume=pd-smoke-data
port=8443
base=https://localhost:$port
work=$(mktemp -d)

cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker volume rm -f "$volume" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

step=""
fail() {
    echo "::error::step $step: $*"
    docker logs "$name" 2>&1 | tail -n 40 || true
    exit 1
}
start() {
    docker run -d --name "$name" --health-interval 2s \
        -p "127.0.0.1:$port:443" -v "$volume:/data" -e SITE=localhost -e CERT_CHECK_SECONDS=2 "$image" >/dev/null
}
# Through Caddy, the way a browser reaches the service, and the image's own HEALTHCHECK.
wait_up() {
    local health=""
    for _ in $(seq 1 30); do
        health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$name" 2>/dev/null || true)
        if [ "$health" = healthy ] && curl -ksf -o /dev/null --max-time 5 "$base/"; then return 0; fi
        sleep 1
    done
    fail "no answer through Caddy at $base/ within 30 seconds (health: ${health:-none})"
}
rpc() {
    local method=$1 params=$2
    shift 2
    curl -ksS --max-time 10 -X POST "$base/api/" -H 'Content-Type: application/json' "$@" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}"
}
# The certificate presented for a name (SNI), or for none with -noservername.
fingerprint() {
    local sni=("-servername" "${1:-localhost}")
    [ "${1:-}" = none ] && sni=("-noservername")
    openssl s_client -connect "127.0.0.1:$port" "${sni[@]}" </dev/null 2>/dev/null \
        | openssl x509 -noout -fingerprint -sha256 2>/dev/null || true
}
# The ready block of $log: the last dashes before the ready line, with the stars and the
# heading above them when a setup is needed, down to the ready line.
block_of() {
    sed -n 's/^portal-download container: //p' <<<"$log" | awk '
        { line[NR] = $0 }
        / is ready: / { last = NR }
        END {
            if (!last) exit
            for (r = last; r > 0 && line[r] !~ /^-{64}$/; r--) {}
            start = (r > 2 && line[r - 2] ~ /^\*{64}$/) ? r - 2 : r
            for (i = start; i <= last; i++) print line[i]
        }'
}
# Even with the authority's key, a certificate for another host name or an address fails the
# name constraint. The test certificate must be issued first, or the check proves nothing.
foreign_names_fail() {
    local other
    for other in DNS:evil.example IP:192.0.2.1; do
        docker exec "$name" sh -c "cd /tmp && rm -f e.pem && openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /O=x -keyout e.key -out e.csr 2>/dev/null \
            && printf 'subjectAltName=$other\n' > e.ext \
            && openssl x509 -req -in e.csr -CA /data/tls/ca.pem -CAkey /data/tls/ca.key -set_serial 7 -days 1 -extfile e.ext -out e.pem 2>/dev/null" \
            || fail "could not issue a test certificate for $other with the authority"
        if docker exec "$name" openssl verify -CAfile /data/tls/ca.pem /tmp/e.pem >/dev/null 2>&1; then
            fail "a certificate for $other issued by the authority passes; the name constraint does not shut it out"
        fi
    done
}
# Reads the container's log into $log once it holds the ready line, which ends the block.
# Called directly, not in $(…), so that fail() reaches the output and ends the test.
ready_log() {
    for _ in $(seq 1 15); do
        log=$(docker logs "$name" 2>&1)
        if grep -q ' is ready: ' <<<"$log"; then return 0; fi
        sleep 1
    done
    fail "no line with ' is ready: ' in the log within 15 seconds"
}

docker volume create "$volume" >/dev/null

step="1 (start)"
start
wait_up
ready_log
block=$(block_of)
[[ "$(tail -n 1 <<<"$block")" =~ ^portal-download\ [0-9][^\ ]*\ is\ ready:\ https://localhost/$ ]] \
    || fail "the last line of the block is not 'portal-download <version> is ready: https://localhost/': $block"
if [ "$(sed -n 1p <<<"$block")" != "$(printf '%.0s*' {1..64})" ] || [ "$(sed -n 2p <<<"$block")" != "*  SETUP NEEDED: nobody is set up yet" ] \
    || [ "$(sed -n 3p <<<"$block")" != "$(printf '%.0s-' {1..64})" ] || [ "$(tail -n 2 <<<"$block" | head -n 1)" != "$(printf '%.0s=' {1..64})" ]; then
    fail "the block is not framed by stars, the heading SETUP NEEDED, dashes and equals signs: $block"
fi
grep -qx '   Open:                   https://localhost/' <<<"$block" || fail "the block does not name the address to open: $block"
# The authority the browsers trust once: downloaded from the service, it issued the served
# certificate for SITE, and the block names its fingerprint.
curl -ksSf --max-time 5 "$base/ca.crt" -o "$work/ca.der" || fail "GET /ca.crt failed"
openssl x509 -inform der -in "$work/ca.der" -out "$work/ca.pem" || fail "/ca.crt is no certificate"
authority=$(openssl x509 -in "$work/ca.pem" -noout -fingerprint -sha256)
grep -qx "   Certificate authority:  SHA-256 ${authority#*=}" <<<"$block" \
    || fail "the block does not name the fingerprint ${authority#*=} of the authority at /ca.crt: $block"
openssl x509 -in "$work/ca.pem" -noout -ext nameConstraints 2>/dev/null | grep -q 'DNS:localhost' \
    || fail "the authority is not limited to SITE by a name constraint: $(openssl x509 -in "$work/ca.pem" -noout -text | head -n 40)"
foreign_names_fail

curl -sSf --max-time 5 --cacert "$work/ca.pem" -o /dev/null "$base/" \
    || fail "a client that trusts the authority does not accept the served certificate for localhost"
[[ "$block" =~ Setup\ secret:\ +(pds_[^\ ]+)\ \ \(valid\ until\ [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z\; ]] \
    || fail "the block does not name the setup secret and its expiry before the setup: $block"
block_secret=${BASH_REMATCH[1]}
served=$(fingerprint)
# Caddy writes warnings and errors only.
! grep -q '"level":"info"' <<<"$log" || fail "Caddy writes lines at level info: $(grep '"level":"info"' <<<"$log" | head -n 3)"
# Another name, or none, gets the certificate of SITE and the service itself, which sees the
# name the browser used.
for sni in other.example none; do
    [ "$(fingerprint "$sni")" = "$served" ] || fail "a handshake with SNI $sni does not present the certificate of SITE"
done
for url in "https://other.example:$port" "https://127.0.0.1:$port"; do
    usage=$(curl -ksS --max-time 5 --resolve "other.example:$port:127.0.0.1" "$url/describe/usage") || usage=""
    grep -qF "$url/" <<<"$usage" || fail "a request to $url/ does not reach the service under that address: $(head -c 300 <<<"$usage")"
done
# The browser extension's preflight passes Caddy and is answered for its origin.
cors=$(curl -ksS --max-time 5 -o /dev/null -D - -X OPTIONS "$base/api/" -H 'Origin: moz-extension://0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f' \
    -H 'Access-Control-Request-Method: POST' -H 'Access-Control-Request-Headers: authorization,content-type')
grep -qi '^access-control-allow-origin: moz-extension://0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f' <<<"$cors" \
    || fail "the preflight of the browser extension is not answered for its origin: $cors"
if ! grep -qi '^access-control-allow-headers:.*authorization' <<<"$cors" || ! grep -qi '^access-control-allow-headers:.*x-backup-password' <<<"$cors"; then
    fail "the preflight does not allow the headers Authorization and X-Backup-Password: $cors"
fi
! grep -qi '^access-control-allow-credentials' <<<"$cors" || fail "the preflight allows credentials: $cors"
echo "step 1: the image starts, answers through Caddy with a certificate of its own authority, shows the framed ready block, serves other names and the extension's preflight"

step="2 (revision)"
reported=$(docker exec "$name" portal-download version | jq -r .revision)
[ "$reported" = "$revision" ] || fail "the binary reports revision '$reported', built from '$revision'"
echo "step 2: the binary reports revision $revision"

step="3 (first setup)"
# The whole log first: under pipefail a reader that stops early would fail the pipe.
log=$(docker logs "$name" 2>&1)
# The service's own line, not the one of the ready block.
secret=$(sed -n 's/^portal-download: not set up yet\. Setup secret: \([^ ]*\) .*/\1/p' <<<"$log" | sed -n 1p)
[ -n "$secret" ] || fail "no setup secret in the log of the service"
[ "$block_secret" = "$secret" ] || fail "the ready block names the setup secret $block_secret, the service $secret"
answer=$(rpc setup.createAdmin '{"name":"Smoke"}' -H "X-Setup-Secret: $secret")
invitation=$(jq -r '.result.invitation_token // empty' <<<"$answer")
[ -n "$invitation" ] || fail "setup.createAdmin with the logged secret answered: $answer"
answer=$(rpc invitation.redeem "{\"token\":\"$invitation\",\"label\":\"smoke\"}" -c "$work/cookies")
jq -e '.result.user_id' <<<"$answer" >/dev/null || fail "invitation.redeem answered: $answer"
before=$(fingerprint)
[ -n "$before" ] || fail "no certificate presented on port 443"
echo "step 3: the logged setup secret creates the first person"

step="4 (stop)"
t0=$(date +%s)
docker stop -t 10 "$name" >/dev/null
seconds=$(( $(date +%s) - t0 ))
code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
[ "$code" = 0 ] || fail "docker stop ended the container with exit code $code"
[ "$seconds" -lt 10 ] || fail "docker stop took $seconds seconds; the stop signal does not reach both processes"
echo "step 4: docker stop ends the container in ${seconds}s with exit code 0"

step="5 (new container, same volume)"
docker rm "$name" >/dev/null
start
wait_up
users=$(docker exec "$name" portal-download status | jq -r .store.users)
[ "$users" = 1 ] || fail "the new container sees $users people, expected 1 from the volume"
after=$(fingerprint)
[ "$after" = "$before" ] || fail "the certificate changed: $before became $after"
curl -ksSf --max-time 5 "$base/ca.crt" | cmp -s - "$work/ca.der" || fail "the certificate authority changed with the container"
ready_log
block=$(block_of)
if [ "$(sed -n 1p <<<"$block")" != "$(printf '%.0s-' {1..64})" ] || [ "$(sed -n 2p <<<"$block")" != "   READY: set up, 1 person, 0 jobs" ]; then
    fail "the block does not begin with dashes and '   READY: set up, 1 person, 0 jobs': $block"
fi
! grep -q 'Setup secret' <<<"$block" || fail "the block names a setup secret although somebody manages people: $block"
echo "step 5: a new container on the same volume keeps the person, the certificate and the authority"

step="6 (backup)"
answer=$(rpc agent.create '{"introduction":"Smoke test of the image","role":"backup.read"}' -b "$work/cookies")
token=$(jq -r '.result.token // empty' <<<"$answer")
[ -n "$token" ] || fail "agent.create with the device cookie answered: $answer"
curl -ksSf --max-time 10 -H "Authorization: Bearer $token" "$base/backup" -o "$work/backup.zip" \
    || fail "GET /backup with the token failed"
if [ "$(head -c 2 "$work/backup.zip")" != PK ] || ! grep -qa 'portal-download.jsonl' "$work/backup.zip"; then
    fail "the backup is no ZIP file with portal-download.jsonl: $(head -c 100 "$work/backup.zip" | od -c | head -n 3)"
fi
curl -ksSf --max-time 10 -H "Authorization: Bearer $token" -H 'X-Backup-Password: smoke test password' "$base/backup" -o "$work/locked.zip" \
    || fail "GET /backup with a password failed"
! grep -qa '"Smoke"' "$work/locked.zip" || fail "the backup with a password is not encrypted"
echo "step 6: the backup token fetches the backup as a ZIP file, encrypted with a password"

step="7 (renewal while running)"
# The volume gets a certificate of this image that runs out in 10 days; the next check
# must replace it with one valid for 120 days and start Caddy anew, the service unchanged.
pid=$(docker exec "$name" pidof portal-download)
started=$(docker inspect -f '{{.State.StartedAt}}' "$name")
docker exec "$name" sh -c 'cd /data/tls && umask 077 \
    && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 10 -subj /CN=localhost \
        -addext subjectAltName=DNS:localhost -keyout key.pem.new -out cert.pem.new 2>/dev/null \
    && openssl x509 -in cert.pem.new -noout -fingerprint -sha256 > generated \
    && mv -f key.pem.new key.pem && mv -f cert.pem.new cert.pem && cat generated' >"$work/short" \
    || fail "could not put a certificate that runs out soon into the volume"
short=$(cat "$work/short")
renewed=""
for _ in $(seq 1 30); do
    stored=$(docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -fingerprint -sha256 2>/dev/null || true)
    served=$(fingerprint)
    if [ -n "$served" ] && [ "$served" = "$stored" ] && [ "$stored" != "$short" ] && [ "$stored" != "$before" ] \
        && curl -sSf --max-time 5 --cacert "$work/ca.pem" -o /dev/null "$base/"; then
        renewed=1
        break
    fi
    sleep 1
done
[ -n "$renewed" ] || fail "a certificate that runs out in 10 days was not replaced by one of the same authority and served within 30 seconds (served: ${served:-none})"
curl -ksSf --max-time 5 "$base/ca.crt" | cmp -s - "$work/ca.der" || fail "the renewal replaced the certificate authority"
if ! docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -checkend $((119 * 86400)) >/dev/null \
    || docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -checkend $((121 * 86400)) >/dev/null; then
    fail "the new certificate is not valid for 120 days: $(docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -enddate)"
fi
if [ "$(docker exec "$name" pidof portal-download)" != "$pid" ] \
    || [ "$(docker inspect -f '{{.State.StartedAt}}' "$name")" != "$started" ]; then
    fail "the service or the container was started anew for the renewal"
fi
# A certificate the operator put there is never replaced, even when it runs out soon.
docker exec "$name" sh -c 'cd /data/tls && umask 077 \
    && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 10 -subj /CN=localhost \
        -addext subjectAltName=DNS:localhost -keyout key.pem.new -out cert.pem.new 2>/dev/null \
    && mv -f key.pem.new key.pem && mv -f cert.pem.new cert.pem \
    && openssl x509 -in cert.pem -noout -fingerprint -sha256' >"$work/own" \
    || fail "could not put a certificate of the operator's into the volume"
sleep 8 # four checks at CERT_CHECK_SECONDS=2, see start()
[ "$(docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -fingerprint -sha256)" = "$(cat "$work/own")" ] \
    || fail "a certificate of the operator's was replaced"
log=$(docker logs "$name" 2>&1)
grep -q 'warning: the certificate in /data/tls/cert.pem runs out within 30 days' <<<"$log" \
    || fail "no warning about the operator's certificate that runs out soon"
[ "$(curl -ksS --max-time 5 -o /dev/null -w '%{http_code}' "$base/ca.crt")" = 404 ] \
    || fail "the image's authority is still offered although it did not issue the operator's certificate"
echo "step 7: the running container replaces its own certificate when it runs out soon, keeps one of the operator's, and the service keeps running"
docker rm -f "$name" >/dev/null

step="8 (PROXY=external)"
# Without Caddy: port 8080 answers directly, and the headers of the proxy in front count,
# although it reaches the service from another address than loopback.
direct=http://127.0.0.1:18080
docker run -d --name "$name" --health-interval 2s -p "127.0.0.1:18080:8080" -v "$volume:/data" -e PROXY=external "$image" >/dev/null
health=""
for _ in $(seq 1 30); do
    health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$name" 2>/dev/null || true)
    if [ "$health" = healthy ] && curl -sf -o /dev/null --max-time 5 "$direct/healthz"; then break; fi
    sleep 1
done
curl -sf -o /dev/null --max-time 5 "$direct/healthz" || fail "no answer at $direct/healthz within 30 seconds (health: ${health:-none})"
[ "$health" = healthy ] || fail "the HEALTHCHECK does not report healthy (health: ${health:-none})"
! docker exec "$name" pidof caddy >/dev/null || fail "Caddy runs although PROXY=external"
users=$(docker exec "$name" portal-download status | jq -r .store.users)
[ "$users" = 1 ] || fail "the container sees $users people on the volume, expected 1"
ready_log
ready=$(grep ' is ready: ' <<<"$log")
[[ "$ready" == *" is ready: port 8080 of this container (PROXY=external)" ]] || fail "the ready line does not name port 8080 of this container: $ready"
grep -q 'Proxy target: *http://portal-download:8080/' <<<"$log" || fail "the block does not name how the proxy reaches the service: $(block_of)"
page=$(curl -sS --max-time 5 -H 'X-Forwarded-Proto: https' -H 'X-Forwarded-Host: downloads.intra.example' "$direct/")
grep -q 'https://downloads.intra.example/' <<<"$page" || fail "the forwarded scheme and host do not reach the entry page: $(head -c 200 <<<"$page")"
t0=$(date +%s)
docker stop -t 10 "$name" >/dev/null
seconds=$(( $(date +%s) - t0 ))
code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
if [ "$code" != 0 ] || [ "$seconds" -ge 10 ]; then
    fail "docker stop ended the container with exit code $code after $seconds seconds"
fi
echo "step 8: PROXY=external serves port 8080 without Caddy and follows the proxy's headers"
docker rm -f "$name" >/dev/null

step="9 (wrong start)"
mkdir "$work/not-writable"
chmod 755 "$work/not-writable"
set +e
out=$(timeout 60 docker run --rm --name "$name" -v "$work/not-writable:/data" -e SITE=localhost "$image" 2>&1)
code=$?
set -e
[ "$code" = 4 ] || fail "a volume the image's user cannot write ended with exit code $code, expected 4: $out"
grep -q 'chown -R 10001:10001' <<<"$out" || fail "the message does not name the chown that is missing: $out"
set +e
out=$(timeout 60 docker run --rm --name "$name" -v "$volume:/data" "$image" 2>&1)
code=$?
set -e
if [ "$code" != 3 ] || ! grep -q 'SITE is not set' <<<"$out"; then
    fail "a start without SITE ended with exit code $code, expected 3 and a message: $out"
fi
set +e
out=$(timeout 60 docker run --rm --name "$name" -v "$volume:/data" -e SITE=https://downloads.intra.example/ "$image" 2>&1)
code=$?
set -e
[ "$code" = 3 ] || fail "SITE=https://downloads.intra.example/ ended with exit code $code, expected 3: $out"
set +e
out=$(timeout 60 docker run --rm --name "$name" -v "$volume:/data" -e SITE=localhost -e CERT_CHECK_SECONDS=soon "$image" 2>&1)
code=$?
set -e
[ "$code" = 3 ] || fail "CERT_CHECK_SECONDS=soon ended with exit code $code, expected 3: $out"
set +e
out=$(timeout 60 docker run --rm --name "$name" -v "$volume:/data" -e PROXY=internal "$image" 2>&1)
code=$?
set -e
[ "$code" = 3 ] || fail "PROXY=internal ended with exit code $code, expected 3: $out"
echo "step 9: an unwritable volume ends with exit code 4 and names the chown; a missing SITE, a wrong interval or an unknown PROXY with 3"

step="10 (other forms of SITE)"
# A long host name and an IPv4 address: the authority and the certificate are made for them.
for site in downloads.intra.a-rather-long-name-of-a-department.example 127.0.0.1; do
    docker volume rm -f "$volume" >/dev/null
    docker run -d --name "$name" -p "127.0.0.1:$port:443" -v "$volume:/data" -e "SITE=$site" "$image" >/dev/null
    ok=""
    for _ in $(seq 1 30); do
        if curl -ksf -o "$work/site-ca.der" --max-time 5 --resolve "$site:$port:127.0.0.1" "https://$site:$port/ca.crt" 2>/dev/null; then ok=1; break; fi
        sleep 1
    done
    [ -n "$ok" ] || fail "with SITE=$site the service does not offer its authority within 30 seconds"
    openssl x509 -inform der -in "$work/site-ca.der" -out "$work/site-ca.pem"
    curl -sSf --max-time 5 --cacert "$work/site-ca.pem" --resolve "$site:$port:127.0.0.1" -o /dev/null "https://$site:$port/" \
        || fail "with SITE=$site a client that trusts the authority does not accept the certificate"
    foreign_names_fail
    docker rm -f "$name" >/dev/null
done
echo "step 10: a long host name and an IPv4 address as SITE get an authority and a certificate a client accepts"

echo "smoke test passed: $image"
