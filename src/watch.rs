//! Input thread + debounced filesystem watcher feeding one AppEvent channel.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult, Debouncer};

pub enum AppEvent {
    Input(crossterm::event::Event),
    Fs(Vec<PathBuf>),
}

/// Blocking crossterm reader on its own thread.
pub fn spawn_input(tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    });
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
