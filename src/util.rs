//! Small helpers: Slurm time parsing, formatting, colors.

use chrono::{DateTime, FixedOffset, Local, Utc};
use ratatui::style::{Color, Style};
use ratatui::text::Span;

pub const DIM: Color = Color::Rgb(110, 115, 130);
pub const FAINT: Color = Color::Rgb(60, 64, 76);
pub const TEXT: Color = Color::Rgb(215, 220, 230);
pub const GREEN: Color = Color::Rgb(80, 220, 140);
pub const SOFT_GREEN: Color = Color::Rgb(90, 165, 125);
pub const YELLOW: Color = Color::Rgb(240, 200, 80);
pub const ORANGE: Color = Color::Rgb(255, 150, 60);
pub const RED: Color = Color::Rgb(255, 80, 80);
pub const ACCENT: Color = Color::Rgb(120, 170, 255);
pub const SEL_BG: Color = Color::Rgb(38, 42, 54);

/// temperature (°C) -> blue, green, yellow, orange, red
pub fn temp_color(t: f64) -> Color {
    const STOPS: [(f64, (u8, u8, u8)); 5] = [
        (30.0, (90, 160, 255)),
        (50.0, (80, 220, 140)),
        (65.0, (240, 220, 80)),
        (78.0, (255, 140, 50)),
        (88.0, (255, 50, 60)),
    ];
    let rgb = |c: (u8, u8, u8)| Color::Rgb(c.0, c.1, c.2);
    if t <= STOPS[0].0 {
        return rgb(STOPS[0].1);
    }
    for w in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * f).round() as u8;
            return Color::Rgb(mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2));
        }
    }
    rgb(STOPS[4].1)
}

/// fraction 0..1 drawn as a bar, colored along the temperature gradient
pub fn bar(frac: f64, width: usize) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    vec![
        Span::styled("━".repeat(filled), Style::new().fg(temp_color(40.0 + frac * 48.0))),
        Span::styled("━".repeat(width - filled), Style::new().fg(FAINT)),
    ]
}

/// (symbol, short label, color) for a Slurm state
pub fn state_look(state: &str) -> (&'static str, &'static str, Color) {
    match state {
        "RUNNING" => ("●", "run", GREEN),
        "PENDING" => ("◌", "wait", YELLOW),
        "CONFIGURING" => ("◍", "boot", ACCENT),
        "COMPLETING" => ("◍", "end", DIM),
        "COMPLETED" => ("✓", "done", SOFT_GREEN),
        "FAILED" => ("✗", "fail", RED),
        "OUT_OF_MEMORY" => ("✗", "oom", RED),
        "NODE_FAIL" | "BOOT_FAIL" => ("✗", "node", RED),
        "TIMEOUT" | "DEADLINE" => ("⏱", "time", ORANGE),
        "PREEMPTED" => ("⊘", "prmt", ORANGE),
        "CANCELLED" => ("⊘", "cncl", DIM),
        "SUSPENDED" => ("‖", "susp", DIM),
        _ => ("·", "?", DIM),
    }
}

pub fn parse_offset(s: &str) -> Option<FixedOffset> {
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let h: i32 = s.get(1..3)?.parse().ok()?;
    let m: i32 = s.get(3..5)?.parse().ok()?;
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

/// '1-02:03:04', '2:03:04', '03:04' -> seconds
pub fn slurm_secs(s: &str) -> Option<u64> {
    if s.is_empty() || !s.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse().ok()?, r),
        None => (0u64, s),
    };
    let mut parts: Vec<u64> = rest.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    while parts.len() < 3 {
        parts.insert(0, 0);
    }
    Some(days * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2])
}

pub fn dur(secs: Option<u64>) -> String {
    let Some(secs) = secs else { return "∞".into() };
    let m = secs / 60;
    let (d, h, m) = (m / 1440, (m / 60) % 24, m % 60);
    match (d, h, m) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m:02}"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

/// 12-hour local clock, with the weekday if it's not within a day of now
pub fn clock(t: DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    let far = (t - Utc::now()).num_seconds().abs() >= 86400;
    local.format(if far { "%a %I:%M%P" } else { "%I:%M%P" }).to_string()
}

pub fn start_text(start: DateTime<Utc>) -> String {
    let delta = (start - Utc::now()).num_seconds();
    if delta <= 0 {
        format!("soon · {}", clock(start))
    } else {
        format!("in {} · {}", dur(Some(delta as u64)), clock(start))
    }
}

pub fn ago(t: DateTime<Utc>) -> String {
    let secs = (Utc::now() - t).num_seconds().max(0) as u64;
    if secs < 60 { "just now".into() } else { format!("{} ago", dur(Some(secs))) }
}

/// squeue %b: 'gres/gpu:h200:1' -> '1× H200'
pub fn gpu_from_gres(gres: &str) -> String {
    let Some(i) = gres.find("gpu:") else { return "cpu".into() };
    let rest = gres[i + 4..].split(',').next().unwrap_or("");
    let parts: Vec<&str> = rest.split(':').collect();
    let count = |s: &str| s.chars().take_while(char::is_ascii_digit).collect::<String>();
    match parts.as_slice() {
        [kind, n, ..] => format!("{}× {}", count(n), kind.to_uppercase()),
        [n] => format!("{}× GPU", count(n)),
        _ => "gpu".into(),
    }
}

/// sacct AllocTRES: '...,gres/gpu:h200=1,gres/gpu=1,...' -> '1× H200'
pub fn gpu_from_tres(tres: &str) -> String {
    let mut plain = None;
    for item in tres.split(',') {
        if let Some(rest) = item.strip_prefix("gres/gpu:") {
            if let Some((kind, n)) = rest.split_once('=') {
                return format!("{n}× {}", kind.to_uppercase());
            }
        } else if let Some(n) = item.strip_prefix("gres/gpu=") {
            plain = Some(format!("{n}× GPU"));
        }
    }
    if tres.trim().is_empty() {
        return "–".into();
    }
    plain.unwrap_or_else(|| "cpu".into())
}

/// 'NVIDIA A100-SXM4-80GB' -> 'A100'
pub fn short_gpu_name(name: &str) -> String {
    let name = name.trim().trim_start_matches("NVIDIA").trim();
    name.split([' ', '-']).next().unwrap_or(name).to_string()
}

pub fn strip_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    match path.strip_prefix(prefix) {
        Some("") => ".",
        Some(p) => p,
        None => path,
    }
}

/// Quote for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Clean one log line for display: keep what a terminal would show after '\r', drop ANSI codes.
pub fn clean_log_line(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let line = line.rsplit('\r').find(|s| !s.trim().is_empty()).unwrap_or("");
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}
