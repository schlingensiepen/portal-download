//! The JSON-RPC surface: a pass-through onto the operations of the store.
//!
//! The method table below is the single source for dispatch, for the access each method
//! needs, and for the machine contract the service delivers about itself.

use crate::bootstrap;
use crate::error::{OpError, OpResult};
use crate::store::{JobInput, Principal, Store};
use chrono::NaiveDate;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Callable without any credential.
    Open,
    /// Needs the setup secret of an open bootstrap window in the header X-Setup-Secret.
    Setup,
    /// Needs a person's device credential (the cookie the application receives on redeeming an invitation).
    Person,
    /// Needs a person's device credential and that person's right to administer people.
    Admin,
}

pub struct Method {
    pub name: &'static str,
    pub access: Access,
    /// The ability that admits an agent token to a method a person may call; None means people only.
    pub ability: Option<&'static str>,
    pub summary: &'static str,
    pub description: &'static str,
    pub params: &'static str,
    pub result: &'static str,
}

pub const METHODS: &[Method] = &[
    Method {
        name: "agent.create",
        access: Access::Admin,
        ability: None,
        summary: "Create an agent token with its ability at once, for a script that cannot register itself.",
        description: "introduction says what the token is for (10 to 500 characters); ability is one of those agent.grant names, and also backup.read for the whole backup. The token is shown only in this response; put it into the script's environment as PORTAL_DOWNLOAD_AGENT_TOKEN. This takes effect: confirm with the user first.",
        params: r#"{"introduction":"NAS: nightly backup","ability":"backup.read"}"#,
        result: r#"{"agent_id":5,"display":"pobrewsa","introduction":"NAS: nightly backup","state":"granted","ability":"backup.read","granted_by":"Ada","granted_at":"2026-11-06T10:00:00Z","created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2027-05-05T10:00:00Z","token":"pda_…"}"#,
    },
    Method {
        name: "agent.grant",
        access: Access::Admin,
        ability: None,
        summary: "Grant a waiting agent token exactly one ability.",
        description: "ability is one of jobs.read (job.list, job.get, job.history, without sealed logins), jobs.write (job.create, job.update, job.delete, without login fields), jobs.complete (job.complete) or backup.read (backup.export: the whole backup, including the sealed logins and the hashes of all accesses). The agent_id comes from agent.list; check its display string and introduction against what the agent told its user. A token's ability never changes afterwards; an agent that needs two abilities registers two tokens. This takes effect: confirm with the user first.",
        params: r#"{"agent_id":4,"ability":"jobs.write"}"#,
        result: r#"{"agent_id":4,"display":"kluvmoth","introduction":"Agent of Ada; purpose: add new portals","state":"granted","ability":"jobs.write","granted_by":"Ada","granted_at":"2026-11-06T10:05:00Z","created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2027-05-05T10:05:00Z"}"#,
    },
    Method {
        name: "agent.list",
        access: Access::Admin,
        ability: None,
        summary: "List agent tokens with display string, introduction, state and ability.",
        description: "state is pending (registered, no ability yet; expires 24 hours after registering), granted (expires 180 days after its last use), expired or revoked. Also lists the abilities that can be granted. Never returns a token itself.",
        params: r#"{}"#,
        result: r#"{"abilities":[{"ability":"jobs.read","permits":"read jobs, their instructions and history, without the sealed logins"}],"agents":[{"agent_id":4,"display":"kluvmoth","introduction":"Agent of Ada; purpose: add new portals","state":"pending","ability":null,"granted_by":null,"granted_at":null,"created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null,"expires_at":"2026-11-07T10:00:00Z"}]}"#,
    },
    Method {
        name: "agent.register",
        access: Access::Open,
        ability: None,
        summary: "Register an agent: receive a token without any ability, and its display string.",
        description: "introduction says whose agent this is and what for, in 10 to 500 characters; the person who grants the token reads it. The token is shown only in this response: store it at once outside the conversation, in the environment variable PORTAL_DOWNLOAD_AGENT_TOKEN, and send it as Authorization: Bearer with the token taken from this response. Tell the user only the display string and the one ability needed, and ask them to have a person with the right to administer people grant it in the application under Personen. Until then every call to a method the ability would open is refused with agent_pending; service.status shows the token's ability once granted. A token nobody grants expires after 24 hours. At most 10 registrations wait at the same time.",
        params: r#"{"introduction":"Agent of Ada; purpose: add new portals to the job list"}"#,
        result: r#"{"agent_id":4,"display":"kluvmoth","token":"pda_…","state":"pending","ability":null,"expires_at":"2026-11-07T10:00:00Z"}"#,
    },
    Method {
        name: "agent.revoke",
        access: Access::Admin,
        ability: None,
        summary: "Revoke one agent token at once, or reject a waiting one.",
        description: "Possible at any time; every other token keeps working. The agent_id comes from agent.list. This takes effect: confirm with the user first.",
        params: r#"{"agent_id":4}"#,
        result: r#"{"agent_id":4,"revoked":true}"#,
    },
    Method {
        name: "backup.export",
        access: Access::Admin,
        ability: Some("backup.read"),
        summary: "Return the whole backup as JSONL text.",
        description: "The first line is the head with format and version; every further line is one record: vault, person, invitation, device, agent, job or run. It holds the portal logins only sealed, readable only with the vault passphrase, and the hashes of all accesses, which grant no access; keep the file as carefully as a secret anyway. The same text comes as a file from GET on /backup of this service, with the same credential.",
        params: r#"{}"#,
        result: r#"{"jsonl":"{\"type\":\"backup\",\"format\":\"portal-download-backup\",\"version\":1,\"created_at\":\"2026-11-06T10:00:00Z\",\"app_version\":\"0.5.0\"}\n…"}"#,
    },
    Method {
        name: "backup.import",
        access: Access::Admin,
        ability: None,
        summary: "Add a backup to what is there, without overwriting anything.",
        description: "jsonl is the text backup.export returns. A job with the same title and entry address, or a person with the same name or e-mail, comes in a second time with (Import YYYY-MM-DD) after its name; renamed lists these entries so the user can delete the ones not needed. include_access false leaves people, invitations, devices and agent tokens out; runs then keep only the name of who closed them. With people and accesses, the file can bring in people with the right to administer people and working credentials: admins_added names those people, so import only a backup the user took from this service themselves. When this service has no vault yet it takes the one from the backup; when it has the same one the logins stay; otherwise, or when the file holds no vault, jobs come without their logins and logins_dropped counts them. The file is checked line by line; a faulty line rejects the whole import and nothing is kept. This takes effect: confirm with the user first.",
        params: r#"{"jsonl":"{\"type\":\"backup\",\"format\":\"portal-download-backup\",\"version\":1}\n…","include_access":true}"#,
        result: r#"{"vault":"same","people_added":1,"invitations_added":1,"devices_added":0,"agents_added":0,"jobs_added":12,"runs_added":140,"logins_dropped":0,"skipped_left_out":0,"skipped_existing":0,"skipped_unrelated":0,"admins_added":[{"user_id":9,"name":"Ada (Import 2026-11-06)"}],"renamed":[{"kind":"job","job_id":31,"name":"Energy invoice (Import 2026-11-06)"},{"kind":"person","user_id":9,"name":"Ada (Import 2026-11-06)"}]}"#,
    },
    Method {
        name: "device.list",
        access: Access::Person,
        ability: None,
        summary: "List the calling person's own devices.",
        description: "Returns every device registered with the calling person's invitations, marking the one the call came from. Use it to find a device_id for device.revoke.",
        params: r#"{}"#,
        result: r#"{"current_device_id":3,"devices":[{"device_id":3,"label":"Laptop","invitation_id":1,"user_agent":"Mozilla/5.0 …","last_address":"10.0.0.12","created_at":"2026-11-06T10:00:00Z","last_seen_at":"2026-11-06T10:05:00Z","revoked_at":null}]}"#,
    },
    Method {
        name: "device.revoke",
        access: Access::Person,
        ability: None,
        summary: "Revoke one device; the person's other devices keep working.",
        description: "Revoking the calling device signs it out and clears its cookie. Revoking a device of another person needs the right to administer people. This takes effect: confirm with the user first.",
        params: r#"{"device_id":3}"#,
        result: r#"{"device_id":3,"revoked":true}"#,
    },
    Method {
        name: "invitation.redeem",
        access: Access::Open,
        ability: None,
        summary: "Exchange an invitation token for this device's own credential.",
        description: "The token comes from the fragment of an invitation link (the part after #invite=). Each redemption registers one more device for the invited person; the response sets the device credential as an HttpOnly cookie. The label names the device for its owner.",
        params: r#"{"token":"pdi_Q2hhbmdlTWUuLi4","label":"Laptop Firefox"}"#,
        result: r#"{"user_id":1,"name":"Ada","device_id":3,"label":"Laptop Firefox"}"#,
    },
    Method {
        name: "invitation.withdraw",
        access: Access::Admin,
        ability: None,
        summary: "Stop an invitation from being redeemed again.",
        description: "Devices already registered with the invitation keep working; revoke them one by one with device.revoke if needed. The invitation_id comes from user.list. This takes effect: confirm with the user first.",
        params: r#"{"invitation_id":4}"#,
        result: r#"{"invitation_id":4,"withdrawn":true}"#,
    },
    Method {
        name: "job.complete",
        access: Access::Person,
        ability: Some("jobs.complete"),
        summary: "Close a download run as succeeded or failed and move the next due date.",
        description: "outcome is \"succeeded\" (next due date: today plus the job's interval in weeks) or \"failed\" (next due date: today plus the job's retry days); today is the date in the server's local time zone. The run is kept in the job's history with the optional note (up to 2000 characters). expected_run_count, the run_count value job.list returned, is optional; when sent and somebody else has closed a run of this job meanwhile, the call is refused instead of closing it twice. This takes effect: confirm with the user first.",
        params: r#"{"job_id":7,"outcome":"succeeded","note":"Invoice October filed","expected_run_count":3}"#,
        result: r#"{"job_id":7,"outcome":"succeeded","due_before":"2026-11-01","next_due":"2026-12-04"}"#,
    },
    Method {
        name: "job.create",
        access: Access::Person,
        ability: Some("jobs.write"),
        summary: "Create a download job.",
        description: "Required: title (text, 1 to 200 characters), entry_url (http or https address without spaces, up to 2000 characters), interval_weeks (integer 1 to 520). Optional: retry_days (integer 1 to 365, default 2), next_due (date YYYY-MM-DD, default today in the server's local time zone), embeddable (boolean, default false: whether the portal may be shown inside the application), instructions_md (Markdown up to 64 KiB saying what to file and where), username_sealed and password_sealed (empty, or sealed in the browser with the vault key as vault.create describes; the service cannot read them). An agent leaves both out; a person enters the login in the application. The result is the stored job; an agent token for jobs.write reads jobs later only with a second token for jobs.read.",
        params: r#"{"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","embeddable":false,"interval_weeks":4,"retry_days":2,"next_due":"2026-11-06","username_sealed":"{\"iv\":\"…\",\"ct\":\"…\"}","password_sealed":"{\"iv\":\"…\",\"ct\":\"…\"}","instructions_md":"Download the PDF invoice and file it under Finance/Energy/2026."}"#,
        result: r#"{"job_id":7,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","embeddable":false,"interval_weeks":4,"retry_days":2,"next_due":"2026-11-06","run_count":0,"urgency_days":0,"username_sealed":"…","password_sealed":"…","instructions_md":"…","instructions_html":"<p>…</p>","last_run":null}"#,
    },
    Method {
        name: "job.delete",
        access: Access::Person,
        ability: Some("jobs.write"),
        summary: "Delete a job together with its history.",
        description: "Cannot be undone. This takes effect: confirm with the user first.",
        params: r#"{"job_id":7}"#,
        result: r#"{"job_id":7,"deleted":true}"#,
    },
    Method {
        name: "job.get",
        access: Access::Person,
        ability: Some("jobs.read"),
        summary: "Read one job.",
        description: "Same fields as one entry of job.list.",
        params: r#"{"job_id":7}"#,
        result: r#"{"job_id":7,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","embeddable":false,"interval_weeks":4,"retry_days":2,"next_due":"2026-11-06","run_count":0,"urgency_days":0,"username_sealed":"…","password_sealed":"…","instructions_md":"…","instructions_html":"<p>…</p>","last_run":{"outcome":"succeeded","finished_at":"2026-10-09T08:00:00Z","note":"","by":"Ada"}}"#,
    },
    Method {
        name: "job.history",
        access: Access::Person,
        ability: Some("jobs.read"),
        summary: "List the closed runs of one job, newest first.",
        description: "Each run says who closed it (a person's name, or Agent with the token's display string), with which outcome and note, and how it moved the due date.",
        params: r#"{"job_id":7}"#,
        result: r#"{"job_id":7,"runs":[{"run_id":12,"outcome":"failed","note":"Portal down","finished_at":"2026-11-06T09:00:00Z","due_before":"2026-11-01","due_after":"2026-11-08","by":"Ada"}]}"#,
    },
    Method {
        name: "job.list",
        access: Access::Person,
        ability: Some("jobs.read"),
        summary: "List all jobs, most urgent first.",
        description: "urgency_days is the number of days from today (the date in the server's local time zone) to next_due; negative means overdue. An agent receives every field except username_sealed and password_sealed.",
        params: r#"{}"#,
        result: r#"{"today":"2026-11-06","jobs":[{"job_id":7,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","embeddable":false,"interval_weeks":4,"retry_days":2,"next_due":"2026-11-01","run_count":0,"urgency_days":-5,"username_sealed":"…","password_sealed":"…","instructions_md":"…","instructions_html":"<p>…</p>","last_run":null}]}"#,
    },
    Method {
        name: "job.update",
        access: Access::Person,
        ability: Some("jobs.write"),
        summary: "Change fields of a job.",
        description: "Send job_id and only the fields to change, with the types and limits job.create states; the others keep their value. Setting next_due moves the job in the order of urgency.",
        params: r#"{"job_id":7,"interval_weeks":6,"next_due":"2026-11-20"}"#,
        result: r#"{"job_id":7,"title":"Energy invoice","entry_url":"https://portal.example.org/invoices","embeddable":false,"interval_weeks":6,"retry_days":2,"next_due":"2026-11-20","run_count":0,"urgency_days":14,"username_sealed":"…","password_sealed":"…","instructions_md":"…","instructions_html":"<p>…</p>","last_run":null}"#,
    },
    Method {
        name: "service.status",
        access: Access::Open,
        ability: None,
        summary: "Say whether the service is set up and who is calling.",
        description: "setup_required is true while nobody has been invited yet; the entry page then explains the setup. signed_in says whether the call carried a valid device credential or agent token; me describes the caller then, for an agent with its display string and its ability (null until granted).",
        params: r#"{}"#,
        result: r#"{"name":"portal-download","version":"0.5.0","revision":"a1b2c3d4e5f6","setup_required":false,"signed_in":true,"me":{"kind":"person","user_id":1,"name":"Ada","is_admin":true,"device_id":3,"device_label":"Laptop"}}"#,
    },
    Method {
        name: "setup.createAdmin",
        access: Access::Setup,
        ability: None,
        summary: "Create a person with the right to administer people, and their first invitation.",
        description: "Possible only while the setup window is open. Used for the very first person, and to get back in when every administrator has lost their devices. Every call creates a new person, also with a name that exists; email is optional. Open the invitation link built from invitation_token on the new person's device.",
        params: r#"{"name":"Ada","email":"ada@example.org"}"#,
        result: r#"{"user_id":1,"invitation_id":1,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.create",
        access: Access::Admin,
        ability: None,
        summary: "Admit a new person and create their first invitation.",
        description: "The invitation can be redeemed on each of the person's devices until it expires after seven days. Build the link as <base URL>/#invite=<invitation_token>, with invitation_token from this response; the token is shown only here.",
        params: r#"{"name":"Bob","email":"bob@example.org","is_admin":false}"#,
        result: r#"{"user_id":2,"invitation_id":2,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.delete",
        access: Access::Admin,
        ability: None,
        summary: "Delete a person together with their invitations and devices.",
        description: "Runs the person closed keep their name in the history. Nobody can delete themselves, and the last active person with the right to administer people cannot be deleted. To lock somebody out without deleting, use user.setActive. This takes effect: confirm with the user first.",
        params: r#"{"user_id":9}"#,
        result: r#"{"user_id":9,"deleted":true}"#,
    },
    Method {
        name: "user.list",
        access: Access::Admin,
        ability: None,
        summary: "List all people with their devices and invitations.",
        description: "Invitation state is open, expired or withdrawn. Never returns a token or credential.",
        params: r#"{}"#,
        result: r#"{"users":[{"user_id":2,"name":"Bob","email":"bob@example.org","is_admin":false,"active":true,"created_at":"2026-11-06T10:00:00Z","devices":[],"invitations":[{"invitation_id":2,"created_at":"2026-11-06T10:00:00Z","expires_at":"2026-11-13T10:00:00Z","state":"open"}]}]}"#,
    },
    Method {
        name: "user.reinvite",
        access: Access::Admin,
        ability: None,
        summary: "Create a new invitation for a person who already exists.",
        description: "Use it when an invitation has expired or a new device needs one; it does not create a second person. The token is shown only in this response.",
        params: r#"{"user_id":2}"#,
        result: r#"{"user_id":2,"invitation_id":5,"invitation_token":"pdi_…","expires_at":"2026-11-13T10:00:00Z"}"#,
    },
    Method {
        name: "user.setActive",
        access: Access::Admin,
        ability: None,
        summary: "Deactivate or reactivate a person on all their devices at once.",
        description: "The last active person with the right to administer people cannot be deactivated. This takes effect: confirm with the user first.",
        params: r#"{"user_id":2,"active":false}"#,
        result: r#"{"user_id":2,"active":false}"#,
    },
    Method {
        name: "user.setAdmin",
        access: Access::Admin,
        ability: None,
        summary: "Grant or remove the right to administer people.",
        description: "The right allows inviting, withdrawing invitations, revoking other people's devices and deactivating people. The last active holder cannot lose it. This takes effect: confirm with the user first.",
        params: r#"{"user_id":2,"is_admin":true}"#,
        result: r#"{"user_id":2,"is_admin":true}"#,
    },
    Method {
        name: "vault.create",
        access: Access::Person,
        ability: None,
        summary: "Store the parameters of the shared vault, once.",
        description: "The browser derives the vault key from the shared passphrase with PBKDF2-SHA256 over the salt (16 random bytes, standard Base64) and the iterations, as an AES-256-GCM key. A sealed value is the JSON text {\"v\":1,\"iv\":…,\"ct\":…} with a random 12-byte iv and the ciphertext including its tag, both standard Base64, and the purpose (\"username\", \"password\" or \"verifier\") as additional authenticated data. The verifier seals the text \"portal-download vault check\". The service keeps salt, iterations and verifier and never sees passphrase or key. Jobs work without a vault; they then hold no logins.",
        params: r#"{"salt":"bm90LWEtcmVhbC1zYWx0","iterations":600000,"verifier":"{\"iv\":\"…\",\"ct\":\"…\"}"}"#,
        result: r#"{"exists":true,"salt":"bm90LWEtcmVhbC1zYWx0","iterations":600000,"verifier":"{\"iv\":\"…\",\"ct\":\"…\"}","created_at":"2026-11-06T10:00:00Z"}"#,
    },
    Method {
        name: "vault.get",
        access: Access::Person,
        ability: None,
        summary: "Read the parameters needed to unlock the vault in the browser.",
        description: "exists is false until somebody calls vault.create.",
        params: r#"{}"#,
        result: r#"{"exists":true,"salt":"bm90LWEtcmVhbC1zYWx0","iterations":600000,"verifier":"{\"iv\":\"…\",\"ct\":\"…\"}","created_at":"2026-11-06T10:00:00Z"}"#,
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
    ability: String,
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
    ability: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    jsonl: String,
    #[serde(default = "yes")]
    include_access: bool,
}
fn yes() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Redeem {
    token: String,
    label: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPerson {
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    is_admin: bool,
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
struct SetAdmin {
    user_id: i64,
    is_admin: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetActive {
    user_id: i64,
    active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewVault {
    salt: String,
    iterations: i64,
    verifier: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Complete {
    job_id: i64,
    outcome: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    expected_run_count: Option<i64>,
}
#[derive(Deserialize)]
struct JobUpdate {
    job_id: i64,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

/// The one place where access is checked, before any identifier in the params is resolved.
fn admit(m: &Method, c: &Call) -> OpResult<()> {
    match m.access {
        Access::Open => Ok(()),
        Access::Setup => bootstrap::check(c.state_dir, c.setup_secret),
        Access::Person | Access::Admin => match &c.principal {
            None => Err(OpError::unauthorized(
                "credential_missing",
                match m.ability {
                    Some(_) => "This call needs a person's device credential or a granted agent token. A person opens the invitation link they received; an agent registers and sends its token as Authorization: Bearer, as the usage description explains.",
                    None => "This call needs a person's device credential. Open the invitation link you received on this device; the application then holds the credential as a cookie.",
                },
            )),
            Some(pr @ Principal::Agent { .. }) => match m.ability {
                Some(ability) => pr.require_ability(ability),
                None => pr.require_person().map(|_| ()),
            },
            Some(pr) => {
                if m.access == Access::Admin {
                    pr.require_admin()?;
                }
                Ok(())
            }
        },
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
                "revision": env!("PORTAL_DOWNLOAD_BUILD_REVISION"),
                "setup_required": !s.has_users()?,
                "signed_in": me.is_some(),
                "me": me,
            }))
        }
        "invitation.redeem" => {
            let a: Redeem = p(params)?;
            let (cred, v) = s.redeem_invitation(&a.token, &a.label, c.user_agent, c.address)?;
            Ok((v, Effect::SetDeviceCredential(cred)))
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
            none(s.agent_create(pr(), &a.introduction, &a.ability)?)
        }
        "backup.export" => {
            p::<Empty>(params)?;
            none(json!({ "jsonl": s.backup_export(pr())? }))
        }
        "backup.import" => {
            let a: Import = p(params)?;
            none(s.backup_import(pr(), &a.jsonl, a.include_access, c.today)?)
        }
        "user.delete" => {
            let a: UserId = p(params)?;
            none(s.user_delete(pr(), a.user_id)?)
        }
        "agent.list" => {
            p::<Empty>(params)?;
            none(s.agent_list(pr())?)
        }
        "agent.grant" => {
            let a: Grant = p(params)?;
            none(s.agent_grant(pr(), a.agent_id, &a.ability)?)
        }
        "agent.revoke" => {
            let a: AgentId = p(params)?;
            none(s.agent_revoke(pr(), a.agent_id)?)
        }
        "device.list" => {
            p::<Empty>(params)?;
            none(s.device_list_own(pr())?)
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
            none(s.user_create(pr(), &a.name, &a.email, a.is_admin)?)
        }
        "user.reinvite" => {
            let a: UserId = p(params)?;
            none(s.user_reinvite(pr(), a.user_id)?)
        }
        "user.setAdmin" => {
            let a: SetAdmin = p(params)?;
            none(s.user_set_admin(pr(), a.user_id, a.is_admin)?)
        }
        "user.setActive" => {
            let a: SetActive = p(params)?;
            none(s.user_set_active(pr(), a.user_id, a.active)?)
        }
        "invitation.withdraw" => {
            let a: InvitationId = p(params)?;
            none(s.invitation_withdraw(pr(), a.invitation_id)?)
        }
        "vault.get" => {
            p::<Empty>(params)?;
            none(s.vault_get(pr())?)
        }
        "vault.create" => {
            let a: NewVault = p(params)?;
            none(s.vault_create(pr(), &a.salt, a.iterations, &a.verifier)?)
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
            none(s.job_complete(pr(), a.job_id, &a.outcome, &a.note, a.expected_run_count, c.today)?)
        }
        "job.history" => {
            let a: JobId = p(params)?;
            none(s.job_history(pr(), a.job_id, c.today)?)
        }
        _ => unreachable!("every method in the table has a dispatch arm; the test every_method_is_dispatched holds this"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let c = Call { store: &store, state_dir: d.path(), principal: None, setup_secret: None, user_agent: "", address: "", today: crate::store::today() };
            // Without credentials, every method either answers or refuses access; none may reach the unreachable arm.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(m.name, &serde_json::from_str(m.params).unwrap(), &c).map(|_| ())));
            assert!(r.is_ok(), "{} panicked", m.name);
        }
    }

    /// Every ability the table names exists, and only job methods and the backup admit agents.
    #[test]
    fn abilities_in_the_table_exist_and_open_job_methods_and_backup_only() {
        for m in METHODS {
            if let Some(a) = m.ability {
                assert!(crate::store::ABILITIES.iter().any(|(n, _)| *n == a), "{} names unknown ability {a}", m.name);
                assert!(m.name.starts_with("job.") || m.name == "backup.export", "{} admits agents", m.name);
            }
        }
        for (a, _) in crate::store::ABILITIES {
            assert!(METHODS.iter().any(|m| m.ability == Some(a)), "ability {a} opens no method");
        }
    }

    /// Only calls an agent was admitted to count as use and keep its token from idling out.
    #[test]
    fn refused_calls_do_not_count_as_use() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let tok = store.agent_register("Agent of Ada; purpose: testing").unwrap()["token"].as_str().unwrap().to_string();
        let pr = store.authenticate_agent(&tok).unwrap();
        let c = Call { store: &store, state_dir: d.path(), principal: Some(pr), setup_secret: None, user_agent: "", address: "", today: crate::store::today() };
        assert!(dispatch("job.list", &json!({}), &c).is_err());
        assert!(store.agent_list_unchecked()[0]["last_used_at"].is_null());
    }

    #[test]
    fn access_is_refused_before_params_are_resolved() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let c = Call { store: &store, state_dir: d.path(), principal: None, setup_secret: None, user_agent: "", address: "", today: crate::store::today() };
        // job 99999 does not exist, but without a credential the answer must not say so.
        let e = dispatch("job.get", &json!({"job_id": 99999}), &c).err().unwrap();
        assert_eq!(e.kind, "credential_missing");
    }

    #[test]
    fn agents_are_admitted_by_ability_before_params_are_resolved() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let call = |pr: Principal, name: &str, params: Value| {
            let c = Call { store: &store, state_dir: d.path(), principal: Some(pr), setup_secret: None, user_agent: "", address: "", today: crate::store::today() };
            dispatch(name, &params, &c).err().map(|e| e.kind)
        };
        let agent = |ability: Option<&str>| Principal::Agent { agent_id: 1, display: "kluvmoth".into(), ability: ability.map(str::to_string) };
        assert_eq!(call(agent(None), "job.get", json!({"job_id": 99999})), Some("agent_pending"));
        assert_eq!(call(agent(Some("jobs.write")), "job.get", json!({"job_id": 99999})), Some("right_missing"));
        assert_eq!(call(agent(Some("jobs.read")), "job.get", json!({"job_id": 99999})), Some("job_unknown"));
        // Methods for people only refuse every agent token, whatever its ability.
        for m in ["user.list", "vault.get", "agent.list", "device.list"] {
            assert_eq!(call(agent(Some("jobs.read")), m, json!({})), Some("right_missing"), "{m}");
        }
    }
}
