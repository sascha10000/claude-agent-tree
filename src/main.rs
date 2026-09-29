mod agent_tree;
mod analytics;
mod app;
mod export;
mod index;
mod model;
mod parser;
mod session;
mod term;
mod ui;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crossterm::event::{Event, KeyEventKind};

use crate::app::App;
use crate::index::ProjectIndex;
use crate::session::LoadedSession;
use crate::watch::AppEvent;

fn projects_root(override_root: Option<PathBuf>) -> Result<PathBuf> {
    let root = match override_root {
        Some(root) => root,
        None => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".claude").join("projects")
        }
    };
    if !root.is_dir() {
        bail!("{} does not exist", root.display());
    }
    Ok(root)
}

/// Extract `--root <path>` from the args, removing both tokens.
fn parse_root(args: &mut Vec<String>) -> Result<Option<PathBuf>> {
    let Some(pos) = args.iter().position(|a| a == "--root") else { return Ok(None) };
    args.remove(pos);
    if pos >= args.len() {
        bail!("--root requires a path");
    }
    Ok(Some(PathBuf::from(args.remove(pos))))
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let root = projects_root(parse_root(&mut args)?)?;
    match args.first().map(String::as_str) {
        Some("--list") => cmd_list(&root),
        Some("--dump") => {
            let id = args.get(1).context("usage: claude-agent-tree --dump <sessionIdPrefix>")?;
            cmd_dump(&root, id)
        }
        Some("--export") => {
            let id = args.get(1).context("usage: claude-agent-tree --export <sessionIdPrefix>")?;
            cmd_export(&root, id)
        }
        Some(other) => bail!(
            "unknown argument: {other} (try --list, --dump <id>, --export <id>, --root <path>)"
        ),
        None => run_tui(root),
    }
}

fn run_tui(root: PathBuf) -> Result<()> {
    let index = ProjectIndex::scan(&root)?;

    let (tx, rx) = mpsc::channel();
    // The park flag is unused since resume became an embedded PTY pane.
    let _suspend = watch::spawn_input(tx.clone());
    let app_tx = tx.clone();
    // Keep the debouncer alive for the lifetime of the loop; a failed watcher
    // degrades to manual refresh via `r`.
    let watcher = watch::spawn_watcher(root, tx);
    let mut app = App::new(index, watcher.is_ok());
    app.tx = Some(app_tx);
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
        AppEvent::Input(Event::Resize(cols, rows)) => {
            // Embedded terminals track the pane size (frame minus statusbar).
            app.resize_ptys(cols, rows.saturating_sub(1));
        }
        AppEvent::Input(_) => {} // other events: the redraw is enough
        AppEvent::Fs(paths) => {
            if paths.iter().any(|p| app.path_touches_loaded(p)) {
                app.refresh_loaded();
            }
            // Keep the list views fresh; a full rescan is ~30 ms for 78 sessions.
            app.rescan_index();
        }
        AppEvent::Loaded { id, result } => app.on_loaded(id, *result),
        AppEvent::Pty => {} // screen state lives in the parser; just redraw
        AppEvent::PtyExited { id } => app.on_pty_exited(&id),
    }
}

fn cmd_list(root: &Path) -> Result<()> {
    let index = ProjectIndex::scan(root)?;
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

fn find_session(root: &Path, id_prefix: &str) -> Result<LoadedSession> {
    let index = ProjectIndex::scan(root)?;
    let meta = index
        .projects
        .iter()
        .flat_map(|p| &p.sessions)
        .find(|s| s.id.starts_with(id_prefix))
        .with_context(|| format!("no session starting with {id_prefix}"))?
        .clone();
    Ok(LoadedSession::load(meta)?)
}

fn cmd_export(root: &Path, id_prefix: &str) -> Result<()> {
    let session = find_session(root, id_prefix)?;
    let path = PathBuf::from(format!("{}.md", session.meta.id));
    std::fs::write(&path, export::export_markdown(&session))?;
    println!("exported to {}", path.display());
    Ok(())
}

fn cmd_dump(root: &Path, id_prefix: &str) -> Result<()> {
    let session = find_session(root, id_prefix)?;

    println!(
        "# {} ({} events, {} parse errors)",
        session.meta.title,
        session.timeline.len(),
        session.parse_errors
    );
    if let Some(cost) = &session.cost {
        println!(
            "cost ${:.2} · +{}/-{} lines · wall {}s · api {}s · tools {}s",
            cost.total_cost_usd,
            cost.total_lines_added,
            cost.total_lines_removed,
            cost.total_duration / 1000,
            cost.total_api_duration / 1000,
            cost.total_tool_duration / 1000
        );
        let mut models: Vec<_> = cost.model_usage.iter().collect();
        models.sort_by(|a, b| b.1.cost_usd.total_cmp(&a.1.cost_usd));
        for (model, mu) in models {
            println!(
                "  {model}: in {} · out {} · cache-r {} · cache-w {} · ${:.2}",
                mu.input_tokens,
                mu.output_tokens,
                mu.cache_read_input_tokens,
                mu.cache_creation_input_tokens,
                mu.cost_usd
            );
        }
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
