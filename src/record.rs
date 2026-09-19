//! Run a command inside a pseudo-terminal, relaying I/O live while capturing output.
//!
//! The child runs attached to a PTY so it behaves as if run directly in the terminal
//! (colors, interactive TUIs). Output is teed to our stdout and an in-memory buffer.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{IsTerminal, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The result of running a command in a PTY.
pub struct Recorded {
    pub output: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
}

/// Restores terminal cooked mode on drop.
struct RawGuard(bool);

impl RawGuard {
    fn enable() -> RawGuard {
        if std::io::stdin().is_terminal() && crossterm::terminal::enable_raw_mode().is_ok() {
            RawGuard(true)
        } else {
            RawGuard(false)
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

fn terminal_dims() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

/// Execute `argv` (program + args) in a PTY, streaming live and capturing combined output.
pub fn run_in_pty(argv: &[String]) -> Result<Recorded> {
    anyhow::ensure!(!argv.is_empty(), "no command given");

    let pty_system = native_pty_system();
    let (cols, rows) = terminal_dims();
    let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

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

    // Reader thread: PTY master -> our stdout + capture buffer.
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let mut reader = pair.master.try_clone_reader()?;
    let buf = Arc::clone(&buffer);
    let reader_thread = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&chunk[..n]);
                    let _ = stdout.flush();
                    if let Ok(mut b) = buf.lock() {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });

    // Only take over stdin when we have a real terminal: enter raw mode and relay
    // keystrokes to the PTY. With a non-interactive stdin (pipe, /dev/null, redirect)
    // we leave it alone — forwarding an immediately-closing stdin makes the PTY line
    // discipline emit spurious echo, and such commands rarely read stdin anyway.
    let interactive = std::io::stdin().is_terminal();
    let raw = if interactive { RawGuard::enable() } else { RawGuard(false) };
    if interactive {
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
    }

    let status = child.wait()?;
    drop(raw); // restore cooked mode before we print anything else
    let _ = reader_thread.join();

    let finished_at = Utc::now();
    let duration_ms = start.elapsed().as_millis() as i64;
    let output = Arc::try_unwrap(buffer)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();

    let (exit_code, signal) = match status.signal() {
        Some(sig) => (None, Some(sig.to_string())),
        None => (Some(status.exit_code() as i32), None),
    };

    Ok(Recorded { output, exit_code, signal, started_at, finished_at, duration_ms })
}
