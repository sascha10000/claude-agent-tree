//! Input thread + debounced filesystem watcher feeding one AppEvent channel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult, Debouncer};

pub enum AppEvent {
    Input(crossterm::event::Event),
    Fs(Vec<PathBuf>),
    /// A background session load finished (boxed: `LoadedSession` is large).
    Loaded { id: String, result: Box<std::io::Result<crate::session::LoadedSession>> },
    /// An embedded PTY produced output (redraw trigger; screen state lives in
    /// the parser).
    Pty,
    /// An embedded PTY's child exited.
    PtyExited { id: String },
}

/// Crossterm reader on its own thread. Polls instead of blocking so it can be
/// parked via the returned flag while a resumed `claude` child owns the tty —
/// a blocked `event::read()` would steal the child's keystrokes.
pub fn spawn_input(tx: Sender<AppEvent>) -> Arc<AtomicBool> {
    let suspended = Arc::new(AtomicBool::new(false));
    let flag = suspended.clone();
    std::thread::spawn(move || loop {
        if flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        match crossterm::event::poll(Duration::from_millis(100)) {
            Ok(true) => {
                let Ok(event) = crossterm::event::read() else { break };
                if tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    });
    suspended
}

/// Recursive debounced watcher on the projects root. The returned debouncer
/// must be kept alive for the watch to stay active.
pub fn spawn_watcher(
    root: PathBuf,
    tx: Sender<AppEvent>,
) -> notify::Result<Debouncer<notify::RecommendedWatcher>> {
    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        move |result: DebounceEventResult| {
            if let Ok(events) = result {
                let paths: Vec<PathBuf> = events.into_iter().map(|e| e.path).collect();
                if !paths.is_empty() {
                    let _ = tx.send(AppEvent::Fs(paths));
                }
            }
        },
    )?;
    debouncer.watcher().watch(&root, RecursiveMode::Recursive)?;
    Ok(debouncer)
}
