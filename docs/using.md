# Using the web application

This page is for everybody in the circle. It describes the web application of the service. Most people work with the [browser extension](https://github.com/schlingensiepen/portal-download-extension#readme) instead; it shows the same jobs next to the portal.

The words *job*, *run* and *due* are explained in the [glossary](glossary.md).

## Getting in

There are no passwords. You get in with an invitation link.

1. Make your computer trust the service's certificate first. [install.md](install.md#certificates) shows how. Whoever invited you gives you the fingerprint to compare.
2. Open the invitation link. The page **Accept the invitation** appears.
3. Under **In this browser**, give the device a name and click **Accept the invitation in this browser**.

The link is valid for seven days. In that time you can open it in each of your browsers, and in the browser extension. Every browser gets an access of its own.

If you see "This device has no access", the browser has no access yet or lost it. Open your invitation link again. If it has run out, ask a person who manages people for a new one.

## The page

The right quarter of the page is the **work bar**. On a narrow screen it stands at the top.

- At its top are your name, the language menu and the navigation: **Overview**, **New job**, **Exchange**, **My devices**. People with rights also see **All devices**, **People** and **Agents**.
- Below, it shows one job: the most urgent one.

The left part shows what you chose in the navigation. **Overview** lists all jobs, the most urgent first.

## Working through a job

1. The work bar shows the most urgent job: when it is due, the login, the last note and the instructions. **◀** and **▶** page through the jobs in order of urgency.
2. Click **Copy** beside the username or the password. **Show** reveals the password.
3. Click **Open the portal (new tab)**. The portal opens in a new tab of the browser.
4. Log in, download the documents and file them as the instructions say.
5. Come back to the tab of the web application. Click **Succeeded** or **Failed**, and confirm. You can leave a note for the next time.

The job then stays in sight with its new due date until you click **Next job**.

## How the schedule works

Every job has an interval in weeks and a number of retry days.

| You close the run as | The next due date is |
|---|---|
| **Succeeded** | the day you closed it, plus the interval in weeks |
| **Failed** | the day you closed it, plus the retry days (2 unless set otherwise) |

- A job is **due** from its due date on. Until then the list shows "due in … days", afterwards "overdue by … days".
- The most urgent job is the one whose due date lies furthest back.
- "Today" is the date in the time zone of the service. The operator sets it with `TZ`.
- A failed run and its note stay in the job's history.

## Creating and changing a job

**New job** opens the form. **Edit** in the work bar opens it for an existing job.

| Field | Meaning |
|---|---|
| **Title** | The name in the list, 1 to 200 characters. |
| **Entry page (URL)** | The portal's address, starting with `http://` or `https://`. This is the job's entry address. |
| **Every how many weeks?** | The interval, 1 to 520 weeks. |
| **After a failure, due again in … days** | The retry days, 1 to 365. |
| **Next due date** | Today unless you choose another date. |
| **Username**, **Password** | The portal login. |
| **Instructions: what to file where? (Markdown)** | A short text for whoever does the download. |

**The login is kept in plain text on the service.** Everybody in the circle sees it, and a change applies for everybody at once.

A new entry address makes the job a new job for the exchange with other instances. Its history stays.

**Delete** removes a job with its history. **History** lists the closed runs of a job: when, by whom, with which outcome and note.

## Importing logins from Firefox

**Import logins (Firefox CSV)**, above the job list, turns logins saved in Firefox into jobs.

1. In Firefox, open `about:logins` and choose *Export Passwords…* in its menu.
2. In the web application, click **Import logins (Firefox CSV)** and pick the exported file. The browser reads the file itself. Nothing is sent yet.
3. The page lists host and username of every login. Nothing is ticked. Tick the logins that should become jobs.
4. Change the weeks in a row if four weeks do not fit.
5. Click **Create jobs from the ticked logins** and confirm.
6. Delete the exported file. It holds every password saved in Firefox.

What you get:

- Each ticked login becomes a job named after its host, with the login and without instructions. It is due today.
- Several logins for one host become several jobs of that name.
- A login that matches an existing job by host name and username is marked "already there" and cannot be ticked.
- Duplicates within the file count once. The entry whose password was changed last wins.
- If the service refuses a login, the others are still created. One message counts the refusals and lists the first five with their reasons; "…" stands for the rest.

## Exchanging jobs as a file

Jobs travel as a ZIP file, called an **exchange file**. The service, every other instance and the browser extension write and read the same format. **Exchange** in the navigation holds both directions.

### Export the jobs

1. Enter a password for the file, or leave the field empty.
2. Click **Download the jobs**. The browser saves `portal-download-jobs-<date>.zip`.

The file holds every job with its login, its history and the deleted jobs.

- **With a password** the file is encrypted with AES-256.
- **Without a password** it holds every login in plain text. The application asks before it downloads.

The password rules are the same everywhere: 8 to 200 characters; letters A to Z and a to z, digits, punctuation and spaces; no umlauts or other non-ASCII characters; no space at either end.

### Read a file in

1. Under **Read a file in**, choose the file and enter its password, if it has one.
2. Click **Show what would change**. Nothing is changed yet.
3. The preview sorts every job of the file into one of three groups:

   | Group | Meaning |
   |---|---|
   | **Taken over, because new** | The job is not known here. It comes with its runs. |
   | **Taken over, because the same** | The job is known here with equal fields. Runs that are missing here are added. |
   | **Please decide what to take over** | The job is known here with other values, or was deleted here, or replaces a job here under a new address. |

4. For each job of the third group, choose per field between **Here** and **In the file**. The newer value is chosen for you. For the due date it is the nearer one.
5. A job that was deleted here can be brought back or left out. A job with a new address can replace the job here, stand beside it, or be left out.
6. Click **Read in**.

No run is ever lost. Reading in copies. To move jobs, read them in on the other side and then delete them where they were.

The same page holds the **Full backup**, for people who manage the backup. [operating.md](operating.md#backup) describes it.

## My devices

**My devices** lists every browser and browser extension in which you opened an invitation link.

- **Revoke** removes a lost device. Your other devices stay signed in.
- **Sign out** signs this browser out. Afterwards you need an invitation link again.
- **Add to the browser extension** creates a link for your browser extension and opens it in this tab. The link serves once, within the hour. Then click **+** in the extension.

## Languages

The web application is in English and German. It follows the browser's language. The menu at the top of the work bar chooses another one; the choice is kept in that browser. Messages that come from the service itself are in English in every language.
