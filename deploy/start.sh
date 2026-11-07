#!/bin/bash
# Entry point of the container, running as PID 1.
#
# Checks SITE and the volume, creates the certificate once, starts the service, waits
# until it answers, then starts Caddy. A stop signal goes to both. When either of them
# ends on its own, the other is stopped too and the container ends, so that Docker's
# restart policy brings both back together.
set -u

DATA=/data
TLS="$DATA/tls"
SERVICE_URL=http://127.0.0.1:8080/
CERT_DAYS=3650

say() { echo "portal-download container: $*" >&2; }

# The host name or IP address the browsers use. Caddy serves only this name.
if [ -z "${SITE:-}" ]; then
    say "SITE is not set. Start the container with -e SITE=<host name or IP address the browsers use>, for example -e SITE=downloads.intra.example."
    exit 3
fi
if [[ ! "$SITE" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]]; then
    say "SITE=$SITE is not a host name or IPv4 address. Use letters, digits, dots and hyphens only, without scheme, port or path."
    exit 3
fi

# The volume must be writable for the image's user.
probe="$DATA/.write-probe.$$"
if ! { : > "$probe"; } 2>/dev/null; then
    say "$DATA is not writable for UID $(id -u). For a host directory run: chown -R $(id -u):$(id -g) <directory>. A named volume (-v portal-download:$DATA) needs nothing."
    exit 4
fi
rm -f "$probe"

# The certificate: created once and kept in the volume, so that a browser accepts it
# once. A new one only when SITE changes or the old one runs out within 30 days.
if ! mkdir -p "$TLS"; then
    say "cannot create $TLS."
    exit 4
fi
if [ ! -s "$TLS/cert.pem" ] || [ ! -s "$TLS/key.pem" ] \
    || [ "$(cat "$TLS/site" 2>/dev/null)" != "$SITE" ] \
    || ! openssl x509 -in "$TLS/cert.pem" -noout -checkend 2592000 >/dev/null 2>&1; then
    if [[ "$SITE" =~ ^[0-9.]+$ ]]; then san="IP:$SITE"; else san="DNS:$SITE"; fi
    if ! (umask 077 && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -days "$CERT_DAYS" -subj "/CN=$SITE" \
        -addext "subjectAltName=$san" -addext "extendedKeyUsage=serverAuth" \
        -keyout "$TLS/key.pem.new" -out "$TLS/cert.pem.new" 2>/dev/null); then
        say "cannot create a certificate for $SITE in $TLS."
        exit 4
    fi
    mv -f "$TLS/key.pem.new" "$TLS/key.pem"
    mv -f "$TLS/cert.pem.new" "$TLS/cert.pem"
    printf '%s\n' "$SITE" > "$TLS/site"
    say "created a certificate for $SITE, valid for $CERT_DAYS days, in $TLS/cert.pem. Browsers ask once to accept it."
fi
say "certificate for $SITE, SHA-256 fingerprint $(openssl x509 -in "$TLS/cert.pem" -noout -fingerprint -sha256 | cut -d= -f2). Compare it with what the browser shows before accepting."

service=""
caddy=""
stopping=""
stop() {
    stopping=1
    [ -n "$caddy" ] && kill -TERM "$caddy" 2>/dev/null
    [ -n "$service" ] && kill -TERM "$service" 2>/dev/null
}
trap stop TERM INT

# Waits for the remaining processes and ends the container with the service's code.
finish() {
    local code=0
    if [ -n "$service" ]; then wait "$service"; code=$?; fi
    if [ -n "$caddy" ]; then wait "$caddy"; fi
    exit "$code"
}

portal-download serve &
service=$!

# Caddy starts only when the service answers, so the first request already reaches it.
for _ in $(seq 1 30); do
    [ -n "$stopping" ] && finish
    wget -q -O /dev/null "$SERVICE_URL" 2>/dev/null && break
    if ! kill -0 "$service" 2>/dev/null; then
        wait "$service"
        code=$?
        say "the service ended during start (exit code $code)."
        exit "$code"
    fi
    sleep 1 &
    wait $!
done
if ! wget -q -O /dev/null "$SERVICE_URL" 2>/dev/null; then
    say "the service did not answer at $SERVICE_URL within 30 seconds."
    stop
    finish
fi

caddy run --config /etc/caddy/Caddyfile --adapter caddyfile &
caddy=$!
# A stop signal that came before Caddy's PID was known did not reach Caddy.
[ -n "$stopping" ] && { stop; finish; }

# Returns when a process ends or a stop signal arrives.
while :; do
    wait -n "$service" "$caddy"
    code=$?
    [ -n "$stopping" ] && finish
    if ! kill -0 "$service" 2>/dev/null; then
        say "the service ended (exit code $code); stopping Caddy and the container."
    elif ! kill -0 "$caddy" 2>/dev/null; then
        say "Caddy ended (exit code $code); stopping the service and the container."
    else
        continue
    fi
    stop
    wait "$service" 2>/dev/null
    wait "$caddy" 2>/dev/null
    [ "$code" -eq 0 ] && code=1
    exit "$code"
done
