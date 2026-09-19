//! End-to-end tests driving the compiled `recover` binary against a temp database.

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
    p.push(format!("recover-it-{}-{}.recoverc", std::process::id(), name));
    p
}

// Default runs point RECOVER_CONFIG at an absent file, so the developer's real
// ~/.recoverc never affects the tests (defaults: quick mode, color auto).
fn run(db: &PathBuf, args: &[&str]) -> Output {
    let none = cfg_path("absent");
    let _ = std::fs::remove_file(&none);
    run_cfg(db, &none, args)
}

fn run_cfg(db: &PathBuf, config: &PathBuf, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("RECOVER_DB", db)
        .env("RECOVER_CONFIG", config)
        .stdin(Stdio::null())
        .output()
        .expect("failed to execute recover")
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

    // raw output preserves the PTY line ending; --strip normalizes it
    let out = run(&db, &["out", "1", "--strip"]);
    assert_eq!(stdout(&out), "hello\n");

    let cmd = run(&db, &["cmd", "1"]);
    assert_eq!(stdout(&cmd), "echo hello\n");
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
fn db_path_reports_the_override() {
    let db = tmp_db("dbpath");
    let out = run(&db, &["db", "path"]);
    assert_eq!(stdout(&out).trim(), db.to_str().unwrap());
}
