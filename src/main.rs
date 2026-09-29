mod agent_tree;
mod app;
mod index;
mod model;
mod parser;
mod session;
mod ui;
mod watch;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crossterm::event::{Event, KeyEventKind};

use crate::app::App;
use crate::index::ProjectIndex;
use crate::session::LoadedSession;
use crate::watch::AppEvent;

fn projects_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let root = PathBuf::from(home).join(".claude").join("projects");
    if !root.is_dir() {
        bail!("{} does not exist", root.display());
    }
    Ok(root)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--list") => cmd_list(),
        Some("--dump") => {
            let id = args.get(1).context("usage: claude-agent-tree --dump <sessionIdPrefix>")?;
            cmd_dump(id)
        }
        Some(other) => bail!("unknown argument: {other} (try --list or --dump <sessionId>)"),
        None => run_tui(),
    }
}

fn run_tui() -> Result<()> {
    let root = projects_root()?;
    let index = ProjectIndex::scan(&root)?;

    let (tx, rx) = mpsc::channel();
    watch::spawn_input(tx.clone());
    // Keep the debouncer alive for the lifetime of the loop; a failed watcher
    // degrades to manual refresh via `r`.
    let watcher = watch::spawn_watcher(root, tx);
    let mut app = App::new(index, watcher.is_ok());
    if let Err(e) = &watcher {
        app.status_msg = Some(format!("watcher unavailable ({e}) — press r to refresh"));
    }

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, rx);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: mpsc::Receiver<AppEvent>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                handle_event(app, event);
                // Coalesce bursts into one redraw.
                while let Ok(event) = rx.try_recv() {
                    handle_event(app, event);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {} // tick: refresh relative times
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
            app.handle_key(key);
        }
        AppEvent::Input(_) => {} // resize triggers the redraw anyway
        AppEvent::Fs(paths) => {
            if paths.iter().any(|p| app.path_touches_loaded(p)) {
                app.refresh_loaded();
            }
            // Keep the list views fresh; a full rescan is ~30 ms for 78 sessions.
            app.rescan_index();
        }
    }
}

fn cmd_list() -> Result<()> {
    let index = ProjectIndex::scan(&projects_root()?)?;
    for project in &index.projects {
        println!("{} ({} sessions)", project.display_path, project.sessions.len());
        for s in &project.sessions {
            let cost = s
                .cost
                .as_ref()
                .map(|c| format!("${:.2}", c.total_cost_usd))
                .unwrap_or_else(|| "-".into());
            println!(
                "  {}  {:>9}  {:>7}  ⚡{}  [{:?}] {}",
                &s.id[..8.min(s.id.len())],
                humansize::format_size(s.size, humansize::DECIMAL),
                cost,
                s.subagent_count,
                s.title_source,
                s.title
            );
        }
    }
    Ok(())
}

fn cmd_dump(id_prefix: &str) -> Result<()> {
    let index = ProjectIndex::scan(&projects_root()?)?;
    let meta = index
        .projects
        .iter()
        .flat_map(|p| &p.sessions)
        .find(|s| s.id.starts_with(id_prefix))
        .with_context(|| format!("no session starting with {id_prefix}"))?
        .clone();
    let session = LoadedSession::load(meta)?;

    println!(
        "# {} ({} events, {} parse errors)",
        session.meta.title,
        session.timeline.len(),
        session.parse_errors
    );
    if let Some(cost) = &session.cost {
        println!(
            "cost ${:.2} · +{}/-{} lines · {}s",
            cost.total_cost_usd,
            cost.total_lines_added,
            cost.total_lines_removed,
            cost.total_duration / 1000
        );
    }

    println!("\n## Agent tree");
    for row in agent_tree::flatten(&session.agent_tree) {
        let duration = row
            .duration
            .map(|d| format!(" ({}m{}s)", d.as_secs() / 60, d.as_secs() % 60))
            .unwrap_or_default();
        println!(
            "{}{} {}{} {}",
            row.prefix,
            session::status_glyph(row.status),
            row.agent_type,
            duration,
            row.description
        );
    }

    println!("\n## Timeline");
    for event in &session.timeline {
        let ts = event
            .timestamp
            .map(|t| t.strftime("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        let indent = "  ".repeat(event.agent_path.len());
        println!("{ts} {indent}{}", session::event_label(&event.kind));
    }
    Ok(())
}
