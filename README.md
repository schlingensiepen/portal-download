# portal-download

An intranet workbench for a small, invited circle of people who regularly have to download invoices, statements and other documents from web portals.

The browser window is split in two. The **right quarter** is a work bar showing the most urgent download job: when it is due, the login with copy buttons, and a short Markdown instruction saying what to file where. **◀ ▶** pages through the jobs in order of urgency. **Run** opens the portal on the **left**, either embedded or in a window of its own beside the bar, because many portals forbid being embedded. When everything is downloaded, close the run with **Succeeded** or **Failed**. The next due date follows from that.

## Languages

The application is in English and follows the browser's language where a translation exists; German is included. The choice can be changed at the top of the work bar and is kept in that browser. A translation is one file, `static/i18n/<code>.json`, listed in `static/config.json`. API, self-description and this documentation are in English. Messages from the API appear in English in every language.

## How the schedule works

"Today" is the date in the server's local time zone. Run the service in the circle's time zone (`TZ=Europe/Berlin` in a container) so that jobs become due at local midnight.

Every job has an interval in weeks and a retry period in days.

- **Urgency** = days from today to the next due date. Negative means overdue. The most urgent job comes first.
- **Succeeded:** next due date = the day it was closed + interval in weeks.
- **Failed:** next due date = the day it was closed + retry days (default 2). The run and its note are kept in the job's history.

## How the secrets are kept

- **Portal logins are sealed in the browser.** The circle shares one *vault passphrase*. The browser derives an AES-256-GCM key from it (PBKDF2-SHA-256, 600 000 iterations) and keeps that key, non-extractable, in IndexedDB. The service stores only sealed values, the salt and a sealed check value. It never sees the passphrase or the key, so a copy of the server or its backup reveals no portal password. The price: **if the passphrase is lost, the stored logins cannot be read any more.** If a browser clears its storage, enter the passphrase again. The vault is optional: jobs work without it, but then hold no logins. The exact sealed format is stated in the API contract at `vault.create`.
- **Access to the service comes by invitation, with no passwords.** A person with the right to administer people creates an invitation link. It can be opened on each of the invited person's devices within seven days. Every device gets its own credential as an `HttpOnly; Secure; SameSite=Lax` cookie, renewed on every visit. The server stores only its hash. A lost device is revoked on its own, without touching the person's other devices.
- **Agents work with a token for one ability**, never with a person's access. An agent registers itself with a short introduction and receives a token without any ability plus an eight-letter display string. It tells its user only the display string. A person with the right to administer people then grants exactly one ability under **People**: reading jobs (`jobs.read`), changing jobs (`jobs.write`), closing runs (`jobs.complete`), or taking the backup (`backup.read`). They can also create a token directly for a script that cannot register itself. No agent sets a portal login or administers people, devices, the vault or tokens. Only the backup ability receives the logins, sealed as stored. A token nobody grants expires after 24 hours, a granted one after 180 days without use, and any token can be revoked at once. At most ten registrations wait at the same time.
- **The setup window** is the file `bootstrap.toml` in the state directory. It holds a setup secret and an expiry 30 minutes ahead. Every start writes a fresh one, and `portal-download bootstrap` opens it without a restart. It is the only way to create the first person who administers people, or a new one when all of them have lost their devices. Nobody has to delete it; it closes by itself.

The service has **no start secret** of its own, so there is nothing to set before the first start.

## Backup

`curl -fsS -H "Authorization: Bearer <token>" https://<host>/backup -o backup.jsonl` fetches everything as one JSONL file. The token is one with the ability `backup.read` (*Take the backup* in the application). When a person creates such a token in the application, it shows this line with the token filled in; over the API, `agent.create` returns the token for the same line. Uploading a backup in the application adds it to what is there and overwrites nothing. Details are in [docs/operating.md](docs/operating.md#backup).

## Running it

Requirements: Rust (stable) to build, and HTTPS in front for anything other than `localhost`. Browsers allow WebCrypto, the clipboard and `Secure` cookies only in a secure context. `http://localhost` and `http://127.0.0.1` count as secure, so a local trial needs no certificate.

```sh
cargo build --release
cp examples/config.toml my-config.toml
# set state_dir in my-config.toml to a directory you can write, for example ./state
./target/release/portal-download --config my-config.toml check   # judge the configuration
./target/release/portal-download --config my-config.toml serve   # run
```

The first start logs the setup secret. Open the service in a browser, enter the secret and your name, and accept the invitation on that device. Then create the vault passphrase and the first download job. Invite the others under **People**.

Put a reverse proxy with TLS on the same host in front, see [`examples/Caddyfile`](examples/Caddyfile), and run the service as its own user, see [`examples/portal-download.service`](examples/portal-download.service). The service takes `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` only from a proxy on the same host (loopback), and that proxy must set them itself rather than pass on what the browser sent (Caddy does by default). Details are in [docs/operating.md](docs/operating.md).

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

There is one TOML file with two keys, `listen` and `state_dir` (see [`examples/config.toml`](examples/config.toml)). The file is selected by `--config PATH`, then by `PORTAL_DOWNLOAD_CONFIG`, then from `$XDG_CONFIG_HOME/portal-download/config.toml` (usually `~/.config/…`), then from `/etc/portal-download/config.toml`. Without a file, `listen` is `127.0.0.1:8080` and `state_dir` is `$XDG_STATE_HOME/portal-download` (usually `~/.local/state/portal-download`). All values come from that one file, so it can be versioned and rolled back as a whole. An empty `listen` means the default `127.0.0.1:8080`. An empty `state_dir` aborts the start instead of silently writing somewhere else.

## For agents and scripts

The service describes itself. `GET /` without `Accept: text/html` returns a short plain-text entry page that links to the usage description (`/describe/usage`), the machine contract in OpenRPC form (`/describe/api`) and an anchor skill (`/describe/skill`). Every URL there is rendered with the host the request arrived on. The API is JSON-RPC 2.0 at `POST /api/`. An agent finds there how to register, where to keep its token (`PORTAL_DOWNLOAD_AGENT_TOKEN`) and the rules for handling it.

## Embedding portals

Most portals send `X-Frame-Options` or a `frame-ancestors` policy, and the browser then refuses to show them inside another page. Each job therefore has a switch **may be embedded**:

- **On:** the portal loads in a sandboxed frame on the left. The frame may run scripts, submit forms and download, but it may not navigate the workbench away.
- **Off (the default):** **Run** opens the portal in a window of its own, about three quarters of the screen wide, with no link back to the workbench.

Even when embedding is allowed, a portal's login cookies count as third-party cookies inside a frame, and some browsers block them. If a portal logs you out inside the frame, switch the job to a window of its own.

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
