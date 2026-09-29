//! Application state machine. Key handling is pure state transition so it can
//! be unit-tested without a terminal.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};

use crate::agent_tree::{flatten, FlatAgentRow};
use crate::index::ProjectIndex;
use crate::session::{EventKind, LoadedSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Browse,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Sessions,
    Timeline,
    Graph,
}

pub struct App {
    pub index: ProjectIndex,
    pub view: View,
    pub focus: Focus,
    pub selected_project: usize,
    pub selected_session: usize,
    pub loaded: Option<LoadedSession>,
    /// Cached flattened agent tree of the loaded session.
    pub agent_rows: Vec<FlatAgentRow>,
    pub selected_event: usize,
    pub selected_agent: usize,
    /// Session-list filter; `filter_input` = the `/` prompt is capturing keys.
    pub filter: String,
    pub filter_input: bool,
    pub watching: bool,
    pub should_quit: bool,
    pub status_msg: Option<String>,
}

impl App {
    pub fn new(index: ProjectIndex, watching: bool) -> Self {
        Self {
            index,
            view: View::Browse,
            focus: Focus::Projects,
            selected_project: 0,
            selected_session: 0,
            loaded: None,
            agent_rows: Vec::new(),
            selected_event: 0,
            selected_agent: 0,
            filter: String::new(),
            filter_input: false,
            watching,
            should_quit: false,
            status_msg: None,
        }
    }

    /// Indices into the selected project's session list that match the filter.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let Some(project) = self.index.projects.get(self.selected_project) else {
            return Vec::new();
        };
        let needle = self.filter.to_lowercase();
        project
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| needle.is_empty() || s.title.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.filter_input {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filter_input = false;
                }
                KeyCode::Enter => self.filter_input = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected_session = 0;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('r') => self.reload(),
            _ => match self.view {
                View::Browse => self.handle_browse_key(key),
                View::Detail => self.handle_detail_key(key),
            },
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) {
        let projects = self.index.projects.len();
        let sessions = self.visible_sessions().len();
        match key.code {
            KeyCode::Char('/') => {
                self.filter_input = true;
                self.focus = Focus::Sessions;
            }
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Projects => {
                    self.selected_project = step(self.selected_project, 1, projects);
                    self.selected_session = 0;
                }
                _ => self.selected_session = step(self.selected_session, 1, sessions),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Projects => {
                    self.selected_project = step(self.selected_project, -1, projects);
                    self.selected_session = 0;
                }
                _ => self.selected_session = step(self.selected_session, -1, sessions),
            },
            KeyCode::Char('g') => self.select_first(),
            KeyCode::Char('G') => match self.focus {
                Focus::Projects => self.selected_project = projects.saturating_sub(1),
                _ => self.selected_session = sessions.saturating_sub(1),
            },
            KeyCode::Enter | KeyCode::Char('l') => match self.focus {
                Focus::Projects => self.focus = Focus::Sessions,
                _ => self.open_selected_session(),
            },
            KeyCode::Esc | KeyCode::Char('h') => {
                if self.focus == Focus::Sessions {
                    if !self.filter.is_empty() {
                        self.filter.clear();
                    } else {
                        self.focus = Focus::Projects;
                    }
                }
            }
            KeyCode::Tab => {
                self.focus =
                    if self.focus == Focus::Projects { Focus::Sessions } else { Focus::Projects };
            }
            _ => {}
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) {
        let events = self.loaded.as_ref().map_or(0, |s| s.timeline.len());
        let agents = self.agent_rows.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Graph => self.selected_agent = step(self.selected_agent, 1, agents),
                _ => self.selected_event = step(self.selected_event, 1, events),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Graph => self.selected_agent = step(self.selected_agent, -1, agents),
                _ => self.selected_event = step(self.selected_event, -1, events),
            },
            KeyCode::PageDown | KeyCode::Char('d') => {
                self.selected_event = step(self.selected_event, 20, events)
            }
            KeyCode::PageUp | KeyCode::Char('u') => {
                self.selected_event = step(self.selected_event, -20, events)
            }
            KeyCode::Char('g') => match self.focus {
                Focus::Graph => self.selected_agent = 0,
                _ => self.selected_event = 0,
            },
            KeyCode::Char('G') => match self.focus {
                Focus::Graph => self.selected_agent = agents.saturating_sub(1),
                _ => self.selected_event = events.saturating_sub(1),
            },
            KeyCode::Tab => {
                self.focus =
                    if self.focus == Focus::Timeline { Focus::Graph } else { Focus::Timeline };
            }
            KeyCode::Enter => self.cross_jump(),
            KeyCode::Esc | KeyCode::Char('h') => {
                self.view = View::Browse;
                self.focus = Focus::Sessions;
                self.loaded = None;
                self.agent_rows.clear();
            }
            _ => {}
        }
    }

    fn select_first(&mut self) {
        match self.focus {
            Focus::Projects => {
                self.selected_project = 0;
                self.selected_session = 0;
            }
            _ => self.selected_session = 0,
        }
    }

    /// Enter in the detail view: spawn event ↔ graph row jump to each other.
    fn cross_jump(&mut self) {
        match self.focus {
            Focus::Timeline => {
                let Some(session) = &self.loaded else { return };
                let Some(event) = session.timeline.get(self.selected_event) else { return };
                if let EventKind::SubagentSpawn { agent_id: Some(id), .. } = &event.kind {
                    if let Some((row_idx, row)) = self
                        .agent_rows
                        .iter()
                        .enumerate()
                        .find(|(_, r)| r.agent_id.as_deref() == Some(id.as_str()))
                    {
                        self.selected_agent = row_idx;
                        if let Some((first, _)) = row.event_range {
                            self.selected_event = first;
                        }
                        self.focus = Focus::Graph;
                    }
                }
            }
            Focus::Graph => {
                if let Some(row) = self.agent_rows.get(self.selected_agent) {
                    if let Some((first, _)) = row.event_range {
                        self.selected_event = first;
                        self.focus = Focus::Timeline;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn open_selected_session(&mut self) {
        let visible = self.visible_sessions();
        let Some(&session_idx) = visible.get(self.selected_session) else { return };
        let Some(project) = self.index.projects.get(self.selected_project) else { return };
        let meta = project.sessions[session_idx].clone();
        match LoadedSession::load(meta) {
            Ok(session) => {
                self.agent_rows = flatten(&session.agent_tree);
                self.selected_event = 0;
                self.selected_agent = 0;
                self.loaded = Some(session);
                self.view = View::Detail;
                self.focus = Focus::Timeline;
                self.status_msg = None;
            }
            Err(e) => self.status_msg = Some(format!("load failed: {e}")),
        }
    }

    /// Manual refresh (`r`): rescan the index, reload the open session.
    pub fn reload(&mut self) {
        self.rescan_index();
        self.refresh_loaded();
    }

    /// Re-scan the whole index, keeping the current selection by identity.
    pub fn rescan_index(&mut self) {
        let selected_dir = self.index.projects.get(self.selected_project).map(|p| p.dir.clone());
        let selected_id = self
            .visible_sessions()
            .get(self.selected_session)
            .and_then(|&i| self.index.projects.get(self.selected_project)?.sessions.get(i))
            .map(|s| s.id.clone());
        match ProjectIndex::scan(&self.index.root) {
            Ok(index) => self.index = index,
            Err(e) => {
                self.status_msg = Some(format!("rescan failed: {e}"));
                return;
            }
        }
        if let Some(dir) = selected_dir {
            self.selected_project =
                self.index.projects.iter().position(|p| p.dir == dir).unwrap_or(0);
        }
        self.selected_project =
            self.selected_project.min(self.index.projects.len().saturating_sub(1));
        if let Some(id) = selected_id {
            let visible = self.visible_sessions();
            if let Some(pos) = visible
                .iter()
                .position(|&i| self.index.projects[self.selected_project].sessions[i].id == id)
            {
                self.selected_session = pos;
            }
        }
        self.selected_session =
            self.selected_session.min(self.visible_sessions().len().saturating_sub(1));
    }

    /// Incrementally reload the open session (called on fs events and `r`).
    pub fn refresh_loaded(&mut self) {
        let Some(session) = &mut self.loaded else { return };
        let was_at_end = self.selected_event + 1 >= session.timeline.len();
        if let Err(e) = session.refresh() {
            self.status_msg = Some(format!("refresh failed: {e}"));
            return;
        }
        self.agent_rows = flatten(&session.agent_tree);
        if was_at_end && !session.timeline.is_empty() {
            // Tail-follow: stay glued to the newest event.
            self.selected_event = session.timeline.len() - 1;
        }
        self.selected_event = self.selected_event.min(session.timeline.len().saturating_sub(1));
        self.selected_agent = self.selected_agent.min(self.agent_rows.len().saturating_sub(1));
    }

    /// Does a changed path belong to the currently loaded session?
    pub fn path_touches_loaded(&self, path: &Path) -> bool {
        let Some(session) = &self.loaded else { return false };
        if session.transcripts.iter().any(|t| t.path == path) {
            return true;
        }
        // New subagent transcript/meta under <projectDir>/<sessionId>/.
        let session_dir: Option<PathBuf> =
            session.meta.path.parent().map(|p| p.join(&session.meta.id));
        session_dir.is_some_and(|dir| path.starts_with(dir))
    }
}

fn step(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, len as isize - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_empty_index() -> App {
        App::new(ProjectIndex::default(), false)
    }

    #[test]
    fn quit_and_navigation_on_empty_index_do_not_panic() {
        let mut app = app_with_empty_index();
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('G')));
        assert!(!app.should_quit);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn filter_input_captures_keys() {
        let mut app = app_with_empty_index();
        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.filter_input);
        app.handle_key(key(KeyCode::Char('q'))); // must filter, not quit
        assert!(!app.should_quit);
        assert_eq!(app.filter, "q");
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter_input);
        assert!(app.filter.is_empty());
    }

    #[test]
    fn tab_cycles_focus_in_browse() {
        let mut app = app_with_empty_index();
        assert_eq!(app.focus, Focus::Projects);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Sessions);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Projects);
    }
}
