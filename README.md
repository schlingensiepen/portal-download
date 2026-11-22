# portal-download

> [!IMPORTANT]
> **Warum es das gibt.** Rechnungen, Bescheide und Abrechnungen kommen bei vielen Unternehmen nicht mehr per Post oder E-Mail, sondern liegen nur noch in einem Portal, aus dem ich sie mir selbst holen muss. Das ist eine absolute Unart. Jedes Unternehmen hat sein eigenes Portal, ein einheitliches Verfahren gibt es nicht, und der einzige Grund, den ich mir dafür vorstellen kann, ist die Einbildung, so ließe sich die Zustellung beweisen. Mich nervt das gewaltig. portal-download löst das Problem nicht, es macht es für mich nur erträglicher, und zufrieden bin ich damit nicht.

> [!NOTE]
> **Why this exists.** Invoices, notices and statements from many companies no longer come by post or e-mail; they sit in a portal, and I have to fetch them myself. That is an utter bad habit. Every company has its own portal, there is no common procedure, and the only reason I can think of is the belief that this proves delivery. It annoys me enormously. portal-download does not solve the problem; it only makes it bearable for me, and I am not happy with it.

An intranet workbench for a small, invited circle of people who regularly have to download invoices, statements and other documents from web portals.

The service keeps the list of download jobs. Two surfaces work with it:

- **The browser extension** for Chrome, Edge and Firefox shows the jobs in the browser's side panel, next to the portal, and works with several instances at once and with a store of its own in the browser. It is the recommended way of working; see [Browser extension](#browser-extension).
- **The web application** of the service, at its address. It holds the administration and a simple way of working. The **right quarter** is a work bar showing the most urgent download job: when it is due, the login with copy buttons, the last note, and a short Markdown instruction saying what to file where. **◀ ▶** pages through the jobs in order of urgency. **Open the portal** opens it in a new tab of the browser, with its full navigation and no link back. When everything is downloaded, come back to the workbench's tab and close the run with **Succeeded** or **Failed**. The job then stays in sight with its new due date until **Next job**.

**Import logins (Firefox CSV)** above the job list turns logins saved in Firefox into jobs. The browser reads the file that Firefox exports under `about:logins` (*Export Passwords…*) itself and sends nothing until you confirm. It lists host and username of every login, with nothing ticked, because the export holds every saved password. Each ticked login becomes a job named after its host, with the login, no instructions, due today and every four weeks unless you change the weeks in its row. Several logins for one host become several jobs of that name. A login whose host name (case-insensitive; scheme, port and path aside) and username, exactly as written, match a job's entry address and username is marked as already there and cannot be ticked; within the file such duplicates count once, and the entry whose password was changed last wins. A login the service refuses (a password with a line break, for example) stays ticked, and one message after the run lists every refusal with its reason; the others are created.

## Languages

The application is in English and follows the browser's language where a translation exists; German is included. The choice can be changed at the top of the work bar and is kept in that browser. A translation is one file, `static/i18n/<code>.json`, listed in `static/config.json`. API, self-description and this documentation are in English. Messages from the API appear in English in every language.

## How the schedule works

"Today" is the date in the server's local time zone. Run the service in the circle's time zone (`TZ=Europe/Berlin` in a container) so that jobs become due at local midnight.

Every job has an interval in weeks and a retry period in days.

- **Urgency** = days from today to the next due date. Negative means overdue. The most urgent job comes first.
- **Succeeded:** next due date = the day it was closed + interval in weeks.
- **Failed:** next due date = the day it was closed + retry days (default 2). The run and its note are kept in the job's history.

## Who may do what

- **Access to the service comes by invitation, with no passwords.** An invitation link can be opened in each of the invited person's browsers, and in the browser extension, within seven days. Every browser gets its own credential as an `HttpOnly; Secure; SameSite=Lax` cookie, renewed on every visit; the extension gets its own token, which it sends as `Authorization: Bearer`. The server stores only their hashes. A lost device is revoked on its own, without touching the person's other devices. Under **My devices**, *Add to the browser extension* creates a link for a further device of one's own, which serves once within the hour.
- **Every person works on all jobs.** Three rights add to that, each held or not:
  - **manage people:** invite, give new invitations, deactivate and delete people, see and revoke every device (**All devices**);
  - **manage agents:** grant, create and revoke agent tokens (**Agents**);
  - **manage the backup:** take the full backup; reading one in with its people needs all three rights.

  A right is given or taken away only by someone who holds it, and one active person always holds all three. Acting on a person (a new invitation, deactivating, deleting, revoking a device) needs every right that person holds, so nobody takes over a person who holds more. The first person, created at setup, holds all three.
- **Portal logins are stored in plain text on the service.** Every person of the circle sees the username and password of every job, copies them with one click and may change them in the job's edit form; a change applies for everybody at once. The protection is the intranet, HTTPS and access by invitation: whoever can read the service's store or a file without a password can read the logins.
- **Agents work with a token for one role**, never with a person's access. An agent registers itself with a short introduction and receives a token without any role plus an eight-letter display string. It tells its user only the display string. A person, or a token, who manages agents then grants exactly one role under **Agents**:

  | Role | permits |
  |---|---|
  | `jobs.read` | reading jobs without their logins |
  | `jobs.work` | working through jobs: reading them with their logins, closing runs, exporting them |
  | `jobs.manage` | managing jobs: reading them without their logins, creating, changing and deleting them (setting logins), closing runs, reading files in |
  | `jobs.all` | everything on jobs |
  | `backup.read` | taking the full backup |
  | `people.manage` | managing people, as the right of a person does |
  | `agents.manage` | managing agent tokens, with exactly the roles the granting person could grant |

  A role is granted only by someone who could hold it: the backup role only by someone with the right to manage the backup, and so on. They can also create a token directly for a script that cannot register itself. An agent receives logins only with `jobs.work`, `jobs.all` or as part of the backup. A token nobody grants expires after 24 hours, a granted one after 180 days without use, and any token can be revoked at once, without touching the tokens it granted. At most ten registrations wait at the same time.
- **The setup window** is the file `bootstrap.toml` in the state directory. It holds a setup secret and an expiry 30 minutes ahead. Every start writes a fresh one, and `portal-download bootstrap` opens it without a restart. It is the only way to create the first person, or a new one with all rights when all of them have lost their devices. Nobody has to delete it; it closes by itself.

The service has **no start secret** of its own, so there is nothing to set before the first start.

## Exchange and backup

Every job carries a GUID that identifies it on every instance and in the browser extension's own store. A new entry address makes it a new job for the exchange; the job keeps its history and remembers the address it replaces.

**One file format** serves the exchange and the backup: a ZIP file. Under **Exchange**:

- **Export the jobs** (every person): every job with its login in plain text, its history and the deleted jobs. With a password (8 to 200 characters, letters A to Z, digits, punctuation and spaces, no umlauts, because it travels in an HTTP header) the file is encrypted with AES-256; without one, the application asks first, because the logins are in plain text.
- **Full backup** (right to manage the backup): the jobs, and the people, invitations, devices and agent tokens with the hashes of their credentials.
- **Read a file in**: from this instance, another one or the browser extension. First a preview shows every job of the file in one of three groups:
  - **taken over, because new:** not known here; it comes with its runs;
  - **taken over, because the same:** known with equal fields; runs not known here are added, and the due date follows the newest run;
  - **please decide:** known with differing fields, each shown with both values and when it changed, the newer one preselected (for the due date, the nearer one, when no newer run decides it); or deleted here (bring it back, or leave it out); or a job with a new address that replaces one here (replace it and keep its history, keep both, or leave it out).

  Nothing is read in until you confirm, no run is ever lost, and the same file read in twice asks again only where it still differs. With all three rights, a full backup can bring its people, devices and tokens in beside the existing ones; a person with the name or e-mail of one here comes in with "(Import YYYY-MM-DD)" after the name.

A script fetches the full backup with a token for `backup.read` (role *Take the backup*). Once, as the user the script runs as, write the password into a header file that only this user can read; the password follows the rules above, and a space at either end is refused:

```sh
umask 077
printf 'X-Backup-Password: %s\n' 'the backup password' > /root/backup-header
```

Then, with the token in the environment variable `PORTAL_DOWNLOAD_AGENT_TOKEN`:

```sh
curl -fsS -H "Authorization: Bearer $PORTAL_DOWNLOAD_AGENT_TOKEN" -H @/root/backup-header \
  https://<host>/backup -o portal-download-$(date +%F).zip
```

In a crontab every `%` must be written `\%`, and the variable set above the line:

```crontab
PORTAL_DOWNLOAD_AGENT_TOKEN=pda_…
30 2 * * * cd /srv/backup && curl -fsS -H "Authorization: Bearer $PORTAL_DOWNLOAD_AGENT_TOKEN" -H @/root/backup-header https://<host>/backup -o portal-download-$(date +\%F).zip
```

The password travels in the header `X-Backup-Password` only, never in the address, and the header file keeps it out of the command line and the process list. Without the header the file is not encrypted. When a person creates such a token under **Agents**, the application shows the line with the token filled in. **A file without a password holds the portal logins in plain text**: keep it where only those who may see the logins can reach it. Details are in [docs/operating.md](docs/operating.md#backup).

## Browser extension

The extension shows the due jobs of every service it knows, and of its own store, in one list in the browser's side panel; the portal opens in a normal tab beside it. Download it from the link on the service's overview page, or from the releases of the `portal-download-extension` repository. In Firefox the signed file installs with a click; in Chrome and Edge, unpack the ZIP and load it under *Extensions → Developer mode → Load unpacked*.

Words used there: a **backend** is one source of jobs, either a service or the **local store**, which keeps jobs in that browser only, encrypted with a passphrase. To move jobs between backends, export them on one, read the file in on the other, and delete them where they were; reading in copies. The extension's own README explains it for its users.

**Adding a service:** open an invitation link in the browser, or *Add to the browser extension* under **My devices**, and click **+** in the extension while that tab is open. The extension takes the invitation from the tab's address and asks for permission to reach that one service.

**The certificate:** the extension reaches a service only when the computer trusts its certificate. Chrome and Edge do not count a certificate that was merely accepted on a warning page. A service in the container image offers its own certificate authority for that: download it from the overview page, compare its fingerprint with the one there or in the container's log, and import it once as a trusted root certificate authority, before opening the invitation (on Windows: double-click the file, *Install certificate*; *Current user* needs no administrator, *Local machine* does and covers every user; store *Trusted Root Certification Authorities*). The authority is limited to the service's name, so it vouches for nothing else. Firefox on Windows and macOS usually trusts the roots of the system; on Linux, and otherwise, import it in Firefox's settings under *Privacy & Security → Certificates → View Certificates → Authorities*, or accept the warning there once, which Firefox also applies to the extension.

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

`SITE` is the host name or IPv4 address the browsers use: a name that their DNS resolves to this Docker host, or the host's IP address, which then has to stay fixed. Port 443 there must be reachable. The certificate authority and the certificate are made for `SITE`; when `SITE` changes, the next start makes new ones, and the authority has to be imported again (not for a certificate of your own in `/data/tls`). For a trial on the same machine, `SITE=localhost` does. `TZ` is the circle's time zone (see the schedule above). The image holds the service and [Caddy](https://caddyserver.com) in front of it. Everything that has to survive lies in the volume `/data`: the store, the setup file, the certificates and Caddy's working files. Images exist for `amd64` and `arm64`. `docker exec` runs as the image's own user, so `docker exec portal-download portal-download status` (and likewise `check`, `version`, `bootstrap`) needs nothing else.

**First start:**

1. The one-liner above, or `docker logs portal-download` later, shows the ready block at the end of the start. While a setup is needed, stars frame it:

   ```text
   portal-download container: ****************************************************************
   portal-download container: *  SETUP NEEDED: nobody is set up yet
   portal-download container: ----------------------------------------------------------------
   portal-download container:    Open:                   https://downloads.intra.example/
   portal-download container:    Certificate authority:  SHA-256 3B:0E:…:7C
   portal-download container:                            trust it once: https://downloads.intra.example/ca.crt (compare first)
   portal-download container:    Setup secret:           pds_…  (valid until 2026-11-08T10:30:00Z; enter it in the browser)
   portal-download container: ================================================================
   portal-download container: portal-download 0.8.1 is ready: https://downloads.intra.example/
   ```

   Once somebody manages people, the block begins with the dashes and the line `READY: set up, 3 people, 12 jobs`, without a setup secret; while nobody manages people, the heading is `SETUP NEEDED: nobody manages people`, with the setup secret. The secret is valid for 30 minutes. If it has run out, `docker exec portal-download portal-download bootstrap` (with WSLC `wslc exec portal-download portal-download bootstrap`) prints a fresh one. With `PROXY=external` the block names no certificate but how the reverse proxy reaches the service: by the container's name or its compose service name, for example `http://portal-download:8080/`. Caddy writes only warnings and errors to the log, so the block stands at the end.
2. Open `https://<SITE>/`, the address on the ready line. The browser warns about the certificate until the computer trusts the image's certificate authority: download it from `https://<SITE>/ca.crt`, compare its SHA-256 fingerprint with the one in the block, and import it as a trusted root (see [Browser extension](#browser-extension)); the browser extension in Chrome and Edge needs that. Alternatively accept the warning in the browser. The image renews its certificate every 90 days with the same authority, so a browser that trusts the authority never asks again. The page also answers under any other name of the machine, such as its IP address; the browser then warns that the certificate is for another name. So if the clients cannot resolve `SITE`, better start the container again with the IP address as `SITE`. Invitation links name the address in the address bar of whoever creates them, so create them under the address the others can open.
3. Enter the setup secret and your name, and continue with the first job, or with **Import logins (Firefox CSV)**. To restore a backup on this instance instead, see [docs/operating.md](docs/operating.md#backup).

**Operating it:**

- **Backup with `curl`:** as described under [Exchange and backup](#exchange-and-backup). A machine that does not trust the image's authority fetches it once, compares its fingerprint with the one in the ready block, and converts it for curl, which wants PEM:

  ```sh
  curl -k https://<SITE>/ca.crt -o ca.der
  openssl x509 -inform der -in ca.der -noout -fingerprint -sha256   # compare with the block
  openssl x509 -inform der -in ca.der -out /srv/backup/ca.pem
  ```

  and adds `--cacert /srv/backup/ca.pem` to the curl line. The authority stays the same across renewals. Or write the backup inside the container, copy it out and delete it there: `docker exec portal-download portal-download backup /data/backup.zip`, `docker cp portal-download:/data/backup.zip .`, `docker exec portal-download rm /data/backup.zip`.
- **Tags:** without a tag the image is `latest`, the last release. A tag `X.Y` such as `0.8` follows the fixes of one minor version, `X.Y.Z` such as `0.8.0` stays put, `edge` follows the main branch.
- **Upgrading:** take a backup first, because a store written by a newer version is refused by an older one. Then `docker pull ghcr.io/schlingensiepen/portal-download`, `docker stop portal-download`, `docker rm portal-download`, and the same `docker run` as at first start, with all its options. The volume keeps everything.
- **From 0.7 to 0.8:** version 0.8 reads no store and no backup of 0.7; it starts empty, as a new instance: setup with a new setup secret, new invitations for every person, new agent tokens. Backups of 0.7 are of no use to it. The steps:
  1. Stop the old container and give it another name, so that name and port are free: `docker stop portal-download && docker rename portal-download portal-download-0.7`.
  2. Start 0.8 with the `docker run` above on a new volume, `-v portal-download-0.8:/data` (in a compose file, a new volume name under `volumes:`). Keep the old volume; on it the new container refuses to start and names the store files.
  3. Bring the jobs over: with **Import logins (Firefox CSV)**, or by hand from the old instance. To look into it, run it once more, on its own volume but another port, while the 0.8 container serves 443: `docker rm portal-download-0.7`, then `docker run -d --name portal-download-0.7 -p 8443:443 -e SITE=<old SITE> -e TZ=Europe/Berlin -v portal-download:/data ghcr.io/schlingensiepen/portal-download:<old tag>`, where the old tag is the one you ran (`docker image ls ghcr.io/schlingensiepen/portal-download`), and open `https://<old SITE>:8443/`. Only one container may use a volume at a time.
  4. Invite everybody anew, and grant the agents new tokens. The new image creates its own certificate authority, so every computer imports it once more; each instance's authority carries a random part in its name, and its fingerprint tells them apart. Once nothing is missing, remove the old container and, when you are sure, its volume.
- **A certificate of your own** (from an intranet CA, for example): copy it in as the image's user, then restart:

  ```sh
  docker exec -i portal-download sh -c 'cat > /data/tls/cert.pem' < cert.pem   # with the chain, if any
  docker exec -i portal-download sh -c 'umask 077; cat > /data/tls/key.pem' < key.pem
  docker restart portal-download
  ```

  A certificate you put there is never replaced, and the image's authority is then not offered; the log warns when it does not match `SITE` or runs out within 30 days. Deleting both files makes the image issue its own again.
- **A host directory instead of the named volume** must belong to the image's user: `chown -R 10001:10001 <directory>`. Otherwise the container ends with exit code 4 and `docker logs` says why; with `--restart unless-stopped` Docker keeps starting it again until the directory is fixed. With rootless Docker or Podman the UID is mapped, and with SELinux the mount needs `:Z`.
- **Do not use `--network host`.** The service trusts proxy headers from the loopback interface, which is the container's own only while the container has its own network.
- **With WSLC or `container`,** check after the first start that the container runs (`wslc ps`, `container ls`). If it ended with exit code 4, the runtime created the volume with another owner; its log names the `chown` that is missing.

**With Docker Compose**, the same container as above:

```yaml
services:
  portal-download:
    image: ghcr.io/schlingensiepen/portal-download:0.8
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

**Behind a reverse proxy of your own**, for example Caddy as a second service in the stack: `PROXY=external` starts the service without the Caddy inside the image, with no certificate and no `SITE`. The service then listens on port 8080 of its container and takes scheme, host and client address from the `X-Forwarded-*` headers of whoever reaches it. Publish port 8080 nowhere, so that only the proxy in the same network reaches it. The proxy must send `X-Forwarded-Proto` and `X-Forwarded-Host` and set `X-Forwarded-For` itself; Caddy does all three by default, nginx sends no `X-Forwarded-Host` unless told to. It must pass `OPTIONS` requests to `/api/` on, which the browser extension sends first.

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
    image: ghcr.io/schlingensiepen/portal-download:0.8
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

Caddy sends the `X-Forwarded-*` headers on its own. The image's configuration for `PROXY=external` already sets `listen = "0.0.0.0:8080"` and `trust_forwarded_headers = true`; nothing needs to be mounted. When Caddy runs in another compose project, both must share a Docker network (an `external: true` network in both files), and Caddy reaches the service by its container name. The certificate in `./certs` is yours, from an intranet CA for example; the browser extension needs the computers to trust it. The ready block with the setup secret is in `docker compose logs portal-download`.

## Running it

Requirements: Rust (stable) to build, and HTTPS in front for anything other than `localhost`. Browsers allow the clipboard and `Secure` cookies only in a secure context. `http://localhost` and `http://127.0.0.1` count as secure, so a local trial needs no certificate.

```sh
cargo build --release
cp examples/config.toml my-config.toml
# set state_dir in my-config.toml to a directory you can write, for example ./state
./target/release/portal-download --config my-config.toml check   # judge the configuration
./target/release/portal-download --config my-config.toml serve   # run
```

The first start logs the setup secret. Open the service in a browser, enter the secret and your name, and accept the invitation in that browser. Then create the first download job. Invite the others under **People**.

Put a reverse proxy with TLS on the same host in front, see [`examples/Caddyfile`](examples/Caddyfile), and run the service as its own user, see [`examples/portal-download.service`](examples/portal-download.service). The service takes `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` only from a proxy on the same host (loopback), unless `trust_forwarded_headers = true`, and that proxy must set them itself rather than pass on what the browser sent (Caddy does by default). Details are in [docs/operating.md](docs/operating.md).

### Commands and exit codes

| Command | What it does |
|---|---|
| `serve` (default) | Runs the service; opens the setup window at start. |
| `check` | Prints every configuration value with its source, and whether the store is usable (JSON). |
| `status` | Prints counts of people, devices and jobs, the jobs due and the store's structure version (JSON). |
| `version` | Prints version and the source revision the binary was built from (JSON). |
| `backup FILE [--password-file PATH]` | Writes the full backup (a ZIP file, the same as `GET /backup`) to a new file, also while the service runs; with the first line of `PATH` as password it is encrypted. Never overwrites. |
| `bootstrap [--minutes N]` | Opens the setup window without a restart and prints the setup secret once. |

Exit codes: `0` success · `1` runtime error · `2` wrong arguments · `3` configuration invalid · `4` state directory or store unusable.

### Configuration

There is one TOML file with four keys, `listen`, `state_dir`, `trust_forwarded_headers` and `ca_certificate` (see [`examples/config.toml`](examples/config.toml)). The file is selected by `--config PATH`, then by `PORTAL_DOWNLOAD_CONFIG`, then from `$XDG_CONFIG_HOME/portal-download/config.toml` (usually `~/.config/…`), then from `/etc/portal-download/config.toml`. Without a file, `listen` is `127.0.0.1:8080` and `state_dir` is `$XDG_STATE_HOME/portal-download` (usually `~/.local/state/portal-download`). All values come from that one file, so it can be versioned and rolled back as a whole. An empty `listen` means the default `127.0.0.1:8080`. An empty `state_dir` aborts the start instead of silently writing somewhere else. `trust_forwarded_headers` is `false` by default: scheme, host and client address are taken from the `X-Forwarded-*` headers only when the request comes from the loopback interface, where the reverse proxy runs. `true` takes them from every sender; set it only when nothing but the reverse proxy reaches the service. `ca_certificate` names the PEM file of the authority that issued the reverse proxy's certificate; the service then offers it at `/ca.crt` and shows its fingerprint on the overview page. Leave it out when the computers trust the certificate already.

## For agents and scripts

The service describes itself. `GET /` without `Accept: text/html` returns a short plain-text entry page that links to the usage description (`/describe/usage`), the machine contract in OpenRPC form (`/describe/api`) and an anchor skill (`/describe/skill`). Every URL there is rendered with the host the request arrived on. The API is JSON-RPC 2.0 at `POST /api/`. An agent finds there how to register, where to keep its token (`PORTAL_DOWNLOAD_AGENT_TOKEN`) and the rules for handling it. `service.status` names the contract version (major and minor, such as `0.8`); a caller built for another one says so.

## Development

```sh
cargo test
```

The tests include:
- the scenarios in `contract-tests/`, which every backend of jobs passes, this service as well as the browser extension's own store;
- the checks that hold the service's self-description: every method has a description and examples; no delivered text carries hindsight, internal identifiers or foreign hosts; the usage description names no operation; every placeholder names the call that yields it;
- the rules for the building blocks and translations of the web application.

Each of these, and the tests for access, rights and roles, the exchange, sanitizing and the schedule, was seen to fail once with its safeguard removed. See [AGENTS.md](AGENTS.md) for how to work on this repository.

## License

Apache License 2.0, see [LICENSE](LICENSE).
