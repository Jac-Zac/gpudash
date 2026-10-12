//! Command palette (press p): every action and setting in one searchable list.

use crate::app::App;
use crate::config::{ClockFormat, Ended, TempUnit};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Menu {
    Root,
    Interval,
    Ended,
    Recent,
    Unit,
    Clock,
    Theme,
    RemoveCluster,
    PrefixCluster,
}

impl Menu {
    pub fn title(self) -> &'static str {
        match self {
            Menu::Root => "Commands",
            Menu::Interval => "Refresh interval",
            Menu::Ended => "Ended jobs",
            Menu::Recent => "How many recent ended jobs",
            Menu::Unit => "Temperature unit",
            Menu::Clock => "Clock",
            Menu::Theme => "Theme",
            Menu::RemoveCluster => "Remove cluster",
            Menu::PrefixCluster => "Set folder prefix",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Cmd {
    // job
    OpenLog,
    Shell,
    Cancel,
    CopyId,
    CopyFolder,
    // view
    Refresh,
    TogglePause,
    Filter,
    // settings
    Interval(u64),
    Ended(Ended),
    Recent(usize),
    ToggleNotify,
    ToggleNotifyQueue,
    ToggleNotifyStart,
    ToggleSound,
    Unit(TempUnit),
    Clock(ClockFormat),
    ToggleGraph,
    Theme(String),
    OpenThemes,
    // clusters
    AddCluster,
    RemoveCluster(usize),
    SetPrefix(usize),
    Reconnect,
    // app
    TestNotify,
    EditConfig,
    Help,
    Quit,
}

pub enum Act {
    Run(Cmd),
    Open(Menu),
}

pub struct Item {
    pub group: &'static str,
    pub label: String,
    pub hint: String,
    pub act: Act,
}

fn item(group: &'static str, label: impl Into<String>, hint: impl Into<String>, act: Act) -> Item {
    Item { group, label: label.into(), hint: hint.into(), act }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn tick(b: bool) -> &'static str {
    if b { "✓" } else { "" }
}

pub fn interval_text(secs: u64) -> String {
    if secs % 60 == 0 { format!("{}m", secs / 60) } else { format!("{secs}s") }
}

pub fn ended_text(app: &App) -> String {
    match app.config.ended_jobs {
        Ended::Recent => format!("recent {}", app.config.recent_ended),
        Ended::All => "all from 24h".into(),
        Ended::Hidden => "hidden".into(),
    }
}

pub fn items(app: &App, menu: Menu) -> Vec<Item> {
    use Act::{Open, Run};
    let c = &app.config;
    match menu {
        Menu::Root => {
            let mut v = Vec::new();
            if let Some((_, _, j)) = app.selected_job() {
                let running = j.running();
                v.push(item("job", "Open log", "enter", Run(Cmd::OpenLog)));
                v.push(item("job", if running { "Shell into job's node" } else { "Shell in job's folder" }, "s", Run(Cmd::Shell)));
                if !j.finished() {
                    v.push(item("job", format!("Cancel {}", j.name), "x", Run(Cmd::Cancel)));
                }
                v.push(item("job", "Copy job id", "y", Run(Cmd::CopyId)));
                v.push(item("job", "Copy folder path", "", Run(Cmd::CopyFolder)));
            }
            v.extend([
                item("view", "Refresh now", "r", Run(Cmd::Refresh)),
                item("view", if app.paused { "Resume auto-refresh" } else { "Pause auto-refresh" }, "", Run(Cmd::TogglePause)),
                item("view", "Filter jobs", "/", Run(Cmd::Filter)),
                item("setting", "Refresh interval…", interval_text(c.refresh_seconds), Open(Menu::Interval)),
                item("setting", "Ended jobs…", ended_text(app), Open(Menu::Ended)),
                item("setting", "How many recent ended jobs…", c.recent_ended.to_string(), Open(Menu::Recent)),
                item("setting", "Notifications", on_off(c.notifications), Run(Cmd::ToggleNotify)),
                item("setting", "Notify when jobs are queued", on_off(c.notify_on_queue), Run(Cmd::ToggleNotifyQueue)),
                item("setting", "Notify when a job starts", on_off(c.notify_on_start), Run(Cmd::ToggleNotifyStart)),
                item("setting", "Notification sound", on_off(c.notify_sound), Run(Cmd::ToggleSound)),
                item("setting", "Temperature unit…", if c.temperature_unit == TempUnit::C { "°C" } else { "°F" }, Open(Menu::Unit)),
                item("setting", "Clock…", if c.clock == ClockFormat::H12 { "12-hour" } else { "24-hour" }, Open(Menu::Clock)),
                item("setting", "Temperature graph", on_off(c.temperature_graph), Run(Cmd::ToggleGraph)),
                item("setting", "Theme…", c.theme.clone(), Open(Menu::Theme)),
                item("setting", "Open themes folder", "", Run(Cmd::OpenThemes)),
                item("cluster", "Add cluster…", "", Run(Cmd::AddCluster)),
            ]);
            if !app.clusters.is_empty() {
                v.push(item("cluster", "Remove cluster…", "", Open(Menu::RemoveCluster)));
                v.push(item("cluster", "Set folder prefix…", "", Open(Menu::PrefixCluster)));
            }
            v.extend([
                item("cluster", "Reconnect ssh", "", Run(Cmd::Reconnect)),
                item("app", "Send a test notification", "", Run(Cmd::TestNotify)),
                item("app", "Edit config file", "$EDITOR", Run(Cmd::EditConfig)),
                item("app", "Keyboard shortcuts", "?", Run(Cmd::Help)),
                item("app", "Quit", "q", Run(Cmd::Quit)),
            ]);
            v
        }
        Menu::Interval => [10, 15, 30, 60, 120, 300]
            .into_iter()
            .map(|s| item("", format!("every {}", interval_text(s)), tick(c.refresh_seconds == s), Run(Cmd::Interval(s))))
            .collect(),
        Menu::Ended => [
            (Ended::Recent, format!("the {} most recent", c.recent_ended)),
            (Ended::All, "all from the last 24h".to_string()),
            (Ended::Hidden, "none".to_string()),
        ]
        .into_iter()
        .map(|(e, l)| item("", l, tick(c.ended_jobs == e), Run(Cmd::Ended(e))))
        .collect(),
        Menu::Recent => [3, 5, 10, 20]
            .into_iter()
            .map(|n| item("", n.to_string(), tick(c.recent_ended == n), Run(Cmd::Recent(n))))
            .collect(),
        Menu::Unit => vec![
            item("", "Celsius (°C)", tick(c.temperature_unit == TempUnit::C), Run(Cmd::Unit(TempUnit::C))),
            item("", "Fahrenheit (°F)", tick(c.temperature_unit == TempUnit::F), Run(Cmd::Unit(TempUnit::F))),
        ],
        Menu::Clock => vec![
            item("", "12-hour (08:24pm)", tick(c.clock == ClockFormat::H12), Run(Cmd::Clock(ClockFormat::H12))),
            item("", "24-hour (20:24)", tick(c.clock == ClockFormat::H24), Run(Cmd::Clock(ClockFormat::H24))),
        ],
        Menu::Theme => crate::theme::available()
            .into_iter()
            .map(|name| {
                let hint = tick(c.theme == name);
                item("", name.clone(), hint, Run(Cmd::Theme(name)))
            })
            .collect(),
        Menu::RemoveCluster => app
            .clusters
            .iter()
            .enumerate()
            .map(|(i, cl)| item("", cl.host.clone(), "", Run(Cmd::RemoveCluster(i))))
            .collect(),
        Menu::PrefixCluster => app
            .clusters
            .iter()
            .enumerate()
            .map(|(i, cl)| {
                let hint = if cl.prefix.is_empty() { "none".to_string() } else { cl.prefix.clone() };
                item("", cl.host.clone(), hint, Run(Cmd::SetPrefix(i)))
            })
            .collect(),
    }
}

/// How well an item matches the search; lower is better, None means no match.
/// Words that start with what you typed beat matches in the middle of a word,
/// which beat letters scattered in order ("refint" → "Refresh interval").
pub fn score(query: &str, item: &Item) -> Option<u8> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let label = item.label.to_lowercase();
    let hay = format!("{} {label}", item.group);
    if label.starts_with(&q) {
        return Some(0);
    }
    if label.split_whitespace().any(|w| w.starts_with(&q)) {
        return Some(1);
    }
    if hay.contains(&q) {
        return Some(2);
    }
    let mut chars = hay.chars();
    q.chars().filter(|c| !c.is_whitespace()).all(|c| chars.any(|h| h == c)).then_some(3)
}

/// Items that match the search, best matches first (ties keep their usual order).
pub fn search(app: &App, menu: Menu, query: &str) -> Vec<Item> {
    let mut scored: Vec<(u8, Item)> =
        items(app, menu).into_iter().filter_map(|it| score(query, &it).map(|s| (s, it))).collect();
    scored.sort_by_key(|(s, _)| *s);
    scored.into_iter().map(|(_, it)| it).collect()
}

/// One open level of the palette (the root list, or a submenu).
pub struct Level {
    pub menu: Menu,
    pub query: String,
    pub sel: usize,
}

pub enum PromptKind {
    AddHost,
    AddPrefix(String),
    Prefix(usize),
}

/// A text box shown in place of the list, e.g. for "Add cluster".
pub struct Prompt {
    pub title: String,
    pub help: String,
    pub text: String,
    pub kind: PromptKind,
}

pub struct Palette {
    pub stack: Vec<Level>,
    pub prompt: Option<Prompt>,
}

impl Palette {
    pub fn new() -> Self {
        Palette { stack: vec![Level { menu: Menu::Root, query: String::new(), sel: 0 }], prompt: None }
    }

    pub fn with_prompt(prompt: Prompt) -> Self {
        Palette { stack: Vec::new(), prompt: Some(prompt) }
    }

    pub fn level(&mut self) -> Option<&mut Level> {
        self.stack.last_mut()
    }

    pub fn open(&mut self, menu: Menu) {
        self.stack.push(Level { menu, query: String::new(), sel: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking() {
        let add = item("cluster", "Add cluster…", "", Act::Run(Cmd::AddCluster));
        let recent = item("setting", "How many recent ended jobs…", "", Act::Open(Menu::Recent));
        let refresh = item("setting", "Refresh interval…", "", Act::Open(Menu::Interval));
        assert!(score("add", &add) < score("add", &recent));
        assert_eq!(score("refint", &refresh), Some(3));
        assert_eq!(score("quit", &refresh), None);
    }
}
