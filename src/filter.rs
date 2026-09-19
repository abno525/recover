//! Parse taskwarrior-style `key:value` filter tokens into a [`Filters`] struct.
//!
//! Keys: `cmd`, `out`, `reg` (regex sources); `date`, `date.after`, `date.before`
//! (date expressions, `date:A..B` for a half-open range); `exit`, `limit`.

use crate::model::Filters;
use crate::timequery::parse_instant;
use chrono::{DateTime, Utc};

/// Parse filter tokens. `now` anchors relative date expressions; `shortcuts` allows the
/// bare-duration date forms (`date:3day`) — disabled in standard mode.
pub fn parse(tokens: &[String], now: DateTime<Utc>, shortcuts: bool) -> Result<Filters, String> {
    let mut f = Filters::default();
    for tok in tokens {
        let (key, val) = tok
            .split_once(':')
            .ok_or_else(|| format!("not a filter (expected key:value): {tok}"))?;
        match key {
            "cmd" => f.cmd_re = Some(val.to_string()),
            "out" => f.out_re = Some(val.to_string()),
            "reg" => f.reg_re = Some(val.to_string()),
            "com" => f.com_re = Some(val.to_string()),
            "exit" => {
                f.exit = Some(val.parse().map_err(|_| format!("invalid exit code: {val}"))?)
            }
            "limit" => {
                f.limit = Some(val.parse().map_err(|_| format!("invalid limit: {val}"))?)
            }
            "date" => {
                if let Some((a, b)) = val.split_once("..") {
                    f.after = Some(parse_instant(a, now, shortcuts)?);
                    f.before = Some(parse_instant(b, now, shortcuts)?);
                } else {
                    f.after = Some(parse_instant(val, now, shortcuts)?);
                }
            }
            "date.after" => f.after = Some(parse_instant(val, now, shortcuts)?),
            "date.before" => f.before = Some(parse_instant(val, now, shortcuts)?),
            other => return Err(format!("unknown filter key: {other}")),
        }
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 14, 30, 0).unwrap()
    }
    fn toks(s: &[&str]) -> Vec<String> {
        s.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn parses_regex_sources() {
        let f = parse(&toks(&["cmd:git", "out:panic", "reg:foo", "com:bar"]), now(), true).unwrap();
        assert_eq!(f.cmd_re.as_deref(), Some("git"));
        assert_eq!(f.out_re.as_deref(), Some("panic"));
        assert_eq!(f.reg_re.as_deref(), Some("foo"));
        assert_eq!(f.com_re.as_deref(), Some("bar"));
    }

    #[test]
    fn date_window_sets_inclusive_after_only() {
        let f = parse(&toks(&["date:today-5day"]), now(), true).unwrap();
        assert_eq!(f.after, Some(Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0).unwrap()));
        assert_eq!(f.before, None);
    }

    #[test]
    fn date_range_is_half_open() {
        let f = parse(&toks(&["date:2026-09-01..2026-09-10"]), now(), true).unwrap();
        assert_eq!(f.after, Some(Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap()));
        assert_eq!(f.before, Some(Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap()));
    }

    #[test]
    fn one_sided_before_and_after() {
        let f = parse(&toks(&["date.after:yesterday", "date.before:today"]), now(), true).unwrap();
        assert_eq!(f.after, Some(Utc.with_ymd_and_hms(2026, 9, 18, 0, 0, 0).unwrap()));
        assert_eq!(f.before, Some(Utc.with_ymd_and_hms(2026, 9, 19, 0, 0, 0).unwrap()));
    }

    #[test]
    fn exit_and_limit() {
        let f = parse(&toks(&["exit:3", "limit:20"]), now(), true).unwrap();
        assert_eq!(f.exit, Some(3));
        assert_eq!(f.limit, Some(20));
        assert_eq!(parse(&toks(&["limit:0"]), now(), true).unwrap().limit, Some(0));
    }

    #[test]
    fn regex_value_may_contain_a_colon() {
        let f = parse(&toks(&["cmd:foo:bar"]), now(), true).unwrap();
        assert_eq!(f.cmd_re.as_deref(), Some("foo:bar"));
    }

    #[test]
    fn unknown_key_and_bad_token_error() {
        assert!(parse(&toks(&["bogus:x"]), now(), true).is_err());
        assert!(parse(&toks(&["nocolon"]), now(), true).is_err());
        assert!(parse(&toks(&["exit:notanumber"]), now(), true).is_err());
    }

    #[test]
    fn standard_mode_rejects_bare_duration_but_allows_explicit() {
        assert!(parse(&toks(&["date:3day"]), now(), false).is_err());
        let f = parse(&toks(&["date:today-3day"]), now(), false).unwrap();
        assert_eq!(f.after, Some(Utc.with_ymd_and_hms(2026, 9, 16, 0, 0, 0).unwrap()));
    }
}
