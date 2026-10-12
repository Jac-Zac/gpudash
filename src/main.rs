//! gpudash — live terminal dashboard for your Slurm jobs across clusters.

mod app;
mod config;
mod highlight;
mod notifier;
mod palette;
mod remote;
mod ui;
mod util;

use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use app::{App, Next, Popup};
use palette::{Act, Palette};

const HELP: &str = "\
gpudash — live dashboard for your Slurm jobs across clusters

usage: gpudash [-n SECONDS] [HOST ...]

  HOST        ssh host names to watch (default: the clusters in the config file)
  -n SECONDS  how often to refresh (overrides the config for this run)

config: ~/.config/gpudash/config.toml
  Press k inside gpudash to change settings and add or remove clusters.
  A minimal config:

    [[cluster]]
    host = \"explorer\"                  # ssh host name from ~/.ssh/config
    prefix = \"/projects/myname/\"       # optional: hidden in the folder column
";

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut interval = None;
    let mut hosts = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-n" => interval = args.next().and_then(|s| s.parse().ok()),
            _ => hosts.push(a),
        }
    }

    let (mut config, err) = config::load();
    if let Some(e) = err {
        eprintln!("gpudash: couldn't read the config file\n  {e}");
        std::process::exit(1);
    }
    if let Some(n) = interval {
        config.refresh_seconds = n;
    }
    if hosts.is_empty() && config.clusters.is_empty() {
        eprint!("No clusters to watch yet. Pass ssh host names (gpudash mycluster), or add them to the config.\n\n{HELP}");
        std::process::exit(1);
    }

    let mut app = App::new(config, hosts);
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
                            suspend(terminal, || {
                                println!("\n  gpudash → {host}:{workdir}  (exit the shell to come back)\n");
                                remote::shell(&host, &workdir, running_id.as_deref())
                            })?;
                        }
                        Next::EditConfig => {
                            let Some(path) = config::path() else { continue };
                            if !path.exists() {
                                let _ = config::save(&app.config);
                            }
                            let editor = std::env::var("VISUAL")
                                .or_else(|_| std::env::var("EDITOR"))
                                .unwrap_or_else(|_| "vi".into());
                            suspend(terminal, || {
                                // $EDITOR may carry flags, e.g. "code --wait"
                                std::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(format!("{editor} \"$1\""))
                                    .arg("sh")
                                    .arg(&path)
                                    .status()
                                    .map(|_| ())
                            })?;
                            app.reload_config();
                        }
                    }
                }
            }
        }
        if !app.paused && !app.loading() && app.last_refresh.elapsed() >= app.interval() {
            app.refresh();
        }
        app.tick += 1;
    }
}

/// Leave the full-screen view, run something in the normal terminal, then come back.
fn suspend(terminal: &mut DefaultTerminal, f: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<()> {
    ratatui::restore();
    let result = f();
    *terminal = ratatui::init();
    terminal.clear()?;
    if let Err(e) = result {
        eprintln!("{e}");
    }
    Ok(())
}

fn on_key(app: &mut App, k: KeyEvent) -> Next {
    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return Next::Quit;
    }

    if app.palette.is_some() {
        return palette_key(app, k);
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
        KeyCode::Char('k') | KeyCode::Char(':') => app.palette = Some(Palette::new()),
        KeyCode::Char('q') => return Next::Quit,
        KeyCode::Esc if !app.filter.is_empty() => {
            app.filter.clear();
            app.fix_selection();
        }
        KeyCode::Esc => return Next::Quit,
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Down | KeyCode::Char('j') => app.step(1),
        KeyCode::Up => app.step(-1),
        KeyCode::PageDown => app.step(10),
        KeyCode::PageUp => app.step(-10),
        KeyCode::Home | KeyCode::Char('g') => app.jump(false),
        KeyCode::End | KeyCode::Char('G') => app.jump(true),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => app.open_log(),
        KeyCode::Char('x') => app.ask_cancel(),
        KeyCode::Char('y') => app.copy_id(),
        KeyCode::Char('/') => app.filtering = true,
        KeyCode::Char('?') => app.popup = Some(Popup::Help),
        KeyCode::Char('h') => app.cycle_ended(),
        KeyCode::Char('n') => return app.run(palette::Cmd::ToggleNotify),
        KeyCode::Char('s') => return app.shell_target(),
        _ => {}
    }
    Next::Continue
}

fn palette_key(app: &mut App, k: KeyEvent) -> Next {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let p = app.palette.as_mut().unwrap();

    // text box (add cluster, set prefix)
    if let Some(prompt) = p.prompt.as_mut() {
        match k.code {
            KeyCode::Esc => app.palette = None,
            KeyCode::Enter => {
                let prompt = app.palette.take().unwrap().prompt.unwrap();
                app.submit_prompt(prompt);
            }
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            KeyCode::Char(c) => prompt.text.push(c),
            _ => {}
        }
        return Next::Continue;
    }

    let Some(level) = p.level() else {
        app.palette = None;
        return Next::Continue;
    };
    let (menu, query, sel) = (level.menu, level.query.clone(), level.sel);
    let items: Vec<palette::Item> =
        palette::search(app, menu, &query);
    let last = items.len().saturating_sub(1);

    let p = app.palette.as_mut().unwrap();
    let depth = p.stack.len();
    let level = p.level().unwrap();
    match k.code {
        KeyCode::Esc => {
            p.stack.pop();
            if p.stack.is_empty() {
                app.palette = None;
            }
        }
        KeyCode::Up => level.sel = sel.saturating_sub(1),
        KeyCode::Down => level.sel = (sel + 1).min(last),
        KeyCode::Char('p') if ctrl => level.sel = sel.saturating_sub(1),
        KeyCode::Char('n') if ctrl => level.sel = (sel + 1).min(last),
        KeyCode::Backspace => {
            if level.query.pop().is_none() && depth > 1 {
                p.stack.pop();
            } else {
                level.sel = 0;
            }
        }
        KeyCode::Char(c) => {
            level.query.push(c);
            level.sel = 0;
        }
        KeyCode::Enter => {
            let Some(it) = items.into_iter().nth(sel.min(last)) else { return Next::Continue };
            match it.act {
                Act::Open(menu) => p.open(menu),
                Act::Run(cmd) => {
                    app.palette = None;
                    return app.run(cmd);
                }
            }
        }
        _ => {}
    }
    Next::Continue
}
