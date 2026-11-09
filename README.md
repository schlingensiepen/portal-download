# portal-download

An intranet workbench for a small, invited circle of people who regularly have to download invoices, statements and other documents from web portals.

The workbench fills the browser tab. The **right quarter** is a work bar showing the most urgent download job: when it is due, the login with copy buttons, and a short Markdown instruction saying what to file where. **◀ ▶** pages through the jobs in order of urgency. **Run** opens the portal on the **left**, either embedded, so that the work bar stays beside it, or, because many portals forbid being embedded, in a new tab of the browser with its full navigation; the work bar then stays in the workbench's tab. When everything is downloaded, close the run with **Succeeded** or **Failed**. The next due date follows from that.

**Import logins (Firefox CSV)** above the job list turns logins saved in Firefox into jobs. The browser reads the file that Firefox exports under `about:logins` (*Export Passwords…*) itself and sends nothing until you confirm. It lists host and username of every login, with nothing ticked, because the export holds every saved password. Each ticked login becomes a job named after its host, with the login, the embedding switch the server suggests (see below), no instructions, due today and every four weeks unless you change the weeks in its row. Several logins for one host become several jobs of that name. A login whose host name (case-insensitive; scheme, port and path aside) and username, exactly as written, match a job's entry address and username is marked as already there and cannot be ticked; within the file such duplicates count once, and the entry whose password was changed last wins. A login the service refuses (a password with a line break, for example) stays ticked, and one message after the run lists every refusal with its reason; the others are created.

## Languages

The application is in English and follows the browser's language where a translation exists; German is included. The choice can be changed at the top of the work bar and is kept in that browser. A translation is one file, `static/i18n/<code>.json`, listed in `static/config.json`. API, self-description and this documentation are in English. Messages from the API appear in English in every language.

## How the schedule works

"Today" is the date in the server's local time zone. Run the service in the circle's time zone (`TZ=Europe/Berlin` in a container) so that jobs become due at local midnight.

Every job has an interval in weeks and a retry period in days.

- **Urgency** = days from today to the next due date. Negative means overdue. The most urgent job comes first.
- **Succeeded:** next due date = the day it was closed + interval in weeks.
- **Failed:** next due date = the day it was closed + retry days (default 2). The run and its note are kept in the job's history.

## How the secrets are kept

- **Portal logins are stored in plain text on the service.** Every person of the circle sees the username and password of every job, copies them with one click and may change them in the job's edit form; a change applies for everybody at once. The protection is the intranet, HTTPS and access by invitation: whoever can read the service's store or its backup can read the logins. Agents see them only with the ability `logins.read`, or as part of the backup with `backup.read`.
- **Access to the service comes by invitation, with no passwords.** A person with the right to administer people creates an invitation link. It can be opened on each of the invited person's devices within seven days. Every device gets its own credential as an `HttpOnly; Secure; SameSite=Lax` cookie, renewed on every visit. The server stores only its hash. A lost device is revoked on its own, without touching the person's other devices.
- **Agents work with a token for one ability**, never with a person's access. An agent registers itself with a short introduction and receives a token without any ability plus an eight-letter display string. It tells its user only the display string. A person with the right to administer people then grants exactly one ability under **People**: reading jobs without their logins (`jobs.read`), reading jobs with their logins (`logins.read`, which lists the jobs as well, so such an agent needs no second token), changing jobs including their logins (`jobs.write`), closing runs (`jobs.complete`), or taking the backup (`backup.read`). They can also create a token directly for a script that cannot register itself. No agent administers people, devices or tokens. An agent receives logins only with `logins.read` or as part of the backup with `backup.read`; `jobs.write` sets them but gets jobs back without them. A token nobody grants expires after 24 hours, a granted one after 180 days without use, and any token can be revoked at once. At most ten registrations wait at the same time.
- **The setup window** is the file `bootstrap.toml` in the state directory. It holds a setup secret and an expiry 30 minutes ahead. Every start writes a fresh one, and `portal-download bootstrap` opens it without a restart. It is the only way to create the first person who administers people, or a new one when all of them have lost their devices. Nobody has to delete it; it closes by itself.

The service has **no start secret** of its own, so there is nothing to set before the first start.

## Backup

`curl -fsS -H "Authorization: Bearer <token>" https://<host>/backup -o backup.jsonl` fetches everything as one JSONL file. The token is one with the ability `backup.read` (*Take the backup* in the application). When a person creates such a token in the application, it shows this line with the token filled in; over the API, `agent.create` returns the token for the same line. Uploading a backup in the application adds it to what is there and overwrites nothing. **The backup holds the portal logins in plain text**, together with the hashes of all accesses. Protect the way to it: grant `backup.read` only to a script you trust, and keep the file where only those who may see the logins can reach it. Details are in [docs/operating.md](docs/operating.md#backup).

## In a container

```bash
docker run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download \
  && sed '/ is ready: /q' <(timeout 300 docker logs -f portal-download 2>&1)
```

The last line shows the container's log until its ready block, which ends with the line `portal-download container: portal-download <version> is ready: https://<SITE>/` (see **First start** below); with `PROXY=external` the ready line names port 8080 of the container instead of an address. It needs bash or zsh; `docker logs portal-download` shows the block at any time later. `sed` reads from `<(…)` rather than from a pipe, so that the command ends at once: in a pipe, the shell would wait for `docker logs -f` to end, and it notices that `sed` is gone only when the next log line arrives. `timeout 300` ends that follower in the background after five minutes at the latest. macOS has no `timeout`, and there the line prints nothing: leave `timeout 300` out on a Mac, and the follower ends with the next log line.

The same start with other runtimes:

```powershell
# Windows with WSLC (PowerShell)
wslc volume create portal-download
wslc run -d --name portal-download `
  -p 0.0.0.0:443:443 -v portal-download:/data `
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin `
  ghcr.io/schlingensiepen/portal-download
wslc logs -f portal-download 2>&1 | Where-Object { Write-Host "$_"; "$_" -match ' is ready: ' } | Select-Object -First 1 | Out-Null
```

```bash
# Linux with Podman
podman run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download \
  && sed '/ is ready: /q' <(timeout 300 podman logs -f portal-download 2>&1)

# macOS with Podman: macOS has no timeout, so the last line follows the log without it
podman run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download \
  && sed '/ is ready: /q' <(podman logs -f portal-download 2>&1)

# macOS with Docker Desktop: the docker command above, without "timeout 300"

# macOS with Apple's container (once: container system start)
container volume create portal-download
container run -d --name portal-download \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download \
  && sed '/ is ready: /q' <(container logs --follow portal-download 2>&1)
```

In PowerShell (Windows PowerShell 5.1 and PowerShell 7 alike), `Where-Object` writes every log line and lets only the ready line pass; `Select-Object -First 1` then stops the pipeline together with `wslc logs -f`, and nothing else ends with it. This is not tested on Windows: if the command does not end after the ready line, stop it with Ctrl-C. `"$_"` turns the lines that arrive through `2>&1` as error records into plain text. `wslc logs portal-download`, `podman logs portal-download` and `container logs portal-download` show the block at any time later.

On a Mac with Apple silicon every runtime takes the `arm64` image, with no emulation. WSLC publishes a port only on `127.0.0.1` unless an address is given, hence `0.0.0.0:443:443`; the Windows firewall must also let port 443 in. WSLC and `container` start the container again only when told to (`wslc start portal-download`, `container start portal-download`); the volume keeps everything in between, also across a reboot of the machine. The smoke test runs with Docker; the other runtimes are not tested by it. Below, `docker logs`, `docker exec` and `docker cp` stand for the same command of the runtime in use (`wslc logs`, `podman logs`, `container logs` and so on).

`SITE` is the host name or IPv4 address the browsers use: a name that their DNS resolves to this Docker host, or the host's IP address, which then has to stay fixed. Port 443 there must be reachable. The certificate is made for `SITE`; when `SITE` changes, the next start makes a new one (not for a certificate of your own in `/data/tls`). For a trial on the same machine, `SITE=localhost` does. `TZ` is the circle's time zone (see the schedule below). The image holds the service and [Caddy](https://caddyserver.com) in front of it. Everything that has to survive lies in the volume `/data`: the store, the setup file, the certificate and Caddy's working files. Images exist for `amd64` and `arm64`. `docker exec` runs as the image's own user, so `docker exec portal-download portal-download status` (and likewise `check`, `version`, `bootstrap`) needs nothing else.

**First start:**

1. The one-liner above, or `docker logs portal-download` later, shows the ready block, three lines at the end of the start:

   ```text
   portal-download container: certificate SHA-256 fingerprint 3F:A1:…:7C; compare it with what the browser shows before accepting.
   portal-download container: not set up yet. Setup secret: pds_… (valid until 2026-11-08T10:30:00Z); enter it in the browser.
   portal-download container: portal-download 0.7.2 is ready: https://downloads.intra.example/
   ```

   Once somebody is set up, the middle line reads `set up: 3 people, 12 jobs.` instead; while nobody administers people, it names the setup secret as well. The secret is valid for 30 minutes. If it has run out, `docker exec portal-download portal-download bootstrap` (with WSLC `wslc exec portal-download portal-download bootstrap`) prints a fresh one. With `PROXY=external` the block has no fingerprint, and the ready line names port 8080 of the container, which the reverse proxy in front reaches by the container's name or its compose service name, for example `http://portal-download:8080/`. Caddy writes only warnings and errors to the log, so the block stands at the end.
2. Open `https://<SITE>/`, the address on the ready line. The page also answers under any other name of the machine, such as its IP address or another host name; the browser then warns in addition that the certificate is for another name, and accepting it works the same. So if the clients cannot resolve `SITE`, the page still opens under the IP address, with that second warning, and the ready line shows an address they cannot open; better start the container again with the IP address as `SITE`. Invitation links name the address in the address bar of whoever creates them, so create them under the address the others can open. The browser warns about the certificate: the image created it for `SITE` at first start, valid for 120 days. The running container renews it every 90 days, and the browser then asks once more; until then a page that is open reports that the connection failed, and reloading it brings up the warning. Open the certificate details on the warning page, compare the SHA-256 fingerprint with the one from the log, then accept it. After each renewal the log shows the new fingerprint; `docker exec portal-download openssl x509 -in /data/tls/cert.pem -noout -fingerprint -sha256 -enddate` shows the current one and when it runs out. Tell the browser users in advance that every 90 days they are asked once more.
3. Enter the setup secret and your name, and continue with the first job. To restore a backup on this instance instead, see [docs/operating.md](docs/operating.md#backup).

**Operating it:**

- **Backup with `curl`:** as described under [Backup](#backup). The line the application shows needs `--cacert cert.pem` added, with the certificate from `docker cp portal-download:/data/tls/cert.pem .`. The certificate changes every 90 days, so a job that runs unattended takes it anew right before each run: `docker cp portal-download:/data/tls/cert.pem . && curl --cacert cert.pem …`, on the Docker host. Or write the backup inside the container, copy it out and delete it there, because the file holds the logins in plain text: `docker exec portal-download portal-download backup /data/backup.jsonl`, `docker cp portal-download:/data/backup.jsonl .`, `docker exec portal-download rm /data/backup.jsonl`.
- **Tags:** without a tag the image is `latest`, the last release. A tag `X.Y` such as `0.4` follows the fixes of one minor version, `X.Y.Z` such as `0.4.0` stays put, `edge` follows the main branch.
- **Upgrading:** take a backup first, because a store written by a newer version is refused by an older one. Then `docker pull ghcr.io/schlingensiepen/portal-download`, `docker stop portal-download`, `docker rm portal-download`, and the same `docker run` as at first start, with all its options. The volume keeps everything.
- **A certificate of your own** (from an intranet CA, for example): copy it in as the image's user, then restart:

  ```sh
  docker exec -i portal-download sh -c 'cat > /data/tls/cert.pem' < cert.pem   # with the chain, if any
  docker exec -i portal-download sh -c 'umask 077; cat > /data/tls/key.pem' < key.pem
  docker restart portal-download
  ```

  A certificate you put there is never replaced; the log warns when it does not match `SITE` or runs out within 30 days. Deleting both files makes the image create its own again.
- **A host directory instead of the named volume** must belong to the image's user: `chown -R 10001:10001 <directory>`. Otherwise the container ends with exit code 4 and `docker logs` says why; with `--restart unless-stopped` Docker keeps starting it again until the directory is fixed. With rootless Docker or Podman the UID is mapped, and with SELinux the mount needs `:Z`.
- **Do not use `--network host`.** The service trusts proxy headers from the loopback interface, which is the container's own only while the container has its own network.
- **With WSLC or `container`,** check after the first start that the container runs (`wslc ps`, `container ls`). If it ended with exit code 4, the runtime created the volume with another owner; its log names the `chown` that is missing.

**With Docker Compose**, the same container as above:

```yaml
services:
  portal-download:
    image: ghcr.io/schlingensiepen/portal-download:0.7
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

**Behind a reverse proxy of your own**, for example Caddy as a second service in the stack: `PROXY=external` starts the service without the Caddy inside the image, with no certificate and no `SITE`. The service then listens on port 8080 of its container and takes scheme, host and client address from the `X-Forwarded-*` headers of whoever reaches it. Publish port 8080 nowhere, so that only the proxy in the same network reaches it. The proxy must send `X-Forwarded-Proto` and `X-Forwarded-Host` and set `X-Forwarded-For` itself; Caddy does all three by default, nginx sends no `X-Forwarded-Host` unless told to.

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
    image: ghcr.io/schlingensiepen/portal-download:0.7
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

with this `Caddyfile` beside it, and the certificate and key in `./certs` (or any other `tls` setting of Caddy):

```caddyfile
downloads.intra.example {
	tls /certs/cert.pem /certs/key.pem
	reverse_proxy portal-download:8080
}
```

Caddy sends the `X-Forwarded-*` headers on its own. The image's configuration for `PROXY=external` already sets `listen = "0.0.0.0:8080"` and `trust_forwarded_headers = true`; nothing needs to be mounted. When Caddy runs in another compose project, both must share a Docker network (an `external: true` network in both files), and Caddy reaches the service by its container name. The certificate in `./certs` is yours, from an intranet CA for example; a backup with `curl` goes through Caddy and needs `--cacert` only when the backup machine does not trust that certificate. The ready block with the setup secret is in `docker compose logs portal-download`.

## Running it

Requirements: Rust (stable) to build, and HTTPS in front for anything other than `localhost`. Browsers allow the clipboard and `Secure` cookies only in a secure context. `http://localhost` and `http://127.0.0.1` count as secure, so a local trial needs no certificate.

```sh
cargo build --release
cp examples/config.toml my-config.toml
# set state_dir in my-config.toml to a directory you can write, for example ./state
./target/release/portal-download --config my-config.toml check   # judge the configuration
./target/release/portal-download --config my-config.toml serve   # run
```

The first start logs the setup secret. Open the service in a browser, enter the secret and your name, and accept the invitation on that device. Then create the first download job. Invite the others under **People**.

Put a reverse proxy with TLS on the same host in front, see [`examples/Caddyfile`](examples/Caddyfile), and run the service as its own user, see [`examples/portal-download.service`](examples/portal-download.service). The service takes `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` only from a proxy on the same host (loopback), unless `trust_forwarded_headers = true`, and that proxy must set them itself rather than pass on what the browser sent (Caddy does by default). Details are in [docs/operating.md](docs/operating.md).

### Commands and exit codes

| Command | What it does |
|---|---|
| `serve` (default) | Runs the service; opens the setup window at start. |
| `check` | Prints every configuration value with its source, and whether the store is usable (JSON). |
| `status` | Prints counts of people, devices and jobs, the jobs due and the store's structure version (JSON). |
| `version` | Prints version and the source revision the binary was built from (JSON). |
| `backup FILE` | Writes the backup (JSONL, the same file as `GET /backup`) to a new file, also while the service runs. Never overwrites. |
| `bootstrap [--minutes N]` | Opens the setup window without a restart and prints the setup secret once. |

Exit codes: `0` success · `1` runtime error · `2` wrong arguments · `3` configuration invalid · `4` state directory or store unusable.

### Configuration

There is one TOML file with three keys, `listen`, `state_dir` and `trust_forwarded_headers` (see [`examples/config.toml`](examples/config.toml)). The file is selected by `--config PATH`, then by `PORTAL_DOWNLOAD_CONFIG`, then from `$XDG_CONFIG_HOME/portal-download/config.toml` (usually `~/.config/…`), then from `/etc/portal-download/config.toml`. Without a file, `listen` is `127.0.0.1:8080` and `state_dir` is `$XDG_STATE_HOME/portal-download` (usually `~/.local/state/portal-download`). All values come from that one file, so it can be versioned and rolled back as a whole. An empty `listen` means the default `127.0.0.1:8080`. An empty `state_dir` aborts the start instead of silently writing somewhere else. `trust_forwarded_headers` is `false` by default: scheme, host and client address are taken from the `X-Forwarded-*` headers only when the request comes from the loopback interface, where the reverse proxy runs. `true` takes them from every sender; set it only when nothing but the reverse proxy reaches the service.

## For agents and scripts

The service describes itself. `GET /` without `Accept: text/html` returns a short plain-text entry page that links to the usage description (`/describe/usage`), the machine contract in OpenRPC form (`/describe/api`) and an anchor skill (`/describe/skill`). Every URL there is rendered with the host the request arrived on. The API is JSON-RPC 2.0 at `POST /api/`. An agent finds there how to register, where to keep its token (`PORTAL_DOWNLOAD_AGENT_TOKEN`) and the rules for handling it.

## Embedding portals

Most portals send `X-Frame-Options` or a `frame-ancestors` policy, and the browser then refuses to show them inside another page. Each job therefore has a switch **may be embedded**:

- **On:** the portal loads in a sandboxed frame on the left. The frame may run scripts, submit forms and download, but it may not navigate the workbench away.
- **Off:** **Run** opens the portal in a new tab, with the browser's address bar and navigation, and with no link back to the workbench. Switch back to the workbench's tab for the login and to close the run. Each **Run** and **Open the portal again** opens a further tab.

The service suggests the switch. When the entry address of a job is entered or changed, and for every login of an import, the server fetches the address once, follows its redirects and reads those headers on the page it ends on; **check now** under the switch does it again, also for a job created before the service checked, whose switch stays as it was until then. For an `http` address, or one that leads on to an `http` page, it suggests off, because the browser refuses such a page inside the `https` page. A page answering with a refusal (401, 403) is judged by its headers like any other; an address that does not exist (404, 410), a server error, or no answer leaves the switch as it is. The server needs to reach the portals for this; when it cannot, the switch stays as it is. The check sees only the page it lands on: when the portal's login page, reached later, stays empty inside the frame, switch the job off.

Even when embedding is allowed, a portal's login cookies count as third-party cookies inside a frame, and some browsers block them. If a portal logs you out inside the frame, switch the job off as well.

## Development

```sh
cargo test
```

The tests include the checks that hold the service's self-description:
- every method has a description and examples;
- no delivered text carries hindsight, internal identifiers or foreign hosts;
- the usage description names no operation;
- every placeholder names the call that yields it.

Each of these, and the tests for access, sanitizing, the last-administrator guard and the schedule, was seen to fail once with its safeguard removed. See [AGENTS.md](AGENTS.md) for how to work on this repository.

## License

Apache License 2.0, see [LICENSE](LICENSE).
