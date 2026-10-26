//! Background notifications.
//!
//! After the first run, gpudash registers itself with launchd (macOS) as `gpudash daemon`, which
//! starts at login and keeps polling while the dashboard is closed, so notifications still
//! arrive. While the dashboard is open it writes a heartbeat file and the daemon stands by;
//! the dashboard sends notifications itself. Both read and write the same "last seen" snapshot
//! per cluster, so whichever one polls next notices what changed since the other one looked.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::notifier;
use crate::remote::{self, Job};

pub const LABEL: &str = "io.github.rollingro11.gpudash";
/// The dashboard counts as open if it touched its heartbeat this recently.
const ALIVE: Duration = Duration::from_secs(90);
/// How often the daemon wakes to check whether it's time to poll.
const TICK: Duration = Duration::from_secs(15);
/// Snapshots older than this aren't used to notify (they'd replay a whole day at once);
/// they just become the new starting point.
const STALE: Duration = Duration::from_secs(12 * 3600);

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/gpudash"))
}

// ---------- shared "last seen" snapshot ----------

#[derive(Serialize, Deserialize)]
struct Seen {
    saved: DateTime<Utc>,
    jobs: Vec<Job>,
}

fn seen_path(host: &str) -> Option<PathBuf> {
    cache_dir().map(|d| d.join(format!("seen-{host}.json")))
}

/// The jobs last seen on `host` and when, unless that's too old to compare against.
pub fn load_seen(host: &str) -> Option<(DateTime<Utc>, Vec<Job>)> {
    let text = std::fs::read_to_string(seen_path(host)?).ok()?;
    let seen: Seen = serde_json::from_str(&text).ok()?;
    let age = (Utc::now() - seen.saved).to_std().unwrap_or_default();
    (age < STALE).then_some((seen.saved, seen.jobs))
}

pub fn save_seen(host: &str, jobs: &[Job]) {
    let Some(path) = seen_path(host) else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let seen = Seen { saved: Utc::now(), jobs: jobs.to_vec() };
    if let Ok(text) = serde_json::to_string(&seen) {
        // write then rename, so a reader never sees half a file
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ---------- dashboard heartbeat ----------

fn heartbeat_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("dashboard-open"))
}

/// Called by the dashboard every few seconds while it's open.
pub fn heartbeat() {
    let Some(path) = heartbeat_path() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, std::process::id().to_string());
}

/// Called when the dashboard quits, so the daemon takes over right away.
pub fn heartbeat_stop() {
    if let Some(path) = heartbeat_path() {
        let _ = std::fs::remove_file(path);
    }
}

fn dashboard_open() -> bool {
    heartbeat_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ALIVE)
}

// ---------- the daemon ----------

fn log(msg: &str) {
    println!("{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    let _ = std::io::stdout().flush();
}

/// `gpudash daemon`: poll in the background and notify. Run by launchd; exits when
/// background mode is turned off.
pub fn run_daemon() -> ! {
    if let Some(dir) = cache_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("daemon-version"), env!("GPUDASH_COMMIT"));
    }
    log(&format!("gpudash daemon started ({})", env!("GPUDASH_COMMIT")));
    let mut last_poll: Option<SystemTime> = None;
    loop {
        let (config, err) = config::load();
        if let Some(e) = err {
            log(&format!("config error, will retry: {e}"));
        } else if !config.background {
            log("background mode is off, stopping");
            std::process::exit(0);
        } else if !config.notifications || config.clusters.is_empty() {
            // nothing to do; check the config again later
        } else if dashboard_open() {
            // the dashboard is polling and notifying; start fresh once it closes
            last_poll = None;
        } else {
            let every = Duration::from_secs(config.background_seconds.max(60));
            let due = last_poll.and_then(|t| t.elapsed().ok()).is_none_or(|e| e >= every);
            if due {
                poll_all(&config);
                last_poll = Some(SystemTime::now());
            }
        }
        thread::sleep(TICK);
    }
}

fn poll_all(config: &config::Config) {
    let handles: Vec<_> = config
        .clusters
        .iter()
        .map(|c| {
            let (host, config) = (c.host.clone(), config.clone());
            thread::spawn(move || poll_one(&host, &config))
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

fn poll_one(host: &str, config: &config::Config) {
    match remote::fetch_jobs(host) {
        Ok(snap) => {
            let notes = match load_seen(host) {
                Some((since, old)) => crate::app::job_changes(host, &old, &snap.jobs, Some(since), config),
                None => Vec::new(),
            };
            save_seen(host, &snap.jobs);
            for (title, body) in &notes {
                notifier::notify_blocking(title, body, config.notify_sound);
            }
            log(&format!("{host}: {} jobs, {} notifications", snap.jobs.len(), notes.len()));
        }
        Err(e) => log(&format!("{host}: {e}")),
    }
}

// ---------- launchd (macOS) ----------

fn plist_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

fn log_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs/gpudash.log"))
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("launchctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist(exe: &str, log: &str) -> String {
    let (exe, log) = (xml_escape(exe), xml_escape(log));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin</string>
    </dict>
</dict>
</plist>
"#
    )
}

pub fn running() -> bool {
    launchctl(&["print", &format!("gui/{}/{LABEL}", uid())])
}

/// Make sure the background daemon is registered and running this same build of gpudash.
/// Returns true if it was set up for the first time.
pub fn ensure() -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe).display().to_string();
    let plist_file = plist_path().ok_or("no home folder")?;
    let log = log_path().ok_or("no home folder")?;
    let want = plist(&exe, &log.display().to_string());
    let first = !plist_file.exists();
    let changed = std::fs::read_to_string(&plist_file).map_or(true, |have| have != want);
    let domain = format!("gui/{}", uid());
    let service = format!("{domain}/{LABEL}");

    if changed {
        if !first {
            launchctl(&["bootout", &service]);
        }
        std::fs::create_dir_all(plist_file.parent().unwrap()).map_err(|e| e.to_string())?;
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        std::fs::write(&plist_file, &want).map_err(|e| e.to_string())?;
    }
    if !running() && !launchctl(&["bootstrap", &domain, &plist_file.display().to_string()]) {
        return Err("couldn't start the background service (launchctl bootstrap failed)".into());
    }
    // after an update, restart the daemon so it runs the new build
    let daemon_version = cache_dir().and_then(|d| std::fs::read_to_string(d.join("daemon-version")).ok());
    if !changed && daemon_version.as_deref().is_some_and(|v| v != env!("GPUDASH_COMMIT")) {
        launchctl(&["kickstart", "-k", &service]);
    }
    Ok(first)
}

/// Stop the daemon and remove it from login items.
pub fn remove() {
    if !cfg!(target_os = "macos") {
        return;
    }
    launchctl(&["bootout", &format!("gui/{}/{LABEL}", uid())]);
    if let Some(p) = plist_path() {
        let _ = std::fs::remove_file(p);
    }
}

pub fn status_text() -> String {
    let (config, _) = config::load();
    let state = if !cfg!(target_os = "macos") {
        "not available on this system (macOS only)".to_string()
    } else if running() {
        format!("running, polling every {}s while the dashboard is closed", config.background_seconds)
    } else if config.background {
        "on, but not running yet (it starts the next time you open gpudash)".to_string()
    } else {
        "off".to_string()
    };
    let log = log_path().map(|p| p.display().to_string()).unwrap_or_default();
    format!("background notifications: {state}\nlog: {log}")
}
