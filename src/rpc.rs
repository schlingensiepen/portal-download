//! The JSON-RPC surface: a pass-through onto the operations of the store.
//!
//! The method table below is the single source for dispatch, for the access each method
//! needs, and for the machine contract the service delivers about itself.

use crate::bootstrap;
use crate::error::{OpError, OpResult};
use crate::store::{Cap, ImportMode, JobInput, Principal, Rights, Store};
use base64::Engine;
use chrono::NaiveDate;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// The contract version: callers built for the same major and minor version understand
/// each other.
pub fn contract() -> String {
    let v = env!("CARGO_PKG_VERSION");
    v.rsplit_once('.').map(|(mm, _)| mm.to_string()).unwrap_or_else(|| v.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Callable without any credential.
    Open,
    /// Needs the setup secret of an open bootstrap window in the header X-Setup-Secret.
    Setup,
    /// Needs a person's device credential; agent tokens are refused.
    Person,
    /// Needs a person's device credential or an agent token with a role; the operation
    /// decides what each may do.
    Signed,
    /// Needs a person who may do this, or an agent token with a role that admits it.
    Can(Cap),
}

pub struct Method {
    pub name: &'static str,
    pub access: Access,
    pub summary: &'static str,
    pub description: &'static str,
    pub params: &'static str,
    pub result: &'static str,
}

const JOB: &str = r#"{"job_id":7,"guid":"0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f","replaces":null,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","interval_weeks":4,"retry_days":2,"next_due":"2026-11-06","urgency_days":0,"username":"ada@example.org","password":"correct horse","instructions_md":"Download the PDF invoice and file it under Finance/Energy/2026.","instructions_html":"<p>Download the PDF invoice and file it under Finance/Energy/2026.</p>","last_run":{"outcome":"succeeded","finished_at":"2026-10-09T08:00:00Z","note":"","by":"Ada"}}"#;
const PREVIEW: &str = r#"{"kind":"jobs","new":[{"guid":"5e1d…","title":"Water","entry_url":"https://portal.example.org/water","new_runs":3}],"same":[{"guid":"7c2a…","title":"Phone","entry_url":"https://portal.example.org/phone","job_id":4,"case":"known","new_runs":1,"next_due_from_run":"2026-12-04"}],"decide":[{"guid":"0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f","title":"Energy invoice","entry_url":"https://portal.example.org/invoices","job_id":7,"case":"known","new_runs":0,"fields":[{"field":"password","local":"correct horse","incoming":"battery staple","local_changed_at":"2026-11-01T10:00:00Z","incoming_changed_at":"2026-11-05T10:00:00Z","preselect":"incoming"}]}]}"#;
const IMPORTED: &str = r#"{"added":1,"updated":2,"unchanged":0,"reactivated":0,"replaced":0,"skipped":0,"runs_added":4}"#;
const AGENT: &str = r#"{"agent_id":4,"display":"kluvmoth","introduction":"Agent of Ada; purpose: add new portals","state":"granted","role":"jobs.manage","granted_by":"Ada","granted_at":"2026-11-06T10:05:00Z","created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2027-05-05T10:05:00Z"}"#;

pub const METHODS: &[Method] = &[
    Method {
        name: "agent.create",
        access: Access::Can(Cap::ManageAgents),
        summary: "Create an agent token with its role at once, for a script that cannot register itself.",
        description: "introduction says what the token is for (10 to 500 characters); role is one of the roles agent.list names with grantable true. The token is shown only in this response; put it into the script's environment as PORTAL_DOWNLOAD_AGENT_TOKEN. This takes effect: confirm with the user first.",
        params: r#"{"introduction":"NAS: nightly backup","role":"backup.read"}"#,
        result: r#"{"agent_id":5,"display":"pobrewsa","introduction":"NAS: nightly backup","state":"granted","role":"backup.read","granted_by":"Ada","granted_at":"2026-11-06T10:00:00Z","created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2027-05-05T10:00:00Z","token":"pda_…"}"#,
    },
    Method {
        name: "agent.grant",
        access: Access::Can(Cap::ManageAgents),
        summary: "Grant a waiting agent token exactly one role.",
        description: "role is one of the roles agent.list names with grantable true; agent.list also says what each permits. The agent_id comes from agent.list; check its display string and introduction against what the agent told its user, and grant only the display string the user named. A token's role never changes afterwards; an agent that needs another role registers a second token. A token granted agents.manage can grant exactly the roles the granting caller could. This takes effect: confirm with the user first.",
        params: r#"{"agent_id":4,"role":"jobs.manage"}"#,
        result: AGENT,
    },
    Method {
        name: "agent.list",
        access: Access::Can(Cap::ManageAgents),
        summary: "List agent tokens with display string, introduction, state and role, and the roles there are.",
        description: "state is pending (registered, no role yet; expires 24 hours after registering), granted (expires 180 days after its last use), expired or revoked. roles lists every role with what it permits and whether the caller may grant it. Never returns a token itself.",
        params: r#"{}"#,
        result: r#"{"roles":[{"role":"jobs.read","permits":"read jobs, their instructions and history, without the logins","grantable":true}],"agents":[{"agent_id":4,"display":"kluvmoth","introduction":"Agent of Ada; purpose: add new portals","state":"pending","role":null,"granted_by":null,"granted_at":null,"created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2026-11-07T10:00:00Z"}]}"#,
    },
    Method {
        name: "agent.register",
        access: Access::Open,
        summary: "Register an agent: receive a token without any role, and its display string.",
        description: "introduction says whose agent this is and what for, in 10 to 500 characters; the person who grants the token reads it. The token is shown only in this response: store it at once outside the conversation, in the environment variable PORTAL_DOWNLOAD_AGENT_TOKEN, and send it as Authorization: Bearer with the token taken from this response. Tell the user only the display string and the one role needed, and ask them to have a person who manages agents grant it in the application under Agenten. Until then every call to a method the role would open is refused with agent_pending; service.status shows the token's role once granted. A token nobody grants expires after 24 hours. At most 10 registrations wait at the same time.",
        params: r#"{"introduction":"Agent of Ada; purpose: add new portals to the job list"}"#,
        result: r#"{"agent_id":4,"display":"kluvmoth","token":"pda_…","state":"pending","role":null,"expires_at":"2026-11-07T10:00:00Z"}"#,
    },
    Method {
        name: "agent.revoke",
        access: Access::Can(Cap::ManageAgents),
        summary: "Revoke one agent token at once, or reject a waiting one.",
        description: "Possible at any time; every other token keeps working, also those this token granted. The agent_id comes from agent.list. This takes effect: confirm with the user first.",
        params: r#"{"agent_id":4}"#,
        result: r#"{"agent_id":4,"revoked":true}"#,
    },
    Method {
        name: "backup.export",
        access: Access::Can(Cap::BackupRead),
        summary: "Return the whole backup as a ZIP file, base64-encoded.",
        description: "The file holds the jobs with their logins in plain text, their runs and the deleted jobs, and the people, invitations, devices and agent tokens with the hashes of their credentials, which grant no access. With the header X-Backup-Password (8 to 200 characters) the file is encrypted with AES-256; without it, it is not. The same file comes from GET on /backup of this service, with the same credential and header.",
        params: r#"{}"#,
        result: r#"{"file_name":"portal-download-2026-11-06.zip","encrypted":true,"zip_base64":"UEsDBBQAAQBjAA…"}"#,
    },
    Method {
        name: "backup.import",
        access: Access::Can(Cap::BackupImport),
        summary: "Read a full backup in: the jobs as job.import does, and the people, devices and tokens beside the existing ones.",
        description: "zip_base64 and decisions as for job.import, after a call of backup.importPreview. People come in with their rights and their working credentials; one with the name or e-mail of a person here comes in with (Import YYYY-MM-DD) after the name, and people.renamed lists the names this concerns. Import only a backup the user took from this service themselves. This takes effect: confirm with the user first.",
        params: r#"{"zip_base64":"UEsDBBQAAQBjAA…","decisions":{}}"#,
        result: r#"{"added":12,"updated":0,"unchanged":0,"reactivated":0,"replaced":0,"skipped":0,"runs_added":140,"people":{"people":3,"invitations":3,"devices":4,"agents":1,"renamed":["Ada"]}}"#,
    },
    Method {
        name: "backup.importPreview",
        access: Access::Can(Cap::BackupImport),
        summary: "Show what reading a full backup in would do, without changing anything.",
        description: "Like job.importPreview, and people counts the people, invitations, devices and agent tokens the file adds.",
        params: r#"{"zip_base64":"UEsDBBQAAQBjAA…"}"#,
        result: r#"{"kind":"full","new":[],"same":[],"decide":[],"people":{"people":3,"invitations":3,"devices":4,"agents":1}}"#,
    },
    Method {
        name: "device.list",
        access: Access::Person,
        summary: "List the calling person's own devices.",
        description: "Returns every device of the calling person, browsers and browser extensions, most recently seen first, and says which one the call came from. Use it to find a device_id for device.revoke.",
        params: r#"{}"#,
        result: r#"{"current_device_id":3,"devices":[{"device_id":3,"label":"Laptop Firefox","kind":"browser","invitation_id":1,"user_agent":"Mozilla/5.0 …","last_address":"10.0.0.12","created_at":"2026-11-06T10:00:00Z","last_seen_at":"2026-11-06T10:05:00Z","revoked_at":null,"user_id":1,"user_name":"Ada"}]}"#,
    },
    Method {
        name: "device.listAll",
        access: Access::Can(Cap::ManagePeople),
        summary: "List every device of every person, most recently seen first.",
        description: "Each device names its person (user_id, user_name). Use it to find a device_id for device.revoke.",
        params: r#"{}"#,
        result: r#"{"devices":[{"device_id":3,"label":"Laptop Firefox","kind":"browser","invitation_id":1,"user_agent":"Mozilla/5.0 …","last_address":"10.0.0.12","created_at":"2026-11-06T10:00:00Z","last_seen_at":"2026-11-06T10:05:00Z","revoked_at":null,"user_id":1,"user_name":"Ada"}]}"#,
    },
    Method {
        name: "device.revoke",
        access: Access::Signed,
        summary: "Revoke one device; the person's other devices keep working.",
        description: "A person revokes their own devices; another person's device needs the right to manage people, or a token with people.manage. Revoking the calling browser signs it out and clears its cookie. The device_id comes from device.list or device.listAll. This takes effect: confirm with the user first.",
        params: r#"{"device_id":3}"#,
        result: r#"{"device_id":3,"revoked":true}"#,
    },
    Method {
        name: "invitation.createOwn",
        access: Access::Person,
        summary: "Create a link for a further device of the calling person, the browser extension for example.",
        description: "The link is <base URL>/#invite=<invitation_token>, with invitation_token from this response; it serves once, within an hour. The token is shown only here.",
        params: r#"{}"#,
        result: r#"{"user_id":1,"invitation_id":6,"invitation_token":"pdi_…","expires_at":"2026-11-06T11:00:00Z"}"#,
    },
    Method {
        name: "invitation.redeem",
        access: Access::Open,
        summary: "Exchange an invitation token for a device's own credential.",
        description: "The token comes from the fragment of an invitation link (the part after #invite=). Each redemption registers one more device for the invited person; label names the device for its owner. kind is \"browser\" (default): the response sets the credential as an HttpOnly cookie; or \"extension\": the response carries it once as device_token, which the browser extension sends as Authorization: Bearer.",
        params: r#"{"token":"pdi_Q2hhbmdlTWUuLi4","label":"Laptop Firefox","kind":"browser"}"#,
        result: r#"{"user_id":1,"name":"Ada","device_id":3,"label":"Laptop Firefox","kind":"browser"}"#,
    },
    Method {
        name: "invitation.withdraw",
        access: Access::Can(Cap::ManagePeople),
        summary: "Stop an invitation from being redeemed again.",
        description: "Devices already registered with the invitation keep working; revoke them one by one with device.revoke if needed. The invitation_id comes from user.list. This takes effect: confirm with the user first.",
        params: r#"{"invitation_id":4}"#,
        result: r#"{"invitation_id":4,"withdrawn":true}"#,
    },
    Method {
        name: "job.complete",
        access: Access::Can(Cap::Complete),
        summary: "Close a download run as succeeded or failed and move the next due date.",
        description: "outcome is \"succeeded\" (next due date: today plus the job's interval in weeks) or \"failed\" (next due date: today plus the job's retry days); today is the date in the server's local time zone. The run is kept in the job's history with the optional note (up to 2000 characters). expected_next_due, the next_due value the caller saw, is optional; when sent and the job is due on another date meanwhile, the call is refused with job_changed instead of closing a run twice. This takes effect: confirm with the user first.",
        params: r#"{"job_id":7,"outcome":"succeeded","note":"Invoice October filed","expected_next_due":"2026-11-01"}"#,
        result: r#"{"job_id":7,"run_guid":"9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d","outcome":"succeeded","due_before":"2026-11-01","next_due":"2026-12-04"}"#,
    },
    Method {
        name: "job.create",
        access: Access::Can(Cap::WriteJobs),
        summary: "Create a download job.",
        description: "Required: title (text, 1 to 200 characters), entry_url (http or https address without spaces, up to 2000 characters), interval_weeks (integer 1 to 520). Optional: retry_days (integer 1 to 365, default 2), next_due (date YYYY-MM-DD, default today in the server's local time zone), instructions_md (Markdown up to 64 KiB saying what to file and where), username and password (the portal login, each up to 1000 characters in one line, kept exactly as sent, in plain text; every person of the circle sees it at once). The job receives a guid that identifies it on every instance. The result is the stored job, for a token with jobs.manage without username and password.",
        params: r#"{"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","interval_weeks":4,"retry_days":2,"next_due":"2026-11-06","username":"ada@example.org","password":"correct horse","instructions_md":"Download the PDF invoice and file it under Finance/Energy/2026."}"#,
        result: JOB,
    },
    Method {
        name: "job.delete",
        access: Access::Can(Cap::WriteJobs),
        summary: "Delete a job together with its history.",
        description: "The job's guid is kept as deleted, so that reading a file in later brings the job back only when the user decides so. This takes effect: confirm with the user first.",
        params: r#"{"job_id":7}"#,
        result: r#"{"job_id":7,"guid":"0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f","deleted":true}"#,
    },
    Method {
        name: "job.export",
        access: Access::Can(Cap::ExportJobs),
        summary: "Return every job with its logins, runs and the deleted jobs as a ZIP file, base64-encoded.",
        description: "The file holds the logins in plain text. With the header X-Backup-Password (8 to 200 characters) it is encrypted with AES-256; without it, it is not: tell the user before. The browser extension and every instance of this service read it with job.importPreview and job.import.",
        params: r#"{}"#,
        result: r#"{"file_name":"portal-download-jobs-2026-11-06.zip","encrypted":true,"zip_base64":"UEsDBBQAAQBjAA…"}"#,
    },
    Method {
        name: "job.get",
        access: Access::Can(Cap::ReadJobs),
        summary: "Read one job.",
        description: "Same fields as one entry of job.list.",
        params: r#"{"job_id":7}"#,
        result: JOB,
    },
    Method {
        name: "job.history",
        access: Access::Can(Cap::ReadJobs),
        summary: "List the closed runs of one job, newest first.",
        description: "Each run says who closed it (a person's name, or Agent with the token's display string), with which outcome and note, and how it moved the due date.",
        params: r#"{"job_id":7}"#,
        result: r#"{"job_id":7,"runs":[{"run_guid":"9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d","outcome":"failed","note":"Portal down","finished_at":"2026-11-06T09:00:00Z","due_before":"2026-11-01","due_after":"2026-11-08","by":"Ada"}]}"#,
    },
    Method {
        name: "job.import",
        access: Access::Can(Cap::ImportJobs),
        summary: "Read a file of jobs in, with a decision for every job of the group decide.",
        description: "zip_base64 is the file as job.importPreview saw it, the password in the header X-Backup-Password as there. decisions holds, for each guid of the group decide, one of three: {\"accept\":\"preselected\"}; or fields with \"local\" or \"incoming\" for every field the preview lists for it, for example {\"fields\":{\"password\":\"incoming\"}}; or one of the actions the preview lists for it, for example {\"action\":\"replace\"}. When one is missing, nothing is read in and decision_missing names the guids. The groups new and same are read in as the preview says. The result counts what happened. This takes effect: confirm with the user first.",
        params: r#"{"zip_base64":"UEsDBBQAAQBjAA…","decisions":{"0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f":{"accept":"preselected"}}}"#,
        result: IMPORTED,
    },
    Method {
        name: "job.importPreview",
        access: Access::Can(Cap::ImportJobs),
        summary: "Show what reading a file of jobs in would do, without changing anything.",
        description: "zip_base64 is a file from job.export, backup.export or the browser extension, base64-encoded; an encrypted one needs its password in the header X-Backup-Password. Every job of the file lands in one group. new: not known here; it is added with its runs. same: known here with equal fields; runs not known here are added (new_runs), and next_due_from_run is the due date that follows from a run newer than both sides' dates. decide: known here with differing fields, each with both values, the time each changed and the preselected side (the newer one; for next_due, the nearer date); or case deleted_here, a job deleted here, with the actions reactivate and skip; or case replaces, a job with a new address that replaces one here, with the actions replace, keep_both and skip. case old_address marks a job of the file that is known here under its new address. For a token with jobs.manage the logins in fields read •••.",
        params: r#"{"zip_base64":"UEsDBBQAAQBjAA…"}"#,
        result: PREVIEW,
    },
    Method {
        name: "job.list",
        access: Access::Can(Cap::ReadJobs),
        summary: "List all jobs, most urgent first.",
        description: "urgency_days is the number of days from today (the date in the server's local time zone) to next_due; negative means overdue. Tokens with jobs.read or jobs.manage receive every field except username and password; tokens with jobs.work or jobs.all, and every person, receive them too.",
        params: r#"{}"#,
        result: r#"{"today":"2026-11-06","jobs":[{"job_id":7,"guid":"0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f","replaces":null,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","interval_weeks":4,"retry_days":2,"next_due":"2026-11-01","urgency_days":-5,"username":"ada@example.org","password":"correct horse","instructions_md":"…","instructions_html":"<p>…</p>","last_run":null}]}"#,
    },
    Method {
        name: "job.update",
        access: Access::Can(Cap::WriteJobs),
        summary: "Change fields of a job.",
        description: "Send job_id and only the fields to change, with the types and limits job.create states; the others keep their value. An empty string removes a username or password. Setting next_due moves the job in the order of urgency. A new entry_url gives the job a new guid, and replaces names the old one; its history stays.",
        params: r#"{"job_id":7,"interval_weeks":6,"next_due":"2026-11-20"}"#,
        result: JOB,
    },
    Method {
        name: "service.status",
        access: Access::Open,
        summary: "Say whether the service is set up and who is calling.",
        description: "contract is the major and minor version of this interface; a caller built for another one says so to its user. ca_certificate, when not null, names the certificate authority this service offers for download and its SHA-256 fingerprint, which the user compares before trusting it. setup_required is true while nobody has been invited yet; the entry page then explains the setup. signed_in says whether the call carried a valid device credential or agent token; me describes the caller then: a person with their rights, an agent with its display string and its role (null until granted).",
        params: r#"{}"#,
        result: r#"{"name":"portal-download","version":"0.8.0","contract":"0.8","revision":"a1b2c3d4e5f6","setup_required":false,"ca_certificate":{"path":"/ca.crt","sha256":"5C:0E:…:7B"},"signed_in":true,"me":{"kind":"person","user_id":1,"name":"Ada","rights":{"people":true,"agents":true,"backup":true},"device_id":3,"device_label":"Laptop","device_kind":"browser"}}"#,
    },
    Method {
        name: "setup.createAdmin",
        access: Access::Setup,
        summary: "Create a person with all three rights, and their first invitation.",
        description: "Possible only while the setup window is open. Used for the very first person, and to get back in when every administrator has lost their devices. Every call creates a new person, also with a name that exists; email is optional. Open the invitation link built from invitation_token on the new person's device.",
        params: r#"{"name":"Ada","email":"ada@example.org"}"#,
        result: r#"{"user_id":1,"invitation_id":1,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.create",
        access: Access::Can(Cap::ManagePeople),
        summary: "Admit a new person and create their first invitation.",
        description: "rights is optional: {\"people\":bool,\"agents\":bool,\"backup\":bool}, the rights to manage people, agents and the backup; only rights the caller holds can be given. Every person works on all jobs. The invitation can be redeemed on each of the person's devices until it expires after seven days. Build the link as <base URL>/#invite=<invitation_token>, with invitation_token from this response; the token is shown only here.",
        params: r#"{"name":"Bob","email":"bob@example.org","rights":{"people":false,"agents":false,"backup":false}}"#,
        result: r#"{"user_id":2,"invitation_id":2,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.delete",
        access: Access::Can(Cap::ManagePeople),
        summary: "Delete a person together with their invitations and devices.",
        description: "Runs the person closed keep their name in the history. Nobody can delete themselves, and the last active person with all three rights cannot be deleted. To lock somebody out without deleting, use user.setActive. This takes effect: confirm with the user first.",
        params: r#"{"user_id":9}"#,
        result: r#"{"user_id":9,"deleted":true}"#,
    },
    Method {
        name: "user.list",
        access: Access::Can(Cap::ManagePeople),
        summary: "List all people with their rights, devices and invitations.",
        description: "Invitation state is open, used (a link for a further device, which serves once), expired or withdrawn; created_by names who invited. Never returns a token or credential.",
        params: r#"{}"#,
        result: r#"{"users":[{"user_id":2,"name":"Bob","email":"bob@example.org","rights":{"people":false,"agents":false,"backup":false},"active":true,"created_at":"2026-11-06T10:00:00Z","created_by":"Ada","devices":[],"invitations":[{"invitation_id":2,"created_at":"2026-11-06T10:00:00Z","expires_at":"2026-11-13T10:00:00Z","own":false,"state":"open","created_by":"Ada"}]}]}"#,
    },
    Method {
        name: "user.reinvite",
        access: Access::Can(Cap::ManagePeople),
        summary: "Create a new invitation for a person who already exists.",
        description: "Use it when an invitation has expired or a new device needs one; it does not create a second person. The token is shown only in this response.",
        params: r#"{"user_id":2}"#,
        result: r#"{"user_id":2,"invitation_id":5,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.setActive",
        access: Access::Can(Cap::ManagePeople),
        summary: "Deactivate or reactivate a person on all their devices at once.",
        description: "The last active person with all three rights cannot be deactivated. This takes effect: confirm with the user first.",
        params: r#"{"user_id":2,"active":false}"#,
        result: r#"{"user_id":2,"active":false}"#,
    },
    Method {
        name: "user.setRights",
        access: Access::Can(Cap::ManagePeople),
        summary: "Set a person's rights to manage people, agents and the backup.",
        description: "rights as for user.create, all three given. Only rights the caller holds can be given or taken away; one active person with all three always remains. This takes effect: confirm with the user first.",
        params: r#"{"user_id":2,"rights":{"people":true,"agents":false,"backup":false}}"#,
        result: r#"{"user_id":2,"rights":{"people":true,"agents":false,"backup":false}}"#,
    },
];

pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name == name)
}

/// What a call needs from its HTTP surroundings, established before dispatch.
pub struct Call<'a> {
    pub store: &'a Store,
    pub state_dir: &'a Path,
    pub principal: Option<Principal>,
    pub setup_secret: Option<&'a str>,
    /// The password of a ZIP file, from the header X-Backup-Password.
    pub file_password: Option<&'a str>,
    /// The SHA-256 fingerprint of the certificate authority offered at /ca.crt, if any.
    pub ca_fingerprint: Option<String>,
    pub user_agent: &'a str,
    pub address: &'a str,
    pub today: NaiveDate,
}

/// A side effect the surface performs after a successful call.
pub enum Effect {
    None,
    SetDeviceCredential(String),
    ClearDeviceCredential,
}

fn p<T: DeserializeOwned>(params: &Value) -> OpResult<T> {
    serde_json::from_value(params.clone()).map_err(|e| {
        OpError::bad("params_invalid", format!("The params do not fit this method: {e}. The service's machine contract shows an example for every method."))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceId {
    device_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobId {
    job_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserId {
    user_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationId {
    invitation_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentId {
    agent_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    agent_id: i64,
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    introduction: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAgent {
    introduction: String,
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    zip_base64: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    zip_base64: String,
    #[serde(default)]
    decisions: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Redeem {
    token: String,
    label: String,
    #[serde(default = "browser")]
    kind: String,
}
fn browser() -> String {
    "browser".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPerson {
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    rights: Rights,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupPerson {
    name: String,
    #[serde(default)]
    email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRights {
    user_id: i64,
    rights: Rights,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetActive {
    user_id: i64,
    active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Complete {
    job_id: i64,
    outcome: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    expected_next_due: Option<String>,
}
#[derive(Deserialize)]
struct JobUpdate {
    job_id: i64,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

fn decode(b64: &str) -> OpResult<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| OpError::bad("file_invalid", "`zip_base64` is not base64. Send the file's bytes base64-encoded, as job.export returns them."))
}

fn file_json(bytes: &[u8], name: &str, encrypted: bool) -> Value {
    json!({ "file_name": name, "encrypted": encrypted, "zip_base64": base64::engine::general_purpose::STANDARD.encode(bytes) })
}

/// The one place where access is checked, before any identifier in the params is resolved.
fn admit(m: &Method, c: &Call) -> OpResult<()> {
    let needs_credential = |agents: bool| {
        Err(OpError::unauthorized(
            "credential_missing",
            if agents {
                "This call needs a person's device credential or a granted agent token. A person opens the invitation link they received; an agent registers and sends its token as Authorization: Bearer, as the usage description explains."
            } else {
                "This call needs a person's device credential. Open the invitation link you received on this device; the application then holds the credential as a cookie, the browser extension as its own token."
            },
        ))
    };
    match (m.access, &c.principal) {
        (Access::Open, _) => Ok(()),
        (Access::Setup, _) => bootstrap::check(c.state_dir, c.setup_secret),
        (Access::Person, None) => needs_credential(false),
        (Access::Person, Some(pr)) => pr.require_person().map(|_| ()),
        (Access::Signed, None) => needs_credential(true),
        (Access::Signed, Some(Principal::Agent { role: None, display, .. })) => Err(OpError::new(
            403,
            "agent_pending",
            format!("This agent token has no role yet. Tell the user its display string {display} and ask them to have it granted in the application under Agenten."),
        )),
        (Access::Signed, Some(_)) => Ok(()),
        (Access::Can(cap), None) => needs_credential(!cap.roles().is_empty()),
        (Access::Can(cap), Some(pr)) => pr.require(cap),
    }
}

pub fn dispatch(name: &str, params: &Value, c: &Call) -> OpResult<(Value, Effect)> {
    let m = method(name).expect("dispatch is only called for known methods");
    admit(m, c)?;
    if let Some(pr) = &c.principal {
        if m.access != Access::Open {
            c.store.agent_touch(pr)?;
        }
    }
    let s = c.store;
    let pr = || c.principal.as_ref().expect("admitted calls carry a principal");
    let none = |v: Value| Ok((v, Effect::None));
    let day = c.today.format("%Y-%m-%d");
    match name {
        "service.status" => {
            p::<Empty>(params)?;
            let me = match &c.principal {
                Some(x) => Some(s.whoami(x)?),
                None => None,
            };
            none(json!({
                "name": "portal-download",
                "version": env!("CARGO_PKG_VERSION"),
                "contract": contract(),
                "revision": env!("PORTAL_DOWNLOAD_BUILD_REVISION"),
                "setup_required": !s.has_users()?,
                "ca_certificate": c.ca_fingerprint.as_ref().map(|fp| json!({ "path": "/ca.crt", "sha256": fp })),
                "signed_in": me.is_some(),
                "me": me,
            }))
        }
        "invitation.redeem" => {
            let a: Redeem = p(params)?;
            let (cred, mut v) = s.redeem_invitation(&a.token, &a.label, &a.kind, c.user_agent, c.address)?;
            if a.kind == "extension" {
                v["device_token"] = json!(cred);
                none(v)
            } else {
                Ok((v, Effect::SetDeviceCredential(cred)))
            }
        }
        "invitation.createOwn" => {
            p::<Empty>(params)?;
            none(s.invitation_create_own(pr())?)
        }
        "setup.createAdmin" => {
            let a: SetupPerson = p(params)?;
            none(s.setup_create_admin(&a.name, &a.email)?)
        }
        "agent.register" => {
            let a: Register = p(params)?;
            none(s.agent_register(&a.introduction)?)
        }
        "agent.create" => {
            let a: NewAgent = p(params)?;
            none(s.agent_create(pr(), &a.introduction, &a.role)?)
        }
        "agent.list" => {
            p::<Empty>(params)?;
            none(s.agent_list(pr())?)
        }
        "agent.grant" => {
            let a: Grant = p(params)?;
            none(s.agent_grant(pr(), a.agent_id, &a.role)?)
        }
        "agent.revoke" => {
            let a: AgentId = p(params)?;
            none(s.agent_revoke(pr(), a.agent_id)?)
        }
        "backup.export" => {
            p::<Empty>(params)?;
            let bytes = s.backup_export(pr(), c.file_password)?;
            none(file_json(&bytes, &format!("portal-download-{day}.zip"), c.file_password.is_some()))
        }
        "backup.importPreview" => {
            let a: Preview = p(params)?;
            none(s.import_preview(pr(), &decode(&a.zip_base64)?, c.file_password, ImportMode::Full)?)
        }
        "backup.import" => {
            let a: Import = p(params)?;
            none(s.import_apply(pr(), &decode(&a.zip_base64)?, c.file_password, ImportMode::Full, &a.decisions, c.today)?)
        }
        "device.list" => {
            p::<Empty>(params)?;
            none(s.device_list_own(pr())?)
        }
        "device.listAll" => {
            p::<Empty>(params)?;
            none(s.device_list_all(pr())?)
        }
        "device.revoke" => {
            let a: DeviceId = p(params)?;
            let v = s.device_revoke(pr(), a.device_id)?;
            let own = matches!(pr(), Principal::Device { device_id, .. } if *device_id == a.device_id);
            Ok((v, if own { Effect::ClearDeviceCredential } else { Effect::None }))
        }
        "user.list" => {
            p::<Empty>(params)?;
            none(s.user_list(pr())?)
        }
        "user.create" => {
            let a: NewPerson = p(params)?;
            none(s.user_create(pr(), &a.name, &a.email, a.rights)?)
        }
        "user.delete" => {
            let a: UserId = p(params)?;
            none(s.user_delete(pr(), a.user_id)?)
        }
        "user.reinvite" => {
            let a: UserId = p(params)?;
            none(s.user_reinvite(pr(), a.user_id)?)
        }
        "user.setRights" => {
            let a: SetRights = p(params)?;
            none(s.user_set_rights(pr(), a.user_id, a.rights)?)
        }
        "user.setActive" => {
            let a: SetActive = p(params)?;
            none(s.user_set_active(pr(), a.user_id, a.active)?)
        }
        "invitation.withdraw" => {
            let a: InvitationId = p(params)?;
            none(s.invitation_withdraw(pr(), a.invitation_id)?)
        }
        "job.list" => {
            p::<Empty>(params)?;
            none(s.job_list(pr(), c.today)?)
        }
        "job.get" => {
            let a: JobId = p(params)?;
            none(s.job_get(pr(), a.job_id, c.today)?)
        }
        "job.create" => {
            let a: JobInput = p(params)?;
            none(s.job_create(pr(), a, c.today)?)
        }
        "job.update" => {
            let a: JobUpdate = p(params)?;
            let fields: JobInput = p(&Value::Object(a.fields))?;
            none(s.job_update(pr(), a.job_id, fields, c.today)?)
        }
        "job.delete" => {
            let a: JobId = p(params)?;
            none(s.job_delete(pr(), a.job_id)?)
        }
        "job.complete" => {
            let a: Complete = p(params)?;
            none(s.job_complete(pr(), a.job_id, &a.outcome, &a.note, a.expected_next_due.as_deref(), c.today)?)
        }
        "job.history" => {
            let a: JobId = p(params)?;
            none(s.job_history(pr(), a.job_id)?)
        }
        "job.export" => {
            p::<Empty>(params)?;
            let bytes = s.job_export(pr(), c.file_password)?;
            none(file_json(&bytes, &format!("portal-download-jobs-{day}.zip"), c.file_password.is_some()))
        }
        "job.importPreview" => {
            let a: Preview = p(params)?;
            none(s.import_preview(pr(), &decode(&a.zip_base64)?, c.file_password, ImportMode::Jobs)?)
        }
        "job.import" => {
            let a: Import = p(params)?;
            none(s.import_apply(pr(), &decode(&a.zip_base64)?, c.file_password, ImportMode::Jobs, &a.decisions, c.today)?)
        }
        _ => unreachable!("every method in the table has a dispatch arm; the test every_method_is_dispatched holds this"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Role, ROLES};

    fn call<'a>(store: &'a Store, dir: &'a Path, principal: Option<Principal>) -> Call<'a> {
        Call { store, state_dir: dir, principal, setup_secret: None, file_password: None, ca_fingerprint: None, user_agent: "", address: "", today: crate::store::today() }
    }

    #[test]
    fn every_method_has_a_real_description_and_examples() {
        for m in METHODS {
            assert!(m.summary.len() > 10, "{} lacks a summary", m.name);
            assert!(m.description.len() > 10, "{} lacks a description", m.name);
            serde_json::from_str::<Value>(m.params).unwrap_or_else(|e| panic!("{} params example: {e}", m.name));
            serde_json::from_str::<Value>(m.result).unwrap_or_else(|e| panic!("{} result example: {e}", m.name));
            let (area, action) = m.name.split_once('.').expect("area.action");
            assert!(!area.is_empty() && !action.is_empty());
        }
    }

    #[test]
    fn method_table_is_sorted_so_it_reads_as_an_outline() {
        let names: Vec<_> = METHODS.iter().map(|m| m.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn every_method_is_dispatched() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        for m in METHODS {
            let c = call(&store, d.path(), None);
            // Without credentials, every method either answers or refuses access; none may reach the unreachable arm.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(m.name, &serde_json::from_str(m.params).unwrap(), &c).map(|_| ())));
            assert!(r.is_ok(), "{} panicked", m.name);
        }
    }

    /// Every role opens some method, and only job methods, the backup and the management of
    /// people and agents admit agents.
    #[test]
    fn every_role_opens_a_method_and_agents_reach_only_what_roles_name() {
        for (role, name, _) in ROLES {
            assert!(
                METHODS.iter().any(|m| matches!(m.access, Access::Can(cap) if cap.roles().contains(role))),
                "role {name} opens no method"
            );
        }
        for m in METHODS {
            if let Access::Can(cap) = m.access {
                for r in cap.roles() {
                    let area = m.name.split('.').next().unwrap();
                    let fits = match r {
                        Role::JobsRead | Role::JobsWork | Role::JobsManage | Role::JobsAll => area == "job",
                        Role::BackupRead => area == "backup",
                        Role::PeopleManage => area == "user" || area == "device" || area == "invitation",
                        Role::AgentsManage => area == "agent",
                    };
                    assert!(fits, "{} admits {}", m.name, r.name());
                }
            }
        }
    }

    /// Only calls an agent was admitted to count as use and keep its token from idling out.
    #[test]
    fn refused_calls_do_not_count_as_use() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let tok = store.agent_register("Agent of Ada; purpose: testing").unwrap()["token"].as_str().unwrap().to_string();
        let pr = store.authenticate_agent(&tok).unwrap();
        assert!(dispatch("job.list", &json!({}), &call(&store, d.path(), Some(pr))).is_err());
        assert!(store.agent_list_unchecked()[0]["last_used_at"].is_null());
    }

    #[test]
    fn access_is_refused_before_params_are_resolved() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        // job 99999 does not exist, but without a credential the answer must not say so.
        let e = dispatch("job.get", &json!({"job_id": 99999}), &call(&store, d.path(), None)).err().unwrap();
        assert_eq!(e.kind, "credential_missing");
    }

    #[test]
    fn agents_are_admitted_by_role_before_params_are_resolved() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let kind = |pr: Principal, name: &str, params: Value| dispatch(name, &params, &call(&store, d.path(), Some(pr))).err().map(|e| e.kind);
        let agent = |role: Option<Role>| Principal::Agent { agent_id: 1, display: "kluvmoth".into(), role, reach: vec![] };
        assert_eq!(kind(agent(None), "job.get", json!({"job_id": 99999})), Some("agent_pending"));
        assert_eq!(kind(agent(None), "device.revoke", json!({"device_id": 99999})), Some("agent_pending"));
        assert_eq!(kind(agent(Some(Role::BackupRead)), "job.get", json!({"job_id": 99999})), Some("right_missing"));
        assert_eq!(kind(agent(Some(Role::JobsRead)), "job.get", json!({"job_id": 99999})), Some("job_unknown"));
        assert_eq!(kind(agent(Some(Role::JobsRead)), "job.update", json!({"job_id": 99999, "title": "x"})), Some("right_missing"));
        assert_eq!(kind(agent(Some(Role::JobsManage)), "job.update", json!({"job_id": 99999, "title": "x"})), Some("job_unknown"));
        // Methods for people only refuse every agent token, whatever its role.
        for m in ["device.list", "invitation.createOwn"] {
            for (r, _, _) in ROLES {
                assert_eq!(kind(agent(Some(*r)), m, json!({})), Some("right_missing"), "{m}");
            }
        }
        assert_eq!(kind(agent(Some(Role::JobsAll)), "backup.importPreview", json!({"zip_base64": ""})), Some("right_missing"));
    }

    #[test]
    fn the_contract_is_major_and_minor() {
        assert_eq!(contract().matches('.').count(), 1);
        assert!(env!("CARGO_PKG_VERSION").starts_with(&contract()));
    }
}
