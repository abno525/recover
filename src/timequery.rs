//! Parse taskwarrior-style date expressions into UTC instants.
//!
//! Grammar: `<anchor>` optionally followed by one arithmetic term `<±><N><unit>`.
//! Anchors: `now`, `today`, `yesterday`, `tomorrow`, `YYYY-MM-DD`, RFC3339 datetime.
//! Units: `s/sec`, `m/min`, `h/hour`, `d/day`, `w/week`, `mon/month`, `y/year`.

use chrono::{DateTime, Duration, Months, NaiveDate, Utc};
use regex::Regex;
use std::sync::OnceLock;

/// Matches an optional trailing arithmetic term: `<anchor> <±> <N> <unit>`.
/// Non-greedy anchor so ISO dates (`2026-09-01`) keep their internal dashes.
fn term_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(.*?)\s*([+-])\s*(\d+)\s*([a-z]+)\s*$").unwrap())
}

/// Matches a bare duration like `3day` or `1week` (no anchor, no sign).
fn bare_dur_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(\d+)\s*([a-z]+)$").unwrap())
}

/// Default anchor for a duration whose anchor was omitted: sub-day units count from
/// `now`, larger units from `today` (midnight). So `2h` == `now-2h`, `3day` == `today-3day`.
fn default_anchor_for_unit(unit: &str) -> &'static str {
    match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" | "m" | "min" | "mins" | "minute"
        | "minutes" | "h" | "hr" | "hrs" | "hour" | "hours" => "now",
        _ => "today",
    }
}

/// Parse a date expression into a UTC instant, relative to `now`.
///
/// When `shortcuts` is true, a bare duration (`3day`) or a leading-sign duration (`-3day`)
/// is shorthand for the default anchor minus/plus that duration (`date:3day` ==
/// `date:today-3day`; `date:2h` == `date:now-2h`). When false (standard mode), only
/// explicit anchors are accepted.
pub fn parse_instant(expr: &str, now: DateTime<Utc>, shortcuts: bool) -> Result<DateTime<Utc>, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("empty date expression".into());
    }
    if let Some(c) = term_re().captures(expr) {
        let anchor = c.get(1).unwrap().as_str();
        let unit = c.get(4).unwrap().as_str();
        let base = if anchor.is_empty() {
            if !shortcuts {
                return Err(format!("date `{expr}` needs an explicit anchor in standard mode (e.g. today{expr})"));
            }
            parse_anchor(default_anchor_for_unit(unit), now)?
        } else {
            parse_anchor(anchor, now)?
        };
        let sign: i64 = if &c[2] == "-" { -1 } else { 1 };
        let n: i64 = c[3].parse().map_err(|_| format!("number too large: {}", &c[3]))?;
        apply_term(base, sign, n, unit)
    } else if let Some(c) = bare_dur_re().captures(expr) {
        if !shortcuts {
            return Err(format!("bare duration `{expr}` needs quick mode; use e.g. today-{expr}"));
        }
        let n: i64 = c[1].parse().map_err(|_| format!("number too large: {}", &c[1]))?;
        let unit = c.get(2).unwrap().as_str();
        let base = parse_anchor(default_anchor_for_unit(unit), now)?;
        apply_term(base, -1, n, unit)
    } else {
        parse_anchor(expr, now)
    }
}

fn parse_anchor(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let midnight = |d: DateTime<Utc>| d.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    match s {
        "now" => Ok(now),
        "today" => Ok(midnight(now)),
        "yesterday" => Ok(midnight(now) - Duration::days(1)),
        "tomorrow" => Ok(midnight(now) + Duration::days(1)),
        _ => {
            if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return Ok(d.and_hms_opt(0, 0, 0).unwrap().and_utc());
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Ok(dt.with_timezone(&Utc));
            }
            Err(format!("unrecognized date anchor: {s}"))
        }
    }
}

fn apply_term(base: DateTime<Utc>, sign: i64, n: i64, unit: &str) -> Result<DateTime<Utc>, String> {
    let signed = sign * n;
    let unit = unit.to_ascii_lowercase();
    let dur = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(Duration::seconds(signed)),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(Duration::minutes(signed)),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(Duration::hours(signed)),
        "d" | "day" | "days" => Some(Duration::days(signed)),
        "w" | "wk" | "wks" | "week" | "weeks" => Some(Duration::weeks(signed)),
        _ => None,
    };
    if let Some(dur) = dur {
        return base
            .checked_add_signed(dur)
            .ok_or_else(|| "date arithmetic overflow".to_string());
    }
    let months = match unit.as_str() {
        "mon" | "month" | "months" => Some(n as u32),
        "y" | "yr" | "yrs" | "year" | "years" => Some(n as u32 * 12),
        _ => None,
    };
    match months {
        Some(m) => {
            let months = Months::new(m);
            let res = if sign < 0 {
                base.checked_sub_months(months)
            } else {
                base.checked_add_months(months)
            };
            res.ok_or_else(|| "date arithmetic overflow".to_string())
        }
        None => Err(format!("unrecognized time unit: {unit}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// Parse with shortcuts enabled (quick mode).
    fn p(expr: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
        parse_instant(expr, now, true)
    }

    #[test]
    fn today_is_midnight_of_now() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("today", now).unwrap(), at(2026, 9, 19, 0, 0, 0));
    }

    #[test]
    fn today_minus_five_days() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("today-5day", now).unwrap(), at(2026, 9, 14, 0, 0, 0));
    }

    #[test]
    fn now_keeps_wall_clock() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("now", now).unwrap(), now);
    }

    #[test]
    fn now_minus_two_hours() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("now-2h", now).unwrap(), at(2026, 9, 19, 12, 30, 0));
    }

    #[test]
    fn iso_date_anchor_is_that_days_midnight() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("2026-09-01", now).unwrap(), at(2026, 9, 1, 0, 0, 0));
    }

    #[test]
    fn iso_date_plus_five_days_keeps_internal_dashes() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("2026-09-01+5day", now).unwrap(), at(2026, 9, 6, 0, 0, 0));
    }

    #[test]
    fn calendar_month_arithmetic() {
        let now = at(2026, 1, 31, 10, 0, 0);
        // Jan 31 minus one calendar month clamps to end of Dec's neighbour, not an invalid Feb 31.
        assert_eq!(p("today-1month", now).unwrap(), at(2025, 12, 31, 0, 0, 0));
    }

    #[test]
    fn year_arithmetic() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("today+1year", now).unwrap(), at(2027, 9, 19, 0, 0, 0));
    }

    #[test]
    fn bare_duration_defaults_to_today_minus_for_days() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert_eq!(p("3day", now).unwrap(), at(2026, 9, 16, 0, 0, 0));
        assert_eq!(p("1week", now).unwrap(), at(2026, 9, 12, 0, 0, 0));
        assert_eq!(p("-3day", now).unwrap(), at(2026, 9, 16, 0, 0, 0));
        assert_eq!(p("+3day", now).unwrap(), at(2026, 9, 22, 0, 0, 0));
    }

    #[test]
    fn bare_hours_anchor_from_now() {
        let now = at(2026, 9, 19, 14, 30, 0);
        // sub-day units count from `now`, not midnight
        assert_eq!(p("2h", now).unwrap(), at(2026, 9, 19, 12, 30, 0));
        assert_eq!(p("-90min", now).unwrap(), at(2026, 9, 19, 13, 0, 0));
        // days still anchor at today (midnight)
        assert_eq!(p("3day", now).unwrap(), at(2026, 9, 16, 0, 0, 0));
    }

    #[test]
    fn standard_mode_rejects_shortcuts_but_allows_explicit() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert!(parse_instant("3day", now, false).is_err());
        assert!(parse_instant("-3day", now, false).is_err());
        assert!(parse_instant("2h", now, false).is_err());
        // explicit anchors still work in standard mode
        assert_eq!(parse_instant("today-3day", now, false).unwrap(), at(2026, 9, 16, 0, 0, 0));
        assert_eq!(parse_instant("now-2h", now, false).unwrap(), at(2026, 9, 19, 12, 30, 0));
    }

    #[test]
    fn garbage_is_an_error() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert!(p("bogus", now).is_err());
        assert!(p("today-5furlong", now).is_err());
    }
}
