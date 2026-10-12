//! Drawing.

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph, Row, Sparkline,
    Table, TableState,
};

use crate::app::{App, Cluster, LogView, Popup};
use crate::config::Ended;
use crate::highlight;
use crate::palette::{self, Act, Palette};
use crate::remote::Job;
use crate::util::*;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TOAST_SECS: u64 = 4;

fn spinner(tick: usize) -> Span<'static> {
    let color = temp_color(30.0 + ((tick * 2) % 60) as f64);
    Span::styled(SPINNER[tick % SPINNER.len()], Style::new().fg(color))
}

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(DIM))
}

fn text(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(TEXT))
}

fn key(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::new().fg(ACCENT))
}

fn panel() -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(FAINT))
}

/// Running jobs get two rows per GPU (util and memory bars), up to 4 GPUs.
fn detail_height(app: &App) -> u16 {
    match app.selected_job() {
        Some((_, _, j)) if j.running() => {
            let gpus = j.gpus.len().clamp(1, 4) as u16;
            let more = u16::from(j.gpus.len() > 4);
            (2 + 2 * gpus + more + 2).max(8)
        }
        _ => 8,
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [head, body, detail, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(detail_height(app)),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, head, app);
    draw_clusters(f, body, app);
    draw_detail(f, detail, app);
    draw_footer(f, foot, app);

    if let Some(v) = &app.log {
        draw_log(f, app, v);
    }
    match &app.popup {
        Some(Popup::Help) => draw_help(f),
        Some(Popup::ConfirmCancel { ci, id, name }) => draw_confirm(f, &app.clusters[*ci].host, id, name),
        None => {}
    }
    if let Some(p) = &app.palette {
        draw_palette(f, app, p);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let jobs = app.clusters.iter().flat_map(|c| c.jobs.iter().flatten()).filter(|j| !j.finished());
    let (running, queued) = jobs.fold((0, 0), |(r, q), j| if j.running() { (r + 1, q) } else { (r, q + 1) });
    let gpus: usize = app
        .clusters
        .iter()
        .flat_map(|c| c.jobs.iter().flatten())
        .map(|j| j.gpus.len())
        .sum();
    let left = Line::from(vec![
        Span::styled(" gpudash ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::styled(format!("  {running} running"), Style::new().fg(GREEN)),
        dim(" · "),
        Span::styled(format!("{queued} queued"), Style::new().fg(YELLOW)),
        dim(format!(" · {gpus} GPUs in use")),
    ]);
    let mut right = vec![];
    if !app.config.notifications {
        right.push(dim("notifications off · "));
    }
    if app.loading() {
        right.extend([spinner(app.tick), dim(" refreshing ")]);
    } else if app.paused {
        right.push(Span::styled("‖ paused ", Style::new().fg(YELLOW)));
    } else {
        let left_s = app.interval().saturating_sub(app.last_refresh.elapsed()).as_secs();
        right.push(dim(format!("↻ {left_s}s ")));
    }
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
}

fn draw_clusters(f: &mut Frame, area: Rect, app: &App) {
    let visible: Vec<Vec<&Job>> = app.clusters.iter().map(|c| app.visible(c)).collect();
    let heights: Vec<u16> = visible
        .iter()
        .map(|v| if v.is_empty() { 3 } else { v.len() as u16 + 3 })
        .collect();
    let constraints: Vec<Constraint> = if heights.iter().sum::<u16>() <= area.height {
        heights.iter().map(|&h| Constraint::Length(h)).chain([Constraint::Fill(1)]).collect()
    } else {
        heights.iter().map(|&h| Constraint::Fill(h)).collect()
    };
    let areas = Layout::vertical(constraints).split(area);
    for (i, c) in app.clusters.iter().enumerate() {
        draw_cluster(f, areas[i], app, i, c, &visible[i]);
    }
}

fn draw_cluster(f: &mut Frame, area: Rect, app: &App, ci: usize, c: &Cluster, jobs: &[&Job]) {
    let mut title = vec![Span::styled(format!(" {} ", c.host), Style::new().fg(TEXT).bold())];
    if let Some(all) = &c.jobs {
        let r = all.iter().filter(|j| j.running()).count();
        let q = all.iter().filter(|j| !j.running() && !j.finished()).count();
        let d = all.iter().filter(|j| j.finished()).count();
        let mut counts = format!("{r} running · {q} queued");
        if d > 0 {
            let recent = app.config.recent_ended;
            counts += &match app.config.ended_jobs {
                Ended::Recent if d > recent => format!(" · {d} ended (24h, showing {recent})"),
                Ended::Hidden => format!(" · {d} ended (hidden)"),
                _ => format!(" · {d} ended (24h)"),
            };
        }
        title.push(dim(counts + " "));
    }
    if c.loading {
        title.extend([spinner(app.tick), Span::raw(" ")]);
    }
    let mut block = panel().title(Line::from(title));
    if let Some(e) = &c.error {
        block = block.title_bottom(Line::from(Span::styled(format!(" ⚠ {e} "), Style::new().fg(RED))));
    }
    if let Some(t) = c.fetched {
        let ago = t.elapsed().as_secs();
        block = block.title_top(Line::from(Span::styled(format!(" {ago}s ago "), Style::new().fg(FAINT))).right_aligned());
    }

    if jobs.is_empty() {
        let msg = match (&c.jobs, c.loading) {
            (None, true) => " connecting…",
            (None, false) => "",
            (Some(_), _) if !app.filter.is_empty() => " nothing matches the filter",
            _ => " no jobs",
        };
        f.render_widget(Paragraph::new(dim(msg)).block(block), area);
        return;
    }

    let since = c.fetched.map_or(0, |t| t.elapsed().as_secs());
    let header = Row::new(["id", "name", "state", "gpu", "time", "temp / start", "folder"]).style(Style::new().fg(DIM));
    let rows: Vec<Row> = jobs.iter().map(|j| job_row(j, &c.prefix, since)).collect();
    let widths = [
        Constraint::Length(10),
        Constraint::Length(22),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(17),
        Constraint::Length(23),
        Constraint::Fill(1),
    ];
    let mut state = TableState::default();
    if let Some((sci, id)) = &app.selected {
        if *sci == ci {
            state.select(jobs.iter().position(|j| &j.id == id));
        }
    }
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::new().bg(SEL_BG))
        .highlight_symbol(Span::styled("▌", Style::new().fg(ACCENT)))
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, area, &mut state);
}

fn job_row(j: &Job, prefix: &str, since: u64) -> Row<'static> {
    let (dot, label, color) = state_look(&j.state);
    let state = Line::from(Span::styled(format!("{dot} {label}"), Style::new().fg(color)));
    let ended = j.finished();
    let name_color = if ended { DIM } else { TEXT };

    let gpu = match j.gpus.first() {
        Some(g) => format!("{}× {}", j.gpus.len(), g.name),
        None => j.gpu_label.clone(),
    };

    let time = if j.running() {
        let used = j.used.map(|u| u + since);
        let frac = match (used, j.limit) {
            (Some(u), Some(l)) if l > 0 => u as f64 / l as f64,
            _ => 0.0,
        };
        let mut spans = bar(frac, 5);
        spans.push(text(format!(" {}", dur(used))));
        spans.push(dim(format!("/{}", dur(j.limit))));
        Line::from(spans)
    } else if ended {
        Line::from(dim(format!("      {}/{}", dur(j.used), dur(j.limit))))
    } else {
        Line::from(dim(format!("      {}", dur(j.limit))))
    };

    let last = if !j.gpus.is_empty() {
        let temps: Vec<u32> = j.gpus.iter().map(|g| g.temp).collect();
        let many = temps.len() > 4;
        let shown = if many { vec![*temps.iter().max().unwrap()] } else { temps };
        let mut spans: Vec<Span> = shown
            .iter()
            .map(|&t| Span::styled(format!("{} ", temp_text(t)), Style::new().fg(temp_color(t as f64)).bold()))
            .collect();
        if many {
            spans.push(dim("max"));
        }
        Line::from(spans)
    } else if let Some(end) = j.end {
        let mut spans = vec![dim(format!("ended {}", ago(end)))];
        if j.state != "COMPLETED" && j.state != "CANCELLED" && j.exit != "0:0" {
            spans.push(Span::styled(format!(" · exit {}", j.exit), Style::new().fg(color)));
        }
        Line::from(spans)
    } else if j.running() {
        Line::from(dim("–"))
    } else if let Some(s) = j.start {
        Line::from(text(start_text(s)))
    } else {
        let why = if matches!(j.reason.as_str(), "Priority" | "None" | "Resources") { "" } else { &j.reason };
        Line::from(vec![dim("? "), Span::styled(why.to_string(), Style::new().fg(FAINT))])
    };

    Row::new(vec![
        Cell::from(dim(j.id.clone())),
        Cell::from(Span::styled(j.name.clone(), Style::new().fg(name_color))),
        Cell::from(state),
        Cell::from(Span::styled(gpu, Style::new().fg(name_color))),
        Cell::from(time),
        Cell::from(last),
        Cell::from(dim(strip_prefix(&j.workdir, prefix).to_string())),
    ])
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let block = panel().padding(Padding::horizontal(1));
    let Some((ci, c, j)) = app.selected_job() else {
        f.render_widget(block, area);
        return;
    };
    let (dot, _, color) = state_look(&j.state);
    let block = block.title(Line::from(vec![
        Span::styled(format!(" {dot} "), Style::new().fg(color)),
        Span::styled(format!("{} ", j.name), Style::new().fg(TEXT).bold()),
        dim(format!("{} · {} ", j.id, c.host)),
    ]));
    let label = |s: &'static str| dim(format!("{s:<8}"));
    let mut lines = vec![Line::from(vec![label("folder"), text(j.workdir.clone())])];

    let history = app.temp_history.get(&(ci, j.id.clone())).filter(|h| h.len() >= 2);
    let mut area = area;

    if j.running() {
        lines.push(Line::from(vec![label("node"), text(format!("{} · {}", j.nodes, j.partition))]));

        // temperature history on the right
        if let Some(h) = history.filter(|_| app.config.temperature_graph && area.width > 110) {
            let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(42)]).areas(area);
            area = left;
            let now = *h.last().unwrap();
            let spark = Sparkline::default()
                .block(panel().title(dim(format!(" max temp · last {} polls ", h.len()))))
                .data(h.as_slice())
                .max(95)
                .style(Style::new().fg(temp_color(now as f64)));
            f.render_widget(spark, right);
        }

        // two big bars per GPU: util on the first row, memory on the second
        let inner = area.width.saturating_sub(4) as usize; // borders + padding
        const LEFT: usize = 8 + 7 + 7 + 6; // label, name, temp, "util "
        const RIGHT: usize = 13; // value after the bar
        let width = inner.saturating_sub(LEFT + RIGHT).max(10);
        let many = j.gpus.len() > 1;
        for (i, g) in j.gpus.iter().take(4).enumerate() {
            let tag = if many { format!("gpu {i}") } else { "gpu".to_string() };
            let mut util = vec![
                dim(format!("{tag:<8}")),
                Span::styled(format!("{:<7}", g.name), Style::new().fg(TEXT).bold()),
                Span::styled(format!("{:<7}", temp_text(g.temp)), Style::new().fg(temp_color(g.temp as f64)).bold()),
                dim("util  "),
            ];
            util.extend(big_bar(g.util as f64 / 100.0, width));
            util.push(Span::styled(format!(" {:>4}%", g.util), Style::new().fg(TEXT).bold()));
            lines.push(Line::from(util));

            let mut mem = vec![dim(format!("{:<22}", "")), dim("mem   ")];
            mem.extend(big_bar(g.mem_used as f64 / g.mem_total.max(1) as f64, width));
            mem.push(text(format!(" {:>3.0}/{:.0} GB", g.mem_used as f64 / 1024.0, g.mem_total as f64 / 1024.0)));
            lines.push(Line::from(mem));
        }
        if j.gpus.len() > 4 {
            lines.push(Line::from(vec![label(""), dim(format!("+{} more GPUs", j.gpus.len() - 4))]));
        }
        if j.gpus.is_empty() {
            lines.push(Line::from(vec![label("gpu"), dim("no reading yet")]));
        }
    } else if let Some(end) = j.end {
        lines.push(Line::from(vec![
            label("ended"),
            Span::styled(j.state.to_lowercase().replace('_', " "), Style::new().fg(color)),
            text(format!(" · {} ({})", ago(end), clock(end))),
            dim(format!(" · exit {}", j.exit)),
        ]));
        lines.push(Line::from(vec![
            label("ran"),
            text(format!("{} of {}", dur(j.used), dur(j.limit))),
            dim(format!(" · {} · {} · {}", j.gpu_label, j.nodes, j.partition)),
        ]));
    } else {
        let start = j.start.map_or("unknown".to_string(), start_text);
        lines.push(Line::from(vec![label("start"), text(start)]));
        lines.push(Line::from(vec![label("reason"), text(j.reason.clone())]));
        lines.push(Line::from(vec![
            label("asks"),
            text(format!("{} · {} · {}", j.gpu_label, dur(j.limit), j.partition)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if app.filtering || !app.filter.is_empty() {
        let mut spans = vec![key(" /"), text(app.filter.clone())];
        if app.filtering {
            spans.push(Span::styled("█", Style::new().fg(ACCENT)));
            spans.push(dim("   enter keep · esc clear"));
        } else {
            spans.push(dim("   (esc to clear)"));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    } else {
        let hints = Line::from(vec![
            key(" ↑↓"), dim(" move  "),
            key("k"), dim(" commands  "),
            key("enter"), dim(" log  "),
            key("s"), dim(" shell  "),
            key("x"), dim(" cancel  "),
            key("/"), dim(" filter  "),
            key("?"), dim(" help  "),
            key("q"), dim(" quit"),
        ]);
        f.render_widget(Paragraph::new(hints), area);
    }
    if let Some((msg, err, when)) = &app.toast {
        if when.elapsed().as_secs() < TOAST_SECS {
            let color = if *err { RED } else { GREEN };
            let line = Line::from(Span::styled(format!("{msg} "), Style::new().fg(color))).right_aligned();
            f.render_widget(Paragraph::new(line), area);
        }
    }
}

fn draw_log(f: &mut Frame, app: &App, v: &LogView) {
    let area = f.area();
    f.render_widget(Clear, area);
    let c = &app.clusters[v.ci];
    let path = v.log.as_ref().map_or(String::new(), |l| format!("{} · {}", shorten(&l.path, 60), size(l.size)));
    let mut title = vec![
        Span::styled(format!(" {} ", v.name), Style::new().fg(TEXT).bold()),
        dim(format!("{} · {} · {} ", v.id, c.host, if v.stderr { "stderr" } else { "stdout" })),
    ];
    if v.loading {
        title.extend([spinner(app.tick), Span::raw(" ")]);
    }
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: &[String] = v.log.as_ref().map_or(&[], |l| &l.lines);
    let n = lines.len();
    let top = v.top.unwrap_or(n.saturating_sub(inner_h)).min(n.saturating_sub(1));
    let position = if v.top.is_none() {
        "following end".to_string()
    } else {
        format!("lines {}–{} of {n}", top + 1, (top + inner_h).min(n))
    };
    let block = panel()
        .title(Line::from(title))
        .title_top(Line::from(dim(format!(" {path} "))).right_aligned())
        .title_bottom(Line::from(vec![
            key(" ↑↓ pgup pgdn g G"), dim(" scroll  "),
            key("e"), dim(" stdout/stderr  "),
            key("r"), dim(" reload  "),
            key("esc"), dim(" close "),
        ]))
        .title_bottom(Line::from(dim(format!(" {position} "))).right_aligned());

    let body: Vec<Line> = if let Some(e) = &v.error {
        vec![Line::from(Span::styled(e.clone(), Style::new().fg(RED)))]
    } else if v.log.is_none() {
        vec![Line::from(dim("loading…"))]
    } else if n == 0 {
        vec![Line::from(dim("(log is empty)"))]
    } else {
        lines[top..(top + inner_h).min(n)].iter().map(|l| highlight::line(l)).collect()
    };
    f.render_widget(Paragraph::new(body).block(block), area);
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn shorten(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_string() } else { format!("…{}", s.chars().skip(n - max + 1).collect::<String>()) }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_help(f: &mut Frame) {
    let rows = [
        ("↑ ↓  j", "move"),
        ("k  :", "command palette: every action and setting"),
        ("g G", "first / last job"),
        ("enter  l", "open the job's log"),
        ("s", "shell into the job (its GPU node if running)"),
        ("x", "cancel the job"),
        ("y", "copy the job id"),
        ("/", "filter by name, folder or id"),
        ("h", "ended jobs: recent → all from 24h → hidden"),
        ("n", "turn Mac notifications on / off"),
        ("r", "refresh now"),
        ("q  esc", "quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<12}"), Style::new().fg(ACCENT)), text(*d)]))
        .collect();
    let area = centered(f.area(), 66, lines.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = panel()
        .title(Span::styled(" keys ", Style::new().fg(TEXT).bold()))
        .padding(Padding::uniform(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_confirm(f: &mut Frame, host: &str, id: &str, name: &str) {
    let area = centered(f.area(), 52, 7);
    f.render_widget(Clear, area);
    let lines = vec![
        Line::from(vec![text("Cancel "), Span::styled(name.to_string(), Style::new().fg(TEXT).bold()), text("?")]),
        Line::from(dim(format!("job {id} on {host}"))),
        Line::from(""),
        Line::from(vec![key("y"), dim(" yes, cancel it    "), key("any other key"), dim(" no")]),
    ];
    let block = panel()
        .border_style(Style::new().fg(RED))
        .padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_palette(f: &mut Frame, app: &App, p: &Palette) {
    if let Some(prompt) = &p.prompt {
        let area = centered(f.area(), 64, 7);
        f.render_widget(Clear, area);
        let block = panel()
            .border_style(Style::new().fg(ACCENT))
            .title(Span::styled(format!(" {} ", prompt.title), Style::new().fg(TEXT).bold()))
            .title_bottom(Line::from(vec![key(" enter"), dim(" save  "), key("esc"), dim(" cancel ")]))
            .padding(Padding::horizontal(1));
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("› ", Style::new().fg(ACCENT)),
                text(prompt.text.clone()),
                Span::styled("█", Style::new().fg(ACCENT)),
            ]),
            Line::from(""),
            Line::from(dim(prompt.help.clone())),
        ];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    let Some(level) = p.stack.last() else { return };
    let area = centered(f.area(), 72, 22);
    f.render_widget(Clear, area);
    let crumbs: Vec<&str> = p.stack.iter().map(|l| l.menu.title()).collect();
    let block = panel()
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(format!(" {} ", crumbs.join(" › ")), Style::new().fg(TEXT).bold()))
        .title_bottom(Line::from(vec![
            key(" ↑↓"), dim(" choose  "),
            key("enter"), dim(" run  "),
            key("esc"), dim(if p.stack.len() > 1 { " back " } else { " close " }),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [input, gap, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    let mut input_line = vec![Span::styled(" › ", Style::new().fg(ACCENT)), text(level.query.clone())];
    input_line.push(Span::styled("█", Style::new().fg(ACCENT)));
    if level.query.is_empty() {
        input_line.push(dim(" type to search"));
    }
    f.render_widget(Paragraph::new(Line::from(input_line)), input);
    f.render_widget(Paragraph::new(Span::styled("─".repeat(gap.width as usize), Style::new().fg(FAINT))), gap);

    let items: Vec<palette::Item> =
        palette::search(app, level.menu, &level.query);
    if items.is_empty() {
        f.render_widget(Paragraph::new(dim("  nothing matches")), list_area);
        return;
    }
    // room for the highlight symbol
    let width = (list_area.width as usize).saturating_sub(1);
    let rows: Vec<ListItem> = items
        .iter()
        .map(|it| {
            let group = if it.group.is_empty() { String::new() } else { format!("{:<9}", it.group) };
            let arrow = if matches!(it.act, Act::Open(_)) { " ›" } else { "" };
            let used = 1 + group.chars().count() + it.label.chars().count() + arrow.chars().count();
            let hint = format!("{} ", it.hint);
            let pad = width.saturating_sub(used + hint.chars().count());
            let hint_color = if it.hint == "on" || it.hint == "✓" { GREEN } else { DIM };
            ListItem::new(Line::from(vec![
                dim(format!(" {group}")),
                text(it.label.clone()),
                dim(arrow),
                Span::raw(" ".repeat(pad)),
                Span::styled(hint, Style::new().fg(hint_color)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(level.sel.min(items.len() - 1)));
    let list = List::new(rows)
        .highlight_style(Style::new().bg(SEL_BG))
        .highlight_symbol("▌")
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(list, list_area, &mut state);
}
