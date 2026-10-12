//! Color themes.
//!
//! A theme is a TOML file of colors. Built-in themes live in `themes/` in the repo and are
//! copied to ~/.config/gpudash/themes/ on first run; any `.toml` file there can be picked with
//! `theme = "name"` in config.toml or from the command palette. A file with the same name as a
//! built-in theme replaces it.

use std::path::PathBuf;
use std::sync::RwLock;

use ratatui::style::Color;
use serde::Deserialize;

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// None keeps the terminal's own background
    pub background: Option<Color>,
    pub text: Color,
    pub dim: Color,
    pub faint: Color,
    pub accent: Color,
    pub selection: Color,
    pub track: Color,
    pub running: Color,
    pub queued: Color,
    pub done: Color,
    pub failed: Color,
    pub warning: Color,
    /// colors at 30°, 50°, 65°, 78°, 88°
    pub temperature: [Rgb; 5],
    pub util: [Rgb; 2],
    pub memory: [Rgb; 2],
    pub log_number: Color,
    pub log_string: Color,
    pub log_path: Color,
    pub log_key: Color,
    pub log_error: Color,
    pub log_warning: Color,
    pub log_bar: Color,
}

const fn c(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Same colors as themes/default.toml.
pub const DEFAULT: Theme = Theme {
    background: None,
    text: c(215, 220, 230),
    dim: c(110, 115, 130),
    faint: c(60, 64, 76),
    accent: c(120, 170, 255),
    selection: c(38, 42, 54),
    track: c(44, 48, 60),
    running: c(80, 220, 140),
    queued: c(240, 200, 80),
    done: c(90, 165, 125),
    failed: c(255, 80, 80),
    warning: c(255, 150, 60),
    temperature: [(90, 160, 255), (80, 220, 140), (240, 220, 80), (255, 140, 50), (255, 50, 60)],
    util: [(255, 125, 70), (255, 60, 160)],
    memory: [(255, 215, 100), (255, 145, 95)],
    log_number: c(215, 160, 255),
    log_string: c(150, 215, 140),
    log_path: c(110, 200, 215),
    log_key: c(140, 160, 200),
    log_error: c(255, 150, 150),
    log_warning: c(240, 215, 150),
    log_bar: c(80, 220, 140),
};

static CURRENT: RwLock<Theme> = RwLock::new(DEFAULT);

/// The active theme.
pub fn t() -> Theme {
    *CURRENT.read().unwrap()
}

pub const BUILTIN: &[(&str, &str)] = &[
    ("default", include_str!("../themes/default.toml")),
    ("light", include_str!("../themes/light.toml")),
    ("catppuccin-mocha", include_str!("../themes/catppuccin-mocha.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("tokyo-night", include_str!("../themes/tokyo-night.toml")),
];

fn dir() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("themes")))
}

pub fn folder() -> Option<PathBuf> {
    dir()
}

/// On first run, copy the built-in themes into the config folder as examples to edit.
pub fn write_examples() {
    let Some(dir) = dir() else { return };
    if dir.exists() || std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, text) in BUILTIN {
        let _ = std::fs::write(dir.join(format!("{name}.toml")), text);
    }
}

/// All theme names: files in the themes folder plus the built-ins, sorted, default first.
pub fn available() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN.iter().map(|(n, _)| n.to_string()).collect();
    if let Some(entries) = dir().and_then(|d| std::fs::read_dir(d).ok()) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "toml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    if let Some(i) = names.iter().position(|n| n == "default") {
        let d = names.remove(i);
        names.insert(0, d);
    }
    names
}

/// Load a theme by name (folder first, then built-in) and make it active.
pub fn apply(name: &str) -> Result<(), String> {
    let theme = load(name)?;
    *CURRENT.write().unwrap() = theme;
    Ok(())
}

fn load(name: &str) -> Result<Theme, String> {
    let file = dir().map(|d| d.join(format!("{name}.toml")));
    if let Some(text) = file.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
        return parse(&text).map_err(|e| format!("themes/{name}.toml: {e}"));
    }
    match BUILTIN.iter().find(|(n, _)| *n == name) {
        Some((_, text)) => parse(text),
        None => Err(format!("no theme called \"{name}\" (looked in {})", short(file))),
    }
}

fn short(p: Option<PathBuf>) -> String {
    let p = p.map(|p| p.display().to_string()).unwrap_or_default();
    match std::env::var("HOME") {
        Ok(home) if p.starts_with(&home) => p.replacen(&home, "~", 1),
        _ => p,
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    background: Option<String>,
    text: Option<String>,
    dim: Option<String>,
    faint: Option<String>,
    accent: Option<String>,
    selection: Option<String>,
    track: Option<String>,
    running: Option<String>,
    queued: Option<String>,
    done: Option<String>,
    failed: Option<String>,
    warning: Option<String>,
    temperature: Option<Vec<String>>,
    util: Option<Vec<String>>,
    memory: Option<Vec<String>>,
    log: LogFile,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct LogFile {
    number: Option<String>,
    string: Option<String>,
    path: Option<String>,
    key: Option<String>,
    error: Option<String>,
    warning: Option<String>,
    bar: Option<String>,
}

fn hex(key: &str, s: &str) -> Result<Rgb, String> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("x"), 16);
    match (h.len(), byte(0), byte(2), byte(4)) {
        (6, Ok(r), Ok(g), Ok(b)) => Ok((r, g, b)),
        _ => Err(format!("{key} = \"{s}\" isn't a color like \"#ff8800\"")),
    }
}

/// Parse a theme file. Missing keys keep the default colors.
pub fn parse(text: &str) -> Result<Theme, String> {
    let f: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let mut t = DEFAULT;
    let color = |slot: &mut Color, key: &str, v: &Option<String>| -> Result<(), String> {
        if let Some(s) = v {
            let (r, g, b) = hex(key, s)?;
            *slot = Color::Rgb(r, g, b);
        }
        Ok(())
    };
    if let Some(bg) = f.background.as_deref().filter(|s| !s.trim().is_empty()) {
        let (r, g, b) = hex("background", bg)?;
        t.background = Some(Color::Rgb(r, g, b));
    }
    color(&mut t.text, "text", &f.text)?;
    color(&mut t.dim, "dim", &f.dim)?;
    color(&mut t.faint, "faint", &f.faint)?;
    color(&mut t.accent, "accent", &f.accent)?;
    color(&mut t.selection, "selection", &f.selection)?;
    color(&mut t.track, "track", &f.track)?;
    color(&mut t.running, "running", &f.running)?;
    color(&mut t.queued, "queued", &f.queued)?;
    color(&mut t.done, "done", &f.done)?;
    color(&mut t.failed, "failed", &f.failed)?;
    color(&mut t.warning, "warning", &f.warning)?;
    color(&mut t.log_number, "log.number", &f.log.number)?;
    color(&mut t.log_string, "log.string", &f.log.string)?;
    color(&mut t.log_path, "log.path", &f.log.path)?;
    color(&mut t.log_key, "log.key", &f.log.key)?;
    color(&mut t.log_error, "log.error", &f.log.error)?;
    color(&mut t.log_warning, "log.warning", &f.log.warning)?;
    color(&mut t.log_bar, "log.bar", &f.log.bar)?;

    fn list<const N: usize>(key: &str, v: &Option<Vec<String>>, slot: &mut [Rgb; N]) -> Result<(), String> {
        let Some(v) = v else { return Ok(()) };
        if v.len() != N {
            return Err(format!("{key} needs exactly {N} colors"));
        }
        for (i, s) in v.iter().enumerate() {
            slot[i] = hex(key, s)?;
        }
        Ok(())
    }
    list("temperature", &f.temperature, &mut t.temperature)?;
    list("util", &f.util, &mut t.util)?;
    list("memory", &f.memory, &mut t.memory)?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        for (name, text) in BUILTIN {
            parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn default_file_matches_default_const() {
        let t = parse(BUILTIN[0].1).unwrap();
        assert_eq!(format!("{t:?}"), format!("{DEFAULT:?}"));
    }

    #[test]
    fn partial_theme_and_errors() {
        let t = parse("accent = \"#ff0000\"").unwrap();
        assert_eq!(t.accent, Color::Rgb(255, 0, 0));
        assert_eq!(t.text, DEFAULT.text);
        assert!(parse("accent = \"red\"").unwrap_err().contains("accent"));
        assert!(parse("acent = \"#ff0000\"").is_err());
        assert!(parse("util = [\"#ff0000\"]").unwrap_err().contains("2 colors"));
    }
}
