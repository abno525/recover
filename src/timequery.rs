//! Parse taskwarrior-style date expressions into UTC instants (days are local).
//!
//! Grammar: `<anchor>` optionally followed by one arithmetic term `<±><N><unit>`.
//! Anchors: `now`, `today`, `yesterday`, `tomorrow`, `YYYY-MM-DD`, RFC3339 datetime.
//! Units: `s/sec`, `m/min`, `h/hour`, `d/day`, `w/week`, `mon/month`, `y/year`.

use chrono::{DateTime, Days, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
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
/// Day words and dates (`today`, `yesterday`, `2026-09-01`) mean midnight in `now`'s
/// time zone, and day/week/month/year steps move the local calendar date, so day
/// boundaries match the local times the list shows. Second/minute/hour steps move the
/// instant itself.
///
/// When `shortcuts` is true, a bare duration (`3day`) or a leading-sign duration (`-3day`)
/// is shorthand for the default anchor minus/plus that duration (`date:3day` ==
/// `date:today-3day`; `date:2h` == `date:now-2h`). When false (standard mode), only
/// explicit anchors are accepted.
pub fn parse_instant<Tz: TimeZone>(
    expr: &str,
    now: DateTime<Tz>,
    shortcuts: bool,
) -> Result<DateTime<Utc>, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("empty date expression".into());
    }
    let local = if let Some(c) = term_re().captures(expr) {
        let anchor = c.get(1).unwrap().as_str();
        let unit = c.get(4).unwrap().as_str();
        let base = if anchor.is_empty() {
            if !shortcuts {
                return Err(format!("date `{expr}` needs an explicit anchor in standard mode (e.g. today{expr})"));
            }
            parse_anchor(default_anchor_for_unit(unit), &now)?
        } else {
            parse_anchor(anchor, &now)?
        };
        let sign: i64 = if &c[2] == "-" { -1 } else { 1 };
        let n: i64 = c[3].parse().map_err(|_| format!("number too large: {}", &c[3]))?;
        apply_term(base, sign, n, unit)?
    } else if let Some(c) = bare_dur_re().captures(expr) {
        if !shortcuts {
            return Err(format!("bare duration `{expr}` needs quick mode; use e.g. today-{expr}"));
        }
        let n: i64 = c[1].parse().map_err(|_| format!("number too large: {}", &c[1]))?;
        let unit = c.get(2).unwrap().as_str();
        let base = parse_anchor(default_anchor_for_unit(unit), &now)?;
        apply_term(base, -1, n, unit)?
    } else {
        parse_anchor(expr, &now)?
    };
    Ok(local.with_timezone(&Utc))
}

/// The first valid local time at or after `naive`: `naive` itself unless a DST jump
/// skips it (Santiago has no 00:00 on its spring-forward day), then the next half hour
/// that exists. An ambiguous time (fall-back) resolves to its earlier instant.
fn resolve_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> Option<DateTime<Tz>> {
    (0..=48i64).find_map(|step| {
        let t = naive.checked_add_signed(TimeDelta::minutes(30 * step))?;
        tz.from_local_datetime(&t).earliest()
    })
}

fn local_midnight<Tz: TimeZone>(tz: &Tz, day: Option<NaiveDate>) -> Result<DateTime<Tz>, String> {
    let day = day.ok_or("date out of range")?;
    resolve_local(tz, day.and_time(NaiveTime::MIN)).ok_or_else(|| format!("no valid local time on {day}"))
}

fn parse_anchor<Tz: TimeZone>(s: &str, now: &DateTime<Tz>) -> Result<DateTime<Tz>, String> {
    let tz = now.timezone();
    let today = now.date_naive();
    match s {
        "now" => Ok(now.clone()),
        "today" => local_midnight(&tz, Some(today)),
        "yesterday" => local_midnight(&tz, today.pred_opt()),
        "tomorrow" => local_midnight(&tz, today.succ_opt()),
        _ => {
            if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return local_midnight(&tz, Some(d));
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Ok(dt.with_timezone(&tz));
            }
            Err(format!("unrecognized date anchor: {s}"))
        }
    }
}

fn apply_term<Tz: TimeZone>(base: DateTime<Tz>, sign: i64, n: i64, unit: &str) -> Result<DateTime<Tz>, String> {
    let overflow = || "date arithmetic overflow".to_string();
    let unit = unit.to_ascii_lowercase();

    // Sub-day units move the instant.
    let signed = sign.checked_mul(n).ok_or_else(overflow)?;
    let instant_step = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(TimeDelta::try_seconds(signed)),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(TimeDelta::try_minutes(signed)),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(TimeDelta::try_hours(signed)),
        _ => None,
    };
    if let Some(step) = instant_step {
        return step.and_then(|d| base.checked_add_signed(d)).ok_or_else(overflow);
    }

    // Calendar units move the local wall-clock date, then resolve back to an instant.
    let wall = base.naive_local();
    let moved = match unit.as_str() {
        "d" | "day" | "days" => shift_days(wall, sign, n),
        "w" | "wk" | "wks" | "week" | "weeks" => n.checked_mul(7).and_then(|d| shift_days(wall, sign, d)),
        "mon" | "month" | "months" => shift_months(wall, sign, n, 1),
        "y" | "yr" | "yrs" | "year" | "years" => shift_months(wall, sign, n, 12),
        _ => return Err(format!("unrecognized time unit: {unit}")),
    };
    moved
        .and_then(|t| resolve_local(&base.timezone(), t))
        .ok_or_else(overflow)
}

fn shift_days(t: NaiveDateTime, sign: i64, n: i64) -> Option<NaiveDateTime> {
    let days = Days::new(u64::try_from(n).ok()?);
    if sign < 0 { t.checked_sub_days(days) } else { t.checked_add_days(days) }
}

fn shift_months(t: NaiveDateTime, sign: i64, n: i64, per: u32) -> Option<NaiveDateTime> {
    let months = Months::new(u32::try_from(n).ok()?.checked_mul(per)?);
    if sign < 0 { t.checked_sub_months(months) } else { t.checked_add_months(months) }
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
    fn huge_offsets_are_errors_not_panics() {
        let now = at(2026, 9, 19, 14, 30, 0);
        for expr in [
            "today-99999999999999day",
            "today-99999999999999week",
            "now-99999999999999999s",
            "now-99999999999999999h",
            "today-999999999year",
            "99999999999999day",
        ] {
            assert!(p(expr, now).is_err(), "{expr} should be an error");
        }
    }

    #[test]
    fn month_count_is_not_silently_truncated() {
        let now = at(2026, 9, 19, 14, 30, 0);
        // 4294967297 = 2^32 + 1; a plain `as u32` cast would turn it into 1 month.
        assert!(p("today-4294967297month", now).is_err());
    }

    /// A wall-clock time in a fixed UTC+2 zone.
    fn plus2(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<chrono::FixedOffset> {
        chrono::FixedOffset::east_opt(2 * 3600).unwrap().with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn day_anchors_are_local_midnight() {
        let now = plus2(2026, 9, 19, 0, 30); // 22:30Z on the 18th
        let q = |e: &str| parse_instant(e, now, true).unwrap();
        assert_eq!(q("today"), at(2026, 9, 18, 22, 0, 0));
        assert_eq!(q("yesterday"), at(2026, 9, 17, 22, 0, 0));
        assert_eq!(q("tomorrow"), at(2026, 9, 19, 22, 0, 0));
        assert_eq!(q("2026-09-01"), at(2026, 8, 31, 22, 0, 0));
        assert_eq!(q("today-1day"), at(2026, 9, 17, 22, 0, 0));
        assert_eq!(q("3day"), at(2026, 9, 15, 22, 0, 0));
        assert_eq!(q("today-1month"), at(2026, 8, 18, 22, 0, 0));
    }

    #[test]
    fn hour_offsets_count_from_the_instant() {
        let now = plus2(2026, 9, 19, 0, 30);
        assert_eq!(parse_instant("now-2h", now, true).unwrap(), at(2026, 9, 18, 20, 30, 0));
        assert_eq!(parse_instant("2h", now, true).unwrap(), at(2026, 9, 18, 20, 30, 0));
    }

    /// UTC, except that local 00:00-00:59 on 2026-09-06 does not exist (a clock change
    /// skips it), like Santiago's spring-forward day.
    #[derive(Clone, Copy, Debug)]
    struct MidnightGap;

    impl TimeZone for MidnightGap {
        type Offset = chrono::FixedOffset;
        fn from_offset(_: &chrono::FixedOffset) -> Self {
            MidnightGap
        }
        fn offset_from_local_date(&self, _: &NaiveDate) -> chrono::MappedLocalTime<chrono::FixedOffset> {
            chrono::MappedLocalTime::Single(chrono::FixedOffset::east_opt(0).unwrap())
        }
        fn offset_from_local_datetime(&self, t: &NaiveDateTime) -> chrono::MappedLocalTime<chrono::FixedOffset> {
            use chrono::Timelike;
            if t.date() == NaiveDate::from_ymd_opt(2026, 9, 6).unwrap() && t.hour() == 0 {
                chrono::MappedLocalTime::None
            } else {
                chrono::MappedLocalTime::Single(chrono::FixedOffset::east_opt(0).unwrap())
            }
        }
        fn offset_from_utc_date(&self, _: &NaiveDate) -> chrono::FixedOffset {
            chrono::FixedOffset::east_opt(0).unwrap()
        }
        fn offset_from_utc_datetime(&self, _: &NaiveDateTime) -> chrono::FixedOffset {
            chrono::FixedOffset::east_opt(0).unwrap()
        }
    }

    #[test]
    fn a_day_whose_midnight_is_skipped_starts_at_its_first_valid_time() {
        let now = MidnightGap.with_ymd_and_hms(2026, 9, 6, 12, 0, 0).unwrap();
        let q = |e: &str| parse_instant(e, now, true);
        assert_eq!(q("today"), Ok(at(2026, 9, 6, 1, 0, 0)));
        assert_eq!(q("2026-09-06"), Ok(at(2026, 9, 6, 1, 0, 0)));
        assert_eq!(q("2026-09-07-1day"), Ok(at(2026, 9, 6, 1, 0, 0)), "a step landing in the gap");
        assert_eq!(q("yesterday"), Ok(at(2026, 9, 5, 0, 0, 0)), "other days are unaffected");
    }

    #[test]
    fn garbage_is_an_error() {
        let now = at(2026, 9, 19, 14, 30, 0);
        assert!(p("bogus", now).is_err());
        assert!(p("today-5furlong", now).is_err());
    }
}
