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
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The result of running a command.
pub struct Recorded {
    pub output: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    /// The status to exit with, shell style: the exit code, or 128 + the signal number.
    pub status: i32,
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

/// How often to check on the child while its output is quiet.
const POLL: Duration = Duration::from_millis(25);
/// After the child exits, stop once its output has been quiet this long...
const DRAIN_IDLE: Duration = Duration::from_millis(200);
/// ...or this long after the exit at the latest, even if output keeps coming.
const DRAIN_MAX: Duration = Duration::from_secs(2);

/// Copy output to our stdout and keep a copy, until the child has exited and its output
/// has drained. Returns the captured bytes and the child's exit status.
///
/// EOF alone is not a reliable end: a background job the command started can hold the
/// output open long after the command itself exited. So once the child is gone, stop at
/// EOF, after `DRAIN_IDLE` of quiet, or at `DRAIN_MAX`, whichever comes first. The
/// reader thread is left behind; it ends with the process.
fn pump(rx: &mpsc::Receiver<Vec<u8>>, child: &mut std::process::Child) -> Result<(Vec<u8>, ExitStatus)> {
    let mut stdout = std::io::stdout();
    let mut captured = Vec::new();
    let mut exited: Option<(ExitStatus, Instant)> = None;
    let mut last_output = Instant::now();
    loop {
        match rx.recv_timeout(POLL) {
            Ok(chunk) => {
                captured.extend_from_slice(&chunk);
                let _ = stdout.write_all(&chunk);
                let _ = stdout.flush();
                last_output = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // EOF: everything was read
        }
        match exited {
            None => {
                if let Some(status) = child.try_wait()? {
                    exited = Some((status, Instant::now()));
                    // Give output written just before the exit time to arrive.
                    last_output = Instant::now();
                }
            }
            Some((_, at)) => {
                if last_output.elapsed() >= DRAIN_IDLE || at.elapsed() >= DRAIN_MAX {
                    break;
                }
            }
        }
    }
    let status = match exited {
        Some((status, _)) => status,
        None => child.wait()?,
    };
    Ok((captured, status))
}

/// Split an exit status into (exit code, signal name, shell-style status). A command
/// killed by a signal has no exit code; like a shell, report 128 + the signal number.
fn status_parts(status: std::process::ExitStatus) -> (Option<i32>, Option<String>, i32) {
    match (status.code(), status.signal()) {
        (Some(code), _) => (Some(code), None, code),
        (None, Some(sig)) => (None, Some(signal_name(sig)), 128 + sig),
        (None, None) => (None, None, 1),
    }
}

/// The C library's description of a signal (`Terminated`, `Killed`, ...).
fn signal_name(sig: i32) -> String {
    // SAFETY: strsignal returns a pointer to a NUL-terminated string (or null), which we
    // copy out before anything else can call it.
    let p = unsafe { libc::strsignal(sig) };
    if p.is_null() {
        return format!("Signal {sig}");
    }
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn recorded(
    output: Vec<u8>,
    (exit_code, signal, status): (Option<i32>, Option<String>, i32),
    started_at: DateTime<Utc>,
    start: Instant,
) -> Recorded {
    Recorded {
        output,
        exit_code,
        signal,
        status,
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

    let (output, status) = pump(&spawn_reader(reader), &mut child)?;
    Ok(recorded(output, status_parts(status), started_at, start))
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
    // On unix portable-pty spawns a plain std child. Use it directly: portable-pty's own
    // exit status keeps only the signal's name, and we need its number for 128 + n.
    let child = (child.as_mut() as &mut dyn portable_pty::Child)
        .downcast_mut::<std::process::Child>()
        .context("unexpected child process type from the PTY system")?;

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

    let (output, status) = pump(&rx, child)?;
    drop(raw); // restore cooked mode before we print anything else
    Ok(recorded(output, status_parts(status), started_at, start))
}
