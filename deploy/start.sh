#!/bin/bash
# Entry point of the container, running as PID 1.
#
# Two modes. Without PROXY: checks SITE and the volume, creates the certificate, starts
# the service, waits until it answers, then starts Caddy. Once a day it checks the
# certificate and, when a new one was created, starts Caddy anew; the service keeps
# running. With PROXY=external: no Caddy and no certificate; the service listens on port
# 8080 for a reverse proxy in front, for example in the same compose stack.
#
# Once everything answers, a framed block names what to open: the address, with Caddy the
# fingerprint of the certificate authority to trust once, the setup secret while a setup is
# needed, and last the line with " is ready: " and the address, so that a reader of the log
# stops at that one line.
#
# A stop signal goes to every process. When one of them ends on its own, the others are
# stopped too and the container ends, so that the restart policy brings all back together.
set -u

DATA=/data
TLS="$DATA/tls"
SERVICE_URL=http://127.0.0.1:8080/
CERT_DAYS=120
CA_DAYS=3650
RENEW_SECONDS=2592000
# A certificate authority that runs out within a year is replaced.
CA_RENEW_SECONDS=31536000
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

# The certificates, kept in the volume. The image creates its own certificate authority
# for SITE (ca.pem, ca.key; 10 years, limited by a name constraint to SITE) and with it the
# certificate the browsers see (cert.pem: that certificate and the authority's; key.pem). A
# browser that trusts the authority once accepts every renewal. The file "generated" holds
# the fingerprint of the certificate this script created; only that one is replaced: when
# SITE changes, when it runs out within 30 days (with 120 days of validity every 90 days)
# or when the authority is new. A certificate the operator put there has another
# fingerprint and is never replaced; the log only warns when it no longer fits, and the
# image's authority is then not offered. Checked at start and then once a day.
fingerprint() { openssl x509 -in "${1:-$TLS/cert.pem}" -noout -fingerprint -sha256 2>/dev/null; }
# Whether key.pem belongs to cert.pem; a stop between moving the two can leave them apart.
pair_fits() {
    [ "$(openssl x509 -in "$TLS/cert.pem" -noout -pubkey 2>/dev/null)" = "$(openssl pkey -in "$TLS/key.pem" -pubout 2>/dev/null)" ]
}
# A new authority for SITE; returns 1 when none could be created.
new_authority() {
    # The constraint names SITE and shuts the other kind of name: for a host name only the
    # address 0.0.0.0 is permitted, for an address only the reserved name "invalid".
    local nc
    if [[ "$san" == IP:* ]]; then nc="permitted;IP:$SITE/255.255.255.255,permitted;DNS:invalid"; else nc="permitted;DNS:$SITE,permitted;IP:0.0.0.0/255.255.255.255"; fi
    if ! (umask 077 && openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -days "$CA_DAYS" -subj "/O=portal-download/CN=portal-download authority" \
        -addext "basicConstraints=critical,CA:TRUE,pathlen:0" -addext "keyUsage=critical,keyCertSign,cRLSign" \
        -addext "nameConstraints=critical,$nc" \
        -keyout "$TLS/ca.key.new" -out "$TLS/ca.pem.new" 2>/dev/null); then
        say "cannot create a certificate authority for $SITE in $TLS."
        return 1
    fi
    mv -f "$TLS/ca.key.new" "$TLS/ca.key"
    mv -f "$TLS/ca.pem.new" "$TLS/ca.pem"
    printf '%s\n' "$SITE" > "$TLS/ca-site"
    say "created a certificate authority for $SITE, valid for 10 years. Browsers trust it once: download https://$SITE/ca.crt and import it as a trusted root."
}
# A new certificate for SITE, issued by the authority; returns 1 when none could be created.
new_certificate() {
    if ! (umask 077 \
        && printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=%s\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid\n' "$san" > "$TLS/leaf.ext" \
        && openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/O=portal-download" \
            -keyout "$TLS/key.pem.new" -out "$TLS/leaf.csr" 2>/dev/null \
        && openssl x509 -req -in "$TLS/leaf.csr" -CA "$TLS/ca.pem" -CAkey "$TLS/ca.key" \
            -set_serial "0x$(openssl rand -hex 16)" -days "$CERT_DAYS" -extfile "$TLS/leaf.ext" \
            -out "$TLS/leaf.pem" 2>/dev/null \
        && cat "$TLS/leaf.pem" "$TLS/ca.pem" > "$TLS/cert.pem.new"); then
        rm -f "$TLS/leaf.ext" "$TLS/leaf.csr" "$TLS/leaf.pem" "$TLS/key.pem.new" "$TLS/cert.pem.new"
        say "cannot create a certificate for $SITE in $TLS."
        return 1
    fi
    rm -f "$TLS/leaf.ext" "$TLS/leaf.csr" "$TLS/leaf.pem"
    mv -f "$TLS/key.pem.new" "$TLS/key.pem"
    mv -f "$TLS/cert.pem.new" "$TLS/cert.pem"
    printf '%s\n' "$SITE" > "$TLS/site"
    fingerprint > "$TLS/generated"
    say "created a certificate for $SITE, valid for $CERT_DAYS days, in $TLS/cert.pem. It is renewed after 90 days; a browser that trusts the authority accepts it without asking."
}
# Returns 0 when the certificate is unchanged, 10 when a new one was created, 1 when
# none could be created or read.
certificate() {
    local create=""
    if ! mkdir -p "$TLS/public"; then
        say "cannot create $TLS."
        return 1
    fi
    if [ ! -s "$TLS/cert.pem" ] || [ ! -s "$TLS/key.pem" ] || [ "$(cat "$TLS/generated" 2>/dev/null)" = "$(fingerprint)" ]; then
        # Ours, or none yet: the authority first, then the certificate it issues.
        if [ ! -s "$TLS/ca.pem" ] || [ ! -s "$TLS/ca.key" ] || [ "$(cat "$TLS/ca-site" 2>/dev/null)" != "$SITE" ] \
            || ! openssl x509 -in "$TLS/ca.pem" -noout -checkend "$CA_RENEW_SECONDS" >/dev/null 2>&1; then
            new_authority || return 1
            create=1
        fi
        if [ -n "$create" ] || [ ! -s "$TLS/cert.pem" ] || [ "$(cat "$TLS/site" 2>/dev/null)" != "$SITE" ] \
            || ! openssl x509 -in "$TLS/cert.pem" -noout -checkend "$RENEW_SECONDS" >/dev/null 2>&1 \
            || ! openssl verify -CAfile "$TLS/ca.pem" "$TLS/cert.pem" >/dev/null 2>&1 \
            || ! pair_fits; then
            new_certificate || return 1
            create=1
        fi
        # Offered at /ca.crt; readable by everybody, unlike the key beside it.
        if ! cmp -s "$TLS/ca.pem" "$TLS/public/ca.crt"; then
            cp "$TLS/ca.pem" "$TLS/public/ca.crt.new" && chmod 644 "$TLS/public/ca.crt.new" && mv -f "$TLS/public/ca.crt.new" "$TLS/public/ca.crt"
        fi
    else
        # The operator's certificate: the image's authority did not issue it.
        rm -f "$TLS/public/ca.crt"
        if ! openssl x509 -in "$TLS/cert.pem" -noout -checkend "$RENEW_SECONDS" >/dev/null 2>&1; then
            say "warning: the certificate in $TLS/cert.pem runs out within 30 days or has run out. Replace cert.pem and key.pem, or delete both to have a new one created."
        fi
        if ! openssl x509 -in "$TLS/cert.pem" -noout -checkhost "$SITE" >/dev/null 2>&1 \
            && ! openssl x509 -in "$TLS/cert.pem" -noout -checkip "$SITE" >/dev/null 2>&1; then
            say "warning: the certificate in $TLS/cert.pem is not for $SITE; browsers will reject it."
        fi
    fi
    local current
    if ! current=$(fingerprint); then
        say "$TLS/cert.pem is not a readable certificate."
        return 1
    fi
    # At start the ready block names the fingerprints.
    if [ "${1:-}" != start ] && [ -n "$create" ]; then
        say "new certificate for $SITE, SHA-256 fingerprint ${current#*=}."
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
# The state directory from the configuration, as `portal-download check` reports it.
state_dir() { portal-download check 2>/dev/null | sed -n '/"state_dir": {/,/}/s/^ *"value": "\(.*\)",\{0,1\}$/\1/p'; }
# One string value of the setup file the service wrote before it answered.
window() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" "$(state_dir)/bootstrap.toml" 2>/dev/null; }
plural() { if [ "$1" = 1 ]; then echo "1 $2"; else echo "$1 $3"; fi; }
# The ready block, framed so that it stands out in a log that mixes several services. $1
# is the address to open, $2 the address on the ready line, $3 what follows it there.
RULE=$(printf '%.0s-' {1..64})
STARS=$(printf '%.0s*' {1..64})
EQUALS=$(printf '%.0s=' {1..64})
ready_block() {
    local status users admins jobs version head="" secret=""
    status=$(portal-download status 2>/dev/null)
    users=$(count users "$status")
    admins=$(count admins "$status")
    jobs=$(count jobs "$status")
    if [ "$users" = 0 ]; then
        head="SETUP NEEDED: nobody is set up yet"
    elif [ "$admins" = 0 ]; then
        head="SETUP NEEDED: nobody manages people"
    fi
    if [ -n "$head" ]; then
        secret="$(window secret)  (valid until $(window expires_at); enter it in the browser)"
        say "$STARS"
        say "*  $head"
    elif [ -n "$users" ]; then
        head="READY: set up, $(plural "$users" person people), $(plural "$jobs" job jobs)"
    fi
    say "$RULE"
    [ -n "$secret" ] || say "   $head"
    say "   Open:                   $1"
    if [ -z "$PROXY" ]; then
        local ca
        ca=$(fingerprint "$TLS/public/ca.crt")
        if [ -n "$ca" ]; then
            say "   Certificate authority:  SHA-256 ${ca#*=}"
            say "                           trust it once: https://$SITE/ca.crt (compare first)"
        else
            local current
            current=$(fingerprint)
            say "   Certificate:            SHA-256 ${current#*=}  (compare before accepting)"
        fi
    else
        say "   Proxy target:           http://portal-download:8080/ (the container's or compose service's name; do not publish that port)"
    fi
    [ -z "$secret" ] || say "   Setup secret:           $secret"
    say "$EQUALS"
    version=$(portal-download version | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
    say "portal-download $version is ready: $2$3"
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
    ready_block "the address of your reverse proxy" "port 8080 of this container" " (PROXY=external)"
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

start_caddy() {
    caddy run --config /etc/caddy/Caddyfile --adapter caddyfile &
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
    ready_block "https://$SITE/" "https://$SITE/" ""
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
