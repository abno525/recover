//! Run a command, relaying its output live while capturing it.
//!
//! With a terminal on stdin the command runs in a pseudo-terminal, so it behaves as if run
//! directly in the terminal (colors, interactive TUIs) and keystrokes are forwarded to it.
//! With piped or redirected input it runs without one: stdin passes straight through and
//! stdout+stderr are captured from a pipe, so programs may turn their colors off.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::time::Instant;

/// The result of running a command.
pub struct Recorded {
    pub output: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
}

/// Restores terminal cooked mode on drop.
struct RawGuard;

impl RawGuard {
    fn enable() -> Option<RawGuard> {
        crossterm::terminal::enable_raw_mode().ok().map(|()| RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn terminal_dims() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

/// Execute `argv` (program + args), streaming its output live and capturing stdout and
/// stderr combined.
pub fn run_command(argv: &[String]) -> Result<Recorded> {
    anyhow::ensure!(!argv.is_empty(), "no command given");
    if std::io::stdin().is_terminal() {
        run_in_pty(argv)
    } else {
        run_piped(argv)
    }
}

/// Read `reader` to EOF on its own thread, sending each chunk to the returned channel.
fn spawn_reader(mut reader: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    rx
}

/// Copy every chunk to our stdout and keep it, until the reader reaches EOF.
fn relay(rx: mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut stdout = std::io::stdout();
    let mut captured = Vec::new();
    for chunk in rx {
        captured.extend_from_slice(&chunk);
        let _ = stdout.write_all(&chunk);
        let _ = stdout.flush();
    }
    captured
}

fn recorded(
    output: Vec<u8>,
    (exit_code, signal): (Option<i32>, Option<String>),
    started_at: DateTime<Utc>,
    start: Instant,
) -> Recorded {
    Recorded {
        output,
        exit_code,
        signal,
        started_at,
        finished_at: Utc::now(),
        duration_ms: start.elapsed().as_millis() as i64,
    }
}

/// No terminal on stdin: inherit stdin, capture stdout+stderr through one pipe.
fn run_piped(argv: &[String]) -> Result<Recorded> {
    let (reader, writer) = std::io::pipe()?;
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdout(writer.try_clone()?).stderr(writer);

    let started_at = Utc::now();
    let start = Instant::now();
    let mut child = cmd.spawn().with_context(|| format!("failed to start `{}`", argv[0]))?;
    // `Command` keeps its copies of the pipe's write end: drop them, or EOF never arrives.
    drop(cmd);

    let output = relay(spawn_reader(reader));
    let status = child.wait()?;
    let parts = (status.code(), status.signal().map(|n| format!("signal {n}")));
    Ok(recorded(output, parts, started_at, start))
}

/// A terminal on stdin: run in a PTY, forwarding keystrokes in raw mode.
fn run_in_pty(argv: &[String]) -> Result<Recorded> {
    let (cols, rows) = terminal_dims();
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let started_at = Utc::now();
    let start = Instant::now();
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to start `{}`", argv[0]))?;
    // Drop the slave in the parent so the master sees EOF once the child exits.
    drop(pair.slave);

    let rx = spawn_reader(pair.master.try_clone_reader()?);
    let raw = RawGuard::enable();
    let mut writer = pair.master.take_writer()?;
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut chunk = [0u8; 8192];
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if writer.write_all(&chunk[..n]).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
            }
        }
    });

    let output = relay(rx);
    let status = child.wait()?;
    drop(raw); // restore cooked mode before we print anything else

    let parts = match status.signal() {
        Some(sig) => (None, Some(sig.to_string())),
        None => (Some(status.exit_code() as i32), None),
    };
    Ok(recorded(output, parts, started_at, start))
}
