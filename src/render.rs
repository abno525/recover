//! Human-facing rendering: the list table and the `show` detail view.

use crate::model::Run;
use crate::text::decode_lossy_stripped;
use chrono::Local;

/// Format a duration in milliseconds compactly (e.g. `350ms`, `1.2s`, `2m3s`, `1h4m`).
pub fn human_duration(ms: i64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let total_secs = ms / 1000;
    if ms < 3_600_000 {
        return format!("{}m{}s", total_secs / 60, total_secs % 60);
    }
    let total_min = total_secs / 60;
    format!("{}h{}m", total_min / 60, total_min % 60)
}

/// The exit/status cell for a run: exit code, or signal name, or `-`.
pub fn status_cell(run: &Run) -> String {
    if let Some(sig) = &run.signal {
        return sig.clone();
    }
    match run.exit_code {
        Some(code) => code.to_string(),
        None => "-".to_string(),
    }
}

/// Local wall-clock timestamp for a run's start, `YYYY-MM-DD HH:MM`.
pub fn started_local(run: &Run) -> String {
    run.started_at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const FG_BLACK: &str = "\x1b[38;2;0;0;0m";
const FG_WHITE: &str = "\x1b[38;2;255;255;255m";

/// Default zebra-stripe background: a subtle dark blue.
pub const DEFAULT_BAR: Rgb = Rgb { r: 0, g: 0, b: 0x33 };

/// An RGB color parsed from a hex string like `#00005f`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Parse `#rrggbb` or `rrggbb`.
    pub fn parse_hex(s: &str) -> Result<Rgb, String> {
        let t = s.trim();
        let h = t.strip_prefix('#').unwrap_or(t);
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("invalid hex color `{s}` (expected #rrggbb)"));
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
        Ok(Rgb { r: byte(0), g: byte(2), b: byte(4) })
    }

    fn bg(&self) -> String {
        format!("\x1b[48;2;{};{};{}m", self.r, self.g, self.b)
    }
    fn fg(&self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.r, self.g, self.b)
    }

    /// Whether this color is light, so text on top of it should be dark.
    fn is_light(&self) -> bool {
        let l = 0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32;
        l > 140.0
    }
}

/// Colors for the list table (quick mode on a terminal). `header` = None means bold only.
pub struct ListStyle {
    pub bar_bg: Rgb,
    pub header: Option<Rgb>,
}

/// Render a list of runs as an aligned table, newest first.
///
/// With `style` set (quick mode on a terminal), alternate rows get a colored background
/// spanning `width`, and commands are truncated to fit one line. With `None`
/// (standard mode, piped, or `color = off`), output is plain and commands are shown in full.
pub fn format_list(runs: &[Run], style: Option<&ListStyle>, width: usize) -> String {
    let mut out = String::new();
    let header = format!("{:>5}  {:<16}  {:>7}  {:>6}  {}", "ID", "STARTED", "DUR", "EXIT", "COMMAND");
    match style {
        Some(s) => {
            let hc = s.header.map(|c| c.fg()).unwrap_or_default();
            out.push_str(&format!("{BOLD}{hc}{header}{RESET}\n"));
        }
        None => {
            out.push_str(&header);
            out.push('\n');
        }
    }

    for (i, r) in runs.iter().enumerate() {
        let prefix = format!(
            "{:>5}  {:<16}  {:>7}  {:>6}  ",
            r.id,
            started_local(r),
            human_duration(r.duration_ms),
            status_cell(r),
        );
        let cmd = first_line(&r.command);

        let Some(s) = style else {
            out.push_str(&format!("{prefix}{cmd}\n"));
            continue;
        };

        // Never let the command column vanish, even on an implausibly narrow terminal.
        let avail = width.saturating_sub(prefix.chars().count()).max(8);
        let mut body = truncate_chars(cmd, avail);

        // If the run has a comment and space is left after the command, append it.
        if let Some(comment) = r.comment.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            const SEP: &str = "  # ";
            let remaining = avail.saturating_sub(body.chars().count());
            if remaining > SEP.chars().count() + 1 {
                body.push_str(SEP);
                body.push_str(&truncate_chars(comment, remaining - SEP.chars().count()));
            }
        }

        let mut line = format!("{prefix}{body}");
        let len = line.chars().count();
        if len < width {
            line.push_str(&" ".repeat(width - len));
        }
        if i % 2 == 1 {
            let text = if s.bar_bg.is_light() { FG_BLACK } else { FG_WHITE };
            out.push_str(&format!("{}{}{}{}\n", s.bar_bg.bg(), text, line, RESET));
        } else {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Truncate to at most `max` display chars, using `…` as the final char when cut.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

/// Render full detail for a single run (metadata header + output).
pub fn format_show(run: &Run) -> String {
    let mut out = String::new();
    out.push_str(&format!("id:       {}\n", run.id));
    out.push_str(&format!("command:  {}\n", run.command));
    out.push_str(&format!("started:  {}\n", started_local(run)));
    out.push_str(&format!("duration: {}\n", human_duration(run.duration_ms)));
    out.push_str(&format!("status:   {}\n", status_cell(run)));
    if let Some(cwd) = &run.cwd {
        out.push_str(&format!("cwd:      {cwd}\n"));
    }
    if let Some(comment) = &run.comment {
        out.push_str(&format!("comment:  {comment}\n"));
    }
    out.push_str("output:\n");
    out.push_str(&decode_lossy_stripped(&run.output));
    out
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn run_with(duration_ms: i64, exit: Option<i32>, signal: Option<&str>) -> Run {
        Run {
            id: 1,
            command: "x".into(),
            output: Vec::new(),
            exit_code: exit,
            signal: signal.map(|s| s.to_string()),
            started_at: Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap(),
            finished_at: Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap(),
            duration_ms,
            cwd: None,
            host: None,
            shell: None,
            comment: None,
        }
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(human_duration(350), "350ms");
        assert_eq!(human_duration(1200), "1.2s");
        assert_eq!(human_duration(1000), "1.0s");
        assert_eq!(human_duration(123_000), "2m3s");
        assert_eq!(human_duration(3_840_000), "1h4m");
    }

    #[test]
    fn status_shows_exit_signal_or_dash() {
        assert_eq!(status_cell(&run_with(0, Some(0), None)), "0");
        assert_eq!(status_cell(&run_with(0, Some(101), None)), "101");
        assert_eq!(status_cell(&run_with(0, None, Some("Terminated"))), "Terminated");
        assert_eq!(status_cell(&run_with(0, None, None)), "-");
    }

    fn run_n(id: i64, command: &str) -> Run {
        let mut r = run_with(10, Some(0), None);
        r.id = id;
        r.command = command.to_string();
        r
    }

    fn visible_len(line: &str) -> usize {
        String::from_utf8_lossy(&crate::text::strip_ansi(line.as_bytes()))
            .chars()
            .count()
    }

    fn navy() -> ListStyle {
        ListStyle { bar_bg: Rgb { r: 0, g: 0, b: 0x5f }, header: None }
    }

    #[test]
    fn rgb_parses_hex_forms() {
        assert_eq!(Rgb::parse_hex("#00005f").unwrap(), Rgb { r: 0, g: 0, b: 0x5f });
        assert_eq!(Rgb::parse_hex("ff8800").unwrap(), Rgb { r: 255, g: 136, b: 0 });
        assert!(Rgb::parse_hex("#zzzzzz").is_err());
        assert!(Rgb::parse_hex("#fff").is_err()); // 3-digit shorthand not supported
    }

    #[test]
    fn plain_list_has_no_ansi_and_full_command() {
        let out = format_list(&[run_n(1, "echo hello world")], None, 100);
        assert!(!out.contains('\x1b'), "plain output must not contain ANSI");
        assert!(out.contains("echo hello world"));
    }

    #[test]
    fn colored_list_stripes_every_other_row() {
        let runs = [run_n(3, "c"), run_n(2, "b"), run_n(1, "a")];
        let style = navy();
        let out = format_list(&runs, Some(&style), 80);
        assert_eq!(out.matches(&style.bar_bg.bg()).count(), 1, "one of three rows should be striped");
        assert!(out.contains(BOLD), "header should be bold");
    }

    #[test]
    fn colored_list_truncates_to_width() {
        let long = "x".repeat(200);
        let style = navy();
        let out = format_list(&[run_n(1, &long), run_n(2, &long)], Some(&style), 60);
        for line in out.lines() {
            assert!(visible_len(line) <= 60, "line exceeds width: {}", visible_len(line));
        }
    }

    #[test]
    fn colored_list_shows_comment_when_space_allows() {
        let mut r = run_n(1, "echo test");
        r.comment = Some("testing".into());
        let style = navy();
        let out = format_list(&[r], Some(&style), 100);
        assert!(out.contains("# testing"), "should append the comment: {out}");
    }

    #[test]
    fn colored_list_hides_comment_when_command_fills_width() {
        let mut r = run_n(1, &"x".repeat(200));
        r.comment = Some("note".into());
        let style = navy();
        let out = format_list(&[r], Some(&style), 60);
        assert!(!out.contains("# note"), "no room for a comment at width 60");
    }

    #[test]
    fn plain_list_omits_comments() {
        let mut r = run_n(1, "echo test");
        r.comment = Some("testing".into());
        let out = format_list(&[r], None, 100);
        assert!(!out.contains("testing"), "piped/plain output stays command-only");
    }

    #[test]
    fn custom_header_color_appears() {
        let style = ListStyle { bar_bg: Rgb { r: 0, g: 0, b: 0x5f }, header: Some(Rgb { r: 255, g: 0, b: 0 }) };
        let out = format_list(&[run_n(1, "a")], Some(&style), 80);
        assert!(out.contains(&Rgb { r: 255, g: 0, b: 0 }.fg()), "header color should be applied");
    }
}
