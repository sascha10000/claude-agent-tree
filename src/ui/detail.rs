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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Timeline ")
        .border_style(border_style(app.focus == Focus::Timeline));
    let Some(session) = &app.loaded else {
        frame.render_widget(block, area);
        return;
    };
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let range = window(app.selected_event, session.timeline.len(), inner_height);

    let mut lines = Vec::new();
    for i in range {
        let event = &session.timeline[i];
        let ts = event
            .timestamp
            .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        let gutter = "│ ".repeat(event.agent_path.len());
        let label = timeline_label(event);
        let mut style = event_style(&event.kind);
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

fn draw_graph(frame: &mut Frame, app: &App, area: Rect) {
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

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Detail ")
        .border_style(Style::default().fg(Color::DarkGray));
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
        EventKind::SystemNote { text } => {
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
            let output_style = if *status == ToolStatus::Error || *status == ToolStatus::Denied {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            for l in detail.output.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), output_style)));
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
