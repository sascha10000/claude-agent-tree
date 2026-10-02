//! Session tab bar shown above Detail and Terminal: every running/waiting
//! session with its status; Ctrl-n focuses it for switching.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{Activity, App, SessionTab};

use super::truncate;

/// Longest label (`project · title`) a single tab may take.
const LABEL_MAX: usize = 32;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let tabs = app.session_tabs();
    let focused = app.tabbar.map(|s| s.min(tabs.len().saturating_sub(1)));
    let current = app.current_session_id();
    let dim = Style::default().fg(Color::DarkGray);

    let prefix = if focused.is_some() {
        Span::styled(" ←/→ 1-9 ⏎ esc ", Style::default().fg(Color::Black).bg(Color::Cyan))
    } else {
        Span::styled(" ^n ", dim)
    };
    let mut spans = vec![prefix];
    if tabs.is_empty() {
        spans.push(Span::styled(" no running or waiting sessions", dim));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let rendered: Vec<Vec<Span>> = tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| tab_spans(i, tab, focused == Some(i), current == Some(tab.id.as_str())))
        .collect();
    let width = |s: &[Span]| s.iter().map(|s| s.content.chars().count()).sum::<usize>();

    // Scroll so the focused (else current) tab is visible: drop tabs from the
    // left until everything up to it fits.
    let target = focused
        .or_else(|| tabs.iter().position(|t| Some(t.id.as_str()) == current))
        .unwrap_or(0);
    let avail = (area.width as usize).saturating_sub(width(&spans) + 2);
    let mut start = 0;
    while start < target && rendered[start..=target].iter().map(|s| width(s)).sum::<usize>() > avail {
        start += 1;
    }
    if start > 0 {
        spans.push(Span::styled("‹", dim));
    }
    let mut used = 0;
    for tab in &rendered[start..] {
        let w = width(tab);
        if used + w > avail {
            spans.push(Span::styled("›", dim));
            break;
        }
        used += w;
        spans.extend(tab.iter().cloned());
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn tab_spans(i: usize, tab: &SessionTab, focused: bool, current: bool) -> Vec<Span<'static>> {
    let (glyph, color) = match tab.activity {
        Activity::Working => ("●", Color::Yellow),
        Activity::SubagentsWorking => ("⑂", Color::Yellow),
        Activity::AwaitingInput => ("▶", Color::Green),
        Activity::NeedsPermission => ("⚠", Color::Red),
        Activity::Idle => ("○", Color::DarkGray),
    };
    let base = if focused {
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if current {
        Style::default().bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    // Number keys only reach the first nine tabs.
    let number = if i < 9 { format!(" {} ", i + 1) } else { "   ".into() };
    let label = truncate(&format!("{} · {}", tab.project, tab.title), LABEL_MAX);
    let glyph_style = if focused { base } else { base.fg(color) };
    vec![
        Span::styled(number, base.fg(if focused { Color::Black } else { Color::DarkGray })),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(format!("{label} "), base),
        Span::raw(" "),
    ]
}
