//! Renders an embedded PTY's vt100 screen into the frame, cell by cell with
//! run-length merged styles, plus the child's cursor.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;

fn vt_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = vt_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = vt_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(pty) = app.term_session.as_deref().and_then(|id| app.ptys.get(id)) else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(no attached session)",
                Style::default().fg(Color::DarkGray),
            )),
            area,
        );
        return;
    };
    let parser = pty.parser();
    let screen = parser.screen();
    let (rows, cols) = screen.size();

    let mut lines: Vec<Line> = Vec::with_capacity(rows as usize);
    for row in 0..rows.min(area.height) {
        // Merge runs of identically styled cells into one Span.
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_style = Style::default();
        for col in 0..cols.min(area.width) {
            let (text, style) = match screen.cell(row, col) {
                Some(cell) if !cell.contents().is_empty() => {
                    (cell.contents().to_string(), cell_style(cell))
                }
                Some(cell) => (" ".to_string(), cell_style(cell)),
                None => (" ".to_string(), Style::default()),
            };
            if style != run_style && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
            run.push_str(&text);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);

    if !screen.hide_cursor() {
        let (cur_row, cur_col) = screen.cursor_position();
        if cur_row < area.height && cur_col < area.width {
            frame.set_cursor_position((area.x + cur_col, area.y + cur_row));
        }
    }
}
