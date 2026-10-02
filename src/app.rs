//! Application state machine. Key handling is pure state transition so it can
//! be unit-tested without a terminal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent};

use crate::watch::AppEvent;

use crate::agent_tree::{flatten, FlatAgentRow};
use crate::index::ProjectIndex;
use crate::session::{EventKind, LoadedSession, ToolStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Browse,
    Detail,
    /// Embedded `claude --resume` terminal; all keys go to the child
    /// (Ctrl-q detaches, the child keeps running).
    Terminal,
}

/// Coarse liveness of a session, derived from transcript mtime + attached PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Transcript written to seconds ago: claude is thinking/working.
    Working,
    /// Main transcript quiet but a subagent transcript is being written: the
    /// main agent is waiting on its subagents, not on the user.
    SubagentsWorking,
    /// A permission / question dialog is open (hooks only): blocked on you.
    NeedsPermission,
    /// Turn finished and the session is open: waiting for input. Exact with
    /// hooks; without them only known for embedded terminals.
    AwaitingInput,
    Idle,
}

/// Without a newer hook event, a "working" claim older than this is stale
/// (crashed session); a waiting/permission claim lasts longer.
const HOOK_WORKING_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
const HOOK_WAITING_TTL: std::time::Duration = std::time::Duration::from_secs(12 * 3600);
/// `tmux list-panes` at most this often.
const PANES_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// Project-list marker, aggregated over the project's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    /// A session waits on a permission / question dialog.
    NeedsAttention,
    /// Some session (or one of its subagents) is writing right now.
    Active,
    /// An embedded terminal is attached and quiet.
    AwaitingInput,
    /// Touched within `RECENT_PROJECT_WINDOW`, nothing running.
    Recent,
    /// Only older sessions.
    Old,
    Empty,
}

/// Projects touched this recently are marked "recent" rather than "old".
pub const RECENT_PROJECT_WINDOW: std::time::Duration = std::time::Duration::from_secs(3600);

/// A transcript touched this recently counts as "working".
pub const ACTIVITY_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Sessions,
    Timeline,
    Graph,
}

/// Session-list sort order, cycled with `s` in Browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSort {
    /// Scan order (newest first) — the default.
    #[default]
    Mtime,
    Cost,
    Size,
    Duration,
}

impl SessionSort {
    pub fn next(self) -> Self {
        match self {
            SessionSort::Mtime => SessionSort::Cost,
            SessionSort::Cost => SessionSort::Size,
            SessionSort::Size => SessionSort::Duration,
            SessionSort::Duration => SessionSort::Mtime,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SessionSort::Mtime => "mtime",
            SessionSort::Cost => "cost",
            SessionSort::Size => "size",
            SessionSort::Duration => "duration",
        }
    }
}

/// Modal overlay on top of the current view; captures all keys while active.
#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    /// Per-model cost/token breakdown of the loaded session.
    Cost,
    /// Fullscreen, scrollable, untruncated view of one timeline event.
    EventDetail { content: String, scroll: usize },
    /// Keybinding reference for the current view.
    Help,
    /// Cross-project cost/token aggregation.
    Analytics { scroll: usize },
    /// Recently active sessions across all projects.
    Fleet { selected: usize },
}

/// A session counts as "active" in the fleet view when written to this recently.
pub const FLEET_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

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
    /// Project-list filter (`/` with the Projects pane focused).
    pub project_filter: String,
    pub project_filter_input: bool,
    /// `A`: list only projects with something running or awaiting input.
    pub only_active: bool,
    pub watching: bool,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    pub overlay: Overlay,
    /// Lazily loaded subagent final reports, keyed by agent id.
    pub agent_report_cache: HashMap<String, String>,
    /// Agents pane rendering: false = indented list, true = time lanes.
    pub lanes: bool,
    /// Show extended-thinking events in the timeline (`T`).
    pub show_thinking: bool,
    /// Timeline order on screen (`o`): true = newest event at the top.
    pub newest_first: bool,
    /// Timeline search (`/` in Detail); independent of the Browse filter.
    pub search: String,
    pub search_input: bool,
    /// Matching raw timeline indices, ascending, visible events only.
    pub search_matches: Vec<usize>,
    /// Live embedded `claude --resume` terminals, keyed by session id.
    pub ptys: HashMap<String, crate::term::PtySession>,
    /// Which PTY the Terminal view shows.
    pub term_session: Option<String>,
    /// Where Ctrl-q returns to.
    pub term_return: View,
    pub sort: SessionSort,
    /// Channel for background workers; None = load synchronously (tests, CLI).
    pub tx: Option<Sender<AppEvent>>,
    /// Session id currently loading on a worker thread.
    pub loading: Option<String>,
    /// Live agent graph of the selected project (Browse, bottom right).
    pub live: Vec<crate::live::LiveSession>,
    /// Project dir `live` was built for; a mismatch triggers a rebuild.
    pub live_project: Option<PathBuf>,
    /// Exact session states from Claude Code hooks (empty when not installed).
    pub hooks: crate::hooks::HookTracker,
    /// Query tmux for pane liveness / jumps (off in tests).
    pub tmux_enabled: bool,
    /// Cached `tmux list-panes`, refreshed at most every `PANES_TTL`.
    pub panes: Vec<crate::tmux::Pane>,
    panes_at: Option<std::time::Instant>,
}

impl App {
    pub fn new(index: ProjectIndex, watching: bool) -> Self {
        let mut app = Self {
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
            project_filter: String::new(),
            project_filter_input: false,
            only_active: false,
            watching,
            should_quit: false,
            status_msg: None,
            overlay: Overlay::None,
            agent_report_cache: HashMap::new(),
            lanes: false,
            show_thinking: false,
            newest_first: true,
            search: String::new(),
            search_input: false,
            search_matches: Vec::new(),
            ptys: HashMap::new(),
            term_session: None,
            term_return: View::Browse,
            sort: SessionSort::default(),
            tx: None,
            loading: None,
            live: Vec::new(),
            live_project: None,
            hooks: crate::hooks::HookTracker::default(),
            tmux_enabled: false,
            panes: Vec::new(),
            panes_at: None,
        };
        app.sync_live(true);
        app
    }

    fn event_visible(&self, kind: &EventKind) -> bool {
        self.show_thinking || !matches!(kind, EventKind::Thinking { .. })
    }

    /// Indices of currently visible timeline events, ascending.
    /// `selected_event` stays a raw timeline index; navigation and rendering
    /// go through this (mirrors the `visible_sessions` pattern).
    pub fn visible_events(&self) -> Vec<usize> {
        let Some(session) = &self.loaded else { return Vec::new() };
        session
            .timeline
            .iter()
            .enumerate()
            .filter(|(_, e)| self.event_visible(&e.kind))
            .map(|(i, _)| i)
            .collect()
    }

    /// `visible_events` in on-screen order (top to bottom).
    pub fn display_events(&self) -> Vec<usize> {
        let mut visible = self.visible_events();
        if self.newest_first {
            visible.reverse();
        }
        visible
    }

    /// Move the timeline selection by `delta` visible steps in SCREEN
    /// direction (positive = down), whatever the timeline order is.
    fn step_visible(&mut self, delta: isize) {
        let visible = self.visible_events();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|&i| i >= self.selected_event)
            .unwrap_or(visible.len() - 1);
        let delta = if self.newest_first { -delta } else { delta };
        self.selected_event = visible[step(pos, delta, visible.len())];
    }

    /// If the selection points at a hidden event, snap to the next visible one
    /// (or the last visible before it).
    fn snap_selected(&mut self) {
        let visible = self.visible_events();
        let Some(&last) = visible.last() else {
            self.selected_event = 0;
            return;
        };
        if !visible.contains(&self.selected_event) {
            self.selected_event =
                visible.iter().copied().find(|&i| i >= self.selected_event).unwrap_or(last);
        }
    }

    /// Indices into `index.projects` whose name matches the project filter.
    /// `selected_project` stays a raw index; navigation walks this list.
    pub fn visible_projects(&self) -> Vec<usize> {
        let needle = self.project_filter.to_lowercase();
        self.index
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| needle.is_empty() || p.name().to_lowercase().contains(&needle))
            .filter(|(_, p)| {
                !self.only_active
                    || matches!(
                        self.project_status(p),
                        ProjectStatus::NeedsAttention
                            | ProjectStatus::Active
                            | ProjectStatus::AwaitingInput
                    )
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Move the project selection within the filtered list (`delta` rows, or
    /// to the first/last visible one via `isize::MIN`/`isize::MAX`).
    fn step_project(&mut self, delta: isize) {
        let visible = self.visible_projects();
        if visible.is_empty() {
            return;
        }
        let pos = visible.iter().position(|&i| i == self.selected_project).unwrap_or(0);
        let next = match delta {
            isize::MIN => 0,
            isize::MAX => visible.len() - 1,
            d => step(pos, d, visible.len()),
        };
        if visible[next] != self.selected_project {
            self.selected_project = visible[next];
            self.selected_session = 0;
        }
    }

    /// Keep the selection on a visible project after the filter changed.
    fn snap_project_to_filter(&mut self) {
        let visible = self.visible_projects();
        if !visible.contains(&self.selected_project)
            && let Some(&first) = visible.first()
        {
            self.selected_project = first;
            self.selected_session = 0;
        }
    }

    /// Indices into the selected project's session list that match the filter,
    /// ordered by the active sort (stable, so mtime stays the tiebreaker).
    pub fn visible_sessions(&self) -> Vec<usize> {
        let Some(project) = self.index.projects.get(self.selected_project) else {
            return Vec::new();
        };
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<usize> = project
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| needle.is_empty() || s.title.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        let sessions = &project.sessions;
        match self.sort {
            SessionSort::Mtime => {} // scan order is already mtime desc
            SessionSort::Cost => visible.sort_by(|&a, &b| {
                let cost = |i: usize| {
                    sessions[i].cost.as_ref().map_or(f64::NEG_INFINITY, |c| c.total_cost_usd)
                };
                cost(b).total_cmp(&cost(a))
            }),
            SessionSort::Size => {
                visible.sort_by_key(|&i| std::cmp::Reverse(sessions[i].size));
            }
            SessionSort::Duration => visible.sort_by_key(|&i| {
                std::cmp::Reverse(sessions[i].cost.as_ref().map_or(0, |c| c.total_duration))
            }),
        }
        visible
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // The embedded terminal owns every key except the detach chord, so
        // this must run before the global q/r handling and prompts.
        if self.view == View::Terminal {
            self.handle_terminal_key(key);
            return;
        }
        if !matches!(self.overlay, Overlay::None) {
            self.handle_overlay_key(key);
            return;
        }
        if self.project_filter_input {
            match key.code {
                KeyCode::Esc => {
                    self.project_filter.clear();
                    self.project_filter_input = false;
                }
                KeyCode::Enter => self.project_filter_input = false,
                KeyCode::Backspace => {
                    self.project_filter.pop();
                }
                KeyCode::Char(c) => self.project_filter.push(c),
                _ => {}
            }
            self.snap_project_to_filter();
            return;
        }
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
        // Like the filter prompt: must run before the global q/r handling.
        if self.search_input {
            match key.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.search_matches.clear();
                    self.search_input = false;
                }
                KeyCode::Enter => {
                    self.search_input = false;
                    self.jump_to_match(true, true);
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.recompute_search_matches();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.recompute_search_matches();
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
                View::Terminal => {} // handled above; unreachable
            },
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        match &mut self.overlay {
            Overlay::EventDetail { content, scroll } => {
                let max = content.lines().count().saturating_sub(1);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(max),
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Char('d') | KeyCode::PageDown => *scroll = (*scroll + 20).min(max),
                    KeyCode::Char('u') | KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                    KeyCode::Char('g') => *scroll = 0,
                    KeyCode::Char('G') => *scroll = max,
                    _ => {}
                }
            }
            Overlay::Cost => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Char('c') => {
                    self.overlay = Overlay::None
                }
                _ => {}
            },
            Overlay::Help => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Char('?') => {
                    self.overlay = Overlay::None
                }
                _ => {}
            },
            Overlay::Analytics { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Char('a') => {
                    self.overlay = Overlay::None
                }
                // No known content height here; saturate against a generous cap.
                KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(500),
                KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Char('d') | KeyCode::PageDown => *scroll = (*scroll + 20).min(500),
                KeyCode::Char('u') | KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                KeyCode::Char('g') => *scroll = 0,
                _ => {}
            },
            Overlay::Fleet { selected } => {
                let len = crate::analytics::fleet(&self.index, FLEET_WINDOW).len();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Char('j') | KeyCode::Down => *selected = step(*selected, 1, len),
                    KeyCode::Char('k') | KeyCode::Up => *selected = step(*selected, -1, len),
                    KeyCode::Char('g') => *selected = 0,
                    KeyCode::Char('G') => *selected = len.saturating_sub(1),
                    KeyCode::Enter => {
                        let selected = *selected;
                        self.fleet_jump(selected);
                    }
                    _ => {}
                }
            }
            Overlay::None => {}
        }
    }

    /// Enter in the fleet overlay: select that session in Browse.
    fn fleet_jump(&mut self, selected: usize) {
        let entries = crate::analytics::fleet(&self.index, FLEET_WINDOW);
        let Some(entry) = entries.get(selected) else { return };
        let Some(project_pos) =
            self.index.projects.iter().position(|p| p.dir == entry.project_dir)
        else {
            return;
        };
        self.selected_project = project_pos;
        self.project_filter.clear();
        self.filter.clear();
        self.filter_input = false;
        let visible = self.visible_sessions();
        let sessions = &self.index.projects[project_pos].sessions;
        if let Some(pos) = visible.iter().position(|&i| sessions[i].id == entry.session_id) {
            self.selected_session = pos;
        }
        self.focus = Focus::Sessions;
        self.overlay = Overlay::None;
    }

    fn handle_browse_key(&mut self, key: KeyEvent) {
        let sessions = self.visible_sessions().len();
        match key.code {
            // `/` filters whichever list has focus.
            KeyCode::Char('/') => match self.focus {
                Focus::Projects => self.project_filter_input = true,
                _ => {
                    self.filter_input = true;
                    self.focus = Focus::Sessions;
                }
            },
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Projects => self.step_project(1),
                _ => self.selected_session = step(self.selected_session, 1, sessions),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Projects => self.step_project(-1),
                _ => self.selected_session = step(self.selected_session, -1, sessions),
            },
            KeyCode::Char('g') => self.select_first(),
            KeyCode::Char('G') => match self.focus {
                Focus::Projects => self.step_project(isize::MAX),
                _ => self.selected_session = sessions.saturating_sub(1),
            },
            KeyCode::Enter | KeyCode::Char('l') => match self.focus {
                Focus::Projects => self.focus = Focus::Sessions,
                _ => self.open_selected_session(),
            },
            KeyCode::Char('R') => self.request_resume(),
            KeyCode::Char('w') => {
                let visible = self.visible_sessions();
                let meta = visible.get(self.selected_session).and_then(|&i| {
                    self.index.projects.get(self.selected_project)?.sessions.get(i)
                });
                if let Some(meta) = meta {
                    let (id, cwd) = (meta.id.clone(), meta.cwd.clone());
                    self.jump_to_session(id, cwd);
                }
            }
            KeyCode::Char('n') => self.new_session(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('a') => self.overlay = Overlay::Analytics { scroll: 0 },
            KeyCode::Char('A') => {
                self.only_active = !self.only_active;
                self.snap_project_to_filter();
            }
            KeyCode::Char('f') => self.overlay = Overlay::Fleet { selected: 0 },
            KeyCode::Esc | KeyCode::Char('h') => {
                // Cheap load cancellation: the stale result is dropped by id.
                if self.loading.take().is_some() {
                    self.status_msg = None;
                } else if self.focus == Focus::Sessions {
                    if !self.filter.is_empty() {
                        self.filter.clear();
                    } else {
                        self.focus = Focus::Projects;
                    }
                } else if !self.project_filter.is_empty() {
                    self.project_filter.clear();
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
        let agents = self.agent_rows.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Graph => self.selected_agent = step(self.selected_agent, 1, agents),
                _ => self.step_visible(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Graph => self.selected_agent = step(self.selected_agent, -1, agents),
                _ => self.step_visible(-1),
            },
            KeyCode::PageDown | KeyCode::Char('d') => self.step_visible(20),
            KeyCode::PageUp | KeyCode::Char('u') => self.step_visible(-20),
            KeyCode::Char('g') => match self.focus {
                Focus::Graph => self.selected_agent = 0,
                _ => self.selected_event = self.display_events().first().copied().unwrap_or(0),
            },
            KeyCode::Char('G') => match self.focus {
                Focus::Graph => self.selected_agent = agents.saturating_sub(1),
                _ => self.selected_event = self.display_events().last().copied().unwrap_or(0),
            },
            KeyCode::Char('T') => {
                self.show_thinking = !self.show_thinking;
                self.snap_selected();
                self.recompute_search_matches();
            }
            KeyCode::Char('/') => {
                self.search_input = true;
                self.search.clear();
                self.search_matches.clear();
                self.focus = Focus::Timeline;
            }
            KeyCode::Char('n') => self.jump_to_match(true, false),
            KeyCode::Char('N') => self.jump_to_match(false, false),
            KeyCode::Char('e') => self.jump_to_error(),
            KeyCode::Char('c') => self.overlay = Overlay::Cost,
            // The selection is a raw index, so it stays on the same event.
            KeyCode::Char('o') => self.newest_first = !self.newest_first,
            KeyCode::Char('O') => self.open_event_overlay(),
            KeyCode::Char('t') => self.lanes = !self.lanes,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('x') => self.export_loaded(),
            KeyCode::Char('R') => self.resume_loaded(),
            KeyCode::Char('w') => {
                if let Some(session) = &self.loaded {
                    let (id, cwd) = (session.meta.id.clone(), session.meta.cwd.clone());
                    self.jump_to_session(id, cwd);
                }
            }
            KeyCode::Tab => {
                self.focus =
                    if self.focus == Focus::Timeline { Focus::Graph } else { Focus::Timeline };
            }
            // Spawn events keep their advertised cross-jump; other timeline
            // events open the fullscreen view (`O` always opens it).
            KeyCode::Enter => match self.focus {
                Focus::Timeline if !self.selected_is_spawn() => self.open_event_overlay(),
                _ => self.cross_jump(),
            },
            // Esc peels layers: active search first, then back to Browse.
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.search_matches.clear();
            }
            KeyCode::Esc | KeyCode::Char('h') => self.close_detail(),
            _ => {}
        }
        self.ensure_agent_report();
    }

    fn close_detail(&mut self) {
        self.view = View::Browse;
        self.focus = Focus::Sessions;
        self.loaded = None;
        self.agent_rows.clear();
        self.agent_report_cache.clear();
        self.search.clear();
        self.search_input = false;
        self.search_matches.clear();
    }

    fn recompute_search_matches(&mut self) {
        if self.search.is_empty() || self.loaded.is_none() {
            self.search_matches.clear();
            return;
        }
        let needle = self.search.to_lowercase();
        let visible = self.visible_events();
        let session = self.loaded.as_ref().expect("checked above");
        self.search_matches = visible
            .into_iter()
            .filter(|&i| {
                crate::session::event_search_text(&session.timeline[i].kind)
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect();
    }

    /// `n`/`N` (and Enter in the search prompt, with `include_current`):
    /// wrap-around jump through `search_matches`. `down` is screen direction.
    fn jump_to_match(&mut self, down: bool, include_current: bool) {
        if self.search_matches.is_empty() {
            if !self.search.is_empty() {
                self.status_msg = Some(format!("no match for '{}'", self.search));
            }
            return;
        }
        let target = next_wrapping(
            &self.search_matches,
            self.selected_event,
            down != self.newest_first,
            include_current,
        );
        self.selected_event = target;
    }

    /// `e`: next visible event (downward on screen) with an Error/Denied
    /// status, wrapping.
    fn jump_to_error(&mut self) {
        let Some(session) = &self.loaded else { return };
        let errors: Vec<usize> = self
            .visible_events()
            .into_iter()
            .filter(|&i| {
                matches!(
                    crate::session::event_status(&session.timeline[i].kind),
                    Some(ToolStatus::Error | ToolStatus::Denied)
                )
            })
            .collect();
        if errors.is_empty() {
            self.status_msg = Some("no failed tool calls".into());
            return;
        }
        self.selected_event = next_wrapping(&errors, self.selected_event, !self.newest_first, false);
    }

    /// With the graph focused, lazily resolve the selected agent's final report
    /// so `draw` (which only gets `&App`) can render it from the cache.
    fn ensure_agent_report(&mut self) {
        if self.focus != Focus::Graph {
            return;
        }
        let Some(row) = self.agent_rows.get(self.selected_agent) else { return };
        let Some(agent_id) = row.agent_id.clone() else { return };
        if self.agent_report_cache.contains_key(&agent_id) {
            return;
        }
        let Some(session) = &self.loaded else { return };
        let report = crate::session::agent_report(session, row);
        self.agent_report_cache.insert(agent_id, report);
    }

    fn select_first(&mut self) {
        match self.focus {
            Focus::Projects => self.step_project(isize::MIN),
            _ => self.selected_session = 0,
        }
    }

    /// `x` in Detail: write the session as Markdown next to the cwd.
    fn export_loaded(&mut self) {
        let Some(session) = &self.loaded else { return };
        let path = PathBuf::from(format!("{}.md", session.meta.id));
        self.status_msg = match std::fs::write(&path, crate::export::export_markdown(session)) {
            Ok(()) => Some(format!("exported to {}", path.display())),
            Err(e) => Some(format!("export failed: {e}")),
        };
    }

    fn selected_is_spawn(&self) -> bool {
        self.loaded
            .as_ref()
            .and_then(|s| s.timeline.get(self.selected_event))
            .is_some_and(|e| matches!(e.kind, EventKind::SubagentSpawn { .. }))
    }

    fn open_event_overlay(&mut self) {
        let Some(session) = &self.loaded else { return };
        let Some(event) = session.timeline.get(self.selected_event) else { return };
        let content = crate::session::full_event_content(session, event);
        self.overlay = Overlay::EventDetail { content, scroll: 0 };
    }

    /// Enter in the detail view: spawn event ↔ graph row jump to each other.
    fn cross_jump(&mut self) {
        match self.focus {
            Focus::Timeline => {
                let Some(session) = &self.loaded else { return };
                let Some(event) = session.timeline.get(self.selected_event) else { return };
                if let EventKind::SubagentSpawn { agent_id: Some(id), .. } = &event.kind
                    && let Some((row_idx, row)) = self
                        .agent_rows
                        .iter()
                        .enumerate()
                        .find(|(_, r)| r.agent_id.as_deref() == Some(id.as_str()))
                    {
                        self.selected_agent = row_idx;
                        if let Some((first, _)) = row.event_range {
                            self.selected_event = first;
                            self.snap_selected();
                        }
                        self.focus = Focus::Graph;
                    }
            }
            Focus::Graph => {
                if let Some(row) = self.agent_rows.get(self.selected_agent)
                    && let Some((first, _)) = row.event_range
                {
                    self.selected_event = first;
                    self.snap_selected();
                    self.focus = Focus::Timeline;
                }
            }
            _ => {}
        }
    }

    /// `s` in Browse: next sort order, keeping the selected session by identity.
    fn cycle_sort(&mut self) {
        let selected_id = self
            .visible_sessions()
            .get(self.selected_session)
            .and_then(|&i| self.index.projects.get(self.selected_project)?.sessions.get(i))
            .map(|s| s.id.clone());
        self.sort = self.sort.next();
        let visible = self.visible_sessions();
        self.selected_session = selected_id
            .and_then(|id| {
                let sessions = &self.index.projects.get(self.selected_project)?.sessions;
                visible.iter().position(|&i| sessions[i].id == id)
            })
            .unwrap_or(0);
    }

    /// `R` in Browse: attach an embedded `claude --resume` terminal for the
    /// selected session (or switch to an already running one).
    fn request_resume(&mut self) {
        let visible = self.visible_sessions();
        let Some(&session_idx) = visible.get(self.selected_session) else { return };
        let Some(project) = self.index.projects.get(self.selected_project) else { return };
        let meta = &project.sessions[session_idx];
        let (id, cwd) = (meta.id.clone(), meta.cwd.clone());
        self.start_resume(id, cwd);
    }

    /// `R` in Detail: same, for the loaded session.
    fn resume_loaded(&mut self) {
        let Some(session) = &self.loaded else { return };
        let (id, cwd) = (session.meta.id.clone(), session.meta.cwd.clone());
        self.start_resume(id, cwd);
    }

    fn start_resume(&mut self, id: String, cwd: Option<String>) {
        if self.ptys.contains_key(&id) {
            self.open_terminal(id);
            return;
        }
        let Some(cwd) = cwd.map(PathBuf::from).filter(|p| p.is_dir()) else {
            self.status_msg = Some("no working directory known for this session".into());
            return;
        };
        self.spawn_claude(id.clone(), &["--resume", &id], &cwd);
    }

    /// `n` in Browse: start a fresh `claude` in the selected project's directory.
    /// We choose the session id up front (`--session-id`), so the terminal is
    /// keyed by the real id and indicators/`R`/Detail pick it up once claude
    /// writes the transcript.
    fn new_session(&mut self) {
        let Some(project) = self.index.projects.get(self.selected_project) else { return };
        let Some(cwd) = project.working_dir() else {
            self.status_msg = Some("project directory not found on disk".into());
            return;
        };
        let id = uuid::Uuid::new_v4().to_string();
        self.spawn_claude(id.clone(), &["--session-id", &id], &cwd);
    }

    fn spawn_claude(&mut self, id: String, args: &[&str], cwd: &Path) {
        let Some(tx) = self.tx.clone() else {
            self.status_msg = Some("embedded terminal needs the interactive event loop".into());
            return;
        };
        let (cols, rows) = self.term_size();
        match crate::term::PtySession::spawn(id.clone(), args, cwd, rows, cols, tx) {
            Ok(pty) => {
                self.ptys.insert(id.clone(), pty);
                self.open_terminal(id);
            }
            Err(e) => self.status_msg = Some(format!("starting claude failed: {e}")),
        }
    }

    /// `w`: go to where the session runs — the embedded terminal if we own
    /// it, else its tmux pane (from hooks, or the unique claude pane in its
    /// cwd).
    fn jump_to_session(&mut self, id: String, cwd: Option<String>) {
        if self.ptys.contains_key(&id) {
            self.open_terminal(id);
            return;
        }
        self.panes_at = None; // panes move; never jump on stale data
        self.refresh_panes();
        let hooked = self.hook_view(&id).and_then(|h| Some((h.tmux_pane.clone()?, h.tmux_socket.clone())));
        let target = hooked.or_else(|| {
            let pane = crate::tmux::pane_by_cwd(&self.panes, cwd.as_deref()?)?;
            Some((pane.id.clone(), None))
        });
        let Some((pane, socket)) = target else {
            self.status_msg = Some(if self.hooks.active() {
                "session isn't running in a tmux pane".into()
            } else {
                "no tmux pane known — install hooks (--install-hooks) for exact tracking".into()
            });
            return;
        };
        match crate::tmux::focus(socket.as_deref(), &pane) {
            Ok(()) => {
                let target = self.panes.iter().find(|p| p.id == pane).map_or(pane.clone(), |p| p.target.clone());
                self.status_msg = Some(format!("→ tmux {target}"));
            }
            Err(e) => self.status_msg = Some(format!("tmux: {e}")),
        }
    }

    /// Re-list tmux panes if the cache is older than `PANES_TTL`.
    pub fn refresh_panes(&mut self) {
        if !self.tmux_enabled || self.panes_at.is_some_and(|t| t.elapsed() < PANES_TTL) {
            return;
        }
        let socket = self.hooks.sessions.values().find_map(|h| h.tmux_socket.clone());
        self.panes = crate::tmux::list_panes(None);
        if self.panes.is_empty() && socket.is_some() {
            self.panes = crate::tmux::list_panes(socket.as_deref());
        }
        self.panes_at = Some(std::time::Instant::now());
    }

    /// The hook state of a session if it is still believable: not ended, not
    /// timed out, its tmux pane (if any) still runs claude, and no newer
    /// session has taken over that pane.
    pub fn hook_view(&self, id: &str) -> Option<&crate::hooks::HookSession> {
        use crate::hooks::HookState;
        let h = self.hooks.sessions.get(id)?;
        let age = h.at.elapsed().unwrap_or_default();
        let ttl = if h.state == HookState::Working { HOOK_WORKING_TTL } else { HOOK_WAITING_TTL };
        if h.state == HookState::Ended || age > ttl {
            return None;
        }
        if let Some(pane) = &h.tmux_pane {
            if self.tmux_enabled
                && self.panes_at.is_some()
                && !self.panes.iter().any(|p| &p.id == pane && p.runs_claude())
            {
                return None; // pane closed or claude exited
            }
            let superseded = self.hooks.sessions.iter().any(|(other, o)| {
                other != id && o.tmux_pane.as_ref() == Some(pane) && o.at > h.at
            });
            if superseded {
                return None;
            }
        }
        Some(h)
    }

    /// What the session is doing per hooks: tool one-liner or prompt text.
    pub fn activity_detail(&self, id: &str) -> Option<&str> {
        self.hook_view(id)?.detail.as_deref()
    }

    fn open_terminal(&mut self, id: String) {
        self.term_return = self.view;
        self.term_session = Some(id);
        self.view = View::Terminal;
        self.status_msg = None;
    }

    /// Terminal pane dimensions: the frame minus the status line.
    fn term_size(&self) -> (u16, u16) {
        crossterm::terminal::size().map(|(w, h)| (w, h.saturating_sub(1))).unwrap_or((80, 24))
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Detach: the child keeps running; indicators track it in Browse.
            self.view = self.term_return;
            return;
        }
        let Some(id) = self.term_session.clone() else {
            self.view = View::Browse;
            return;
        };
        if let Some(pty) = self.ptys.get_mut(&id) {
            pty.send_key(key);
        }
    }

    /// A PTY child exited: drop it and leave the Terminal view if it was shown.
    pub fn on_pty_exited(&mut self, id: &str) {
        self.ptys.remove(id);
        if self.term_session.as_deref() == Some(id) {
            self.term_session = None;
            if self.view == View::Terminal {
                self.view = self.term_return;
            }
        }
        self.status_msg = Some(format!("resumed session {} ended", &id[..8.min(id.len())]));
        self.reload();
    }

    pub fn resize_ptys(&mut self, cols: u16, rows: u16) {
        for pty in self.ptys.values_mut() {
            pty.resize(rows, cols);
        }
    }

    /// Rebuild the live agent graph when the selected project changed, or
    /// unconditionally with `force` (after a rescan: files moved on).
    pub fn sync_live(&mut self, force: bool) {
        let project = self.index.projects.get(self.selected_project);
        let dir = project.map(|p| p.dir.clone());
        if !force && dir == self.live_project {
            return;
        }
        let live = project
            .map(|p| crate::live::snapshot(p, |s| self.hook_view(&s.id).is_some()))
            .unwrap_or_default();
        self.live = live;
        self.live_project = dir;
    }

    /// Strongest activity over a project's sessions, for the project list.
    pub fn project_status(&self, project: &crate::index::ProjectEntry) -> ProjectStatus {
        let Some(newest) = project.sessions.iter().map(|s| s.mtime).max() else {
            return ProjectStatus::Empty;
        };
        let mut awaiting = false;
        // Only recent sessions can be active; skips the long tail cheaply.
        let candidates = project
            .sessions
            .iter()
            .filter(|s| {
                crate::live::is_recent(s)
                    || self.ptys.contains_key(&s.id)
                    || self.hooks.sessions.contains_key(&s.id)
            });
        let mut active = false;
        for session in candidates {
            match self.activity(session) {
                Activity::NeedsPermission => return ProjectStatus::NeedsAttention,
                Activity::Working | Activity::SubagentsWorking => active = true,
                Activity::AwaitingInput => awaiting = true,
                Activity::Idle => {}
            }
        }
        if active {
            ProjectStatus::Active
        } else if awaiting {
            ProjectStatus::AwaitingInput
        } else if newest.elapsed().is_ok_and(|d| d < RECENT_PROJECT_WINDOW) {
            ProjectStatus::Recent
        } else {
            ProjectStatus::Old
        }
    }

    /// Liveness for indicators: hook state when we have a believable one,
    /// else transcript mtimes + attached PTY.
    pub fn activity(&self, meta: &crate::index::SessionMeta) -> Activity {
        use crate::hooks::HookState;
        let fresh = |t: std::time::SystemTime| {
            t.elapsed().map(|d| d < ACTIVITY_WINDOW).unwrap_or(false)
        };
        let pty = self.ptys.get(&meta.id);
        if pty.is_some_and(|p| p.output_within(std::time::Duration::from_secs(2))) {
            return Activity::Working;
        }
        if let Some(hook) = self.hook_view(&meta.id) {
            return match hook.state {
                HookState::NeedsPermission => Activity::NeedsPermission,
                HookState::Working => Activity::Working,
                // Background subagents keep running after the main turn ends.
                _ if meta.subagent_mtime.is_some_and(fresh) => Activity::SubagentsWorking,
                _ => Activity::AwaitingInput,
            };
        }
        if self.hooks.sessions.get(&meta.id).is_some_and(|h| h.state == HookState::Ended) {
            return Activity::Idle; // ended: trailing bookkeeping writes don't count
        }
        if fresh(meta.mtime) {
            Activity::Working
        } else if meta.subagent_mtime.is_some_and(fresh) {
            // Checked before AwaitingInput: with background agents the main
            // prompt is idle, but the session is still busy.
            Activity::SubagentsWorking
        } else if pty.is_some() {
            Activity::AwaitingInput
        } else {
            Activity::Idle
        }
    }

    pub fn open_selected_session(&mut self) {
        let visible = self.visible_sessions();
        let Some(&session_idx) = visible.get(self.selected_session) else { return };
        let Some(project) = self.index.projects.get(self.selected_project) else { return };
        let meta = project.sessions[session_idx].clone();
        if self.loading.as_deref() == Some(meta.id.as_str()) {
            return; // already loading this one (Enter mashing)
        }
        match &self.tx {
            // Load on a worker thread; big sessions must not freeze the UI.
            Some(tx) => {
                self.loading = Some(meta.id.clone());
                self.status_msg =
                    Some(format!("loading {}…", &meta.id[..8.min(meta.id.len())]));
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let id = meta.id.clone();
                    let result = LoadedSession::load(meta);
                    let _ = tx.send(AppEvent::Loaded { id, result: Box::new(result) });
                });
            }
            None => self.finish_load(LoadedSession::load(meta)),
        }
    }

    /// A worker's load result arrived; ignore it if the user moved on.
    pub fn on_loaded(&mut self, id: String, result: std::io::Result<LoadedSession>) {
        if self.loading.as_deref() != Some(id.as_str()) {
            return; // stale: a different session was opened meanwhile
        }
        self.loading = None;
        self.status_msg = None;
        self.finish_load(result);
    }

    fn finish_load(&mut self, result: std::io::Result<LoadedSession>) {
        match result {
            Ok(session) => {
                self.agent_rows = flatten(&session.agent_tree);
                // Start at the top of the screen: newest event if newest-first.
                self.selected_event = if self.newest_first { usize::MAX } else { 0 };
                self.selected_agent = 0;
                self.loaded = Some(session);
                self.snap_selected();
                self.view = View::Detail;
                self.focus = Focus::Timeline;
                self.status_msg = None;
                self.agent_report_cache.clear();
                self.search.clear();
                self.search_input = false;
                self.search_matches.clear();
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
        self.snap_project_to_filter();
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
        self.refresh_panes();
        self.sync_live(true);
    }

    /// Incrementally reload the open session (called on fs events and `r`).
    pub fn refresh_loaded(&mut self) {
        // "At end" must mean the last VISIBLE event, or a trailing hidden
        // thinking event would silently break tail-follow.
        let was_at_end =
            self.visible_events().last().is_none_or(|&last| self.selected_event >= last);
        let Some(session) = &mut self.loaded else { return };
        if let Err(e) = session.refresh() {
            self.status_msg = Some(format!("refresh failed: {e}"));
            return;
        }
        self.agent_rows = flatten(&session.agent_tree);
        let visible = self.visible_events();
        if was_at_end && let Some(&last) = visible.last() {
            // Tail-follow: stay glued to the newest visible event.
            self.selected_event = last;
        }
        self.snap_selected();
        self.selected_agent = self.selected_agent.min(self.agent_rows.len().saturating_sub(1));
        // Running agents keep appending; drop cached reports so they re-resolve.
        self.agent_report_cache.clear();
        self.ensure_agent_report();
        // rebuild() re-sorts the timeline wholesale: match indices are stale.
        self.recompute_search_matches();
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

/// Next entry of the ascending, non-empty `sorted` after `cur` (`forward`)
/// or before it, wrapping around; `include_current` also accepts `cur`.
fn next_wrapping(sorted: &[usize], cur: usize, forward: bool, include_current: bool) -> usize {
    let hit = if forward {
        sorted.iter().copied().find(|&i| if include_current { i >= cur } else { i > cur })
    } else {
        sorted.iter().rev().copied().find(|&i| if include_current { i <= cur } else { i < cur })
    };
    let wrap = if forward { sorted.first() } else { sorted.last() };
    hit.or(wrap.copied()).unwrap_or(cur)
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
    fn active_toggle_hides_quiet_projects() {
        let dir = tempfile::tempdir().unwrap();
        let session = |name: &str, age_secs: u64| {
            let path = dir.path().join(format!("{name}.jsonl"));
            std::fs::write(&path, "{}\n").unwrap();
            let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(mtime).unwrap();
            crate::index::SessionMeta::tail_scan(&path).unwrap()
        };
        let project = |path: &str, sessions| crate::index::ProjectEntry {
            dir: PathBuf::from(path),
            display_path: path.into(),
            sessions,
        };
        let index = ProjectIndex {
            root: PathBuf::from("/"),
            projects: vec![
                project("/work/old", vec![session("old", 7200)]),
                project("/work/live", vec![session("live", 0)]),
                project("/work/empty", Vec::new()),
            ],
        };
        let mut app = App::new(index, false);
        app.focus = Focus::Projects;
        assert_eq!(app.project_status(&app.index.projects[0]), ProjectStatus::Old);
        app.handle_key(key(KeyCode::Char('A')));
        assert!(app.only_active);
        assert_eq!(app.visible_projects(), vec![1]);
        assert_eq!(app.selected_project, 1, "hidden selection snaps to the active one");
        assert!(matches!(app.overlay, Overlay::None), "A must not open analytics");
        app.handle_key(key(KeyCode::Char('A')));
        assert_eq!(app.visible_projects().len(), 3);
    }

    #[test]
    fn hook_state_overrides_mtime_heuristics() {
        let dir = tempfile::tempdir().unwrap();
        let session = |name: &str| {
            let path = dir.path().join(format!("{name}.jsonl"));
            std::fs::write(&path, "{}\n").unwrap(); // fresh mtime ⇒ heuristic says Working
            crate::index::SessionMeta::tail_scan(&path).unwrap()
        };
        let (perm, ended, old, new) = (session("perm"), session("ended"), session("old"), session("new"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let events = dir.path().join("events.jsonl");
        let line = |id: &str, event: &str, ts: u128, extra: &str| {
            format!(r#"{{"ts":{ts},"event":"{event}","session_id":"{id}"{extra}}}"#)
        };
        std::fs::write(
            &events,
            [
                line("perm", "PermissionRequest", now, r#","tool":"Bash: rm""#),
                line("ended", "SessionEnd", now, ""),
                line("old", "Stop", now - 1000, r#","tmux_pane":"%1""#),
                line("new", "Stop", now, r#","tmux_pane":"%1""#),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let mut app = App::new(ProjectIndex::default(), false);
        app.hooks = crate::hooks::HookTracker::new(Some(events));
        assert_eq!(app.activity(&perm), Activity::NeedsPermission);
        assert_eq!(app.activity_detail("perm"), Some("Bash: rm"));
        // Ended beats the fresh transcript (trailing bookkeeping writes).
        assert_eq!(app.activity(&ended), Activity::Idle);
        // A newer session in the same pane supersedes the old one, whose
        // hook state is ignored ⇒ heuristic (fresh mtime) applies.
        assert_eq!(app.activity(&new), Activity::AwaitingInput);
        assert!(app.hook_view("old").is_none());
        assert_eq!(app.activity(&old), Activity::Working);
    }

    #[test]
    fn project_filter_matches_names_and_moves_selection() {
        let project = |path: &str| crate::index::ProjectEntry {
            dir: PathBuf::from(path),
            display_path: path.into(),
            sessions: Vec::new(),
        };
        let index = ProjectIndex {
            root: PathBuf::from("/"),
            projects: vec![
                project("/work/alpha"),
                project("/work/beta"),
                project("/work/alphabet"),
            ],
        };
        let mut app = App::new(index, false);
        app.focus = Focus::Projects;
        app.selected_project = 1; // beta
        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.project_filter_input);
        assert!(!app.filter_input, "session filter stays untouched");
        // "work" is only in the hidden path part, so it must not match.
        for c in "work".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert!(app.visible_projects().is_empty());
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Backspace));
        }
        for c in "alph".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.visible_projects(), vec![0, 2]);
        assert_eq!(app.selected_project, 0, "hidden selection snaps to first match");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('j'))); // skips hidden beta
        assert_eq!(app.selected_project, 2);
        app.handle_key(key(KeyCode::Esc)); // clears the project filter
        assert!(app.project_filter.is_empty());
        assert_eq!(app.visible_projects().len(), 3);
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
        app.focus = Focus::Sessions; // `/` on Projects filters projects instead
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
    fn resume_without_cwd_sets_status_message() {
        use crate::index::{ProjectEntry, SessionMeta, TitleSource};
        use std::time::SystemTime;
        let index = ProjectIndex {
            root: PathBuf::new(),
            projects: vec![ProjectEntry {
                dir: PathBuf::from("/x"),
                display_path: "/x".into(),
                sessions: vec![SessionMeta {
                    id: "s1".into(),
                    path: PathBuf::from("/x/s1.jsonl"),
                    title: "t".into(),
                    title_source: TitleSource::SessionId,
                    mtime: SystemTime::UNIX_EPOCH,
                    size: 0,
                    cost: None,
                    cwd: None,
                    subagent_count: 0,
                    subagent_mtime: None,
                }],
            }],
        };
        let mut app = App::new(index, false);
        app.focus = Focus::Sessions;
        app.handle_key(key(KeyCode::Char('R')));
        assert!(app.ptys.is_empty());
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn sort_cycles_orders_sessions_and_keeps_selection_identity() {
        use crate::index::{ProjectEntry, SessionMeta, TitleSource};
        use crate::model::CostState;
        use std::time::{Duration, SystemTime};
        let mk = |id: &str, secs: u64, cost: f64, size: u64, dur: u64| SessionMeta {
            id: id.into(),
            path: PathBuf::from(format!("/x/{id}.jsonl")),
            title: id.into(),
            title_source: TitleSource::SessionId,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            size,
            cost: Some(CostState {
                total_cost_usd: cost,
                total_duration: dur,
                ..Default::default()
            }),
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        let index = ProjectIndex {
            root: PathBuf::new(),
            projects: vec![ProjectEntry {
                dir: PathBuf::from("/x"),
                display_path: "/x".into(),
                // scan order = mtime desc
                sessions: vec![
                    mk("a", 300, 1.0, 50, 10),
                    mk("b", 200, 3.0, 10, 30),
                    mk("c", 100, 2.0, 90, 20),
                ],
            }],
        };
        let mut app = App::new(index, false);
        app.focus = Focus::Sessions;
        assert_eq!(app.visible_sessions(), vec![0, 1, 2]); // mtime
        app.handle_key(key(KeyCode::Char('s'))); // cost desc: b, c, a
        assert_eq!(app.visible_sessions(), vec![1, 2, 0]);
        assert_eq!(app.selected_session, 2); // selection followed session "a"
        app.handle_key(key(KeyCode::Char('s'))); // size desc: c, a, b
        assert_eq!(app.visible_sessions(), vec![2, 0, 1]);
        app.handle_key(key(KeyCode::Char('s'))); // duration desc: b, c, a
        assert_eq!(app.visible_sessions(), vec![1, 2, 0]);
        app.handle_key(key(KeyCode::Char('s'))); // back to mtime
        assert_eq!(app.visible_sessions(), vec![0, 1, 2]);
        assert_eq!(app.selected_session, 0);
    }

    #[test]
    fn fleet_jump_selects_project_and_session() {
        use crate::index::{ProjectEntry, SessionMeta, TitleSource};
        use std::time::{Duration, SystemTime};
        let mk = |id: &str, age_secs: u64| SessionMeta {
            id: id.into(),
            path: PathBuf::from(format!("/x/{id}.jsonl")),
            title: id.into(),
            title_source: TitleSource::SessionId,
            mtime: SystemTime::now() - Duration::from_secs(age_secs),
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        let index = ProjectIndex {
            root: PathBuf::new(),
            projects: vec![
                ProjectEntry {
                    dir: PathBuf::from("/p1"),
                    display_path: "/p1".into(),
                    sessions: vec![mk("stale", 3600)],
                },
                ProjectEntry {
                    dir: PathBuf::from("/p2"),
                    display_path: "/p2".into(),
                    sessions: vec![mk("older-active", 120), mk("live", 10)],
                },
            ],
        };
        let mut app = App::new(index, false);
        app.filter = "x".into(); // must be cleared by the jump
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.overlay, Overlay::Fleet { selected: 0 }));
        // Fleet is newest-first: entry 0 = "live", entry 1 = "older-active".
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.selected_project, 1);
        assert_eq!(app.focus, Focus::Sessions);
        assert!(app.filter.is_empty());
        let visible = app.visible_sessions();
        let sessions = &app.index.projects[1].sessions;
        assert_eq!(sessions[visible[app.selected_session]].id, "older-active");
    }

    #[test]
    fn thinking_hidden_navigation_skips_and_toggle_snaps() {
        use crate::index::{SessionMeta, TitleSource};
        use crate::session::LoadedSession;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{{"role":"user","content":"go"}}}}"#).unwrap();
        writeln!(f, r#"{{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{{"id":"m1","role":"assistant","content":[{{"type":"thinking","thinking":"pondering"}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:02Z","message":{{"id":"m2","role":"assistant","content":[{{"type":"text","text":"done"}}]}}}}"#).unwrap();
        drop(f);
        let meta = SessionMeta {
            id: "s1".into(),
            path,
            title: "t".into(),
            title_source: TitleSource::SessionId,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        let mut app = app_with_empty_index();
        app.loaded = Some(LoadedSession::load(meta).unwrap());
        app.view = View::Detail;
        app.focus = Focus::Timeline;
        app.newest_first = false;
        assert_eq!(app.visible_events(), vec![0, 2]); // thinking at 1 hidden
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_event, 2); // skipped the hidden event
        app.handle_key(key(KeyCode::Char('T'))); // reveal
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected_event, 1); // thinking selectable now
        app.handle_key(key(KeyCode::Char('T'))); // hide while selected → snap
        assert_eq!(app.selected_event, 2);
    }

    fn app_with_search_fixture() -> App {
        use crate::index::{SessionMeta, TitleSource};
        use crate::session::LoadedSession;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{{"role":"user","content":"find the needle"}}}}"#).unwrap();
        writeln!(f, r#"{{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{{"id":"m1","role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"uuid":"u2","type":"user","timestamp":"2026-01-01T10:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"Error: boom","is_error":true}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:03Z","message":{{"id":"m2","role":"assistant","content":[{{"type":"text","text":"the needle was here"}}]}}}}"#).unwrap();
        drop(f);
        let meta = SessionMeta {
            id: "s1".into(),
            path,
            title: "t".into(),
            title_source: TitleSource::SessionId,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        let mut app = app_with_empty_index();
        app.loaded = Some(LoadedSession::load(meta).unwrap());
        app.view = View::Detail;
        app.focus = Focus::Timeline;
        app.newest_first = false; // tests below are written top-down = ascending
        // keep the tempdir alive via leak: the file is fully read already
        std::mem::forget(dir);
        app
    }

    #[test]
    fn search_captures_keys_matches_and_wraps() {
        let mut app = app_with_search_fixture();
        // timeline: [0]=prompt, [1]=tool call (error), [2]=assistant text
        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.search_input);
        for c in "needle".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert!(!app.should_quit); // 'e' in "needle" must not error-jump, 'n'... etc.
        assert_eq!(app.search_matches, vec![0, 2]);
        app.handle_key(key(KeyCode::Enter)); // commit: jump to first match >= current(0)
        assert!(!app.search_input);
        assert_eq!(app.selected_event, 0);
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.selected_event, 2);
        app.handle_key(key(KeyCode::Char('n'))); // wraps
        assert_eq!(app.selected_event, 0);
        app.handle_key(key(KeyCode::Char('N'))); // wraps backwards
        assert_eq!(app.selected_event, 2);
        // Esc layering: clears search first, stays in Detail.
        app.handle_key(key(KeyCode::Esc));
        assert!(app.search.is_empty());
        assert_eq!(app.view, View::Detail);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Browse);
    }

    #[test]
    fn newest_first_reverses_screen_order_and_navigation() {
        let mut app = app_with_search_fixture();
        // timeline: [0]=prompt, [1]=tool call (error), [2]=assistant text
        app.newest_first = true;
        assert_eq!(app.display_events(), vec![2, 1, 0]);
        app.handle_key(key(KeyCode::Char('g'))); // top = newest
        assert_eq!(app.selected_event, 2);
        app.handle_key(key(KeyCode::Char('j'))); // down = older
        assert_eq!(app.selected_event, 1);
        app.handle_key(key(KeyCode::Char('G'))); // bottom = oldest
        assert_eq!(app.selected_event, 0);
        app.handle_key(key(KeyCode::Char('k'))); // up = newer
        assert_eq!(app.selected_event, 1);
        // `o` flips the order, keeping the same event selected.
        app.handle_key(key(KeyCode::Char('o')));
        assert!(!app.newest_first);
        assert_eq!(app.display_events(), vec![0, 1, 2]);
        assert_eq!(app.selected_event, 1);
        app.handle_key(key(KeyCode::Char('o')));
        // Search `n` walks downward on screen: newest match first, then older.
        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(key(KeyCode::Char('/')));
        for c in "needle".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter)); // include current (2, a match)
        assert_eq!(app.selected_event, 2);
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.selected_event, 0);
        app.handle_key(key(KeyCode::Char('n'))); // wraps to the top
        assert_eq!(app.selected_event, 2);
        app.handle_key(key(KeyCode::Char('N'))); // upward, wrapping to the bottom
        assert_eq!(app.selected_event, 0);
    }

    #[test]
    fn error_jump_finds_failed_tool_call_and_wraps() {
        let mut app = app_with_search_fixture();
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.selected_event, 1); // the failed Bash call
        app.handle_key(key(KeyCode::Char('e'))); // no later error: wraps to itself
        assert_eq!(app.selected_event, 1);
    }

    #[test]
    fn stale_load_results_are_dropped_matching_ones_open_detail() {
        use crate::index::{SessionMeta, TitleSource};
        use crate::session::LoadedSession;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{{"role":"user","content":"hi"}}}}"#).unwrap();
        drop(f);
        let meta = SessionMeta {
            id: "s1".into(),
            path,
            title: "t".into(),
            title_source: TitleSource::SessionId,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        let mut app = app_with_empty_index();
        let session = LoadedSession::load(meta.clone()).unwrap();
        // Stale: nothing is loading → dropped.
        app.on_loaded("s1".into(), Ok(session));
        assert_eq!(app.view, View::Browse);
        assert!(app.loaded.is_none());
        // Stale by id: a different session is loading.
        app.loading = Some("other".into());
        app.on_loaded("s1".into(), Ok(LoadedSession::load(meta.clone()).unwrap()));
        assert!(app.loaded.is_none());
        assert_eq!(app.loading.as_deref(), Some("other"));
        // Matching id opens Detail.
        app.loading = Some("s1".into());
        app.on_loaded("s1".into(), Ok(LoadedSession::load(meta).unwrap()));
        assert_eq!(app.view, View::Detail);
        assert!(app.loaded.is_some());
        assert!(app.loading.is_none());
    }

    #[test]
    fn terminal_view_routes_keys_to_child_not_app() {
        let mut app = app_with_empty_index();
        app.view = View::Terminal;
        app.term_return = View::Browse;
        app.term_session = Some("s1".into());
        // 'q' inside the terminal view must NOT quit the app.
        app.handle_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert_eq!(app.view, View::Terminal);
        // Ctrl-q detaches (even with no live pty) and keeps the app running.
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert_eq!(app.view, View::Browse);
        assert!(!app.should_quit);
    }

    #[test]
    fn activity_reflects_mtime() {
        use std::time::SystemTime;
        let app = app_with_empty_index();
        let mut meta = crate::index::SessionMeta {
            id: "x".into(),
            path: PathBuf::from("/tmp/x.jsonl"),
            title: "t".into(),
            title_source: crate::index::TitleSource::SessionId,
            mtime: SystemTime::now(),
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        };
        assert_eq!(app.activity(&meta), Activity::Working);
        // Quiet and unattached = idle (AwaitingInput needs a live pty).
        meta.mtime = SystemTime::UNIX_EPOCH;
        assert_eq!(app.activity(&meta), Activity::Idle);
        // Quiet main transcript, busy subagent: waiting on the subagent.
        meta.subagent_mtime = Some(SystemTime::now());
        assert_eq!(app.activity(&meta), Activity::SubagentsWorking);
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
