# Upgrading portal-download

This page has one table per version step. Find the step from your version to the next, and do the steps in order. To skip a version, do each step in turn.

All examples use a container named `portal-download`, the volume `portal-download` and the host name `downloads.intra.example`. Every command names its image tag. A command without a tag would install `latest`, which is not what an upgrade step wants.

## Before every upgrade

- **Find your version.** `docker exec portal-download portal-download version` prints it.
- **Take a backup.** [operating.md](operating.md#backup) shows how. An older version refuses a store that a newer version wrote, so the backup is the way back.
- **Keep your start command.** The new container needs the same options as the old one: `SITE`, `TZ`, the port and the volume.

## Which versions work together

| Version of the service | Reads the store of | Interface version | Extensions that work with it |
|---|---|---|---|
| 0.9 | 0.8 and 0.9 | 0.8 | 0.8 and 0.9 |
| 0.8 | 0.8 | 0.8 | 0.8 and 0.9 |
| 0.7 | 0.7 | 0.7 | none |

[reference.md](reference.md#versions) explains the three kinds of version.

## From 0.8 to 0.9

**In short: pull the new image and start a new container on the same volume.** Everything stays: the jobs, the people, their devices, the agent tokens and the certificate authority. Nobody has to be invited again, and no computer has to import a certificate again.

| Step | Command |
|---|---|
| 1. Take a backup | see [operating.md](operating.md#backup) |
| 2. Pull the new image | `docker pull ghcr.io/schlingensiepen/portal-download:0.9` |
| 3. Stop and remove the old container | `docker stop portal-download && docker rm portal-download` |
| 4. Start the new container on the same volume | the `docker run` below |
| 5. Check the log | `docker logs portal-download` |

```bash
docker run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.9
```

Removing the container in step 3 removes no data. The data lies in the volume.

The log must end like this, with your numbers:

```text
portal-download container: ----------------------------------------------------------------
portal-download container:    READY: set up, 3 people, 12 jobs
portal-download container:    Open:                   https://downloads.intra.example/
portal-download container:    Certificate authority:  SHA-256 <FINGERPRINT>
portal-download container:                            trust it once: https://downloads.intra.example/ca.crt (compare first)
portal-download container: ================================================================
portal-download container: portal-download 0.9.0 is ready: https://downloads.intra.example/
```

**With Docker Compose:** change the image line to `ghcr.io/schlingensiepen/portal-download:0.9`. Then run `docker compose pull` and `docker compose up -d`.

**The browser extension:** extensions of version 0.8 and 0.9 both work with a 0.9 service. The service's interface version is still 0.8. People can update the extension whenever they like.

## From 0.7 to 0.8

**In short: version 0.8 is a new instance.** It reads no store and no backup of 0.7. It starts empty on a new volume. You set it up anew, bring the jobs over, and invite everybody again.

The same holds for going from 0.7 straight to 0.9. Use the tag `:0.9` instead of `:0.8` in step 2; nothing else changes.

| Step | Command |
|---|---|
| 1. Stop the old container and rename it | `docker stop portal-download && docker rename portal-download portal-download-0.7` |
| 2. Start 0.8 on a **new** volume | the first `docker run` below |
| 3. Set the new instance up | read the setup secret with `docker logs portal-download`; see [install.md](install.md#first-setup) |
| 4. Import the new certificate authority on every computer | see [install.md](install.md#certificates) |
| 5. Bring the jobs over | see below |
| 6. Invite every person anew, and give every agent a new token | see [operating.md](operating.md#invitations) |
| 7. Remove the old container, later its volume | `docker rm -f portal-download-0.7`, then `docker volume rm portal-download` |

Step 2, the new instance on the new volume `portal-download-0.8`:

```bash
docker run -d --name portal-download --restart unless-stopped \
  -p 443:443 -v portal-download-0.8:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.8
```

In a compose file, give the volume a new name under `volumes:` instead.

### Why the old volume is kept

The old volume is the only place that still holds your 0.7 data, the portal logins among them. Version 0.8 cannot read that store, and it cannot read backups of 0.7 either. So keep the volume until every job is in the new instance.

If you start 0.8 on the old volume by mistake, nothing is damaged. The container refuses to start, and the log names the store files and what to do.

### Bringing the jobs over

There are two ways:

- **From Firefox:** if the logins are saved in Firefox, use **Import logins (Firefox CSV)** in the new instance. [using.md](using.md#importing-logins-from-firefox) describes it.
- **By hand from the old instance:** run the old version once more, beside the new one, and copy the jobs over by hand.

To look into the old instance, start the old image on the old volume, on another port:

```bash
docker rm portal-download-0.7
docker run -d --name portal-download-0.7 \
  -p 8443:443 -v portal-download:/data \
  -e SITE=downloads.intra.example -e TZ=Europe/Berlin \
  ghcr.io/schlingensiepen/portal-download:0.7
```

Then open `https://downloads.intra.example:8443/`. If you ran a fixed version such as `0.7.2`, use that tag. `docker image ls ghcr.io/schlingensiepen/portal-download` lists the tags you have.

**Only one container may use a volume at a time.** Two services that write the same store can damage it. Here that rule is kept: the old container uses the old volume, the new container the new one.

### What happens to the old accesses

The new instance knows none of the old people, devices and tokens. When `SITE` stays the same, the old entries still point to the right address, but their accesses are no longer valid:

| Where | What the person sees | What to do |
|---|---|---|
| A browser that was signed in | "This device has no access." | Open the new invitation link. |
| The browser extension, if the service was added there | The entry says that its access is no longer valid. | Remove the service under **⚙** and add it again with a new link. |
| An agent or script | Its token is refused. | Give it a new token. |
| A computer that trusted the old certificate authority | The browser warns about the certificate again. | Import the new authority. Remove the old one. |

The new image makes its own certificate authority, because the volume is new. Each authority carries a random part in its name, and the fingerprint tells them apart.

### When nothing is missing

Remove the old container with `docker rm -f portal-download-0.7`. When you are sure, remove its volume with `docker volume rm portal-download`. This deletes the 0.7 data for good.

## Later: a new instance under the same address

Sometimes a later version, or a move, also means a new instance on a new volume. If you read a full backup in with its people, the devices and tokens keep working under the same host name. [operating.md](operating.md#restoring-on-a-fresh-instance) describes that.

If you start empty instead, the old accesses end as in the table above. In the browser extension, the entry for the service then shows that its access is no longer valid. Remove the service under **⚙** and add it again with a new invitation link.

## Without a container

Replace the binary and restart the service. `portal-download status` shows the store's structure version. For the step from 0.7, stop the service and move `portal-download.sqlite` with its `-wal` and `-shm` files out of the state directory. Then start again and set up with the new setup secret.
