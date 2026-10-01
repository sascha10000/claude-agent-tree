//! Browse view: projects sidebar (left) + session list and live agent graph
//! of the selected project (right).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{Activity, App, Focus, ProjectStatus, ACTIVITY_WINDOW};
use crate::live::{AgentState, LiveAgent};

use super::{format_duration_ms, relative_time, truncate, window};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        *Layout::horizontal([Constraint::Length(36), Constraint::Min(30)]).split(area)
    else {
        return;
    };
    draw_projects(frame, app, left);
    let live = live_lines(app, right.width.saturating_sub(2) as usize);
    // The graph grows with its content but never takes more than ~half.
    let live_height = (live.len() as u16 + 2).clamp(3, (right.height / 2).max(3));
    let [sessions, graph] =
        *Layout::vertical([Constraint::Min(5), Constraint::Length(live_height)]).split(right)
    else {
        return;
    };
    draw_sessions(frame, app, sessions);
    draw_live(frame, app, graph, live);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let mut title = if app.project_filter_input || !app.project_filter.is_empty() {
        format!(
            " Projects /{}{} ",
            app.project_filter,
            if app.project_filter_input { "▌" } else { "" }
        )
    } else {
        " Projects ".to_string()
    };
    if app.only_active {
        title.push_str("· active ");
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(app.focus == Focus::Projects));
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let visible = app.visible_projects();
    let selected_pos = visible.iter().position(|&i| i == app.selected_project).unwrap_or(0);
    let range = window(selected_pos, visible.len(), inner_height);

    let mut lines = Vec::new();
    if visible.is_empty() && app.only_active {
        lines.push(Line::from(Span::styled(
            "nothing running · A shows all",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for &i in &visible[range] {
        let project = &app.index.projects[i];
        let count = format!(" ({})", project.sessions.len());
        let (glyph, glyph_color) = project_glyph(app.project_status(project));
        let name =
            truncate(project.name(), inner_width.saturating_sub(count.chars().count() + 2));
        let mut style = Style::default();
        if project.sessions.is_empty() {
            style = style.fg(Color::DarkGray);
        }
        if i == app.selected_project {
            style = style.bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(glyph, style.fg(glyph_color)),
            Span::styled(format!("{name}{count}"), style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_sessions(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.filter_input || !app.filter.is_empty() {
        format!(" Sessions /{}{} ", app.filter, if app.filter_input { "▌" } else { "" })
    } else if app.sort != crate::app::SessionSort::Mtime {
        format!(" Sessions ↓{} ", app.sort.label())
    } else {
        " Sessions ".to_string()
    };
    // The project list shows names only; the full path lives up here.
    let home = std::env::var("HOME").unwrap_or_default();
    let path = app
        .index
        .projects
        .get(app.selected_project)
        .map(|p| format!(" {} ", p.display_path.replacen(&home, "~", 1)))
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_top(Line::from(Span::styled(path, Style::default().fg(Color::DarkGray))).right_aligned())
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
        let duration = session
            .cost
            .as_ref()
            .map(|c| format_duration_ms(c.total_duration))
            .unwrap_or_else(|| "-".into());
        let right = format!(
            " {:>9} {:>8} {:>6} {:>9} ⚡{}",
            relative_time(session.mtime),
            cost,
            duration,
            humansize::format_size(session.size, humansize::DECIMAL),
            session.subagent_count
        );
        // Liveness glyph: ● working, ⑂ subagents working, ▶ waiting for input.
        let (glyph, glyph_color) = match app.activity(session) {
            crate::app::Activity::Working => ("● ", Color::Yellow),
            crate::app::Activity::SubagentsWorking => ("⑂ ", Color::Yellow),
            crate::app::Activity::AwaitingInput => ("▶ ", Color::Green),
            crate::app::Activity::Idle => ("  ", Color::Reset),
        };
        let title_width = inner_width.saturating_sub(right.chars().count() + 3);
        let selected = pos == app.selected_session;
        let base = if selected {
            Style::default().bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(glyph, base.fg(glyph_color)),
            Span::styled(
                format!("{:<w$}", truncate(&session.title, title_width), w = title_width),
                base,
            ),
            Span::styled(right, base.fg(Color::Gray)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Project-list marker: ● running · ▶ awaits input · ○ touched within the
/// hour · · older.
fn project_glyph(status: ProjectStatus) -> (&'static str, Color) {
    match status {
        ProjectStatus::Active => ("● ", Color::Yellow),
        ProjectStatus::AwaitingInput => ("▶ ", Color::Green),
        ProjectStatus::Recent => ("○ ", Color::Cyan),
        ProjectStatus::Old => ("· ", Color::DarkGray),
        ProjectStatus::Empty => ("  ", Color::DarkGray),
    }
}

/// Finished agents shown per level before collapsing into "+N earlier".
const MAX_DONE_SHOWN: usize = 3;

fn draw_live(frame: &mut Frame, app: &App, area: Rect, lines: Vec<Line<'static>>) {
    let mut running = 0;
    let mut waiting = 0;
    fn count(agents: &[LiveAgent], running: &mut usize, waiting: &mut usize) {
        for a in agents {
            match a.state(ACTIVITY_WINDOW) {
                AgentState::Running => *running += 1,
                AgentState::Waiting => *waiting += 1,
                _ => {}
            }
            count(&a.children, running, waiting);
        }
    }
    for session in &app.live {
        count(&session.agents, &mut running, &mut waiting);
    }
    let mut title = format!(" Live agents · {} session(s) ", app.live.len());
    if running + waiting > 0 {
        title = format!(" Live agents · {running} running · {waiting} waiting ");
    }
    let busy = running > 0
        || app.live.iter().any(|ls| session_meta(app, &ls.session_id).is_some_and(|m| {
            matches!(app.activity(m), Activity::Working | Activity::SubagentsWorking)
        }));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if busy { Color::Yellow } else { Color::DarkGray }));
    // Keep the newest (bottom) rows when the graph overflows.
    let inner_height = area.height.saturating_sub(2) as usize;
    let skip = lines.len().saturating_sub(inner_height);
    let lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn session_meta<'a>(app: &'a App, id: &str) -> Option<&'a crate::index::SessionMeta> {
    app.index.projects.get(app.selected_project)?.sessions.iter().find(|s| s.id == id)
}

/// Rows of the live graph: each recent session as a root, its subagents as a
/// box-drawn tree below it.
fn live_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let Some(project) = app.index.projects.get(app.selected_project) else {
        return Vec::new();
    };
    if app.live.is_empty() {
        let last = project
            .sessions
            .iter()
            .map(|s| s.mtime)
            .max()
            .map(|t| format!(" · last activity {}", relative_time(t)))
            .unwrap_or_default();
        return vec![Line::from(Span::styled(format!("no live sessions{last}"), dim))];
    }
    let mut lines = Vec::new();
    for live in &app.live {
        let Some(meta) = session_meta(app, &live.session_id) else { continue };
        let (glyph, color, label) = match app.activity(meta) {
            Activity::Working => ("● ", Color::Yellow, "working"),
            Activity::SubagentsWorking => ("⑂ ", Color::Yellow, "subagents working"),
            Activity::AwaitingInput => ("▶ ", Color::Green, "awaits input"),
            Activity::Idle => ("○ ", Color::DarkGray, "idle"),
        };
        let touched = meta.subagent_mtime.map_or(meta.mtime, |s| s.max(meta.mtime));
        lines.push(two_columns(
            vec![
                Span::styled(glyph, Style::default().fg(color)),
                Span::styled(meta.title.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ],
            Span::styled(format!("{label} · {}", relative_time(touched)), Style::default().fg(color)),
            width,
        ));
        push_agents(&live.agents, "", width, &mut lines);
    }
    lines
}

fn push_agents(agents: &[LiveAgent], indent: &str, width: usize, lines: &mut Vec<Line<'static>>) {
    // Active agents always show; of the finished ones only the newest few.
    let inactive: Vec<usize> =
        (0..agents.len()).filter(|&i| !agents[i].is_active(ACTIVITY_WINDOW)).collect();
    let hidden = inactive.len().saturating_sub(MAX_DONE_SHOWN);
    let shown: Vec<&LiveAgent> = agents
        .iter()
        .enumerate()
        .filter(|(i, _)| !inactive[..hidden].contains(i))
        .map(|(_, a)| a)
        .collect();
    let dim = Style::default().fg(Color::DarkGray);
    if hidden > 0 {
        let branch = if shown.is_empty() { "└─" } else { "├─" };
        lines.push(Line::from(Span::styled(format!("{indent}{branch}… +{hidden} earlier finished"), dim)));
    }
    for (i, agent) in shown.iter().enumerate() {
        let last = i == shown.len() - 1;
        let branch = if last { "└─" } else { "├─" };
        let (glyph, color, label) = match agent.state(ACTIVITY_WINDOW) {
            AgentState::Running => ("● ", Color::Yellow, "running"),
            AgentState::Waiting => ("◌ ", Color::Cyan, "in tool"),
            AgentState::Done => ("✓ ", Color::Green, "done"),
            AgentState::Stopped => ("⊘ ", Color::Red, "stopped"),
        };
        let text = if matches!(agent.state(ACTIVITY_WINDOW), AgentState::Done | AgentState::Stopped) {
            dim
        } else {
            Style::default()
        };
        lines.push(two_columns(
            vec![
                Span::styled(format!("{indent}{branch}"), dim),
                Span::styled(glyph, Style::default().fg(color)),
                Span::styled(format!("{} ", agent.agent_type), text.fg(Color::Magenta)),
                Span::styled(agent.description.clone(), text),
            ],
            Span::styled(format!("{label} · {}", relative_time(agent.mtime)), text.fg(color)),
            width,
        ));
        let child_indent = format!("{indent}{}", if last { "  " } else { "│ " });
        push_agents(&agent.children, &child_indent, width, lines);
    }
}

/// `left … right` on one row of `width` columns; the last left span is
/// truncated so the right column always fits.
fn two_columns(mut left: Vec<Span<'static>>, right: Span<'static>, width: usize) -> Line<'static> {
    let right_w = right.content.chars().count() + 1;
    let fixed: usize = left[..left.len() - 1].iter().map(|s| s.content.chars().count()).sum();
    let budget = width.saturating_sub(right_w + fixed);
    if let Some(last) = left.last_mut() {
        let text = truncate(&last.content, budget);
        let pad = budget.saturating_sub(text.chars().count());
        last.content = format!("{text}{}", " ".repeat(pad)).into();
    }
    left.push(Span::raw(" "));
    left.push(right);
    Line::from(left)
}
