//! Database location resolution and the settings file.

use crate::render::Rgb;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Whether convenience shortcuts (negative indices, bare-duration dates) are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Quick,
    Standard,
}

/// When to colorize the list table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    On,
    Off,
}

/// Parsed settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub mode: Mode,
    pub color: ColorMode,
    pub bar_color: Option<Rgb>,
    pub header_color: Option<Rgb>,
    /// Default number of rows shown by bare `recover` (0 = no cap). `list`/`-l` ignore it.
    pub limit: usize,
    /// Most bytes of output stored per run (0 = no cap); past it the start and end are kept.
    pub max_output: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Quick,
            color: ColorMode::Auto,
            bar_color: None,
            header_color: None,
            limit: 10,
            max_output: 16 * 1024 * 1024,
        }
    }
}

impl Config {
    /// True when shortcuts (negative indices, `date:3day`) are enabled.
    pub fn shortcuts(&self) -> bool {
        matches!(self.mode, Mode::Quick)
    }
}

/// Parse the config file contents: `key = value` lines, `#` full-line comments.
pub fn parse(text: &str) -> Result<Config, String> {
    let mut cfg = Config::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, val) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected `key = value`", i + 1))?;
        let (key, val) = (key.trim(), val.trim());
        match key {
            "mode" => {
                cfg.mode = match val {
                    "quick" => Mode::Quick,
                    "standard" => Mode::Standard,
                    other => return Err(format!("invalid mode `{other}` (quick|standard)")),
                }
            }
            "color" => {
                cfg.color = match val {
                    "auto" => ColorMode::Auto,
                    "on" | "true" | "always" => ColorMode::On,
                    "off" | "false" | "never" => ColorMode::Off,
                    other => return Err(format!("invalid color `{other}` (auto|on|off)")),
                }
            }
            "bar_color" => cfg.bar_color = Some(Rgb::parse_hex(val)?),
            "header_color" => cfg.header_color = Some(Rgb::parse_hex(val)?),
            "limit" => {
                cfg.limit = val
                    .parse()
                    .map_err(|_| format!("invalid limit `{val}` (expected a non-negative integer)"))?
            }
            "max_output" => cfg.max_output = parse_size(val)?,
            other => return Err(format!("unknown config key `{other}`")),
        }
    }
    Ok(cfg)
}

/// Parse a byte count: plain digits, or digits followed by `K`, `M` or `G` (powers of 1024).
fn parse_size(val: &str) -> Result<usize, String> {
    let err = || format!("invalid max_output `{val}` (expected bytes, e.g. 16M, 512K or 0)");
    let (digits, unit) = match val.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&val[..i], c.to_ascii_uppercase()),
        _ => (val, ' '),
    };
    let factor: usize = match unit {
        ' ' => 1,
        'K' => 1 << 10,
        'M' => 1 << 20,
        'G' => 1 << 30,
        _ => return Err(err()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    digits.parse::<usize>().ok().and_then(|n| n.checked_mul(factor)).ok_or_else(err)
}

/// Resolve the config path from explicit inputs (empty values count as unset):
/// `$RECOVER_CONFIG`, else `$XDG_CONFIG_HOME/recover/config`, else
/// `$HOME/.config/recover/config`. Split out from [`config_path`] so it is testable
/// without mutating the process environment.
pub fn resolve_config_path(
    env_override: Option<&str>,
    xdg_config: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    if let Some(p) = env_override.filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let base = match xdg_config.filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(home.filter(|h| !h.is_empty()).unwrap_or(".")).join(".config"),
    };
    base.join("recover").join("config")
}

/// The config file path for this process's environment.
pub fn config_path() -> PathBuf {
    resolve_config_path(
        std::env::var("RECOVER_CONFIG").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Load config, falling back to defaults when the file is missing. A parse error is
/// reported to stderr and defaults are used, so a typo never breaks the tool.
pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => match parse(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("recover: {}: {e}", config_path().display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Resolve the database path: `$RECOVER_DB`, else `$XDG_DATA_HOME/recover/recover.db`,
/// else `~/.local/share/recover/recover.db`.
pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("RECOVER_DB")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    let base = match std::env::var("XDG_DATA_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local").join("share")
        }
    };
    base.join("recover").join("recover.db")
}

/// Ensure the parent directory of `path` exists. Directories created here are private
/// (0700); existing ones are left as they are.
pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(dir)
            .with_context(|| format!("creating data directory {}", dir.display()))?;
    }
    Ok(())
}

/// Make sure `path` exists and only its owner can read it: create it 0600, or tighten an
/// existing file that group/others can access. Run history holds every recorded command
/// line and its output, so it gets the same treatment as a shell history file.
pub fn ensure_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        match std::fs::metadata(path) {
            Ok(meta) if meta.permissions().mode() & 0o077 != 0 => {
                // Best effort: a file we can open but don't own stays usable.
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(path)
                    .with_context(|| format!("creating {}", path.display()))?;
            }
            Err(e) => return Err(e).with_context(|| format!("checking {}", path.display())),
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_defaults() {
        let cfg = parse("# a comment\n\n   \n").unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.mode, Mode::Quick);
        assert_eq!(cfg.color, ColorMode::Auto);
    }

    #[test]
    fn parses_mode_color_and_hex() {
        let cfg = parse("mode = standard\ncolor = off\nbar_color = #00005f\nheader_color = ffcc00\n").unwrap();
        assert_eq!(cfg.mode, Mode::Standard);
        assert_eq!(cfg.color, ColorMode::Off);
        assert_eq!(cfg.bar_color, Some(Rgb { r: 0, g: 0, b: 0x5f }));
        assert_eq!(cfg.header_color, Some(Rgb { r: 0xff, g: 0xcc, b: 0 }));
        assert!(!cfg.shortcuts());
    }

    #[test]
    fn limit_defaults_to_ten_and_parses() {
        assert_eq!(Config::default().limit, 10);
        assert_eq!(parse("limit = 25\n").unwrap().limit, 25);
        assert_eq!(parse("limit = 0\n").unwrap().limit, 0); // 0 = no cap
    }

    #[test]
    fn max_output_defaults_to_16_mib_and_takes_suffixes() {
        assert_eq!(Config::default().max_output, 16 * 1024 * 1024);
        let size = |v: &str| parse(&format!("max_output = {v}\n")).map(|c| c.max_output);
        assert_eq!(size("512K"), Ok(512 * 1024));
        assert_eq!(size("16M"), Ok(16 * 1024 * 1024));
        assert_eq!(size("1g"), Ok(1024 * 1024 * 1024));
        assert_eq!(size("1234"), Ok(1234));
        assert_eq!(size("0"), Ok(0)); // no cap
        for bad in ["", "abc", "12X", "-1", "1.5M", "99999999999999999999G"] {
            assert!(size(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn rejects_bad_values_and_keys() {
        assert!(parse("mode = fancy").is_err());
        assert!(parse("color = rainbow").is_err());
        assert!(parse("bar_color = nothex").is_err());
        assert!(parse("limit = notanumber").is_err());
        assert!(parse("unknown = 1").is_err());
        assert!(parse("noequalssign").is_err());
    }

    // --- path resolution: inputs are passed in, so no test touches the environment ---

    fn resolve(env: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> String {
        resolve_config_path(env, xdg, home).display().to_string()
    }

    #[test]
    fn env_override_wins_and_empty_means_unset() {
        assert_eq!(resolve(Some("/tmp/explicit"), Some("/cfg"), Some("/home/u")), "/tmp/explicit");
        assert_eq!(resolve(Some(""), None, Some("/home/u")), "/home/u/.config/recover/config");
    }

    #[test]
    fn xdg_config_home_is_honored_when_set() {
        assert_eq!(resolve(None, Some("/cfg"), Some("/home/u")), "/cfg/recover/config");
        assert_eq!(resolve(None, Some(""), Some("/home/u")), "/home/u/.config/recover/config");
    }

    #[test]
    fn default_is_under_home_dot_config() {
        assert_eq!(resolve(None, None, Some("/home/u")), "/home/u/.config/recover/config");
        assert_eq!(resolve(None, None, None), "./.config/recover/config");
    }
}
