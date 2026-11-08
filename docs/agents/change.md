# Changing the service

## Order of work

1. Read the module map below. Find the one place your change belongs to.
2. Change the operation in `src/store.rs` first, then the pass-through in `src/rpc.rs`, then the browser application in `static/index.html`.
3. Run `cargo test`. When you add a check, remove its safeguard once and confirm the check fails, then restore the safeguard.
4. Handle every pairing listed below in the same change.
5. Done means: `cargo test` is green, `cargo build` has no warnings, and the browser flow (setup → invitation → vault → job → run → close) still works against a running instance.

## Module map

| File | Holds |
|---|---|
| `src/main.rs` | Command tree and exit codes; `serve` opens the setup window and listens. |
| `src/config.rs` | Configuration: one file selected by `--config`, `PORTAL_DOWNLOAD_CONFIG`, user-level place, system-level place. Every value reports its source. |
| `src/store.rs` | The store and **every operation**. Operations know nothing of HTTP; they take arguments and return a result or an `OpError`. |
| `src/rpc.rs` | The method table (the single source for dispatch, access and the machine contract) and the JSON-RPC pass-through. |
| `src/http.rs` | Routes, content negotiation, JSON-RPC envelope, cookie, security headers. |
| `src/describe.rs` | Entry page, usage description, anchor skill and OpenRPC contract, rendered with the requesting host. |
| `src/bootstrap.rs` | The setup window file. |
| `src/schedule.rs` | Next due date and urgency. |
| `static/index.html` | The whole browser application: every view, dialog, row and message as a `<template>` in English, and the script that clones, fills and binds them. No data, no third-party code. |
| `static/i18n/<code>.json` | The texts of one further language, by key. Listed in `static/config.json`, embedded through `LANGUAGES` in `src/http.rs`. |
| `src/frontend_check.rs` | Tests that hold the building-block and translation rules below. |
| `deploy/Dockerfile`, `deploy/start.sh`, `deploy/Caddyfile`, `deploy/config.toml`, `deploy/config-external.toml` | The container image: the service and Caddy in one container, the entry point that checks `PROXY`, `SITE` and the volume, creates and renews the certificate and supervises the processes; with `PROXY=external` the service alone behind a proxy of the operator's. |
| `deploy/smoke.sh`, `deploy/smoke-red.sh` | The smoke test every image passes before it is published, and the proof that each of its steps fails with its safeguard removed. |
| `.github/workflows/` | `ci.yml` (tests, build without warnings, shellcheck), `build-image.yml` (both platforms, smoke test, publish to ghcr.io), `smoke-red.yml` (the proof above, on changes to `deploy/`). |

## Rules that hold

- **Every action exists once, in `src/store.rs`.** `src/rpc.rs` only parses params, calls one operation and returns its result. A surface that needs something no operation offers gets a new operation.
- **Access is checked in one place** (`admit` in `src/rpc.rs`), before any identifier in the params is resolved, so an error message never reveals whether an object exists. Where an operation checks ownership itself (`device.revoke`), an object the caller may not touch is answered exactly like a missing one.
- **Store errors reach the caller without detail;** the detail goes to the service log only.
- **One API address:** `POST /api/` with an unmodified JSON-RPC 2.0 envelope. Methods are named `area.action`, and the table is kept in alphabetical order. Application errors answer HTTP 200, with `error.code` (HTTP semantics), `error.data.type` and an `error.message` that names the next step. Reserved codes are used only for protocol faults.
- **Credentials travel only in headers:** the device cookie, `Authorization: Bearer` for agent tokens, `X-Setup-Secret` during setup. Never in an address or in params. The one exception is the invitation token, which travels in the fragment of the link and is sent once in the body of `invitation.redeem`.
- **The service never sees a portal login in plain text.** The browser seals it with the vault key. `store.rs` refuses anything that does not have the shape of a sealed value (12-byte nonce, ciphertext with tag); it cannot tell real ciphertext from bytes that merely look like it.
- **The HTML file contains no data.** Two unauthenticated requests return identical bytes. Configuration lives in `static/config.json`. No script or stylesheet comes from another host. The inline script is allowed by its hash, which `src/http.rs` computes at start, so inline event handler attributes do not run: bind events with `addEventListener`.
- **The markup holds the building blocks; the script only clones, fills and binds.** Every piece of the interface is a `<template>`. The script fills `[data-field]` with `textContent` only (the one exception is the sanitized Markdown of the instructions), shows or hides `[data-ref]` parts and binds `[data-action]`. It creates no element with text of its own. Questions use the page's own dialog (`ask`), never `confirm()`.
- **Every text for people is translatable.** A text carries `data-i18n="key"`; a value inside it is an empty `<span data-field="name">`, which a translation places with `{name}`. Attributes use `data-i18n-placeholder`, `data-i18n-title`, `data-i18n-aria-label`. A text the script needs on its own (toast, question, chip) is an entry in the `messages` template, used through `msg`/`msgNode`/`plural`. Technical lines that must not be translated carry `translate="no"`. `src/frontend_check.rs` fails on text outside these rules and on a language file that lacks a key or has one too many.
- **Delivered texts are present tense and self-contained.** They contain no history ("removed", "formerly"), no internal identifiers, no file names of this repository, and every placeholder names the call that yields it. `src/describe.rs` tests this.
- **A behavior rule is stated once.** The principle goes in the usage description, the case in the description of the method it concerns. The usage description names no method, and a test holds this.
- **No administration surface besides the browser application and the commands.** People and agent tokens are administered through the person-level right in the application. The first administering person comes only through the setup window.
- **A backup is written in one place** (`Store::backup_text`), for `GET /backup`, `backup.export` and the `backup` command alike. Every table belongs in it, and `backup_import` reads every record type it writes; a new column goes into both.
- **An agent token carries exactly one ability, and only job methods and `backup.export` admit it.** A method's `ability` in `METHODS` names which; `None` means people only. Agents never set sealed values and receive them only as part of the backup (`Principal::sees_sealed`, `backup.read`). A test in `src/rpc.rs` holds that every ability exists and opens only job methods and `backup.export`.

## Pairings: change these together

| When you change … | … also change |
|---|---|
| a method in `METHODS` (`src/rpc.rs`) | its dispatch arm, the operation in `src/store.rs`, the caller in `static/index.html` |
| a text in `static/index.html` | its entry in every `static/i18n/<code>.json` |
| a language | its file in `static/i18n/`, `LANGUAGES` in `src/http.rs`, the list in `static/config.json` |
| what a caller may do or must do | the usage description in `src/describe.rs` and `README.md` |
| commands or exit codes in `src/main.rs` | the table in `README.md` and the head comment of `src/main.rs` |
| configuration keys | `src/config.rs` messages, `examples/config.toml`, `README.md`, and `deploy/config.toml` or `deploy/config-external.toml` where the image needs them |
| the invitation lifetime, the setup window length, the agent token deadlines or limit, the KDF parameters | `README.md`, `docs/operating.md`, the texts in `src/describe.rs` and `static/index.html` |
| the entry point, the Caddyfile or the image | `deploy/smoke.sh` and its mutations in `deploy/smoke-red.sh`, the container sections of `README.md` and `docs/operating.md` |
| the store's structure | a new step in `Store::migrate` with the next `user_version`, never an edit of an old step; and `backup_text` and `backup_import` |
