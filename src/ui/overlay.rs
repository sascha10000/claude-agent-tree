//! Modal overlays: per-model cost breakdown and fullscreen event detail.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Overlay};

use super::format_duration_ms;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Cost => draw_cost(frame, app, centered(area, 76, 70)),
        Overlay::EventDetail { content, scroll } => draw_event_detail(frame, content, *scroll, area),
        Overlay::Help => draw_help(frame, app, centered(area, 60, 80)),
        Overlay::Analytics { scroll } => {
            draw_analytics(frame, app, *scroll, centered(area, 80, 80))
        }
        Overlay::Fleet { selected } => draw_fleet(frame, app, *selected, centered(area, 84, 60)),
    }
}

fn draw_fleet(frame: &mut Frame, app: &App, selected: usize, area: Rect) {
    let entries = crate::analytics::fleet(&app.index, crate::app::FLEET_WINDOW);
    let dim = Style::default().fg(Color::DarkGray);
    let home = std::env::var("HOME").unwrap_or_default();
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    if entries.is_empty() {
        lines.push(Line::from(Span::styled("(no sessions active in the last 5m)", dim)));
    }
    for i in super::window(selected, entries.len(), inner_height) {
        let e = &entries[i];
        let live = e.mtime.elapsed().map(|el| el.as_secs() < 60).unwrap_or(true);
        let cost = e.cost.map(|c| format!("${c:.2}")).unwrap_or_else(|| "-".into());
        let text = format!(
            "{} {:>8}  {:>7}  ⚡{}  {}  · {}",
            if live { "●" } else { "○" },
            super::relative_time(e.mtime),
            cost,
            e.subagents,
            super::truncate(&e.title, 40),
            super::truncate(&e.display_path.replace(&home, "~"), 30),
        );
        let mut style = if live { Style::default().fg(Color::Green) } else { Style::default() };
        if i == selected {
            style = style.bg(Color::Rgb(50, 50, 70)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(super::truncate(&text, inner_width), style)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Fleet · active last 5m ")
        .title_bottom(" j/k move · enter jump · esc close ")
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_analytics(frame: &mut Frame, app: &App, scroll: usize, area: Rect) {
    let a = crate::analytics::aggregate(&app.index);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let home = std::env::var("HOME").unwrap_or_default();
    let tail = |p: &str, max: usize| super::truncate(&p.replace(&home, "~"), max);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("total ${:.2}", a.total_cost), head),
        Span::styled(
            format!("   {} of {} sessions costed", a.costed_sessions, a.total_sessions),
            dim,
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "cache-read {} · cache-write {} tokens",
            fmt_tokens(a.cache_read_total),
            fmt_tokens(a.cache_creation_total)
        ),
        dim,
    )));

    lines.push(Line::from(Span::styled("── spend by project ──", dim)));
    for (path, cost, n) in a.per_project.iter().take(10) {
        lines.push(Line::raw(format!("{:>9}  {:>3}  {}", format!("${cost:.2}"), n, tail(path, 52))));
    }

    lines.push(Line::from(Span::styled("── top sessions ──", dim)));
    for (title, path, cost) in &a.top_sessions {
        lines.push(Line::raw(format!(
            "{:>9}  {}  · {}",
            format!("${cost:.2}"),
            super::truncate(title, 38),
            tail(path, 22)
        )));
    }

    lines.push(Line::from(Span::styled("── by model ──", dim)));
    lines.push(Line::from(Span::styled(
        format!(
            "{:<34} {:>7} {:>7} {:>8} {:>8} {:>8}",
            "model", "in", "out", "cache-r", "cache-w", "cost"
        ),
        dim,
    )));
    for (model, m) in &a.per_model {
        lines.push(Line::raw(format!(
            "{:<34} {:>7} {:>7} {:>8} {:>8} {:>8}",
            super::truncate(model, 34),
            fmt_tokens(m.input),
            fmt_tokens(m.output),
            fmt_tokens(m.cache_read),
            fmt_tokens(m.cache_creation),
            format!("${:.2}", m.cost),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Analytics · all projects ")
        .title_bottom(" j/k scroll · esc close ")
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll as u16, 0)).block(block),
        area,
    );
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match app.view {
        // Unreachable (no overlays in the terminal view), but keep it total.
        crate::app::View::Terminal => &[("ctrl-q", "detach; the session keeps running")],
        crate::app::View::Browse => &[
            ("j/k ↓/↑", "move selection"),
            ("g / G", "first / last"),
            ("enter / l", "open project or session"),
            ("tab", "switch pane"),
            ("/", "filter sessions"),
            ("s", "cycle sort (mtime/cost/size/duration)"),
            ("a", "analytics across all projects"),
            ("f", "fleet: recently active sessions"),
            ("R", "resume session via claude --resume"),
            ("r", "rescan"),
            ("?", "this help"),
            ("q", "quit"),
        ],
        crate::app::View::Detail => &[
            ("j/k ↓/↑", "move selection"),
            ("d/u PgDn/PgUp", "fast scroll"),
            ("g / G", "first / last"),
            ("tab", "timeline ↔ agents pane"),
            ("enter", "spawn: jump to agent · else fullscreen"),
            ("o", "fullscreen event view (uncapped)"),
            ("c", "cost & token breakdown"),
            ("t", "agents as time lanes"),
            ("x", "export session as Markdown"),
            ("esc / h", "back"),
            ("?", "this help"),
            ("q", "quit"),
        ],
    };
    let dim = Style::default().fg(Color::DarkGray);
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, desc)| {
            Line::from(vec![
                Span::styled(format!(" {k:<15}"), Style::default().fg(Color::Cyan)),
                Span::styled((*desc).to_string(), dim),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keys ")
        .title_bottom(" esc close ")
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Centered sub-rect taking `pct_x`/`pct_y` percent of `area`.
fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let w = area.width * pct_x / 100;
    let h = area.height * pct_y / 100;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 10_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn draw_cost(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Cost & tokens ")
        .title_bottom(" esc close ")
        .border_style(Style::default().fg(Color::Cyan));
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    match app.loaded.as_ref().and_then(|s| s.cost.as_ref()) {
        None => lines.push(Line::from(Span::styled("(no cost data)", dim))),
        Some(cost) => {
            lines.push(Line::from(vec![
                Span::styled(format!("total ${:.2}", cost.total_cost_usd), head),
                Span::styled(
                    format!(
                        "   wall {} · api {} · tools {}",
                        format_duration_ms(cost.total_duration),
                        format_duration_ms(cost.total_api_duration),
                        format_duration_ms(cost.total_tool_duration),
                    ),
                    Style::default(),
                ),
            ]));
            if let Some(start) = cost.start_time
                && let Ok(ts) = jiff::Timestamp::from_millisecond(start) {
                    let local = ts.to_zoned(jiff::tz::TimeZone::system());
                    lines.push(Line::from(Span::styled(
                        format!("started {}", local.strftime("%Y-%m-%d %H:%M:%S")),
                        dim,
                    )));
                }
            lines.push(Line::raw(""));
            if cost.model_usage.is_empty() {
                lines.push(Line::from(Span::styled("(no per-model breakdown)", dim)));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{:<34} {:>7} {:>7} {:>8} {:>8} {:>8}",
                        "model", "in", "out", "cache-r", "cache-w", "cost"
                    ),
                    dim,
                )));
                let mut models: Vec<_> = cost.model_usage.iter().collect();
                models.sort_by(|a, b| b.1.cost_usd.total_cmp(&a.1.cost_usd));
                for (model, mu) in models {
                    lines.push(Line::raw(format!(
                        "{:<34} {:>7} {:>7} {:>8} {:>8} {:>8}",
                        super::truncate(model, 34),
                        fmt_tokens(mu.input_tokens),
                        fmt_tokens(mu.output_tokens),
                        fmt_tokens(mu.cache_read_input_tokens),
                        fmt_tokens(mu.cache_creation_input_tokens),
                        format!("${:.2}", mu.cost_usd),
                    )));
                }
            }
        }
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_event_detail(frame: &mut Frame, content: &str, scroll: usize, area: Rect) {
    let total = content.lines().count();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Event · line {}/{} ", (scroll + 1).min(total.max(1)), total))
        .title_bottom(" j/k/d/u/g/G scroll · esc close ")
        .border_style(Style::default().fg(Color::Cyan));
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = content
        .lines()
        .map(|l| {
            Line::styled(l.to_string(), super::diff_line_style(l).unwrap_or_default())
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll as u16, 0)).block(block),
        area,
    );
}
