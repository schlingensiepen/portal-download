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
/// The only task a machine key can carry in this version.
pub const MACHINE_TASKS: &[&str] = &["job.list"];

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
    Machine { key_id: i64, task: String },
}

impl Principal {
    pub fn require_person(&self) -> OpResult<(i64, i64, bool)> {
        match self {
            Principal::Device { device_id, user_id, is_admin, .. } => Ok((*device_id, *user_id, *is_admin)),
            Principal::Machine { .. } => Err(OpError::forbidden(
                "A machine key carries one task only; this call needs a person's device. Open the application in a browser with an invitation link instead.",
            )),
        }
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
    let parser = pulldown_cmark::Parser::new_ext(md, pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH | pulldown_cmark::Options::ENABLE_TASKLISTS);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    ammonia::clean(&html)
}

fn parse_date(field: &str, s: &str) -> OpResult<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
        OpError::bad("date_invalid", format!("`{field}` = \"{s}\" is not a date. Write it as YYYY-MM-DD, for example \"2026-12-01\"."))
    })
}

fn check_sealed(field: &str, s: &str) -> OpResult<()> {
    if s.is_empty() {
        return Ok(());
    }
    let bad = || {
        OpError::bad(
            "sealed_invalid",
            format!("`{field}` must be empty or a value sealed in the browser with the vault key, a JSON object with the string fields `iv` and `ct`. Unlock the vault in the application and save the job again."),
        )
    };
    if s.len() > 8192 {
        return Err(bad());
    }
    let v: Value = serde_json::from_str(s).map_err(|_| bad())?;
    if v.get("iv").and_then(Value::as_str).is_none() || v.get("ct").and_then(Value::as_str).is_none() {
        return Err(bad());
    }
    Ok(())
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
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id),
  token_hash TEXT NOT NULL UNIQUE, created_by INTEGER REFERENCES users(id),
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, withdrawn_at TEXT);
CREATE TABLE devices(
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id),
  invitation_id INTEGER NOT NULL REFERENCES invitations(id),
  credential_hash TEXT NOT NULL UNIQUE, label TEXT NOT NULL,
  user_agent TEXT NOT NULL DEFAULT '', last_address TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, revoked_at TEXT);
CREATE TABLE machine_keys(
  id INTEGER PRIMARY KEY, machine TEXT NOT NULL, task TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE, display TEXT NOT NULL, created_at TEXT NOT NULL,
  last_used_at TEXT, revoked_at TEXT);
CREATE TABLE vault(
  id INTEGER PRIMARY KEY CHECK (id = 1), salt TEXT NOT NULL, iterations INTEGER NOT NULL,
  verifier TEXT NOT NULL, created_by INTEGER REFERENCES users(id), created_at TEXT NOT NULL);
CREATE TABLE jobs(
  id INTEGER PRIMARY KEY, title TEXT NOT NULL, entry_url TEXT NOT NULL,
  embeddable INTEGER NOT NULL DEFAULT 0, interval_weeks INTEGER NOT NULL,
  retry_days INTEGER NOT NULL DEFAULT 2, next_due TEXT NOT NULL,
  username_sealed TEXT NOT NULL DEFAULT '', password_sealed TEXT NOT NULL DEFAULT '',
  instructions_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE runs(
  id INTEGER PRIMARY KEY, job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL REFERENCES users(id), device_id INTEGER REFERENCES devices(id),
  outcome TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', finished_at TEXT NOT NULL,
  due_before TEXT NOT NULL, due_after TEXT NOT NULL);
PRAGMA user_version = 1;
"#,
            )?;
            tx.commit()?;
        }
        Ok(())
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

    pub fn authenticate_machine(&self, key: &str) -> OpResult<Principal> {
        let row = self
            .conn
            .query_row("SELECT id, task, revoked_at FROM machine_keys WHERE key_hash = ?1", [token::hash(key)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
            })
            .optional()?;
        match row {
            Some((key_id, task, None)) => {
                self.conn.execute("UPDATE machine_keys SET last_used_at = ?1 WHERE id = ?2", params![now(), key_id])?;
                Ok(Principal::Machine { key_id, task })
            }
            Some((_, _, Some(_))) => Err(OpError::unauthorized("machine_key_revoked", "This machine key was shut down. Ask the operator for a new key for this machine and task.")),
            None => Err(OpError::unauthorized(
                "machine_key_unknown",
                "The machine key in the Authorization header is not known. Check that the whole key was copied, including its prefix `pdk_`.",
            )),
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
            Principal::Machine { key_id, task } => Ok(json!({ "kind": "machine", "machine_key_id": key_id, "task": task })),
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
        let owner: Option<i64> = self.conn.query_row("SELECT user_id FROM devices WHERE id = ?1", [device_id], |r| r.get(0)).optional()?;
        let Some(owner) = owner else {
            return Err(OpError::not_found("device_unknown", format!("There is no device {device_id}. Call device.list for your own devices.")));
        };
        if owner != me && !is_admin {
            return Err(OpError::forbidden("You can revoke only your own devices. Ask a person with the right to administer people to revoke this one."));
        }
        self.conn.execute("UPDATE devices SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), device_id])?;
        Ok(json!({ "device_id": device_id, "revoked": true }))
    }

    // ---------------------------------------------------------------- machine keys

    /// Bootstrap only: one key for one task on one machine; the clear text is returned exactly once.
    pub fn machine_key_create(&self, machine: &str, task: &str) -> OpResult<Value> {
        let machine = text("machine", machine, 1, 80)?;
        if !MACHINE_TASKS.contains(&task) {
            return Err(OpError::bad("task_unknown", format!("`task` = \"{task}\" is not a task a machine key can carry. The possible tasks are: {}.", MACHINE_TASKS.join(", "))));
        }
        let key = token::generate("pdk_");
        let display = token::display_string(&key);
        self.conn.execute(
            "INSERT INTO machine_keys(machine, task, key_hash, display, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![machine, task, token::hash(&key), display, now()],
        )?;
        Ok(json!({ "machine_key_id": self.conn.last_insert_rowid(), "machine": machine, "task": task, "display": display, "key": key }))
    }

    pub fn machine_key_list(&self, p: &Principal) -> OpResult<Value> {
        p.require_admin()?;
        let mut st = self.conn.prepare("SELECT id, machine, task, display, created_at, last_used_at, revoked_at FROM machine_keys ORDER BY id")?;
        let keys: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                    "machine_key_id": r.get::<_, i64>(0)?, "machine": r.get::<_, String>(1)?, "task": r.get::<_, String>(2)?,
                    "display": r.get::<_, String>(3)?, "created_at": r.get::<_, String>(4)?,
                    "last_used_at": r.get::<_, Option<String>>(5)?, "revoked_at": r.get::<_, Option<String>>(6)?,
                }))
            })?
            .collect::<Result<_, _>>()?;
        Ok(json!({ "machine_keys": keys }))
    }

    /// Shutting a machine key down needs no bootstrap window.
    pub fn machine_key_revoke(&self, p: &Principal, machine_key_id: i64) -> OpResult<Value> {
        p.require_admin()?;
        let n = self.conn.execute("UPDATE machine_keys SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), machine_key_id])?;
        if n == 0 {
            return Err(OpError::not_found("machine_key_unknown", format!("There is no machine key {machine_key_id}. Call machinekey.list for the valid identifiers.")));
        }
        Ok(json!({ "machine_key_id": machine_key_id, "revoked": true }))
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

    fn row_to_job(&self, r: &rusqlite::Row, today: NaiveDate, with_secrets: bool) -> rusqlite::Result<Job> {
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
            next_due,
            username_sealed: with_secrets.then(|| r.get(7)).transpose()?,
            password_sealed: with_secrets.then(|| r.get(8)).transpose()?,
            instructions_html: with_secrets.then(|| render_markdown(&md)),
            instructions_md: with_secrets.then_some(md),
            last_run: None,
        })
    }

    const JOB_COLS: &'static str = "id, title, entry_url, embeddable, interval_weeks, retry_days, next_due, username_sealed, password_sealed, instructions_md";

    fn last_run(&self, job_id: i64) -> OpResult<Option<Value>> {
        Ok(self
            .conn
            .query_row(
                "SELECT r.outcome, r.finished_at, r.note, u.name FROM runs r JOIN users u ON u.id = r.user_id WHERE r.job_id = ?1 ORDER BY r.id DESC LIMIT 1",
                [job_id],
                |r| Ok(json!({ "outcome": r.get::<_, String>(0)?, "finished_at": r.get::<_, String>(1)?, "note": r.get::<_, String>(2)?, "by": r.get::<_, String>(3)? })),
            )
            .optional()?)
    }

    /// All jobs, most urgent first (smallest number of days to the next due date).
    pub fn job_list(&self, p: &Principal, today: NaiveDate) -> OpResult<Value> {
        let with_secrets = match p {
            Principal::Device { .. } => true,
            Principal::Machine { task, .. } if task == "job.list" => false,
            Principal::Machine { .. } => return Err(OpError::forbidden("This machine key does not carry the task job.list.")),
        };
        let mut st = self.conn.prepare(&format!("SELECT {} FROM jobs ORDER BY next_due, title COLLATE NOCASE", Self::JOB_COLS))?;
        let mut jobs: Vec<Job> = st.query_map([], |r| self.row_to_job(r, today, with_secrets))?.collect::<Result<_, _>>()?;
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
        p.require_person()?;
        Ok(serde_json::to_value(self.job_get_inner(job_id, today)?).expect("job serializes"))
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
        p.require_person()?;
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
        self.job_get(p, self.conn.last_insert_rowid(), today)
    }

    pub fn job_update(&self, p: &Principal, job_id: i64, input: JobInput, today: NaiveDate) -> OpResult<Value> {
        p.require_person()?;
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
        self.job_get(p, job_id, today)
    }

    pub fn job_delete(&self, p: &Principal, job_id: i64) -> OpResult<Value> {
        p.require_person()?;
        let n = self.conn.execute("DELETE FROM jobs WHERE id = ?1", [job_id])?;
        if n == 0 {
            return Err(OpError::not_found("job_unknown", format!("There is no job {job_id}. Call job.list for the valid identifiers.")));
        }
        Ok(json!({ "job_id": job_id, "deleted": true }))
    }

    /// Closes a run: records it and moves the next due date according to the outcome.
    pub fn job_complete(&self, p: &Principal, job_id: i64, outcome: &str, note: &str, today: NaiveDate) -> OpResult<Value> {
        let (device_id, user_id, _) = p.require_person()?;
        let outcome = Outcome::parse(outcome)
            .ok_or_else(|| OpError::bad("outcome_invalid", format!("`outcome` = \"{outcome}\" is not possible. Send \"succeeded\" when everything was filed, \"failed\" otherwise.")))?;
        let note = text("note", note, 0, 2000)?;
        let job = self.job_get_inner(job_id, today)?;
        let due_after = schedule::next_due(today, outcome, job.interval_weeks, job.retry_days).format("%Y-%m-%d").to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs(job_id, user_id, device_id, outcome, note, finished_at, due_before, due_after) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![job_id, user_id, device_id, outcome.as_str(), note, now(), job.next_due, due_after],
        )?;
        tx.execute("UPDATE jobs SET next_due = ?1, updated_at = ?2 WHERE id = ?3", params![due_after, now(), job_id])?;
        tx.commit()?;
        Ok(json!({ "job_id": job_id, "outcome": outcome.as_str(), "due_before": job.next_due, "next_due": due_after }))
    }

    pub fn job_history(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        p.require_person()?;
        self.job_get_inner(job_id, today)?;
        let mut st = self.conn.prepare(
            "SELECT r.id, r.outcome, r.note, r.finished_at, r.due_before, r.due_after, u.name FROM runs r JOIN users u ON u.id = r.user_id WHERE r.job_id = ?1 ORDER BY r.id DESC LIMIT 200",
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
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
        let input = JobInput { title: Some("Telekom".into()), entry_url: Some("https://telekom.de".into()), interval_weeks: Some(4), ..Default::default() };
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
        let r = s.job_complete(&a, id, "failed", "Portal down", today).unwrap();
        assert_eq!(r["next_due"], "2026-11-08");
        let r = s.job_complete(&a, id, "succeeded", "", today).unwrap();
        assert_eq!(r["next_due"], "2026-12-04");
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
            password_sealed: Some("hunter2".into()),
            ..Default::default()
        };
        assert_eq!(s.job_create(&a, input, d("2026-11-06")).unwrap_err().kind, "sealed_invalid");
    }

    #[test]
    fn machine_key_sees_due_list_without_sealed_values() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let input = JobInput {
            title: Some("x".into()),
            entry_url: Some("https://example.org".into()),
            interval_weeks: Some(1),
            password_sealed: Some(r#"{"iv":"a","ct":"b"}"#.into()),
            ..Default::default()
        };
        s.job_create(&a, input, d("2026-11-06")).unwrap();
        let k = s.machine_key_create("backup-host", "job.list").unwrap();
        let m = s.authenticate_machine(k["key"].as_str().unwrap()).unwrap();
        let list = s.job_list(&m, d("2026-11-06")).unwrap();
        assert!(list["jobs"][0].get("password_sealed").is_none());
        assert_eq!(s.vault_get(&m).unwrap_err().code, 403);
        s.machine_key_revoke(&a, k["machine_key_id"].as_i64().unwrap()).unwrap();
        assert_eq!(s.authenticate_machine(k["key"].as_str().unwrap()).unwrap_err().kind, "machine_key_revoked");
    }

    #[test]
    fn vault_is_created_once() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let v = r#"{"iv":"a","ct":"b"}"#;
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
