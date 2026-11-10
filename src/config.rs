//! Settings, stored in ~/.config/gpudash/config.toml.
//! Changes made from the command palette are written back here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Ended {
    Recent,
    All,
    Hidden,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum TempUnit {
    C,
    F,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum ClockFormat {
    #[serde(rename = "12h")]
    H12,
    #[serde(rename = "24h")]
    H24,
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Slurm,
    Runpod,
}

impl Provider {
    fn is_slurm(&self) -> bool {
        *self == Provider::Slurm
    }
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default)]
pub struct ClusterConf {
    /// left out of the file for Slurm, so existing configs save exactly as before
    #[serde(skip_serializing_if = "Provider::is_slurm")]
    pub provider: Provider,
    /// SSH host alias, or an optional display label for Runpod.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host: String,
    /// folder prefix hidden in the table
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

impl ClusterConf {
    pub fn is_runpod(&self) -> bool {
        self.provider == Provider::Runpod
    }

    pub fn label(&self) -> &str {
        if self.host.is_empty() { "runpod" } else { &self.host }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub refresh_seconds: u64,
    pub background: bool,
    pub background_seconds: u64,
    pub ended_jobs: Ended,
    pub recent_ended: usize,
    pub notifications: bool,
    pub notify_on_queue: bool,
    pub notify_on_start: bool,
    pub notify_sound: bool,
    pub temperature_unit: TempUnit,
    pub clock: ClockFormat,
    pub temperature_graph: bool,
    pub flip_list_scroll: bool,
    pub flip_log_scroll: bool,
    #[serde(rename = "cluster")]
    pub clusters: Vec<ClusterConf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".into(),
            refresh_seconds: 30,
            background: true,
            background_seconds: 300,
            ended_jobs: Ended::Recent,
            recent_ended: 5,
            notifications: true,
            notify_on_queue: true,
            notify_on_start: true,
            notify_sound: false,
            temperature_unit: TempUnit::C,
            clock: ClockFormat::H12,
            temperature_graph: true,
            flip_list_scroll: true,
            flip_log_scroll: false,
            clusters: Vec::new(),
        }
    }
}

const HEADER: &str = "\
# gpudash settings. Edit by hand, or press p in gpudash to change them from the command palette.
#
# theme              a theme from the themes/ folder next to this file (\"default\", \"light\", \"nord\"…)
# refresh_seconds    how often to poll the clusters while the dashboard is open
# background         keep sending notifications after the dashboard closes (macOS)
# background_seconds how often to poll in the background
# ended_jobs         \"recent\", \"all\" or \"hidden\": jobs that ended in the last 24h
# recent_ended       how many ended jobs per cluster \"recent\" shows
# notifications      macOS notifications when jobs start / end
# notify_on_queue    notify when new jobs show up in the queue
# notify_on_start    notify when a queued job starts
# notify_sound       play a sound with notifications
# temperature_unit   \"C\" or \"F\"
# clock              \"12h\" or \"24h\"
# temperature_graph  show the temperature graph for running jobs
# flip_list_scroll   flip the wheel direction in the job list and palette
# flip_log_scroll    flip the wheel direction in the log view
#
# [[cluster]]        one block per cluster
# provider           \"slurm\" (default) or \"runpod\"
# host               ssh host alias; optional label for Runpod
# prefix             (optional) folder prefix to hide in the table

";

fn dir() -> Option<PathBuf> {
    // an empty or relative value would put the config under the current folder; ignore it (XDG spec)
    let absolute = |v: std::ffi::OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .and_then(absolute)
        .or_else(|| std::env::var_os("HOME").and_then(absolute).map(|h| h.join(".config")))?;
    Some(base.join("gpudash"))
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.toml"))
}

fn parse(path: &std::path::Path, text: &str) -> Result<Config, String> {
    let c = toml::from_str::<Config>(text).map_err(|e| format!("{}: {}", path.display(), e.message()))?;
    if c.clusters.iter().any(|c| c.provider == Provider::Slurm && c.host.trim().is_empty()) {
        return Err("Slurm clusters need a host".into());
    }
    Ok(c)
}

/// Load the config. Errors are returned as text so they can be shown in the app.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = path() else { return (Config::default(), None) };
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse(&path, &text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (migrate_old().unwrap_or_default(), None),
        Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
    }
}

/// Earlier versions used a plain `clusters` file with "host prefix" lines.
fn migrate_old() -> Option<Config> {
    let old = dir()?.join("clusters");
    let text = std::fs::read_to_string(&old).ok()?;
    let clusters = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.split_whitespace();
            ClusterConf {
                host: parts.next().unwrap_or_default().to_string(),
                prefix: parts.next().unwrap_or_default().to_string(),
                ..ClusterConf::default()
            }
        })
        .collect();
    let config = Config { clusters, ..Config::default() };
    if save(&config).is_ok() {
        let _ = std::fs::rename(&old, old.with_file_name("clusters.old"));
    }
    Some(config)
}

pub fn save(config: &Config) -> Result<(), String> {
    save_to(&path().ok_or("no home folder")?, config)
}

fn save_to(path: &std::path::Path, config: &Config) -> Result<(), String> {
    // A file that doesn't load was most likely mid-edit by hand. The config in memory is then
    // only the defaults, so writing it would throw away the user's clusters and settings.
    match std::fs::read_to_string(path) {
        Ok(text) => {
            parse(path, &text).map_err(|e| format!("not overwriting a config file with errors ({e})"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let body = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    // write a temporary file and rename it over the config, so a crash or full disk
    // can't leave a half-written config behind. Follow a symlinked config (dotfiles) so the
    // link itself stays in place.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, format!("{HEADER}{body}"))?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let c = Config {
            clusters: vec![ClusterConf { host: "explorer".into(), prefix: "/projects/me/".into(), ..ClusterConf::default() }],
            clock: ClockFormat::H24,
            ..Config::default()
        };
        let text = format!("{HEADER}{}", toml::to_string_pretty(&c).unwrap());
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.clusters, c.clusters);
        assert_eq!(back.clock, ClockFormat::H24);
        assert!(text.contains("[[cluster]]"));
    }

    #[test]
    fn save_replaces_good_files_and_keeps_broken_ones() {
        let dir = std::env::temp_dir().join(format!("gpudash-config-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mine = Config { clusters: vec![ClusterConf { host: "explorer".into(), ..ClusterConf::default() }], ..Config::default() };
        save_to(&path, &mine).unwrap();
        assert_eq!(parse(&path, &std::fs::read_to_string(&path).unwrap()).unwrap().clusters, mine.clusters);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            save_to(&path, &mine).unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // a hand edit with a typo loads as defaults; saving those must not wipe the file
        let broken = "refresh_seconds = \"oops\"\n[[cluster]]\nhost = \"explorer\"\n";
        std::fs::write(&path, broken).unwrap();
        assert!(save_to(&path, &Config::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary files left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let c: Config = toml::from_str("[[cluster]]\nhost = \"x\"\n").unwrap();
        assert_eq!(c.refresh_seconds, 30);
        assert_eq!(c.clusters[0].prefix, "");
    }

    #[test]
    fn mixed_providers_round_trip_and_slurm_saves_as_before() {
        let config: Config = toml::from_str("[[cluster]]\nhost = 'myriad-ng'\n[[cluster]]\nprovider = 'runpod'\n").unwrap();
        assert_eq!(config.clusters[0].provider, Provider::Slurm);
        assert_eq!(config.clusters[1].label(), "runpod");
        let saved = toml::to_string(&config).unwrap();
        let back: Config = toml::from_str(&saved).unwrap();
        assert_eq!(back.clusters, config.clusters);
        assert_eq!(saved.matches("provider").count(), 1, "only the Runpod block names a provider");
        // a per-cluster refresh_seconds left over from an earlier build is ignored, not an error
        let old: Config = toml::from_str("[[cluster]]\nprovider = 'runpod'\nrefresh_seconds = 0\n").unwrap();
        assert_eq!(old.clusters[0].provider, Provider::Runpod);
    }
}
