//! SQLite persistence for recorded runs.

use crate::model::{Filters, NewRun, Run};
use crate::text::decode_lossy_stripped;
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// Decode a run's stored output for regex matching (ANSI stripped, lossy UTF-8).
fn decoded_output(bytes: &[u8]) -> String {
    decode_lossy_stripped(bytes)
}

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    command     TEXT    NOT NULL,
    output      BLOB    NOT NULL,
    exit_code   INTEGER,
    signal      TEXT,
    started_at  TEXT    NOT NULL,
    finished_at TEXT    NOT NULL,
    duration_ms INTEGER NOT NULL,
    cwd         TEXT,
    host        TEXT,
    shell       TEXT,
    comment     TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at);";

const COLS: &str =
    "id, command, output, exit_code, signal, started_at, finished_at, duration_ms, cwd, host, shell, comment";

/// A handle to the recover database.
pub struct Db {
    conn: Connection,
}

fn fmt_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn ts_from_sql(idx: usize, s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}

fn row_to_run(row: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        command: row.get(1)?,
        output: row.get(2)?,
        exit_code: row.get(3)?,
        signal: row.get(4)?,
        started_at: ts_from_sql(5, &row.get::<_, String>(5)?)?,
        finished_at: ts_from_sql(6, &row.get::<_, String>(6)?)?,
        duration_ms: row.get(7)?,
        cwd: row.get(8)?,
        host: row.get(9)?,
        shell: row.get(10)?,
        comment: row.get(11)?,
    })
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    // Add columns introduced after the initial schema to pre-existing databases.
    if !column_exists(conn, "runs", "comment")? {
        conn.execute("ALTER TABLE runs ADD COLUMN comment TEXT", [])?;
    }
    Ok(())
}

impl Db {
    /// Open (creating if needed) the database at `path` and apply migrations.
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)?;
        migrate(&conn)?;
        Ok(Db { conn })
    }

    /// Open an ephemeral in-memory database (for tests).
    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn)?;
        Ok(Db { conn })
    }

    /// Insert a run, returning its assigned id.
    pub fn insert(&self, run: &NewRun) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs \
             (command, output, exit_code, signal, started_at, finished_at, duration_ms, cwd, host, shell, comment) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.command,
                run.output,
                run.exit_code,
                run.signal,
                fmt_ts(run.started_at),
                fmt_ts(run.finished_at),
                run.duration_ms,
                run.cwd,
                run.host,
                run.shell,
                run.comment,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fetch a run by id.
    pub fn get(&self, id: i64) -> Result<Option<Run>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {COLS} FROM runs WHERE id = ?1"))?;
        let run = stmt.query_row([id], row_to_run).optional()?;
        Ok(run)
    }

    /// Set (or clear, with `None`) a run's comment. Returns true if the run existed.
    pub fn set_comment(&self, id: i64, comment: Option<&str>) -> Result<bool> {
        let n = self
            .conn
            .execute("UPDATE runs SET comment = ?1 WHERE id = ?2", params![comment, id])?;
        Ok(n > 0)
    }

    /// Resolve a possibly-negative index to a concrete run id.
    /// A positive value is the id itself; `-k` is the k-th most recent run (`-1` = newest).
    pub fn resolve_id(&self, idx: i64) -> Result<i64> {
        if idx > 0 {
            return Ok(idx);
        }
        if idx == 0 {
            anyhow::bail!("index 0 is not valid; use a positive id, or -1 for the most recent run");
        }
        let offset = -idx - 1; // -1 -> newest (offset 0)
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM runs ORDER BY id DESC LIMIT 1 OFFSET ?1")?;
        match stmt.query_row([offset], |r| r.get::<_, i64>(0)).optional()? {
            Some(id) => Ok(id),
            None => {
                let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))?;
                anyhow::bail!("no run at index {idx} (history has {n} runs)")
            }
        }
    }

    /// Query runs matching `filters`, newest first.
    ///
    /// Cheap predicates (date bounds, exit code) run in SQL; regex predicates run in Rust
    /// against the command and ANSI-stripped output, so the `limit` is applied last.
    pub fn query(&self, filters: &Filters) -> Result<Vec<Run>> {
        use rusqlite::types::Value;

        let mut clauses: Vec<&str> = Vec::new();
        let mut vals: Vec<Value> = Vec::new();
        if let Some(after) = filters.after {
            clauses.push("started_at >= ?");
            vals.push(Value::Text(fmt_ts(after)));
        }
        if let Some(before) = filters.before {
            clauses.push("started_at < ?");
            vals.push(Value::Text(fmt_ts(before)));
        }
        if let Some(exit) = filters.exit {
            clauses.push("exit_code = ?");
            vals.push(Value::Integer(exit as i64));
        }

        let mut sql = format!("SELECT {COLS} FROM runs");
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY id DESC");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(vals), row_to_run)?;
        let mut runs: Vec<Run> = rows.collect::<rusqlite::Result<_>>()?;

        let cmd_re = filters.cmd_re.as_deref().map(Regex::new).transpose()?;
        let out_re = filters.out_re.as_deref().map(Regex::new).transpose()?;
        let reg_re = filters.reg_re.as_deref().map(Regex::new).transpose()?;
        let com_re = filters.com_re.as_deref().map(Regex::new).transpose()?;

        if cmd_re.is_some() || out_re.is_some() || reg_re.is_some() || com_re.is_some() {
            runs.retain(|run| {
                let decoded = || decoded_output(&run.output);
                if let Some(re) = &cmd_re
                    && !re.is_match(&run.command)
                {
                    return false;
                }
                if let Some(re) = &out_re
                    && !re.is_match(&decoded())
                {
                    return false;
                }
                if let Some(re) = &reg_re
                    && !re.is_match(&run.command)
                    && !re.is_match(&decoded())
                {
                    return false;
                }
                if let Some(re) = &com_re
                    && !re.is_match(run.comment.as_deref().unwrap_or(""))
                {
                    return false;
                }
                true
            });
        }

        match filters.limit {
            None | Some(0) => {}
            Some(n) => runs.truncate(n),
        }
        Ok(runs)
    }

    /// Delete a run by id; returns true if a row was removed.
    pub fn delete(&self, id: i64) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM runs WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    /// Delete all runs; returns the number removed.
    pub fn clear(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM runs", [])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn sample() -> NewRun {
        NewRun {
            command: "echo hello".to_string(),
            // deliberately includes a non-UTF-8 byte (0xFF) to prove binary safety
            output: vec![b'h', b'i', 0xFF, b'\n'],
            exit_code: Some(0),
            signal: None,
            started_at: Utc.with_ymd_and_hms(2026, 9, 19, 14, 30, 0).unwrap(),
            finished_at: Utc.with_ymd_and_hms(2026, 9, 19, 14, 30, 1).unwrap(),
            duration_ms: 1000,
            cwd: Some("/tmp".to_string()),
            host: Some("cave".to_string()),
            shell: Some("/bin/bash".to_string()),
            comment: None,
        }
    }

    fn mk(command: &str, output: &[u8], exit: i32, day: u32) -> NewRun {
        let start = Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap();
        NewRun {
            command: command.to_string(),
            output: output.to_vec(),
            exit_code: Some(exit),
            signal: None,
            started_at: start,
            finished_at: start,
            duration_ms: 0,
            cwd: None,
            host: None,
            shell: None,
            comment: None,
        }
    }

    /// Three rows: (1) git status ok Sep10, (2) cargo build panic Sep15, (3) git push ok Sep18.
    fn seeded() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.insert(&mk("git status", b"clean", 0, 10)).unwrap();
        db.insert(&mk("cargo build", b"error: panic here", 101, 15)).unwrap();
        db.insert(&mk("git push", b"done", 0, 18)).unwrap();
        db
    }

    fn commands(runs: &[Run]) -> Vec<&str> {
        runs.iter().map(|r| r.command.as_str()).collect()
    }

    #[test]
    fn query_no_filters_returns_newest_first() {
        let runs = seeded().query(&Filters::default()).unwrap();
        assert_eq!(commands(&runs), vec!["git push", "cargo build", "git status"]);
    }

    #[test]
    fn query_cmd_regex_matches_command_only() {
        let f = Filters { cmd_re: Some("git".into()), ..Default::default() };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["git push", "git status"]);
    }

    #[test]
    fn query_out_regex_matches_output_only() {
        let f = Filters { out_re: Some("panic".into()), ..Default::default() };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["cargo build"]);
    }

    #[test]
    fn query_reg_regex_matches_command_or_output() {
        let f = Filters { reg_re: Some("done|panic".into()), ..Default::default() };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["git push", "cargo build"]);
    }

    #[test]
    fn query_exit_filter() {
        let f = Filters { exit: Some(0), ..Default::default() };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["git push", "git status"]);
    }

    #[test]
    fn query_date_window_after() {
        let f = Filters {
            after: Some(Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0).unwrap()),
            ..Default::default()
        };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["git push", "cargo build"]);
    }

    #[test]
    fn query_date_range_is_half_open() {
        let f = Filters {
            after: Some(Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0).unwrap()),
            before: Some(Utc.with_ymd_and_hms(2026, 9, 16, 0, 0, 0).unwrap()),
            ..Default::default()
        };
        assert_eq!(commands(&seeded().query(&f).unwrap()), vec!["cargo build"]);
    }

    #[test]
    fn query_limit_caps_after_ordering_and_zero_means_all() {
        let two = Filters { limit: Some(2), ..Default::default() };
        assert_eq!(commands(&seeded().query(&two).unwrap()), vec!["git push", "cargo build"]);
        let all = Filters { limit: Some(0), ..Default::default() };
        assert_eq!(seeded().query(&all).unwrap().len(), 3);
    }

    #[test]
    fn query_strips_ansi_before_matching_output() {
        let db = Db::open_in_memory().unwrap();
        // "\x1b[31mred\x1b[0m" — "red" wrapped in ANSI color codes
        db.insert(&mk("colorcmd", b"\x1b[31mred\x1b[0m", 0, 12)).unwrap();
        let f = Filters { out_re: Some("^red$".into()), ..Default::default() };
        assert_eq!(commands(&db.query(&f).unwrap()), vec!["colorcmd"]);
    }

    #[test]
    fn set_get_and_clear_comment() {
        let db = seeded();
        assert!(db.set_comment(2, Some("important build")).unwrap());
        assert_eq!(db.get(2).unwrap().unwrap().comment.as_deref(), Some("important build"));
        assert!(db.set_comment(2, None).unwrap());
        assert_eq!(db.get(2).unwrap().unwrap().comment, None);
        assert!(!db.set_comment(999, Some("x")).unwrap()); // missing run
    }

    #[test]
    fn query_com_regex_matches_comment() {
        let db = seeded();
        db.set_comment(1, Some("keep: prod deploy")).unwrap();
        db.set_comment(3, Some("scratch")).unwrap();
        let f = Filters { com_re: Some("^keep:".into()), ..Default::default() };
        assert_eq!(commands(&db.query(&f).unwrap()), vec!["git status"]);
    }

    #[test]
    fn opens_and_migrates_a_pre_comment_database() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("recover-migrate-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Build an "old" database that lacks the comment column.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    command TEXT NOT NULL, output BLOB NOT NULL,
                    exit_code INTEGER, signal TEXT,
                    started_at TEXT NOT NULL, finished_at TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL, cwd TEXT, host TEXT, shell TEXT);
                 INSERT INTO runs (command, output, started_at, finished_at, duration_ms)
                 VALUES ('legacy cmd', X'6f6b', '2026-09-01T00:00:00.000Z', '2026-09-01T00:00:00.000Z', 1);",
            )
            .unwrap();
        }
        // Opening runs the migration; the row is readable and the new column is usable.
        let db = Db::open(&path).unwrap();
        let run = db.get(1).unwrap().unwrap();
        assert_eq!(run.command, "legacy cmd");
        assert_eq!(run.comment, None);
        assert!(db.set_comment(1, Some("annotated later")).unwrap());
        assert_eq!(db.get(1).unwrap().unwrap().comment.as_deref(), Some("annotated later"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_id_handles_positive_and_negative() {
        let db = seeded(); // ids 1,2,3 (git status, cargo build, git push)
        assert_eq!(db.resolve_id(2).unwrap(), 2); // positive = literal id
        assert_eq!(db.resolve_id(-1).unwrap(), 3); // newest
        assert_eq!(db.resolve_id(-2).unwrap(), 2);
        assert_eq!(db.resolve_id(-3).unwrap(), 1); // oldest
        assert!(db.resolve_id(-4).is_err()); // past the start of history
        assert!(db.resolve_id(0).is_err());
    }

    #[test]
    fn delete_and_clear() {
        let db = seeded();
        assert!(db.delete(2).unwrap());
        assert!(!db.delete(2).unwrap());
        assert!(db.get(2).unwrap().is_none());
        assert_eq!(db.clear().unwrap(), 2);
        assert_eq!(db.query(&Filters::default()).unwrap().len(), 0);
    }

    #[test]
    fn insert_then_get_round_trips_including_non_utf8_output() {
        let db = Db::open_in_memory().unwrap();
        let id = db.insert(&sample()).unwrap();
        let got = db.get(id).unwrap().expect("row should exist");

        assert_eq!(got.id, id);
        assert_eq!(got.command, "echo hello");
        assert_eq!(got.output, vec![b'h', b'i', 0xFF, b'\n']);
        assert_eq!(got.exit_code, Some(0));
        assert_eq!(got.signal, None);
        assert_eq!(got.started_at, sample().started_at);
        assert_eq!(got.duration_ms, 1000);
        assert_eq!(got.cwd.as_deref(), Some("/tmp"));
    }
}
