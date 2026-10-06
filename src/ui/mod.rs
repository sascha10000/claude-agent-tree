//! Top-level draw: layout split, view dispatch, status bar.

mod browse;
mod detail;
mod overlay;
mod tabs;
mod term;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{Activity, App, View};

pub fn draw(frame: &mut Frame, app: &App) {
    // Outside Browse a session tab bar sits on top (Browse already lists them).
    let tab_rows = if app.view == View::Browse { 0 } else { 1 };
    let [tab_bar, main, status] = *Layout::vertical([
        Constraint::Length(tab_rows),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(frame.area()) else {
        return;
    };
    if tab_rows > 0 {
        tabs::draw(frame, app, tab_bar);
    }
    match app.view {
        View::Browse => browse::draw(frame, app, main),
        View::Detail => detail::draw(frame, app, main),
        View::Terminal => term::draw(frame, app, main),
    }
    overlay::draw(frame, app, main);
    draw_statusbar(frame, app, status);
}

fn draw_statusbar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    if let Some(msg) = &app.status_msg {
        spans.push(Span::styled(msg.clone(), Style::default().fg(Color::Red)));
    } else {
        match app.view {
            View::Browse => {
                let sessions: usize = app.index.projects.iter().map(|p| p.sessions.len()).sum();
                spans.push(Span::raw(format!(
                    " {} projects · {} sessions",
                    app.index.projects.len(),
                    sessions
                )));
            }
            View::Terminal => {
                if let Some(id) = &app.term_session {
                    spans.push(Span::raw(format!(" claude {}", &id[..8.min(id.len())])));
                    spans.push(activity_span(app, id));
                }
            }
            View::Detail => {
                if let Some(session) = &app.loaded {
                    spans.push(Span::raw(format!(
                        " {} · {} events · {} agents",
                        session.meta.title,
                        session.timeline.len(),
                        app.agent_rows.len().saturating_sub(1)
                    )));
                    spans.push(activity_span(app, &session.meta.id));
                    if let Some(cost) = &session.cost {
                        spans.push(Span::raw(format!(
                            " · ${:.2} · +{}/-{} lines",
                            cost.total_cost_usd, cost.total_lines_added, cost.total_lines_removed
                        )));
                    }
                    if session.parse_errors > 0 {
                        spans.push(Span::styled(
                            format!(" · ⚠ {} parse errors", session.parse_errors),
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                }
            }
        }
    }
    spans.push(Span::styled(
        if app.watching { " · ● watching" } else { " · ○ no watch (r = refresh)" },
        Style::default().fg(if app.watching { Color::Green } else { Color::DarkGray }),
    ));
    if app.hooks.active() {
        spans.push(Span::styled(" · ⚓ hooks", Style::default().fg(Color::Green)));
    }
    let hints = match app.view {
        View::Browse => "  j/k move · enter open · / filter · s sort · a stats · A active · f fleet · n new · N new in dir · R resume · w goto · ? help ",
        View::Detail => "  / search · e error · T think · o order · O full · c cost · t lanes · x export · R resume · w goto · ^n tabs · ? help ",
        View::Terminal => "  keys go to claude · ctrl-n tabs · ctrl-q detach (keeps running) ",
    };
    spans.push(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// ` · ● working` / ` · ⑂ subagents working` / ` · ▶ awaits input` for the
/// statusbar; empty when idle or unknown.
fn activity_span(app: &App, session_id: &str) -> Span<'static> {
    let Some(meta) =
        app.index.projects.iter().flat_map(|p| &p.sessions).find(|s| s.id == session_id)
    else {
        return Span::raw("");
    };
    match app.activity(meta) {
        Activity::Working => {
            Span::styled(" · ● working", Style::default().fg(Color::Yellow))
        }
        Activity::SubagentsWorking => {
            Span::styled(" · ⑂ subagents working", Style::default().fg(Color::Yellow))
        }
        Activity::AwaitingInput => {
            Span::styled(" · ▶ awaits input", Style::default().fg(Color::Green))
        }
        Activity::NeedsPermission => Span::styled(
            format!(" · ⚠ needs permission: {}", app.activity_detail(session_id).unwrap_or("?")),
            Style::default().fg(Color::Red).add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Activity::Idle => Span::raw(""),
    }
}

/// Visible slice `[start, start+height)` keeping `selected` centered-ish.
pub fn window(selected: usize, len: usize, height: usize) -> std::ops::Range<usize> {
    if len == 0 || height == 0 {
        return 0..0;
    }
    let half = height / 2;
    let start = selected.saturating_sub(half).min(len.saturating_sub(height));
    start..(start + height).min(len)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn relative_time(t: std::time::SystemTime) -> String {
    let elapsed = t.elapsed().unwrap_or_default().as_secs();
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

/// Style for diff-looking lines: "+…" green, "-…" red, "@@…" cyan; None otherwise.
pub fn diff_line_style(line: &str) -> Option<Style> {
    if line.starts_with("@@") {
        Some(Style::default().fg(Color::Cyan))
    } else if line.starts_with('+') {
        Some(Style::default().fg(Color::Green))
    } else if line.starts_with('-') {
        Some(Style::default().fg(Color::Red))
    } else {
        None
    }
}

/// Like `format_duration`, for the raw millisecond counters in `cost-state`.
pub fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn format_duration(d: jiff::SignedDuration) -> String {
    let secs = d.as_secs().max(0);
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::diff_line_style;
    use ratatui::style::Color;

    #[test]
    fn diff_line_style_colors_by_prefix() {
        assert_eq!(diff_line_style("+added").unwrap().fg, Some(Color::Green));
        assert_eq!(diff_line_style("-removed").unwrap().fg, Some(Color::Red));
        assert_eq!(diff_line_style("@@ -1,2 +1,3 @@").unwrap().fg, Some(Color::Cyan));
        assert!(diff_line_style(" context").is_none());
        assert!(diff_line_style("plain").is_none());
    }
}
