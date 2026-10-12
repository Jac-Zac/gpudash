//! gpudash — live terminal dashboard for your Slurm jobs across clusters.

mod app;
mod remote;
mod ui;
mod util;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use app::{App, Ended, Popup};

const DEFAULT_INTERVAL: u64 = 30;

const HELP: &str = "\
gpudash — live dashboard for your Slurm jobs across clusters

usage: gpudash [-n SECONDS] [HOST ...]

  HOST        ssh host aliases to watch (default: the ones in the config file)
  -n SECONDS  how often to refresh (default 30)

config: ~/.config/gpudash/clusters, one cluster per line:

  # ssh alias   folder prefix to hide in the table (optional)
  explorer      /projects/myname/
  KISSKI        /mnt/vast-kisski/projects/myproject/
";

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("gpudash/clusters"))
}

fn load_config() -> Vec<(String, String)> {
    let Some(text) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.split_whitespace();
            let host = parts.next().unwrap_or_default().to_string();
            let prefix = parts.next().unwrap_or_default().to_string();
            (host, prefix)
        })
        .collect()
}

/// What the event loop should do after a key press.
enum Next {
    Continue,
    Quit,
    Shell { host: String, workdir: String, running_id: Option<String> },
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut interval = DEFAULT_INTERVAL;
    let mut hosts = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-n" => interval = args.next().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_INTERVAL),
            _ => hosts.push(a),
        }
    }

    let config = load_config();
    let clusters: Vec<(String, String)> = if hosts.is_empty() {
        config
    } else {
        hosts
            .into_iter()
            .map(|h| {
                let prefix = config.iter().find(|(c, _)| *c == h).map(|(_, p)| p.clone()).unwrap_or_default();
                (h, prefix)
            })
            .collect()
    };
    if clusters.is_empty() {
        eprint!("No clusters to watch. Pass ssh host names, or list them in the config file.\n\n{HELP}");
        std::process::exit(1);
    }

    let mut app = App::new(clusters, Duration::from_secs(interval.max(5)));
    let mut terminal = ratatui::init();
    app.refresh();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        app.receive();
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match on_key(app, k) {
                        Next::Continue => {}
                        Next::Quit => return Ok(()),
                        Next::Shell { host, workdir, running_id } => {
                            ratatui::restore();
                            println!("\n  gpudash → {host}:{workdir}  (exit the shell to come back)\n");
                            if let Err(e) = remote::shell(&host, &workdir, running_id.as_deref()) {
                                app.say(format!("shell failed: {e}"), true);
                            }
                            *terminal = ratatui::init();
                            terminal.clear()?;
                        }
                    }
                }
            }
        }
        if !app.loading() && app.last_refresh.elapsed() >= app.interval {
            app.refresh();
        }
        app.tick += 1;
    }
}

fn on_key(app: &mut App, k: KeyEvent) -> Next {
    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return Next::Quit;
    }

    // popups take the key first
    if let Some(popup) = app.popup.take() {
        if let Popup::ConfirmCancel { ci, id, .. } = popup {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                app.cancel(ci, id);
            }
        }
        return Next::Continue;
    }

    if app.filtering {
        match k.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.filtering = false;
            }
            KeyCode::Enter => app.filtering = false,
            KeyCode::Backspace => {
                app.filter.pop();
            }
            KeyCode::Down => app.step(1),
            KeyCode::Up => app.step(-1),
            KeyCode::Char(c) => app.filter.push(c),
            _ => {}
        }
        app.fix_selection();
        return Next::Continue;
    }

    if let Some(v) = app.log.as_mut() {
        let page = 20;
        let n = v.log.as_ref().map_or(0, |l| l.lines.len());
        let height = ratatui::crossterm::terminal::size().map_or(30, |(_, h)| h as usize).saturating_sub(2);
        let bottom = n.saturating_sub(height);
        let top = v.top.unwrap_or(bottom);
        let mut scroll = |to: usize| v.top = if to >= bottom { None } else { Some(to) };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => app.log = None,
            KeyCode::Up | KeyCode::Char('k') => scroll(top.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => scroll(top + 1),
            KeyCode::PageUp | KeyCode::Char('u') => scroll(top.saturating_sub(page)),
            KeyCode::PageDown | KeyCode::Char('d') | KeyCode::Char(' ') => scroll(top + page),
            KeyCode::Home | KeyCode::Char('g') => scroll(0),
            KeyCode::End | KeyCode::Char('G') => scroll(bottom),
            KeyCode::Char('e') => app.toggle_log_stream(),
            KeyCode::Char('r') => app.fetch_log(),
            _ => {}
        }
        return Next::Continue;
    }

    match k.code {
        KeyCode::Char('q') => return Next::Quit,
        KeyCode::Esc if !app.filter.is_empty() => {
            app.filter.clear();
            app.fix_selection();
        }
        KeyCode::Esc => return Next::Quit,
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Down | KeyCode::Char('j') => app.step(1),
        KeyCode::Up | KeyCode::Char('k') => app.step(-1),
        KeyCode::PageDown => app.step(10),
        KeyCode::PageUp => app.step(-10),
        KeyCode::Home | KeyCode::Char('g') => app.jump(false),
        KeyCode::End | KeyCode::Char('G') => app.jump(true),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => app.open_log(),
        KeyCode::Char('x') => app.ask_cancel(),
        KeyCode::Char('y') => app.copy_id(),
        KeyCode::Char('/') => app.filtering = true,
        KeyCode::Char('?') => app.popup = Some(Popup::Help),
        KeyCode::Char('h') => {
            let (next, msg) = match app.ended {
                Ended::Recent => (Ended::All, "showing every job that ended in the last 24h"),
                Ended::All => (Ended::Hidden, "hiding ended jobs"),
                Ended::Hidden => (Ended::Recent, "showing the 5 most recent ended jobs"),
            };
            app.ended = next;
            app.fix_selection();
            app.say(msg, false);
        }
        KeyCode::Char('n') => {
            app.notify = !app.notify;
            app.say(if app.notify { "notifications on" } else { "notifications off" }, false);
        }
        KeyCode::Char('s') => {
            if let Some((_, c, j)) = app.selected_job() {
                return Next::Shell {
                    host: c.host.clone(),
                    workdir: j.workdir.clone(),
                    running_id: j.running().then(|| j.id.clone()),
                };
            }
        }
        _ => {}
    }
    Next::Continue
}
