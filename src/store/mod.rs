//! The store and every operation on it. Operations know neither HTTP nor process exit:
//! they take arguments, act, and return a result or an [`OpError`].
//!
//! `mod.rs` holds the structure, who is calling and what they may do, people, devices and
//! agent tokens; `jobs.rs` the jobs and their runs; `exchange.rs` the ZIP file that carries
//! jobs between instances and holds the backup.

mod exchange;
mod jobs;

pub use exchange::{check_password, ImportMode};
pub use jobs::JobInput;

use crate::error::{OpError, OpResult};
use crate::token;
use chrono::{Duration, NaiveDate, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

/// How long an invitation link can be redeemed. Decided with the operator: seven days.
pub const INVITATION_DAYS: i64 = 7;
/// How long a person's own invitation for a further device of theirs can be redeemed, once.
pub const OWN_INVITATION_MINUTES: i64 = 60;
/// A registered agent token that nobody grants a role expires after this many hours.
pub const AGENT_PENDING_HOURS: i64 = 24;
/// A granted agent token expires after this many days without use.
pub const AGENT_IDLE_DAYS: i64 = 180;
/// At most this many registrations wait for a grant at the same time.
pub const AGENT_PENDING_MAX: i64 = 10;

/// The structure this program writes. Versions 1 and 2 were written by 0.7, whose data this
/// program does not read: it starts with an empty store.
const SCHEMA_VERSION: i64 = 10;
/// The store file inside the state directory.
pub const STORE_FILE: &str = "portal-download.sqlite";

pub struct Store {
    conn: Connection,
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

// -------------------------------------------------------------------- rights and roles

/// The rights to administer that a person holds besides working on jobs, which every
/// person may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rights {
    #[serde(default)]
    pub people: bool,
    #[serde(default)]
    pub agents: bool,
    #[serde(default)]
    pub backup: bool,
}

impl Rights {
    pub const ALL: Rights = Rights { people: true, agents: true, backup: true };
    fn covers(&self, other: &Rights) -> bool {
        (self.people || !other.people) && (self.agents || !other.agents) && (self.backup || !other.backup)
    }
}

/// The role an agent token carries, exactly one per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    JobsRead,
    JobsWork,
    JobsManage,
    JobsAll,
    BackupRead,
    PeopleManage,
    AgentsManage,
}

/// Every role with what it permits, in the order the application offers them.
pub const ROLES: &[(Role, &str, &str)] = &[
    (Role::JobsRead, "jobs.read", "read jobs, their instructions and history, without the logins"),
    (Role::JobsWork, "jobs.work", "work through the jobs: read them with their logins, close runs, export them"),
    (Role::JobsManage, "jobs.manage", "manage jobs: read them without the logins, create, change and delete them (setting their logins), close runs, import them"),
    (Role::JobsAll, "jobs.all", "everything on jobs: jobs.work and jobs.manage together"),
    (Role::BackupRead, "backup.read", "download the whole backup, with the logins in plain text and the people, devices and tokens"),
    (Role::PeopleManage, "people.manage", "manage people as a person with the right to manage people does: invite them, see and revoke every device"),
    (Role::AgentsManage, "agents.manage", "manage agent tokens: grant, create and revoke them, with the roles the person who granted this token could grant"),
];

impl Role {
    pub fn name(self) -> &'static str {
        ROLES.iter().find(|(r, _, _)| *r == self).map(|(_, n, _)| *n).expect("every role is listed")
    }
    pub fn parse(s: &str) -> OpResult<Role> {
        ROLES.iter().find(|(_, n, _)| *n == s).map(|(r, _, _)| *r).ok_or_else(|| {
            let names: Vec<_> = ROLES.iter().map(|(_, n, _)| *n).collect();
            OpError::bad("role_unknown", format!("`role` = \"{s}\" is not a role. The possible roles are: {}.", names.join(", ")))
        })
    }
    fn job_roles() -> Vec<Role> {
        vec![Role::JobsRead, Role::JobsWork, Role::JobsManage, Role::JobsAll]
    }
}

fn roles_text(roles: &[Role]) -> String {
    roles.iter().map(|r| r.name()).collect::<Vec<_>>().join(",")
}

fn roles_of(text: &str) -> Vec<Role> {
    text.split(',').filter_map(|s| Role::parse(s).ok()).collect()
}

/// What a call needs; the method table names one for every method a principal may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    ReadJobs,
    WriteJobs,
    Complete,
    ExportJobs,
    ImportJobs,
    BackupRead,
    BackupImport,
    ManagePeople,
    ManageAgents,
}

impl Cap {
    /// The agent roles that admit this; empty means people only.
    pub fn roles(self) -> &'static [Role] {
        use Role::*;
        match self {
            Cap::ReadJobs => &[JobsRead, JobsWork, JobsManage, JobsAll],
            Cap::WriteJobs | Cap::ImportJobs => &[JobsManage, JobsAll],
            Cap::Complete => &[JobsWork, JobsManage, JobsAll],
            Cap::ExportJobs => &[JobsWork, JobsAll],
            Cap::BackupRead => &[BackupRead],
            Cap::BackupImport => &[],
            Cap::ManagePeople => &[PeopleManage],
            Cap::ManageAgents => &[AgentsManage],
        }
    }
    /// The person's right this needs, if any; working on jobs needs none.
    pub fn right(self) -> Option<&'static str> {
        match self {
            Cap::BackupRead => Some("backup"),
            Cap::BackupImport => Some("people, agents and the backup"),
            Cap::ManagePeople => Some("people"),
            Cap::ManageAgents => Some("agents"),
            _ => None,
        }
    }
}

/// Who is calling, established before any method runs.
#[derive(Debug, Clone)]
pub enum Principal {
    Device { device_id: i64, user_id: i64, rights: Rights, name: String },
    /// An agent token; `role` is None until it is granted one. `reach` holds the roles an
    /// agent with agents.manage may grant.
    Agent { agent_id: i64, display: String, role: Option<Role>, reach: Vec<Role>, granted_by: Option<String> },
}

impl Principal {
    pub fn require_person(&self) -> OpResult<(i64, i64, Rights)> {
        match self {
            Principal::Device { device_id, user_id, rights, .. } => Ok((*device_id, *user_id, *rights)),
            Principal::Agent { .. } => Err(OpError::forbidden(
                "This call is reserved for people; an agent token cannot make it. Ask the user to do it in the application.",
            )),
        }
    }

    pub fn can(&self, cap: Cap) -> bool {
        match self {
            Principal::Device { rights, .. } => match cap {
                Cap::BackupRead => rights.backup,
                Cap::BackupImport => *rights == Rights::ALL,
                Cap::ManagePeople => rights.people,
                Cap::ManageAgents => rights.agents,
                _ => true,
            },
            Principal::Agent { role: Some(r), .. } => cap.roles().contains(r),
            Principal::Agent { role: None, .. } => false,
        }
    }

    /// Admits the caller to what the call needs, or says what is missing.
    pub fn require(&self, cap: Cap) -> OpResult<()> {
        if self.can(cap) {
            return Ok(());
        }
        let roles = roles_text(cap.roles()).replace(',', " or ");
        Err(match self {
            Principal::Device { .. } => OpError::forbidden(format!(
                "This call needs the right to manage {}. Ask a person who holds it to do it, or to give you the right.",
                cap.right().unwrap_or("jobs")
            )),
            Principal::Agent { role: Some(r), .. } if roles.is_empty() => OpError::forbidden(format!(
                "This agent token carries the role {}; this call is reserved for people. Ask the user to do it in the application.",
                r.name()
            )),
            Principal::Agent { role: Some(r), .. } => OpError::forbidden(format!(
                "This agent token carries the role {}; this call needs {roles}. Register a second token and have it granted one of them.",
                r.name()
            )),
            Principal::Agent { display, role: None, .. } => OpError::new(
                403,
                "agent_pending",
                format!("This agent token has no role yet. Tell the user its display string {display} and ask them to have a person who manages agents grant it {} in the application under Agents.", if roles.is_empty() { "a role" } else { &roles }),
            ),
        })
    }

    /// Every person sees the logins; an agent only with jobs.work or jobs.all.
    pub fn sees_logins(&self) -> bool {
        match self {
            Principal::Device { .. } => true,
            Principal::Agent { role, .. } => matches!(role, Some(Role::JobsWork | Role::JobsAll)),
        }
    }

    /// How the caller is named in runs, invitations and grants.
    pub fn by_name(&self) -> String {
        match self {
            Principal::Device { name, .. } => name.clone(),
            Principal::Agent { display, granted_by: Some(g), .. } => format!("Agent {display} ({g})"),
            Principal::Agent { display, .. } => format!("Agent {display}"),
        }
    }

    /// The roles the caller may grant an agent token.
    fn grantable_roles(&self) -> Vec<Role> {
        match self {
            Principal::Device { rights, .. } => {
                let mut v = Role::job_roles();
                if rights.backup {
                    v.push(Role::BackupRead);
                }
                if rights.people {
                    v.push(Role::PeopleManage);
                }
                if rights.agents {
                    v.push(Role::AgentsManage);
                }
                v
            }
            Principal::Agent { role: Some(Role::AgentsManage), reach, .. } => reach.clone(),
            Principal::Agent { .. } => vec![],
        }
    }

    /// The rights the caller may give a person or take away.
    fn grantable_rights(&self) -> Rights {
        match self {
            Principal::Device { rights, .. } => *rights,
            Principal::Agent { role: Some(Role::PeopleManage), .. } => Rights { people: true, ..Rights::default() },
            Principal::Agent { .. } => Rights::default(),
        }
    }
}

// -------------------------------------------------------------------- checks of input

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

/// A login is kept exactly as entered, spaces included; only its length and control
/// characters are checked.
fn check_login(field: &str, s: &str) -> OpResult<()> {
    let n = s.chars().count();
    if n > 1000 {
        return Err(OpError::bad("field_length", format!("`{field}` may have at most 1000 characters; it has {n}. Shorten it and send again.")));
    }
    if s.chars().any(char::is_control) {
        return Err(OpError::bad("login_invalid", format!("`{field}` contains a line break or another control character. Enter the login in one line and send again.")));
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

fn plus(ts: &str, d: Duration) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| (t.with_timezone(&Utc) + d).to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|_| ts.to_string())
}

fn ago(d: Duration) -> String {
    (Utc::now() - d).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A random identifier in the form of a version 4 UUID.
pub fn new_guid() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("the operating system's random source is available");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

// -------------------------------------------------------------------- agent rows

const AGENT_COLS: &str = "id, display, introduction, role, reach, granted_by, granted_at, created_at, last_used_at, revoked_at";

struct AgentRow {
    id: i64,
    display: String,
    introduction: String,
    role: Option<String>,
    reach: String,
    granted_by: Option<String>,
    granted_at: Option<String>,
    created_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

fn agent_row(r: &rusqlite::Row) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: r.get(0)?,
        display: r.get(1)?,
        introduction: r.get(2)?,
        role: r.get(3)?,
        reach: r.get(4)?,
        granted_by: r.get(5)?,
        granted_at: r.get(6)?,
        created_at: r.get(7)?,
        last_used_at: r.get(8)?,
        revoked_at: r.get(9)?,
    })
}

impl AgentRow {
    /// When the token expires unless it is used (granted) or granted (pending).
    fn expires_at(&self) -> String {
        match (&self.role, &self.granted_at) {
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
        } else if self.role.is_some() {
            "granted"
        } else {
            "pending"
        }
    }
    fn to_json(&self) -> Value {
        json!({
            "agent_id": self.id, "display": self.display, "introduction": self.introduction,
            "state": self.state(), "role": self.role, "granted_by": self.granted_by, "granted_at": self.granted_at,
            "created_at": self.created_at, "last_used_at": self.last_used_at, "revoked_at": self.revoked_at,
            "expires_at": self.expires_at(),
        })
    }
}

// -------------------------------------------------------------------- the store

const STRUCTURE: &str = r#"
CREATE TABLE users(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL DEFAULT '',
  can_people INTEGER NOT NULL DEFAULT 0, can_agents INTEGER NOT NULL DEFAULT 0, can_backup INTEGER NOT NULL DEFAULT 0,
  deactivated_at TEXT, created_at TEXT NOT NULL, created_by TEXT NOT NULL DEFAULT '');
CREATE TABLE invitations(
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE, own INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, withdrawn_at TEXT, redeemed_at TEXT);
CREATE TABLE devices(
  id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  invitation_id INTEGER REFERENCES invitations(id) ON DELETE SET NULL,
  kind TEXT NOT NULL CHECK (kind IN ('browser', 'extension')),
  credential_hash TEXT NOT NULL UNIQUE, label TEXT NOT NULL,
  user_agent TEXT NOT NULL DEFAULT '', last_address TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, revoked_at TEXT);
CREATE TABLE agents(
  id INTEGER PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, display TEXT NOT NULL,
  introduction TEXT NOT NULL, role TEXT, reach TEXT NOT NULL DEFAULT '', granted_by TEXT,
  granted_at TEXT, created_at TEXT NOT NULL, last_used_at TEXT, revoked_at TEXT);
CREATE TABLE jobs(
  id INTEGER PRIMARY KEY, guid TEXT NOT NULL UNIQUE, replaces TEXT,
  title TEXT NOT NULL, entry_url TEXT NOT NULL, interval_weeks INTEGER NOT NULL, retry_days INTEGER NOT NULL,
  next_due TEXT NOT NULL, username TEXT NOT NULL DEFAULT '', password TEXT NOT NULL DEFAULT '',
  instructions_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
  title_at TEXT NOT NULL, interval_weeks_at TEXT NOT NULL, retry_days_at TEXT NOT NULL, next_due_at TEXT NOT NULL,
  username_at TEXT NOT NULL, password_at TEXT NOT NULL, instructions_md_at TEXT NOT NULL);
CREATE TABLE runs(
  id INTEGER PRIMARY KEY, guid TEXT NOT NULL UNIQUE, job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  by_name TEXT NOT NULL, outcome TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', finished_at TEXT NOT NULL,
  due_before TEXT NOT NULL, due_after TEXT NOT NULL);
CREATE TABLE deleted_jobs(
  guid TEXT PRIMARY KEY, title TEXT NOT NULL, entry_url TEXT NOT NULL, deleted_at TEXT NOT NULL, replaced_by TEXT);
"#;

impl Store {
    pub fn open(state_dir: &Path) -> OpResult<Self> {
        std::fs::create_dir_all(state_dir).map_err(|e| {
            OpError::internal(format!("The state directory {} cannot be created: {e}. Create it and make it writable for the service user.", state_dir.display()))
        })?;
        let conn = Connection::open(state_dir.join(STORE_FILE))?;
        Self::from_conn(conn, &state_dir.join(STORE_FILE))
    }

    #[cfg(test)]
    pub fn open_in_memory() -> OpResult<Self> {
        Self::from_conn(Connection::open_in_memory()?, Path::new(":memory:"))
    }

    fn from_conn(conn: Connection, file: &Path) -> OpResult<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        let s = Store { conn };
        s.prepare_structure(file)?;
        Ok(s)
    }

    /// Creates the structure in an empty store; refuses one of another structure.
    fn prepare_structure(&self, file: &Path) -> OpResult<()> {
        let v: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match v {
            0 => {
                let tables: i64 = self.conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
                if tables > 0 {
                    return Err(OpError::internal(format!("{} holds tables of another program. Name an empty state directory.", file.display())));
                }
                self.conn.execute_batch(&format!("BEGIN; {STRUCTURE} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"))?;
            }
            SCHEMA_VERSION => {}
            1 | 2 => {
                return Err(OpError::internal(format!(
                    "{} was written by portal-download 0.7 (structure version {v}). This version starts with an empty store and reads no older data: stop the service, move the file away and start again; then import the logins anew.",
                    file.display()
                )))
            }
            _ => {
                return Err(OpError::internal(format!(
                    "{} has structure version {v}; this program writes {SCHEMA_VERSION}. Run the program version that wrote it.",
                    file.display()
                )))
            }
        }
        self.check_structure()
    }

    /// The structure version alone does not prove the structure: a store written by a
    /// development build can carry the same number. Every table and column the operations
    /// use must be there, or the store is refused at start instead of failing on first use.
    fn check_structure(&self) -> OpResult<()> {
        let check = Connection::open_in_memory()?;
        check.execute_batch(STRUCTURE)?;
        let mut st = check.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
        let tables: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for table in tables {
            let cols = |c: &Connection| -> OpResult<Vec<String>> {
                let mut st = c.prepare("SELECT name FROM pragma_table_info(?1)")?;
                let v = st.query_map([&table], |r| r.get(0))?.collect::<Result<_, _>>()?;
                Ok(v)
            };
            let have = cols(&self.conn)?;
            if let Some(missing) = cols(&check)?.into_iter().find(|c| !have.contains(c)) {
                return Err(OpError::internal(format!(
                    "The store does not have the structure this program needs: table {table} lacks {missing}. It was written by a different build of the same structure version. Take a backup with that build if you need the data, move the store away, start this program on an empty state directory and import the backup."
                )));
            }
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

    /// Whether some active person may manage people; otherwise the setup is how to get in.
    pub fn has_active_admin(&self) -> OpResult<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE can_people = 1 AND deactivated_at IS NULL)", [], |r| r.get(0))?)
    }

    pub fn counts(&self) -> OpResult<Value> {
        let q = |sql: &str| -> OpResult<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        let today = today().format("%Y-%m-%d").to_string();
        let due: i64 = self.conn.query_row("SELECT COUNT(*) FROM jobs WHERE next_due <= ?1", [today], |r| r.get(0))?;
        Ok(json!({
            "schema_version": self.schema_version()?,
            "users": q("SELECT COUNT(*) FROM users WHERE deactivated_at IS NULL")?,
            "admins": q("SELECT COUNT(*) FROM users WHERE can_people = 1 AND deactivated_at IS NULL")?,
            "devices": q("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL")?,
            "jobs": q("SELECT COUNT(*) FROM jobs")?,
            "jobs_due": due,
        }))
    }

    // ---------------------------------------------------------------- access

    /// Resolves a device credential: credential → device not revoked → user not deactivated.
    /// A browser's credential counts only in its cookie, an extension's only as a bearer token.
    pub fn authenticate_device(&self, credential: &str, kind: &str, user_agent: &str, address: &str) -> OpResult<Principal> {
        let row = self
            .conn
            .query_row(
                "SELECT d.id, d.user_id, d.revoked_at, d.kind, u.deactivated_at, u.can_people, u.can_agents, u.can_backup, u.name
                 FROM devices d JOIN users u ON u.id = d.user_id WHERE d.credential_hash = ?1",
                [token::hash(credential)],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        Rights { people: r.get(5)?, agents: r.get(6)?, backup: r.get(7)? },
                        r.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        let unknown = || {
            OpError::unauthorized(
                "credential_unknown",
                "This device holds no valid access. Open your invitation link again on this device, or ask a person in the circle for a new one.",
            )
        };
        let Some((device_id, user_id, revoked, have_kind, deactivated, rights, name)) = row else {
            return Err(unknown());
        };
        if have_kind != kind {
            return Err(unknown());
        }
        if revoked.is_some() {
            return Err(OpError::unauthorized(
                "device_revoked",
                "Access from this device was revoked. Open your invitation link again to register it anew, or ask for a new invitation.",
            ));
        }
        if deactivated.is_some() {
            return Err(OpError::unauthorized("user_deactivated", "Your access was deactivated. Ask a person who manages people to reactivate you."));
        }
        let ua: String = user_agent.chars().take(400).collect();
        let addr: String = address.chars().take(100).collect();
        self.conn.execute("UPDATE devices SET last_seen_at = ?1, user_agent = ?2, last_address = ?3 WHERE id = ?4", params![now(), ua, addr, device_id])?;
        Ok(Principal::Device { device_id, user_id, rights, name })
    }

    /// Resolves an agent token. A token without a grant is admitted as a principal without
    /// role, so the caller learns its state; an expired or revoked one is refused.
    pub fn authenticate_agent(&self, token: &str) -> OpResult<Principal> {
        let row = self.conn.query_row(&format!("SELECT {AGENT_COLS} FROM agents WHERE token_hash = ?1"), [token::hash(token)], agent_row).optional()?;
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
            _ => Ok(Principal::Agent {
                agent_id: a.id,
                display: a.display,
                role: a.role.as_deref().map(Role::parse).transpose()?,
                reach: roles_of(&a.reach),
                granted_by: a.granted_by,
            }),
        }
    }

    fn new_invitation(&self, user_id: i64, own: bool, created_by: &str) -> OpResult<(i64, String, String)> {
        let tok = token::generate("pdi_");
        let lifetime = if own { Duration::minutes(OWN_INVITATION_MINUTES) } else { Duration::days(INVITATION_DAYS) };
        let expires = (Utc::now() + lifetime).to_rfc3339_opts(SecondsFormat::Secs, true);
        self.conn.execute(
            "INSERT INTO invitations(user_id, token_hash, own, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, token::hash(&tok), own, created_by, now(), expires],
        )?;
        Ok((self.conn.last_insert_rowid(), tok, expires))
    }

    /// Exchanges an invitation token for a device credential. Each redemption creates a
    /// device; a person's own invitation for a further device serves once.
    pub fn redeem_invitation(&self, invitation_token: &str, label: &str, kind: &str, user_agent: &str, address: &str) -> OpResult<(String, Value)> {
        if kind != "browser" && kind != "extension" {
            return Err(OpError::bad("kind_invalid", format!("`kind` = \"{kind}\" is not possible. Send \"browser\" for this browser or \"extension\" for the browser extension.")));
        }
        let label = text("label", label, 1, 80)?;
        let row = self
            .conn
            .query_row(
                "SELECT i.id, i.user_id, i.expires_at, i.withdrawn_at, i.own, i.redeemed_at, u.deactivated_at, u.name
                 FROM invitations i JOIN users u ON u.id = i.user_id WHERE i.token_hash = ?1",
                [token::hash(invitation_token)],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((inv_id, user_id, expires_at, withdrawn, own, redeemed, deactivated, name)) = row else {
            return Err(OpError::not_found("invitation_unknown", "This invitation link is not known. Check that the link was copied completely, or ask for a new one."));
        };
        if withdrawn.is_some() {
            return Err(OpError::gone("invitation_withdrawn", "This invitation was withdrawn. Ask a person who manages people for a new invitation."));
        }
        if own && redeemed.is_some() {
            return Err(OpError::gone("invitation_used", "This link for a further device of yours has been used already. Create a new one on a device that is signed in."));
        }
        if expires_at < now() {
            return Err(OpError::gone("invitation_expired", "This invitation has expired. Ask a person who manages people for a new invitation, or create a link for a further device on a device that is signed in."));
        }
        if deactivated.is_some() {
            return Err(OpError::gone("user_deactivated", "The person this invitation is for was deactivated. Ask a person who manages people."));
        }
        let cred = token::generate("pdd_");
        let ts = now();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO devices(user_id, invitation_id, kind, credential_hash, label, user_agent, last_address, created_at, last_seen_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![user_id, inv_id, kind, token::hash(&cred), label, user_agent.chars().take(400).collect::<String>(), address.chars().take(100).collect::<String>(), ts],
        )?;
        let device_id = tx.last_insert_rowid();
        tx.execute("UPDATE invitations SET redeemed_at = COALESCE(redeemed_at, ?1) WHERE id = ?2", params![ts, inv_id])?;
        tx.commit()?;
        Ok((cred, json!({ "user_id": user_id, "name": name, "device_id": device_id, "label": label, "kind": kind })))
    }

    pub fn whoami(&self, p: &Principal) -> OpResult<Value> {
        match p {
            Principal::Device { device_id, user_id, rights, name } => {
                let (label, kind): (String, String) = self.conn.query_row("SELECT label, kind FROM devices WHERE id = ?1", [device_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(json!({ "kind": "person", "user_id": user_id, "name": name, "rights": rights, "device_id": device_id, "device_label": label, "device_kind": kind }))
            }
            Principal::Agent { agent_id, display, role, .. } => Ok(json!({ "kind": "agent", "agent_id": agent_id, "display": display, "role": role.map(Role::name) })),
        }
    }

    // ---------------------------------------------------------------- people

    /// Setup only: creates a person with every right, and their first invitation.
    pub fn setup_create_admin(&self, name: &str, email: &str) -> OpResult<Value> {
        let name = text("name", name, 1, 120)?;
        let email = text("email", email, 0, 200)?;
        self.conn.execute(
            "INSERT INTO users(name, email, can_people, can_agents, can_backup, created_at, created_by) VALUES (?1, ?2, 1, 1, 1, ?3, 'setup')",
            params![name, email, now()],
        )?;
        let user_id = self.conn.last_insert_rowid();
        let (invitation_id, tok, expires) = self.new_invitation(user_id, false, "setup")?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    /// Acting on a person (a new invitation, deactivating, deleting, revoking a device) needs
    /// every right that person holds: otherwise it would hand over rights one does not hold.
    fn require_reach_over(&self, p: &Principal, user_id: i64) -> OpResult<()> {
        let (target, _) = self.user_rights(user_id)?;
        if !p.grantable_rights().covers(&target) {
            return Err(OpError::forbidden("This person holds rights you do not hold; only someone who holds them too may invite, deactivate or delete them, or revoke their devices."));
        }
        Ok(())
    }

    fn require_grantable(p: &Principal, wanted: &Rights) -> OpResult<()> {
        if !p.grantable_rights().covers(wanted) {
            return Err(OpError::forbidden("A right can be given or taken away only by someone who holds it. Ask a person who holds that right to do it."));
        }
        Ok(())
    }

    pub fn user_create(&self, p: &Principal, name: &str, email: &str, rights: Rights) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        Self::require_grantable(p, &rights)?;
        let name = text("name", name, 1, 120)?;
        let email = text("email", email, 0, 200)?;
        let by = p.by_name();
        self.conn.execute(
            "INSERT INTO users(name, email, can_people, can_agents, can_backup, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![name, email, rights.people, rights.agents, rights.backup, now(), by],
        )?;
        let user_id = self.conn.last_insert_rowid();
        let (invitation_id, tok, expires) = self.new_invitation(user_id, false, &by)?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    fn user_rights(&self, user_id: i64) -> OpResult<(Rights, bool)> {
        self.conn
            .query_row("SELECT can_people, can_agents, can_backup, deactivated_at IS NULL FROM users WHERE id = ?1", [user_id], |r| {
                Ok((Rights { people: r.get(0)?, agents: r.get(1)?, backup: r.get(2)? }, r.get(3)?))
            })
            .optional()?
            .ok_or_else(|| OpError::not_found("user_unknown", format!("There is no person with user_id {user_id}. Call user.list for the valid identifiers.")))
    }

    pub fn user_reinvite(&self, p: &Principal, user_id: i64) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        self.require_reach_over(p, user_id)?;
        let (invitation_id, tok, expires) = self.new_invitation(user_id, false, &p.by_name())?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    /// A person creates a link for a further device of their own, the browser extension for
    /// example; it serves once within the hour.
    pub fn invitation_create_own(&self, p: &Principal) -> OpResult<Value> {
        let (_, user_id, _) = p.require_person()?;
        let (invitation_id, tok, expires) = self.new_invitation(user_id, true, &p.by_name())?;
        Ok(json!({ "user_id": user_id, "invitation_id": invitation_id, "invitation_token": tok, "expires_at": expires }))
    }

    pub fn invitation_withdraw(&self, p: &Principal, invitation_id: i64) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        let n = self.conn.execute("UPDATE invitations SET withdrawn_at = COALESCE(withdrawn_at, ?1) WHERE id = ?2", params![now(), invitation_id])?;
        if n == 0 {
            return Err(OpError::not_found("invitation_unknown", format!("There is no invitation {invitation_id}. Call user.list for the invitations of each person.")));
        }
        Ok(json!({ "invitation_id": invitation_id, "withdrawn": true }))
    }

    /// Whether any active person other than this one holds every right; at least one must.
    fn another_full_admin(&self, user_id: i64) -> OpResult<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE can_people = 1 AND can_agents = 1 AND can_backup = 1 AND deactivated_at IS NULL AND id <> ?1)",
            [user_id],
            |r| r.get(0),
        )?)
    }

    fn keep_a_full_admin(&self, user_id: i64, what: &str) -> OpResult<()> {
        let (rights, active) = self.user_rights(user_id)?;
        if rights == Rights::ALL && active && !self.another_full_admin(user_id)? {
            return Err(OpError::conflict(
                "last_admin",
                format!("This is the last active person who holds all three rights; {what} would leave nobody to manage everything. Give all three rights to another person first."),
            ));
        }
        Ok(())
    }

    pub fn user_set_rights(&self, p: &Principal, user_id: i64, rights: Rights) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        let (cur, _) = self.user_rights(user_id)?;
        // What changes, in either direction, must be within the caller's own rights.
        let changed = Rights { people: cur.people != rights.people, agents: cur.agents != rights.agents, backup: cur.backup != rights.backup };
        Self::require_grantable(p, &changed)?;
        if rights != Rights::ALL {
            self.keep_a_full_admin(user_id, "taking a right away")?;
        }
        self.conn.execute(
            "UPDATE users SET can_people = ?1, can_agents = ?2, can_backup = ?3 WHERE id = ?4",
            params![rights.people, rights.agents, rights.backup, user_id],
        )?;
        Ok(json!({ "user_id": user_id, "rights": rights }))
    }

    pub fn user_set_active(&self, p: &Principal, user_id: i64, active: bool) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        self.require_reach_over(p, user_id)?;
        if !active {
            self.keep_a_full_admin(user_id, "deactivating them")?;
            self.conn.execute("UPDATE users SET deactivated_at = COALESCE(deactivated_at, ?1) WHERE id = ?2", params![now(), user_id])?;
        } else {
            self.conn.execute("UPDATE users SET deactivated_at = NULL WHERE id = ?1", [user_id])?;
        }
        Ok(json!({ "user_id": user_id, "active": active }))
    }

    /// Deletes a person with their invitations and devices; their closed runs keep the name.
    pub fn user_delete(&self, p: &Principal, user_id: i64) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        self.require_reach_over(p, user_id)?;
        if matches!(p, Principal::Device { user_id: me, .. } if *me == user_id) {
            return Err(OpError::conflict("user_self", "You cannot delete yourself. Ask another person who manages people."));
        }
        self.keep_a_full_admin(user_id, "deleting them")?;
        self.conn.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
        Ok(json!({ "user_id": user_id, "deleted": true }))
    }

    fn devices_where(&self, filter: &str, arg: Option<i64>) -> OpResult<Vec<Value>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT d.id, d.label, d.kind, d.invitation_id, d.user_agent, d.last_address, d.created_at, d.last_seen_at, d.revoked_at, d.user_id, u.name
             FROM devices d JOIN users u ON u.id = d.user_id WHERE {filter} ORDER BY d.last_seen_at DESC, d.id DESC"
        ))?;
        let map = |r: &rusqlite::Row| {
            Ok(json!({
                "device_id": r.get::<_, i64>(0)?, "label": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                "invitation_id": r.get::<_, Option<i64>>(3)?, "user_agent": r.get::<_, String>(4)?, "last_address": r.get::<_, String>(5)?,
                "created_at": r.get::<_, String>(6)?, "last_seen_at": r.get::<_, String>(7)?, "revoked_at": r.get::<_, Option<String>>(8)?,
                "user_id": r.get::<_, i64>(9)?, "user_name": r.get::<_, String>(10)?,
            }))
        };
        let rows = match arg {
            Some(a) => st.query_map([a], map)?.collect::<Result<_, _>>()?,
            None => st.query_map([], map)?.collect::<Result<_, _>>()?,
        };
        Ok(rows)
    }

    pub fn user_list(&self, p: &Principal) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        let mut st = self.conn.prepare("SELECT id, name, email, can_people, can_agents, can_backup, deactivated_at, created_at, created_by FROM users ORDER BY name COLLATE NOCASE")?;
        let users: Vec<(i64, String, String, Rights, Option<String>, String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, Rights { people: r.get(3)?, agents: r.get(4)?, backup: r.get(5)? }, r.get(6)?, r.get(7)?, r.get(8)?)))?
            .collect::<Result<_, _>>()?;
        let ts = now();
        let mut out = Vec::new();
        for (id, name, email, rights, deactivated_at, created_at, created_by) in users {
            let mut st = self.conn.prepare("SELECT id, created_at, expires_at, withdrawn_at, own, redeemed_at, created_by FROM invitations WHERE user_id = ?1 ORDER BY id")?;
            let invitations: Vec<Value> = st
                .query_map([id], |r| {
                    let exp: String = r.get(2)?;
                    let wd: Option<String> = r.get(3)?;
                    let own: bool = r.get(4)?;
                    let used: Option<String> = r.get(5)?;
                    let state = if wd.is_some() {
                        "withdrawn"
                    } else if own && used.is_some() {
                        "used"
                    } else if exp < ts {
                        "expired"
                    } else {
                        "open"
                    };
                    Ok(json!({ "invitation_id": r.get::<_, i64>(0)?, "created_at": r.get::<_, String>(1)?, "expires_at": exp, "own": own, "state": state, "created_by": r.get::<_, String>(6)? }))
                })?
                .collect::<Result<_, _>>()?;
            out.push(json!({
                "user_id": id, "name": name, "email": email, "rights": rights,
                "active": deactivated_at.is_none(), "created_at": created_at, "created_by": created_by,
                "devices": self.devices_where("d.user_id = ?1", Some(id))?, "invitations": invitations,
            }));
        }
        Ok(json!({ "users": out }))
    }

    pub fn device_list_own(&self, p: &Principal) -> OpResult<Value> {
        let (device_id, user_id, _) = p.require_person()?;
        Ok(json!({ "current_device_id": device_id, "devices": self.devices_where("d.user_id = ?1", Some(user_id))? }))
    }

    /// Every device of every person, most recently seen first.
    pub fn device_list_all(&self, p: &Principal) -> OpResult<Value> {
        p.require(Cap::ManagePeople)?;
        Ok(json!({ "devices": self.devices_where("1 = 1", None)? }))
    }

    /// Anyone may revoke their own device; revoking another person's device needs the right
    /// to manage people.
    pub fn device_revoke(&self, p: &Principal, device_id: i64) -> OpResult<Value> {
        let me = match p {
            Principal::Device { user_id, .. } => Some(*user_id),
            Principal::Agent { .. } => {
                p.require(Cap::ManagePeople)?;
                None
            }
        };
        let all = p.can(Cap::ManagePeople);
        // Without that right, another person's device is answered like a missing one.
        let found: bool = self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE id = ?1 AND (user_id = ?2 OR ?3))", params![device_id, me, all], |r| r.get(0))?;
        if !found {
            return Err(OpError::not_found(
                "device_unknown",
                format!("There is no device {device_id} you may revoke. Call device.list for your own devices; another person's device can be revoked by a person who manages people."),
            ));
        }
        let owner: i64 = self.conn.query_row("SELECT user_id FROM devices WHERE id = ?1", [device_id], |r| r.get(0))?;
        if Some(owner) != me {
            self.require_reach_over(p, owner)?;
        }
        self.conn.execute("UPDATE devices SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), device_id])?;
        Ok(json!({ "device_id": device_id, "revoked": true }))
    }

    // ---------------------------------------------------------------- agents

    /// Records that an agent token was used for a call it was admitted to; refused calls do
    /// not keep a token alive.
    pub fn agent_touch(&self, p: &Principal) -> OpResult<()> {
        if let Principal::Agent { agent_id, .. } = p {
            self.conn.execute("UPDATE agents SET last_used_at = ?1 WHERE id = ?2", params![now(), agent_id])?;
        }
        Ok(())
    }

    /// Open self-registration: a token without any role, its display string, and the
    /// self-introduction kept as the basis for the grant. The clear text is returned once.
    pub fn agent_register(&self, introduction: &str) -> OpResult<Value> {
        let introduction = text("introduction", introduction, 10, 500)?;
        let cutoff = ago(Duration::hours(AGENT_PENDING_HOURS));
        // Registrations nobody granted in time are of no use to anyone.
        self.conn.execute("DELETE FROM agents WHERE role IS NULL AND created_at < ?1", [&cutoff])?;
        let waiting: i64 = self.conn.query_row("SELECT COUNT(*) FROM agents WHERE role IS NULL AND revoked_at IS NULL", [], |r| r.get(0))?;
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
        self.conn.execute("INSERT INTO agents(token_hash, display, introduction, created_at) VALUES (?1, ?2, ?3, ?4)", params![token::hash(&tok), display, introduction, ts])?;
        Ok(json!({
            "agent_id": self.conn.last_insert_rowid(), "display": display, "token": tok,
            "state": "pending", "role": null, "expires_at": plus(&ts, Duration::hours(AGENT_PENDING_HOURS)),
        }))
    }

    /// The role checked against what the caller may grant, and the reach it passes on.
    fn grant_of(p: &Principal, role: &str) -> OpResult<(Role, String)> {
        let role = Role::parse(role)?;
        let grantable = p.grantable_roles();
        if !grantable.contains(&role) {
            return Err(OpError::forbidden(format!(
                "You may grant only the roles {}; {} needs a right you do not hold. Ask a person who holds it.",
                roles_text(&grantable).replace(',', ", "),
                role.name()
            )));
        }
        let reach = if role == Role::AgentsManage { roles_text(&grantable) } else { String::new() };
        Ok((role, reach))
    }

    /// A person or agent who manages agents creates a token with its role at once, for a
    /// script that cannot register itself. The clear text is returned once.
    pub fn agent_create(&self, p: &Principal, introduction: &str, role: &str) -> OpResult<Value> {
        p.require(Cap::ManageAgents)?;
        let (role, reach) = Self::grant_of(p, role)?;
        let introduction = text("introduction", introduction, 10, 500)?;
        let tok = token::generate("pda_");
        let ts = now();
        self.conn.execute(
            "INSERT INTO agents(token_hash, display, introduction, role, reach, granted_by, granted_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![token::hash(&tok), token::display_string(&tok), introduction, role.name(), reach, p.by_name(), ts],
        )?;
        let mut v = self.agent_get(self.conn.last_insert_rowid())?.to_json();
        v["token"] = json!(tok);
        Ok(v)
    }

    pub fn agent_list(&self, p: &Principal) -> OpResult<Value> {
        p.require(Cap::ManageAgents)?;
        let mut st = self.conn.prepare(&format!("SELECT {AGENT_COLS} FROM agents ORDER BY id DESC"))?;
        let agents: Vec<AgentRow> = st.query_map([], agent_row)?.collect::<Result<_, _>>()?;
        let grantable = p.grantable_roles();
        Ok(json!({
            "roles": ROLES.iter().map(|(r, n, d)| json!({ "role": n, "permits": d, "grantable": grantable.contains(r) })).collect::<Vec<_>>(),
            "agents": agents.iter().map(AgentRow::to_json).collect::<Vec<_>>(),
        }))
    }

    #[cfg(test)]
    pub fn agent_list_unchecked(&self) -> Vec<Value> {
        let mut st = self.conn.prepare(&format!("SELECT {AGENT_COLS} FROM agents ORDER BY id")).unwrap();
        let rows: Vec<AgentRow> = st.query_map([], agent_row).unwrap().collect::<Result<_, _>>().unwrap();
        rows.iter().map(AgentRow::to_json).collect()
    }

    fn agent_get(&self, agent_id: i64) -> OpResult<AgentRow> {
        self.conn
            .query_row(&format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1"), [agent_id], agent_row)
            .optional()?
            .ok_or_else(|| OpError::not_found("agent_unknown", format!("There is no agent token {agent_id}. Call agent.list for the valid identifiers.")))
    }

    /// Grants exactly one role to a waiting token; a token's role never changes afterwards.
    pub fn agent_grant(&self, p: &Principal, agent_id: i64, role: &str) -> OpResult<Value> {
        p.require(Cap::ManageAgents)?;
        let (role, reach) = Self::grant_of(p, role)?;
        let a = self.agent_get(agent_id)?;
        match a.state() {
            "pending" => {}
            "granted" => {
                return Err(OpError::conflict(
                    "agent_granted",
                    format!("Agent token {} already carries {}. For another role the agent registers a second token.", a.display, a.role.unwrap_or_default()),
                ))
            }
            state => return Err(OpError::gone("agent_not_waiting", format!("Agent token {} is {state} and cannot be granted. The agent registers a new one.", a.display))),
        }
        if matches!(p, Principal::Agent { agent_id: me, .. } if *me == agent_id) {
            return Err(OpError::conflict("agent_self", "A token cannot grant itself."));
        }
        self.conn.execute(
            "UPDATE agents SET role = ?1, reach = ?2, granted_by = ?3, granted_at = ?4 WHERE id = ?5",
            params![role.name(), reach, p.by_name(), now(), agent_id],
        )?;
        Ok(self.agent_get(agent_id)?.to_json())
    }

    /// Revoking needs no window and hits exactly one token; it also rejects a waiting one.
    pub fn agent_revoke(&self, p: &Principal, agent_id: i64) -> OpResult<Value> {
        p.require(Cap::ManageAgents)?;
        self.agent_get(agent_id)?;
        self.conn.execute("UPDATE agents SET revoked_at = COALESCE(revoked_at, ?1) WHERE id = ?2", params![now(), agent_id])?;
        Ok(json!({ "agent_id": agent_id, "revoked": true }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn admin(store: &Store) -> Principal {
        admin_named(store, "Ada")
    }

    pub fn admin_named(store: &Store, name: &str) -> Principal {
        let v = store.setup_create_admin(name, "").unwrap();
        let (cred, _) = store.redeem_invitation(v["invitation_token"].as_str().unwrap(), "Laptop", "browser", "", "").unwrap();
        store.authenticate_device(&cred, "browser", "", "").unwrap()
    }

    pub fn person(store: &Store, by: &Principal, name: &str, rights: Rights) -> Principal {
        let v = store.user_create(by, name, "", rights).unwrap();
        let (cred, _) = store.redeem_invitation(v["invitation_token"].as_str().unwrap(), "Laptop", "browser", "", "").unwrap();
        store.authenticate_device(&cred, "browser", "", "").unwrap()
    }

    pub fn agent(s: &Store, by: &Principal, role: Option<&str>) -> Principal {
        let v = s.agent_register("Agent of Ada; purpose: testing").unwrap();
        if let Some(r) = role {
            s.agent_grant(by, v["agent_id"].as_i64().unwrap(), r).unwrap();
        }
        s.authenticate_agent(v["token"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn dates_are_stored_only_in_canonical_form() {
        assert!(parse_date("d", "2026-11-06").is_ok());
        for bad in ["2026-1-6", "26-11-06", "2026-11-31", "12026-11-06", " 2026-11-06"] {
            assert!(parse_date("d", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn guids_have_the_uuid_form_and_differ() {
        let a = new_guid();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert_ne!(a, new_guid());
    }

    #[test]
    fn a_store_of_version_0_7_is_refused_with_the_way_forward() {
        let d = tempfile::tempdir().unwrap();
        let c = Connection::open(d.path().join(STORE_FILE)).unwrap();
        c.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY); PRAGMA user_version = 2;").unwrap();
        drop(c);
        let e = Store::open(d.path()).err().unwrap();
        assert!(e.message.contains("0.7") && e.message.contains("move the file away"), "{}", e.message);
    }

    #[test]
    fn a_store_with_the_same_version_but_another_structure_is_refused() {
        let d = tempfile::tempdir().unwrap();
        drop(Store::open(d.path()).unwrap());
        let c = Connection::open(d.path().join(STORE_FILE)).unwrap();
        c.execute_batch("ALTER TABLE jobs DROP COLUMN password_at;").unwrap();
        drop(c);
        let e = Store::open(d.path()).err().unwrap();
        assert!(e.message.contains("jobs lacks password_at"), "{}", e.message);
    }

    #[test]
    fn another_persons_device_looks_like_a_missing_one() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let b = person(&s, &a, "Bob", Rights::default());
        let Principal::Device { device_id: a_dev, .. } = a else { panic!() };
        assert_eq!(s.device_revoke(&b, a_dev).err().unwrap().kind, "device_unknown");
        assert_eq!(s.device_revoke(&b, 99999).err().unwrap().kind, "device_unknown");
        assert!(s.device_revoke(&a, a_dev).is_ok());
    }

    #[test]
    fn one_invitation_serves_several_devices_each_with_its_own_credential() {
        let s = Store::open_in_memory().unwrap();
        let v = s.setup_create_admin("Ada", "").unwrap();
        let tok = v["invitation_token"].as_str().unwrap();
        let (c1, _) = s.redeem_invitation(tok, "Laptop", "browser", "", "").unwrap();
        let (c2, _) = s.redeem_invitation(tok, "Phone", "browser", "", "").unwrap();
        assert_ne!(c1, c2);
        let p1 = s.authenticate_device(&c1, "browser", "", "").unwrap();
        s.device_revoke(&p1, match p1 { Principal::Device { device_id, .. } => device_id, _ => 0 }).unwrap();
        assert!(s.authenticate_device(&c1, "browser", "", "").is_err());
        assert!(s.authenticate_device(&c2, "browser", "", "").is_ok());
    }

    #[test]
    fn an_extension_credential_counts_only_as_a_bearer_token_and_a_browsers_only_as_a_cookie() {
        let s = Store::open_in_memory().unwrap();
        let v = s.setup_create_admin("Ada", "").unwrap();
        let tok = v["invitation_token"].as_str().unwrap();
        let (ext, _) = s.redeem_invitation(tok, "Firefox extension", "extension", "", "").unwrap();
        let (web, _) = s.redeem_invitation(tok, "Laptop", "browser", "", "").unwrap();
        assert!(s.authenticate_device(&ext, "extension", "", "").is_ok());
        assert_eq!(s.authenticate_device(&ext, "browser", "", "").err().unwrap().kind, "credential_unknown");
        assert_eq!(s.authenticate_device(&web, "extension", "", "").err().unwrap().kind, "credential_unknown");
        assert_eq!(s.redeem_invitation(tok, "x", "tablet", "", "").err().unwrap().kind, "kind_invalid");
    }

    #[test]
    fn an_own_invitation_serves_once() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let b = person(&s, &a, "Bob", Rights::default());
        let v = s.invitation_create_own(&b).unwrap();
        let tok = v["invitation_token"].as_str().unwrap();
        let (cred, who) = s.redeem_invitation(tok, "Chrome extension", "extension", "", "").unwrap();
        assert_eq!(who["name"], "Bob");
        assert!(s.authenticate_device(&cred, "extension", "", "").is_ok());
        assert_eq!(s.redeem_invitation(tok, "again", "extension", "", "").err().unwrap().kind, "invitation_used");
        let agent = agent(&s, &a, Some("people.manage"));
        assert_eq!(s.invitation_create_own(&agent).err().unwrap().kind, "right_missing");
    }

    #[test]
    fn withdrawn_invitation_cannot_be_redeemed_but_devices_stay() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let v = s.user_create(&a, "Bob", "", Rights::default()).unwrap();
        let tok = v["invitation_token"].as_str().unwrap();
        let (cred, _) = s.redeem_invitation(tok, "Laptop", "browser", "", "").unwrap();
        s.invitation_withdraw(&a, v["invitation_id"].as_i64().unwrap()).unwrap();
        assert_eq!(s.redeem_invitation(tok, "Phone", "browser", "", "").err().unwrap().kind, "invitation_withdrawn");
        assert!(s.authenticate_device(&cred, "browser", "", "").is_ok());
    }

    #[test]
    fn one_person_with_all_three_rights_always_remains() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let Principal::Device { user_id: me, .. } = a else { panic!() };
        assert_eq!(s.user_set_rights(&a, me, Rights { backup: false, ..Rights::ALL }).err().unwrap().kind, "last_admin");
        assert_eq!(s.user_set_active(&a, me, false).err().unwrap().kind, "last_admin");
        let b = person(&s, &a, "Bob", Rights::ALL);
        let Principal::Device { user_id: bob, .. } = b else { panic!() };
        assert!(s.user_set_rights(&a, me, Rights::default()).is_ok());
        assert_eq!(s.user_delete(&b, bob).err().unwrap().kind, "user_self");
        assert_eq!(s.user_set_active(&b, bob, false).err().unwrap().kind, "last_admin");
    }

    #[test]
    fn a_right_is_given_and_taken_only_by_someone_who_holds_it() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let people_only = person(&s, &a, "Pia", Rights { people: true, ..Rights::default() });
        assert_eq!(s.user_create(&people_only, "Bob", "", Rights { backup: true, ..Rights::default() }).err().unwrap().kind, "right_missing");
        let v = s.user_create(&people_only, "Bob", "", Rights { people: true, ..Rights::default() }).unwrap();
        let bob = v["user_id"].as_i64().unwrap();
        assert!(s.user_set_rights(&a, bob, Rights { people: true, backup: true, ..Rights::default() }).is_ok());
        // Pia may not take away the backup right she does not hold herself.
        assert_eq!(s.user_set_rights(&people_only, bob, Rights { people: true, ..Rights::default() }).err().unwrap().kind, "right_missing");
        // A person without the right to manage people manages nobody.
        let plain = person(&s, &a, "Carl", Rights::default());
        assert_eq!(s.user_list(&plain).err().unwrap().kind, "right_missing");
        assert_eq!(s.device_list_all(&plain).err().unwrap().kind, "right_missing");
    }

    #[test]
    fn nobody_acts_on_a_person_who_holds_more_rights_than_they_do() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let Principal::Device { user_id: ada, device_id: ada_dev, .. } = a else { panic!() };
        let pia = person(&s, &a, "Pia", Rights { people: true, ..Rights::default() });
        let pm = agent(&s, &a, Some("people.manage"));
        for p in [&pia, &pm] {
            assert_eq!(s.user_reinvite(p, ada).err().unwrap().kind, "right_missing", "a new link for a full administrator would hand over all rights");
            assert_eq!(s.user_set_active(p, ada, false).err().unwrap().kind, "right_missing");
            assert_eq!(s.user_delete(p, ada).err().unwrap().kind, "right_missing");
            assert_eq!(s.device_revoke(p, ada_dev).err().unwrap().kind, "right_missing");
        }
        // A person with the same rights or fewer is within reach.
        let v = s.user_create(&pia, "Bob", "", Rights::default()).unwrap();
        let bob = v["user_id"].as_i64().unwrap();
        assert!(s.user_reinvite(&pia, bob).is_ok());
        assert!(s.user_reinvite(&pm, bob).is_ok());
        assert!(s.user_set_active(&pia, bob, false).is_ok());
        // An agent without people.manage revokes no device at all.
        let reader = agent(&s, &a, Some("jobs.read"));
        assert_eq!(s.device_revoke(&reader, ada_dev).err().unwrap().kind, "right_missing");
    }

    #[test]
    fn an_agent_is_named_with_the_person_who_granted_it() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let g = agent(&s, &a, Some("jobs.all"));
        assert!(g.by_name().ends_with(" (Ada)"), "{}", g.by_name());
    }

    #[test]
    fn every_device_is_listed_for_those_who_manage_people() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        person(&s, &a, "Bob", Rights::default());
        let all = s.device_list_all(&a).unwrap();
        let names: Vec<_> = all["devices"].as_array().unwrap().iter().map(|d| d["user_name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Ada".to_string()) && names.contains(&"Bob".to_string()));
        let pm = agent(&s, &a, Some("people.manage"));
        assert_eq!(s.device_list_all(&pm).unwrap()["devices"].as_array().unwrap().len(), 2);
        let bob_dev = all["devices"].as_array().unwrap().iter().find(|d| d["user_name"] == "Bob").unwrap()["device_id"].as_i64().unwrap();
        assert!(s.device_revoke(&pm, bob_dev).is_ok(), "an agent that manages people revokes devices within its reach");
    }

    #[test]
    fn deactivated_user_is_locked_out_on_every_device() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let v = s.user_create(&a, "Bob", "", Rights::default()).unwrap();
        let (cred, _) = s.redeem_invitation(v["invitation_token"].as_str().unwrap(), "Laptop", "browser", "", "").unwrap();
        s.user_set_active(&a, v["user_id"].as_i64().unwrap(), false).unwrap();
        assert_eq!(s.authenticate_device(&cred, "browser", "", "").err().unwrap().kind, "user_deactivated");
    }

    #[test]
    fn a_registered_agent_can_do_nothing_until_granted() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let g = agent(&s, &a, None);
        assert_eq!(g.require(Cap::ReadJobs).err().unwrap().kind, "agent_pending");
    }

    #[test]
    fn roles_are_granted_only_within_the_granters_reach() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        // A person who manages agents but holds no backup right cannot hand out the backup.
        let agents_only = person(&s, &a, "Ana", Rights { agents: true, ..Rights::default() });
        let v = s.agent_register("Agent of Ana; purpose: backup").unwrap();
        let id = v["agent_id"].as_i64().unwrap();
        assert_eq!(s.agent_grant(&agents_only, id, "backup.read").err().unwrap().kind, "right_missing");
        assert!(s.agent_grant(&agents_only, id, "jobs.all").is_ok());
        // An agent that manages agents grants what the person who granted it could.
        let manager = agent(&s, &agents_only, Some("agents.manage"));
        let w = s.agent_register("Agent of Ana; purpose: downloads").unwrap();
        assert_eq!(s.agent_grant(&manager, w["agent_id"].as_i64().unwrap(), "backup.read").err().unwrap().kind, "right_missing");
        let granted = s.agent_grant(&manager, w["agent_id"].as_i64().unwrap(), "jobs.work").unwrap();
        assert!(granted["granted_by"].as_str().unwrap().starts_with(&format!("Agent {} (", match &manager { Principal::Agent { display, .. } => display.clone(), _ => String::new() })));
        // The full administrator's manager may hand out the backup.
        let full = agent(&s, &a, Some("agents.manage"));
        let x = s.agent_register("Agent of Ada; purpose: backup").unwrap();
        assert!(s.agent_grant(&full, x["agent_id"].as_i64().unwrap(), "backup.read").is_ok());
        // A token without agents.manage grants nothing.
        let worker = agent(&s, &a, Some("jobs.all"));
        assert_eq!(s.agent_list(&worker).err().unwrap().kind, "right_missing");
    }

    #[test]
    fn an_agent_that_manages_people_invites_without_further_rights() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let pm = agent(&s, &a, Some("people.manage"));
        let v = s.user_create(&pm, "Erna", "erna@example.org", Rights::default()).unwrap();
        assert!(v["invitation_token"].as_str().unwrap().starts_with("pdi_"));
        assert_eq!(s.user_create(&pm, "Fritz", "", Rights { agents: true, ..Rights::default() }).err().unwrap().kind, "right_missing");
        let list = s.user_list(&a).unwrap();
        let erna = list["users"].as_array().unwrap().iter().find(|u| u["name"] == "Erna").unwrap().clone();
        assert!(erna["created_by"].as_str().unwrap().starts_with("Agent "));
    }

    #[test]
    fn agent_tokens_expire_and_can_be_revoked() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let v = s.agent_register("Agent of Ada; purpose: testing").unwrap();
        let id = v["agent_id"].as_i64().unwrap();
        s.agent_grant(&a, id, "jobs.read").unwrap();
        s.agent_revoke(&a, id).unwrap();
        assert_eq!(s.authenticate_agent(v["token"].as_str().unwrap()).err().unwrap().kind, "agent_token_revoked");
        let w = s.agent_register("Agent of Ada; purpose: testing").unwrap();
        s.conn.execute("UPDATE agents SET created_at = '2000-01-01T00:00:00Z' WHERE id = ?1", [w["agent_id"].as_i64().unwrap()]).unwrap();
        assert_eq!(s.authenticate_agent(w["token"].as_str().unwrap()).err().unwrap().kind, "agent_token_expired");
    }

    #[test]
    fn open_registration_is_throttled() {
        let s = Store::open_in_memory().unwrap();
        for _ in 0..AGENT_PENDING_MAX {
            s.agent_register("Agent of Ada; purpose: testing").unwrap();
        }
        assert_eq!(s.agent_register("Agent of Ada; purpose: testing").err().unwrap().kind, "registrations_full");
    }

    #[test]
    fn markdown_is_sanitized() {
        let h = render_markdown("# T\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1))");
        assert!(h.contains("<h1>T</h1>"), "{h}");
        assert!(!h.contains("<script") && !h.contains("javascript:"), "{h}");
    }
}
