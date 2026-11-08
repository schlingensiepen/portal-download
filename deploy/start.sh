#!/bin/bash
# Entry point of the container, running as PID 1.
#
# Two modes. Without PROXY: checks SITE and the volume, creates the certificate, starts
# the service, waits until it answers, then starts Caddy. Once a day it checks the
# certificate and, when a new one was created, starts Caddy anew; the service keeps
# running. With PROXY=external: no Caddy and no certificate; the service listens on port
# 8080 for a reverse proxy in front, for example in the same compose stack.
#
# Once everything answers, a block of up to three lines names what to open: the fingerprint of
# the certificate (with Caddy), the state of the setup (the setup secret while nobody
# administers people), and last the line with " is ready: " and the address, so that a
# reader of the log stops at that one line.
#
# A stop signal goes to every process. When one of them ends on its own, the others are
# stopped too and the container ends, so that the restart policy brings all back together.
set -u

DATA=/data
TLS="$DATA/tls"
SERVICE_URL=http://127.0.0.1:8080/
CERT_DAYS=120
RENEW_SECONDS=2592000
# How often the running container checks the certificate; shortened only by the smoke test.
CHECK_SECONDS=${CERT_CHECK_SECONDS:-86400}
PROXY=${PROXY:-}

say() { echo "portal-download container: $*" >&2; }

# Installed first: as PID 1 the shell ignores a stop signal that has no handler.
service=""
caddy=""
timer=""
stopping=""
stop() {
    stopping=1
    # Nothing started yet: end at once.
    [ -z "$service" ] && exit 0
    # Wakes a wait that began after the loop last looked at "stopping". With Caddy the
    # timer ends, and shutdown stops Caddy before the service; without Caddy the service
    # is told at once. Signals sent twice do no harm.
    if [ -n "$caddy" ]; then
        [ -n "$timer" ] && kill "$timer" 2>/dev/null
    else
        kill -TERM "$service" 2>/dev/null
    fi
}
trap stop TERM INT

# Caddy first, so that no request is passed to a service that is already gone; then
# the service. Returns the service's exit code.
shutdown() {
    [ -n "$timer" ] && kill "$timer" 2>/dev/null
    if [ -n "$caddy" ]; then
        kill -TERM "$caddy" 2>/dev/null
        wait "$caddy" 2>/dev/null
    fi
    kill -TERM "$service" 2>/dev/null
    wait "$service"
}

case "$PROXY" in
    "") ;;
    external) export PORTAL_DOWNLOAD_CONFIG=/etc/portal-download/config-external.toml ;;
    *)
        say "PROXY=$PROXY is not a mode. Leave it unset for Caddy in this container, or set PROXY=external when a reverse proxy in front reaches the service on port 8080."
        exit 3
        ;;
esac

# The host name or IPv4 address the browsers use. Caddy serves only this name.
check_site() {
    if [ -z "${SITE:-}" ]; then
        say "SITE is not set. Start the container with -e SITE=<host name or IP address the browsers use>, for example -e SITE=downloads.intra.example. Behind a reverse proxy of your own, set -e PROXY=external instead."
        exit 3
    fi
    local ipv4='^(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])(\.(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])){3}$'
    if [[ "$SITE" =~ $ipv4 ]]; then
        san="IP:$SITE"
    elif [[ "$SITE" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$ && ! "$SITE" =~ ^[0-9.]+$ ]]; then
        san="DNS:$SITE"
    else
        say "SITE=$SITE is neither a host name nor an IPv4 address. Give the name or address alone, without scheme, port or path."
        exit 3
    fi
    if ! [[ "$CHECK_SECONDS" =~ ^[1-9][0-9]{0,6}$ ]]; then
        say "CERT_CHECK_SECONDS=$CHECK_SECONDS is not a whole number of seconds up to 9999999. Leave it unset for the daily check."
        exit 3
    fi
}
san=""
[ -z "$PROXY" ] && check_site

# The volume must be writable for the image's user.
probe="$DATA/.write-probe.$$"
if ! { : > "$probe"; } 2>/dev/null; then
    say "$DATA is not writable for UID $(id -u). For a host directory run: chown -R $(id -u):$(id -g) <directory>. A named volume (-v portal-download:$DATA) needs nothing."
    exit 4
fi
rm -f "$probe"

# The certificate, kept in the volume. The file "generated" holds the fingerprint of the
# certificate this script created; only that one is replaced, when SITE changes or it
# runs out within 30 days, which with 120 days of validity is every 90 days. A
# certificate the operator put there has another fingerprint and is never replaced; the
# log only warns when it no longer fits. Checked at start and then once a day.
fingerprint() { openssl x509 -in "$TLS/cert.pem" -noout -fingerprint -sha256 2>/dev/null; }
# Returns 0 when the certificate is unchanged, 10 when a new one was created, 1 when
# none could be created or read. The fingerprint is logged at start and when it changes.
certificate() {
    local create=""
    if ! mkdir -p "$TLS"; then
        say "cannot create $TLS."
        return 1
    fi
    if [ ! -s "$TLS/cert.pem" ] || [ ! -s "$TLS/key.pem" ]; then
        create=1
    elif [ "$(cat "$TLS/generated" 2>/dev/null)" = "$(fingerprint)" ]; then
        if [ "$(cat "$TLS/site" 2>/dev/null)" != "$SITE" ] \
            || ! openssl x509 -in "$TLS/cert.pem" -noout -checkend "$RENEW_SECONDS" >/dev/null 2>&1; then
            create=1
        fi
    else
        if ! openssl x509 -in "$TLS/cert.pem" -noout -checkend "$RENEW_SECONDS" >/dev/null 2>&1; then
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
            return 1
        fi
        mv -f "$TLS/key.pem.new" "$TLS/key.pem"
        mv -f "$TLS/cert.pem.new" "$TLS/cert.pem"
        printf '%s\n' "$SITE" > "$TLS/site"
        fingerprint > "$TLS/generated"
        say "created a certificate for $SITE, valid for $CERT_DAYS days, in $TLS/cert.pem. It is renewed after 90 days; browsers then ask once more to accept it."
    fi
    local current
    if ! current=$(fingerprint); then
        say "$TLS/cert.pem is not a readable certificate."
        return 1
    fi
    # At start the ready block names the fingerprint.
    if [ "${1:-}" != start ] && [ -n "$create" ]; then
        say "certificate for $SITE, SHA-256 fingerprint ${current#*=}. Compare it with what the browser shows before accepting."
    fi
    [ -n "$create" ] && return 10
    return 0
}
if [ -z "$PROXY" ]; then
    certificate start
    [ $? = 1 ] && exit 4
fi

portal-download serve &
service=$!

# One value of the pretty-printed JSON of `portal-download status`, empty when missing.
count() { sed -n "s/^ *\"$1\": \([0-9]*\),\{0,1\}\$/\1/p" <<<"$2" | sed -n 1p; }
# One string value of the setup file the service wrote before it answered.
window() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" "$DATA/portal-download/bootstrap.toml" 2>/dev/null; }
plural() { if [ "$1" = 1 ]; then echo "1 $2"; else echo "$1 $3"; fi; }
# The ready block; $1 is the address, $2 what follows it on the ready line.
ready_block() {
    local status users admins jobs version setup
    if [ -z "$PROXY" ]; then
        local current
        current=$(fingerprint)
        say "certificate SHA-256 fingerprint ${current#*=}; compare it with what the browser shows before accepting."
    fi
    status=$(portal-download status 2>/dev/null)
    users=$(count users "$status")
    admins=$(count admins "$status")
    jobs=$(count jobs "$status")
    setup="set up: $(plural "$users" person people), $(plural "$jobs" job jobs)."
    if [ "$users" = 0 ]; then
        say "not set up yet. Setup secret: $(window secret) (valid until $(window expires_at)); enter it in the browser."
    elif [ "$admins" = 0 ]; then
        say "$setup Nobody administers people. Setup secret: $(window secret) (valid until $(window expires_at))."
    elif [ -n "$users" ]; then
        say "$setup"
    fi
    version=$(portal-download version | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
    say "portal-download $version is ready: $1$2"
}

# Caddy starts only when the service answers, so the first request already reaches it.
up=""
for _ in $(seq 1 30); do
    [ -n "$stopping" ] && { shutdown; exit $?; }
    if wget -q -O /dev/null "$SERVICE_URL" 2>/dev/null; then up=1; break; fi
    if ! kill -0 "$service" 2>/dev/null; then
        [ -n "$stopping" ] && { shutdown; exit $?; }
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

if [ "$PROXY" = external ]; then
    ready_block "http://$(hostname):8080/" " (PROXY=external, no Caddy here: the reverse proxy reaches it on port 8080; do not publish that port)"
    while :; do
        if [ -n "$stopping" ]; then
            shutdown
            exit $?
        fi
        wait "$service"
        code=$?
        [ -n "$stopping" ] && continue
        say "the service ended (exit code $code); stopping the container."
        [ "$code" -eq 0 ] && code=1
        exit "$code"
    done
fi

# Before it has read the Caddyfile, whose global option keeps it at WARN, Caddy writes a few
# lines at level info with no logger name; only those are left out of the log.
caddy_log() {
    local line
    while IFS= read -r line; do
        [[ $line == '{"level":"info",'* && $line != *'"logger":'* ]] && continue
        printf '%s\n' "$line" >&2
    done
}
start_caddy() {
    caddy run --config /etc/caddy/Caddyfile --adapter caddyfile 2> >(caddy_log) &
    caddy=$!
}
start_timer() {
    sleep "$CHECK_SECONDS" &
    timer=$!
}
start_caddy

# The ready block comes once Caddy completes a TLS handshake, so that its address works.
caddy_up=""
for _ in $(seq 1 30); do
    [ -n "$stopping" ] && break
    if ! kill -0 "$caddy" 2>/dev/null || ! kill -0 "$service" 2>/dev/null; then break; fi
    if timeout 5 openssl s_client -connect 127.0.0.1:443 -servername "$SITE" </dev/null >/dev/null 2>&1; then
        caddy_up=1
        break
    fi
    sleep 1 &
    wait $!
done
if [ -n "$caddy_up" ]; then
    ready_block "https://$SITE/" ""
elif [ -z "$stopping" ] && kill -0 "$caddy" 2>/dev/null; then
    say "Caddy does not answer on port 443 within 30 seconds; the lines above name the cause."
fi
start_timer

# Returns when a process ends, the daily check is due or a stop signal arrives.
while :; do
    if [ -n "$stopping" ]; then
        shutdown
        exit $?
    fi
    wait -n "$service" "$caddy" "$timer"
    code=$?
    [ -n "$stopping" ] && continue
    if ! kill -0 "$service" 2>/dev/null; then
        say "the service ended (exit code $code); stopping Caddy and the container."
    elif ! kill -0 "$caddy" 2>/dev/null; then
        say "Caddy ended (exit code $code); stopping the service and the container."
    elif ! kill -0 "$timer" 2>/dev/null; then
        timer=""
        if [ "$code" -ne 0 ]; then
            say "the timer of the daily certificate check failed (exit code $code); stopping the container."
            code=1
        else
            certificate
            if [ $? = 10 ]; then
                say "starting Caddy anew with the new certificate; the service keeps running."
                kill -TERM "$caddy" 2>/dev/null
                wait "$caddy" 2>/dev/null
                [ -n "$stopping" ] && continue
                start_caddy
            fi
            [ -n "$stopping" ] && continue
            start_timer
            continue
        fi
    else
        continue
    fi
    shutdown
    [ "$code" -eq 0 ] && code=1
    exit "$code"
done
