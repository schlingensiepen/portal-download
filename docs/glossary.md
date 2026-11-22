# Glossary

| Word | Meaning |
|---|---|
| **service** | The portal-download program that runs on a server. It keeps the jobs, the people and their access, and includes the web application. |
| **instance** | One running service with its own store, its own address and its own people. Two instances know nothing of each other. |
| **backend** | A word of the browser extension: one source of jobs. A backend is either a service or the local store. |
| **local store** | Jobs that the browser extension keeps in one browser only, encrypted with a passphrase. It needs no service. |
| **job** | One portal with a recurring download: its title, entry address, login, instructions and schedule. |
| **run** | One time a job was worked through. A run is closed as succeeded or failed, and stays in the job's history. |
| **due** | A job is due from its next due date on. Closing a run moves that date forward. |
| **entry address** | The web address of the portal that a job opens. A new entry address makes the job a new job for the exchange. |
| **exchange file** | A ZIP file with jobs, their logins and their history. Services and the extension write and read it. With a password it is encrypted with AES-256. |
| **full backup** | An exchange file that also holds the people, invitations, devices and agent tokens of a service. |
| **agent** | A program or script that uses the service through its API, with a token of its own. |
| **role** | What one agent token may do. A token has exactly one role, such as `jobs.read`. |
| **right** | What a person may do beyond working on jobs: manage people, manage agents, manage the backup. A person holds each right or not. |
| **device** | One browser or one browser extension in which a person opened an invitation link. Each has an access of its own. |
| **ready block** | The framed lines the container writes to its log once the service answers. They name the address, the certificate and, while needed, the setup secret. |
| **setup secret** | A secret that is valid for 30 minutes. It creates a person with all three rights. |
| **`SITE`** | The start option that names the host name or IPv4 address the browsers use. The certificate is made for it. |
| **`PROXY`** | The start option for the mode. Unset: the image serves HTTPS itself. `PROXY=external`: a reverse proxy of your own stands in front. |
