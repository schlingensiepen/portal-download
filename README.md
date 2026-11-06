# portal-download

An intranet workbench for a small, invited circle of people who regularly have to download invoices, statements and other documents from web portals.

The browser window is split in two. The **right quarter** is a work bar showing the most urgent download job: when it is due, the login with copy buttons, and a short Markdown instruction saying what to file where. **◀ ▶** pages through the jobs in order of urgency. **Run** opens the portal on the **left**, either embedded or in a window of its own beside the bar, because many portals forbid being embedded. When everything is downloaded, close the run with **Succeeded** or **Failed**. The next due date follows from that.

## How the schedule works

Every job has an interval in weeks and a retry period in days.

- **Urgency** = days from today to the next due date. Negative means overdue. The most urgent job comes first.
- **Succeeded:** next due date = the day it was closed + interval in weeks.
- **Failed:** next due date = the day it was closed + retry days (default 2). The run and its note are kept in the job's history.

## How the secrets are kept

- **Portal logins are sealed in the browser.** The circle shares one *vault passphrase*. The browser derives an AES-256-GCM key from it (PBKDF2-SHA-256, 600 000 iterations) and keeps that key, non-extractable, in IndexedDB. The service stores only sealed values, the salt and a sealed check value. It never sees the passphrase or the key, so a copy of the server or its backup reveals no portal password. The price: **if the passphrase is lost, the stored logins cannot be read any more.** If a browser clears its storage, enter the passphrase again.
- **Access to the service comes by invitation, with no passwords.** A person with the right to administer people creates an invitation link. It can be opened on each of the invited person's devices within seven days. Every device gets its own credential as an `HttpOnly; Secure; SameSite=Lax` cookie, renewed on every visit. The server stores only its hash. A lost device is revoked on its own, without touching the person's other devices.
- **Machines get a key for one task** (`job.list`, the due list without logins), never a person's access. Keys can only be created while the setup window is open, and they can be shut down at any time.
- **The setup window** is the file `bootstrap.toml` in the state directory. It holds a setup secret and an expiry 30 minutes ahead. Every start writes a fresh one, and `portal-download bootstrap` opens it without a restart. It is the only way to create the first person who administers people, or a new one when all of them have lost their devices. Nobody has to delete it; it closes by itself.

The service has **no start secret** of its own, so there is nothing to set before the first start.

## Running it

Requirements: Rust (stable) to build, and HTTPS in front for anything other than `localhost`. Browsers only allow WebCrypto, the clipboard and `Secure` cookies in a secure context.

```sh
cargo build --release
./target/release/portal-download --config examples/config.toml check   # judge the configuration
./target/release/portal-download --config examples/config.toml serve   # run
```

The first start logs the setup secret. Open the service in a browser, enter the secret and your name, and accept the invitation on that device. Then create the vault passphrase and the first download job. Invite the others under **Personen**.

Put a reverse proxy with TLS in front, see [`examples/Caddyfile`](examples/Caddyfile), and run the service as its own user, see [`examples/portal-download.service`](examples/portal-download.service). Details are in [docs/operating.md](docs/operating.md).

### Commands and exit codes

| Command | What it does |
|---|---|
| `serve` (default) | Runs the service; opens the setup window at start. |
| `check` | Prints every configuration value with its source, and whether the store is usable (JSON). |
| `status` | Prints counts of people, devices and jobs, the jobs due and the store's structure version (JSON). |
| `version` | Prints version and the source revision the binary was built from (JSON). |
| `bootstrap [--minutes N]` | Opens the setup window without a restart and prints the setup secret once. |

Exit codes: `0` success · `1` runtime error · `2` wrong arguments · `3` configuration invalid · `4` state directory or store unusable.

### Configuration

There is one TOML file with two keys, `listen` and `state_dir` (see [`examples/config.toml`](examples/config.toml)). The file is selected by `--config PATH`, then by `PORTAL_DOWNLOAD_CONFIG`, then from `~/.config/portal-download/config.toml`, then from `/etc/portal-download/config.toml`. All values come from that one file, so it can be versioned and rolled back as a whole. An empty `listen` means the default `127.0.0.1:8080`. An empty `state_dir` aborts the start instead of silently writing somewhere else.

## For agents and scripts

The service describes itself. `GET /` without `Accept: text/html` returns a short plain-text entry page that links to the usage description (`/describe/usage`), the machine contract in OpenRPC form (`/describe/api`) and an anchor skill (`/describe/skill`). Every URL there is rendered with the host the request arrived on. The API is JSON-RPC 2.0 at `POST /api/`.

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

Each of these was seen to fail once with its safeguard removed. See [AGENTS.md](AGENTS.md) for how to work on this repository.
