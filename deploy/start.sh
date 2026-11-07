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

# Installed first: as PID 1 the shell ignores a stop signal that has no handler.
service=""
caddy=""
stopping=""
stop() {
    stopping=1
    # Nothing started yet: end at once.
    [ -z "$service" ] && exit 0
}
trap stop TERM INT

# Caddy first, so that no request is passed to a service that is already gone; then
# the service. Returns the service's exit code.
shutdown() {
    if [ -n "$caddy" ]; then
        kill -TERM "$caddy" 2>/dev/null
        wait "$caddy" 2>/dev/null
    fi
    kill -TERM "$service" 2>/dev/null
    wait "$service"
}

# The host name or IPv4 address the browsers use. Caddy serves only this name.
if [ -z "${SITE:-}" ]; then
    say "SITE is not set. Start the container with -e SITE=<host name or IP address the browsers use>, for example -e SITE=downloads.intra.example."
    exit 3
fi
ipv4='^(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}$'
if [[ "$SITE" =~ $ipv4 ]]; then
    san="IP:$SITE"
elif [[ "$SITE" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$ && ! "$SITE" =~ ^[0-9.]+$ ]]; then
    san="DNS:$SITE"
else
    say "SITE=$SITE is neither a host name nor an IPv4 address. Give the name or address alone, without scheme, port or path."
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
# once. A certificate this script created carries the marker file "generated" and is
# replaced when SITE changes or it runs out within 30 days. A certificate the operator
# put there is never replaced; the log only warns when it no longer fits.
if ! mkdir -p "$TLS"; then
    say "cannot create $TLS."
    exit 4
fi
create=""
if [ ! -s "$TLS/cert.pem" ] || [ ! -s "$TLS/key.pem" ]; then
    create=1
elif [ -e "$TLS/generated" ]; then
    if [ "$(cat "$TLS/site" 2>/dev/null)" != "$SITE" ] \
        || ! openssl x509 -in "$TLS/cert.pem" -noout -checkend 2592000 >/dev/null 2>&1; then
        create=1
    fi
else
    if ! openssl x509 -in "$TLS/cert.pem" -noout -checkend 2592000 >/dev/null 2>&1; then
        say "warning: the certificate in $TLS/cert.pem runs out within 30 days or has run out. Replace cert.pem and key.pem, or delete both to have a new one created."
    fi
    if ! openssl x509 -in "$TLS/cert.pem" -noout -checkhost "$SITE" >/dev/null 2>&1 \
        && ! openssl x509 -in "$TLS/cert.pem" -noout -checkip "$SITE" >/dev/null 2>&1; then
        say "warning: the certificate in $TLS/cert.pem is not for $SITE; browsers will reject it."
    fi
fi
if [ -n "$create" ]; then
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
    : > "$TLS/generated"
    say "created a certificate for $SITE, valid for $CERT_DAYS days, in $TLS/cert.pem. Browsers ask once to accept it."
fi
if ! fingerprint=$(openssl x509 -in "$TLS/cert.pem" -noout -fingerprint -sha256 2>/dev/null); then
    say "$TLS/cert.pem is not a readable certificate."
    exit 4
fi
say "certificate for $SITE, SHA-256 fingerprint ${fingerprint#*=}. Compare it with what the browser shows before accepting."

portal-download serve &
service=$!

# Caddy starts only when the service answers, so the first request already reaches it.
up=""
for _ in $(seq 1 30); do
    [ -n "$stopping" ] && { shutdown; exit $?; }
    if wget -q -O /dev/null "$SERVICE_URL" 2>/dev/null; then up=1; break; fi
    if ! kill -0 "$service" 2>/dev/null; then
        wait "$service"
        code=$?
        say "the service ended during start (exit code $code)."
        [ "$code" -eq 0 ] && code=1
        exit "$code"
    fi
    sleep 1 &
    wait $!
done
if [ -z "$up" ]; then
    say "the service did not answer at $SERVICE_URL within 30 seconds."
    shutdown
    exit 1
fi

caddy run --config /etc/caddy/Caddyfile --adapter caddyfile &
caddy=$!

# Returns when a process ends or a stop signal arrives.
while :; do
    wait -n "$service" "$caddy"
    code=$?
    if [ -n "$stopping" ]; then
        shutdown
        exit $?
    fi
    if ! kill -0 "$service" 2>/dev/null; then
        say "the service ended (exit code $code); stopping Caddy and the container."
    elif ! kill -0 "$caddy" 2>/dev/null; then
        say "Caddy ended (exit code $code); stopping the service and the container."
    else
        continue
    fi
    shutdown
    [ "$code" -eq 0 ] && code=1
    exit "$code"
done
