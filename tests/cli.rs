//! End-to-end tests driving the compiled `recover` binary against a temp database.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_recover")
}

fn tmp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("recover-it-{}-{}.db", std::process::id(), name));
    let _ = std::fs::remove_file(&p);
    p
}

fn cfg_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("recover-it-{}-{}.config", std::process::id(), name));
    p
}

// Default runs point RECOVER_CONFIG at an absent file, so the developer's real
// config never affects the tests (defaults: quick mode, color auto).
fn run(db: &PathBuf, args: &[&str]) -> Output {
    let none = cfg_path("absent");
    let _ = std::fs::remove_file(&none);
    run_cfg(db, &none, args)
}

fn recover_cmd(db: &PathBuf, config: &PathBuf, args: &[&str]) -> Command {
    let mut c = Command::new(bin());
    c.args(args)
        .env("RECOVER_DB", db)
        .env("RECOVER_CONFIG", config)
        .stdin(Stdio::null());
    c
}

fn run_cfg(db: &PathBuf, config: &PathBuf, args: &[&str]) -> Output {
    recover_cmd(db, config, args).output().expect("failed to execute recover")
}

/// Like [`run`], with extra environment variables (e.g. `TZ`).
fn run_env(db: &PathBuf, env: &[(&str, &str)], args: &[&str]) -> Output {
    let none = cfg_path("absent");
    let mut c = recover_cmd(db, &none, args);
    c.envs(env.iter().copied());
    c.output().expect("failed to execute recover")
}

/// Write an export-format JSON file holding `(id, command, started_at)` runs and import it.
fn import_runs(db: &PathBuf, name: &str, runs: &[(i64, &str, &str)]) -> Output {
    let rows: Vec<String> = runs
        .iter()
        .map(|(id, cmd, start)| {
            format!(
                r#"{{"id":{id},"command":"{cmd}","output_b64":"","exit_code":0,"signal":null,"started_at":"{start}","finished_at":"{start}","duration_ms":0,"cwd":null,"host":null,"shell":null}}"#
            )
        })
        .collect();
    let mut json = std::env::temp_dir();
    json.push(format!("recover-it-{}-{name}.import.json", std::process::id()));
    std::fs::write(&json, format!("[{}]", rows.join(","))).unwrap();
    let out = run(db, &["import", json.to_str().unwrap()]);
    let _ = std::fs::remove_file(&json);
    out
}

/// Run `cmd`, killing it and failing the test if it is still running after `secs`.
/// `stdin`: `Some(bytes)` writes them and closes stdin; `None` keeps stdin open and silent
/// until the process exits (so a terminal under `script` never sees EOF).
fn output_within(mut cmd: Command, stdin: Option<&[u8]>, secs: u64) -> Output {
    use std::io::Write;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to spawn");
    let mut pipe = child.stdin.take();
    if let Some(data) = stdin {
        let mut w = pipe.take().unwrap();
        w.write_all(data).unwrap();
    }
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(out) => out.expect("wait failed"),
        Err(_) => {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            panic!("timed out after {secs}s");
        }
    };
    drop(pipe);
    out
}

/// Run a shell command line inside a pseudo-terminal (util-linux `script`), so recover sees
/// a terminal on stdin and takes its PTY path. `None` when `script` is not installed.
fn run_in_tty(db: &PathBuf, cmdline: &str, secs: u64) -> Option<Output> {
    if Command::new("script").arg("-V").output().is_err() {
        eprintln!("skipping: script(1) is not installed");
        return None;
    }
    let mut c = Command::new("script");
    c.args(["-qec", cmdline, "/dev/null"])
        .env("RECOVER_DB", db)
        .env("RECOVER_CONFIG", cfg_path("absent"));
    Some(output_within(c, None, secs))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn records_then_lists_and_prints_output() {
    let db = tmp_db("basic");
    run(&db, &["run", "--", "echo", "hello"]);

    let list = run(&db, &[]);
    assert!(stdout(&list).contains("echo hello"), "list was: {}", stdout(&list));

    // --strip removes ANSI escapes and normalizes PTY line endings (CRLF -> LF)
    let out = run(&db, &["out", "1", "--strip"]);
    assert_eq!(stdout(&out), "hello\n");

    let cmd = run(&db, &["cmd", "1"]);
    assert_eq!(stdout(&cmd), "echo hello\n");
}

#[test]
fn piped_stdin_reaches_the_command() {
    let db = tmp_db("stdin-pipe");
    let cmd = recover_cmd(&db, &cfg_path("absent"), &["run", "--", "cat"]);
    let out = output_within(cmd, Some(b"hi\n"), 10);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&out), "hi\n", "live output");
    assert_eq!(stdout(&run(&db, &["out", "1"])), "hi\n", "stored output");
}

#[test]
fn a_terminal_stdin_runs_the_command_in_a_pty() {
    let db = tmp_db("stdin-tty");
    let line = format!("'{}' run -- sh -c 'test -t 1 && echo tty'", bin());
    let Some(out) = run_in_tty(&db, &line, 10) else { return };
    assert!(out.status.success(), "script stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&run(&db, &["out", "1", "--strip"])), "tty\n");
}

#[test]
fn exit_code_is_recorded_and_propagated() {
    let db = tmp_db("exit");
    let out = run(&db, &["run", "--", "sh", "-c", "exit 7"]);
    assert_eq!(out.status.code(), Some(7), "recover should exit with the child's code");

    let show = run(&db, &["show", "1"]);
    assert!(stdout(&show).contains("status:   7"), "show was: {}", stdout(&show));
}

#[test]
fn a_signal_death_exits_128_plus_the_signal_number() {
    let db = tmp_db("signal-pipe");
    let out = run(&db, &["run", "--", "sh", "-c", "kill -TERM $$"]);
    assert_eq!(out.status.code(), Some(143), "SIGTERM is 15, so 128 + 15");
    let show = stdout(&run(&db, &["show", "1"]));
    assert!(show.contains("status:   Terminated"), "show was: {show}");
}

#[test]
fn a_signal_death_in_a_pty_exits_128_plus_the_signal_number() {
    let db = tmp_db("signal-tty");
    let line = format!("'{}' run -- sh -c 'kill -TERM $$'", bin());
    let Some(out) = run_in_tty(&db, &line, 10) else { return };
    assert_eq!(out.status.code(), Some(143), "script passes recover's status through");
    let show = stdout(&run(&db, &["show", "1"]));
    assert!(show.contains("status:   Terminated"), "show was: {show}");
}

#[test]
fn a_background_process_does_not_keep_recover_waiting() {
    let db = tmp_db("bg-pipe");
    let cmd = recover_cmd(&db, &cfg_path("absent"), &["run", "--", "sh", "-c", "echo before; sleep 3 &"]);
    let started = std::time::Instant::now();
    let out = output_within(cmd, Some(b""), 10);
    let took = started.elapsed();
    assert!(out.status.success());
    assert!(took < std::time::Duration::from_secs(2), "recover waited {took:?} for the orphan");
    assert_eq!(stdout(&run(&db, &["out", "1", "--strip"])), "before\n");
}

#[test]
fn a_background_process_does_not_keep_recover_waiting_in_a_pty() {
    let db = tmp_db("bg-tty");
    // Ignore SIGHUP so the orphan survives the PTY closing and really holds it open.
    let line = format!("'{}' run -- sh -c 'trap \"\" HUP; echo before; sleep 3 &'", bin());
    let started = std::time::Instant::now();
    let Some(out) = run_in_tty(&db, &line, 10) else { return };
    let took = started.elapsed();
    assert!(out.status.success(), "script stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(took < std::time::Duration::from_secs(2), "recover waited {took:?} for the orphan");
    assert_eq!(stdout(&run(&db, &["out", "1", "--strip"])), "before\n");
}

#[test]
fn a_terminal_resize_reaches_the_command() {
    let db = tmp_db("resize");
    // The command resizes the terminal recover runs in (script's), waits, then reports the
    // size of its own PTY. recover has to pass the new size on.
    let line = format!(
        "OUTER=$(tty) '{}' run -- sh -c 'stty -F \"$OUTER\" rows 20 cols 50; sleep 0.5; stty size'",
        bin()
    );
    let Some(out) = run_in_tty(&db, &line, 10) else { return };
    assert!(out.status.success(), "script stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&run(&db, &["out", "1", "--strip"])), "20 50\n");
}

#[test]
fn max_output_caps_what_is_stored_but_not_what_is_shown() {
    let db = tmp_db("cap");
    let cfg = cfg_path("cap");
    std::fs::write(&cfg, "max_output = 1K\n").unwrap();
    let out = run_cfg(&db, &cfg, &["run", "--", "seq", "1", "5000"]);
    assert_eq!(stdout(&out).lines().count(), 5000, "live output is complete");
    let stored = stdout(&run(&db, &["out", "1"]));
    // The cap covers the kept output; the one-line marker comes on top of it.
    assert!(stored.len() <= 1024 + 64, "stored {} bytes", stored.len());
    assert!(stored.starts_with("1\n2\n3\n"), "head: {stored:?}");
    assert!(stored.contains(" bytes omitted]\n"), "marker: {stored:?}");
    assert!(stored.ends_with("4999\n5000\n"), "tail: {stored:?}");
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn a_slow_reader_still_gets_all_the_output() {
    use std::io::Read;
    let db = tmp_db("slow-reader");
    let mut cmd = recover_cmd(&db, &cfg_path("absent"), &["run", "--", "seq", "1", "100000"]);
    let mut child = cmd.stdout(Stdio::piped()).spawn().unwrap();
    let mut out = child.stdout.take().unwrap();
    // Read 4 KiB every 20 ms: ~3 s for the ~590 KB, well past the 2 s drain cap.
    let (mut got, mut buf) = (Vec::new(), [0u8; 4096]);
    loop {
        let n = out.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(String::from_utf8_lossy(&got).lines().count(), 100000, "live output");
    assert_eq!(stdout(&run(&db, &["out", "1"])).lines().count(), 100000, "stored output");
}

#[test]
fn regex_filters_command_and_output() {
    let db = tmp_db("regex");
    run(&db, &["run", "--", "echo", "alpha"]);
    run(&db, &["run", "--", "echo", "beta"]);

    let by_cmd = stdout(&run(&db, &["cmd:alpha"]));
    assert!(by_cmd.contains("echo alpha"));
    assert!(!by_cmd.contains("echo beta"));

    // out: matches the recorded output text
    let by_out = stdout(&run(&db, &["out:beta"]));
    assert!(by_out.contains("echo beta"));
    assert!(!by_out.contains("echo alpha"));

    // reg: matches either command or output
    let by_reg = stdout(&run(&db, &["reg:alpha"]));
    assert!(by_reg.contains("echo alpha"));
    assert!(!by_reg.contains("echo beta"));
}

#[test]
fn date_window_filters() {
    let db = tmp_db("date");
    run(&db, &["run", "--", "echo", "now-ish"]);

    // today's window includes it
    assert!(stdout(&run(&db, &["date:today"])).contains("echo now-ish"));
    // a window starting tomorrow excludes it (only the header row remains)
    let future = stdout(&run(&db, &["date:tomorrow"]));
    assert!(!future.contains("echo now-ish"), "future listing: {future}");
}

#[test]
fn date_anchors_follow_local_time() {
    let db = tmp_db("localtz");
    // 20:00Z on the 18th is 06:00 on the 19th in Brisbane (UTC+10, no DST).
    import_runs(&db, "localtz", &[(1, "echo early-morning", "2026-09-18T20:00:00.000Z")]);
    let tz = [("TZ", "Australia/Brisbane")];
    let on_19th = stdout(&run_env(&db, &tz, &["date:2026-09-19..2026-09-20"]));
    assert!(on_19th.contains("echo early-morning"), "local 19th should include it: {on_19th}");
    let on_18th = stdout(&run_env(&db, &tz, &["date:2026-09-18..2026-09-19"]));
    assert!(!on_18th.contains("echo early-morning"), "local 18th should not: {on_18th}");
}

#[test]
fn a_day_without_a_midnight_starts_at_its_first_valid_time() {
    let db = tmp_db("dstgap");
    // Santiago skips 00:00-01:00 on 2026-09-06 (-04 -> -03). The day starts 01:00 -03 = 04:00Z.
    import_runs(
        &db,
        "dstgap",
        &[
            (1, "echo saturday-night", "2026-09-06T03:30:00.000Z"), // 23:30 on the 5th, local
            (2, "echo sunday-early", "2026-09-06T04:30:00.000Z"),   // 01:30 on the 6th, local
        ],
    );
    let out = run_env(&db, &[("TZ", "America/Santiago")], &["date:2026-09-06"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let list = stdout(&out);
    assert!(list.contains("echo sunday-early"), "{list}");
    assert!(!list.contains("echo saturday-night"), "{list}");
}

#[test]
fn show_reports_the_real_hostname() {
    let db = tmp_db("host");
    // $HOSTNAME is a bash variable that is usually not exported; never trust it.
    run_env(&db, &[("HOSTNAME", "not-the-real-host")], &["run", "--", "true"]);
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    let show = stdout(&run(&db, &["show", "1"]));
    assert!(show.contains(&format!("host:     {}\n", host.trim())), "show was: {show}");
}

#[test]
fn rerun_creates_a_second_record() {
    let db = tmp_db("rerun");
    run(&db, &["run", "--", "echo", "again"]);
    run(&db, &["rerun", "1"]);

    let list = stdout(&run(&db, &["limit:0"]));
    assert_eq!(list.matches("echo again").count(), 2, "list: {list}");
}

#[test]
fn export_then_import_round_trips() {
    let db = tmp_db("export");
    run(&db, &["run", "--", "echo", "keepme"]);

    let mut json = std::env::temp_dir();
    json.push(format!("recover-it-{}-export.json", std::process::id()));
    let _ = std::fs::remove_file(&json);
    run(&db, &["export", "--json", json.to_str().unwrap()]);
    assert!(json.exists(), "export file should exist");

    run(&db, &["clear", "--yes"]);
    assert!(!stdout(&run(&db, &[])).contains("echo keepme"), "clear should empty the db");

    run(&db, &["import", json.to_str().unwrap()]);
    assert!(stdout(&run(&db, &[])).contains("echo keepme"), "import should restore the run");
    let _ = std::fs::remove_file(&json);
}

#[test]
fn an_import_with_a_bad_record_imports_nothing() {
    let db = tmp_db("import-bad");
    let out = import_runs(
        &db,
        "import-bad",
        &[(1, "echo good-one", "2026-09-18T20:00:00.000Z"), (2, "echo bad-one", "not-a-date")],
    );
    assert!(!out.status.success(), "a bad record must fail the import");
    let list = stdout(&run(&db, &[]));
    assert!(!list.contains("echo good-one"), "nothing may be imported: {list}");
}

#[test]
fn export_clear_import_keeps_the_newest_run_last() {
    let db = tmp_db("import-order");
    run(&db, &["run", "--", "echo", "first"]);
    run(&db, &["run", "--", "echo", "second"]);
    let mut json = std::env::temp_dir();
    json.push(format!("recover-it-{}-import-order.json", std::process::id()));
    run(&db, &["export", json.to_str().unwrap()]);
    run(&db, &["clear", "--yes"]);
    run(&db, &["import", json.to_str().unwrap()]);
    let _ = std::fs::remove_file(&json);
    assert_eq!(stdout(&run(&db, &["cmd", "-1"])), "echo second\n", "-1 must be the newest run");
}

#[test]
fn standard_mode_disables_shortcuts() {
    let db = tmp_db("stdmode");
    run(&db, &["run", "--", "echo", "hi"]);

    let cfg = cfg_path("standard");
    std::fs::write(&cfg, "mode = standard\n").unwrap();

    // negative index rejected
    let neg = run_cfg(&db, &cfg, &["cmd", "-1"]);
    assert!(!neg.status.success(), "negative index should fail in standard mode");

    // bare-duration date rejected
    let bare = run_cfg(&db, &cfg, &["date:3day"]);
    assert!(!bare.status.success(), "date:3day should fail in standard mode");

    // explicit forms still work
    assert!(run_cfg(&db, &cfg, &["cmd", "1"]).status.success());
    assert!(run_cfg(&db, &cfg, &["date:today-3day"]).status.success());

    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn quick_mode_default_allows_shortcuts() {
    let db = tmp_db("quickmode");
    run(&db, &["run", "--", "echo", "hi"]);
    // no config file => default quick mode
    assert_eq!(stdout(&run(&db, &["cmd", "-1"])), "echo hi\n");
}

#[test]
fn comments_set_display_filter_and_show() {
    let db = tmp_db("comment");
    run(&db, &["run", "--", "echo", "one"]);
    run(&db, &["run", "--", "echo", "two"]);

    // `recover <id> com <text>` sets, `recover <id> com` prints
    run(&db, &["1", "com", "important note"]);
    assert_eq!(stdout(&run(&db, &["1", "com"])), "important note\n");

    // com: filter finds only the annotated run
    let listed = stdout(&run(&db, &["com:important"]));
    assert!(listed.contains("echo one"), "listed: {listed}");
    assert!(!listed.contains("echo two"));

    // show includes the comment
    assert!(stdout(&run(&db, &["show", "1"])).contains("comment:  important note"));

    // negative index works for comments too (quick mode default)
    run(&db, &["-1", "com", "the latest"]);
    assert_eq!(stdout(&run(&db, &["2", "com"])), "the latest\n");

    // bare `recover <id>` shows the run
    assert!(stdout(&run(&db, &["1"])).contains("command:  echo one"));
}

#[test]
fn id_first_out_cmd_and_show_forms() {
    let db = tmp_db("idfirst");
    run(&db, &["run", "--", "echo", "hello"]);
    assert_eq!(stdout(&run(&db, &["1", "cmd"])), "echo hello\n");
    assert_eq!(stdout(&run(&db, &["1", "out", "--strip"])), "hello\n");
    assert!(stdout(&run(&db, &["1", "show"])).contains("command:  echo hello"));
    // unknown action reports an error
    assert!(!run(&db, &["1", "bogus"]).status.success());
}

#[test]
fn list_word_and_dash_l_are_identical_and_paged() {
    let db = tmp_db("browse");
    run(&db, &["run", "--", "echo", "alpha"]);
    run(&db, &["run", "--", "echo", "beta"]);

    // stdout is piped in tests (not a TTY), so both forms write the listing plainly.
    let via_word = stdout(&run(&db, &["list", "cmd:e"]));
    let via_flag = stdout(&run(&db, &["cmd:e", "-l"]));
    assert_eq!(via_word, via_flag, "`list X` and `X -l` must be identical");

    // Both are the ordinary listing: table header plus one row per matching command.
    assert!(via_word.contains("STARTED") && via_word.contains("COMMAND"), "header: {via_word}");
    assert!(via_word.contains("echo alpha") && via_word.contains("echo beta"), "rows: {via_word}");
}

#[test]
fn browse_respects_filters_in_either_position() {
    let db = tmp_db("browsefilter");
    run(&db, &["run", "--", "echo", "alpha"]);
    run(&db, &["run", "--", "echo", "beta"]);

    // `-l` after the filter
    let a = stdout(&run(&db, &["cmd:alpha", "-l"]));
    assert!(a.contains("echo alpha") && !a.contains("echo beta"), "cmd:alpha -l: {a}");

    // the explicit `list` word, and `list` + `-l` together
    let b = stdout(&run(&db, &["list", "cmd:beta"]));
    assert!(b.contains("echo beta") && !b.contains("echo alpha"), "list cmd:beta: {b}");
    let c = stdout(&run(&db, &["list", "-l", "cmd:beta"]));
    assert!(c.contains("echo beta") && !c.contains("echo alpha"), "list -l cmd:beta: {c}");
}

#[test]
fn bare_list_is_capped_but_browse_is_uncapped() {
    let db = tmp_db("browsecap");
    for i in 0..12 {
        run(&db, &["run", "--", "echo", &format!("line{i}")]);
    }
    // bare `recover` is the quick list: 10 data rows (11 lines incl. the header)
    let plain = stdout(&run(&db, &[]));
    assert_eq!(plain.lines().count(), 11, "bare recover should cap at 10 rows: {plain}");
    assert!(!plain.contains("echo line0"), "oldest run should be capped out of the quick list");

    // `recover list` and `-l` have no cap: header + all 12 rows = 13 lines
    let word = stdout(&run(&db, &["list"]));
    let flag = stdout(&run(&db, &["-l"]));
    assert_eq!(word.lines().count(), 13, "list should show every run: {word}");
    assert_eq!(flag.lines().count(), 13, "-l should show every run: {flag}");
    assert!(word.contains("echo line0"), "oldest run should be present: {word}");
}

#[test]
fn config_limit_sets_quick_list_size() {
    let db = tmp_db("cfglimit");
    for i in 0..5 {
        run(&db, &["run", "--", "echo", &format!("row{i}")]);
    }
    let cfg = cfg_path("limit3");
    std::fs::write(&cfg, "limit = 3\n").unwrap();

    // bare `recover` honors the configured default: header + 3 rows = 4 lines
    let quick = stdout(&run_cfg(&db, &cfg, &[]));
    assert_eq!(quick.lines().count(), 4, "quick list should show 3 rows: {quick}");

    // `recover list` ignores the cap and shows all 5 (header + 5 = 6 lines)
    let full = stdout(&run_cfg(&db, &cfg, &["list"]));
    assert_eq!(full.lines().count(), 6, "list should be uncapped: {full}");

    // a limit: filter still overrides per-invocation
    let one = stdout(&run_cfg(&db, &cfg, &["limit:1"]));
    assert_eq!(one.lines().count(), 2, "limit:1 should override: {one}");

    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn del_removes_multiple_ids() {
    let db = tmp_db("delmulti");
    run(&db, &["run", "--", "echo", "one"]);
    run(&db, &["run", "--", "echo", "two"]);
    run(&db, &["run", "--", "echo", "three"]);

    let out = run(&db, &["del", "1", "3"]);
    assert!(out.status.success(), "del should succeed");
    let text = stdout(&out);
    assert!(text.contains("deleted run 1") && text.contains("deleted run 3"), "del output: {text}");

    // only run 2 remains
    let remaining = stdout(&run(&db, &["limit:0"]));
    assert!(remaining.contains("echo two"), "run 2 should remain: {remaining}");
    assert!(!remaining.contains("echo one") && !remaining.contains("echo three"), "1 and 3 gone");
}

#[test]
fn rm_is_an_alias_and_missing_id_errors() {
    let db = tmp_db("delalias");
    run(&db, &["run", "--", "echo", "keep"]);
    run(&db, &["run", "--", "echo", "drop"]);

    // `rm` still works as an alias of `del`
    assert!(run(&db, &["rm", "2"]).status.success());
    assert!(!stdout(&run(&db, &["limit:0"])).contains("echo drop"));

    // deleting a non-existent id fails with a clear message
    let missing = run(&db, &["del", "999"]);
    assert!(!missing.status.success(), "del of a missing id should fail");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no run with id 999"));
}

#[test]
fn config_path_reports_the_override() {
    let db = tmp_db("cfgpath");
    let cfg = cfg_path("cfgpath");
    let out = run_cfg(&db, &cfg, &["config", "path"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout(&out).trim(), cfg.to_str().unwrap());
}

#[test]
fn config_rejects_an_unknown_subcommand() {
    let db = tmp_db("cfgbad");
    let out = run(&db, &["config", "bogus"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown config subcommand"), "stderr: {err}");
}

fn mode(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_database_is_private() {
    let db = tmp_db("perm-new");
    run(&db, &["run", "--", "true"]);
    assert_eq!(mode(&db), 0o600, "db file mode");
}

#[test]
fn an_existing_world_readable_database_is_tightened() {
    let db = tmp_db("perm-old");
    run(&db, &["run", "--", "true"]);
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
    run(&db, &[]);
    assert_eq!(mode(&db), 0o600, "db file mode after reopening");
}

#[test]
fn a_created_data_directory_is_private() {
    let mut root = std::env::temp_dir();
    root.push(format!("recover-it-{}-permdir", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let db = root.join("sub").join("recover.db");
    run(&db, &["run", "--", "true"]);
    assert_eq!(mode(&root.join("sub")), 0o700, "created dir mode");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_export_file_is_private() {
    let db = tmp_db("perm-export");
    run(&db, &["run", "--", "true"]);
    let mut json = std::env::temp_dir();
    json.push(format!("recover-it-{}-perm-export.json", std::process::id()));
    let _ = std::fs::remove_file(&json);
    run(&db, &["export", json.to_str().unwrap()]);
    assert_eq!(mode(&json), 0o600, "export file mode");
    let _ = std::fs::remove_file(&json);
}

#[test]
fn exporting_to_a_directory_fails_without_touching_its_mode() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("recover-it-{}-export-dir", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let db = tmp_db("export-dir");
    let out = run(&db, &["export", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "exporting onto a directory must fail");
    assert_eq!(mode(&dir), 0o755, "the directory's mode must be left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_database_path_that_is_a_directory_is_not_chmodded() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("recover-it-{}-db-dir", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = run(&dir, &[]);
    assert!(!out.status.success());
    assert_eq!(mode(&dir), 0o755, "the directory's mode must be left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn db_path_reports_the_override() {
    let db = tmp_db("dbpath");
    let out = run(&db, &["db", "path"]);
    assert_eq!(stdout(&out).trim(), db.to_str().unwrap());
}
