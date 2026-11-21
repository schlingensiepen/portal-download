//! The ZIP file that carries jobs between instances and holds the backup.
//!
//! One format for both: a ZIP with the single entry `portal-download.jsonl`, encrypted with
//! AES-256 when a password is given. Its first line is the head; every further line is one
//! record. `kind: "jobs"` holds what every person sees: jobs with their logins, their runs
//! and the GUIDs of deleted jobs. `kind: "full"` adds people, invitations, devices and agent
//! tokens (their hashes, never a clear credential).
//!
//! Reading in is a merge in two steps, both stateless: a preview sorts every job of the file
//! into new, same or decide, and the import applies the preview with the caller's decisions.

use super::jobs::{check_url, stored_row, validate_job, JobInput, Stored, STORED_COLS};
use super::{now, text, Cap, Principal, Rights, Store};
use crate::error::{OpError, OpResult};
use chrono::NaiveDate;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};

pub const FORMAT: &str = "portal-download";
pub const VERSION: i64 = 3;
const ENTRY: &str = "portal-download.jsonl";
/// The largest file read in: what the API accepts from an administrator.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Jobs only, whatever else the file holds.
    Jobs,
    /// Jobs, and people, invitations, devices and agent tokens added beside the existing ones.
    Full,
}

/// The fields of a job that are merged one by one, with their column names.
const FIELDS: &[&str] = &["title", "interval_weeks", "retry_days", "next_due", "username", "password", "instructions_md"];

// -------------------------------------------------------------------- writing

fn job_record(j: &Stored) -> Value {
    json!({
        "type": "job", "guid": j.guid, "replaces": j.replaces, "title": j.title, "entry_url": j.entry_url,
        "interval_weeks": j.interval_weeks, "retry_days": j.retry_days, "next_due": j.next_due,
        "username": j.username, "password": j.password, "instructions_md": j.instructions_md, "created_at": j.created_at,
        "changed": {
            "title": j.title_at, "interval_weeks": j.interval_weeks_at, "retry_days": j.retry_days_at, "next_due": j.next_due_at,
            "username": j.username_at, "password": j.password_at, "instructions_md": j.instructions_md_at,
        },
    })
}

fn zip_text(text: &str, password: Option<&str>) -> OpResult<Vec<u8>> {
    let failed = |e: &dyn std::fmt::Display| OpError::internal(format!("The file could not be packed: {e}."));
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let opts = match password {
        Some(pw) => opts.with_aes_encryption(zip::AesMode::Aes256, pw),
        None => opts,
    };
    w.start_file(ENTRY, opts).map_err(|e| failed(&e))?;
    w.write_all(text.as_bytes()).map_err(|e| failed(&e))?;
    Ok(w.finish().map_err(|e| failed(&e))?.into_inner())
}

/// A password travels in a header, so it is visible ASCII: letters, digits, punctuation and spaces.
pub fn check_password(password: Option<&str>) -> OpResult<()> {
    if let Some(pw) = password {
        if pw.len() < 8 || pw.len() > 200 || !pw.chars().all(|c| (' '..='~').contains(&c)) || pw.trim() != pw {
            return Err(OpError::bad(
                "password_invalid",
                "The password for the file must have between 8 and 200 characters, only letters A to Z, digits, punctuation and spaces (no umlauts, no space at either end). Choose another and send again.",
            ));
        }
    }
    Ok(())
}

impl Store {
    fn records_of_jobs(&self, out: &mut Vec<Value>) -> OpResult<()> {
        let mut st = self.conn.prepare(&format!("SELECT {STORED_COLS} FROM jobs ORDER BY id"))?;
        let jobs: Vec<Stored> = st.query_map([], stored_row)?.collect::<Result<_, _>>()?;
        for j in &jobs {
            out.push(job_record(j));
        }
        let mut st = self.conn.prepare(
            "SELECT r.guid, j.guid, r.by_name, r.outcome, r.note, r.finished_at, r.due_before, r.due_after FROM runs r JOIN jobs j ON j.id = r.job_id ORDER BY r.id",
        )?;
        let runs = st.query_map([], |r| {
            Ok(json!({
                "type": "run", "guid": r.get::<_, String>(0)?, "job_guid": r.get::<_, String>(1)?, "by": r.get::<_, String>(2)?,
                "outcome": r.get::<_, String>(3)?, "note": r.get::<_, String>(4)?, "finished_at": r.get::<_, String>(5)?,
                "due_before": r.get::<_, String>(6)?, "due_after": r.get::<_, String>(7)?,
            }))
        })?;
        for r in runs {
            out.push(r?);
        }
        let mut st = self.conn.prepare("SELECT guid, title, entry_url, deleted_at, replaced_by FROM deleted_jobs ORDER BY guid")?;
        let del = st.query_map([], |r| {
            Ok(json!({
                "type": "deleted_job", "guid": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "entry_url": r.get::<_, String>(2)?,
                "deleted_at": r.get::<_, String>(3)?, "replaced_by": r.get::<_, Option<String>>(4)?,
            }))
        })?;
        for d in del {
            out.push(d?);
        }
        Ok(())
    }

    fn records_of_access(&self, out: &mut Vec<Value>) -> OpResult<()> {
        let rows = |sql: &str, f: &dyn Fn(&rusqlite::Row) -> rusqlite::Result<Value>| -> OpResult<Vec<Value>> {
            let mut st = self.conn.prepare(sql)?;
            let v = st.query_map([], |r| f(r))?.collect::<Result<_, _>>()?;
            Ok(v)
        };
        out.extend(rows("SELECT id, name, email, can_people, can_agents, can_backup, deactivated_at, created_at, created_by FROM users ORDER BY id", &|r| {
            Ok(json!({
                "type": "user", "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "email": r.get::<_, String>(2)?,
                "rights": { "people": r.get::<_, bool>(3)?, "agents": r.get::<_, bool>(4)?, "backup": r.get::<_, bool>(5)? },
                "deactivated_at": r.get::<_, Option<String>>(6)?, "created_at": r.get::<_, String>(7)?, "created_by": r.get::<_, String>(8)?,
            }))
        })?);
        out.extend(rows("SELECT id, user_id, token_hash, own, created_by, created_at, expires_at, withdrawn_at, redeemed_at FROM invitations ORDER BY id", &|r| {
            Ok(json!({
                "type": "invitation", "id": r.get::<_, i64>(0)?, "user_id": r.get::<_, i64>(1)?, "token_hash": r.get::<_, String>(2)?,
                "own": r.get::<_, bool>(3)?, "created_by": r.get::<_, String>(4)?, "created_at": r.get::<_, String>(5)?,
                "expires_at": r.get::<_, String>(6)?, "withdrawn_at": r.get::<_, Option<String>>(7)?, "redeemed_at": r.get::<_, Option<String>>(8)?,
            }))
        })?);
        out.extend(rows(
            "SELECT user_id, invitation_id, kind, credential_hash, label, user_agent, last_address, created_at, last_seen_at, revoked_at FROM devices ORDER BY id",
            &|r| {
                Ok(json!({
                    "type": "device", "user_id": r.get::<_, i64>(0)?, "invitation_id": r.get::<_, Option<i64>>(1)?, "kind": r.get::<_, String>(2)?,
                    "credential_hash": r.get::<_, String>(3)?, "label": r.get::<_, String>(4)?, "user_agent": r.get::<_, String>(5)?,
                    "last_address": r.get::<_, String>(6)?, "created_at": r.get::<_, String>(7)?, "last_seen_at": r.get::<_, String>(8)?,
                    "revoked_at": r.get::<_, Option<String>>(9)?,
                }))
            },
        )?);
        out.extend(rows(
            "SELECT token_hash, display, introduction, role, reach, granted_by, granted_at, created_at, last_used_at, revoked_at FROM agents ORDER BY id",
            &|r| {
                Ok(json!({
                    "type": "agent", "token_hash": r.get::<_, String>(0)?, "display": r.get::<_, String>(1)?, "introduction": r.get::<_, String>(2)?,
                    "role": r.get::<_, Option<String>>(3)?, "reach": r.get::<_, String>(4)?, "granted_by": r.get::<_, Option<String>>(5)?,
                    "granted_at": r.get::<_, Option<String>>(6)?, "created_at": r.get::<_, String>(7)?, "last_used_at": r.get::<_, Option<String>>(8)?,
                    "revoked_at": r.get::<_, Option<String>>(9)?,
                }))
            },
        )?);
        Ok(())
    }

    /// The text inside the file, for the jobs alone or for the full backup.
    fn exchange_text(&self, full: bool) -> OpResult<String> {
        let mut out = vec![json!({
            "format": FORMAT, "version": VERSION, "kind": if full { "full" } else { "jobs" },
            "created_at": now(), "service": env!("CARGO_PKG_VERSION"),
        })];
        self.records_of_jobs(&mut out)?;
        if full {
            self.records_of_access(&mut out)?;
        }
        let mut s = String::new();
        for v in out {
            s.push_str(&v.to_string());
            s.push('\n');
        }
        Ok(s)
    }

    /// The jobs as a file: every person, and agents that see the logins.
    pub fn job_export(&self, p: &Principal, password: Option<&str>) -> OpResult<Vec<u8>> {
        p.require(Cap::ExportJobs)?;
        check_password(password)?;
        zip_text(&self.exchange_text(false)?, password)
    }

    /// The whole backup as a file: a person who manages the backup, or a token for backup.read.
    pub fn backup_export(&self, p: &Principal, password: Option<&str>) -> OpResult<Vec<u8>> {
        p.require(Cap::BackupRead)?;
        self.backup_bytes(password)
    }

    /// The whole backup, for the command on the server, which needs no principal.
    pub fn backup_bytes(&self, password: Option<&str>) -> OpResult<Vec<u8>> {
        check_password(password)?;
        zip_text(&self.exchange_text(true)?, password)
    }

    /// Writes the backup to a new file, readable by its owner only; never overwrites.
    pub fn backup_to_file(&self, target: &std::path::Path, password: Option<&str>) -> OpResult<Value> {
        if target.exists() {
            return Err(OpError::conflict("backup_exists", format!("{} exists already. Name a new file, for example with the date in its name; a backup never overwrites.", target.display())));
        }
        let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
        if !dir.is_dir() {
            return Err(OpError::bad("backup_dir_missing", format!("The directory {} does not exist. Create it, or name a file in an existing directory.", dir.display())));
        }
        let bytes = self.backup_bytes(password)?;
        let failed = |e: &dyn std::fmt::Display| OpError::internal(format!("The backup to {} failed: {e}. Check that the directory is writable and has room.", target.display()));
        let mut o = std::fs::OpenOptions::new();
        // create_new never replaces a file, also one created meanwhile, on every file system.
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                OpError::conflict("backup_exists", format!("{} exists already. Name a new file; a backup never overwrites.", target.display()))
            } else {
                failed(&e)
            }
        })?;
        if let Err(e) = f.write_all(&bytes).and_then(|_| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(target);
            return Err(failed(&e));
        }
        Ok(json!({ "backup": target.display().to_string(), "bytes": bytes.len(), "encrypted": password.is_some() }))
    }
}

// -------------------------------------------------------------------- reading

/// The records of a file, checked line by line like API input.
struct File {
    kind: String,
    jobs: Vec<Value>,
    runs: Vec<Value>,
    deleted: Vec<Value>,
    users: Vec<Value>,
    invitations: Vec<Value>,
    devices: Vec<Value>,
    agents: Vec<Value>,
}

fn unzip(bytes: &[u8], password: Option<&str>) -> OpResult<String> {
    let not_ours = || OpError::bad("file_invalid", "This is not a file of portal-download: it is no ZIP with the entry portal-download.jsonl. Choose a file this service or the browser extension wrote.");
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| not_ours())?;
    // Exactly the one entry; anything beside it is no file of ours.
    if a.len() != 1 {
        return Err(not_ours());
    }
    let index = a.index_for_name(ENTRY).ok_or_else(not_ours)?;
    let encrypted = a.by_index_raw(index).map_err(|_| not_ours())?.encrypted();
    let mut text = String::new();
    let mut read = |f: &mut dyn Read, encrypted: bool| -> OpResult<()> {
        f.take(MAX_BYTES + 1).read_to_string(&mut text).map_err(|_| {
            if encrypted {
                OpError::bad("password_wrong", "The file could not be read: the password is wrong, or the file is damaged. Enter the password it was written with.")
            } else {
                OpError::bad("file_invalid", "The file is damaged or holds no text. Choose a file this service or the browser extension wrote.")
            }
        })?;
        Ok(())
    };
    if encrypted {
        match a.get_aes_verification_key_and_salt(index).ok().flatten() {
            None => return Err(OpError::bad("file_encryption", "The file is encrypted with the old ZIP method, which is not safe and not read here. Pack it anew with AES-256 encryption.")),
            Some(info) if info.aes_mode != zip::AesMode::Aes256 => {
                return Err(OpError::bad("file_encryption", "The file is encrypted with an AES strength other than 256 bits, which is not read here. Pack it anew with AES-256."))
            }
            Some(_) => {}
        }
        let pw = password.ok_or_else(|| OpError::bad("password_required", "The file is encrypted. Send its password in the header X-Backup-Password."))?;
        let mut f = a.by_index_decrypt(index, pw.as_bytes()).map_err(|_| OpError::bad("password_wrong", "The password does not fit this file. Enter the password it was written with."))?;
        read(&mut f, true)?;
    } else {
        let mut f = a.by_index(index).map_err(|_| not_ours())?;
        read(&mut f, false)?;
    }
    if text.len() as u64 > MAX_BYTES {
        return Err(OpError::bad("file_too_large", "The file holds more than 64 MiB of text; this service reads at most that."));
    }
    Ok(text)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// A time from a file in the one form the store compares: UTC, whole seconds, with Z.
fn norm_ts(t: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| d.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Rewrites the time under the key in the canonical form; false when it is no time.
fn norm_field(v: &mut Value, k: &str) -> bool {
    match v.get(k).and_then(Value::as_str).and_then(norm_ts) {
        Some(t) => {
            v[k] = json!(t);
            true
        }
        None => false,
    }
}

/// Like norm_field, for a key that may be null.
fn norm_optional(v: &mut Value, k: &str) -> bool {
    v.get(k).is_none_or(Value::is_null) || norm_field(v, k)
}

fn guid_ok(g: &str) -> bool {
    g.len() == 36 && g.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn line_error(n: usize, why: &str) -> OpError {
    OpError::bad("file_invalid", format!("Line {n} of the file is not valid: {why}. Nothing was read in; choose a file this service or the browser extension wrote."))
}

fn parse(content: &str) -> OpResult<File> {
    let mut lines = content.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    let (_, head) = lines.next().ok_or_else(|| line_error(1, "the file is empty"))?;
    let head: Value = serde_json::from_str(head).map_err(|_| line_error(1, "the head is no JSON"))?;
    if s(&head, "format") != FORMAT {
        return Err(line_error(1, "the head does not name the format portal-download"));
    }
    if head.get("version").and_then(Value::as_i64) != Some(VERSION) {
        return Err(OpError::bad(
            "file_version",
            format!("The file has format version {}; this service reads version {VERSION}, which portal-download 0.8 writes. Files of older versions are not read.", head.get("version").unwrap_or(&Value::Null)),
        ));
    }
    let kind = s(&head, "kind").to_string();
    if kind != "jobs" && kind != "full" {
        return Err(line_error(1, "the head names neither jobs nor full"));
    }
    let mut f = File { kind, jobs: vec![], runs: vec![], deleted: vec![], users: vec![], invitations: vec![], devices: vec![], agents: vec![] };
    let (mut job_guids, mut run_guids, mut replaced) = (HashSet::new(), HashSet::new(), HashSet::new());
    for (i, line) in lines {
        let n = i + 1;
        let mut v: Value = serde_json::from_str(line).map_err(|_| line_error(n, "it is no JSON"))?;
        match s(&v, "type") {
            "job" => {
                if !guid_ok(s(&v, "guid")) || check_url(s(&v, "entry_url")).is_err() || !norm_field(&mut v, "created_at") {
                    return Err(line_error(n, "a job lacks a GUID, an entry address or its creation time"));
                }
                if opt(&v, "replaces").is_some_and(|g| !guid_ok(&g)) {
                    return Err(line_error(n, "the GUID a job replaces is no GUID"));
                }
                if !job_guids.insert(s(&v, "guid").to_string()) {
                    return Err(line_error(n, "a job with this GUID came earlier in the file"));
                }
                if let Some(old) = opt(&v, "replaces") {
                    if !replaced.insert(old) {
                        return Err(line_error(n, "another job of the file replaces the same job"));
                    }
                }
                if !v.get("title").is_some_and(Value::is_string) || !v.get("retry_days").is_some_and(Value::is_u64) || !v.get("next_due").is_some_and(Value::is_string) {
                    return Err(line_error(n, "a job lacks its title, its retry days or its due date"));
                }
                let input = job_input(&v);
                validate_job(&input, true).map_err(|e| line_error(n, &e.message))?;
                v["title"] = json!(s(&v, "title").trim());
                for f in FIELDS {
                    let ok = v.get_mut("changed").filter(|c| c.is_object()).is_some_and(|c| norm_field(c, f));
                    if !ok {
                        return Err(line_error(n, &format!("a job lacks the time its field {f} changed")));
                    }
                }
                f.jobs.push(v);
            }
            "run" => {
                let ok = guid_ok(s(&v, "guid"))
                    && guid_ok(s(&v, "job_guid"))
                    && matches!(s(&v, "outcome"), "succeeded" | "failed")
                    && norm_field(&mut v, "finished_at")
                    && super::parse_date("due_before", s(&v, "due_before")).is_ok()
                    && super::parse_date("due_after", s(&v, "due_after")).is_ok()
                    && text("by", s(&v, "by"), 1, 200).is_ok()
                    && text("note", s(&v, "note"), 0, 2000).is_ok();
                if !ok {
                    return Err(line_error(n, "a run lacks its GUID, its job, its outcome or its dates"));
                }
                if !run_guids.insert(s(&v, "guid").to_string()) {
                    return Err(line_error(n, "a run with this GUID came earlier in the file"));
                }
                f.runs.push(v);
            }
            "deleted_job" => {
                if !guid_ok(s(&v, "guid")) || !norm_field(&mut v, "deleted_at") || opt(&v, "replaced_by").is_some_and(|g| !guid_ok(&g)) {
                    return Err(line_error(n, "a deleted job lacks its GUID or the time it was deleted"));
                }
                f.deleted.push(v);
            }
            "user" | "invitation" | "device" | "agent" if f.kind == "full" => {
                check_access_record(&mut v).map_err(|why| line_error(n, why))?;
                match s(&v, "type") {
                    "user" => f.users.push(v),
                    "invitation" => f.invitations.push(v),
                    "device" => f.devices.push(v),
                    _ => f.agents.push(v),
                }
            }
            t => return Err(line_error(n, &format!("the record type \"{t}\" is not known here"))),
        }
    }
    let known: HashSet<&str> = f.jobs.iter().map(|j| s(j, "guid")).collect();
    if let Some(r) = f.runs.iter().find(|r| !known.contains(s(r, "job_guid"))) {
        return Err(OpError::bad("file_invalid", format!("A run in the file belongs to job {}, which the file does not hold. Nothing was read in.", s(r, "job_guid"))));
    }
    Ok(f)
}

/// Checks a record of people and accesses as API input would be checked, and puts its times
/// in the canonical form.
fn check_access_record(v: &mut Value) -> Result<(), &'static str> {
    let hash_ok = |h: &str| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit());
    let len = |v: &Value, k: &str, min: usize, max: usize| text(k, s(v, k), min, max).is_ok();
    match s(v, "type") {
        "user" => {
            let rights_ok = v.get("rights").is_some_and(|r| serde_json::from_value::<Rights>(r.clone()).is_ok());
            if !len(v, "name", 1, 120) || !len(v, "email", 0, 200) || !rights_ok || !norm_field(v, "created_at") || !norm_optional(v, "deactivated_at") || !len(v, "created_by", 0, 200) {
                return Err("a person lacks a name, valid rights or the time they were created");
            }
        }
        "invitation" => {
            if !hash_ok(s(v, "token_hash")) || !len(v, "created_by", 0, 200) || !norm_field(v, "created_at") || !norm_field(v, "expires_at") || !norm_optional(v, "withdrawn_at") || !norm_optional(v, "redeemed_at") {
                return Err("an invitation lacks its hash or its times");
            }
        }
        "device" => {
            let kind_ok = matches!(s(v, "kind"), "browser" | "extension");
            if !kind_ok || !hash_ok(s(v, "credential_hash")) || !len(v, "label", 1, 80) || s(v, "user_agent").chars().count() > 400 || s(v, "last_address").chars().count() > 100 || !norm_field(v, "created_at") || !norm_field(v, "last_seen_at") || !norm_optional(v, "revoked_at") {
                return Err("a device lacks its kind, its hash, its name or its times");
            }
        }
        _ => {
            let display_ok = s(v, "display").len() == 8 && s(v, "display").chars().all(|c| c.is_ascii_lowercase());
            let role_ok = opt(v, "role").is_none_or(|r| super::Role::parse(&r).is_ok());
            let reach_ok = s(v, "reach").split(',').filter(|r| !r.is_empty()).all(|r| super::Role::parse(r).is_ok());
            if !hash_ok(s(v, "token_hash")) || !display_ok || !len(v, "introduction", 10, 500) || !role_ok || !reach_ok || !len(v, "granted_by", 0, 200) || !norm_field(v, "created_at") || !norm_optional(v, "granted_at") || !norm_optional(v, "last_used_at") || !norm_optional(v, "revoked_at") {
                return Err("an agent token lacks its hash, its display string, its introduction, a known role or its times");
            }
        }
    }
    Ok(())
}

fn job_input(v: &Value) -> JobInput {
    JobInput {
        title: opt(v, "title"),
        entry_url: opt(v, "entry_url"),
        interval_weeks: v.get("interval_weeks").and_then(Value::as_u64).map(|n| n.min(u32::MAX as u64) as u32),
        retry_days: v.get("retry_days").and_then(Value::as_u64).map(|n| n.min(u32::MAX as u64) as u32),
        next_due: opt(v, "next_due"),
        username: opt(v, "username").or(Some(String::new())),
        password: opt(v, "password").or(Some(String::new())),
        instructions_md: opt(v, "instructions_md").or(Some(String::new())),
    }
}

/// One side of a field: its value as JSON and when it last changed.
fn field_of_stored(j: &Stored, f: &str) -> (Value, String) {
    match f {
        "title" => (json!(j.title), j.title_at.clone()),
        "interval_weeks" => (json!(j.interval_weeks), j.interval_weeks_at.clone()),
        "retry_days" => (json!(j.retry_days), j.retry_days_at.clone()),
        "next_due" => (json!(j.next_due), j.next_due_at.clone()),
        "username" => (json!(j.username), j.username_at.clone()),
        "password" => (json!(j.password), j.password_at.clone()),
        _ => (json!(j.instructions_md), j.instructions_md_at.clone()),
    }
}

fn field_of_record(v: &Value, f: &str) -> (Value, String) {
    let at = v.get("changed").and_then(|c| c.get(f)).and_then(Value::as_str).unwrap_or("").to_string();
    (v.get(f).cloned().unwrap_or(json!("")), at)
}

/// The newest run among those of both sides, as (finished_at, due_after).
type Newest = Option<(String, String)>;

/// How one job of the file meets this store.
#[derive(Debug, Clone)]
enum Case {
    New,
    /// The GUID is known here, directly or as the old GUID of a job with a new address.
    Known { target: Stored, via_old_guid: bool },
    /// Deleted here, and not replaced.
    DeletedHere { deleted_at: String },
    /// The file's job replaces one known here.
    Replaces { target: Stored },
}

struct Item {
    record: Value,
    case: Case,
    /// Runs of the file not known here.
    new_runs: Vec<Value>,
    /// Field differences: (field, local, incoming, local_at, incoming_at, preselect).
    diffs: Vec<(String, Value, Value, String, String, &'static str)>,
    /// The due date that follows from a run newer than both dates, with that run's time.
    due_from_run: Option<(String, String)>,
}

impl Item {
    fn guid(&self) -> &str {
        s(&self.record, "guid")
    }
    fn group(&self) -> &'static str {
        match &self.case {
            Case::New => "new",
            Case::Known { .. } if self.diffs.is_empty() => "same",
            _ => "decide",
        }
    }
    fn actions(&self) -> (&'static [&'static str], &'static str) {
        match &self.case {
            Case::DeletedHere { .. } => (&["reactivate", "skip"], "skip"),
            Case::Replaces { .. } => (&["replace", "keep_both", "skip"], "replace"),
            _ => (&[], ""),
        }
    }
}

/// Compares two sides field by field; the due date follows the newest run when it is newer
/// than both dates, otherwise the nearer date is preselected.
type Diff = (String, Value, Value, String, String, &'static str);

/// Compares two sides field by field; the due date follows the newest run when it is not
/// older than both dates (a run sets the date at its own time), otherwise the nearer date is
/// preselected. For a caller who does not see the logins, both login fields are always
/// listed, so that the list does not tell whether a guessed login matches.
fn compare(local: &Stored, record: &Value, newest: &Newest, hide_logins: bool) -> (Vec<Diff>, Option<(String, String)>) {
    let mut diffs = vec![];
    let mut due_from_run = None;
    for f in FIELDS {
        let (lv, lt) = field_of_stored(local, f);
        let (iv, it) = field_of_record(record, f);
        if *f == "next_due" {
            if let Some((run_at, due_after)) = newest {
                if *run_at >= lt && *run_at >= it {
                    if *due_after != local.next_due {
                        due_from_run = Some((due_after.clone(), run_at.clone()));
                    }
                    continue;
                }
            }
            if lv != iv {
                let pre = if iv.as_str().unwrap_or("") < lv.as_str().unwrap_or("") { "incoming" } else { "local" };
                diffs.push((f.to_string(), lv, iv, lt, it, pre));
            }
            continue;
        }
        let login = *f == "username" || *f == "password";
        if hide_logins && login {
            // Always listed and always keeping the value here: neither the list nor the
            // preselection may tell whether a guessed login matches.
            diffs.push((f.to_string(), lv, iv, lt, it, "local"));
        } else if lv != iv {
            diffs.push((f.to_string(), lv, iv, lt.clone(), it.clone(), if it > lt { "incoming" } else { "local" }));
        }
    }
    (diffs, due_from_run)
}

/// What a decision for one GUID says: the preselection as a whole, a side per field, an
/// action, or an action together with a side per field (replacing a job and its fields).
#[derive(Debug, Default)]
struct Choice {
    preselected: bool,
    fields: Option<HashMap<String, bool>>,
    action: Option<String>,
}

fn parse_decisions(v: &Value) -> OpResult<HashMap<String, Choice>> {
    let bad = |why: String| OpError::bad("decision_invalid", format!("{why} Send for each GUID of the group decide {{\"accept\":\"preselected\"}}, or \"local\" or \"incoming\" for every field the preview lists, as {{\"fields\":{{\"password\":\"incoming\"}}}}, and one of the actions it lists, as {{\"action\":\"replace\"}}."));
    let mut out = HashMap::new();
    let Some(map) = v.as_object() else {
        return if v.is_null() { Ok(out) } else { Err(bad("`decisions` must be an object keyed by GUID.".into())) };
    };
    for (guid, d) in map {
        let mut c = Choice { preselected: d.get("accept").and_then(Value::as_str) == Some("preselected"), ..Choice::default() };
        if let Some(a) = d.get("action") {
            c.action = Some(a.as_str().ok_or_else(|| bad(format!("The action for {guid} is no text.")))?.to_string());
        }
        if let Some(fs) = d.get("fields") {
            let fs = fs.as_object().ok_or_else(|| bad(format!("The fields for {guid} are no object.")))?;
            let mut m = HashMap::new();
            for (f, side) in fs {
                match side.as_str() {
                    Some("local") => m.insert(f.clone(), false),
                    Some("incoming") => m.insert(f.clone(), true),
                    _ => return Err(bad(format!("The side for field {f} of {guid} is neither local nor incoming."))),
                };
            }
            c.fields = Some(m);
        }
        if !c.preselected && c.action.is_none() && c.fields.is_none() {
            return Err(bad(format!("The decision for {guid} names nothing to do.")));
        }
        out.insert(guid.clone(), c);
    }
    Ok(out)
}

/// What happened to the people part of a full import.
#[derive(Debug, Default)]
struct AccessReport {
    people: usize,
    invitations: usize,
    devices: usize,
    agents: usize,
    renamed: Vec<String>,
}

impl Store {
    fn stored_by_guid(&self, guid: &str) -> OpResult<Option<Stored>> {
        Ok(self.conn.query_row(&format!("SELECT {STORED_COLS} FROM jobs WHERE guid = ?1"), [guid], stored_row).optional()?)
    }

    fn deleted_here(&self, guid: &str) -> OpResult<Option<(String, Option<String>)>> {
        Ok(self.conn.query_row("SELECT deleted_at, replaced_by FROM deleted_jobs WHERE guid = ?1", [guid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    /// When a chain of replacements from this GUID ends in a deleted job, when that was deleted.
    fn deleted_at_chain_end(&self, guid: &str) -> OpResult<Option<String>> {
        let mut g = guid.to_string();
        for _ in 0..50 {
            match self.deleted_here(&g)? {
                Some((at, None)) => return Ok(Some(at)),
                Some((_, Some(next))) if self.stored_by_guid(&next)?.is_none() => g = next,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The job here that a chain of replacements leads to from this GUID, if any.
    fn successor(&self, guid: &str) -> OpResult<Option<Stored>> {
        let mut g = guid.to_string();
        for _ in 0..50 {
            match self.deleted_here(&g)? {
                Some((_, Some(next))) => match self.stored_by_guid(&next)? {
                    Some(j) => return Ok(Some(j)),
                    None => g = next,
                },
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    fn run_known(&self, guid: &str) -> OpResult<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE guid = ?1)", [guid], |r| r.get(0))?)
    }

    fn newest_local_run(&self, job_id: i64) -> OpResult<Newest> {
        Ok(self.conn.query_row("SELECT finished_at, due_after FROM runs WHERE job_id = ?1 ORDER BY finished_at DESC, id DESC LIMIT 1", [job_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    /// Sorts every job of the file into new, same or decide.
    fn plan(&self, f: &File, hide_logins: bool) -> OpResult<Vec<Item>> {
        let mut runs_of: HashMap<&str, Vec<&Value>> = HashMap::new();
        for r in &f.runs {
            runs_of.entry(s(r, "job_guid")).or_default().push(r);
        }
        let mut items = vec![];
        for rec in &f.jobs {
            let guid = s(rec, "guid");
            let case = if let Some(j) = self.stored_by_guid(guid)? {
                Case::Known { target: j, via_old_guid: false }
            } else if let Some(j) = self.successor(guid)? {
                Case::Known { target: j, via_old_guid: true }
            } else if let Some(deleted_at) = self.deleted_at_chain_end(guid)? {
                Case::DeletedHere { deleted_at }
            } else if let Some(deleted_at) = match opt(rec, "replaces") {
                // A job with a new address for one deleted here is a deleted job too.
                Some(old) if self.stored_by_guid(&old)?.is_none() && self.successor(&old)?.is_none() => self.deleted_at_chain_end(&old)?,
                _ => None,
            } {
                Case::DeletedHere { deleted_at }
            } else if let Some(j) = match opt(rec, "replaces") {
                Some(old) => match self.stored_by_guid(&old)? {
                    Some(j) => Some(j),
                    None => self.successor(&old)?,
                },
                None => None,
            } {
                Case::Replaces { target: j }
            } else {
                Case::New
            };
            let mut new_runs = vec![];
            for r in runs_of.get(guid).map(Vec::as_slice).unwrap_or(&[]) {
                if !self.run_known(s(r, "guid"))? {
                    new_runs.push((*r).clone());
                }
            }
            let (diffs, due_from_run) = match &case {
                Case::Known { target, .. } | Case::Replaces { target } => {
                    let mut newest = self.newest_local_run(target.id)?;
                    for r in &new_runs {
                        let cand = (s(r, "finished_at").to_string(), s(r, "due_after").to_string());
                        if newest.as_ref().is_none_or(|n| cand.0 > n.0) {
                            newest = Some(cand);
                        }
                    }
                    compare(target, rec, &newest, hide_logins)
                }
                _ => (vec![], None),
            };
            items.push(Item { record: rec.clone(), case, new_runs, diffs, due_from_run });
        }
        Ok(items)
    }

    fn preview_json(&self, p: &Principal, f: &File, items: &[Item]) -> Value {
        let hide = !p.sees_logins();
        let shown = |field: &str, v: &Value| if hide && (field == "username" || field == "password") { json!("•••") } else { v.clone() };
        let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::from([("new", vec![]), ("same", vec![]), ("decide", vec![])]);
        for it in items {
            let mut v = json!({
                "guid": it.guid(), "title": s(&it.record, "title"), "entry_url": s(&it.record, "entry_url"), "new_runs": it.new_runs.len(),
            });
            match &it.case {
                Case::Known { target, via_old_guid } => {
                    v["job_id"] = json!(target.id);
                    v["case"] = json!(if *via_old_guid { "old_address" } else { "known" });
                    if *via_old_guid {
                        v["local_entry_url"] = json!(target.entry_url);
                    }
                }
                Case::DeletedHere { deleted_at } => {
                    v["case"] = json!("deleted_here");
                    v["deleted_at"] = json!(deleted_at);
                }
                Case::Replaces { target } => {
                    v["case"] = json!("replaces");
                    v["job_id"] = json!(target.id);
                    v["replaces_title"] = json!(target.title);
                    v["local_entry_url"] = json!(target.entry_url);
                }
                Case::New => {}
            }
            if let Some((d, _)) = &it.due_from_run {
                v["next_due_from_run"] = json!(d);
            }
            if !it.diffs.is_empty() {
                v["fields"] = json!(it
                    .diffs
                    .iter()
                    .map(|(f, l, i, lt, itm, pre)| json!({ "field": f, "local": shown(f, l), "incoming": shown(f, i), "local_changed_at": lt, "incoming_changed_at": itm, "preselect": pre }))
                    .collect::<Vec<_>>());
            }
            let (actions, pre) = it.actions();
            if !actions.is_empty() {
                v["actions"] = json!(actions);
                v["preselect"] = json!(pre);
            }
            groups.get_mut(it.group()).expect("known group").push(v);
        }
        let mut out = json!({ "kind": f.kind, "new": groups["new"], "same": groups["same"], "decide": groups["decide"] });
        if f.kind == "full" {
            out["people"] = json!({ "people": f.users.len(), "invitations": f.invitations.len(), "devices": f.devices.len(), "agents": f.agents.len() });
        }
        out
    }

    pub fn import_preview(&self, p: &Principal, bytes: &[u8], password: Option<&str>, mode: ImportMode) -> OpResult<Value> {
        self.require_import(p, mode)?;
        let f = parse(&unzip(bytes, password)?)?;
        let items = self.plan(&f, !p.sees_logins())?;
        Ok(self.preview_json(p, &f, &items))
    }

    fn require_import(&self, p: &Principal, mode: ImportMode) -> OpResult<()> {
        match mode {
            ImportMode::Jobs => p.require(Cap::ImportJobs),
            ImportMode::Full => p.require(Cap::BackupImport),
        }
    }

    /// Applies the preview with the decisions given; nothing is applied when a job of the
    /// group decide has none.
    pub fn import_apply(&self, p: &Principal, bytes: &[u8], password: Option<&str>, mode: ImportMode, decisions: &Value, today: NaiveDate) -> OpResult<Value> {
        self.require_import(p, mode)?;
        let f = parse(&unzip(bytes, password)?)?;
        if mode == ImportMode::Full && f.kind != "full" {
            return Err(OpError::bad("file_kind", "This file holds jobs only, no people. Read it in under the exchange of jobs."));
        }
        let decisions = parse_decisions(decisions)?;
        let items = self.plan(&f, !p.sees_logins())?;
        let missing: Vec<&str> = items.iter().filter(|i| i.group() == "decide" && !decisions.contains_key(i.guid())).map(Item::guid).collect();
        if !missing.is_empty() {
            return Err(OpError::bad(
                "decision_missing",
                format!("The jobs {} need a decision; call the preview again and send one for each of them. Nothing was read in.", missing.join(", ")),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut done = json!({ "added": 0, "updated": 0, "unchanged": 0, "reactivated": 0, "replaced": 0, "skipped": 0, "runs_added": 0 });
        let bump = |done: &mut Value, k: &str, n: usize| done[k] = json!(done[k].as_u64().unwrap_or(0) + n as u64);
        for it in &items {
            let choice = decisions.get(it.guid());
            // A side per field, unless the whole preselection was accepted; a job of the group
            // same has no decision and no differences.
            let take = |field: &str, pre: &'static str| -> OpResult<bool> {
                match choice {
                    Some(Choice { fields: Some(m), .. }) => m.get(field).copied().ok_or_else(|| {
                        OpError::bad("decision_missing", format!("The decision for {} names no side for its field {field}. Nothing was read in.", it.guid()))
                    }),
                    Some(Choice { preselected: true, .. }) | None => Ok(pre == "incoming"),
                    Some(_) => Err(OpError::bad(
                        "decision_missing",
                        format!("The decision for {} names no side for its field {field}; send fields with a side for each field the preview lists. Nothing was read in.", it.guid()),
                    )),
                }
            };
            let action = |offered: &[&str], pre: &str| -> OpResult<String> {
                match choice.and_then(|c| c.action.as_deref()) {
                    Some(a) if offered.contains(&a) => Ok(a.to_string()),
                    Some(a) => Err(OpError::bad("decision_invalid", format!("\"{a}\" is not offered for {}; the preview offers {}. Nothing was read in.", it.guid(), offered.join(", ")))),
                    None if choice.is_some_and(|c| c.preselected) => Ok(pre.to_string()),
                    None => Err(OpError::bad("decision_missing", format!("The decision for {} names no action; the preview offers {}. Nothing was read in.", it.guid(), offered.join(", ")))),
                }
            };
            match &it.case {
                Case::New => {
                    let id = insert_job(&tx, &it.record)?;
                    bump(&mut done, "runs_added", insert_runs(&tx, id, &it.new_runs)?);
                    bump(&mut done, "added", 1);
                }
                Case::Known { target, .. } => {
                    let mut changed = false;
                    for (field, _, incoming, _, at, pre) in &it.diffs {
                        if take(field, pre)? {
                            set_field(&tx, target.id, field, incoming, at)?;
                            changed = true;
                        }
                    }
                    bump(&mut done, "runs_added", insert_runs(&tx, target.id, &it.new_runs)?);
                    if let Some((d, run_at)) = &it.due_from_run {
                        tx.execute("UPDATE jobs SET next_due = ?1, next_due_at = ?2 WHERE id = ?3", params![d, run_at, target.id])?;
                        changed = true;
                    }
                    bump(&mut done, if changed || !it.new_runs.is_empty() { "updated" } else { "unchanged" }, 1);
                }
                Case::DeletedHere { .. } => {
                    if action(&["reactivate", "skip"], "skip")? == "reactivate" {
                        tx.execute("DELETE FROM deleted_jobs WHERE guid = ?1", [it.guid()])?;
                        // Brought back as the replacement of a job deleted here: the old GUID now leads to it.
                        if let Some(old) = opt(&it.record, "replaces") {
                            tx.execute("UPDATE deleted_jobs SET replaced_by = ?1 WHERE guid = ?2 AND replaced_by IS NULL", params![it.guid(), old])?;
                        }
                        let id = insert_job(&tx, &it.record)?;
                        bump(&mut done, "runs_added", insert_runs(&tx, id, &it.new_runs)?);
                        bump(&mut done, "reactivated", 1);
                    } else {
                        bump(&mut done, "skipped", 1);
                    }
                }
                Case::Replaces { target } => match action(&["replace", "keep_both", "skip"], "replace")?.as_str() {
                    "replace" => {
                        let new_guid = it.guid();
                        tx.execute(
                            "INSERT OR REPLACE INTO deleted_jobs(guid, title, entry_url, deleted_at, replaced_by) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![target.guid, target.title, target.entry_url, now(), new_guid],
                        )?;
                        tx.execute(
                            "UPDATE jobs SET guid = ?1, replaces = ?2, entry_url = ?3 WHERE id = ?4",
                            params![new_guid, target.guid, s(&it.record, "entry_url"), target.id],
                        )?;
                        for (field, _, incoming, _, at, pre) in &it.diffs {
                            if take(field, pre)? {
                                set_field(&tx, target.id, field, incoming, at)?;
                            }
                        }
                        bump(&mut done, "runs_added", insert_runs(&tx, target.id, &it.new_runs)?);
                        if let Some((d, run_at)) = &it.due_from_run {
                            tx.execute("UPDATE jobs SET next_due = ?1, next_due_at = ?2 WHERE id = ?3", params![d, run_at, target.id])?;
                        }
                        bump(&mut done, "replaced", 1);
                    }
                    "keep_both" => {
                        let id = insert_job(&tx, &it.record)?;
                        bump(&mut done, "runs_added", insert_runs(&tx, id, &it.new_runs)?);
                        bump(&mut done, "added", 1);
                    }
                    _ => bump(&mut done, "skipped", 1),
                },
            }
        }
        // The file's deletions are remembered here too, unless the job is in use here.
        for d in &f.deleted {
            tx.execute(
                "INSERT OR IGNORE INTO deleted_jobs(guid, title, entry_url, deleted_at, replaced_by) SELECT ?1, ?2, ?3, ?4, ?5 WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE guid = ?1)",
                params![s(d, "guid"), s(d, "title"), s(d, "entry_url"), s(d, "deleted_at"), opt(d, "replaced_by")],
            )?;
        }
        if mode == ImportMode::Full {
            let r = import_access(&tx, &f, today)?;
            done["people"] = json!({ "people": r.people, "invitations": r.invitations, "devices": r.devices, "agents": r.agents, "renamed": r.renamed });
        }
        tx.commit()?;
        Ok(done)
    }
}

fn insert_job(tx: &Transaction, rec: &Value) -> OpResult<i64> {
    let at = |f: &str| field_of_record(rec, f).1;
    let i = job_input(rec);
    tx.execute(
        "INSERT INTO jobs(guid, replaces, title, entry_url, interval_weeks, retry_days, next_due, username, password, instructions_md, created_at,
           title_at, interval_weeks_at, retry_days_at, next_due_at, username_at, password_at, instructions_md_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![
            s(rec, "guid"),
            opt(rec, "replaces"),
            i.title.unwrap_or_default().trim(),
            check_url(s(rec, "entry_url"))?,
            i.interval_weeks,
            i.retry_days.unwrap_or(2),
            i.next_due.unwrap_or_else(|| super::today().format("%Y-%m-%d").to_string()),
            i.username.unwrap_or_default(),
            i.password.unwrap_or_default(),
            i.instructions_md.unwrap_or_default(),
            s(rec, "created_at"),
            at("title"),
            at("interval_weeks"),
            at("retry_days"),
            at("next_due"),
            at("username"),
            at("password"),
            at("instructions_md"),
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn insert_runs(tx: &Transaction, job_id: i64, runs: &[Value]) -> OpResult<usize> {
    let mut n = 0;
    for r in runs {
        n += tx.execute(
            "INSERT OR IGNORE INTO runs(guid, job_id, by_name, outcome, note, finished_at, due_before, due_after) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![s(r, "guid"), job_id, s(r, "by"), s(r, "outcome"), s(r, "note"), s(r, "finished_at"), s(r, "due_before"), s(r, "due_after")],
        )?;
    }
    Ok(n)
}

fn set_field(tx: &Transaction, job_id: i64, field: &str, value: &Value, at: &str) -> OpResult<()> {
    assert!(FIELDS.contains(&field), "only merged fields are set");
    let sql = format!("UPDATE jobs SET {field} = ?1, {field}_at = ?2 WHERE id = ?3");
    match value {
        Value::Number(n) => tx.execute(&sql, params![n.as_i64(), at, job_id])?,
        v => tx.execute(&sql, params![v.as_str().unwrap_or(""), at, job_id])?,
    };
    Ok(())
}

/// Adds people, invitations, devices and agent tokens beside the existing ones. A person
/// with the name or e-mail of one here comes in as "… (Import YYYY-MM-DD)".
fn import_access(tx: &Transaction, f: &File, today: NaiveDate) -> OpResult<AccessReport> {
    let mut r = AccessReport::default();
    let mut user_ids: HashMap<i64, i64> = HashMap::new();
    let mut inv_ids: HashMap<i64, i64> = HashMap::new();
    let suffix = format!(" (Import {})", today.format("%Y-%m-%d"));
    for u in &f.users {
        let mut name = text("name", s(u, "name"), 1, 120).map_err(|e| OpError::bad("file_invalid", e.message))?;
        let email = s(u, "email").trim().to_string();
        let taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE name = ?1 COLLATE NOCASE OR (?2 <> '' AND email = ?2 COLLATE NOCASE))",
            params![name, email],
            |x| x.get(0),
        )?;
        let email = if taken {
            r.renamed.push(name.clone());
            name = format!("{}{suffix}", name.chars().take(120 - suffix.chars().count()).collect::<String>());
            String::new()
        } else {
            email
        };
        let rights: Rights = serde_json::from_value(u.get("rights").cloned().unwrap_or(json!({}))).unwrap_or_default();
        tx.execute(
            "INSERT INTO users(name, email, can_people, can_agents, can_backup, deactivated_at, created_at, created_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![name, email, rights.people, rights.agents, rights.backup, opt(u, "deactivated_at"), s(u, "created_at"), s(u, "created_by")],
        )?;
        user_ids.insert(u.get("id").and_then(Value::as_i64).unwrap_or(-1), tx.last_insert_rowid());
        r.people += 1;
    }
    for i in &f.invitations {
        let Some(&uid) = i.get("user_id").and_then(Value::as_i64).and_then(|x| user_ids.get(&x)) else { continue };
        let n = tx.execute(
            "INSERT OR IGNORE INTO invitations(user_id, token_hash, own, created_by, created_at, expires_at, withdrawn_at, redeemed_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![uid, s(i, "token_hash"), i.get("own").and_then(Value::as_bool).unwrap_or(false), s(i, "created_by"), s(i, "created_at"), s(i, "expires_at"), opt(i, "withdrawn_at"), opt(i, "redeemed_at")],
        )?;
        if n == 1 {
            inv_ids.insert(i.get("id").and_then(Value::as_i64).unwrap_or(-1), tx.last_insert_rowid());
            r.invitations += 1;
        }
    }
    for d in &f.devices {
        let Some(&uid) = d.get("user_id").and_then(Value::as_i64).and_then(|x| user_ids.get(&x)) else { continue };
        let inv = d.get("invitation_id").and_then(Value::as_i64).and_then(|x| inv_ids.get(&x).copied());
        let kind = if s(d, "kind") == "extension" { "extension" } else { "browser" };
        r.devices += tx.execute(
            "INSERT OR IGNORE INTO devices(user_id, invitation_id, kind, credential_hash, label, user_agent, last_address, created_at, last_seen_at, revoked_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![uid, inv, kind, s(d, "credential_hash"), s(d, "label"), s(d, "user_agent"), s(d, "last_address"), s(d, "created_at"), s(d, "last_seen_at"), opt(d, "revoked_at")],
        )?;
    }
    for a in &f.agents {
        r.agents += tx.execute(
            "INSERT OR IGNORE INTO agents(token_hash, display, introduction, role, reach, granted_by, granted_at, created_at, last_used_at, revoked_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![s(a, "token_hash"), s(a, "display"), s(a, "introduction"), opt(a, "role"), s(a, "reach"), opt(a, "granted_by"), opt(a, "granted_at"), s(a, "created_at"), opt(a, "last_used_at"), opt(a, "revoked_at")],
        )?;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{admin, agent, person};
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn job(s: &Store, p: &Principal, title: &str, url: &str) -> i64 {
        let i = JobInput { title: Some(title.into()), entry_url: Some(url.into()), interval_weeks: Some(4), password: Some("pw".into()), ..Default::default() };
        s.job_create(p, i, d("2026-11-06")).unwrap()["job_id"].as_i64().unwrap()
    }

    /// Two stores with the same person, as a server and a laptop.
    fn pair() -> (Store, Principal, Store, Principal) {
        let a = Store::open_in_memory().unwrap();
        let pa = admin(&a);
        let b = Store::open_in_memory().unwrap();
        let pb = admin(&b);
        (a, pa, b, pb)
    }

    fn groups(v: &Value) -> (usize, usize, usize) {
        (v["new"].as_array().unwrap().len(), v["same"].as_array().unwrap().len(), v["decide"].as_array().unwrap().len())
    }

    #[test]
    fn a_file_is_read_back_with_and_without_a_password() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        job(&s, &a, "Energy", "https://a.example/");
        let plain = s.job_export(&a, None).unwrap();
        let locked = s.job_export(&a, Some("correct horse")).unwrap();
        assert!(!String::from_utf8_lossy(&locked).contains("Energy"), "the encrypted file hides its content");
        assert!(unzip(&plain, None).unwrap().contains("\"Energy\""));
        assert_eq!(unzip(&locked, None).err().unwrap().kind, "password_required");
        assert_eq!(unzip(&locked, Some("wrong horse")).err().unwrap().kind, "password_wrong");
        assert!(unzip(&locked, Some("correct horse")).unwrap().contains("\"Energy\""));
        assert_eq!(s.job_export(&a, Some("short")).err().unwrap().kind, "password_invalid");
        assert_eq!(unzip(b"no zip", None).err().unwrap().kind, "file_invalid");
    }

    #[test]
    fn the_old_zip_encryption_is_refused() {
        // A file marked encrypted without the AES field is one of the old method; this
        // program writes none, so the mark is set by hand in both headers.
        let mut bytes = zip_text("{}", None).unwrap();
        bytes[6] |= 1;
        let cd = bytes.windows(4).position(|w| w == [0x50, 0x4b, 0x01, 0x02]).unwrap();
        bytes[cd + 8] |= 1;
        assert_eq!(unzip(&bytes, Some("password1")).err().unwrap().kind, "file_encryption");
    }

    #[test]
    fn new_same_and_decide_and_a_merge_that_keeps_every_run() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://energy.example/");
        // The laptop takes the server's jobs.
        let file = a.job_export(&pa, None).unwrap();
        let pre = b.import_preview(&pb, &file, None, ImportMode::Jobs).unwrap();
        assert_eq!(groups(&pre), (1, 0, 0));
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        // Read again: the same.
        assert_eq!(groups(&b.import_preview(&pb, &file, None, ImportMode::Jobs).unwrap()), (0, 1, 0));
        // On the laptop the job is done today; the server meanwhile changes the password.
        let bid = b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["job_id"].as_i64().unwrap();
        b.job_complete(&pb, bid, "succeeded", "filed", None, d("2026-11-06")).unwrap();
        a.conn.execute("UPDATE jobs SET password = 'new', password_at = '2099-01-01T00:00:00Z' WHERE id = ?1", [id]).unwrap();
        // Back to the server: the run comes, the due date follows it, the password is a decision.
        let back = b.job_export(&pb, None).unwrap();
        let pre = a.import_preview(&pa, &back, None, ImportMode::Jobs).unwrap();
        assert_eq!(groups(&pre), (0, 0, 1));
        let item = &pre["decide"][0];
        assert_eq!(item["new_runs"], 1);
        assert_eq!(item["next_due_from_run"], "2026-12-04");
        assert_eq!(item["fields"].as_array().unwrap().len(), 1);
        assert_eq!(item["fields"][0]["field"], "password");
        assert_eq!(item["fields"][0]["preselect"], "local", "the newer side is preselected");
        // Without a decision nothing happens.
        assert_eq!(a.import_apply(&pa, &back, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).err().unwrap().kind, "decision_missing");
        let guid = item["guid"].as_str().unwrap();
        let r = a.import_apply(&pa, &back, None, ImportMode::Jobs, &json!({ guid: {"accept": "preselected"} }), d("2026-11-06")).unwrap();
        assert_eq!(r["runs_added"], 1);
        let j = a.job_get(&pa, id, d("2026-11-06")).unwrap();
        assert_eq!((j["password"].as_str(), j["next_due"].as_str()), (Some("new"), Some("2026-12-04")));
        assert_eq!(j["last_run"]["note"], "filed");
    }

    #[test]
    fn a_date_changed_by_hand_on_both_sides_preselects_the_nearer() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://energy.example/");
        let file = a.job_export(&pa, None).unwrap();
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        a.job_update(&pa, id, JobInput { next_due: Some("2026-12-20".into()), ..Default::default() }, d("2026-11-06")).unwrap();
        let bid = b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["job_id"].as_i64().unwrap();
        b.job_update(&pb, bid, JobInput { next_due: Some("2026-12-10".into()), ..Default::default() }, d("2026-11-06")).unwrap();
        let pre = a.import_preview(&pa, &b.job_export(&pb, None).unwrap(), None, ImportMode::Jobs).unwrap();
        let f = &pre["decide"][0]["fields"][0];
        assert_eq!((f["field"].as_str(), f["preselect"].as_str()), (Some("next_due"), Some("incoming")));
    }

    #[test]
    fn a_job_deleted_here_stays_deleted_unless_reactivated() {
        let (a, pa, b, pb) = pair();
        job(&a, &pa, "Energy", "https://energy.example/");
        let file = a.job_export(&pa, None).unwrap();
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        let bid = b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["job_id"].as_i64().unwrap();
        b.job_delete(&pb, bid).unwrap();
        let pre = b.import_preview(&pb, &file, None, ImportMode::Jobs).unwrap();
        let item = &pre["decide"][0];
        assert_eq!((item["case"].as_str(), item["preselect"].as_str()), (Some("deleted_here"), Some("skip")));
        let guid = item["guid"].as_str().unwrap();
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({ guid: {"accept": "preselected"} }), d("2026-11-06")).unwrap();
        assert_eq!(b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"].as_array().unwrap().len(), 0);
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({ guid: {"action": "reactivate"} }), d("2026-11-06")).unwrap();
        assert_eq!(b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["guid"], guid);
    }

    #[test]
    fn a_new_address_replaces_the_old_job_on_the_other_side() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://old.example/");
        a.job_complete(&pa, id, "succeeded", "server run", None, d("2026-11-06")).unwrap();
        let server_before = a.job_export(&pa, None).unwrap();
        b.import_apply(&pb, &server_before, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        // On the laptop the address changes.
        let bid = b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["job_id"].as_i64().unwrap();
        b.job_update(&pb, bid, JobInput { entry_url: Some("https://new.example/".into()), ..Default::default() }, d("2026-11-06")).unwrap();
        let laptop = b.job_export(&pb, None).unwrap();
        let pre = a.import_preview(&pa, &laptop, None, ImportMode::Jobs).unwrap();
        let item = &pre["decide"][0];
        assert_eq!((item["case"].as_str(), item["preselect"].as_str()), (Some("replaces"), Some("replace")));
        let guid = item["guid"].as_str().unwrap().to_string();
        a.import_apply(&pa, &laptop, None, ImportMode::Jobs, &json!({ guid.clone(): {"accept": "preselected"} }), d("2026-11-06")).unwrap();
        let j = a.job_get(&pa, id, d("2026-11-06")).unwrap();
        assert_eq!((j["guid"].as_str(), j["entry_url"].as_str()), (Some(guid.as_str()), Some("https://new.example/")));
        assert_eq!(a.job_history(&pa, id).unwrap()["runs"].as_array().unwrap().len(), 1, "the history stays with the job");
        // A file the server wrote before, with the old GUID, folds into the job with the new address.
        let pre = b.import_preview(&pb, &server_before, None, ImportMode::Jobs).unwrap();
        assert_eq!(groups(&pre).0, 0, "known through the replacement, never new");
        let item = if pre["same"].as_array().unwrap().is_empty() { &pre["decide"][0] } else { &pre["same"][0] };
        assert_eq!(item["case"], "old_address");
    }

    #[test]
    fn agents_that_manage_jobs_import_without_seeing_the_logins() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://energy.example/");
        b.import_apply(&pb, &a.job_export(&pa, None).unwrap(), None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        a.conn.execute("UPDATE jobs SET password = 'new', password_at = '2099-01-01T00:00:00Z' WHERE id = ?1", [id]).unwrap();
        let manage = agent(&b, &pb, Some("jobs.manage"));
        let pre = b.import_preview(&manage, &a.job_export(&pa, None).unwrap(), None, ImportMode::Jobs).unwrap();
        assert_eq!(pre["decide"][0]["fields"][0]["incoming"], "•••");
        assert_eq!(b.job_export(&manage, None).err().unwrap().kind, "right_missing", "it cannot take the logins out");
        let read = agent(&b, &pb, Some("jobs.read"));
        assert_eq!(b.import_preview(&read, &a.job_export(&pa, None).unwrap(), None, ImportMode::Jobs).err().unwrap().kind, "right_missing");
    }

    #[test]
    fn the_full_backup_restores_people_and_access_beside_existing_ones() {
        let (a, pa, b, pb) = pair();
        person(&a, &pa, "Bob", Rights::default());
        job(&a, &pa, "Energy", "https://energy.example/");
        let full = a.backup_export(&pa, Some("long password")).unwrap();
        let pre = b.import_preview(&pb, &full, Some("long password"), ImportMode::Full).unwrap();
        assert_eq!(pre["people"]["people"], 2);
        let r = b.import_apply(&pb, &full, Some("long password"), ImportMode::Full, &json!({}), d("2026-11-06")).unwrap();
        assert_eq!(r["people"]["renamed"], json!(["Ada"]), "the Ada of the file meets the Ada here");
        let names: Vec<String> = b.user_list(&pb).unwrap()["users"].as_array().unwrap().iter().map(|u| u["name"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&"Bob".to_string()) && names.contains(&"Ada (Import 2026-11-06)".to_string()));
        // A person without both rights cannot read people in.
        let plain = person(&b, &pb, "Carl", Rights { backup: true, ..Rights::default() });
        assert_eq!(b.import_preview(&plain, &full, Some("long password"), ImportMode::Full).err().unwrap().kind, "right_missing");
        // The jobs alone can be read from a full backup by anybody.
        assert!(b.import_preview(&plain, &full, Some("long password"), ImportMode::Jobs).is_ok());
        // The backup is for those who manage it and for backup tokens.
        assert_eq!(a.backup_export(&person(&a, &pa, "Dora", Rights::default()), None).err().unwrap().kind, "right_missing");
        assert!(a.backup_export(&agent(&a, &pa, Some("backup.read")), None).is_ok());
    }

    #[test]
    fn a_damaged_line_rejects_the_whole_file() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let head = json!({"format": FORMAT, "version": VERSION, "kind": "jobs"}).to_string();
        let bad = format!("{head}\n{{\"type\":\"job\",\"guid\":\"x\"}}\n");
        let e = s.import_preview(&a, &zip_text(&bad, None).unwrap(), None, ImportMode::Jobs).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("Line 2"), "{}", e.message);
        let other = json!({"format": "another-program", "version": VERSION, "kind": "jobs"}).to_string();
        let e = s.import_preview(&a, &zip_text(&other, None).unwrap(), None, ImportMode::Jobs).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("Line 1"), "{}", e.message);
        let old = json!({"format": FORMAT, "version": 2, "kind": "jobs"}).to_string();
        assert_eq!(s.import_preview(&a, &zip_text(&old, None).unwrap(), None, ImportMode::Jobs).err().unwrap().kind, "file_version");
    }

    /// A file with lines of our own, for the checks of reading a file in.
    fn file_of(lines: &[Value]) -> Vec<u8> {
        let mut t = json!({"format": FORMAT, "version": VERSION, "kind": "jobs"}).to_string();
        for l in lines {
            t.push('\n');
            t.push_str(&l.to_string());
        }
        zip_text(&t, None).unwrap()
    }

    fn job_line(guid: &str, title: &str, at: &str) -> Value {
        let changed: serde_json::Map<String, Value> = FIELDS.iter().map(|f| (f.to_string(), json!(at))).collect();
        json!({"type": "job", "guid": guid, "replaces": null, "title": title, "entry_url": "https://a.example/", "interval_weeks": 4, "retry_days": 2,
               "next_due": "2026-11-06", "username": "", "password": "pw", "instructions_md": "", "created_at": at, "changed": changed})
    }

    #[test]
    fn a_job_replaced_and_then_deleted_here_is_not_new() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://old.example/");
        let old_file = a.job_export(&pa, None).unwrap();
        a.job_update(&pa, id, JobInput { entry_url: Some("https://new.example/".into()), ..Default::default() }, d("2026-11-06")).unwrap();
        a.job_delete(&pa, id).unwrap();
        let pre = a.import_preview(&pa, &old_file, None, ImportMode::Jobs).unwrap();
        assert_eq!(groups(&pre), (0, 0, 1));
        assert_eq!(pre["decide"][0]["case"], "deleted_here");
        let _ = (b, pb);
    }

    #[test]
    fn replacing_takes_a_side_for_every_differing_field() {
        let (a, pa, b, pb) = pair();
        let id = job(&a, &pa, "Energy", "https://old.example/");
        b.import_apply(&pb, &a.job_export(&pa, None).unwrap(), None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        let bid = b.job_list(&pb, d("2026-11-06")).unwrap()["jobs"][0]["job_id"].as_i64().unwrap();
        b.job_update(&pb, bid, JobInput { entry_url: Some("https://new.example/".into()), ..Default::default() }, d("2026-11-06")).unwrap();
        b.conn.execute("UPDATE jobs SET password = 'theirs', password_at = '2099-01-01T00:00:00Z' WHERE id = ?1", [bid]).unwrap();
        let file = b.job_export(&pb, None).unwrap();
        let pre = a.import_preview(&pa, &file, None, ImportMode::Jobs).unwrap();
        let item = &pre["decide"][0];
        assert_eq!((item["case"].as_str(), item["fields"][0]["field"].as_str()), (Some("replaces"), Some("password")));
        let guid = item["guid"].as_str().unwrap();
        // An action alone does not take over a field the user did not see.
        assert_eq!(a.import_apply(&pa, &file, None, ImportMode::Jobs, &json!({ guid: {"action": "replace"} }), d("2026-11-06")).err().unwrap().kind, "decision_missing");
        a.import_apply(&pa, &file, None, ImportMode::Jobs, &json!({ guid: {"action": "replace", "fields": {"password": "local"}} }), d("2026-11-06")).unwrap();
        let j = a.job_get(&pa, id, d("2026-11-06")).unwrap();
        assert_eq!((j["password"].as_str(), j["entry_url"].as_str()), (Some("pw"), Some("https://new.example/")));
    }

    #[test]
    fn times_in_a_file_are_compared_as_instants() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let g = "0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f";
        s.import_apply(&a, &file_of(&[job_line(g, "Energy", "2026-11-06T12:00:00Z")]), None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        // 13:30 at +02:00 is 11:30 UTC: older than the 12:00 here, although it sorts later as text.
        let mut older = job_line(g, "Energy invoice", "2026-11-06T13:30:00+02:00");
        older["changed"]["title"] = json!("2026-11-06T13:30:00+02:00");
        let pre = s.import_preview(&a, &file_of(&[older]), None, ImportMode::Jobs).unwrap();
        let f = &pre["decide"][0]["fields"][0];
        assert_eq!((f["field"].as_str(), f["preselect"].as_str(), f["incoming_changed_at"].as_str()), (Some("title"), Some("local"), Some("2026-11-06T11:30:00Z")));
    }

    #[test]
    fn a_guid_twice_in_a_file_is_refused_with_its_line() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let g = "0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f";
        let e = s.import_preview(&a, &file_of(&[job_line(g, "A", "2026-11-06T12:00:00Z"), job_line(g, "B", "2026-11-06T12:00:00Z")]), None, ImportMode::Jobs).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("Line 3"), "{}", e.message);
    }

    #[test]
    fn a_job_in_a_file_names_its_retry_days_and_due_date() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let mut j = job_line("0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f", "A", "2026-11-06T12:00:00Z");
        j.as_object_mut().unwrap().remove("retry_days");
        let e = s.import_preview(&a, &file_of(&[j]), None, ImportMode::Jobs).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("retry days"), "{}", e.message);
    }

    #[test]
    fn a_file_replaces_one_job_only_once() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let old = "0b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f";
        let mut j1 = job_line("1b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f", "A", "2026-11-06T12:00:00Z");
        let mut j2 = job_line("2b6f3c1e-5d2a-4c8e-9f10-2a3b4c5d6e7f", "B", "2026-11-06T12:00:00Z");
        j1["replaces"] = json!(old);
        j2["replaces"] = json!(old);
        let e = s.import_preview(&a, &file_of(&[j1, j2]), None, ImportMode::Jobs).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("Line 3"), "{}", e.message);
    }

    #[test]
    fn the_preview_tells_a_manager_nothing_about_a_guessed_login() {
        let (a, pa, b, pb) = pair();
        job(&a, &pa, "Energy", "https://energy.example/");
        let file = a.job_export(&pa, None).unwrap();
        b.import_apply(&pb, &file, None, ImportMode::Jobs, &json!({}), d("2026-11-06")).unwrap();
        let manage = agent(&b, &pb, Some("jobs.manage"));
        let pre = b.import_preview(&manage, &file, None, ImportMode::Jobs).unwrap();
        let fields: Vec<&str> = pre["decide"][0]["fields"].as_array().unwrap().iter().map(|f| f["field"].as_str().unwrap()).collect();
        assert_eq!(fields, ["username", "password"], "equal logins are listed like differing ones");
        // A wrong guess, newer than the login here, is preselected exactly like a right one.
        let g = pre["decide"][0]["guid"].as_str().unwrap();
        let mut guess = job_line(g, "Energy", "2026-11-06T12:00:00Z");
        guess["entry_url"] = json!("https://energy.example/");
        guess["password"] = json!("wrong guess");
        guess["changed"]["password"] = json!("2099-01-01T00:00:00Z");
        let pre = b.import_preview(&manage, &file_of(&[guess]), None, ImportMode::Jobs).unwrap();
        let pw = pre["decide"][0]["fields"].as_array().unwrap().iter().find(|f| f["field"] == "password").unwrap().clone();
        assert_eq!((pw["preselect"].as_str(), pw["incoming"].as_str()), (Some("local"), Some("•••")));
        assert_eq!(b.import_preview(&pb, &file, None, ImportMode::Jobs).unwrap()["same"].as_array().unwrap().len(), 1, "a person sees them equal");
    }

    #[test]
    fn people_are_read_in_only_with_all_three_rights_and_valid_records() {
        let (a, pa, b, pb) = pair();
        let full = a.backup_export(&pa, None).unwrap();
        let two = person(&b, &pb, "Bea", Rights { people: true, backup: true, ..Rights::default() });
        assert_eq!(b.import_preview(&two, &full, None, ImportMode::Full).err().unwrap().kind, "right_missing");
        let head_and_agent = format!("{}\n{}\n", json!({"format": FORMAT, "version": VERSION, "kind": "full"}),
            json!({"type":"agent","token_hash":"a".repeat(64),"display":"abcdefgh","introduction":"Agent of nobody","role":"root","reach":"","granted_by":null,"granted_at":null,"created_at":"2026-11-06T10:00:00Z","last_used_at":null,"revoked_at":null}));
        let e = b.import_preview(&pb, &zip_text(&head_and_agent, None).unwrap(), None, ImportMode::Full).err().unwrap();
        assert!(e.kind == "file_invalid" && e.message.contains("Line 2"), "{}", e.message);
    }

    #[test]
    fn a_password_is_visible_ascii_and_a_file_has_one_entry() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        assert_eq!(s.job_export(&a, Some("Pässwort123")).err().unwrap().kind, "password_invalid");
        assert!(s.job_export(&a, Some("Passwort 123!")).is_ok());
        assert_eq!(s.job_export(&a, Some(" Passwort123")).err().unwrap().kind, "password_invalid", "a browser drops spaces at the ends");
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file(ENTRY, opts).unwrap();
        w.write_all(json!({"format": FORMAT, "version": VERSION, "kind": "jobs"}).to_string().as_bytes()).unwrap();
        w.start_file("other.txt", opts).unwrap();
        w.write_all(b"x").unwrap();
        let two = w.finish().unwrap().into_inner();
        assert_eq!(s.import_preview(&a, &two, None, ImportMode::Jobs).err().unwrap().kind, "file_invalid");
    }

    #[test]
    fn the_backup_file_is_written_once_and_never_overwritten() {
        let s = Store::open_in_memory().unwrap();
        admin(&s);
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("b.zip");
        s.backup_to_file(&f, None).unwrap();
        assert_eq!(s.backup_to_file(&f, None).err().unwrap().kind, "backup_exists");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
