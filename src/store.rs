//! The store and every operation on it. Operations know neither HTTP nor process exit:
//! they take arguments, act, and return a result or an [`OpError`].

use crate::error::{OpError, OpResult};
use crate::schedule::{self, Outcome};
use crate::token;
use chrono::{Duration, NaiveDate, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

/// How long an invitation link can be redeemed. Decided with the operator: seven days.
pub const INVITATION_DAYS: i64 = 7;
/// The abilities an agent token can carry, one per token, with what each permits.
pub const ABILITIES: &[(&str, &str)] = &[
    ("jobs.read", "read jobs, their instructions and history, without the sealed logins"),
    ("jobs.write", "create, change and delete jobs, without setting logins"),
    ("jobs.complete", "close runs as succeeded or failed"),
    ("backup.read", "download the whole backup, including the sealed logins and the hashes of all accesses"),
];
/// A registered agent token that nobody grants an ability expires after this many hours.
pub const AGENT_PENDING_HOURS: i64 = 24;
/// A granted agent token expires after this many days without use.
pub const AGENT_IDLE_DAYS: i64 = 180;
/// At most this many registrations wait for a grant at the same time.
pub const AGENT_PENDING_MAX: i64 = 10;

const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// Who is calling, established before any method runs.
#[derive(Debug, Clone)]
pub enum Principal {
    Device { device_id: i64, user_id: i64, is_admin: bool, name: String },
    /// An agent token; `ability` is None until a person grants one.
    Agent { agent_id: i64, display: String, ability: Option<String> },
}

impl Principal {
    pub fn require_person(&self) -> OpResult<(i64, i64, bool)> {
        match self {
            Principal::Device { device_id, user_id, is_admin, .. } => Ok((*device_id, *user_id, *is_admin)),
            Principal::Agent { .. } => Err(OpError::forbidden(
                "This call is reserved for people; an agent token cannot make it. Ask the user to do it in the application.",
            )),
        }
    }
    /// Admits a person, or an agent whose token carries exactly this ability.
    pub fn require_ability(&self, ability: &str) -> OpResult<()> {
        match self {
            Principal::Device { .. } => Ok(()),
            Principal::Agent { ability: Some(a), .. } if a == ability => Ok(()),
            Principal::Agent { ability: Some(a), .. } => Err(OpError::forbidden(format!(
                "This agent token carries the ability {a}; this call needs {ability}. Register a second token for {ability} and have it granted."
            ))),
            Principal::Agent { display, ability: None, .. } => Err(OpError::new(
                403,
                "agent_pending",
                format!("This agent token has no ability yet. Tell the user its display string {display} and ask them to have a person with the right to administer people grant it {ability} in the application under Personen."),
            )),
        }
    }
    /// Only a person's device sees the sealed logins; agents never do.
    pub fn sees_sealed(&self) -> bool {
        matches!(self, Principal::Device { .. })
    }
    pub fn require_admin(&self) -> OpResult<i64> {
        let (_, user_id, is_admin) = self.require_person()?;
        if !is_admin {
            return Err(OpError::forbidden(
                "This call needs the right to administer people. Ask a person with that right to do it, or to grant you the right.",
            ));
        }
        Ok(user_id)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct JobInput {
    pub title: Option<String>,
    pub entry_url: Option<String>,
    pub embeddable: Option<bool>,
    pub interval_weeks: Option<u32>,
    pub retry_days: Option<u32>,
    pub next_due: Option<String>,
    pub username_sealed: Option<String>,
    pub password_sealed: Option<String>,
    pub instructions_md: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Job {
    pub job_id: i64,
    pub title: String,
    pub entry_url: String,
    pub embeddable: bool,
    pub interval_weeks: u32,
    pub retry_days: u32,
    pub next_due: String,
    pub urgency_days: i64,
    /// Number of closed runs; sent back with job.complete to detect a run closed meanwhile.
    pub run_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_sealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_sealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions_html: Option<String>,
    pub last_run: Option<Value>,
}

pub fn render_markdown(md: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(md, pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    ammonia::clean(&html)
}

fn parse_date(field: &str, s: &str) -> OpResult<NaiveDate> {
    let bad = || OpError::bad("date_invalid", format!("`{field}` = \"{s}\" is not a date. Write it as YYYY-MM-DD, for example \"2026-12-01\"."));
    use chrono::Datelike;
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| bad())?;
    // Stored dates are compared as text, so only the canonical form with a four-digit year is accepted.
    if !(2000..=2200).contains(&d.year()) || d.format("%Y-%m-%d").to_string() != s {
        return Err(bad());
    }
    Ok(d)
}

fn check_sealed(field: &str, s: &str) -> OpResult<()> {
    if s.is_empty() {
        return Ok(());
    }
    let bad = || {
        OpError::bad(
            "sealed_invalid",
            format!("`{field}` must be empty or a value sealed in the browser with the vault key: a JSON object whose fields `iv` (12 bytes) and `ct` (ciphertext with its 16-byte tag) are standard Base64. Unlock the vault in the application and save the job again."),
        )
    };
    if s.len() > 8192 {
        return Err(bad());
    }
    let v: Value = serde_json::from_str(s).map_err(|_| bad())?;
    use base64::Engine;
    let b64 = |k: &str| v.get(k).and_then(Value::as_str).and_then(|x| base64::engine::general_purpose::STANDARD.decode(x).ok());
    match (b64("iv"), b64("ct")) {
        // AES-GCM: a 12-byte nonce, and a ciphertext that at least carries its 16-byte tag.
        (Some(iv), Some(ct)) if iv.len() == 12 && ct.len() >= 16 => Ok(()),
        _ => Err(bad()),
    }
}

fn text(field: &str, s: &str, min: usize, max: usize) -> OpResult<String> {
    let t = s.trim();
    if t.chars().count() < min || t.chars().count() > max {
        return Err(OpError::bad(
            "field_length",
            format!("`{field}` must have between {min} and {max} characters; it has {}. Shorten or fill it and send again.", t.chars().count()),
        ));
    }
    Ok(t.to_string())
}

/// Who closed a run, as recorded when it was closed: a person's name, or "Agent" with the
/// token's display string. Kept with the run, so deleting a person keeps the history readable.
const RUN_BY: &str = "r.by_name";

fn plus(ts: &str, d: Duration) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| (t.with_timezone(&Utc) + d).to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|_| ts.to_string())
}

fn ago(d: Duration) -> String {
    (Utc::now() - d).to_rfc3339_opts(SecondsFormat::Secs, true)
}

const AGENT_COLS: &str = "a.id, a.display, a.introduction, a.ability, a.granted_at, u.name, a.created_at, a.last_used_at, a.revoked_at";

struct AgentRow {
    id: i64,
    display: String,
    introduction: String,
    ability: Option<String>,
    granted_at: Option<String>,
    granted_by: Option<String>,
    created_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

fn agent_row(r: &rusqlite::Row) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: r.get(0)?,
        display: r.get(1)?,
        introduction: r.get(2)?,
        ability: r.get(3)?,
        granted_at: r.get(4)?,
        granted_by: r.get(5)?,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
        revoked_at: r.get(8)?,
    })
}

impl AgentRow {
    /// When the token expires unless it is used (granted) or granted (pending).
    fn expires_at(&self) -> String {
        match (&self.ability, &self.granted_at) {
            (Some(_), Some(g)) => {
                let last = self.last_used_at.as_ref().filter(|u| *u > g).unwrap_or(g);
                plus(last, Duration::days(AGENT_IDLE_DAYS))
            }
            _ => plus(&self.created_at, Duration::hours(AGENT_PENDING_HOURS)),
        }
    }
    fn state(&self) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at() < now() {
            "expired"
        } else if self.ability.is_some() {
            "granted"
        } else {
            "pending"
        }
    }
    fn to_json(&self) -> Value {
        json!({
            "agent_id": self.id, "display": self.display, "introduction": self.introduction,
            "state": self.state(), "ability": self.ability, "granted_by": self.granted_by, "granted_at": self.granted_at,
            "created_at": self.created_at, "last_used_at": self.last_used_at, "revoked_at": self.revoked_at,
            "expires_at": self.expires_at(),
        })
    }
}

impl Store {
    pub fn open(state_dir: &Path) -> OpResult<Self> {
        std::fs::create_dir_all(state_dir).map_err(|e| {
            OpError::internal(format!("The state directory {} cannot be created: {e}. Create it and make it writable for the service user.", state_dir.display()))
        })?;
        let conn = Connection::open(state_dir.join("portal-download.sqlite"))?;
        Self::from_conn(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> OpResult<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> OpResult<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        let mut s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    /// Data migration runs at start, before the service listens.
    fn migrate(&mut self) -> OpResult<()> {
        let v: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(OpError::internal(format!(
                "The store has structure version {v}, this program knows up to {SCHEMA_VERSION}. Run the newer program version that wrote it."
            )));
        }
        if v < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                r#"
CREATE TABLE users(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL DEFAULT '',
  is_admin INTEGER NOT NULL DEFAULT 0, deactivated_at TEXT, created_at TEXT NOT NULL);
CREATE TABLE invitations(
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE, created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, withdrawn_at TEXT);
CREATE TABLE devices(
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  invitation_id INTEGER NOT NULL REFERENCES invitations(id) ON DELETE CASCADE,
  credential_hash TEXT NOT NULL UNIQUE, label TEXT NOT NULL,
  user_agent TEXT NOT NULL DEFAULT '', last_address TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, revoked_at TEXT);
CREATE TABLE agents(
  id INTEGER PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, display TEXT NOT NULL,
  introduction TEXT NOT NULL, ability TEXT, granted_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
  granted_at TEXT, created_at TEXT NOT NULL, last_used_at TEXT, revoked_at TEXT);
CREATE TABLE vault(
  id INTEGER PRIMARY KEY CHECK (id = 1), salt TEXT NOT NULL, iterations INTEGER NOT NULL,
  verifier TEXT NOT NULL, created_by INTEGER REFERENCES users(id) ON DELETE SET NULL, created_at TEXT NOT NULL);
CREATE TABLE jobs(
  id INTEGER PRIMARY KEY, title TEXT NOT NULL, entry_url TEXT NOT NULL,
  embeddable INTEGER NOT NULL DEFAULT 0, interval_weeks INTEGER NOT NULL,
  retry_days INTEGER NOT NULL DEFAULT 2, next_due TEXT NOT NULL,
  username_sealed TEXT NOT NULL DEFAULT '', password_sealed TEXT NOT NULL DEFAULT '',
  instructions_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE runs(
  id INTEGER PRIMARY KEY, job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  device_id INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  agent_id INTEGER REFERENCES agents(id) ON DELETE SET NULL,
  by_name TEXT NOT NULL,
  outcome TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', finished_at TEXT NOT NULL,
  due_before TEXT NOT NULL, due_after TEXT NOT NULL);
PRAGMA user_version = 1;
"#,
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Writes the backup to a new file, readable by its owner only; never overwrites.
    pub fn backup_to_file(&self, target: &Path) -> OpResult<Value> {
        if target.exists() {
            return Err(OpError::conflict("backup_exists", format!("{} exists already. Name a new file, for example with the date in its name; a backup never overwrites.", target.display())));
        }
        let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if !dir.is_dir() {
            return Err(OpError::bad("backup_dir_missing", format!("The directory {} does not exist. Create it, or name a file in an existing directory.", dir.display())));
        }
        let text = self.backup_text()?;
        let failed = |e: &dyn std::fmt::Display| OpError::internal(format!("The backup to {} failed: {e}. Check that the directory is writable and has room.", target.display()));
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let partial = dir.join(format!(".{name}.partial"));
        let _ = std::fs::remove_file(&partial);
        {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&partial).map_err(|e| failed(&e))?;
            f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| failed(&e))?;
        }
        std::fs::rename(&partial, target).map_err(|e| failed(&e))?;
        Ok(json!({ "backup": target.display().to_string(), "bytes": text.len(), "lines": text.lines().count() }))
    }

    pub fn schema_version(&self) -> OpResult<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    // ---------------------------------------------------------------- status

    pub fn has_users(&self) -> OpResult<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |r| r.get(0))?)
    }

    pub fn has_active_admin(&self) -> OpResult<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin = 1 AND deactivated_at IS NULL)", [], |r| r.get(0))?)
    }

    pub fn counts(&self) -> OpResult<Value> {
        let q = |sql: &str| -> OpResult<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        let today = today().format("%Y-%m-%d").to_string();
        let due: i64 = self.conn.query_row("SELECT COUNT(*) FROM jobs WHERE next_due <= ?1", [today], |r| r.get(0))?;
        Ok(json!({
            "schema_version": self.schema_version()?,
            "users": q("SELECT COUNT(*) FROM users WHERE deactivated_at IS NULL")?,
            "admins": q("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND deactivated_at IS NULL")?,
            "devices": q("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL")?,
            "jobs": q("SELECT COUNT(*) FROM jobs")?,
            "jobs_due": due,
            "vault_exists": q("SELECT COUNT(*) FROM vault")? > 0,
        }))
    }

    // ---------------------------------------------------------------- access

    /// Resolves a device credential: credential → device not revoked → user not deactivated.
    pub fn authenticate_device(&self, credential: &str, user_agent: &str, address: &str) -> OpResult<Principal> {
        let row = self
            .conn
            .query_row(
                "SELECT d.id, d.user_id, d.revoked_at, u.deactivated_at, u.is_admin, u.name
                 FROM devices d JOIN users u ON u.id = d.user_id WHERE d.credential_hash = ?1",
                [token::hash(credential)],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, bool>(4)?, r.get::<_, String>(5)?)),
            )
            .optional()?;
        let Some((device_id, user_id, revoked, deactivated, is_admin, name)) = row else {
            return Err(OpError::unauthorized(
                "credential_unknown",
                "This browser holds no valid access. Open your invitation link again on this device, or ask a person in the circle for a new one.",
            ));
        };
        if revoked.is_some() {
            return Err(OpError::unauthorized(
                "device_revoked",
                "Access from this device was revoked. Open your invitation link again to register it anew, or ask for a new invitation.",
            ));
        }
        if deactivated.is_some() {
            return Err(OpError::unauthorized("user_deactivated", "Your access was deactivated. Ask a person with the right to administer people to reactivate you."));
        }
        let ua: String = user_agent.chars().take(400).collect();
        let addr: String = address.chars().take(100).collect();
        self.conn.execute("UPDATE devices SET last_seen_at = ?1, user_agent = ?2, last_address = ?3 WHERE id = ?4", params![now(), ua, addr, device_id])?;
        Ok(Principal::Device { device_id, user_id, is_admin, name })
    }

    /// Resolves an agent token. A token without a grant is admitted as a principal without
    /// ability, so the caller learns its state; an expired or revoked one is refused.
    pub fn authenticate_agent(&self, token: &str) -> OpResult<Principal> {
        let row = self.conn.query_row(
            &format!("SELECT {} FROM agents a LEFT JOIN users u ON u.id = a.granted_by WHERE a.token_hash = ?1", AGENT_COLS),
            [token::hash(token)],
            agent_row,
        ).optional()?;
        let Some(a) = row else {
            return Err(OpError::unauthorized(
                "agent_token_unknown",
                "The token in the Authorization header is not known. Check that the whole token was sent, including its prefix; if it is lost, register a new one.",
            ));
        };
        match a.state() {
            "revoked" => Err(OpError::unauthorized("agent_token_revoked", "This agent token was revoked. Register a new one and ask the user to have it granted.")),
            "expired" => Err(OpError::unauthorized(
                "agent_token_expired",
                format!("This agent token has expired: tokens without a grant expire after {AGENT_PENDING_HOURS} hours, granted ones after {AGENT_IDLE_DAYS} days without use. Register a new one."),
            )),
            _ => {
                self.conn.execute("UPDATE agents SET last_used_at = ?1 WHERE id = ?2", params![now(), a.id])?;
                Ok(Principal::Agent { agent_id: a.id, display: a.display, ability: a.ability })
            }
        }
    }

    fn new_invitation(&self, user_id: i64, created_by: Option<i64>) -> OpResult<(i64, String, String)> {
        let tok = token::generate("pdi_");
        let expires = (Utc::now() + Duration::days(INVITATION_DAYS)).to_rfc3339_opts(SecondsFormat::Secs, true);
        self.conn.execute(
            "INSERT INTO invitations(user_id, token_hash, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![user_id, token::hash(&tok), created_by, now(), expires],
        )?;
        Ok((self.conn.last_insert_rowid(), tok, expires))
    }

    /// Exchanges an invitation token for a device credential. Each redemption creates a device.
    pub fn redeem_invitation(&self, invitation_token: &str, label: &str, user_agent: &str, address: &str) -> OpResult<(String, Value)> {
        let label = text("label", label, 1, 80)?;
        let row = self
            .conn
            .query_row(
                "SELECT i.id, i.user_id, i.expires_at, i.withdrawn_at, u.deactivated_at, u.name
                 FROM invitations i JOIN users u ON u.id = i.user_id WHERE i.token_hash = ?1",
                [token::hash(invitation_token)],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, String>(5)?)),
            )
            .optional()?;
        let Some((inv_id, user_id, expires_at, withdrawn, deactivated, name)) = row else {
            return Err(OpError::not_found("invitation_unknown", "This invitation link is not known. Check that the link was copied completely, or ask for a new one."));
        };
        if withdrawn.is_some() {
            return Err(OpError::gone("invitation_withdrawn", "This invitation was withdrawn. Ask a person with the right to administer people for a new invitation."));
        }
        if expires_at < now() {
            return Err(OpError::gone("invitation_expired", "This invitation has expired. Ask a person with the right to administer people for a new invitation."));
        }
        if deactivated.is_some() {
            return Err(OpError::gone("user_deactivated", "The person this invitation is for was deactivated. Ask a person with the right to administer people."));
        }
        let cred = token::generate("pdd_");
        let ts = now();
        self.conn.execute(
            "INSERT INTO devices(user_id, invitation_id, credential_hash, label, user_agent, last_address, created_at, last_seen_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
            params![user_id, inv_id, token::hash(&cred), label, user_agent.chars().take(400).collect::<String>(), address.chars().take(100).collect::<String>(), ts],
        )?;
        Ok((cred, json!({ "user_id": user_id, "name": name, "device_id": self.conn.last_insert_rowid(), "label": label })))
    }

    pub fn whoami(&self, p: &Principal) -> OpResult<Value> {
        match p {
            Principal::Device { device_id, user_id, is_admin, name } => {
                let label: String = self.conn.query_row("SELECT label FROM devices WHERE id = ?1", [device_id], |r| r.get(0))?;
                Ok(json!({ "kind": "person", "user_id": user_id, "name": name, "is_admin": is_admin, "device_id": device_id, "device_label": label }))
            }
            Principal::Agent { agent_id, display, ability } => Ok(json!({ "kind": "agent", "agent_id": agent_id, "display": display, "ability": ability })),
        }
    }

    // ---------------------------------------------------------------- people

    /// Bootstrap only: creates a person with the right to administer people, and their first invitation.
    pub fn setup_create_admin(&self, name: &str, email: &str) -> OpResult<Value> {
        let name = text("name", name, 1, 120)?;
        let email = text("email", email, 0, 200)?;
        self.conn.execute("INSERT INTO users(name, email, is_admin, created_at) VALUES (?1, ?2, 1, ?3)", params![name, email, now()])?;
        let user_id = self.conn.last_insert_rowid();
        let (invitation_id, tok, expires) = self.new_invitation(user_id, None)?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    pub fn user_create(&self, p: &Principal, name: &str, email: &str, is_admin: bool) -> OpResult<Value> {
        let by = p.require_admin()?;
        let name = text("name", name, 1, 120)?;
        let email = text("email", email, 0, 200)?;
        self.conn.execute("INSERT INTO users(name, email, is_admin, created_at) VALUES (?1, ?2, ?3, ?4)", params![name, email, is_admin, now()])?;
        let user_id = self.conn.last_insert_rowid();
        let (invitation_id, tok, expires) = self.new_invitation(user_id, Some(by))?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    fn user_exists(&self, user_id: i64) -> OpResult<()> {
        let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1)", [user_id], |r| r.get(0))?;
        if !exists {
            return Err(OpError::not_found("user_unknown", format!("There is no person with user_id {user_id}. Call user.list for the valid identifiers.")));
        }
        Ok(())
    }

    pub fn user_reinvite(&self, p: &Principal, user_id: i64) -> OpResult<Value> {
        let by = p.require_admin()?;
        self.user_exists(user_id)?;
        let (invitation_id, tok, expires) = self.new_invitation(user_id, Some(by))?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    pub fn invitation_withdraw(&self, p: &Principal, invitation_id: i64) -> OpResult<Value> {
        p.require_admin()?;
        let n = self.conn.execute("UPDATE invitations SET withdrawn_at = COALESCE(withdrawn_at, ?1) WHERE id = ?2", params![now(), invitation_id])?;
        if n == 0 {
            return Err(OpError::not_found("invitation_unknown", format!("There is no invitation {invitation_id}. Call user.list for the invitations of each person.")));
        }
        Ok(json!({ "invitation_id": invitation_id, "withdrawn": true }))
    }

    fn other_active_admins(&self, user_id: i64) -> OpResult<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND deactivated_at IS NULL AND id <> ?1", [user_id], |r| r.get(0))?)
    }

    pub fn user_set_admin(&self, p: &Principal, user_id: i64, is_admin: bool) -> OpResult<Value> {
        p.require_admin()?;
        self.user_exists(user_id)?;
        if !is_admin && self.other_active_admins(user_id)? == 0 {
            return Err(OpError::conflict(
                "last_admin",
                "This is the last active person with the right to administer people; removing it would lock everybody out. Grant the right to another person first.",
            ));
        }
        self.conn.execute("UPDATE users SET is_admin = ?1 WHERE id = ?2", params![is_admin, user_id])?;
        Ok(json!({ "user_id": user_id, "is_admin": is_admin }))
    }

    pub fn user_set_active(&self, p: &Principal, user_id: i64, active: bool) -> OpResult<Value> {
        p.require_admin()?;
        self.user_exists(user_id)?;
        if !active {
            let is_admin: bool = self.conn.query_row("SELECT is_admin FROM users WHERE id = ?1", [user_id], |r| r.get(0))?;
            if is_admin && self.other_active_admins(user_id)? == 0 {
                return Err(OpError::conflict(
                    "last_admin",
                    "This is the last active person with the right to administer people; deactivating them would lock everybody out. Grant the right to another person first.",
                ));
            }
            self.conn.execute("UPDATE users SET deactivated_at = COALESCE(deactivated_at, ?1) WHERE id = ?2", params![now(), user_id])?;
        } else {
            self.conn.execute("UPDATE users SET deactivated_at = NULL WHERE id = ?1", [user_id])?;
        }
        Ok(json!({ "user_id": user_id, "active": active }))
    }

    /// Deletes a person with their invitations and devices; their closed runs keep the name.
    pub fn user_delete(&self, p: &Principal, user_id: i64) -> OpResult<Value> {
        let me = p.require_admin()?;
        self.user_exists(user_id)?;
        if user_id == me {
            return Err(OpError::conflict("user_self", "You cannot delete yourself. Ask another person with the right to administer people."));
        }
        let (is_admin, active): (bool, bool) = self.conn.query_row("SELECT is_admin, deactivated_at IS NULL FROM users WHERE id = ?1", [user_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if is_admin && active && self.other_active_admins(user_id)? == 0 {
            return Err(OpError::conflict("last_admin", "This is the last active person with the right to administer people; deleting them would lock everybody out. Grant the right to another person first."));
        }
        self.conn.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
        Ok(json!({ "user_id": user_id, "deleted": true }))
    }

    fn devices_of(&self, user_id: i64) -> OpResult<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT id, label, invitation_id, user_agent, last_address, created_at, last_seen_at, revoked_at FROM devices WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = st.query_map([user_id], |r| {
            Ok(json!({
                "device_id": r.get::<_, i64>(0)?, "label": r.get::<_, String>(1)?, "invitation_id": r.get::<_, i64>(2)?,
                "user_agent": r.get::<_, String>(3)?, "last_address": r.get::<_, String>(4)?,
                "created_at": r.get::<_, String>(5)?, "last_seen_at": r.get::<_, String>(6)?, "revoked_at": r.get::<_, Option<String>>(7)?,
            }))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn user_list(&self, p: &Principal) -> OpResult<Value> {
        p.require_admin()?;
        let mut st = self.conn.prepare("SELECT id, name, email, is_admin, deactivated_at, created_at FROM users ORDER BY name COLLATE NOCASE")?;
        let users: Vec<(i64, String, String, bool, Option<String>, String)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
        let ts = now();
        let mut out = Vec::new();
        for (id, name, email, is_admin, deactivated_at, created_at) in users {
            let mut st = self.conn.prepare("SELECT id, created_at, expires_at, withdrawn_at FROM invitations WHERE user_id = ?1 ORDER BY id")?;
            let invitations: Vec<Value> = st
                .query_map([id], |r| {
                    let exp: String = r.get(2)?;
                    let wd: Option<String> = r.get(3)?;
                    let state = if wd.is_some() { "withdrawn" } else if exp < ts { "expired" } else { "open" };
                    Ok(json!({ "invitation_id": r.get::<_, i64>(0)?, "created_at": r.get::<_, String>(1)?, "expires_at": exp, "state": state }))
                })?
                .collect::<Result<_, _>>()?;
            out.push(json!({
                "user_id": id, "name": name, "email": email, "is_admin": is_admin,
                "active": deactivated_at.is_none(), "created_at": created_at,
                "devices": self.devices_of(id)?, "invitations": invitations,
            }));
        }
        Ok(json!({ "users": out }))
    }

    pub fn device_list_own(&self, p: &Principal) -> OpResult<Value> {
        let (device_id, user_id, _) = p.require_person()?;
        Ok(json!({ "current_device_id": device_id, "devices": self.devices_of(user_id)? }))
    }

    /// Anyone may revoke their own device; revoking another person's device needs the admin right.
    pub fn device_revoke(&self, p: &Principal, device_id: i64) -> OpResult<Value> {
        let (_, me, is_admin) = p.require_person()?;
        // Without the right to administer people, another person's device is answered like a missing one.
        let found: bool = self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE id = ?1 AND (user_id = ?2 OR ?3))", params![device_id, me, is_admin], |r| r.get(0))?;
        if !found {
            return Err(OpError::not_found(
                "device_unknown",
                format!("There is no device {device_id} you may revoke. Call device.list for your own devices; another person's device can be revoked by a person with the right to administer people."),
            ));
        }
        self.conn.execute("UPDATE devices SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), device_id])?;
        Ok(json!({ "device_id": device_id, "revoked": true }))
    }

    // ---------------------------------------------------------------- agents

    /// Open self-registration: a token without any ability, its display string, and the
    /// self-introduction kept as the basis for the grant. The clear text is returned once.
    pub fn agent_register(&self, introduction: &str) -> OpResult<Value> {
        let introduction = text("introduction", introduction, 10, 500)?;
        let cutoff = ago(Duration::hours(AGENT_PENDING_HOURS));
        // Registrations nobody granted in time are of no use to anyone.
        self.conn.execute("DELETE FROM agents WHERE ability IS NULL AND created_at < ?1", [&cutoff])?;
        let waiting: i64 = self.conn.query_row("SELECT COUNT(*) FROM agents WHERE ability IS NULL AND revoked_at IS NULL", [], |r| r.get(0))?;
        if waiting >= AGENT_PENDING_MAX {
            return Err(OpError::new(
                429,
                "registrations_full",
                format!("{AGENT_PENDING_MAX} registrations are already waiting for a grant. Ask the user to have the waiting ones granted or rejected, or try again in {AGENT_PENDING_HOURS} hours."),
            ));
        }
        let tok = token::generate("pda_");
        let display = token::display_string(&tok);
        let ts = now();
        self.conn.execute(
            "INSERT INTO agents(token_hash, display, introduction, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![token::hash(&tok), display, introduction, ts],
        )?;
        Ok(json!({
            "agent_id": self.conn.last_insert_rowid(), "display": display, "token": tok,
            "state": "pending", "ability": null, "expires_at": plus(&ts, Duration::hours(AGENT_PENDING_HOURS)),
        }))
    }

    fn check_ability(ability: &str) -> OpResult<()> {
        if !ABILITIES.iter().any(|(n, _)| *n == ability) {
            let names: Vec<_> = ABILITIES.iter().map(|(n, _)| *n).collect();
            return Err(OpError::bad("ability_unknown", format!("`ability` = \"{ability}\" is not an ability. The possible abilities are: {}.", names.join(", "))));
        }
        Ok(())
    }

    /// A person with the right to administer people creates a token with its ability at once,
    /// for a script that cannot register itself. The clear text is returned once.
    pub fn agent_create(&self, p: &Principal, introduction: &str, ability: &str) -> OpResult<Value> {
        let by = p.require_admin()?;
        Self::check_ability(ability)?;
        let introduction = text("introduction", introduction, 3, 500)?;
        let tok = token::generate("pda_");
        let ts = now();
        self.conn.execute(
            "INSERT INTO agents(token_hash, display, introduction, ability, granted_by, granted_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![token::hash(&tok), token::display_string(&tok), introduction, ability, by, ts],
        )?;
        let mut v = self.agent_get(self.conn.last_insert_rowid())?.to_json();
        v["token"] = json!(tok);
        Ok(v)
    }

    pub fn agent_list(&self, p: &Principal) -> OpResult<Value> {
        p.require_admin()?;
        let mut st = self.conn.prepare(&format!("SELECT {} FROM agents a LEFT JOIN users u ON u.id = a.granted_by ORDER BY a.id DESC", AGENT_COLS))?;
        let agents: Vec<AgentRow> = st.query_map([], agent_row)?.collect::<Result<_, _>>()?;
        Ok(json!({
            "abilities": ABILITIES.iter().map(|(n, d)| json!({ "ability": n, "permits": d })).collect::<Vec<_>>(),
            "agents": agents.iter().map(AgentRow::to_json).collect::<Vec<_>>(),
        }))
    }

    fn agent_get(&self, agent_id: i64) -> OpResult<AgentRow> {
        self.conn
            .query_row(&format!("SELECT {} FROM agents a LEFT JOIN users u ON u.id = a.granted_by WHERE a.id = ?1", AGENT_COLS), [agent_id], agent_row)
            .optional()?
            .ok_or_else(|| OpError::not_found("agent_unknown", format!("There is no agent token {agent_id}. Call agent.list for the valid identifiers.")))
    }

    /// Grants exactly one ability to a waiting token; a token's ability never changes afterwards.
    pub fn agent_grant(&self, p: &Principal, agent_id: i64, ability: &str) -> OpResult<Value> {
        let by = p.require_admin()?;
        Self::check_ability(ability)?;
        let a = self.agent_get(agent_id)?;
        match a.state() {
            "pending" => {}
            "granted" => {
                return Err(OpError::conflict(
                    "agent_granted",
                    format!("Agent token {} already carries {}. For another ability the agent registers a second token.", a.display, a.ability.unwrap_or_default()),
                ))
            }
            state => return Err(OpError::gone("agent_not_waiting", format!("Agent token {} is {state} and cannot be granted. The agent registers a new one.", a.display))),
        }
        self.conn.execute("UPDATE agents SET ability = ?1, granted_by = ?2, granted_at = ?3 WHERE id = ?4", params![ability, by, now(), agent_id])?;
        Ok(self.agent_get(agent_id)?.to_json())
    }

    /// Revoking needs no window and hits exactly one token; it also rejects a waiting one.
    pub fn agent_revoke(&self, p: &Principal, agent_id: i64) -> OpResult<Value> {
        p.require_admin()?;
        self.agent_get(agent_id)?;
        self.conn.execute("UPDATE agents SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), agent_id])?;
        Ok(json!({ "agent_id": agent_id, "revoked": true }))
    }

    // ---------------------------------------------------------------- vault

    pub fn vault_get(&self, p: &Principal) -> OpResult<Value> {
        p.require_person()?;
        let row = self
            .conn
            .query_row("SELECT salt, iterations, verifier, created_at FROM vault WHERE id = 1", [], |r| {
                Ok(json!({ "exists": true, "salt": r.get::<_, String>(0)?, "iterations": r.get::<_, i64>(1)?, "verifier": r.get::<_, String>(2)?, "created_at": r.get::<_, String>(3)? }))
            })
            .optional()?;
        Ok(row.unwrap_or_else(|| json!({ "exists": false })))
    }

    pub fn vault_create(&self, p: &Principal, salt: &str, iterations: i64, verifier: &str) -> OpResult<Value> {
        let (_, user_id, _) = p.require_person()?;
        if salt.len() < 16 || salt.len() > 200 {
            return Err(OpError::bad("salt_invalid", "`salt` must be the Base64 text of at least 16 random bytes, as the application creates it. Create the vault from the application."));
        }
        if !(100_000..=10_000_000).contains(&iterations) {
            return Err(OpError::bad("iterations_invalid", "`iterations` must lie between 100000 and 10000000. The application uses 600000."));
        }
        check_sealed("verifier", verifier)?;
        if verifier.is_empty() {
            return Err(OpError::bad("verifier_missing", "`verifier` must be the sealed check value the application creates with the new key."));
        }
        let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM vault)", [], |r| r.get(0))?;
        if exists {
            return Err(OpError::conflict("vault_exists", "A vault already exists. Unlock it with the shared passphrase instead; call vault.get for its salt and check value."));
        }
        self.conn.execute(
            "INSERT INTO vault(id, salt, iterations, verifier, created_by, created_at) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![salt, iterations, verifier, user_id, now()],
        )?;
        self.vault_get(p)
    }

    // ---------------------------------------------------------------- jobs

    fn row_to_job(&self, r: &rusqlite::Row, today: NaiveDate, with_sealed: bool) -> rusqlite::Result<Job> {
        let next_due: String = r.get(6)?;
        let due = NaiveDate::parse_from_str(&next_due, "%Y-%m-%d").unwrap_or(today);
        let md: String = r.get(9)?;
        Ok(Job {
            job_id: r.get(0)?,
            title: r.get(1)?,
            entry_url: r.get(2)?,
            embeddable: r.get(3)?,
            interval_weeks: r.get(4)?,
            retry_days: r.get(5)?,
            urgency_days: schedule::urgency_days(today, due),
            run_count: r.get(10)?,
            next_due,
            username_sealed: with_sealed.then(|| r.get(7)).transpose()?,
            password_sealed: with_sealed.then(|| r.get(8)).transpose()?,
            instructions_html: Some(render_markdown(&md)),
            instructions_md: Some(md),
            last_run: None,
        })
    }

    const JOB_COLS: &'static str = "id, title, entry_url, embeddable, interval_weeks, retry_days, next_due, username_sealed, password_sealed, instructions_md, (SELECT COUNT(*) FROM runs WHERE runs.job_id = jobs.id)";

    fn last_run(&self, job_id: i64) -> OpResult<Option<Value>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT r.outcome, r.finished_at, r.note, {RUN_BY} FROM runs r WHERE r.job_id = ?1 ORDER BY r.id DESC LIMIT 1"),
                [job_id],
                |r| Ok(json!({ "outcome": r.get::<_, String>(0)?, "finished_at": r.get::<_, String>(1)?, "note": r.get::<_, String>(2)?, "by": r.get::<_, String>(3)? })),
            )
            .optional()?)
    }

    /// All jobs, most urgent first (smallest number of days to the next due date).
    pub fn job_list(&self, p: &Principal, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.read")?;
        let mut st = self.conn.prepare(&format!("SELECT {} FROM jobs ORDER BY next_due, title COLLATE NOCASE", Self::JOB_COLS))?;
        let mut jobs: Vec<Job> = st.query_map([], |r| self.row_to_job(r, today, p.sees_sealed()))?.collect::<Result<_, _>>()?;
        for j in &mut jobs {
            j.last_run = self.last_run(j.job_id)?;
        }
        Ok(json!({ "today": today.format("%Y-%m-%d").to_string(), "jobs": jobs }))
    }

    fn job_get_inner(&self, job_id: i64, today: NaiveDate) -> OpResult<Job> {
        let job = self
            .conn
            .query_row(&format!("SELECT {} FROM jobs WHERE id = ?1", Self::JOB_COLS), [job_id], |r| self.row_to_job(r, today, true))
            .optional()?;
        let mut job = job.ok_or_else(|| OpError::not_found("job_unknown", format!("There is no job {job_id}. Call job.list for the valid identifiers.")))?;
        job.last_run = self.last_run(job_id)?;
        Ok(job)
    }

    pub fn job_get(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.read")?;
        self.job_for(p, job_id, today)
    }

    /// A job as the caller may see it: agents never receive the sealed logins.
    fn job_for(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        let mut job = self.job_get_inner(job_id, today)?;
        if !p.sees_sealed() {
            job.username_sealed = None;
            job.password_sealed = None;
        }
        Ok(serde_json::to_value(job).expect("job serializes"))
    }

    /// Logins are sealed with the vault key in a person's browser, so only a person sets them.
    fn refuse_logins_from_agents(p: &Principal, input: &JobInput) -> OpResult<()> {
        if !p.sees_sealed() && (input.username_sealed.is_some() || input.password_sealed.is_some()) {
            return Err(OpError::forbidden(
                "An agent cannot set a portal login; logins are sealed in a person's browser. Leave username_sealed and password_sealed out, and ask the user to enter the login in the application.",
            ));
        }
        Ok(())
    }

    fn validate_job(input: &JobInput, creating: bool) -> OpResult<()> {
        let need = |o: bool, f: &str| -> OpResult<()> {
            if creating && !o {
                return Err(OpError::bad("field_missing", format!("`{f}` is required to create a job. Add it and send again.")));
            }
            Ok(())
        };
        need(input.title.is_some(), "title")?;
        need(input.entry_url.is_some(), "entry_url")?;
        need(input.interval_weeks.is_some(), "interval_weeks")?;
        if let Some(t) = &input.title {
            text("title", t, 1, 200)?;
        }
        if let Some(u) = &input.entry_url {
            let u = u.trim();
            if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 2000 || u.contains(char::is_whitespace) {
                return Err(OpError::bad("entry_url_invalid", "`entry_url` must be a web address starting with https:// (or http://), without spaces. Copy it from the browser's address bar."));
            }
        }
        if let Some(w) = input.interval_weeks {
            if !(1..=520).contains(&w) {
                return Err(OpError::bad("interval_invalid", "`interval_weeks` must lie between 1 and 520."));
            }
        }
        if let Some(d) = input.retry_days {
            if !(1..=365).contains(&d) {
                return Err(OpError::bad("retry_invalid", "`retry_days` must lie between 1 and 365."));
            }
        }
        if let Some(d) = &input.next_due {
            parse_date("next_due", d)?;
        }
        if let Some(s) = &input.username_sealed {
            check_sealed("username_sealed", s)?;
        }
        if let Some(s) = &input.password_sealed {
            check_sealed("password_sealed", s)?;
        }
        if let Some(m) = &input.instructions_md {
            if m.len() > 65536 {
                return Err(OpError::bad("instructions_too_long", "`instructions_md` may hold at most 64 KiB of text. Shorten it."));
            }
        }
        Ok(())
    }

    pub fn job_create(&self, p: &Principal, input: JobInput, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.write")?;
        Self::refuse_logins_from_agents(p, &input)?;
        Self::validate_job(&input, true)?;
        let ts = now();
        self.conn.execute(
            "INSERT INTO jobs(title, entry_url, embeddable, interval_weeks, retry_days, next_due, username_sealed, password_sealed, instructions_md, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
            params![
                input.title.unwrap().trim(),
                input.entry_url.unwrap().trim(),
                input.embeddable.unwrap_or(false),
                input.interval_weeks.unwrap(),
                input.retry_days.unwrap_or(2),
                input.next_due.unwrap_or_else(|| today.format("%Y-%m-%d").to_string()),
                input.username_sealed.unwrap_or_default(),
                input.password_sealed.unwrap_or_default(),
                input.instructions_md.unwrap_or_default(),
                ts
            ],
        )?;
        self.job_for(p, self.conn.last_insert_rowid(), today)
    }

    pub fn job_update(&self, p: &Principal, job_id: i64, input: JobInput, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.write")?;
        Self::refuse_logins_from_agents(p, &input)?;
        Self::validate_job(&input, false)?;
        let cur = self.job_get_inner(job_id, today)?;
        self.conn.execute(
            "UPDATE jobs SET title=?1, entry_url=?2, embeddable=?3, interval_weeks=?4, retry_days=?5, next_due=?6,
             username_sealed=?7, password_sealed=?8, instructions_md=?9, updated_at=?10 WHERE id=?11",
            params![
                input.title.map(|s| s.trim().to_string()).unwrap_or(cur.title),
                input.entry_url.map(|s| s.trim().to_string()).unwrap_or(cur.entry_url),
                input.embeddable.unwrap_or(cur.embeddable),
                input.interval_weeks.unwrap_or(cur.interval_weeks),
                input.retry_days.unwrap_or(cur.retry_days),
                input.next_due.unwrap_or(cur.next_due),
                input.username_sealed.or(cur.username_sealed).unwrap_or_default(),
                input.password_sealed.or(cur.password_sealed).unwrap_or_default(),
                input.instructions_md.or(cur.instructions_md).unwrap_or_default(),
                now(),
                job_id
            ],
        )?;
        self.job_for(p, job_id, today)
    }

    pub fn job_delete(&self, p: &Principal, job_id: i64) -> OpResult<Value> {
        p.require_ability("jobs.write")?;
        let n = self.conn.execute("DELETE FROM jobs WHERE id = ?1", [job_id])?;
        if n == 0 {
            return Err(OpError::not_found("job_unknown", format!("There is no job {job_id}. Call job.list for the valid identifiers.")));
        }
        Ok(json!({ "job_id": job_id, "deleted": true }))
    }

    /// Closes a run: records it and moves the next due date according to the outcome.
    pub fn job_complete(&self, p: &Principal, job_id: i64, outcome: &str, note: &str, expected_run_count: Option<i64>, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.complete")?;
        let (user_id, device_id, agent_id, by_name) = match p {
            Principal::Device { device_id, user_id, name, .. } => (Some(*user_id), Some(*device_id), None, name.clone()),
            Principal::Agent { agent_id, display, .. } => (None, None, Some(*agent_id), format!("Agent {display}")),
        };
        let outcome = Outcome::parse(outcome)
            .ok_or_else(|| OpError::bad("outcome_invalid", format!("`outcome` = \"{outcome}\" is not possible. Send \"succeeded\" when everything was filed, \"failed\" otherwise.")))?;
        let note = text("note", note, 0, 2000)?;
        let job = self.job_get_inner(job_id, today)?;
        if let Some(exp) = expected_run_count {
            if exp != job.run_count {
                return Err(OpError::conflict(
                    "job_changed",
                    format!("Somebody else has closed a run of this job meanwhile; it is now due on {}. Reload the list and check whether the run is still needed.", job.next_due),
                ));
            }
        }
        let due_after = schedule::next_due(today, outcome, job.interval_weeks, job.retry_days).format("%Y-%m-%d").to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs(job_id, user_id, device_id, agent_id, by_name, outcome, note, finished_at, due_before, due_after) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![job_id, user_id, device_id, agent_id, by_name, outcome.as_str(), note, now(), job.next_due, due_after],
        )?;
        tx.execute("UPDATE jobs SET next_due = ?1, updated_at = ?2 WHERE id = ?3", params![due_after, now(), job_id])?;
        tx.commit()?;
        Ok(json!({ "job_id": job_id, "outcome": outcome.as_str(), "due_before": job.next_due, "next_due": due_after }))
    }

    pub fn job_history(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        p.require_ability("jobs.read")?;
        self.job_get_inner(job_id, today)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT r.id, r.outcome, r.note, r.finished_at, r.due_before, r.due_after, {RUN_BY} FROM runs r WHERE r.job_id = ?1 ORDER BY r.id DESC LIMIT 200"
        ))?;
        let runs: Vec<Value> = st
            .query_map([job_id], |r| {
                Ok(json!({
                    "run_id": r.get::<_, i64>(0)?, "outcome": r.get::<_, String>(1)?, "note": r.get::<_, String>(2)?,
                    "finished_at": r.get::<_, String>(3)?, "due_before": r.get::<_, String>(4)?, "due_after": r.get::<_, String>(5)?, "by": r.get::<_, String>(6)?,
                }))
            })?
            .collect::<Result<_, _>>()?;
        Ok(json!({ "job_id": job_id, "runs": runs }))
    }
}

// -------------------------------------------------------------------- backup

/// The first line of every backup.
pub const BACKUP_FORMAT: &str = "portal-download-backup";
pub const BACKUP_VERSION: i64 = 1;

/// What one import did; the renamed entries are listed so the user can delete them.
#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub vault: &'static str,
    pub people_added: usize,
    pub invitations_added: usize,
    pub devices_added: usize,
    pub agents_added: usize,
    pub jobs_added: usize,
    pub runs_added: usize,
    pub logins_dropped: usize,
    pub skipped: usize,
    pub renamed: Vec<Value>,
}

fn field<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}
fn s_of(v: &Value, k: &str) -> String {
    field(v, k).as_str().unwrap_or("").to_string()
}
fn opt_s(v: &Value, k: &str) -> Option<String> {
    field(v, k).as_str().map(str::to_string)
}
fn i_of(v: &Value, k: &str) -> Option<i64> {
    field(v, k).as_i64()
}
/// "Title (Import 2026-11-06)", shortened so it still fits the field.
fn import_name(base: &str, max: usize, today: NaiveDate) -> String {
    let suffix = format!(" (Import {})", today.format("%Y-%m-%d"));
    let keep = max.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", base.chars().take(keep).collect::<String>().trim_end())
}

impl Store {
    /// Who may take the backup: a person with the right to administer people, or a token
    /// carrying backup.read.
    pub fn require_backup_reader(p: &Principal) -> OpResult<()> {
        match p {
            Principal::Device { .. } => p.require_admin().map(|_| ()),
            Principal::Agent { .. } => p.require_ability("backup.read"),
        }
    }

    pub fn backup_export(&self, p: &Principal) -> OpResult<String> {
        Self::require_backup_reader(p)?;
        self.backup_text()
    }

    /// Everything the service keeps, as JSONL: a head line, then one typed record per line.
    pub fn backup_text(&self) -> OpResult<String> {
        let mut out = Vec::new();
        out.push(json!({ "type": "backup", "format": BACKUP_FORMAT, "version": BACKUP_VERSION, "created_at": now(), "app_version": env!("CARGO_PKG_VERSION") }));
        let mut each = |sql: &str, f: &dyn Fn(&rusqlite::Row) -> rusqlite::Result<Value>| -> OpResult<()> {
            let mut st = self.conn.prepare(sql)?;
            for v in st.query_map([], |r| f(r))? {
                out.push(v?);
            }
            Ok(())
        };
        each("SELECT salt, iterations, verifier, created_at FROM vault", &|r| {
            Ok(json!({ "type": "vault", "salt": r.get::<_, String>(0)?, "iterations": r.get::<_, i64>(1)?, "verifier": r.get::<_, String>(2)?, "created_at": r.get::<_, String>(3)? }))
        })?;
        each("SELECT id, name, email, is_admin, deactivated_at, created_at FROM users ORDER BY id", &|r| {
            Ok(json!({ "type": "person", "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "email": r.get::<_, String>(2)?, "is_admin": r.get::<_, bool>(3)?, "deactivated_at": r.get::<_, Option<String>>(4)?, "created_at": r.get::<_, String>(5)? }))
        })?;
        each("SELECT id, user_id, token_hash, created_at, expires_at, withdrawn_at FROM invitations ORDER BY id", &|r| {
            Ok(json!({ "type": "invitation", "id": r.get::<_, i64>(0)?, "person_id": r.get::<_, i64>(1)?, "token_hash": r.get::<_, String>(2)?, "created_at": r.get::<_, String>(3)?, "expires_at": r.get::<_, String>(4)?, "withdrawn_at": r.get::<_, Option<String>>(5)? }))
        })?;
        each("SELECT id, user_id, invitation_id, credential_hash, label, user_agent, last_address, created_at, last_seen_at, revoked_at FROM devices ORDER BY id", &|r| {
            Ok(json!({ "type": "device", "id": r.get::<_, i64>(0)?, "person_id": r.get::<_, i64>(1)?, "invitation_id": r.get::<_, i64>(2)?, "credential_hash": r.get::<_, String>(3)?, "label": r.get::<_, String>(4)?, "user_agent": r.get::<_, String>(5)?, "last_address": r.get::<_, String>(6)?, "created_at": r.get::<_, String>(7)?, "last_seen_at": r.get::<_, String>(8)?, "revoked_at": r.get::<_, Option<String>>(9)? }))
        })?;
        each("SELECT id, token_hash, display, introduction, ability, granted_at, created_at, last_used_at, revoked_at FROM agents ORDER BY id", &|r| {
            Ok(json!({ "type": "agent", "id": r.get::<_, i64>(0)?, "token_hash": r.get::<_, String>(1)?, "display": r.get::<_, String>(2)?, "introduction": r.get::<_, String>(3)?, "ability": r.get::<_, Option<String>>(4)?, "granted_at": r.get::<_, Option<String>>(5)?, "created_at": r.get::<_, String>(6)?, "last_used_at": r.get::<_, Option<String>>(7)?, "revoked_at": r.get::<_, Option<String>>(8)? }))
        })?;
        each("SELECT id, title, entry_url, embeddable, interval_weeks, retry_days, next_due, username_sealed, password_sealed, instructions_md, created_at, updated_at FROM jobs ORDER BY id", &|r| {
            Ok(json!({ "type": "job", "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?, "entry_url": r.get::<_, String>(2)?, "embeddable": r.get::<_, bool>(3)?, "interval_weeks": r.get::<_, i64>(4)?, "retry_days": r.get::<_, i64>(5)?, "next_due": r.get::<_, String>(6)?, "username_sealed": r.get::<_, String>(7)?, "password_sealed": r.get::<_, String>(8)?, "instructions_md": r.get::<_, String>(9)?, "created_at": r.get::<_, String>(10)?, "updated_at": r.get::<_, String>(11)? }))
        })?;
        each("SELECT id, job_id, user_id, agent_id, by_name, outcome, note, finished_at, due_before, due_after FROM runs ORDER BY id", &|r| {
            Ok(json!({ "type": "run", "id": r.get::<_, i64>(0)?, "job_id": r.get::<_, i64>(1)?, "person_id": r.get::<_, Option<i64>>(2)?, "agent_id": r.get::<_, Option<i64>>(3)?, "by": r.get::<_, String>(4)?, "outcome": r.get::<_, String>(5)?, "note": r.get::<_, String>(6)?, "finished_at": r.get::<_, String>(7)?, "due_before": r.get::<_, String>(8)?, "due_after": r.get::<_, String>(9)? }))
        })?;
        let mut text = String::new();
        for v in out {
            text.push_str(&v.to_string());
            text.push('\n');
        }
        Ok(text)
    }

    /// Adds a backup to what is there. Nothing is overwritten: a job or person that exists
    /// already comes in a second time with "(Import <date>)" after its name.
    pub fn backup_import(&self, p: &Principal, jsonl: &str, include_access: bool, today: NaiveDate) -> OpResult<Value> {
        p.require_admin()?;
        let mut records = Vec::new();
        for (n, line) in jsonl.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line).map_err(|e| OpError::bad("backup_invalid", format!("Line {} of the backup is not JSON: {e}. Upload the file exactly as it was downloaded.", n + 1)))?;
            records.push((n + 1, v));
        }
        let head_ok = records.first().map(|(_, h)| s_of(h, "type") == "backup" && s_of(h, "format") == BACKUP_FORMAT && i_of(h, "version") == Some(BACKUP_VERSION)).unwrap_or(false);
        if !head_ok {
            return Err(OpError::bad("backup_invalid", format!("The first line must be the head of a {BACKUP_FORMAT} file of version {BACKUP_VERSION}. Upload a file this service wrote.")));
        }
        let bad = |n: usize, what: &str| OpError::bad("backup_invalid", format!("Line {n} of the backup: {what}. Upload the file exactly as it was downloaded."));
        let mut rep = ImportReport { vault: "none", ..Default::default() };
        let tx = self.conn.unchecked_transaction()?;
        let mut people = std::collections::HashMap::new();
        let mut invitations = std::collections::HashMap::new();
        let mut agents = std::collections::HashMap::new();
        let mut jobs = std::collections::HashMap::new();
        let mut drop_logins = false;
        let mut backup_has_vault = false;
        let target_vault: Option<(String, String)> = tx.query_row("SELECT salt, verifier FROM vault WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        for (n, v) in records.iter().skip(1) {
            let n = *n;
            match s_of(v, "type").as_str() {
                "vault" => {
                    backup_has_vault = true;
                    let (salt, verifier) = (s_of(v, "salt"), s_of(v, "verifier"));
                    match &target_vault {
                        None => {
                            check_sealed("verifier", &verifier).map_err(|_| bad(n, "the vault check value is not a sealed value"))?;
                            let iterations = i_of(v, "iterations").filter(|i| (100_000..=10_000_000).contains(i)).ok_or_else(|| bad(n, "iterations out of range"))?;
                            tx.execute("INSERT INTO vault(id, salt, iterations, verifier, created_at) VALUES (1, ?1, ?2, ?3, ?4)", params![salt, iterations, verifier, s_of(v, "created_at")])?;
                            rep.vault = "adopted";
                        }
                        Some((ts, tv)) if *ts == salt && *tv == verifier => rep.vault = "same",
                        Some(_) => {
                            rep.vault = "different";
                            drop_logins = true;
                        }
                    }
                }
                "person" | "invitation" | "device" | "agent" if !include_access => rep.skipped += 1,
                "person" => {
                    let name = text("name", &s_of(v, "name"), 1, 120).map_err(|_| bad(n, "a person without a usable name"))?;
                    let email = s_of(v, "email");
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM users WHERE lower(name) = lower(?1) OR (?2 <> '' AND lower(email) = lower(?2)))",
                        params![name, email],
                        |r| r.get(0),
                    )?;
                    let final_name = if exists { import_name(&name, 120, today) } else { name };
                    tx.execute(
                        "INSERT INTO users(name, email, is_admin, deactivated_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![final_name, email, field(v, "is_admin").as_bool().unwrap_or(false), opt_s(v, "deactivated_at"), s_of(v, "created_at")],
                    )?;
                    let id = tx.last_insert_rowid();
                    if exists {
                        rep.renamed.push(json!({ "kind": "person", "user_id": id, "name": final_name }));
                    }
                    people.insert(i_of(v, "id").ok_or_else(|| bad(n, "a person without id"))?, id);
                    rep.people_added += 1;
                }
                "invitation" => {
                    let Some(&user) = i_of(v, "person_id").and_then(|i| people.get(&i)) else { rep.skipped += 1; continue };
                    let n_ins = tx.execute(
                        "INSERT OR IGNORE INTO invitations(user_id, token_hash, created_at, expires_at, withdrawn_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![user, s_of(v, "token_hash"), s_of(v, "created_at"), s_of(v, "expires_at"), opt_s(v, "withdrawn_at")],
                    )?;
                    if n_ins == 0 { rep.skipped += 1; continue; }
                    invitations.insert(i_of(v, "id").ok_or_else(|| bad(n, "an invitation without id"))?, tx.last_insert_rowid());
                    rep.invitations_added += 1;
                }
                "device" => {
                    let (Some(&user), Some(&inv)) = (i_of(v, "person_id").and_then(|i| people.get(&i)), i_of(v, "invitation_id").and_then(|i| invitations.get(&i))) else { rep.skipped += 1; continue };
                    let n_ins = tx.execute(
                        "INSERT OR IGNORE INTO devices(user_id, invitation_id, credential_hash, label, user_agent, last_address, created_at, last_seen_at, revoked_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                        params![user, inv, s_of(v, "credential_hash"), s_of(v, "label"), s_of(v, "user_agent"), s_of(v, "last_address"), s_of(v, "created_at"), s_of(v, "last_seen_at"), opt_s(v, "revoked_at")],
                    )?;
                    if n_ins == 0 { rep.skipped += 1; } else { rep.devices_added += 1; }
                }
                "agent" => {
                    let ability = opt_s(v, "ability").filter(|a| ABILITIES.iter().any(|(n, _)| n == a));
                    let n_ins = tx.execute(
                        "INSERT OR IGNORE INTO agents(token_hash, display, introduction, ability, granted_at, created_at, last_used_at, revoked_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![s_of(v, "token_hash"), s_of(v, "display"), s_of(v, "introduction"), ability, opt_s(v, "granted_at"), s_of(v, "created_at"), opt_s(v, "last_used_at"), opt_s(v, "revoked_at")],
                    )?;
                    if n_ins == 0 { rep.skipped += 1; continue; }
                    agents.insert(i_of(v, "id").ok_or_else(|| bad(n, "an agent without id"))?, tx.last_insert_rowid());
                    rep.agents_added += 1;
                }
                "job" => {
                    let mut input: JobInput = serde_json::from_value(json!({
                        "title": field(v, "title"), "entry_url": field(v, "entry_url"), "embeddable": field(v, "embeddable"),
                        "interval_weeks": field(v, "interval_weeks"), "retry_days": field(v, "retry_days"), "next_due": field(v, "next_due"),
                        "username_sealed": field(v, "username_sealed"), "password_sealed": field(v, "password_sealed"), "instructions_md": field(v, "instructions_md"),
                    }))
                    .map_err(|e| bad(n, &format!("a job does not fit ({e})")))?;
                    Self::validate_job(&input, true).map_err(|e| bad(n, &e.message))?;
                    // Without the vault the logins were sealed with, they cannot be read here.
                    let has_login = |s: &Option<String>| s.as_deref().is_some_and(|x| !x.is_empty());
                    if (drop_logins || (!backup_has_vault && target_vault.is_some())) && (has_login(&input.username_sealed) || has_login(&input.password_sealed)) {
                        input.username_sealed = None;
                        input.password_sealed = None;
                        rep.logins_dropped += 1;
                    }
                    let title = input.title.clone().unwrap_or_default().trim().to_string();
                    let url = input.entry_url.clone().unwrap_or_default().trim().to_string();
                    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE lower(title) = lower(?1) AND entry_url = ?2)", params![title, url], |r| r.get(0))?;
                    let final_title = if exists { import_name(&title, 200, today) } else { title };
                    tx.execute(
                        "INSERT INTO jobs(title, entry_url, embeddable, interval_weeks, retry_days, next_due, username_sealed, password_sealed, instructions_md, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                        params![
                            final_title, url, input.embeddable.unwrap_or(false), input.interval_weeks, input.retry_days.unwrap_or(2),
                            input.next_due.unwrap_or_else(|| today.format("%Y-%m-%d").to_string()),
                            input.username_sealed.unwrap_or_default(), input.password_sealed.unwrap_or_default(), input.instructions_md.unwrap_or_default(),
                            s_of(v, "created_at"), now()
                        ],
                    )?;
                    let id = tx.last_insert_rowid();
                    if exists {
                        rep.renamed.push(json!({ "kind": "job", "job_id": id, "name": final_title }));
                    }
                    jobs.insert(i_of(v, "id").ok_or_else(|| bad(n, "a job without id"))?, id);
                    rep.jobs_added += 1;
                }
                "run" => {
                    let Some(&job) = i_of(v, "job_id").and_then(|i| jobs.get(&i)) else { rep.skipped += 1; continue };
                    if Outcome::parse(&s_of(v, "outcome")).is_none() {
                        return Err(bad(n, "a run with an unknown outcome"));
                    }
                    let user = i_of(v, "person_id").and_then(|i| people.get(&i)).copied();
                    let agent = i_of(v, "agent_id").and_then(|i| agents.get(&i)).copied();
                    tx.execute(
                        "INSERT INTO runs(job_id, user_id, agent_id, by_name, outcome, note, finished_at, due_before, due_after) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                        params![job, user, agent, s_of(v, "by"), s_of(v, "outcome"), s_of(v, "note"), s_of(v, "finished_at"), s_of(v, "due_before"), s_of(v, "due_after")],
                    )?;
                    rep.runs_added += 1;
                }
                other => return Err(bad(n, &format!("the record type \"{other}\" is not known"))),
            }
        }
        tx.commit()?;
        Ok(serde_json::to_value(rep).expect("report serializes"))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    const SEALED: &str = r#"{"v":1,"iv":"AAAAAAAAAAAAAAAA","ct":"AAAAAAAAAAAAAAAAAAAAAA=="}"#;

    #[test]
    fn dates_are_stored_only_in_canonical_form() {
        assert!(parse_date("d", "2026-11-01").is_ok());
        for bad in ["2026-1-5", "+12026-01-05", "1999-12-31", "2026-02-30"] {
            assert!(parse_date("d", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn another_persons_device_looks_like_a_missing_one() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let Principal::Device { device_id: admin_device, .. } = a.clone() else { panic!() };
        let r = s.user_create(&a, "Bob", "", false).unwrap();
        let (c, _) = s.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
        let b = s.authenticate_device(&c, "", "").unwrap();
        let other = s.device_revoke(&b, admin_device).unwrap_err();
        let missing = s.device_revoke(&b, 999).unwrap_err();
        assert_eq!(other.kind, missing.kind);
        assert_eq!(other.code, missing.code);
    }

    fn admin(store: &Store) -> Principal {
        let r = store.setup_create_admin("Ada", "").unwrap();
        let (cred, _) = store.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "ua", "127.0.0.1").unwrap();
        store.authenticate_device(&cred, "ua", "127.0.0.1").unwrap()
    }

    #[test]
    fn one_invitation_serves_several_devices_each_with_its_own_credential() {
        let s = Store::open_in_memory().unwrap();
        let r = s.setup_create_admin("Ada", "").unwrap();
        let tok = r["invitation_token"].as_str().unwrap();
        let (c1, _) = s.redeem_invitation(tok, "Laptop", "", "").unwrap();
        let (c2, _) = s.redeem_invitation(tok, "Phone", "", "").unwrap();
        assert_ne!(c1, c2);
        let p1 = s.authenticate_device(&c1, "", "").unwrap();
        s.authenticate_device(&c2, "", "").unwrap();
        // Revoking one device leaves the other untouched.
        let Principal::Device { device_id, .. } = p1 else { panic!() };
        s.device_revoke(&p1, device_id).unwrap();
        assert_eq!(s.authenticate_device(&c1, "", "").unwrap_err().kind, "device_revoked");
        s.authenticate_device(&c2, "", "").unwrap();
    }

    #[test]
    fn withdrawn_invitation_cannot_be_redeemed_but_devices_stay() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let r = s.user_create(&a, "Bob", "", false).unwrap();
        let tok = r["invitation_token"].as_str().unwrap();
        let (c, _) = s.redeem_invitation(tok, "Laptop", "", "").unwrap();
        s.invitation_withdraw(&a, r["invitation_id"].as_i64().unwrap()).unwrap();
        assert_eq!(s.redeem_invitation(tok, "Phone", "", "").unwrap_err().kind, "invitation_withdrawn");
        s.authenticate_device(&c, "", "").unwrap();
    }

    #[test]
    fn last_admin_cannot_remove_their_right() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let Principal::Device { user_id, .. } = a.clone() else { panic!() };
        assert_eq!(s.user_set_admin(&a, user_id, false).unwrap_err().kind, "last_admin");
        assert_eq!(s.user_set_active(&a, user_id, false).unwrap_err().kind, "last_admin");
    }

    #[test]
    fn non_admin_cannot_invite_but_can_work_on_jobs() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let r = s.user_create(&a, "Bob", "", false).unwrap();
        let (c, _) = s.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
        let b = s.authenticate_device(&c, "", "").unwrap();
        assert_eq!(s.user_create(&b, "Eve", "", false).unwrap_err().code, 403);
        let input = JobInput { title: Some("Energy".into()), entry_url: Some("https://portal.example.org".into()), interval_weeks: Some(4), ..Default::default() };
        s.job_create(&b, input, d("2026-11-06")).unwrap();
    }

    #[test]
    fn deactivated_user_is_locked_out_on_every_device() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let r = s.user_create(&a, "Bob", "", false).unwrap();
        let (c, _) = s.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
        s.user_set_active(&a, r["user_id"].as_i64().unwrap(), false).unwrap();
        assert_eq!(s.authenticate_device(&c, "", "").unwrap_err().kind, "user_deactivated");
    }

    #[test]
    fn jobs_are_listed_most_urgent_first_and_completion_moves_the_due_date() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let today = d("2026-11-06");
        let mk = |t: &str, due: &str| JobInput {
            title: Some(t.into()),
            entry_url: Some("https://example.org".into()),
            interval_weeks: Some(4),
            retry_days: Some(2),
            next_due: Some(due.into()),
            ..Default::default()
        };
        s.job_create(&a, mk("Later", "2026-11-20"), today).unwrap();
        let over = s.job_create(&a, mk("Overdue", "2026-11-01"), today).unwrap();
        let list = s.job_list(&a, today).unwrap();
        assert_eq!(list["jobs"][0]["title"], "Overdue");
        assert_eq!(list["jobs"][0]["urgency_days"], -5);

        let id = over["job_id"].as_i64().unwrap();
        let r = s.job_complete(&a, id, "failed", "Portal down", Some(0), today).unwrap();
        assert_eq!(r["next_due"], "2026-11-08");
        let r = s.job_complete(&a, id, "succeeded", "", None, today).unwrap();
        assert_eq!(r["next_due"], "2026-12-04");
        assert_eq!(s.job_complete(&a, id, "succeeded", "", Some(1), today).unwrap_err().kind, "job_changed");
        let h = s.job_history(&a, id, today).unwrap();
        assert_eq!(h["runs"].as_array().unwrap().len(), 2);
        assert_eq!(h["runs"][0]["outcome"], "succeeded");
    }

    #[test]
    fn plain_text_credentials_are_refused() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let input = JobInput {
            title: Some("x".into()),
            entry_url: Some("https://example.org".into()),
            interval_weeks: Some(1),
            password_sealed: Some(r#"{"iv":"","ct":"hunter2"}"#.into()),
            ..Default::default()
        };
        assert_eq!(s.job_create(&a, input, d("2026-11-06")).unwrap_err().kind, "sealed_invalid");
    }

    fn job_with_login(s: &Store, a: &Principal) -> i64 {
        let input = JobInput {
            title: Some("x".into()),
            entry_url: Some("https://example.org".into()),
            interval_weeks: Some(1),
            password_sealed: Some(SEALED.into()),
            ..Default::default()
        };
        s.job_create(a, input, d("2026-11-06")).unwrap()["job_id"].as_i64().unwrap()
    }

    /// Registers an agent token and, if an ability is given, has the administrator grant it.
    fn agent(s: &Store, a: &Principal, ability: Option<&str>) -> (String, i64) {
        let r = s.agent_register("Agent of Ada; purpose: keep the job list tidy").unwrap();
        let id = r["agent_id"].as_i64().unwrap();
        if let Some(ab) = ability {
            s.agent_grant(a, id, ab).unwrap();
        }
        (r["token"].as_str().unwrap().to_string(), id)
    }

    #[test]
    fn a_registered_agent_can_do_nothing_until_granted() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let (tok, id) = agent(&s, &a, None);
        let p = s.authenticate_agent(&tok).unwrap();
        assert_eq!(s.job_list(&p, d("2026-11-06")).unwrap_err().kind, "agent_pending");
        s.agent_grant(&a, id, "jobs.read").unwrap();
        let p = s.authenticate_agent(&tok).unwrap();
        s.job_list(&p, d("2026-11-06")).unwrap();
        // A token's ability never changes; a second ability needs a second token.
        assert_eq!(s.agent_grant(&a, id, "jobs.write").unwrap_err().kind, "agent_granted");
        assert_eq!(s.agent_grant(&a, id, "everything").unwrap_err().kind, "ability_unknown");
    }

    #[test]
    fn an_agent_never_sees_or_sets_a_login() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let job = job_with_login(&s, &a);
        let (r, _) = agent(&s, &a, Some("jobs.read"));
        let r = s.authenticate_agent(&r).unwrap();
        let list = s.job_list(&r, d("2026-11-06")).unwrap();
        assert!(list["jobs"][0].get("password_sealed").is_none());
        assert!(list["jobs"][0].get("instructions_md").is_some());
        assert!(s.job_get(&r, job, d("2026-11-06")).unwrap().get("password_sealed").is_none());
        let (w, _) = agent(&s, &a, Some("jobs.write"));
        let w = s.authenticate_agent(&w).unwrap();
        let with_login = JobInput { password_sealed: Some(SEALED.into()), ..Default::default() };
        assert_eq!(s.job_update(&w, job, with_login, d("2026-11-06")).unwrap_err().code, 403);
        let rename = JobInput { title: Some("y".into()), ..Default::default() };
        let v = s.job_update(&w, job, rename, d("2026-11-06")).unwrap();
        assert!(v.get("password_sealed").is_none());
        // The login the person set stays in place.
        assert_eq!(s.job_get(&a, job, d("2026-11-06")).unwrap()["password_sealed"], SEALED);
    }

    #[test]
    fn each_ability_admits_its_own_methods_only() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let job = job_with_login(&s, &a);
        let (r, _) = agent(&s, &a, Some("jobs.read"));
        let r = s.authenticate_agent(&r).unwrap();
        assert_eq!(s.job_delete(&r, job).unwrap_err().code, 403);
        assert_eq!(s.job_complete(&r, job, "succeeded", "", None, d("2026-11-06")).unwrap_err().code, 403);
        assert_eq!(s.vault_get(&r).unwrap_err().code, 403);
        assert_eq!(s.user_list(&r).unwrap_err().code, 403);
        assert_eq!(s.agent_list(&r).unwrap_err().code, 403);
        let (c, _) = agent(&s, &a, Some("jobs.complete"));
        let c = s.authenticate_agent(&c).unwrap();
        s.job_complete(&c, job, "succeeded", "filed", None, d("2026-11-06")).unwrap();
        let h = s.job_history(&a, job, d("2026-11-06")).unwrap();
        let Principal::Agent { display, .. } = c else { panic!() };
        assert_eq!(h["runs"][0]["by"], format!("Agent {display}"));
    }

    #[test]
    fn agent_tokens_expire_and_can_be_revoked() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let (pending, pid) = agent(&s, &a, None);
        let (granted, gid) = agent(&s, &a, Some("jobs.read"));
        s.conn.execute("UPDATE agents SET created_at = ?1 WHERE id = ?2", params![ago(Duration::hours(AGENT_PENDING_HOURS + 1)), pid]).unwrap();
        assert_eq!(s.authenticate_agent(&pending).unwrap_err().kind, "agent_token_expired");
        assert_eq!(s.agent_grant(&a, pid, "jobs.read").unwrap_err().kind, "agent_not_waiting");
        let old = ago(Duration::days(AGENT_IDLE_DAYS + 1));
        s.conn.execute("UPDATE agents SET granted_at = ?1, last_used_at = ?1 WHERE id = ?2", params![old, gid]).unwrap();
        assert_eq!(s.authenticate_agent(&granted).unwrap_err().kind, "agent_token_expired");
        let (live, lid) = agent(&s, &a, Some("jobs.read"));
        s.agent_revoke(&a, lid).unwrap();
        assert_eq!(s.authenticate_agent(&live).unwrap_err().kind, "agent_token_revoked");
    }

    #[test]
    fn open_registration_is_throttled() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let ids: Vec<i64> = (0..AGENT_PENDING_MAX).map(|_| agent(&s, &a, None).1).collect();
        assert_eq!(s.agent_register("one registration too many").unwrap_err().code, 429);
        // Rejecting a waiting registration frees its place.
        s.agent_revoke(&a, ids[0]).unwrap();
        s.agent_register("one registration fits again").unwrap();
    }

    #[test]
    fn backup_file_is_written_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state")).unwrap();
        admin(&s);
        let target = dir.path().join("b.jsonl");
        let r = s.backup_to_file(&target).unwrap();
        assert!(r["lines"].as_u64().unwrap() >= 3);
        assert_eq!(s.backup_to_file(&target).unwrap_err().kind, "backup_exists");
        assert_eq!(s.backup_to_file(&dir.path().join("nowhere/b.jsonl")).unwrap_err().kind, "backup_dir_missing");
    }

    #[test]
    fn import_into_a_fresh_store_restores_everything_including_access() {
        let src = Store::open_in_memory().unwrap();
        let a = admin(&src);
        src.vault_create(&a, "c2FsdHNhbHRzYWx0c2FsdA==", 600_000, SEALED).unwrap();
        let job = job_with_login(&src, &a);
        src.job_complete(&a, job, "succeeded", "filed", None, d("2026-11-06")).unwrap();
        let r = src.user_create(&a, "Bob", "", false).unwrap();
        let (bob_cred, _) = src.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Phone", "", "").unwrap();
        let text = src.backup_text().unwrap();

        let dst = Store::open_in_memory().unwrap();
        let x = admin_named(&dst, "Setup");
        let rep = dst.backup_import(&x, &text, true, d("2026-11-07")).unwrap();
        assert_eq!(rep["vault"], "adopted");
        assert_eq!(rep["jobs_added"], 1);
        assert_eq!(rep["runs_added"], 1);
        assert_eq!(rep["renamed"].as_array().unwrap().len(), 0);
        // The imported device credential works on the new store, and the login came along.
        dst.authenticate_device(&bob_cred, "", "").unwrap();
        let jobs = dst.job_list(&x, d("2026-11-07")).unwrap();
        assert_eq!(jobs["jobs"][0]["password_sealed"], SEALED);
        assert_eq!(jobs["jobs"][0]["last_run"]["by"], "Ada");
    }

    #[test]
    fn import_adds_what_exists_as_a_marked_copy_and_can_leave_access_out() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        job_with_login(&s, &a);
        let text = s.backup_text().unwrap();
        let rep = s.backup_import(&a, &text, true, d("2026-11-07")).unwrap();
        let renamed: Vec<String> = rep["renamed"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
        assert!(renamed.contains(&"x (Import 2026-11-07)".to_string()), "{renamed:?}");
        assert!(renamed.contains(&"Ada (Import 2026-11-07)".to_string()), "{renamed:?}");
        // The credentials exist already, so the copied person gets no second device.
        assert_eq!(rep["devices_added"], 0);
        let rep = s.backup_import(&a, &text, false, d("2026-11-07")).unwrap();
        assert_eq!(rep["people_added"], 0);
        assert_eq!(rep["jobs_added"], 1);
        // A renamed person can be deleted again; their runs keep the name.
        let copy = rep_user(&s, "Ada (Import 2026-11-07)");
        s.user_delete(&a, copy).unwrap();
        let Principal::Device { user_id, .. } = a.clone() else { panic!() };
        assert_eq!(s.user_delete(&a, user_id).unwrap_err().kind, "user_self");
    }

    #[test]
    fn import_from_another_vault_drops_the_logins_it_cannot_open() {
        let src = Store::open_in_memory().unwrap();
        let a = admin(&src);
        src.vault_create(&a, "c2FsdHNhbHRzYWx0c2FsdA==", 600_000, SEALED).unwrap();
        job_with_login(&src, &a);
        let dst = Store::open_in_memory().unwrap();
        let b = admin(&dst);
        dst.vault_create(&b, "b3RoZXJzYWx0b3RoZXJzYQ==", 600_000, SEALED).unwrap();
        let rep = dst.backup_import(&b, &src.backup_text().unwrap(), false, d("2026-11-07")).unwrap();
        assert_eq!(rep["vault"], "different");
        assert_eq!(rep["logins_dropped"], 1);
        assert!(dst.job_list(&b, d("2026-11-07")).unwrap()["jobs"][0]["password_sealed"].as_str().unwrap().is_empty());
        assert_eq!(dst.backup_import(&b, "{\"type\":\"nonsense\"}", true, d("2026-11-07")).unwrap_err().kind, "backup_invalid");
    }

    #[test]
    fn backup_is_for_administrators_and_backup_tokens_only() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        job_with_login(&s, &a);
        s.backup_export(&a).unwrap();
        let (r, _) = agent(&s, &a, Some("jobs.read"));
        assert_eq!(s.backup_export(&s.authenticate_agent(&r).unwrap()).unwrap_err().code, 403);
        let made = s.agent_create(&a, "NAS nightly backup", "backup.read").unwrap();
        let b = s.authenticate_agent(made["token"].as_str().unwrap()).unwrap();
        assert!(s.backup_export(&b).unwrap().contains("\"password_sealed\""));
        let u = s.user_create(&a, "Bob", "", false).unwrap();
        let (c, _) = s.redeem_invitation(u["invitation_token"].as_str().unwrap(), "Phone", "", "").unwrap();
        assert_eq!(s.backup_export(&s.authenticate_device(&c, "", "").unwrap()).unwrap_err().code, 403);
    }

    fn admin_named(store: &Store, name: &str) -> Principal {
        let r = store.setup_create_admin(name, "").unwrap();
        let (cred, _) = store.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
        store.authenticate_device(&cred, "", "").unwrap()
    }

    fn rep_user(s: &Store, name: &str) -> i64 {
        s.conn.query_row("SELECT id FROM users WHERE name = ?1", [name], |r| r.get(0)).unwrap()
    }

    #[test]
    fn vault_is_created_once() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let v = SEALED;
        s.vault_create(&a, "c2FsdHNhbHRzYWx0c2FsdA==", 600_000, v).unwrap();
        assert_eq!(s.vault_create(&a, "c2FsdHNhbHRzYWx0c2FsdA==", 600_000, v).unwrap_err().kind, "vault_exists");
    }

    #[test]
    fn markdown_is_sanitized() {
        let h = render_markdown("# Hi\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>");
        assert!(h.contains("<h1>Hi</h1>"));
        assert!(!h.contains("<script"));
        assert!(!h.contains("href=\"javascript"));
        assert!(!h.contains("onerror"));
    }
}
