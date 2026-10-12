//! App state: clusters, selection, popups, background fetches.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use std::sync::atomic::Ordering;

use crate::config::{self, ClockFormat, ClusterConf, Config, Ended, TempUnit};
use crate::notifier;
use crate::palette::{Cmd, Palette, Prompt, PromptKind};
use crate::remote::{self, Job, Log, Snapshot};
use crate::util::{CLOCK_24H, FAHRENHEIT};

const HISTORY_LEN: usize = 120;
const LOG_REFRESH: Duration = Duration::from_secs(3);

pub struct Cluster {
    /// stable id, so results from a fetch still land right after clusters are added or removed
    pub uid: usize,
    pub host: String,
    pub prefix: String,
    pub jobs: Option<Vec<Job>>,
    pub fetched: Option<Instant>,
    pub error: Option<String>,
    pub loading: bool,
    pub loading_since: Instant,
    /// job id -> (stdout, stderr) path patterns; kept on disk so logs of old jobs stay reachable
    pub outputs: HashMap<String, (String, String)>,
}

pub struct LogView {
    pub ci: usize,
    pub id: String,
    pub name: String,
    pub stderr: bool,
    pub log: Option<Log>,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Instant,
    /// first visible line; None = follow the end
    pub top: Option<usize>,
}

pub enum Popup {
    Help,
    ConfirmCancel { ci: usize, id: String, name: String },
}

/// What the event loop should do after a key press or command.
pub enum Next {
    Continue,
    Quit,
    Shell { host: String, workdir: String, running_id: Option<String> },
    EditConfig,
}

pub enum Msg {
    Jobs(usize, Result<Snapshot, String>), // (cluster uid, result)
    Log(String, bool, Result<Log, String>),
    Cancelled(String, Result<(), String>),
}

pub struct App {
    pub clusters: Vec<Cluster>,
    pub selected: Option<(usize, String)>, // (cluster index, job id)
    moved: bool,                           // user has moved the selection
    pub tick: usize,
    pub last_refresh: Instant,
    pub config: Config,
    /// clusters came from the command line, so cluster edits aren't saved
    cli_clusters: bool,
    next_uid: usize,
    pub paused: bool,
    pub palette: Option<Palette>,
    pub filter: String,
    pub filtering: bool,
    pub popup: Option<Popup>,
    pub log: Option<LogView>,
    pub toast: Option<(String, bool, Instant)>, // (message, is_error, when)
    /// (cluster, job id) -> max GPU temperature at each poll
    pub temp_history: HashMap<(usize, String), Vec<u64>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl App {
    pub fn new(config: Config, cli_hosts: Vec<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let cli_clusters = !cli_hosts.is_empty();
        let confs: Vec<ClusterConf> = if cli_clusters {
            cli_hosts
                .into_iter()
                .map(|host| {
                    let prefix = config.clusters.iter().find(|c| c.host == host).map(|c| c.prefix.clone());
                    ClusterConf { host, prefix: prefix.unwrap_or_default() }
                })
                .collect()
        } else {
            config.clusters.clone()
        };
        let mut app = App {
            clusters: Vec::new(),
            selected: None,
            moved: false,
            tick: 0,
            last_refresh: Instant::now(),
            config,
            cli_clusters,
            next_uid: 0,
            paused: false,
            palette: None,
            filter: String::new(),
            filtering: false,
            popup: None,
            log: None,
            toast: None,
            temp_history: HashMap::new(),
            tx,
            rx,
        };
        for c in confs {
            app.push_cluster(c);
        }
        app.apply_display();
        app
    }

    fn push_cluster(&mut self, c: ClusterConf) {
        self.clusters.push(Cluster {
            uid: self.next_uid,
            outputs: load_outputs(&c.host),
            host: c.host,
            prefix: c.prefix,
            jobs: None,
            fetched: None,
            error: None,
            loading: false,
            loading_since: Instant::now(),
        });
        self.next_uid += 1;
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.config.refresh_seconds.max(5))
    }

    fn apply_display(&self) {
        CLOCK_24H.store(self.config.clock == ClockFormat::H24, Ordering::Relaxed);
        FAHRENHEIT.store(self.config.temperature_unit == TempUnit::F, Ordering::Relaxed);
    }

    /// Save settings after a change from the palette.
    fn save(&mut self, msg: impl Into<String>) {
        self.apply_display();
        let mut config = self.config.clone();
        if self.cli_clusters {
            // keep the file's cluster list; only settings changed
            config.clusters = config::load().0.clusters;
        }
        match config::save(&config) {
            Ok(()) => self.say(msg, false),
            Err(e) => self.say(format!("couldn't save config: {e}"), true),
        }
    }

    fn sync_cluster_config(&mut self) {
        self.config.clusters = self
            .clusters
            .iter()
            .map(|c| ClusterConf { host: c.host.clone(), prefix: c.prefix.clone() })
            .collect();
    }

    /// Re-read the config file (after editing it by hand).
    pub fn reload_config(&mut self) {
        let (config, err) = config::load();
        if let Some(e) = err {
            self.say(e, true);
            return;
        }
        if !self.cli_clusters {
            let mut old: Vec<Cluster> = std::mem::take(&mut self.clusters);
            for conf in &config.clusters {
                match old.iter().position(|c| c.host == conf.host) {
                    Some(i) => {
                        let mut c = old.remove(i);
                        c.prefix = conf.prefix.clone();
                        self.clusters.push(c);
                    }
                    None => {
                        self.push_cluster(conf.clone());
                    }
                }
            }
            self.forget_positions();
        }
        self.config = config;
        self.apply_display();
        match crate::theme::apply(&self.config.theme) {
            Ok(()) => self.say("config reloaded", false),
            Err(e) => self.say(e, true),
        }
        self.refresh();
    }

    /// Cluster indices changed: drop anything that points at one.
    fn forget_positions(&mut self) {
        self.selected = None;
        self.moved = false;
        self.log = None;
        self.popup = None;
        self.temp_history.clear();
    }

    pub fn refresh(&mut self) {
        for c in self.clusters.iter_mut() {
            if c.loading {
                continue;
            }
            c.loading = true;
            c.loading_since = Instant::now();
            let (tx, host, uid) = (self.tx.clone(), c.host.clone(), c.uid);
            thread::spawn(move || {
                let _ = tx.send(Msg::Jobs(uid, remote::fetch_jobs(&host)));
            });
        }
        self.last_refresh = Instant::now();
    }

    pub fn loading(&self) -> bool {
        self.clusters.iter().any(|c| c.loading)
    }

    pub fn say(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    /// Handle finished background work. Called every frame.
    pub fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Jobs(uid, result) => {
                    if let Some(i) = self.clusters.iter().position(|c| c.uid == uid) {
                        self.got_jobs(i, result);
                    }
                }
                Msg::Log(id, stderr, result) => {
                    if let Some(v) = self.log.as_mut().filter(|v| v.id == id && v.stderr == stderr) {
                        v.loading = false;
                        v.fetched = Instant::now();
                        match result {
                            Ok(log) => {
                                v.log = Some(log);
                                v.error = None;
                            }
                            Err(e) => v.error = Some(e),
                        }
                    }
                }
                Msg::Cancelled(id, result) => {
                    match result {
                        Ok(()) => self.say(format!("cancelled {id}"), false),
                        Err(e) => self.say(format!("couldn't cancel {id}: {e}"), true),
                    }
                    self.refresh();
                }
            }
        }
        self.fix_selection();
        self.auto_refresh_log();
    }

    fn got_jobs(&mut self, i: usize, result: Result<Snapshot, String>) {
        let c = &mut self.clusters[i];
        c.loading = false;
        let snap = match result {
            Ok(s) => s,
            Err(e) => {
                c.error = Some(e);
                return;
            }
        };
        if self.config.notifications {
            if let Some(old) = &c.jobs {
                for (title, body) in job_changes(&c.host, old, &snap.jobs, &self.config) {
                    notifier::notify(&title, &body, self.config.notify_sound);
                }
            }
        }
        let before = c.outputs.len();
        c.outputs.extend(snap.outputs);
        if c.outputs.len() != before {
            save_outputs(&c.host, &c.outputs);
        }
        for j in snap.jobs.iter().filter(|j| !j.gpus.is_empty()) {
            let max = j.gpus.iter().map(|g| g.temp as u64).max().unwrap_or(0);
            let h = self.temp_history.entry((i, j.id.clone())).or_default();
            h.push(max);
            if h.len() > HISTORY_LEN {
                h.remove(0);
            }
        }
        c.jobs = Some(snap.jobs);
        c.fetched = Some(Instant::now());
        c.error = None;
    }

    /// Jobs of one cluster that pass the current filter / finished toggle.
    pub fn visible<'a>(&self, c: &'a Cluster) -> Vec<&'a Job> {
        let needle = self.filter.to_lowercase();
        let mut ended_left = match self.config.ended_jobs {
            Ended::Recent => self.config.recent_ended,
            Ended::All => usize::MAX,
            Ended::Hidden => 0,
        };
        c.jobs
            .iter()
            .flatten()
            .filter(|j| {
                needle.is_empty()
                    || j.name.to_lowercase().contains(&needle)
                    || j.workdir.to_lowercase().contains(&needle)
                    || j.id.contains(&needle)
            })
            // ended jobs come last, newest first, so this keeps the most recent ones
            .filter(|j| {
                if !j.finished() {
                    return true;
                }
                if ended_left == 0 {
                    return false;
                }
                ended_left -= 1;
                true
            })
            .collect()
    }

    fn flat(&self) -> Vec<(usize, String)> {
        self.clusters
            .iter()
            .enumerate()
            .flat_map(|(i, c)| self.visible(c).into_iter().map(move |j| (i, j.id.clone())))
            .collect()
    }

    /// Keep the selection on a visible job; until the user moves, stick to the top one.
    pub fn fix_selection(&mut self) {
        let flat = self.flat();
        if !self.moved || !self.selected.as_ref().is_some_and(|s| flat.contains(s)) {
            self.selected = flat.first().cloned();
        }
    }

    pub fn step(&mut self, delta: isize) {
        let flat = self.flat();
        if flat.is_empty() {
            return;
        }
        let pos = self.selected.as_ref().and_then(|s| flat.iter().position(|f| f == s)).unwrap_or(0);
        let new = (pos as isize + delta).clamp(0, flat.len() as isize - 1) as usize;
        self.selected = Some(flat[new].clone());
        self.moved = true;
    }

    pub fn jump(&mut self, last: bool) {
        let flat = self.flat();
        self.selected = if last { flat.last() } else { flat.first() }.cloned();
        self.moved = true;
    }

    pub fn selected_job(&self) -> Option<(usize, &Cluster, &Job)> {
        let (ci, id) = self.selected.as_ref()?;
        let c = &self.clusters[*ci];
        Some((*ci, c, c.jobs.as_ref()?.iter().find(|j| &j.id == id)?))
    }

    // ---------- actions ----------

    pub fn open_log(&mut self) {
        let Some((ci, _, j)) = self.selected_job() else { return };
        self.log = Some(LogView {
            ci,
            id: j.id.clone(),
            name: j.name.clone(),
            stderr: false,
            log: None,
            error: None,
            loading: false,
            fetched: Instant::now(),
            top: None,
        });
        self.fetch_log();
    }

    pub fn toggle_log_stream(&mut self) {
        if let Some(v) = self.log.as_mut() {
            v.stderr = !v.stderr;
            v.log = None;
            v.error = None;
            v.top = None;
        }
        self.fetch_log();
    }

    pub fn fetch_log(&mut self) {
        let Some(v) = self.log.as_mut() else { return };
        if v.loading {
            return;
        }
        v.loading = true;
        let c = &self.clusters[v.ci];
        let (out, err) = c.outputs.get(&v.id).cloned().unwrap_or_default();
        let pattern = if v.stderr { err } else { out };
        let workdir = c.jobs.iter().flatten().find(|j| j.id == v.id).map_or(String::new(), |j| j.workdir.clone());
        let (tx, host, id, name, stderr) = (self.tx.clone(), c.host.clone(), v.id.clone(), v.name.clone(), v.stderr);
        thread::spawn(move || {
            let r = remote::fetch_log(&host, &id, &name, &workdir, &pattern, stderr);
            let _ = tx.send(Msg::Log(id, stderr, r));
        });
    }

    /// Re-tail the open log every few seconds while its job is running.
    fn auto_refresh_log(&mut self) {
        let Some(v) = &self.log else { return };
        let running = self.clusters[v.ci]
            .jobs
            .iter()
            .flatten()
            .any(|j| j.id == v.id && j.running());
        if running && !v.loading && v.fetched.elapsed() >= LOG_REFRESH {
            self.fetch_log();
        }
    }

    pub fn ask_cancel(&mut self) {
        let Some((ci, _, j)) = self.selected_job() else { return };
        if j.finished() {
            self.say("that job already ended", true);
            return;
        }
        self.popup = Some(Popup::ConfirmCancel { ci, id: j.id.clone(), name: j.name.clone() });
    }

    pub fn cancel(&mut self, ci: usize, id: String) {
        let (tx, host) = (self.tx.clone(), self.clusters[ci].host.clone());
        self.say(format!("cancelling {id}…"), false);
        thread::spawn(move || {
            let r = remote::cancel(&host, &id);
            let _ = tx.send(Msg::Cancelled(id, r));
        });
    }

    fn copy(&mut self, text: String) {
        if remote::copy_to_clipboard(&text) {
            self.say(format!("copied {text}"), false);
        } else {
            self.say("couldn't copy", true);
        }
    }

    pub fn cycle_ended(&mut self) {
        self.config.ended_jobs = match self.config.ended_jobs {
            Ended::Recent => Ended::All,
            Ended::All => Ended::Hidden,
            Ended::Hidden => Ended::Recent,
        };
        self.fix_selection();
        let msg = format!("ended jobs: {}", crate::palette::ended_text(self));
        self.save(msg);
    }

    pub fn shell_target(&self) -> Next {
        match self.selected_job() {
            Some((_, c, j)) => Next::Shell {
                host: c.host.clone(),
                workdir: j.workdir.clone(),
                running_id: j.running().then(|| j.id.clone()),
            },
            None => Next::Continue,
        }
    }

    /// Run a command from the palette (or a key).
    pub fn run(&mut self, cmd: Cmd) -> Next {
        let c = &mut self.config;
        match cmd {
            Cmd::OpenLog => self.open_log(),
            Cmd::Shell => return self.shell_target(),
            Cmd::Cancel => self.ask_cancel(),
            Cmd::CopyId => self.copy_id(),
            Cmd::CopyFolder => {
                if let Some((_, _, j)) = self.selected_job() {
                    let dir = j.workdir.clone();
                    self.copy(dir);
                }
            }
            Cmd::Refresh => self.refresh(),
            Cmd::TogglePause => {
                self.paused = !self.paused;
                self.say(if self.paused { "auto-refresh paused" } else { "auto-refresh on" }, false);
            }
            Cmd::Filter => self.filtering = true,
            Cmd::Interval(s) => {
                c.refresh_seconds = s;
                self.save(format!("refreshing every {}", crate::palette::interval_text(s)));
            }
            Cmd::Ended(e) => {
                c.ended_jobs = e;
                self.fix_selection();
                let msg = format!("ended jobs: {}", crate::palette::ended_text(self));
                self.save(msg);
            }
            Cmd::Recent(n) => {
                c.recent_ended = n;
                c.ended_jobs = Ended::Recent;
                self.fix_selection();
                self.save(format!("showing the {n} most recent ended jobs"));
            }
            Cmd::ToggleNotify => {
                c.notifications = !c.notifications;
                let on = c.notifications;
                self.save(if on { "notifications on" } else { "notifications off" });
            }
            Cmd::ToggleNotifyQueue => {
                c.notify_on_queue = !c.notify_on_queue;
                let on = c.notify_on_queue;
                self.save(if on { "will notify when jobs are queued" } else { "won't notify when jobs are queued" });
            }
            Cmd::ToggleNotifyStart => {
                c.notify_on_start = !c.notify_on_start;
                let on = c.notify_on_start;
                self.save(if on { "will notify when jobs start" } else { "won't notify when jobs start" });
            }
            Cmd::ToggleSound => {
                c.notify_sound = !c.notify_sound;
                let on = c.notify_sound;
                self.save(if on { "notification sound on" } else { "notification sound off" });
            }
            Cmd::Unit(u) => {
                c.temperature_unit = u;
                self.save(if u == TempUnit::C { "temperatures in °C" } else { "temperatures in °F" });
            }
            Cmd::Clock(k) => {
                c.clock = k;
                self.save(if k == ClockFormat::H12 { "12-hour clock" } else { "24-hour clock" });
            }
            Cmd::ToggleGraph => {
                c.temperature_graph = !c.temperature_graph;
                let on = c.temperature_graph;
                self.save(if on { "temperature graph on" } else { "temperature graph off" });
            }
            Cmd::Theme(name) => match crate::theme::apply(&name) {
                Ok(()) => {
                    c.theme = name.clone();
                    self.save(format!("theme: {name}"));
                }
                Err(e) => self.say(e, true),
            },
            Cmd::OpenThemes => {
                crate::theme::write_examples();
                match crate::theme::folder() {
                    Some(dir) => {
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("open").arg(&dir).status();
                        self.say("opened the themes folder", false);
                    }
                    None => self.say("no config folder", true),
                }
            }
            Cmd::AddCluster => {
                self.palette = Some(Palette::with_prompt(Prompt {
                    title: "Add cluster".into(),
                    help: "ssh host name, as in ~/.ssh/config".into(),
                    text: String::new(),
                    kind: PromptKind::AddHost,
                }));
            }
            Cmd::RemoveCluster(i) => {
                if i < self.clusters.len() {
                    let host = self.clusters.remove(i).host;
                    self.forget_positions();
                    self.sync_cluster_config();
                    self.save(format!("removed {host}"));
                }
            }
            Cmd::SetPrefix(i) => {
                if let Some(cl) = self.clusters.get(i) {
                    self.palette = Some(Palette::with_prompt(Prompt {
                        title: format!("Folder prefix for {}", cl.host),
                        help: "hidden from the folder column · leave empty for none".into(),
                        text: cl.prefix.clone(),
                        kind: PromptKind::Prefix(i),
                    }));
                }
            }
            Cmd::Reconnect => {
                let hosts: Vec<String> = self.clusters.iter().map(|c| c.host.clone()).collect();
                for h in &hosts {
                    remote::disconnect(h);
                }
                self.say("reconnecting…", false);
                self.refresh();
            }
            Cmd::TestNotify => {
                notifier::notify("gpudash", "Notifications are working.", self.config.notify_sound);
                self.say("sent a test notification", false);
            }
            Cmd::EditConfig => return Next::EditConfig,
            Cmd::Help => self.popup = Some(Popup::Help),
            Cmd::Quit => return Next::Quit,
        }
        Next::Continue
    }

    /// Enter pressed in a palette text box.
    pub fn submit_prompt(&mut self, prompt: Prompt) {
        let text = prompt.text.trim().to_string();
        match prompt.kind {
            PromptKind::AddHost => {
                if text.is_empty() {
                    return;
                }
                if self.clusters.iter().any(|c| c.host == text) {
                    self.say(format!("{text} is already there"), true);
                    return;
                }
                self.palette = Some(Palette::with_prompt(Prompt {
                    title: format!("Folder prefix for {text}"),
                    help: "optional · hidden from the folder column · enter to skip".into(),
                    text: String::new(),
                    kind: PromptKind::AddPrefix(text),
                }));
            }
            PromptKind::AddPrefix(host) => {
                self.push_cluster(ClusterConf { host: host.clone(), prefix: text });
                self.sync_cluster_config();
                self.save(format!("added {host}"));
                self.refresh();
            }
            PromptKind::Prefix(i) => {
                if let Some(cl) = self.clusters.get_mut(i) {
                    cl.prefix = text;
                    let host = cl.host.clone();
                    self.sync_cluster_config();
                    self.save(format!("updated prefix for {host}"));
                }
            }
        }
    }

    pub fn copy_id(&mut self) {
        let Some((_, _, j)) = self.selected_job() else { return };
        let id = j.id.clone();
        self.copy(id);
    }
}

/// Mac notification when a job starts, or leaves the queue.
/// The notifications one poll should send, as (title, body).
fn job_changes(host: &str, old: &[Job], new: &[Job], config: &Config) -> Vec<(String, String)> {
    let mut out = Vec::new();

    // new jobs since the last poll; several at once (e.g. a sweep) get one notification
    let fresh: Vec<&Job> = new.iter().filter(|n| !n.finished() && !old.iter().any(|o| o.id == n.id)).collect();
    let (started, queued): (Vec<&Job>, Vec<&Job>) = fresh.into_iter().partition(|j| j.running());
    if config.notify_on_queue {
        if let [j] = queued.as_slice() {
            let when = j.start.map(crate::util::start_text).map(|t| format!(" · starts {t}")).unwrap_or_default();
            out.push((format!("◌ {} queued", j.name), format!("{host} · {}{when}", j.gpu_label)));
        } else if !queued.is_empty() {
            let mut names: Vec<&str> = queued.iter().map(|j| j.name.as_str()).collect();
            names.dedup();
            let shown = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
            let more = if names.len() > 3 { format!(" +{}", names.len() - 3) } else { String::new() };
            out.push((format!("◌ {} jobs queued on {host}", queued.len()), format!("{shown}{more}")));
        }
    }
    if config.notify_on_start {
        for j in started {
            out.push((format!("▶ {} started", j.name), format!("{host} · job {}", j.id)));
        }
    }

    for o in old.iter().filter(|o| !o.finished()) {
        let now = new.iter().find(|n| n.id == o.id);
        match now {
            Some(n) if o.pending() && n.running() => {
                if config.notify_on_start {
                    out.push((format!("▶ {} started", o.name), format!("{host} · job {}", o.id)));
                }
            }
            Some(n) if n.finished() => {
                let (title, body) = match n.state.as_str() {
                    "COMPLETED" => (format!("✓ {} finished", n.name), String::new()),
                    "CANCELLED" => (format!("{} cancelled", n.name), String::new()),
                    "TIMEOUT" => (format!("⏱ {} hit its time limit", n.name), String::new()),
                    "OUT_OF_MEMORY" => (format!("✗ {} ran out of memory", n.name), String::new()),
                    s => (format!("✗ {} {}", n.name, s.to_lowercase()), format!("exit {} · ", n.exit)),
                };
                out.push((title, format!("{body}{host} · job {}", n.id)));
            }
            None => out.push((format!("{} left the queue", o.name), format!("{host} · job {}", o.id))),
            _ => {}
        }
    }
    out
}

// ---------- log path cache ----------

fn cache_file(host: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/gpudash").join(format!("{host}.tsv")))
}

fn load_outputs(host: &str) -> HashMap<String, (String, String)> {
    let Some(text) = cache_file(host).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), (f.next()?.to_string(), f.next().unwrap_or("").to_string())))
        })
        .collect()
}

fn save_outputs(host: &str, outputs: &HashMap<String, (String, String)>) {
    let Some(path) = cache_file(host) else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let text: String = outputs.iter().map(|(id, (o, e))| format!("{id}\t{o}\t{e}\n")).collect();
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, name: &str, state: &str) -> Job {
        Job {
            id: id.into(),
            name: name.into(),
            state: state.into(),
            used: None,
            limit: None,
            gpu_label: "1× H200".into(),
            nodes: String::new(),
            start: None,
            end: None,
            workdir: String::new(),
            reason: String::new(),
            partition: String::new(),
            exit: String::new(),
            gpus: Vec::new(),
        }
    }

    #[test]
    fn one_new_job_is_named() {
        let old = vec![job("1", "a", "RUNNING")];
        let new = vec![job("1", "a", "RUNNING"), job("2", "sweep", "PENDING")];
        let n = job_changes("explorer", &old, &new, &Config::default());
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "◌ sweep queued");
    }

    #[test]
    fn many_new_jobs_are_grouped() {
        let new: Vec<Job> = (0..5).map(|i| job(&i.to_string(), "sweep", "PENDING")).collect();
        let n = job_changes("explorer", &[], &new, &Config::default());
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "◌ 5 jobs queued on explorer");
        assert_eq!(n[0].1, "sweep");
    }

    #[test]
    fn settings_turn_notifications_off() {
        let config = Config { notify_on_queue: false, notify_on_start: false, ..Config::default() };
        let old = vec![job("1", "a", "PENDING")];
        let new = vec![job("1", "a", "RUNNING"), job("2", "b", "PENDING")];
        assert!(job_changes("x", &old, &new, &config).is_empty());
    }

    #[test]
    fn start_and_finish() {
        let old = vec![job("1", "a", "PENDING"), job("2", "b", "RUNNING")];
        let mut done = job("2", "b", "FAILED");
        done.end = Some(chrono::Utc::now());
        done.exit = "1:0".into();
        let new = vec![job("1", "a", "RUNNING"), done];
        let n = job_changes("x", &old, &new, &Config::default());
        let titles: Vec<&str> = n.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(titles, ["▶ a started", "✗ b failed"]);
    }
}
