//! Session tab bar shown above Detail and Terminal: every running/waiting
//! session with its status; Ctrl-n focuses it for switching, which unfolds
//! it into a vertical list (one row per tab) until focus leaves again.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{Activity, App, SessionTab};

use super::truncate;

/// Longest label (`project · title`) a single tab may take.
const LABEL_MAX: usize = 32;

/// Rows the bar takes: one when horizontal; when focused, a hint row plus a
/// row per tab, capped at half the screen (the list scrolls beyond that).
pub fn height(app: &App, screen: u16) -> u16 {
    if app.tabbar.is_none() {
        return 1;
    }
    let rows = app.session_tabs().len() as u16 + 1;
    rows.clamp(1, (screen / 2).max(2))
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let tabs = app.session_tabs();
    let focused = app.tabbar.map(|s| s.min(tabs.len().saturating_sub(1)));
    let current = app.current_session_id();
    let dim = Style::default().fg(Color::DarkGray);

    if let Some(selected) = focused.filter(|_| !tabs.is_empty()) {
        draw_list(frame, &tabs, selected, current, area);
        return;
    }

    let mut spans = vec![Span::styled(" ^n ", dim)];
    if tabs.is_empty() {
        spans.push(Span::styled(" no running or waiting sessions", dim));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let rendered: Vec<Vec<Span>> = tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| tab_spans(i, tab, false, current == Some(tab.id.as_str()), LABEL_MAX))
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

/// Focused layout: hint row, then one full-width row per tab, scrolled so
/// the selected one stays visible.
fn draw_list(frame: &mut Frame, tabs: &[SessionTab], selected: usize, current: Option<&str>, area: Rect) {
    let hint = Style::default().fg(Color::Black).bg(Color::Cyan);
    let mut lines = vec![Line::from(Span::styled(" ↑/↓ 1-9 ⏎ esc ", hint))];
    let visible = (area.height as usize).saturating_sub(1).max(1);
    let start = (selected + 1).saturating_sub(visible);
    for (i, tab) in tabs.iter().enumerate().skip(start).take(visible) {
        let is_sel = i == selected;
        let label_max = (area.width as usize).saturating_sub(8);
        let mut spans = tab_spans(i, tab, is_sel, current == Some(tab.id.as_str()), label_max);
        spans.pop(); // the inter-tab gap is only needed horizontally
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        if is_sel {
            // Stretch the highlight across the whole row.
            let pad = (area.width as usize).saturating_sub(used);
            spans.push(Span::styled(" ".repeat(pad), hint));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn tab_spans(i: usize, tab: &SessionTab, focused: bool, current: bool, label_max: usize) -> Vec<Span<'static>> {
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
    let label = truncate(&format!("{} · {}", tab.project, tab.title), label_max);
    let glyph_style = if focused { base } else { base.fg(color) };
    vec![
        Span::styled(number, base.fg(if focused { Color::Black } else { Color::DarkGray })),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(format!("{label} "), base),
        Span::raw(" "),
    ]
}
