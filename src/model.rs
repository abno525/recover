//! Core data types shared across modules.

use chrono::{DateTime, Utc};

/// A single recorded command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub command: String,
    pub output: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub shell: Option<String>,
    pub comment: Option<String>,
}

/// A run ready to be inserted — same as [`Run`] but without an assigned `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRun {
    pub command: String,
    pub output: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub shell: Option<String>,
    pub comment: Option<String>,
}

/// Parsed query filters. Regex fields hold the raw pattern source (compiled at match time).
/// Date bounds are half-open: `after` is inclusive (`>=`), `before` is exclusive (`<`).
/// `limit`: `None` = unspecified (caller applies default), `Some(0)` = no cap, `Some(n)` = n.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub cmd_re: Option<String>,
    pub out_re: Option<String>,
    pub reg_re: Option<String>,
    pub com_re: Option<String>,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub exit: Option<i32>,
    pub limit: Option<usize>,
}
