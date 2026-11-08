# Operating portal-download

## Layout and permissions

| What | Where | Written by | Read by |
|---|---|---|---|
| Binary | `/usr/local/bin/portal-download` | the operator | everyone |
| Configuration | `/etc/portal-download/config.toml` | the operator (root) | the service user |
| Store | `<state_dir>/portal-download.sqlite` (+ `-wal`, `-shm`) | the service | the service |
| Setup window | `<state_dir>/bootstrap.toml`, mode `0600` | the service at start, or `portal-download bootstrap` | the service |

The service writes only the state directory and never its configuration. Run the commands `check`, `status` and `bootstrap` as the service user, so the files they create belong to the service.

## First start

1. Write the configuration and run `portal-download check`. Every value is shown with its source.
2. Start the service. The log line `not set up yet. Setup secret: pds_…` names the secret and how long it is valid. Once somebody is set up, the start log names only the setup file, not the secret.
3. Open the service over its HTTPS address, enter the setup secret and your name. This creates the first person with the right to administer people and opens their invitation in that browser.
4. Create the first job with the portal's login. Every person of the circle sees the logins in plain text; agents see them only with the ability *Read jobs with logins*.

## Daily operation

- **Inviting:** under **People**, a person with the right to administer people creates the invitation. The link is shown once and is valid for seven days on any number of the invited person's devices. **New invitation** creates a fresh link for an existing person without creating a second one.
- **A lost device:** revoke that device, either the owner under **My devices** or an administering person under **People**. If the invitation may have leaked and is still open, withdraw it too. Devices that are already registered stay registered.
- **Somebody leaves:** deactivate the person. All their devices stop at the next request.
- **Locked out (all administering people lost their devices):** run `portal-download bootstrap` on the server, then call `setup.createAdmin` with the printed secret or open the setup form. The form appears in a browser without a device credential only while nobody is set up. With people already set up, use the API:

  ```sh
  curl -s https://downloads.intra.example/api/ -H 'Content-Type: application/json' \
    -H "X-Setup-Secret: $SECRET" \
    -d '{"jsonrpc":"2.0","id":1,"method":"setup.createAdmin","params":{"name":"Ada"}}'
  ```

  The response holds `invitation_token`. Open `https://downloads.intra.example/#invite=<invitation_token>` with the token from that response.
- **An agent or a script** works with an agent token in the header `Authorization: Bearer`, not with a person's cookie. It registers itself through the API and names an eight-letter display string. Under **People → Agents**, check that the display string and the introduction match what the agent said, and grant exactly one ability. Reject registrations you do not recognise. A script that only needs the due list gets *Read jobs*. The token lives in that machine's environment as `PORTAL_DOWNLOAD_AGENT_TOKEN`. Revoke it there at any time.

## Backup

The backup is one JSONL file: a head line, then one record per line (people, invitations, devices, agent tokens, jobs with their logins in plain text, runs). There are two ways to get it, and they write the same file:

- **With `curl`, from any machine:** under **People → Agents**, create a token with the right *Take the backup*. The application shows the complete line once, for example as a nightly cron job:

  ```sh
  curl -fsS -H "Authorization: Bearer pda_…" https://downloads.intra.example/backup -o portal-download-$(date +%F).jsonl
  ```

  When the service's certificate is not one the backup machine trusts, as with the one the container image creates, add `--cacert` with that certificate (see the README's container section). The image's own certificate changes every 90 days, so a nightly job takes it anew before each run.

  A person with the right to administer people can also click **Download backup** under **People**.
- **On the server:** `portal-download backup /some/dir/portal-download-$(date +%F).jsonl`. It never overwrites a file and makes it readable by its owner only.

**Restoring is adding.** Under **People → Backup**, a person with the right to administer people uploads the file. Nothing is overwritten. A job with the same title and entry address, or a person with the same name or e-mail, comes in a second time as "… (Import YYYY-MM-DD)". The application then lists those copies with a delete button each. *Leave people and accesses out* imports only jobs with their logins and their history. With people and accesses, a file brings in people with the right to administer people and working credentials. The application names those people after the import. **Import only a backup you took from this service yourself.** The file is checked line by line, and a faulty line rejects the whole import.

**On a fresh instance**, set up with a name such as "Setup", upload the backup with people and accesses, and delete "Setup" afterwards. Devices and tokens from the backup keep working when the instance runs under the same host name, because the browsers keep their device cookies per host name. Jobs come with their logins. A backup of another format version is refused with a message naming the version this service reads.

**The backup holds the portal logins in plain text**, and the hashes of all accesses. Protect the way to it: grant *Take the backup* (`backup.read`) only to a script you trust and revoke it when its purpose ends, download it only to a machine where only the people of the circle can read it, and keep the files there with owner-only permissions. `portal-download backup` writes its file readable by its owner only.

## In a container

The image `ghcr.io/schlingensiepen/portal-download` runs the service and Caddy together, or with `PROXY=external` the service alone behind a reverse proxy of the operator's; both starts are described in the README. Inside it:

| What | Where |
|---|---|
| Volume | `/data`, owned by UID/GID `10001`, which the image runs as; nothing in the container runs as root |
| State directory | `/data/portal-download` (store and setup file) |
| Certificate | `/data/tls/cert.pem` and `key.pem`. Created at first start for `SITE`, valid 120 days; `generated` beside it holds that certificate's fingerprint and `site` its name. Checked at start and once a day. Only a certificate with that fingerprint is created anew, when `SITE` changes or it runs out within 30 days, so every 90 days; the running container then starts Caddy anew, while the service keeps running. One the operator put there has another fingerprint and is never replaced, only warned about, at start and once a day. Not used with `PROXY=external`. |
| Caddy's working files | `/data/caddy`; Caddy's configuration is not persisted |
| Configuration | `/etc/portal-download/config.toml` in the image: `listen = "127.0.0.1:8080"`, reached by Caddy over the container's loopback interface. With `PROXY=external`, `/etc/portal-download/config-external.toml`: `listen = "0.0.0.0:8080"` and `trust_forwarded_headers = true` |

The entry point checks `PROXY` (exit code `3` for any value but `external`), then, without `PROXY`, `SITE` (exit code `3` when it is missing or neither a host name nor an IPv4 address) and that `/data` is writable (exit code `4`, with the `chown` that is missing), then starts the service, waits until it answers, and starts Caddy; with `PROXY=external` it starts only the service. `docker stop` reaches both. When either ends by itself, the container ends with a non-zero exit code, and `--restart unless-stopped` brings both back. A container that fails the checks at start is restarted the same way, again and again; `docker logs` names the cause. The health check asks the service; `docker ps` shows it. Caddy serves HTTP/1.1 and HTTP/2 on 443/tcp only, with no redirect from port 80. When the certificate is renewed, browsers ask once more to accept it; a page that is open reports a failed connection until it is reloaded.

Each image was built for its revision (`portal-download version`) and passed a smoke test on its platform before it was published: start through Caddy, revision, first setup with the logged secret, a stop within ten seconds with exit code 0, a new container on the same volume with the same person and certificate, the backup fetched with a token, the renewal of a certificate that runs out soon in the running container, the start with `PROXY=external` and its trust in the proxy's headers, and the refusal of an unwritable volume, a missing `SITE` and an unknown `PROXY`.

## Upgrading

Replace the binary and restart. In a container, pull the new image and start a new container on the same volume. A newer version migrates the store at start, before it listens, and records the structure version (`portal-download status`). An older binary refuses a store written by a newer one.
