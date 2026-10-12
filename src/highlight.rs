//! Coloring for the log viewer.
//!
//! Lines that carry their own ANSI colors are shown with those colors.
//! Plain lines get light highlighting: timestamps, log levels, tracebacks, numbers,
//! strings, paths, URLs, key=value pairs and progress bars.

use std::sync::LazyLock;

use ratatui::prelude::*;
use regex::Regex;

use crate::util::*;

const NUMBER: Color = Color::Rgb(215, 160, 255);
const STRING: Color = Color::Rgb(150, 215, 140);
const PATH: Color = Color::Rgb(110, 200, 215);
const KEY: Color = Color::Rgb(140, 160, 200);
const ERR_TEXT: Color = Color::Rgb(255, 150, 150);
const WARN_TEXT: Color = Color::Rgb(240, 215, 150);
const BAR: Color = Color::Rgb(80, 220, 140);

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?P<ts>\[?\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?\]?|\b\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\b)",
        r"|(?P<err>\b(?:ERROR|CRITICAL|FATAL|FAILED|Traceback|\w*Error|\w*Exception)\b)",
        r"|(?P<warn>\b(?:WARNING|WARN|Warning|DEPRECATED)\b)",
        r"|(?P<info>\b(?:INFO|NOTICE)\b)",
        r"|(?P<debug>\b(?:DEBUG|TRACE)\b)",
        r"|(?P<ok>\b(?:OK|DONE|SUCCESS|PASSED|Done|done|saved|Saved)\b)",
        r"|(?P<url>https?://[^\s'\x22)\]]+)",
        r#"|(?P<str>"[^"\n]*"|'[\w./:@+\-]*')"#,
        r"|(?P<path>(?:~|\.{1,2})?/(?:[\w.@+\-]+/)+[\w.@+\-]*)",
        r"|(?P<key>\b[A-Za-z_][\w.\-]*(?:=|:\s))",
        r"|(?P<num>[-+]?\b\d+(?:\.\d+)?(?:[eE][-+]?\d+)?(?:%|[kKMGT]i?B?\b|it/s|s/it|ms|s\b)?)",
        r"|(?P<bar>[█▉▊▋▌▍▎▏━─#]{3,})",
    ))
    .unwrap()
});

static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:Traceback \(most recent call last\)|[\w.]*(?:Error|Exception|Interrupt)\b:?|.*\b(?:ERROR|CRITICAL|FATAL)\b|srun: error|slurmstepd: error|.*\bCUDA out of memory)").unwrap()
});
static WARN_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:WARNING|WARN|UserWarning|FutureWarning|DeprecationWarning)\b").unwrap());
static TRACE_FRAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^\s+File ".*", line \d+"#).unwrap());

pub fn line(text: &str) -> Line<'static> {
    if text.contains('\x1b') {
        return ansi(text);
    }
    plain(text)
}

fn plain(text: &str) -> Line<'static> {
    let base = if ERROR_LINE.is_match(text) {
        Style::new().fg(ERR_TEXT)
    } else if WARN_LINE.is_match(text) {
        Style::new().fg(WARN_TEXT)
    } else if TRACE_FRAME.is_match(text) {
        Style::new().fg(DIM)
    } else {
        Style::new().fg(TEXT)
    };

    let mut spans = Vec::new();
    let mut last = 0;
    for caps in TOKENS.captures_iter(text) {
        let m = caps.get(0).unwrap();
        if m.start() > last {
            spans.push(Span::styled(text[last..m.start()].to_string(), base));
        }
        let style = if caps.name("ts").is_some() {
            Style::new().fg(DIM)
        } else if caps.name("err").is_some() {
            Style::new().fg(RED).bold()
        } else if caps.name("warn").is_some() {
            Style::new().fg(YELLOW).bold()
        } else if caps.name("info").is_some() {
            Style::new().fg(ACCENT)
        } else if caps.name("debug").is_some() {
            Style::new().fg(DIM)
        } else if caps.name("ok").is_some() {
            Style::new().fg(GREEN)
        } else if caps.name("url").is_some() {
            Style::new().fg(ACCENT).underlined()
        } else if caps.name("str").is_some() {
            Style::new().fg(STRING)
        } else if caps.name("path").is_some() {
            Style::new().fg(PATH)
        } else if caps.name("key").is_some() {
            Style::new().fg(KEY)
        } else if caps.name("num").is_some() {
            Style::new().fg(NUMBER)
        } else if caps.name("bar").is_some() {
            Style::new().fg(BAR)
        } else {
            base
        };
        spans.push(Span::styled(m.as_str().to_string(), style));
        last = m.end();
    }
    if last < text.len() {
        spans.push(Span::styled(text[last..].to_string(), base));
    }
    Line::from(spans)
}

/// Apply SGR color codes (ESC [ ... m) the way a terminal would.
fn ansi(text: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::new().fg(TEXT);
    let mut buf = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("\x1b[") {
        buf.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find(|c: char| c.is_ascii_alphabetic()) else {
            rest = "";
            break;
        };
        if after.as_bytes()[end] == b'm' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            style = apply_sgr(style, &after[..end]);
        }
        rest = &after[end + 1..];
    }
    buf.push_str(rest);
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split(';').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let mut i = 0;
    while i < codes.len() {
        let c = codes[i];
        match c {
            0 => style = Style::new().fg(TEXT),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            7 => style = style.add_modifier(Modifier::REVERSED),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            30..=37 => style = style.fg(Color::Indexed((c - 30) as u8)),
            39 => style = style.fg(TEXT),
            40..=47 => style = style.bg(Color::Indexed((c - 40) as u8)),
            49 => style.bg = None,
            90..=97 => style = style.fg(Color::Indexed((c - 90 + 8) as u8)),
            100..=107 => style = style.bg(Color::Indexed((c - 100 + 8) as u8)),
            38 | 48 => {
                let color = match codes.get(i + 1) {
                    Some(5) => {
                        let n = codes.get(i + 2).copied().unwrap_or(0) as u8;
                        i += 2;
                        Some(Color::Indexed(n))
                    }
                    Some(2) => {
                        let g = |k: usize| codes.get(i + k).copied().unwrap_or(0) as u8;
                        let col = Color::Rgb(g(2), g(3), g(4));
                        i += 4;
                        Some(col)
                    }
                    _ => None,
                };
                if let Some(col) = color {
                    style = if c == 38 { style.fg(col) } else { style.bg(col) };
                }
            }
            _ => {}
        }
        i += 1;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color_of(l: &Line, word: &str) -> Option<Color> {
        l.spans.iter().find(|s| s.content.contains(word)).and_then(|s| s.style.fg)
    }

    #[test]
    fn traceback_lines_are_red() {
        let l = line("ValueError: shapes do not match");
        assert_eq!(color_of(&l, "shapes"), Some(ERR_TEXT));
        let l = line("torch.OutOfMemoryError: CUDA out of memory. Tried to allocate 2.00 GiB");
        assert_eq!(color_of(&l, "Tried"), Some(ERR_TEXT));
    }

    #[test]
    fn levels_and_tokens() {
        let l = line("2026-10-12 09:26:15,180 INFO loss=0.25 saved to /scratch/me/ckpt/step_100.pt");
        assert_eq!(color_of(&l, "2026-10-12"), Some(DIM));
        assert_eq!(color_of(&l, "INFO"), Some(ACCENT));
        assert_eq!(color_of(&l, "0.25"), Some(NUMBER));
        assert_eq!(color_of(&l, "/scratch"), Some(PATH));
        let l = line("WARNING something odd");
        assert_eq!(color_of(&l, "odd"), Some(WARN_TEXT));
    }

    #[test]
    fn ansi_colors_are_kept() {
        let raw = "plain \x1b[31mred\x1b[0m \x1b[1;38;2;10;20;30mrgb\x1b[0m";
        let l = line(&crate::util::clean_log_line(raw));
        assert_eq!(color_of(&l, "red"), Some(Color::Indexed(1)));
        assert_eq!(color_of(&l, "rgb"), Some(Color::Rgb(10, 20, 30)));
        assert!(l.spans.iter().all(|s| !s.content.contains('\x1b')));
    }

    #[test]
    fn non_color_escapes_are_dropped() {
        let cleaned = crate::util::clean_log_line("\x1b[2Kprogress 10%\rprogress 50%");
        assert_eq!(cleaned, "progress 50%");
    }
}
