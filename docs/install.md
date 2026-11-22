# Installing portal-download

This page is for whoever sets the service up. The short way is in the [README](../README.md#in-a-container). Words such as *service*, *instance* and `SITE` are explained in the [glossary](glossary.md).

All examples use the host name `downloads.intra.example`. Replace it with yours.

## What you need

- A machine that runs containers and that the circle's browsers reach on port 443.
- A host name that the browsers' DNS resolves to that machine, or the machine's fixed IPv4 address.
- The time zone of the circle, such as `Europe/Berlin`.

The image holds the service and [Caddy](https://caddyserver.com), which serves HTTPS in front of it. Images exist for `amd64` and `arm64`. Everything that has to survive lies in the volume at `/data`.

## The start options

| Option | Meaning |
|---|---|
| `-e SITE=…` | The host name or IPv4 address the browsers use, alone: no scheme, no port, no path. The certificate is made for it. For a trial on your own machine, `SITE=localhost` does. |
| `-e TZ=…` | The circle's time zone. Jobs become due at midnight in this zone. |
| `-e PROXY=external` | Starts the service without Caddy, for a [reverse proxy of your own](#behind-a-reverse-proxy-of-your-own). `SITE` is then not used. |
| `-p 443:443` | The port the browsers reach. Only 443/tcp is served; there is no redirect from port 80. |
| `-v portal-download:/data` | The volume with the store, the setup file and the certificates. |
| `--restart unless-stopped` | Starts the container again after a failure or a reboot. |
| the image tag, `:0.9` | The version. See [image tags](reference.md#versions). |

When `SITE` changes, the next start makes a new certificate authority. Every computer then has to import it again.

Two things to avoid:

- **Do not use `--network host`.** The service trusts proxy headers that arrive on the loopback interface. That interface belongs to the container alone only while the container has its own network.
- **Do not start two containers on one volume.** Both would write the same store.

## Docker

```bash
docker run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.9
```

Then read the log:

```bash
docker logs portal-download
```

The start takes a few seconds. It is finished when the log ends with a line that contains ` is ready: `. Above that line stands the [ready block](reference.md#the-ready-block). If the line is not there yet, run the command again.

## Docker Compose

```yaml
services:
  portal-download:
    image: ghcr.io/schlingensiepen/portal-download:0.9
    restart: unless-stopped
    ports:
      - "443:443"
    volumes:
      - portal-download:/data
    environment:
      SITE: downloads.intra.example
      TZ: Europe/Berlin

volumes:
  portal-download:
```

Start it with `docker compose up -d`. The ready block is in `docker compose logs portal-download`.

## Other container runtimes

The image's test runs with Docker. The runtimes below are not covered by it.

**Podman on Linux or macOS:**

```bash
podman run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.9
podman logs portal-download
```

**Windows with WSLC (PowerShell):**

```powershell
wslc volume create portal-download
wslc run -d --name portal-download `
  -p 0.0.0.0:443:443 -v portal-download:/data `
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin `
  ghcr.io/schlingensiepen/portal-download:0.9
wslc logs portal-download
```

WSLC publishes a port only on `127.0.0.1` unless an address is given, hence `0.0.0.0:443:443`. The Windows firewall must also let port 443 in.

**macOS with Apple's `container`** (once before: `container system start`):

```bash
container volume create portal-download
container run -d --name portal-download \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.9
container logs portal-download
```

Notes for these runtimes:

- WSLC and `container` start the container again only when told to: `wslc start portal-download`, `container start portal-download`. The volume keeps everything in between.
- After the first start, check that the container runs (`wslc ps`, `container ls`). If it ended with exit code 4, the runtime created the volume with another owner. The log names the `chown` that is missing.
- On the other pages, `docker logs`, `docker exec` and `docker cp` stand for the same command of your runtime.

## First setup

1. Read the [ready block](reference.md#the-ready-block) in the log. On a fresh instance it says `SETUP NEEDED: nobody is set up yet` and shows the setup secret.
2. Make your computer trust the service's certificate authority, as [Certificates](#certificates) describes.
3. Open the address from the line `Open:`.
4. Enter the setup secret and your name, and click **Create the first access**. This creates the first person, with all three rights, and signs this browser in.
5. Create the first job, or use **Import logins (Firefox CSV)**. [using.md](using.md) describes both.
6. Invite the others under **People**. [operating.md](operating.md#invitations) describes how.

The setup secret is valid for 30 minutes after the start. If it has run out, print a fresh one:

```bash
docker exec portal-download portal-download bootstrap
```

To restore a backup on the new instance instead of starting empty, see [operating.md](operating.md#restoring-on-a-fresh-instance).

**If the browsers cannot resolve `SITE`:** the service also answers under any other name of the machine, such as its IP address. The browser then warns that the certificate is for another name. Better start the container again with the IP address as `SITE`.

## Certificates

The service is reached over HTTPS. The image makes its own certificate authority for `SITE` at the first start, and with it the certificate the browsers see. A computer has to trust that authority once. Until then:

- every browser warns about the certificate;
- the browser extension in Chrome and Edge cannot reach the service at all. These browsers do not count a certificate that was only accepted on a warning page.

**Do this before you open an invitation link.** Whoever invites people sends them this section together with the link, and the fingerprint from step 2.

The authority is limited to `SITE` by a name constraint. A computer that trusts it trusts it for this one name only.

### 1. Download the authority

Open `https://downloads.intra.example/ca.crt` in a browser. The browser warns about the certificate; accept the warning for this one download. The file is saved as `portal-download-ca.crt`.

A person who is signed in finds the same file on the overview page, under **Download the certificate authority**.

### 2. Compare the fingerprint

Compare the file's SHA-256 fingerprint with the one in the line `Certificate authority:` of the ready block. People who cannot read the log get the fingerprint from the operator. The overview page of the web application shows it too.

| System | Command | Output |
|---|---|---|
| Windows | `certutil -hashfile portal-download-ca.crt SHA256` | the same digits, in lower case and without colons |
| macOS | `shasum -a 256 portal-download-ca.crt` | the same digits, in lower case and without colons |
| Linux | `openssl x509 -inform der -in portal-download-ca.crt -noout -fingerprint -sha256` | the fingerprint as the block shows it |

If the fingerprints differ, do not import the file.

### 3. Import it as a trusted root

**Windows** (Chrome, Edge and usually Firefox):

1. Double-click the file and choose *Install Certificate…*.
2. Choose *Current User*. It needs no administrator. *Local Machine* needs one and covers every user of the computer.
3. Choose *Place all certificates in the following store*, then *Trusted Root Certification Authorities*.
4. Confirm the security warning and restart the browser.

**macOS** (Chrome, Edge and usually Firefox):

1. Double-click the file. *Keychain Access* opens; add the certificate to the keychain *login*.
2. Open the certificate *portal-download authority* there.
3. Under *Trust*, set *When using this certificate* to *Always Trust*, and close the window.
4. Restart the browser.

**Linux** (Chrome and Edge): these browsers keep their own list of authorities per user.

1. Open `chrome://certificate-manager` (Edge: `edge://certificate-manager`).
2. Under *Local certificates*, *Installed by you*, import the file as a trusted certificate.
3. Restart the browser.

In older versions the place is *Settings → Privacy and security → Security → Manage certificates → Authorities → Import*.

**Firefox** on Windows and macOS usually takes the roots of the system, so the steps above cover it. On Linux, and where Firefox still warns:

1. Open *Settings → Privacy & Security → Certificates → View Certificates…*.
2. On the tab *Authorities*, click *Import…* and choose the file.
3. Tick *Trust this CA to identify websites*.

In Firefox it is also enough to accept the warning page once. Firefox applies that to the extension too.

**`curl` and scripts** want the authority in PEM form:

```sh
curl -k https://downloads.intra.example/ca.crt -o ca.der
openssl x509 -inform der -in ca.der -noout -fingerprint -sha256   # compare with the ready block
openssl x509 -inform der -in ca.der -out ca.pem
```

Then add `--cacert ca.pem` to every `curl` call. `-k` is used for this one download only, because the fingerprint is compared afterwards.

### How long it lasts

| What | Valid for | Renewed |
|---|---|---|
| The certificate authority | 10 years | when `SITE` changes, or a year before it runs out |
| The certificate the browsers see | 120 days | after 90 days, by the same authority, while the service keeps running |

The container checks both at start and then once a day. A computer that trusts the authority accepts every renewed certificate without asking. Only a new authority has to be imported again. The log names it:

```text
portal-download container: created a certificate authority for downloads.intra.example, valid for 10 years. Browsers trust it once: download https://downloads.intra.example/ca.crt and import it as a trusted root.
```

The authority's key lies in the volume, readable by the image's user only. Whoever has the volume can issue certificates for `SITE`.

When an instance is replaced by a new one on a new volume, the new one has a new authority. Remove the old one from the computers, in the same place where it was imported. Each authority carries a random part in its name, and the fingerprint tells them apart.

### A certificate of your own

If you have a certificate for `SITE`, from an intranet CA for example, copy it into the volume and restart:

```sh
docker exec -i portal-download sh -c 'cat > /data/tls/cert.pem' < cert.pem   # with the chain, if any
docker exec -i portal-download sh -c 'umask 077; cat > /data/tls/key.pem' < key.pem
docker restart portal-download
```

- A certificate you put there is never replaced.
- The image's authority is then not offered at `/ca.crt`. The ready block shows the line `Certificate:` with the fingerprint of your certificate instead.
- The log warns when your certificate does not fit `SITE`, or runs out within 30 days.
- Delete both files and restart to have the image issue its own again.

## A host directory instead of the named volume

The directory must belong to the image's user:

```sh
chown -R 10001:10001 /srv/portal-download
```

Otherwise the container ends with exit code 4, and `docker logs` says why. With `--restart unless-stopped`, Docker keeps starting it again until the directory is fixed. With rootless Docker or Podman the user ID is mapped. With SELinux the mount needs `:Z`.

## Behind a reverse proxy of your own

`PROXY=external` starts the service without the Caddy inside the image. It makes no certificate and needs no `SITE`. The service listens on port 8080 of its container.

Rules for the proxy:

- Publish port 8080 nowhere. Only the proxy, in the same container network, may reach it.
- The proxy must send `X-Forwarded-Proto` and `X-Forwarded-Host`, and set `X-Forwarded-For` itself. Caddy does all three by default. nginx sends no `X-Forwarded-Host` unless told to.
- The proxy must pass `OPTIONS` requests to `/api/` on. The browser extension sends them first.

An example with Caddy as a second service:

```yaml
services:
  caddy:
    image: caddy:2
    restart: unless-stopped
    ports:
      - "443:443"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - ./certs:/certs:ro
      - caddy:/data
  portal-download:
    image: ghcr.io/schlingensiepen/portal-download:0.9
    restart: unless-stopped
    volumes:
      - portal-download:/data
    environment:
      PROXY: external
      TZ: Europe/Berlin

volumes:
  caddy:
  portal-download:
```

with this `Caddyfile` beside it, and your certificate and key in `./certs`:

```caddyfile
downloads.intra.example {
	tls /certs/cert.pem /certs/key.pem
	reverse_proxy portal-download:8080
}
```

Notes:

- Nothing needs to be mounted into the service's container. Its configuration for this mode is part of the image.
- When the proxy runs in another compose project, both need a shared Docker network. The proxy then reaches the service by its container name.
- The ready block is in `docker compose logs portal-download`. It names no certificate. Its line `Proxy target:` says how the proxy reaches the service.
- The certificate is yours. The computers must trust it, or the browser extension in Chrome and Edge cannot reach the service.
- The service offers no authority at `/ca.crt` in this mode.

## Without a container

You need Rust (stable) to build, and a reverse proxy with HTTPS in front. Browsers allow the clipboard and `Secure` cookies only in a secure context. `http://localhost` and `http://127.0.0.1` count as secure, so a trial on your own machine needs no certificate.

```sh
cargo build --release
cp examples/config.toml my-config.toml
# set state_dir in my-config.toml to a directory you can write, for example ./state
./target/release/portal-download --config my-config.toml check   # judge the configuration
./target/release/portal-download --config my-config.toml serve   # run
```

The start log shows the setup secret:

```text
portal-download: not set up yet. Setup secret: pds_<SECRET> (valid until <YYYY-MM-DD>T<hh:mm:ss>Z; also in <state_dir>/bootstrap.toml). Open the service in a browser and enter it.
```

Open the service in a browser and continue as under [First setup](#first-setup), from step 4.

For a lasting installation:

- Put a reverse proxy with TLS on the same host in front; see [`examples/Caddyfile`](../examples/Caddyfile).
- Run the service as its own user; see [`examples/portal-download.service`](../examples/portal-download.service).
- The proxy must set the `X-Forwarded-*` headers itself and not pass on what the browser sent. Caddy does so by default.
- To offer your proxy's authority at `/ca.crt`, set `ca_certificate` in the configuration.

[reference.md](reference.md#configuration) lists the configuration keys, the commands and the files.
