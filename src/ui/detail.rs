//! Session detail view: timeline (left) + agent graph and event detail (right).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::session::{first_line, status_glyph, EventKind, TimelineEvent, ToolStatus};

use super::{format_duration, truncate, window};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        *Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area)
    else {
        return;
    };
    let [graph, detail] =
        *Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).split(right)
    else {
        return;
    };
    draw_timeline(frame, app, left);
    draw_graph(frame, app, graph);
    draw_detail(frame, app, detail);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn status_color(status: ToolStatus) -> Color {
    match status {
        ToolStatus::Ok => Color::Green,
        ToolStatus::Error => Color::Red,
        ToolStatus::Denied => Color::Magenta,
        ToolStatus::Pending => Color::Yellow,
    }
}

fn event_style(kind: &EventKind) -> Style {
    match kind {
        EventKind::UserPrompt { .. } => {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        }
        EventKind::AssistantText { .. } => Style::default().fg(Color::White),
        EventKind::Thinking { .. } => {
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
        }
        EventKind::ToolCall { status, .. } => match status {
            ToolStatus::Ok => Style::default().fg(Color::Cyan),
            other => Style::default().fg(status_color(*other)),
        },
        EventKind::SubagentSpawn { .. } => {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        }
        EventKind::SystemNote { .. } => Style::default().fg(Color::DarkGray),
    }
}

fn timeline_label(event: &TimelineEvent) -> String {
    match &event.kind {
        EventKind::UserPrompt { text } => format!("▸ user  {}", first_line(text, 70)),
        EventKind::AssistantText { text } => format!("✻ {}", first_line(text, 70)),
        EventKind::Thinking { text } => format!("∴ {}", first_line(text, 70)),
        EventKind::ToolCall { one_liner, status, .. } => {
            format!("⚒ {} {}", one_liner, status_glyph(*status))
        }
        EventKind::SubagentSpawn { agent_type, description, status, .. } => {
            format!("⑂ {agent_type}: {} {}", first_line(description, 45), status_glyph(*status))
        }
        EventKind::SystemNote { text } => format!("· {}", first_line(text, 70)),
    }
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.search_input {
        format!(" Timeline /{}▌ ", app.search)
    } else if !app.search.is_empty() {
        format!(" Timeline /{} ({}) n/N ", app.search, app.search_matches.len())
    } else {
        " Timeline ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(app.focus == Focus::Timeline));
    let Some(session) = &app.loaded else {
        frame.render_widget(block, area);
        return;
    };
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    // Window over POSITIONS in the visible list; highlight by raw index.
    let visible = app.visible_events();
    let selected_pos =
        visible.iter().position(|&i| i == app.selected_event).unwrap_or(0);
    let range = window(selected_pos, visible.len(), inner_height);

    let mut lines = Vec::new();
    for pos in range {
        let i = visible[pos];
        let event = &session.timeline[i];
        let ts = event
            .timestamp
            .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        let gutter = "│ ".repeat(event.agent_path.len());
        let label = timeline_label(event);
        let mut style = event_style(&event.kind);
        if app.search_matches.binary_search(&i).is_ok() {
            style = style.add_modifier(Modifier::UNDERLINED);
            if i != app.selected_event {
                style = style.fg(Color::Yellow);
            }
        }
        if i == app.selected_event {
            style = style.bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray)),
            Span::styled(gutter, Style::default().fg(Color::Blue)),
            Span::styled(truncate(&label, inner_width.saturating_sub(9)), style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn fmt_local(t: jiff::Timestamp) -> String {
    t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string()
}

fn draw_graph(frame: &mut Frame, app: &App, area: Rect) {
    if app.lanes {
        draw_lanes(frame, app, area);
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Agents ")
        .border_style(border_style(app.focus == Focus::Graph));
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let range = window(app.selected_agent, app.agent_rows.len(), inner_height);

    let mut lines = Vec::new();
    for i in range {
        let row = &app.agent_rows[i];
        let duration =
            row.duration.map(|d| format!(" ({})", format_duration(d))).unwrap_or_default();
        let desc = if row.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", row.description)
        };
        let text = format!(
            "{}{} {}{}{}",
            row.prefix,
            match row.status {
                ToolStatus::Pending => "◌",
                ToolStatus::Error => "✖",
                ToolStatus::Denied => "⊘",
                ToolStatus::Ok => "●",
            },
            row.agent_type,
            duration,
            desc
        );
        let mut style = Style::default().fg(status_color(row.status));
        if i == app.selected_agent {
            style = style.bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(truncate(&text, inner_width), style)));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Map an agent's lifetime onto `width` cells over `range`. Returns
/// (leading offset, bar length); None when the agent never started.
fn lane_cells(
    started: Option<jiff::Timestamp>,
    finished: Option<jiff::Timestamp>,
    range: (jiff::Timestamp, jiff::Timestamp),
    width: usize,
) -> Option<(usize, usize)> {
    let started = started?;
    if width == 0 {
        return None;
    }
    let (r0, r1) = range;
    let total = r1.duration_since(r0).as_millis().max(1);
    let pos = |t: jiff::Timestamp| -> usize {
        let off = t.duration_since(r0).as_millis().clamp(0, total);
        ((off * width as i128) / total) as usize
    };
    let a = pos(started).min(width.saturating_sub(1));
    // A still-running agent (finished = None) extends to the range end.
    let b = pos(finished.unwrap_or(r1)).min(width);
    Some((a, b.saturating_sub(a).max(1)))
}

/// Alternate Agents pane: one horizontal time bar per agent, so overlapping
/// async agents are visible as parallel lanes.
fn draw_lanes(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Agents · lanes ")
        .border_style(border_style(app.focus == Focus::Graph));
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let range = app.agent_rows.first().and_then(|r| Some((r.started?, r.finished?)));
    let Some((r0, r1)) = range else {
        frame.render_widget(
            Paragraph::new(Span::styled("(no timing data)", Style::default().fg(Color::DarkGray)))
                .block(block),
            area,
        );
        return;
    };

    const LABEL_W: usize = 15;
    let bar_width = inner_width.saturating_sub(LABEL_W).max(1);
    let mut lines = Vec::new();
    // Axis: session start … end over the bar area.
    let (start, end) = (fmt_local(r0), fmt_local(r1));
    let pad = bar_width.saturating_sub(start.len() + end.len());
    lines.push(Line::from(Span::styled(
        format!("{:LABEL_W$}{start}{}{end}", "", " ".repeat(pad)),
        Style::default().fg(Color::DarkGray),
    )));

    for i in window(app.selected_agent, app.agent_rows.len(), inner_height.saturating_sub(1)) {
        let row = &app.agent_rows[i];
        let label = format!("{:<LABEL_W$}", truncate(&row.agent_type, LABEL_W - 1));
        let base = if i == app.selected_agent {
            Style::default().bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        match lane_cells(row.started, row.finished, (r0, r1), bar_width) {
            Some((offset, len)) => lines.push(Line::from(vec![
                Span::styled(label, base),
                Span::styled(" ".repeat(offset), base),
                Span::styled("█".repeat(len), base.fg(status_color(row.status))),
                Span::styled(" ".repeat(bar_width.saturating_sub(offset + len)), base),
            ])),
            None => lines.push(Line::from(Span::styled(
                format!("{label}(not started)"),
                base.fg(Color::DarkGray),
            ))),
        }
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Detail ")
        .border_style(Style::default().fg(Color::DarkGray));
    if app.focus == Focus::Graph {
        draw_agent_detail(frame, app, area, block);
        return;
    }
    let Some(session) = &app.loaded else {
        frame.render_widget(block, area);
        return;
    };
    let Some(event) = session.timeline.get(app.selected_event) else {
        frame.render_widget(block, area);
        return;
    };

    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    let agent = if event.agent_path.is_empty() {
        "main".to_string()
    } else {
        event.agent_path.join(" → ")
    };
    lines.push(Line::from(Span::styled(format!("agent: {agent}"), dim)));

    match &event.kind {
        EventKind::UserPrompt { text } | EventKind::AssistantText { text } => {
            for l in text.lines() {
                lines.push(Line::raw(l.to_string()));
            }
        }
        EventKind::SystemNote { text } | EventKind::Thinking { text } => {
            for l in text.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), dim)));
            }
        }
        EventKind::ToolCall { name, status, detail, .. } => {
            lines.push(Line::from(vec![
                Span::styled(format!("{name} "), head),
                Span::styled(
                    format!("[{}]", status_glyph(*status)),
                    Style::default().fg(status_color(*status)),
                ),
            ]));
            lines.push(Line::from(Span::styled("── input ──", dim)));
            for l in detail.input.lines() {
                lines.push(Line::raw(l.to_string()));
            }
            lines.push(Line::from(Span::styled("── output ──", dim)));
            let failed = *status == ToolStatus::Error || *status == ToolStatus::Denied;
            for l in detail.output.lines() {
                let style = if failed {
                    Style::default().fg(Color::Red)
                } else {
                    super::diff_line_style(l).unwrap_or_default()
                };
                lines.push(Line::from(Span::styled(l.to_string(), style)));
            }
        }
        EventKind::SubagentSpawn { agent_id, agent_type, description, status, prompt, .. } => {
            lines.push(Line::from(vec![
                Span::styled(format!("Subagent {agent_type} "), head),
                Span::styled(
                    format!("[{}]", status_glyph(*status)),
                    Style::default().fg(status_color(*status)),
                ),
            ]));
            lines.push(Line::raw(format!("description: {description}")));
            if let Some(id) = agent_id {
                lines.push(Line::raw(format!("agent id: {id}  (enter = jump to its events)")));
            }
            lines.push(Line::from(Span::styled("── prompt ──", dim)));
            for l in prompt.lines() {
                lines.push(Line::raw(l.to_string()));
            }
        }
    }

    // Cap what we hand to the Paragraph: only ~area worth of wrapped lines matters.
    let max_lines = (area.height as usize).saturating_mul(4).max(200);
    lines.truncate(max_lines);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), area);
}

/// Detail pane with the Agents pane focused: the selected agent itself —
/// model, lifetime, prompt, and its lazily loaded final report.
fn draw_agent_detail(frame: &mut Frame, app: &App, area: Rect, block: Block) {
    let Some(row) = app.agent_rows.get(app.selected_agent) else {
        frame.render_widget(block, area);
        return;
    };
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();

    let mut title = vec![
        Span::styled(format!("Agent {} ", row.agent_type), head),
        Span::styled(
            format!("[{}]", status_glyph(row.status)),
            Style::default().fg(status_color(row.status)),
        ),
    ];
    if row.is_async {
        title.push(Span::styled("  async", dim));
    }
    lines.push(Line::from(title));
    if let Some(model) = &row.resolved_model {
        lines.push(Line::raw(format!("model: {model}")));
    }
    match (row.started, row.finished) {
        (Some(s), Some(f)) => lines.push(Line::raw(format!(
            "time: {} → {}{}",
            fmt_local(s),
            fmt_local(f),
            row.duration.map(|d| format!(" ({})", format_duration(d))).unwrap_or_default()
        ))),
        (Some(s), None) => lines.push(Line::raw(format!("time: {} → … (running)", fmt_local(s)))),
        _ => {}
    }
    if !row.description.is_empty() {
        lines.push(Line::raw(format!("description: {}", row.description)));
    }

    if let Some(agent_id) = &row.agent_id {
        lines.push(Line::from(Span::styled(format!("id: {agent_id}"), dim)));
        if !row.prompt.is_empty() {
            lines.push(Line::from(Span::styled("── prompt ──", dim)));
            for l in row.prompt.lines() {
                lines.push(Line::raw(l.to_string()));
            }
        }
        match app.agent_report_cache.get(agent_id) {
            Some(report) if !report.is_empty() => {
                lines.push(Line::from(Span::styled("── report ──", dim)));
                for l in report.lines() {
                    lines.push(Line::raw(l.to_string()));
                }
            }
            _ => lines.push(Line::from(Span::styled("── report ── (none yet)", dim))),
        }
    } else {
        lines.push(Line::from(Span::styled("(main agent)", dim)));
    }

    let max_lines = (area.height as usize).saturating_mul(4).max(200);
    lines.truncate(max_lines);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::lane_cells;
    use jiff::Timestamp;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn lane_cells_maps_and_clamps() {
        let range = (ts("2026-01-01T10:00:00Z"), ts("2026-01-01T11:00:00Z"));
        // Full-range agent covers the whole width.
        assert_eq!(lane_cells(Some(range.0), Some(range.1), range, 60), Some((0, 60)));
        // Second half of the range starts at the middle.
        assert_eq!(
            lane_cells(Some(ts("2026-01-01T10:30:00Z")), Some(range.1), range, 60),
            Some((30, 30))
        );
        // Instant agent still renders one cell.
        let t = ts("2026-01-01T10:30:00Z");
        assert_eq!(lane_cells(Some(t), Some(t), range, 60), Some((30, 1)));
        // Never-started agent has no bar.
        assert_eq!(lane_cells(None, None, range, 60), None);
        // Running agent (no finish) extends to the range end.
        assert_eq!(lane_cells(Some(t), None, range, 60), Some((30, 30)));
    }

    #[test]
    fn lane_cells_survives_degenerate_ranges() {
        let t = ts("2026-01-01T10:00:00Z");
        // Zero-duration range must not divide by zero.
        assert_eq!(lane_cells(Some(t), Some(t), (t, t), 60), Some((0, 1)));
        assert_eq!(lane_cells(Some(t), Some(t), (t, t), 0), None);
    }
}
