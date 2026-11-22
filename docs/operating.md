# Operating portal-download

This page is for whoever looks after a running service: people, rights, devices, agents and the backup. Setting the service up is in [install.md](install.md), moving to a new version in [upgrading.md](upgrading.md). The [glossary](glossary.md) explains the words.

All examples use the host name `downloads.intra.example` and a container named `portal-download`.

## People

Access comes by invitation. There are no passwords.

- **Every person works on all jobs** and sees every portal login in plain text.
- **Three rights** add to that. A person holds each of them or not: *manage people*, *manage agents*, *manage the backup*.
- The first person, created at setup, holds all three.

Rules for the rights:

- You can give or take away only a right that you hold yourself.
- One active person always holds all three. The service refuses a change that would end this.
- To act on a person you need every right that person holds. This covers a new invitation, changing the rights, deactivating, deleting, withdrawing an invitation and revoking a device.

Under **People**, a person who manages people finds:

| Action | What it does |
|---|---|
| **Invite a person** | Creates the person and the first invitation. Tick the rights the person gets. |
| **Set rights** | Changes the three rights of a person. |
| **New invitation** | Creates a fresh link for an existing person. It does not create a second person. |
| **Deactivate** / **Reactivate** | Locks a person out on all devices at once, or lets them in again. |
| **Delete** | Removes a person with all invitations and devices. Runs the person closed keep the name. Nobody can delete themselves. |
| **Withdraw** | Stops an open invitation. Devices already signed in stay signed in. |
| **Revoke** | Removes one device of the person. |

**When somebody leaves,** deactivate or delete the person. All their devices stop at the next request.

Agent tokens that the person created or granted keep working. The **Agents** page shows who granted each token. Revoke those that should end.

## Rights and roles

A **right** belongs to a person. A **role** belongs to an agent token. A person can hold several rights; a token has exactly one role, and it never changes.

| What it permits | A person needs | An agent token needs the role |
|---|---|---|
| Read jobs, their instructions and history, without the logins | nothing; a person also sees the logins | `jobs.read`, *Read jobs* |
| Read jobs with their logins, close runs, export the jobs | nothing | `jobs.work`, *Work through jobs (with logins)* |
| Create, change and delete jobs, set logins without seeing them, close runs, read files in | nothing | `jobs.manage`, *Manage jobs (sets logins, does not see them)* |
| Everything on jobs | nothing | `jobs.all`, *Everything on jobs* |
| Take the full backup | the right *manage the backup* | `backup.read`, *Take the backup* |
| Invite people, deactivate and delete them, see and revoke every device | the right *manage people* | `people.manage`, *Manage people* |
| Grant, create and revoke agent tokens | the right *manage agents* | `agents.manage`, *Manage agents* |
| Read a full backup in together with its people | all three rights | not possible for a token |

## Invitations

1. Under **People**, enter the name, tick the rights and click **Create invitation**.
2. The link is shown once. Copy it with **Copy link**.
3. Send the person three things through a confidential channel: the link, the fingerprint of the certificate authority, and the [certificate instructions](install.md#certificates).

The invited person must import the certificate authority **before** opening the link. Otherwise the browser warns, and the browser extension in Chrome and Edge cannot use the link at all.

Facts about the link:

- It is valid for seven days.
- In that time the person can open it in each of their browsers and browser extensions. Each one becomes a device of its own.
- The link names the address in the address bar of whoever creates it. Create it under the address the others can open.
- If it may have leaked while it is still open, withdraw it. Devices that are already registered stay registered.
- Who invited whom stays visible under **People**.

## Devices

A device is one browser or one browser extension of a person. A browser keeps its access as a cookie. The extension keeps a token. The service stores only hashes of both.

- **A lost device:** revoke it. The owner does that under **My devices**; a person who manages people under **All devices**. The person's other devices keep working.
- **A further device:** the person opens the invitation link there while it is valid. Afterwards a person who manages people creates a **New invitation**.
- **The browser extension:** the person adds it with an invitation link, or with **Add to the browser extension** under **My devices**. That link serves once, within the hour.

What the service records for each device:

| Record | Meaning |
|---|---|
| the label | the name the person gave the device |
| the kind | browser or browser extension |
| the identification | what the browser says it is (its `User-Agent`) |
| the last address | the network address the device last came from |
| the times | when it was registered, last seen and, if so, revoked |
| the invitation | which invitation registered it |

The owner sees these under **My devices**. A person who manages people sees them for everybody under **All devices**.

The service connects to nothing. The browsers and the extension reach the portals themselves.

## Agents

An agent or a script works with an agent token, never with a person's access. It sends the token in the header `Authorization: Bearer`.

**An agent that registers itself:**

1. The agent registers through the API. It gets a token without any role, and tells its user a display string of eight letters.
2. Under **Agents**, check that the display string and the introduction match what the agent said.
3. Click **Grant** and choose exactly one role. Reject registrations you do not recognise.

**A script that cannot register itself:** under **Agents**, **Create a token**, enter what it is for, choose the role and click **Create token**. The token is shown once.

**The backup line.** For the role *Take the backup*, the application shows a complete `curl` line that fetches the backup. After **Create token** the line holds the token itself. After **Grant** it holds `$PORTAL_DOWNLOAD_AGENT_TOKEN` in the token's place, because the agent already has its token.

Which role to choose:

| The agent or script | Role |
|---|---|
| only needs the list of due jobs | *Read jobs* |
| logs into the portals | *Work through jobs (with logins)* |
| keeps the job list up to date | *Manage jobs (sets logins, does not see them)* |
| fetches the backup | *Take the backup* |

Things to know:

- You can grant only a role you could hold: *Take the backup* needs the right to manage the backup, and so on.
- A token that manages people can invite. It is as strong as a person with that right, because it could open the invitation itself.
- A token that manages agents grants exactly the roles its granter could grant at that time. When that person loses a right, revoke such tokens.
- A registration nobody grants expires after 24 hours. A granted token expires after 180 days without use.
- At most ten registrations wait at the same time.
- **Revoke** works at any time. It needs only the right to manage agents, whatever role the token has. So anybody with that right can stop a token at once.
- Tokens that a revoked token granted keep working until they are revoked themselves.
- The machine that uses a token keeps it in the environment variable `PORTAL_DOWNLOAD_AGENT_TOKEN`.

[reference.md](reference.md#for-agents-and-scripts) describes the API.

## Backup

The full backup is one ZIP file. It holds the jobs with their logins, the runs, the deleted jobs, the people, invitations, devices and agent tokens. Of the accesses it holds only hashes.

**A file without a password holds the portal logins in plain text.** Set a password, or keep the files where only the people who may see the logins can read them.

The password rules are the same for every exported file: 8 to 200 characters; letters A to Z and a to z, digits, punctuation and spaces; no umlauts or other non-ASCII characters; no space at either end.

There are three ways to take the backup. They write the same file.

| Way | Who | How |
|---|---|---|
| In the web application | a person who manages the backup | **Exchange**, **Full backup**, **Download the full backup** |
| With `curl`, from any machine | a token with the role *Take the backup* | see below |
| Inside the container | the operator | see [Inside the container](#inside-the-container) |

### With curl

The steps below set up a nightly backup on a backup machine. A user of its own, `pd-backup`, does the work in the directory `/srv/backup`. Debian and Ubuntu already have a system user called `backup`, hence the other name.

**1. Create the user and the directory,** as root:

```sh
mkdir -p /srv/backup
useradd --system --home-dir /srv/backup --shell /usr/sbin/nologin pd-backup
chown pd-backup: /srv/backup
chmod 700 /srv/backup
```

Do the steps 3 to 5 in a shell of that user. Open it as root with:

```sh
sudo -u pd-backup -H bash
```

**2. Create the token.** In the web application, under **Agents**, **Create a token** with the role *Take the backup*. You need the rights to manage agents and to manage the backup.

The application shows a complete `curl` line with the token in it. **It is shown only once.** Copy the token, the part that starts with `pda_`, now. If it gets lost, revoke the token under **Agents** and create a new one.

**3. Let the machine trust the service's certificate.** Skip this if it trusts the certificate already. Otherwise fetch the authority, compare its fingerprint with the ready block, and convert it:

```sh
cd /srv/backup
curl -k https://downloads.intra.example/ca.crt -o ca.der
openssl x509 -inform der -in ca.der -noout -fingerprint -sha256
openssl x509 -inform der -in ca.der -out ca.pem
```

The authority stays the same when the certificate is renewed. See [install.md](install.md#certificates).

**4. Write the password into a header file** that only this user can read:

```sh
umask 077
read -rs -p 'Backup password: ' PW && printf 'X-Backup-Password: %s\n' "$PW" > /srv/backup/header
unset PW
```

- `read -rs` asks for the password without showing it. So it appears neither in the shell history nor on a command line. This needs bash.
- Or write the file with an editor: one line, `X-Backup-Password: ` followed by the password. Then set its mode with `chmod 600 /srv/backup/header`.
- Do not put the password itself into a command. It would stay in the shell history.

`curl` reads the header from the file. So the password is not part of the `curl` command, and not visible in the process list. It travels in this header only, never in the address.

**5. Take one backup by hand:**

```sh
cd /srv/backup
read -rs -p 'Token: ' PORTAL_DOWNLOAD_AGENT_TOKEN && export PORTAL_DOWNLOAD_AGENT_TOKEN
curl -fsS --cacert /srv/backup/ca.pem \
  -H "Authorization: Bearer $PORTAL_DOWNLOAD_AGENT_TOKEN" \
  -H @/srv/backup/header \
  https://downloads.intra.example/backup -o portal-download-$(date +%F).zip
```

- Leave `--cacert …` out when the machine trusts the certificate already.
- Leave `-H @/srv/backup/header` out for a file without a password.
- On an error, `curl -f` prints the HTTP status, such as 401 or 403, and ends with a non-zero exit code. For the reason in words, run the line once without `-f` and without `-o …`.

**6. Test the backup.** In the web application, open **Exchange**, **Read a file in**. Choose the file, enter its password and click **Show what would change**. The preview shows that the file and the password are good. Then click **Cancel**; nothing was changed.

Do this once now and again from time to time. A backup that was never read is a hope, not a backup.

**7. Run it every night.** As root, open the user's crontab with `crontab -u pd-backup -e` and add:

```crontab
PORTAL_DOWNLOAD_AGENT_TOKEN=pda_<TOKEN>
30 2 * * * cd /srv/backup && curl -fsS --cacert /srv/backup/ca.pem -H "Authorization: Bearer $PORTAL_DOWNLOAD_AGENT_TOKEN" -H @/srv/backup/header https://downloads.intra.example/backup -o portal-download-$(date +\%F).zip
40 2 * * * find /srv/backup -name 'portal-download-*.zip' -mtime +30 -delete
```

- In a crontab every `%` must be written `\%`.
- The first line sets the token for the lines below it. A user's crontab is readable by that user and by root only.
- The last line deletes backups older than 30 days. Change the number to keep more or fewer.
- Cron mails what a command prints. `curl -fsS` prints only errors, so a mail means a failed backup.

When the purpose ends, revoke the token under **Agents**.

### Inside the container

```sh
docker exec portal-download portal-download backup /data/backup.zip
docker cp portal-download:/data/backup.zip .
docker exec portal-download rm /data/backup.zip
```

The command also works while the service runs. It never overwrites a file, and it makes the file readable by its owner only.

For an encrypted file, add `--password-file PATH`. The first line of that file is the password. Without it the file is not encrypted.

### Reading a backup in

Reading in adds and merges. It never replaces what is there.

- **The jobs:** every person can read the jobs of a full backup in, under **Exchange**, **Read a file in**. [using.md](using.md#read-a-file-in) describes the preview.
- **The people:** a person with all three rights can tick **Also read in the people, devices and tokens of a full backup (added beside the existing ones)**. They come in beside the existing ones.
- A person with the name or e-mail of a person here comes in with "(Import YYYY-MM-DD)" after the name.

**Read people in only from a backup you took from this service yourself.** It brings rights and working accesses.

The file is checked line by line. One faulty line rejects the whole file. A file of another format version is refused, with a message that names the version this service reads.

### Restoring on a fresh instance

1. Start a new instance, as [install.md](install.md) describes.
2. Set it up with a name such as "Setup".
3. Under **Exchange**, read the backup in, with the people ticked.
4. Still as "Setup", open **People**. Find your own restored person and click **New invitation**.
5. Open that link in this browser and accept it. The browser is now signed in as your restored person.
6. Under **People**, delete "Setup". Your restored person needs all three rights for that.

Devices and tokens from the backup keep working when the new instance runs under the same host name. The browsers keep their cookies per host name, and the extension keeps its tokens per address.

A new volume also means a new certificate authority. Every computer imports it once more; see [install.md](install.md#certificates).

## Locked out

You are locked out when every person who manages people has lost their devices. The setup window is the way back in. Its secret creates a new person with all three rights.

1. Print a fresh setup secret. It is valid for 30 minutes.

   ```sh
   docker exec portal-download portal-download bootstrap
   ```

2. Create the person through the API. The setup form in the browser appears only while the service has no people at all.

   `ca.pem` is the service's certificate authority in PEM form. [install.md](install.md#certificates) shows how to fetch it, under "`curl` and scripts". Leave `--cacert ca.pem` out when the machine trusts the certificate already.

   ```sh
   curl -s --cacert ca.pem https://downloads.intra.example/api/ \
     -H 'Content-Type: application/json' \
     -H 'X-Setup-Secret: pds_<SECRET>' \
     -d '{"jsonrpc":"2.0","id":1,"method":"setup.createAdmin","params":{"name":"Ada"}}'
   ```

3. The response holds `invitation_token`, a value that starts with `pdi_`. Build the invitation link from it: the service's address, then `/#invite=`, then that value. For example `https://downloads.intra.example/#invite=pdi_<TOKEN>`.
4. Open the link in your browser and accept the invitation there. You are the new person, with all three rights.

Without a container, run `portal-download bootstrap` as the service user.

## Upgrading

Take a backup first. Then follow [upgrading.md](upgrading.md); it has one table per version step.

- **From 0.8 to 0.9:** pull the image `:0.9` and start a new container on the same volume. The store, the certificate authority, the people and their devices stay. Going back to 0.8 works the same way.
- **From 0.7 to 0.8:** a new instance on a new volume. Version 0.8 reads no data of 0.7.

## Where things are

[reference.md](reference.md) lists the files in the container and on a host, the commands with their exit codes, the configuration keys and the lines of the ready block.
