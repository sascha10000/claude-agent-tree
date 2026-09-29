//! Browse view: projects sidebar (left) + session list (right).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus};

use super::{relative_time, truncate, window};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        *Layout::horizontal([Constraint::Length(36), Constraint::Min(30)]).split(area)
    else {
        return;
    };
    draw_projects(frame, app, left);
    draw_sessions(frame, app, right);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Projects ")
        .border_style(border_style(app.focus == Focus::Projects));
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let range = window(app.selected_project, app.index.projects.len(), inner_height);

    let home = std::env::var("HOME").unwrap_or_default();
    let mut lines = Vec::new();
    for i in range {
        let project = &app.index.projects[i];
        let name = project.display_path.replace(&home, "~");
        // Show the tail of the path — the discriminating part.
        let shown: String = if name.chars().count() > inner_width.saturating_sub(5) {
            let tail: String = name
                .chars()
                .rev()
                .take(inner_width.saturating_sub(6))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("…{tail}")
        } else {
            name
        };
        let text = format!("{shown} ({})", project.sessions.len());
        let mut style = Style::default();
        if project.sessions.is_empty() {
            style = style.fg(Color::DarkGray);
        }
        if i == app.selected_project {
            style = style.bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_sessions(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.filter_input || !app.filter.is_empty() {
        format!(" Sessions /{}{} ", app.filter, if app.filter_input { "▌" } else { "" })
    } else {
        " Sessions ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(app.focus == Focus::Sessions));
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;

    let visible = app.visible_sessions();
    let Some(project) = app.index.projects.get(app.selected_project) else {
        frame.render_widget(Paragraph::new("no project selected").block(block), area);
        return;
    };
    if visible.is_empty() {
        let msg = if project.sessions.is_empty() { "(no sessions)" } else { "(no match)" };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray))).block(block),
            area,
        );
        return;
    }

    let range = window(app.selected_session, visible.len(), inner_height);
    let mut lines = Vec::new();
    for pos in range {
        let session = &project.sessions[visible[pos]];
        let cost = session
            .cost
            .as_ref()
            .map(|c| format!("${:.2}", c.total_cost_usd))
            .unwrap_or_else(|| "-".into());
        let right = format!(
            " {:>9} {:>8} {:>9} ⚡{}",
            relative_time(session.mtime),
            cost,
            humansize::format_size(session.size, humansize::DECIMAL),
            session.subagent_count
        );
        let title_width = inner_width.saturating_sub(right.chars().count() + 1);
        let selected = pos == app.selected_session;
        let base = if selected {
            Style::default().bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<w$}", truncate(&session.title, title_width), w = title_width),
                base,
            ),
            Span::styled(right, base.fg(Color::Gray)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
