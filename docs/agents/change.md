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
| `static/index.html` | The whole browser application: form and hydration logic only, no data, no third-party code. |

## Rules that hold

- **Every action exists once, in `src/store.rs`.** `src/rpc.rs` only parses params, calls one operation and returns its result. A surface that needs something no operation offers gets a new operation.
- **Access is checked in one place** (`admit` in `src/rpc.rs`), before any identifier in the params is resolved, so an error message never reveals whether an object exists. Where an operation checks ownership itself (`device.revoke`), an object the caller may not touch is answered exactly like a missing one.
- **Store errors reach the caller without detail;** the detail goes to the service log only.
- **One API address:** `POST /api/` with an unmodified JSON-RPC 2.0 envelope. Methods are named `area.action`, and the table is kept in alphabetical order. Application errors answer HTTP 200, with `error.code` (HTTP semantics), `error.data.type` and an `error.message` that names the next step. Reserved codes are used only for protocol faults.
- **Credentials travel only in headers:** the device cookie, `Authorization: Bearer` for machine keys, `X-Setup-Secret` during setup. Never in an address or in params. The one exception is the invitation token, which travels in the fragment of the link and is sent once in the body of `invitation.redeem`.
- **The service never sees a portal login in plain text.** The browser seals it with the vault key. `store.rs` refuses anything that does not have the shape of a sealed value (12-byte nonce, ciphertext with tag); it cannot tell real ciphertext from bytes that merely look like it.
- **The HTML file contains no data.** Two unauthenticated requests return identical bytes. Configuration lives in `static/config.json`. No script or stylesheet comes from another host. The inline script is allowed by its hash, which `src/http.rs` computes at start, so inline event handler attributes do not run: bind events with `addEventListener`.
- **Delivered texts are present tense and self-contained.** They contain no history ("removed", "formerly"), no internal identifiers, no file names of this repository, and every placeholder names the call that yields it. `src/describe.rs` tests this.
- **A behavior rule is stated once.** The principle goes in the usage description, the case in the description of the method it concerns. The usage description names no method, and a test holds this.
- **No administration surface besides the browser application and the commands.** People are administered through the person-level right in the application. Machine keys and the first administering person come only through the setup window.

## Pairings: change these together

| When you change … | … also change |
|---|---|
| a method in `METHODS` (`src/rpc.rs`) | its dispatch arm, the operation in `src/store.rs`, the caller in `static/index.html` |
| what a caller may do or must do | the usage description in `src/describe.rs` and `README.md` |
| commands or exit codes in `src/main.rs` | the table in `README.md` and the head comment of `src/main.rs` |
| configuration keys | `src/config.rs` messages, `examples/config.toml`, `README.md` |
| the invitation lifetime, the setup window length, the KDF parameters | `README.md`, `docs/operating.md`, the texts in `src/describe.rs` and `static/index.html` |
| the store's structure | a new step in `Store::migrate` with the next `user_version`, never an edit of an old step |
