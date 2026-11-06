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
2. Start the service. The log line `not set up yet. Setup secret: pds_…` names the secret and how long it is valid.
3. Open the service over its HTTPS address, enter the setup secret and your name. This creates the first person with the right to administer people and opens their invitation in that browser.
4. Create the vault passphrase. Pass it to the other people of the circle in person, not through this service.

## Daily operation

- **Inviting:** under **Personen**, a person with the right to administer people creates the invitation. The link is shown once and is valid for seven days on any number of the invited person's devices. **Neue Einladung** creates a fresh link for an existing person without creating a second one.
- **A lost device:** revoke that device, either the owner under **Meine Geräte** or an administering person under **Personen**. If the invitation may have leaked and is still open, withdraw it too. Devices that are already registered stay registered.
- **Somebody leaves:** deactivate the person. All their devices stop at the next request.
- **Locked out (all administering people lost their devices):** run `portal-download bootstrap` on the server, then call `setup.createAdmin` with the printed secret or open the setup form. The form appears in a browser without a device credential only while nobody is set up. With people already set up, use the API:

  ```sh
  curl -s https://downloads.intra.example/api/ -H 'Content-Type: application/json' \
    -H "X-Setup-Secret: $SECRET" \
    -d '{"jsonrpc":"2.0","id":1,"method":"setup.createAdmin","params":{"name":"Ada"}}'
  ```

  The response holds `invitation_token`. Open `https://downloads.intra.example/#invite=<invitation_token>` with the token from that response.
- **A machine key:** while the setup window is open, call `machinekey.create` with `{"machine":"nas","task":"job.list"}` and the header `X-Setup-Secret`. Put the returned key into that machine's environment as `PORTAL_DOWNLOAD_MACHINE_KEY`. The key goes in the header `Authorization: Bearer`. Shut it down under **Personen** at any time.

## Backup

Back up the state directory while the service is stopped, or use `sqlite3 portal-download.sqlite ".backup copy.sqlite"`. The backup contains no readable portal login, because those are sealed with the vault key. It does contain the hashes of device credentials and the history of runs.

## Upgrading

Replace the binary and restart. A newer version migrates the store at start, before it listens, and records the structure version (`portal-download status`). An older binary refuses a store written by a newer one.

## What is known not to be solved yet

- **Changing the vault passphrase** is not offered yet. Every sealed login would have to be re-sealed in a browser that knows both passphrases.
- **A container image** with a published build is not provided yet; the binary is built from source.
