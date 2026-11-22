# Reference

This page is for operators, for agents and scripts, and for developers. It lists facts; the other pages say how to use them.

## Versions

Three kinds of version matter. They change at different times.

| Kind | Where you see it | In 0.9.0 | What it decides |
|---|---|---|---|
| **Program version**, also the image tag | `portal-download version`, the ready line, `service.status` → `version` | `0.9.0` | Which program runs. |
| **Interface version**, also called contract version | `service.status` → `contract` | `0.8` | Whether a caller and the service understand each other. The browser extension compares it with its own. |
| **Store structure version** | `portal-download status` → `store.schema_version` | `10` | Whether the program can open a store. It refuses a store of another structure. |

Versions 0.8 and 0.9 share the interface version and the store structure. So a 0.9 service opens a store of 0.8, and extensions of 0.8 and 0.9 work with it.

Exchange files have a format version of their own. Versions 0.8 and 0.9 write and read format 3. Files of 0.7 are not read.

**Image tags** of `ghcr.io/schlingensiepen/portal-download`:

| Tag | Follows |
|---|---|
| `0.9.0` | nothing; it stays put |
| `0.9` | the fixes of the minor version 0.9 |
| `latest` | the last release; also what you get without a tag |
| `edge` | the main branch, between releases |

Use `0.9` or a fixed version. With `latest`, a restart can bring a new minor version without warning.

## The ready block

The container writes the ready block to its log once everything answers. Each line begins with `portal-download container:`.

```text
portal-download container: ****************************************************************
portal-download container: *  SETUP NEEDED: nobody is set up yet
portal-download container: ----------------------------------------------------------------
portal-download container:    Open:                   https://downloads.intra.example/
portal-download container:    Certificate authority:  SHA-256 <FINGERPRINT>
portal-download container:                            trust it once: https://downloads.intra.example/ca.crt (compare first)
portal-download container:    Setup secret:           pds_<SECRET>  (valid until <YYYY-MM-DD>T<hh:mm:ss>Z; enter it in the browser)
portal-download container: ================================================================
portal-download container: portal-download 0.9.0 is ready: https://downloads.intra.example/
```

**When it says SETUP NEEDED.** The block says `SETUP NEEDED` exactly while no active person holds the right to manage people. Then nobody could invite anybody, and the setup secret is the way in. There are two cases:

| Heading | When |
|---|---|
| `SETUP NEEDED: nobody is set up yet` | There is no active person at all, as on a fresh instance. |
| `SETUP NEEDED: nobody manages people` | There are active people, but none of them holds the right to manage people. |

In both cases a line of stars and the heading stand above the dashes, and the block shows the setup secret. After the secret stands how to use it. While the service has no people at all, that is `enter it in the browser`. Otherwise it is `for setup.createAdmin, see docs/operating.md`; [operating.md](operating.md#locked-out) has the steps.

In every other case the block begins with the dashes and shows no secret:

```text
portal-download container: ----------------------------------------------------------------
portal-download container:    READY: set up, 3 people, 12 jobs
```

The lines of the block:

| Line | Meaning |
|---|---|
| `Open:` | The address to open. With `PROXY=external` it reads `the address of your reverse proxy`. |
| `Certificate authority:` | The SHA-256 fingerprint of the image's authority, and where to download it. |
| `Certificate:` | Shown instead when you put a certificate of your own into the volume: its fingerprint. |
| `Proxy target:` | Shown instead with `PROXY=external`: how your proxy reaches the service, `http://portal-download:8080/`. The host is the container's or the compose service's name. |
| `Setup secret:` | The secret and the time until which it is valid, in UTC. Only while a setup is needed. |
| the last line | `portal-download <version> is ready: <address>`. With `PROXY=external` the address reads `port 8080 of this container (PROXY=external)`. |

A script that reads the log can stop at the first line that contains ` is ready: `.

The setup secret is valid for 30 minutes from the start. `docker exec portal-download portal-download bootstrap` opens a new window and prints a new secret.

## The container

**Start checks and exit codes:**

| Check | If it fails |
|---|---|
| `PROXY` is unset or `external` | exit code 3 |
| Without `PROXY`: `SITE` is set and is a host name or an IPv4 address | exit code 3 |
| `/data` is writable for the image's user | exit code 4; the log names the missing `chown` |
| The certificate can be created or read | exit code 4 |

The log names the cause in each case. With `--restart unless-stopped`, a container that fails a check is started again and again until the cause is fixed.

**While it runs:**

- The entry point starts the service, waits until it answers, and then starts Caddy. With `PROXY=external` it starts only the service.
- `docker stop` reaches both. When one of them ends by itself, the container ends with a non-zero exit code, and the restart policy brings both back.
- The health check asks the service. `docker ps` shows the result.
- Caddy serves HTTP/1.1 and HTTP/2 on 443/tcp. It writes only warnings and errors to the log.
- A request for another name than `SITE`, or for none, gets the certificate of `SITE` and reaches the service. There is no redirect to `SITE`.
- The container runs as user `10001`, never as root. `docker exec` runs as that user too.

**Files in the volume:**

| What | Where |
|---|---|
| Store and setup file | `/data/portal-download/` |
| Certificate authority | `/data/tls/ca.pem`, `/data/tls/ca.key` |
| The copy offered at `/ca.crt` | `/data/tls/public/ca.crt` |
| Certificate and key | `/data/tls/cert.pem`, `/data/tls/key.pem` |
| Caddy's working files | `/data/caddy/` |

The configuration is part of the image: `/etc/portal-download/config.toml`, and with `PROXY=external` `/etc/portal-download/config-external.toml`.

## Commands and exit codes

In a container, put `docker exec portal-download` in front, for example `docker exec portal-download portal-download status`.

| Command | What it does |
|---|---|
| `portal-download serve` (the default) | Runs the service. Every start opens the setup window. |
| `portal-download check` | Prints every configuration value with its source, and whether the store is usable (JSON). |
| `portal-download status` | Prints the counts of people, devices and jobs, the jobs due and the store's structure version (JSON). |
| `portal-download version` | Prints the version and the source revision the program was built from (JSON). |
| `portal-download backup FILE [--password-file PATH]` | Writes the full backup to a new file, also while the service runs. The first line of `PATH` is the password. It never overwrites. |
| `portal-download bootstrap [--minutes N]` | Opens the setup window without a restart and prints the setup secret once. `N` is 1 to 1440; the default is 30. |

Every command takes `--config PATH`.

| Exit code | Meaning |
|---|---|
| `0` | success |
| `1` | runtime error |
| `2` | wrong arguments |
| `3` | configuration invalid |
| `4` | state directory or store unusable |

## Configuration

Outside the container, the service reads one TOML file. The file is chosen in this order:

1. `--config PATH`
2. the environment variable `PORTAL_DOWNLOAD_CONFIG`
3. `$XDG_CONFIG_HOME/portal-download/config.toml`, usually `~/.config/portal-download/config.toml`
4. `/etc/portal-download/config.toml`

Without a file, every value is its default. All values come from that one file; [`examples/config.toml`](../examples/config.toml) is a commented example.

| Key | Default | Meaning |
|---|---|---|
| `listen` | `127.0.0.1:8080` | Address and port the service listens on. Empty means the default. |
| `state_dir` | `$XDG_STATE_HOME/portal-download`, usually `~/.local/state/portal-download` | Directory for the store and the setup file. An empty value aborts the start. |
| `trust_forwarded_headers` | `false` | `false`: the `X-Forwarded-*` headers count only from the loopback interface. `true`: they count from every sender. Set `true` only when nothing but the reverse proxy reaches the service. |
| `ca_certificate` | none | PEM file of the authority that issued the proxy's certificate. The service then offers it at `/ca.crt` and shows its fingerprint. |

An unknown key is an error. `portal-download check` shows every value with its source.

**Files on a host:**

| What | Where | Written by |
|---|---|---|
| Program | for example `/usr/local/bin/portal-download` | the operator |
| Configuration | for example `/etc/portal-download/config.toml` | the operator |
| Store | `<state_dir>/portal-download.sqlite`, with `-wal` and `-shm` files | the service |
| Setup window | `<state_dir>/bootstrap.toml`, mode `0600` | the service at start, or `portal-download bootstrap` |

The service writes only the state directory, never its configuration. Run `check`, `status`, `backup` and `bootstrap` as the service user, so that the files they create belong to the service.

## The setup window

The setup window is the file `bootstrap.toml` in the state directory. It holds a setup secret and the time at which it closes.

- Every start writes a fresh window of 30 minutes. `portal-download bootstrap` opens one without a restart.
- The secret creates a person with all three rights, through the setup form or the method `setup.createAdmin`.
- The setup form in the browser appears only while the service has no people at all.
- Nobody has to delete the file. The window closes by itself.
- The service has no other start secret. There is nothing to set before the first start.

## Addresses of the service

| Address | What it serves |
|---|---|
| `GET /` | For a browser, the web application. Without `Accept: text/html`, a short plain-text entry page. |
| `POST /api/` | The API, JSON-RPC 2.0. |
| `GET /describe/usage` | Who may do what, and the duties of a caller (plain text). |
| `GET /describe/api` | Every method with parameters, result and an example (OpenRPC, JSON). |
| `GET /describe/skill` | A short skill file that points to the pages above. |
| `GET /backup` | The full backup as a ZIP file. |
| `GET /ca.crt` | The certificate authority, if the service offers one (DER form). Otherwise 404. |
| `GET /healthz` | The text `running`. |

Credentials travel in headers only, never in an address:

| Header | Carries |
|---|---|
| the cookie `pd_device` | a browser's access (`HttpOnly; Secure; SameSite=Lax`) |
| `Authorization: Bearer pdd_…` | the browser extension's access |
| `Authorization: Bearer pda_…` | an agent token |
| `X-Setup-Secret` | the setup secret, `pds_…` |
| `X-Backup-Password` | the password of an exported file, or of a file that is read in |

**The browser extension and CORS.** The API answers requests from the origins `moz-extension://…` and `chrome-extension://…` with CORS headers, and without credentials. The extension sends a bearer token, never a cookie. It first sends an `OPTIONS` request to `/api/`; a reverse proxy in front must pass that on.

## For agents and scripts

The service describes itself. An agent starts at the base address:

```sh
curl -s --cacert ca.pem https://downloads.intra.example/
```

The entry page links the usage description, the API contract and the skill file. Every address in them names the host the request arrived on.

- **Calls** go to `POST /api/` as JSON-RPC 2.0 with `Content-Type: application/json`.
- **Application errors** come with HTTP status 200 and an `error` object. `error.data.type` is a stable identifier, and `error.message` says what to do next.
- **A request the API cannot read** gets another status: 400 for a body that is not JSON, 405 for a method other than `POST`, 413 for a body that is too large.
- **`GET /backup`** is not JSON-RPC. It answers an error with the fitting HTTP status, such as 401 or 403, and the reason as plain text.
- **A token** is obtained by registering (`agent.register`), or a person creates one. [operating.md](operating.md#agents) describes both.
- **The token's place** is the environment variable `PORTAL_DOWNLOAD_AGENT_TOKEN`.
- **The interface version** is in `service.status` → `contract`. A caller built for another one says so to its user.

One call as an example:

```sh
curl -s --cacert ca.pem https://downloads.intra.example/api/ \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $PORTAL_DOWNLOAD_AGENT_TOKEN" \
  -d '{"jsonrpc":"2.0","id":1,"method":"job.list","params":{}}'
```

The methods, by group. `/describe/api` of your instance has the parameters and an example for each.

| Group | Methods |
|---|---|
| Service | `service.status`, `setup.createAdmin` |
| Jobs | `job.list`, `job.get`, `job.history`, `job.create`, `job.update`, `job.delete`, `job.complete` |
| Exchange | `job.export`, `job.importPreview`, `job.import`, `backup.export`, `backup.importPreview`, `backup.import` |
| People | `user.list`, `user.create`, `user.reinvite`, `user.setRights`, `user.setActive`, `user.delete` |
| Invitations and devices | `invitation.redeem`, `invitation.createOwn`, `invitation.withdraw`, `device.list`, `device.listAll`, `device.revoke` |
| Agents | `agent.register`, `agent.list`, `agent.grant`, `agent.create`, `agent.revoke` |

Limits worth knowing:

| What | Limit |
|---|---|
| An invitation | 7 days; a link for a further device of one's own: once, within 60 minutes |
| A registration nobody grants | 24 hours; at most 10 wait at the same time |
| A granted token | 180 days without use |
| The setup window | 30 minutes |
| A password for a file | 8 to 200 characters; letters A to Z and a to z, digits, punctuation and spaces; no umlauts or other non-ASCII characters; no space at either end |

## For developers

```sh
cargo test
```

The tests include the scenarios in [`contract-tests/`](../contract-tests/README.md). Every backend of jobs passes them: this service and the browser extension's local store. [AGENTS.md](../AGENTS.md) says how to work on the repository.

A translation of the web application is one file, `static/i18n/<code>.json`, listed in `static/config.json`.
