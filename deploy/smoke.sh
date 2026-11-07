#!/usr/bin/env bash
# Smoke test of a built image, run before it is published.
#
#   deploy/smoke.sh IMAGE REVISION
#
# Exits 0 when the image passes all seven steps, and 1 with an ::error:: line naming
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
        -p "127.0.0.1:$port:443" -v "$volume:/data" -e SITE=localhost "$image" >/dev/null
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
docker rm -f "$name" >/dev/null

step="7 (volume not writable)"
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
[ "$code" = 3 ] || fail "a start without SITE ended with exit code $code, expected 3: $out"
echo "step 7: an unwritable volume ends with exit code 4 and names the chown; a missing SITE with 3"

echo "smoke test passed: $image"
