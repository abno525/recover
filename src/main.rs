//! `recover` — record commands and their output, query them later.
//!
//! Dispatch: a leading reserved word selects a subcommand; otherwise all tokens are
//! treated as `key:value` filters and an implicit `list` runs.

use anyhow::{Result, bail};
use base64::prelude::{BASE64_STANDARD, Engine};
use chrono::{DateTime, Utc};
use recover::db::Db;
use recover::model::{Filters, NewRun};
use recover::{config, filter, record, render, text};
use serde::{Deserialize, Serialize};
use std::io::{IsTerminal, Write};

const RESERVED: &[&str] = &[
    "run", "list", "out", "cmd", "show", "rerun", "rm", "del", "clear", "export", "import", "db",
    "help", "--help", "-h",
];

fn main() {
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("recover: {e:#}");
            std::process::exit(1);
        }
    }
}

fn real_main() -> Result<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // id-first form: `recover <id> [action ...]` (e.g. `recover 5 com "note"`).
    if let Some(first) = args.first()
        && let Ok(idx) = first.parse::<i64>()
    {
        return cmd_id_action(idx, &args[1..]);
    }

    let (cmd, rest) = split_command(&args);
    match cmd {
        "run" => cmd_run(rest),
        // Implicit list (bare `recover [filters]`) is the quick capped table; the explicit
        // `list` word is the paged view, identical to the `-l` flag.
        "list" => cmd_list(rest, false),
        "list-browse" => cmd_list(rest, true),
        "out" => cmd_out(rest),
        "cmd" => cmd_cmd(rest),
        "show" => cmd_show(rest),
        "rerun" => cmd_rerun(rest),
        "rm" | "del" => cmd_rm(rest),
        "clear" => cmd_clear(rest),
        "export" => cmd_export(rest),
        "import" => cmd_import(rest),
        "db" => cmd_db(rest),
        _ => {
            print_help();
            Ok(0)
        }
    }
}

/// Reserved word → (subcommand, remaining args); otherwise implicit `list` over all tokens.
/// The explicit `list` word maps to `list-browse` (the paged view); a bare filter listing
/// stays the quick `list`.
fn split_command(args: &[String]) -> (&str, &[String]) {
    match args.first() {
        None => ("list", args),
        Some(f) if f == "help" || f == "--help" || f == "-h" => ("help", &args[1..]),
        Some(f) if f == "list" => ("list-browse", &args[1..]),
        Some(f) if RESERVED.contains(&f.as_str()) => (f.as_str(), &args[1..]),
        Some(_) => ("list", args),
    }
}

fn open_db() -> Result<Db> {
    let path = config::db_path();
    config::ensure_parent(&path)?;
    Db::open(&path)
}

// ---- id-first actions: `recover <id> [com [text]]` ------------------------

fn cmd_id_action(idx: i64, rest: &[String]) -> Result<i32> {
    // `recover <id>` alone shows the run.
    let Some(verb) = rest.first().map(String::as_str) else {
        return cmd_show(&[idx.to_string()]);
    };
    let after = &rest[1..];
    // Reuse the verb-first handlers by prepending the id to their args.
    let with_id = || {
        let mut v = vec![idx.to_string()];
        v.extend_from_slice(after);
        v
    };
    match verb {
        "com" | "comment" => cmd_comment(idx, after),
        "out" => cmd_out(&with_id()),
        "cmd" => cmd_cmd(&with_id()),
        "show" => cmd_show(&with_id()),
        "rerun" => cmd_rerun(&with_id()),
        "rm" | "del" => cmd_rm(&with_id()),
        other => bail!("unknown action `{other}` for a run (try: out, cmd, com, show, rerun, rm/del)"),
    }
}

/// `recover <id> com` prints the comment; `recover <id> com <text...>` sets it
/// (empty text clears it).
fn cmd_comment(idx: i64, args: &[String]) -> Result<i32> {
    let db = open_db()?;
    let id = resolve_index(&db, idx)?;
    let run = db.get(id)?.ok_or_else(|| no_such(id))?;

    if args.is_empty() {
        match run.comment {
            Some(c) => println!("{c}"),
            None => eprintln!("(no comment on run {id})"),
        }
        return Ok(0);
    }

    let text = args.join(" ");
    if text.trim().is_empty() {
        db.set_comment(id, None)?;
        eprintln!("comment cleared on run {id}");
    } else {
        db.set_comment(id, Some(&text))?;
        eprintln!("comment set on run {id}");
    }
    Ok(0)
}

// ---- run / rerun ----------------------------------------------------------

fn cmd_run(rest: &[String]) -> Result<i32> {
    let mut shell_mode = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-c" => {
                shell_mode = true;
                i += 1;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let tokens = &rest[i..];
    if tokens.is_empty() {
        bail!("usage: recover run [-c] <command...>");
    }

    let (command_str, argv) = if shell_mode {
        let script = tokens.join(" ");
        (script.clone(), vec![shell(), "-c".to_string(), script])
    } else {
        (shell_join(tokens), tokens.to_vec())
    };
    run_and_record(command_str, &argv)
}

fn cmd_rerun(rest: &[String]) -> Result<i32> {
    let idx = single_id(rest)?;
    let db = open_db()?;
    let id = resolve_index(&db, idx)?;
    let run = db.get(id)?.ok_or_else(|| no_such(id))?;
    let argv = vec![shell(), "-c".to_string(), run.command.clone()];
    run_and_record(run.command, &argv)
}

/// Run `argv` in a PTY, store the result under `command_str`, and return the child's exit code.
fn run_and_record(command_str: String, argv: &[String]) -> Result<i32> {
    let rec = record::run_in_pty(argv)?;
    let new = NewRun {
        command: command_str,
        output: rec.output,
        exit_code: rec.exit_code,
        signal: rec.signal.clone(),
        started_at: rec.started_at,
        finished_at: rec.finished_at,
        duration_ms: rec.duration_ms,
        cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
        host: std::env::var("HOSTNAME").ok(),
        shell: std::env::var("SHELL").ok(),
        comment: None,
    };
    match open_db().and_then(|db| db.insert(&new)) {
        Ok(_) => {}
        Err(e) => eprintln!("recover: failed to record run: {e:#}"),
    }
    Ok(rec.exit_code.unwrap_or(1))
}

// ---- list -----------------------------------------------------------------

fn cmd_list(rest: &[String], browse_explicit: bool) -> Result<i32> {
    let cfg = config::load();

    // Browse (paged) when the `list` word was used or the `-l` flag is present; either way
    // pull `-l` out before parsing filters. `recover list X` and `recover X -l` are identical.
    let browse = browse_explicit || rest.iter().any(|a| a == "-l");
    let filter_toks: Vec<String> = rest.iter().filter(|a| a.as_str() != "-l").cloned().collect();
    let mut filters =
        filter::parse(&filter_toks, Utc::now(), cfg.shortcuts()).map_err(anyhow::Error::msg)?;
    // The quick table caps at the configured default; `-l`/`list` pull in every match to
    // scroll. A `limit:N` filter token overrides either way.
    if filters.limit.is_none() {
        filters.limit = Some(if browse { 0 } else { cfg.limit });
    }
    let db = open_db()?;
    let runs = db.query(&filters)?;

    let no_color = std::env::var_os("NO_COLOR").is_some();
    let want_color = match cfg.color {
        config::ColorMode::Off => false,
        config::ColorMode::On => !no_color,
        config::ColorMode::Auto => std::io::stdout().is_terminal() && !no_color,
    };
    let mut width = match (want_color, crossterm::terminal::size()) {
        (true, Ok((c, _))) if c >= 20 => c as usize,
        _ => 100,
    };
    // In the pager, leave one column free so full-width zebra rows don't wrap onto a blank line.
    if browse && want_color {
        width = width.saturating_sub(1);
    }
    let style = want_color.then(|| render::ListStyle {
        bar_bg: cfg.bar_color.unwrap_or(render::DEFAULT_BAR),
        header: cfg.header_color,
    });
    let listing = render::format_list(&runs, style.as_ref(), width);

    if browse {
        return page_document(listing);
    }
    print!("{listing}");
    Ok(0)
}

/// Display `doc` in a scrollable pager (`/` regex search, `q` to quit) when stdout is a
/// terminal; otherwise dump it plainly so `recover -l | grep …` keeps working.
fn page_document(doc: String) -> Result<i32> {
    if std::io::stdout().is_terminal() {
        let pager = minus::Pager::new();
        pager.push_str(&doc)?;
        let _ = pager.set_prompt("recover    / search    q quit");
        minus::page_all(pager)?;
    } else {
        std::io::stdout().write_all(doc.as_bytes())?;
    }
    Ok(0)
}

// ---- out / cmd / show -----------------------------------------------------

fn cmd_out(rest: &[String]) -> Result<i32> {
    let mut strip = false;
    let mut id = None;
    for a in rest {
        if a == "--strip" {
            strip = true;
        } else {
            id = Some(parse_id(a)?);
        }
    }
    let idx = id.ok_or_else(|| anyhow::anyhow!("usage: recover out <id> [--strip]"))?;
    let db = open_db()?;
    let id = resolve_index(&db, idx)?;
    let run = db.get(id)?.ok_or_else(|| no_such(id))?;
    let bytes = if strip {
        text::decode_lossy_stripped(&run.output).into_bytes()
    } else {
        run.output
    };
    std::io::stdout().write_all(&bytes)?;
    Ok(0)
}

fn cmd_cmd(rest: &[String]) -> Result<i32> {
    let idx = single_id(rest)?;
    let db = open_db()?;
    let id = resolve_index(&db, idx)?;
    let run = db.get(id)?.ok_or_else(|| no_such(id))?;
    println!("{}", run.command);
    Ok(0)
}

fn cmd_show(rest: &[String]) -> Result<i32> {
    let idx = single_id(rest)?;
    let db = open_db()?;
    let id = resolve_index(&db, idx)?;
    let run = db.get(id)?.ok_or_else(|| no_such(id))?;
    print!("{}", render::format_show(&run));
    Ok(0)
}

// ---- rm / clear -----------------------------------------------------------

/// Delete one or more runs (`recover del|rm <id...>`).
fn cmd_rm(rest: &[String]) -> Result<i32> {
    if rest.is_empty() {
        bail!("expected a run id");
    }
    let db = open_db()?;
    // Resolve all indices to concrete ids first so negative indices (e.g. -1) aren't shifted
    // by deletions happening mid-loop; de-dup while preserving order.
    let mut ids: Vec<i64> = Vec::new();
    for a in rest {
        let id = resolve_index(&db, parse_id(a)?)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut deleted_any = false;
    for id in &ids {
        if db.delete(*id)? {
            println!("deleted run {id}");
            deleted_any = true;
        } else {
            eprintln!("recover: no run with id {id}");
        }
    }
    // Exit nonzero when nothing was deleted; the per-id message above already explained why.
    Ok(if deleted_any { 0 } else { 1 })
}

fn cmd_clear(rest: &[String]) -> Result<i32> {
    let db = open_db()?;
    let n = db.query(&Filters { limit: Some(0), ..Default::default() })?.len();
    if rest.iter().any(|a| a == "--yes") {
        let removed = db.clear()?;
        println!("deleted {removed} runs");
        Ok(0)
    } else {
        println!("this will delete {n} runs; re-run with: recover clear --yes");
        Ok(0)
    }
}

// ---- export / import ------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct ExportRun {
    id: i64,
    command: String,
    output_b64: String,
    exit_code: Option<i32>,
    signal: Option<String>,
    started_at: String,
    finished_at: String,
    duration_ms: i64,
    cwd: Option<String>,
    host: Option<String>,
    shell: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

fn cmd_export(rest: &[String]) -> Result<i32> {
    // Accept an optional `--json` (the only format) and an optional output file path.
    let file = rest.iter().find(|a| a.as_str() != "--json");
    let db = open_db()?;
    let runs = db.query(&Filters { limit: Some(0), ..Default::default() })?;
    let exported: Vec<ExportRun> = runs
        .into_iter()
        .map(|r| ExportRun {
            id: r.id,
            command: r.command,
            output_b64: BASE64_STANDARD.encode(&r.output),
            exit_code: r.exit_code,
            signal: r.signal,
            started_at: r.started_at.to_rfc3339(),
            finished_at: r.finished_at.to_rfc3339(),
            duration_ms: r.duration_ms,
            cwd: r.cwd,
            host: r.host,
            shell: r.shell,
            comment: r.comment,
        })
        .collect();
    let json = serde_json::to_string_pretty(&exported)?;
    match file {
        Some(path) => {
            std::fs::write(path, json)?;
            eprintln!("exported {} runs to {path}", exported.len());
        }
        None => println!("{json}"),
    }
    Ok(0)
}

fn cmd_import(rest: &[String]) -> Result<i32> {
    let path = rest
        .iter()
        .find(|a| a.as_str() != "--json")
        .ok_or_else(|| anyhow::anyhow!("usage: recover import <file.json>"))?;
    let json = std::fs::read_to_string(path)?;
    let records: Vec<ExportRun> = serde_json::from_str(&json)?;
    let db = open_db()?;
    let mut n = 0;
    for r in records {
        let new = NewRun {
            command: r.command,
            output: BASE64_STANDARD.decode(r.output_b64.as_bytes())?,
            exit_code: r.exit_code,
            signal: r.signal,
            started_at: parse_ts(&r.started_at)?,
            finished_at: parse_ts(&r.finished_at)?,
            duration_ms: r.duration_ms,
            cwd: r.cwd,
            host: r.host,
            shell: r.shell,
            comment: r.comment,
        };
        db.insert(&new)?;
        n += 1;
    }
    eprintln!("imported {n} runs");
    Ok(0)
}

// ---- db -------------------------------------------------------------------

fn cmd_db(rest: &[String]) -> Result<i32> {
    match rest.first().map(String::as_str) {
        Some("path") | None => {
            println!("{}", config::db_path().display());
            Ok(0)
        }
        Some(other) => bail!("unknown db subcommand: {other} (try: recover db path)"),
    }
}

// ---- helpers --------------------------------------------------------------

fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

fn parse_id(s: &str) -> Result<i64> {
    s.parse::<i64>().map_err(|_| anyhow::anyhow!("invalid id: {s}"))
}

fn single_id(rest: &[String]) -> Result<i64> {
    match rest.first() {
        Some(s) => parse_id(s),
        None => bail!("expected a run id"),
    }
}

fn no_such(id: i64) -> anyhow::Error {
    anyhow::anyhow!("no run with id {id}")
}

/// Resolve an index to a concrete id, rejecting negative indices in standard mode.
fn resolve_index(db: &Db, idx: i64) -> Result<i64> {
    if idx < 0 && !config::load().shortcuts() {
        bail!("negative indices need quick mode (set `mode = quick` in ~/.recoverc, or use a positive id)");
    }
    db.resolve_id(idx)
}

/// Join command tokens into a shell-safe string (for storage and reruns).
fn shell_join(args: &[String]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn print_help() {
    println!(
        "recover — record commands and their output, query them later\n\
\n\
USAGE:\n\
  r <command...>                        run a command, recording it\n\
  recover run [-c] [--] <command...>    the underlying recorder (-c = shell pipeline)\n\
  recover [filters]                     quick list of recent runs (newest first, capped)\n\
  recover list [filters] | recover [filters] -l   same list in a scrollable pager (uncapped)\n\
  recover <id>                          show a run in full\n\
  recover <id> out|cmd|com|show|rerun|rm|del [args]   act on a run (id-first)\n\
  recover out|cmd|show|rerun <id>       act on a run (verb-first)\n\
  recover del|rm <id...>               delete one or more runs\n\
  recover <id> com \"text\"               add or replace a run's note\n\
  recover export [--json FILE] | import <FILE>    dump / load all runs as JSON\n\
  recover clear --yes                   delete all runs\n\
  recover db path                       print the database file path\n\
  recover help                          this message\n\
\n\
Filters (cmd:/out:/reg:/com:/date:/exit:/limit:), date expressions, configuration\n\
(~/.recoverc), and environment variables are documented in the manual: man recover"
    );
}
