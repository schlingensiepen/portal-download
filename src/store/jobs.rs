//! Jobs and their runs.
//!
//! Every job carries a GUID that identifies it across instances, and for every field the
//! time it was last changed, so that two copies of a job can be merged field by field. The
//! entry address belongs to the GUID: a new address gives the job a new GUID that names the
//! old one in `replaces`, and the old GUID is kept as deleted and replaced.

use super::{check_login, new_guid, now, parse_date, render_markdown, text, Cap, Principal, Store};
use crate::error::{OpError, OpResult};
use crate::schedule::{self, Outcome};
use chrono::NaiveDate;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct JobInput {
    pub title: Option<String>,
    pub entry_url: Option<String>,
    pub interval_weeks: Option<u32>,
    pub retry_days: Option<u32>,
    pub next_due: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub instructions_md: Option<String>,
}

/// A job as stored, with the time each field was last changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stored {
    pub id: i64,
    pub guid: String,
    pub replaces: Option<String>,
    pub title: String,
    pub entry_url: String,
    pub interval_weeks: u32,
    pub retry_days: u32,
    pub next_due: String,
    pub username: String,
    pub password: String,
    pub instructions_md: String,
    pub created_at: String,
    pub title_at: String,
    pub interval_weeks_at: String,
    pub retry_days_at: String,
    pub next_due_at: String,
    pub username_at: String,
    pub password_at: String,
    pub instructions_md_at: String,
}

pub(super) const STORED_COLS: &str = "id, guid, replaces, title, entry_url, interval_weeks, retry_days, next_due, username, password, instructions_md, created_at, \
     title_at, interval_weeks_at, retry_days_at, next_due_at, username_at, password_at, instructions_md_at";

pub(super) fn stored_row(r: &rusqlite::Row) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: r.get(0)?,
        guid: r.get(1)?,
        replaces: r.get(2)?,
        title: r.get(3)?,
        entry_url: r.get(4)?,
        interval_weeks: r.get(5)?,
        retry_days: r.get(6)?,
        next_due: r.get(7)?,
        username: r.get(8)?,
        password: r.get(9)?,
        instructions_md: r.get(10)?,
        created_at: r.get(11)?,
        title_at: r.get(12)?,
        interval_weeks_at: r.get(13)?,
        retry_days_at: r.get(14)?,
        next_due_at: r.get(15)?,
        username_at: r.get(16)?,
        password_at: r.get(17)?,
        instructions_md_at: r.get(18)?,
    })
}

/// A job as a caller sees it.
#[derive(Debug, Serialize, Clone)]
pub struct Job {
    pub job_id: i64,
    pub guid: String,
    pub replaces: Option<String>,
    pub title: String,
    pub entry_url: String,
    pub interval_weeks: u32,
    pub retry_days: u32,
    pub next_due: String,
    pub urgency_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub instructions_md: String,
    pub instructions_html: String,
    pub last_run: Option<Value>,
}

pub(super) fn check_url(u: &str) -> OpResult<String> {
    let u = u.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 2000 || u.contains(char::is_whitespace) {
        return Err(OpError::bad("entry_url_invalid", "`entry_url` must be a web address starting with https:// (or http://), without spaces. Copy it from the browser's address bar."));
    }
    Ok(u.to_string())
}

pub(super) fn validate_job(input: &JobInput, creating: bool) -> OpResult<()> {
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
        check_url(u)?;
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
    if let Some(s) = &input.username {
        check_login("username", s)?;
    }
    if let Some(s) = &input.password {
        check_login("password", s)?;
    }
    if let Some(m) = &input.instructions_md {
        if m.len() > 65536 {
            return Err(OpError::bad("instructions_too_long", "`instructions_md` may hold at most 64 KiB of text. Shorten it."));
        }
    }
    Ok(())
}

impl Store {
    pub(super) fn stored_by_id(&self, job_id: i64) -> OpResult<Stored> {
        self.conn
            .query_row(&format!("SELECT {STORED_COLS} FROM jobs WHERE id = ?1"), [job_id], stored_row)
            .optional()?
            .ok_or_else(|| OpError::not_found("job_unknown", format!("There is no job {job_id}. Call job.list for the valid identifiers.")))
    }

    fn last_run(&self, job_id: i64) -> OpResult<Option<Value>> {
        Ok(self
            .conn
            .query_row(
                "SELECT outcome, finished_at, note, by_name FROM runs WHERE job_id = ?1 ORDER BY finished_at DESC, id DESC LIMIT 1",
                [job_id],
                |r| Ok(json!({ "outcome": r.get::<_, String>(0)?, "finished_at": r.get::<_, String>(1)?, "note": r.get::<_, String>(2)?, "by": r.get::<_, String>(3)? })),
            )
            .optional()?)
    }

    fn view(&self, s: Stored, today: NaiveDate, with_logins: bool) -> OpResult<Job> {
        let due = NaiveDate::parse_from_str(&s.next_due, "%Y-%m-%d").unwrap_or(today);
        Ok(Job {
            last_run: self.last_run(s.id)?,
            job_id: s.id,
            guid: s.guid,
            replaces: s.replaces,
            title: s.title,
            entry_url: s.entry_url,
            interval_weeks: s.interval_weeks,
            retry_days: s.retry_days,
            urgency_days: schedule::urgency_days(today, due),
            next_due: s.next_due,
            username: with_logins.then_some(s.username),
            password: with_logins.then_some(s.password),
            instructions_html: render_markdown(&s.instructions_md),
            instructions_md: s.instructions_md,
        })
    }

    /// A job as the caller may see it: an agent receives the logins only with jobs.work or jobs.all.
    pub(super) fn job_for(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        let s = self.stored_by_id(job_id)?;
        Ok(serde_json::to_value(self.view(s, today, p.sees_logins())?).expect("job serializes"))
    }

    /// All jobs, most urgent first (smallest number of days to the next due date).
    pub fn job_list(&self, p: &Principal, today: NaiveDate) -> OpResult<Value> {
        p.require(Cap::ReadJobs)?;
        let mut st = self.conn.prepare(&format!("SELECT {STORED_COLS} FROM jobs ORDER BY next_due, title COLLATE NOCASE, id"))?;
        let rows: Vec<Stored> = st.query_map([], stored_row)?.collect::<Result<_, _>>()?;
        let jobs = rows.into_iter().map(|s| self.view(s, today, p.sees_logins())).collect::<OpResult<Vec<_>>>()?;
        Ok(json!({ "today": today.format("%Y-%m-%d").to_string(), "jobs": jobs }))
    }

    pub fn job_get(&self, p: &Principal, job_id: i64, today: NaiveDate) -> OpResult<Value> {
        p.require(Cap::ReadJobs)?;
        self.job_for(p, job_id, today)
    }

    pub fn job_create(&self, p: &Principal, input: JobInput, today: NaiveDate) -> OpResult<Value> {
        p.require(Cap::WriteJobs)?;
        validate_job(&input, true)?;
        let ts = now();
        self.conn.execute(
            "INSERT INTO jobs(guid, title, entry_url, interval_weeks, retry_days, next_due, username, password, instructions_md, created_at,
               title_at, interval_weeks_at, retry_days_at, next_due_at, username_at, password_at, instructions_md_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,?10,?10,?10,?10,?10,?10)",
            params![
                new_guid(),
                input.title.unwrap().trim(),
                check_url(&input.entry_url.unwrap())?,
                input.interval_weeks.unwrap(),
                input.retry_days.unwrap_or(2),
                input.next_due.unwrap_or_else(|| today.format("%Y-%m-%d").to_string()),
                input.username.unwrap_or_default(),
                input.password.unwrap_or_default(),
                input.instructions_md.unwrap_or_default(),
                ts
            ],
        )?;
        self.job_for(p, self.conn.last_insert_rowid(), today)
    }

    /// Changes the fields given; each changed field records when. A new entry address gives
    /// the job a new GUID that names the old one, which is kept as replaced.
    pub fn job_update(&self, p: &Principal, job_id: i64, input: JobInput, today: NaiveDate) -> OpResult<Value> {
        p.require(Cap::WriteJobs)?;
        validate_job(&input, false)?;
        let cur = self.stored_by_id(job_id)?;
        let ts = now();
        let tx = self.conn.unchecked_transaction()?;
        macro_rules! field {
            ($name:ident, $value:expr) => {
                if let Some(v) = $value {
                    if v != cur.$name {
                        tx.execute(
                            &format!("UPDATE jobs SET {0} = ?1, {0}_at = ?2 WHERE id = ?3", stringify!($name)),
                            params![v, ts, job_id],
                        )?;
                    }
                }
            };
        }
        field!(title, input.title.map(|s| s.trim().to_string()));
        field!(interval_weeks, input.interval_weeks);
        field!(retry_days, input.retry_days);
        field!(next_due, input.next_due);
        field!(username, input.username);
        field!(password, input.password);
        field!(instructions_md, input.instructions_md);
        if let Some(u) = input.entry_url {
            let u = check_url(&u)?;
            if u != cur.entry_url {
                let guid = new_guid();
                tx.execute(
                    "INSERT OR REPLACE INTO deleted_jobs(guid, title, entry_url, deleted_at, replaced_by) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![cur.guid, cur.title, cur.entry_url, ts, guid],
                )?;
                tx.execute("UPDATE jobs SET entry_url = ?1, guid = ?2, replaces = ?3 WHERE id = ?4", params![u, guid, cur.guid, job_id])?;
            }
        }
        tx.commit()?;
        self.job_for(p, job_id, today)
    }

    /// Deletes a job with its runs; its GUID is kept, so that an import does not bring it
    /// back unasked.
    pub fn job_delete(&self, p: &Principal, job_id: i64) -> OpResult<Value> {
        p.require(Cap::WriteJobs)?;
        let cur = self.stored_by_id(job_id)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO deleted_jobs(guid, title, entry_url, deleted_at, replaced_by) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![cur.guid, cur.title, cur.entry_url, now()],
        )?;
        tx.execute("DELETE FROM jobs WHERE id = ?1", [job_id])?;
        tx.commit()?;
        Ok(json!({ "job_id": job_id, "guid": cur.guid, "deleted": true }))
    }

    /// Closes a run: records it and moves the next due date according to the outcome. With
    /// `expected_next_due`, a run somebody else closed meanwhile is noticed.
    pub fn job_complete(&self, p: &Principal, job_id: i64, outcome: &str, note: &str, expected_next_due: Option<&str>, today: NaiveDate) -> OpResult<Value> {
        p.require(Cap::Complete)?;
        let outcome = Outcome::parse(outcome)
            .ok_or_else(|| OpError::bad("outcome_invalid", format!("`outcome` = \"{outcome}\" is not possible. Send \"succeeded\" when everything was filed, \"failed\" otherwise.")))?;
        let note = text("note", note, 0, 2000)?;
        let job = self.stored_by_id(job_id)?;
        if let Some(exp) = expected_next_due {
            if exp != job.next_due {
                return Err(OpError::conflict(
                    "job_changed",
                    format!("This job is now due on {}, not on {exp}: somebody else has closed a run or changed it meanwhile. Reload it and check whether the run is still needed.", job.next_due),
                ));
            }
        }
        let due_after = schedule::next_due(today, outcome, job.interval_weeks, job.retry_days).format("%Y-%m-%d").to_string();
        let ts = now();
        let run_guid = new_guid();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs(guid, job_id, by_name, outcome, note, finished_at, due_before, due_after) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![run_guid, job_id, p.by_name(), outcome.as_str(), note, ts, job.next_due, due_after],
        )?;
        tx.execute("UPDATE jobs SET next_due = ?1, next_due_at = ?2 WHERE id = ?3", params![due_after, ts, job_id])?;
        tx.commit()?;
        Ok(json!({ "job_id": job_id, "run_guid": run_guid, "outcome": outcome.as_str(), "due_before": job.next_due, "next_due": due_after }))
    }

    pub fn job_history(&self, p: &Principal, job_id: i64) -> OpResult<Value> {
        p.require(Cap::ReadJobs)?;
        self.stored_by_id(job_id)?;
        let mut st = self.conn.prepare(
            "SELECT guid, outcome, note, finished_at, due_before, due_after, by_name FROM runs WHERE job_id = ?1 ORDER BY finished_at DESC, id DESC LIMIT 200",
        )?;
        let runs: Vec<Value> = st
            .query_map([job_id], |r| {
                Ok(json!({
                    "run_guid": r.get::<_, String>(0)?, "outcome": r.get::<_, String>(1)?, "note": r.get::<_, String>(2)?,
                    "finished_at": r.get::<_, String>(3)?, "due_before": r.get::<_, String>(4)?, "due_after": r.get::<_, String>(5)?, "by": r.get::<_, String>(6)?,
                }))
            })?
            .collect::<Result<_, _>>()?;
        Ok(json!({ "job_id": job_id, "runs": runs }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{admin, agent, person};
    use super::super::Rights;
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn input(title: &str, url: &str) -> JobInput {
        JobInput { title: Some(title.into()), entry_url: Some(url.into()), interval_weeks: Some(4), ..Default::default() }
    }

    #[test]
    fn jobs_are_listed_most_urgent_first_and_completion_moves_the_due_date() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let today = d("2026-11-06");
        let mut late = input("Late", "https://a.example/");
        late.next_due = Some("2026-11-01".into());
        let late = s.job_create(&a, late, today).unwrap()["job_id"].as_i64().unwrap();
        s.job_create(&a, input("Now", "https://b.example/"), today).unwrap();
        let list = s.job_list(&a, today).unwrap();
        assert_eq!(list["jobs"][0]["title"], "Late");
        assert_eq!(list["jobs"][0]["urgency_days"], -5);
        let r = s.job_complete(&a, late, "succeeded", "all filed", Some("2026-11-01"), today).unwrap();
        assert_eq!(r["next_due"], "2026-12-04");
        assert_eq!(s.job_list(&a, today).unwrap()["jobs"][1]["last_run"]["note"], "all filed");
        // Somebody who still sees the old date is told.
        assert_eq!(s.job_complete(&a, late, "failed", "", Some("2026-11-01"), today).err().unwrap().kind, "job_changed");
        let r = s.job_complete(&a, late, "failed", "", None, today).unwrap();
        assert_eq!(r["next_due"], "2026-11-08");
        assert_eq!(s.job_history(&a, late).unwrap()["runs"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_new_address_gives_a_new_guid_that_names_the_old_one() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let today = d("2026-11-06");
        let j = s.job_create(&a, input("Energy", "https://old.example/"), today).unwrap();
        let id = j["job_id"].as_i64().unwrap();
        let old = j["guid"].as_str().unwrap().to_string();
        s.job_complete(&a, id, "succeeded", "", None, today).unwrap();
        // Another field keeps the GUID.
        let j = s.job_update(&a, id, JobInput { title: Some("Energy invoice".into()), ..Default::default() }, today).unwrap();
        assert_eq!(j["guid"], old);
        let j = s.job_update(&a, id, JobInput { entry_url: Some("https://new.example/".into()), ..Default::default() }, today).unwrap();
        assert_ne!(j["guid"], old);
        assert_eq!(j["replaces"], old);
        assert_eq!(j["job_id"], id, "the job itself stays");
        assert_eq!(s.job_history(&a, id).unwrap()["runs"].as_array().unwrap().len(), 1, "its runs stay");
        let replaced_by: String = s.conn.query_row("SELECT replaced_by FROM deleted_jobs WHERE guid = ?1", [&old], |r| r.get(0)).unwrap();
        assert_eq!(replaced_by, j["guid"].as_str().unwrap());
        // The same address again changes nothing.
        let k = s.job_update(&a, id, JobInput { entry_url: Some(" https://new.example/ ".into()), ..Default::default() }, today).unwrap();
        assert_eq!(k["guid"], j["guid"]);
    }

    #[test]
    fn each_changed_field_records_when_and_unchanged_ones_keep_their_time() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let today = d("2026-11-06");
        let id = s.job_create(&a, input("Energy", "https://a.example/"), today).unwrap()["job_id"].as_i64().unwrap();
        s.conn.execute("UPDATE jobs SET title_at = '2001-01-01T00:00:00Z', password_at = '2001-01-01T00:00:00Z' WHERE id = ?1", [id]).unwrap();
        s.job_update(&a, id, JobInput { password: Some("new".into()), title: Some("Energy".into()), ..Default::default() }, today).unwrap();
        let st = s.stored_by_id(id).unwrap();
        assert_eq!(st.title_at, "2001-01-01T00:00:00Z", "the same title is no change");
        assert_ne!(st.password_at, "2001-01-01T00:00:00Z");
    }

    #[test]
    fn a_deleted_job_keeps_its_guid() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let j = s.job_create(&a, input("Energy", "https://a.example/"), d("2026-11-06")).unwrap();
        let r = s.job_delete(&a, j["job_id"].as_i64().unwrap()).unwrap();
        let n: i64 = s.conn.query_row("SELECT COUNT(*) FROM deleted_jobs WHERE guid = ?1 AND replaced_by IS NULL", [r["guid"].as_str().unwrap()], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_login_in_two_lines_is_refused() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let mut i = input("Energy", "https://a.example/");
        i.password = Some("pw\nsecond".into());
        assert_eq!(s.job_create(&a, i, d("2026-11-06")).err().unwrap().kind, "login_invalid");
    }

    #[test]
    fn every_person_sees_the_login_as_entered_and_a_change_applies_for_all() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let b = person(&s, &a, "Bob", Rights::default());
        let mut i = input("Energy", "https://a.example/");
        i.username = Some(" ada ".into());
        i.password = Some("s3cret ".into());
        let id = s.job_create(&a, i, d("2026-11-06")).unwrap()["job_id"].as_i64().unwrap();
        let j = s.job_get(&b, id, d("2026-11-06")).unwrap();
        assert_eq!((j["username"].as_str(), j["password"].as_str()), (Some(" ada "), Some("s3cret ")));
    }

    #[test]
    fn each_role_reads_and_changes_what_it_permits() {
        let s = Store::open_in_memory().unwrap();
        let a = admin(&s);
        let today = d("2026-11-06");
        let mut i = input("Energy", "https://a.example/");
        i.password = Some("pw".into());
        let id = s.job_create(&a, i, today).unwrap()["job_id"].as_i64().unwrap();
        let read = agent(&s, &a, Some("jobs.read"));
        let work = agent(&s, &a, Some("jobs.work"));
        let manage = agent(&s, &a, Some("jobs.manage"));
        let all = agent(&s, &a, Some("jobs.all"));
        assert!(s.job_get(&read, id, today).unwrap().get("password").is_none());
        assert_eq!(s.job_get(&work, id, today).unwrap()["password"], "pw");
        assert!(s.job_get(&manage, id, today).unwrap().get("password").is_none());
        assert_eq!(s.job_get(&all, id, today).unwrap()["password"], "pw");
        let upd = || JobInput { title: Some("x".into()), ..Default::default() };
        assert_eq!(s.job_update(&read, id, upd(), today).err().unwrap().kind, "right_missing");
        assert_eq!(s.job_update(&work, id, upd(), today).err().unwrap().kind, "right_missing");
        assert!(s.job_update(&manage, id, upd(), today).unwrap().get("password").is_none(), "manage sets but does not read logins");
        assert!(s.job_update(&all, id, upd(), today).is_ok());
        assert_eq!(s.job_complete(&read, id, "succeeded", "", None, today).err().unwrap().kind, "right_missing");
        for p in [&work, &manage, &all] {
            assert!(s.job_complete(p, id, "succeeded", "", None, today).is_ok());
        }
        let backup = agent(&s, &a, Some("backup.read"));
        assert_eq!(s.job_list(&backup, today).err().unwrap().kind, "right_missing");
        let runs = s.job_history(&a, id).unwrap();
        assert!(runs["runs"][0]["by"].as_str().unwrap().starts_with("Agent "));
    }
}
