//! Database location resolution and the `~/.recoverc` settings file.

use crate::render::Rgb;
use anyhow::{Context, Result};
use std::path::PathBuf;

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

/// Parsed `~/.recoverc` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub mode: Mode,
    pub color: ColorMode,
    pub bar_color: Option<Rgb>,
    pub header_color: Option<Rgb>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Quick,
            color: ColorMode::Auto,
            bar_color: None,
            header_color: None,
        }
    }
}

impl Config {
    /// True when shortcuts (negative indices, `date:3day`) are enabled.
    pub fn shortcuts(&self) -> bool {
        matches!(self.mode, Mode::Quick)
    }
}

/// Parse the `~/.recoverc` contents: `key = value` lines, `#` full-line comments.
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
            other => return Err(format!("unknown config key `{other}`")),
        }
    }
    Ok(cfg)
}

/// Resolve the config path: `$RECOVER_CONFIG`, else `~/.recoverc`.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("RECOVER_CONFIG")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".recoverc")
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

/// Ensure the parent directory of `path` exists.
pub fn ensure_parent(path: &std::path::Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating data directory {}", dir.display()))?;
    }
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
    fn rejects_bad_values_and_keys() {
        assert!(parse("mode = fancy").is_err());
        assert!(parse("color = rainbow").is_err());
        assert!(parse("bar_color = nothex").is_err());
        assert!(parse("unknown = 1").is_err());
        assert!(parse("noequalssign").is_err());
    }
}
