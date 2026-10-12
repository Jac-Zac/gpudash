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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ClusterConf {
    /// ssh host alias
    pub host: String,
    /// folder prefix hidden in the table
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub refresh_seconds: u64,
    pub ended_jobs: Ended,
    pub recent_ended: usize,
    pub notifications: bool,
    pub notify_on_queue: bool,
    pub notify_on_start: bool,
    pub notify_sound: bool,
    pub temperature_unit: TempUnit,
    pub clock: ClockFormat,
    pub temperature_graph: bool,
    #[serde(rename = "cluster")]
    pub clusters: Vec<ClusterConf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".into(),
            refresh_seconds: 30,
            ended_jobs: Ended::Recent,
            recent_ended: 5,
            notifications: true,
            notify_on_queue: true,
            notify_on_start: true,
            notify_sound: false,
            temperature_unit: TempUnit::C,
            clock: ClockFormat::H12,
            temperature_graph: true,
            clusters: Vec::new(),
        }
    }
}

const HEADER: &str = "\
# gpudash settings. Edit by hand, or press p in gpudash to change them from the command palette.
#
# theme              a theme from the themes/ folder next to this file (\"default\", \"light\", \"nord\"…)
# refresh_seconds    how often to poll the clusters
# ended_jobs         \"recent\", \"all\" or \"hidden\": jobs that ended in the last 24h
# recent_ended       how many ended jobs per cluster \"recent\" shows
# notifications      macOS notifications when jobs start / end
# notify_on_queue    notify when new jobs show up in the queue
# notify_on_start    notify when a queued job starts
# notify_sound       play a sound with notifications
# temperature_unit   \"C\" or \"F\"
# clock              \"12h\" or \"24h\"
# temperature_graph  show the temperature graph for running jobs
#
# [[cluster]]        one block per cluster
# host               ssh host alias from ~/.ssh/config
# prefix             (optional) folder prefix to hide in the table

";

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("gpudash"))
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.toml"))
}

/// Load the config. Errors are returned as text so they can be shown in the app.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = path() else { return (Config::default(), None) };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{}: {}", path.display(), e.message()))),
        },
        Err(_) => (migrate_old().unwrap_or_default(), None),
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
    let path = path().ok_or("no home folder")?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let body = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{HEADER}{body}")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let c = Config {
            clusters: vec![ClusterConf { host: "explorer".into(), prefix: "/projects/me/".into() }],
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
    fn missing_fields_use_defaults() {
        let c: Config = toml::from_str("[[cluster]]\nhost = \"x\"\n").unwrap();
        assert_eq!(c.refresh_seconds, 30);
        assert_eq!(c.clusters[0].prefix, "");
    }
}
