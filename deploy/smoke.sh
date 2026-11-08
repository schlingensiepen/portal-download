#!/usr/bin/env bash
# Smoke test of a built image, run before it is published.
#
#   deploy/smoke.sh IMAGE REVISION
#
# Exits 0 when the image passes all nine steps, and 1 with an ::error:: line naming
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
fingerprint() {
    openssl s_client -connect "127.0.0.1:$port" -servername localhost </dev/null 2>/dev/null \
        | openssl x509 -noout -fingerprint -sha256 2>/dev/null || true
}

docker volume create "$volume" >/dev/null

step="1 (start)"
start
wait_up
echo "step 1: the image starts and answers through Caddy"

step="2 (revision)"
reported=$(docker exec "$name" portal-download version | jq -r .revision)
[ "$reported" = "$revision" ] || fail "the binary reports revision '$reported', built from '$revision'"
echo "step 2: the binary reports revision $revision"

step="3 (first setup)"
secret=$(docker logs "$name" 2>&1 | sed -n 's/.*Setup secret: \([^ ]*\) .*/\1/p' | head -n 1)
[ -n "$secret" ] || fail "no setup secret in the log"
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
echo "step 5: a new container on the same volume keeps the person and the certificate"

step="6 (backup)"
answer=$(rpc agent.create '{"introduction":"Smoke test of the image","ability":"backup.read"}' -b "$work/cookies")
token=$(jq -r '.result.token // empty' <<<"$answer")
[ -n "$token" ] || fail "agent.create with the device cookie answered: $answer"
curl -ksSf --max-time 10 -H "Authorization: Bearer $token" "$base/backup" -o "$work/backup.jsonl" \
    || fail "GET /backup with the token failed"
head -n 1 "$work/backup.jsonl" | jq -e '.type == "backup" and .format == "portal-download-backup"' >/dev/null \
    || fail "the first line of the backup is not its head: $(head -c 200 "$work/backup.jsonl")"
echo "step 6: the backup token fetches the backup"

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
    if [ -n "$served" ] && [ "$served" = "$stored" ] && [ "$stored" != "$short" ] && [ "$stored" != "$before" ]; then
        renewed=1
        break
    fi
    sleep 1
done
[ -n "$renewed" ] || fail "a certificate that runs out in 10 days was not replaced and served within 30 seconds (served: ${served:-none})"
if ! docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -checkend $((119 * 86400)) >/dev/null \
    || docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -checkend $((121 * 86400)) >/dev/null; then
    fail "the new certificate is not valid for 120 days: $(docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -enddate)"
fi
[ "$(docker exec "$name" pidof portal-download)" = "$pid" ] \
    && [ "$(docker inspect -f '{{.State.StartedAt}}' "$name")" = "$started" ] \
    || fail "the service or the container was started anew for the renewal"
# A certificate the operator put there is never replaced, even when it runs out soon.
docker exec "$name" sh -c 'cd /data/tls && umask 077 \
    && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 10 -subj /CN=localhost \
        -addext subjectAltName=DNS:localhost -keyout key.pem.new -out cert.pem.new 2>/dev/null \
    && mv -f key.pem.new key.pem && mv -f cert.pem.new cert.pem \
    && openssl x509 -in cert.pem -noout -fingerprint -sha256' >"$work/own" \
    || fail "could not put a certificate of the operator's into the volume"
sleep 8
[ "$(docker exec "$name" openssl x509 -in /data/tls/cert.pem -noout -fingerprint -sha256)" = "$(cat "$work/own")" ] \
    || fail "a certificate of the operator's was replaced"
docker logs "$name" 2>&1 | grep -q 'warning: the certificate in /data/tls/cert.pem runs out within 30 days' \
    || fail "no warning about the operator's certificate that runs out soon"
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
page=$(curl -sS --max-time 5 -H 'X-Forwarded-Proto: https' -H 'X-Forwarded-Host: downloads.intra.example' "$direct/")
grep -q 'https://downloads.intra.example/' <<<"$page" || fail "the forwarded scheme and host do not reach the entry page: $(head -c 200 <<<"$page")"
t0=$(date +%s)
docker stop -t 10 "$name" >/dev/null
seconds=$(( $(date +%s) - t0 ))
code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
[ "$code" = 0 ] && [ "$seconds" -lt 10 ] || fail "docker stop ended the container with exit code $code after $seconds seconds"
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

echo "smoke test passed: $image"
