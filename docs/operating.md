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
- **An agent or a script:** it registers itself through the API and names an eight-letter display string. Under **Personen → Agenten**, check that the display string and the introduction match what the agent said, and grant exactly one ability. Reject registrations you do not recognise. A script that only needs the due list gets *Jobs lesen*. The token lives in that machine's environment as `PORTAL_DOWNLOAD_AGENT_TOKEN`. Revoke it there at any time.

## Backup

The backup is one JSONL file: a head line, then one record per line (vault, people, invitations, devices, agent tokens, jobs, runs). There are two ways to get it, and they write the same file:

- **With `curl`, from any machine:** under **Personen → Agenten**, create a token with the ability *Backup holen*. The application shows the complete line once, for example as a nightly cron job:

  ```sh
  curl -fsS -H "Authorization: Bearer pda_…" https://downloads.intra.example/backup -o portal-download-$(date +%F).jsonl
  ```

  A person with the right to administer people can also click **Backup herunterladen** under **Personen**.
- **On the server:** `portal-download backup /some/dir/portal-download-$(date +%F).jsonl`. It never overwrites a file and makes it readable by its owner only.

**Restoring is adding.** Under **Personen → Backup**, a person with the right to administer people uploads the file. Nothing is overwritten. A job with the same title and entry address, or a person with the same name or e-mail, comes in a second time as "… (Import YYYY-MM-DD)". The application then lists those copies with a delete button each. *Personen und Zugänge weglassen* imports only jobs, their history and the vault.

**On a fresh instance**, set up with a name such as "Setup", upload the backup with people and accesses, and delete "Setup" afterwards. Devices and tokens from the backup keep working when the instance runs under the same address. If the instance has no vault yet, it adopts the one from the backup, and the old passphrase applies. If it has a different vault, jobs come without their logins.

**Without the vault passphrase the logins in a backup cannot be read.** Keep the passphrase safe apart from the backup, for example in a password manager. Keep the backup file as carefully as a token anyway: it holds the sealed logins and the hashes of all accesses.

## Upgrading

Replace the binary and restart. A newer version migrates the store at start, before it listens, and records the structure version (`portal-download status`). An older binary refuses a store written by a newer one.

## What is known not to be solved yet

- **Changing the vault passphrase** is not offered yet. Every sealed login would have to be re-sealed in a browser that knows both passphrases.
- **A container image** with a published build is not provided yet; the binary is built from source.
