//! App state: clusters, selection, popups, background fetches.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::remote::{self, Job, Log, Snapshot};

const HISTORY_LEN: usize = 120;
const LOG_REFRESH: Duration = Duration::from_secs(3);

pub struct Cluster {
    pub host: String,
    pub prefix: String,
    pub jobs: Option<Vec<Job>>,
    pub fetched: Option<Instant>,
    pub error: Option<String>,
    pub loading: bool,
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

#[derive(Clone, Copy, PartialEq)]
pub enum Ended {
    Recent,
    All,
    Hidden,
}

pub const RECENT_ENDED: usize = 5;

pub enum Popup {
    Help,
    ConfirmCancel { ci: usize, id: String, name: String },
}

pub enum Msg {
    Jobs(usize, Result<Snapshot, String>),
    Log(String, bool, Result<Log, String>),
    Cancelled(String, Result<(), String>),
}

pub struct App {
    pub clusters: Vec<Cluster>,
    pub selected: Option<(usize, String)>, // (cluster index, job id)
    moved: bool,                           // user has moved the selection
    pub tick: usize,
    pub last_refresh: Instant,
    pub interval: Duration,
    pub ended: Ended,
    pub notify: bool,
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
    pub fn new(clusters: Vec<(String, String)>, interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let clusters = clusters
            .into_iter()
            .map(|(host, prefix)| Cluster {
                outputs: load_outputs(&host),
                host,
                prefix,
                jobs: None,
                fetched: None,
                error: None,
                loading: false,
            })
            .collect();
        App {
            clusters,
            selected: None,
            moved: false,
            tick: 0,
            last_refresh: Instant::now(),
            interval,
            ended: Ended::Recent,
            notify: true,
            filter: String::new(),
            filtering: false,
            popup: None,
            log: None,
            toast: None,
            temp_history: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn refresh(&mut self) {
        for (i, c) in self.clusters.iter_mut().enumerate() {
            if c.loading {
                continue;
            }
            c.loading = true;
            let (tx, host) = (self.tx.clone(), c.host.clone());
            thread::spawn(move || {
                let _ = tx.send(Msg::Jobs(i, remote::fetch_jobs(&host)));
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
                Msg::Jobs(i, result) => self.got_jobs(i, result),
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
        if self.notify {
            if let Some(old) = &c.jobs {
                notify_changes(&c.host, old, &snap.jobs);
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
        let mut ended_left = match self.ended {
            Ended::Recent => RECENT_ENDED,
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

    pub fn copy_id(&mut self) {
        let Some((_, _, j)) = self.selected_job() else { return };
        let id = j.id.clone();
        if remote::copy_to_clipboard(&id) {
            self.say(format!("copied {id}"), false);
        } else {
            self.say("couldn't copy", true);
        }
    }
}

/// Mac notification when a job starts, or leaves the queue.
fn notify_changes(host: &str, old: &[Job], new: &[Job]) {
    for o in old.iter().filter(|o| !o.finished()) {
        let now = new.iter().find(|n| n.id == o.id);
        match now {
            Some(n) if o.pending() && n.running() => {
                remote::notify(&format!("▶ {} started", o.name), &format!("{host} · job {}", o.id));
            }
            Some(n) if n.finished() => {
                let (title, body) = match n.state.as_str() {
                    "COMPLETED" => (format!("✓ {} finished", n.name), String::new()),
                    "CANCELLED" => (format!("{} cancelled", n.name), String::new()),
                    "TIMEOUT" => (format!("⏱ {} hit its time limit", n.name), String::new()),
                    "OUT_OF_MEMORY" => (format!("✗ {} ran out of memory", n.name), String::new()),
                    s => (format!("✗ {} {}", n.name, s.to_lowercase()), format!("exit {} · ", n.exit)),
                };
                remote::notify(&title, &format!("{body}{host} · job {}", n.id));
            }
            None => remote::notify(&format!("{} left the queue", o.name), &format!("{host} · job {}", o.id)),
            _ => {}
        }
    }
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
