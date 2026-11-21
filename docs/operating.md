# Operating portal-download

## Layout and permissions

| What | Where | Written by | Read by |
|---|---|---|---|
| Binary | `/usr/local/bin/portal-download` | the operator | everyone |
| Configuration | `/etc/portal-download/config.toml` | the operator (root) | the service user |
| Store | `<state_dir>/portal-download.sqlite` (+ `-wal`, `-shm`) | the service | the service |
| Setup window | `<state_dir>/bootstrap.toml`, mode `0600` | the service at start, or `portal-download bootstrap` | the service |

The service writes only the state directory and never its configuration. Run the commands `check`, `status`, `backup` and `bootstrap` as the service user, so the files they create belong to the service.

## First start

1. Write the configuration and run `portal-download check`. Every value is shown with its source.
2. Start the service. The log line `not set up yet. Setup secret: pds_…` names the secret and how long it is valid. Once somebody is set up, the start log names only the setup file, not the secret.
3. Open the service over its HTTPS address, enter the setup secret and your name. This creates the first person, with the rights to manage people, agents and the backup, and opens their invitation in that browser.
4. Create the first job with the portal's login, or import the logins Firefox exports. Every person of the circle sees the logins in plain text; agents see them only with the role *Work through jobs* (`jobs.work`) or *Everything on jobs* (`jobs.all`).

## Daily operation

- **Inviting:** under **People**, a person who manages people creates the invitation and ticks the rights the new person gets, among those the inviting person holds. The link is shown once and is valid for seven days in any number of the invited person's browsers and browser extensions. **New invitation** creates a fresh link for an existing person without creating a second one. Who invited whom stays visible.
- **Rights:** under **People**, *Set rights* changes the three rights of a person. Only rights you hold can be given or taken away, and one active person keeps all three. A new invitation, deactivating, deleting and revoking a device of a person need every right that person holds.
- **A lost device:** revoke that device, either the owner under **My devices** or a person who manages people under **All devices**. If the invitation may have leaked and is still open, withdraw it too. Devices that are already registered stay registered.
- **The browser extension:** a person adds it with an invitation link, or with *Add to the browser extension* under **My devices**, which creates a link that serves once within the hour. It is a device like a browser and is revoked the same way. It reaches the service only when the computer trusts the service's certificate (see the README, *Browser extension*). The API answers its requests from the origins `moz-extension://…` and `chrome-extension://…` with CORS headers, without credentials, because it sends a bearer token and never a cookie; a reverse proxy in front must pass `OPTIONS` requests to `/api/` on.
- **Connections:** the service connects to nothing; the browsers and the extension reach the portals themselves, through whatever proxy they are set up with.
- **Somebody leaves:** deactivate the person. All their devices stop at the next request.
- **Locked out (every person with the right to manage people lost their devices):** run `portal-download bootstrap` on the server, then call `setup.createAdmin` with the printed secret or open the setup form. The form appears in a browser without a device credential only while nobody is set up. With people already set up, use the API:

  ```sh
  curl -s https://downloads.intra.example/api/ -H 'Content-Type: application/json' \
    -H "X-Setup-Secret: $SECRET" \
    -d '{"jsonrpc":"2.0","id":1,"method":"setup.createAdmin","params":{"name":"Ada"}}'
  ```

  The response holds `invitation_token`. Open `https://downloads.intra.example/#invite=<invitation_token>` with the token from that response. The new person holds all three rights.
- **An agent or a script** works with an agent token in the header `Authorization: Bearer`, not with a person's cookie. It registers itself through the API and names an eight-letter display string. Under **Agents**, check that the display string and the introduction match what the agent said, and grant exactly one role. Reject registrations you do not recognise. A script that only needs the due list gets *Read jobs* (`jobs.read`); an agent that logs into the portals gets *Work through jobs* (`jobs.work`), which lists the jobs with their logins and closes runs; one that keeps the list gets *Manage jobs* (`jobs.manage`), which sets logins without seeing them. A token that manages people can invite, and is as strong as a person with that right, since it could open the invitation itself. A token that manages agents grants exactly the roles the person who granted it could at the time of the grant; when that person loses a right, revoke such tokens. It grants only display strings its user named. The token lives in that machine's environment as `PORTAL_DOWNLOAD_AGENT_TOKEN`. Revoke it under **Agents** at any time; the tokens it granted keep working until revoked themselves.

## Backup

The full backup is a ZIP file with one entry, `portal-download.jsonl`: a head line, then one record per line (jobs with their logins in plain text, runs, deleted jobs, people, invitations, devices, agent tokens with the hashes of their credentials). With a password it is encrypted with AES-256; the service reads and writes no other ZIP encryption. There are three ways to get it, and they write the same file:

- **With `curl`, from any machine:** under **Agents**, create a token with the role *Take the backup*. The application shows the complete line once, for example as a nightly cron job:

  ```sh
  umask 077
  curl -fsS -H "Authorization: Bearer pda_…" -H @/root/backup-header https://downloads.intra.example/backup -o portal-download-$(date +%F).zip
  ```

  `/root/backup-header` holds the one line `X-Backup-Password: <password>`; leave `-H @…` out for a file without a password. The password travels only in that header, never in the address.

  When the backup machine does not trust the service's certificate, as with the one the container image issues, add `--cacert ca.crt` with the image's certificate authority from `https://<host>/ca.crt`; it stays the same when the certificate is renewed.
- **In the application:** under **Exchange**, a person who manages the backup downloads it, with or without a password.
- **On the server:** `portal-download backup /some/dir/portal-download-$(date +%F).zip --password-file /root/backup-password`, where the first line of that file is the password; without `--password-file` the file is not encrypted. It never overwrites a file and makes it readable by its owner only.

**Reading in is adding and merging.** Under **Exchange → Read a file in**, every person can read the jobs of any such file in, also of an export of the jobs or of the browser extension. A preview sorts the file's jobs into new, the same, and those that need a decision; nothing is read in before you confirm, and no run is lost. A person with all three rights can tick that the file's people, invitations, devices and agent tokens come in too: they are added beside the existing ones, and a person with the name or e-mail of one here comes in with "(Import YYYY-MM-DD)" after the name. **Read people in only from a backup you took from this service yourself:** it brings rights and working credentials. The file is checked line by line, and a faulty line rejects the whole file.

**On a fresh instance**, set up with a name such as "Setup", read the backup in with people and accesses, and delete "Setup" afterwards. Devices and tokens from the backup keep working when the instance runs under the same host name, because the browsers keep their device cookies per host name and the extension its tokens per address. A file of another format version is refused with a message naming the version this service reads; files of version 0.7 are not read.

**A file without a password holds the portal logins in plain text**, and the full backup the hashes of all accesses. Protect the way to it: grant *Take the backup* (`backup.read`) only to a script you trust and revoke it when its purpose ends, keep the files where only the people of the circle can read them, with owner-only permissions, or set a password.

## In a container

The image `ghcr.io/schlingensiepen/portal-download` runs the service and Caddy together, or with `PROXY=external` the service alone behind a reverse proxy of the operator's; both starts are described in the README. Inside it:

| What | Where |
|---|---|
| Store and setup file | `/data/portal-download/` (the volume) |
| Certificate authority | `/data/tls/ca.pem`, `/data/tls/ca.key` (mode `0600`); the copy offered at `/ca.crt` in `/data/tls/public/ca.crt` |
| Certificate | `/data/tls/cert.pem` (the certificate and the authority's), `/data/tls/key.pem` (mode `0600`) |
| Caddy's working files | `/data/caddy/`, `/config/caddy/` |
| Configuration | `/etc/portal-download/config.toml`, with `PROXY=external` `/etc/portal-download/config-external.toml` |

The entry point checks `PROXY` (exit code `3` for any value but `external`), then, without `PROXY`, `SITE` (exit code `3` when it is missing or neither a host name nor an IPv4 address) and that `/data` is writable (exit code `4`, with the `chown` that is missing), then starts the service, waits until it answers, and starts Caddy; with `PROXY=external` it starts only the service. `docker stop` reaches both. When either ends by itself, the container ends with a non-zero exit code, and `--restart unless-stopped` brings both back. A container that fails the checks at start is restarted the same way, again and again; `docker logs` names the cause. The health check asks the service; `docker ps` shows it. Caddy serves HTTP/1.1 and HTTP/2 on 443/tcp only, with no redirect from port 80, and writes only warnings and errors to the log. A handshake for another name than `SITE`, or for none, gets the certificate of `SITE`, so the browser warns about the name, and the request reaches the service like one for `SITE`. There is no redirect to `SITE`, since a client that cannot resolve `SITE` could not follow it. Links the service creates, such as invitation links, name the address of the request.

**Certificates:** at first start the entry point creates a certificate authority for `SITE`, valid for ten years and limited by a name constraint to `SITE` (to that one address when `SITE` is an IP address), and with it the certificate the browsers see, valid for 120 days. Once a day it checks them: the certificate is issued anew 30 days before it runs out, by the same authority, and Caddy restarts with it while the service keeps running; the authority is replaced when `SITE` changes or a year before it runs out. A computer that trusts the authority accepts every renewal without asking. A certificate the operator put into `/data/tls` (another one than the image issued) is never replaced; the log warns when it no longer fits `SITE` or runs out within 30 days, and the image's authority is then not offered at `/ca.crt`. The authority's key lies in the volume, readable by the image's user only: whoever has the volume can issue certificates for `SITE`.

Once everything answers (with Caddy: once Caddy completes a TLS handshake; with `PROXY=external`: once the service answers), the entry point writes the ready block to the log, each line beginning with `portal-download container:`. It is framed by a line of 64 dashes and a line of 64 equals signs; while a setup is needed, a line of 64 stars and the heading `*  SETUP NEEDED: …` stand above the dashes, otherwise the first line after the dashes is `   READY: set up, N people, M jobs`. Then labelled lines: `Open:` with the address; with Caddy `Certificate authority:` with its SHA-256 fingerprint and where to download it (or `Certificate:` with the certificate's fingerprint, for a certificate of the operator's); with `PROXY=external` `Proxy target:` with how the reverse proxy reaches the service, by the container's name or its compose service name, for example `http://portal-download:8080/`; and while a setup is needed `Setup secret:` with its expiry. After the equals signs comes the line `portal-download <version> is ready: <address>`, `https://SITE/` with Caddy, and with `PROXY=external` `port 8080 of this container (PROXY=external)`. A reader of the log can stop at the first line containing ` is ready: `. A certificate created later, on renewal, is logged with its fingerprint on its own line.

Each image was built for its revision (`portal-download version`) and passed a smoke test on its platform before it was published: start through Caddy with the framed ready block, no log lines of Caddy at level info, the certificate authority at `/ca.crt` with its name constraint and its fingerprint in the block, a client that trusts it accepting the served certificate, the certificate of `SITE` and the service under another name and the bare IP address, the preflight of the browser extension, revision, first setup with the logged secret, a stop within ten seconds with exit code 0, a new container on the same volume with the same person, certificate and authority and a ready block that says so, the backup fetched with a token as a ZIP file and encrypted with a password, the renewal of a certificate that runs out soon by the same authority in the running container, a certificate of the operator's kept and the authority then not offered, the start with `PROXY=external`, its ready line and its trust in the proxy's headers, and the refusal of an unwritable volume, a missing `SITE` and an unknown `PROXY`.

## Upgrading

Replace the binary and restart. In a container, pull the new image and start a new container on the same volume. An older binary refuses a store written by a newer one, and the store records its structure version (`portal-download status`).

Version 0.8 starts with an empty store: it refuses a store of 0.7 and names what to do. Stop the service, move `portal-download.sqlite` with its `-wal` and `-shm` files out of the state directory (in a container: start on a new volume), start again and import the logins anew, from Firefox or by hand; files of 0.7 cannot be read in. The image's new certificate authority is imported once more.
