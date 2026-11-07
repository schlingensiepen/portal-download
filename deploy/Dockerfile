# syntax=docker/dockerfile:1
# portal-download in one image with Caddy in front, so that one `docker run` starts
# everything (see "In a container" in README.md). Built by .github/workflows/build-image.yml.

# Stage 1 compiles the service and is thrown away. Alpine, because the final image is
# Alpine-based Caddy: a binary linked against glibc would not run there.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# Dependencies first: a change to the source then reuses this layer from the cache.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src src
COPY static static
# Which build this is; the binary reports it in `portal-download version`. The source
# tree carries no .git here, so the pipeline passes the revision in.
ARG PORTAL_DOWNLOAD_REVISION=unknown
RUN touch src/main.rs \
    && PORTAL_DOWNLOAD_REVISION="$PORTAL_DOWNLOAD_REVISION" cargo build --release --locked \
    && ./target/release/portal-download version

# Stage 2 is what is delivered: the official Caddy image plus the binary.
FROM caddy:2
# bash: the entry point waits on two processes (`wait -n`), which busybox sh cannot.
# openssl: creates the certificate on first start. tzdata: TZ=Europe/Berlin takes effect.
RUN apk add --no-cache bash openssl tzdata \
    && addgroup -S -g 10001 portal-download \
    && adduser -S -D -H -u 10001 -G portal-download -h /data -s /sbin/nologin portal-download \
    && mkdir -p /data /config \
    && chown 10001:10001 /data /config \
    && getcap /usr/bin/caddy | grep -q cap_net_bind_service
COPY --from=build /src/target/release/portal-download /usr/local/bin/portal-download
COPY deploy/start.sh /usr/local/bin/start.sh
COPY deploy/Caddyfile /etc/caddy/Caddyfile
COPY deploy/config.toml /etc/portal-download/config.toml
ENV PORTAL_DOWNLOAD_CONFIG=/etc/portal-download/config.toml

# Never root: Caddy may open port 443 through the capability the Caddy image binds to
# its binary (checked above), and /data belongs to this user, so a named volume takes
# over that ownership when Docker creates it.
USER 10001:10001
WORKDIR /data
EXPOSE 443
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:8080/ || exit 1
ENTRYPOINT ["/usr/local/bin/start.sh"]
CMD []
