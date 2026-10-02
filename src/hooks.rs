//! Exact session state from Claude Code hooks.
//!
//! `--install-hooks` registers `claude-agent-tree --hook` (async, so it can
//! never slow claude down) for the lifecycle events in the user's global
//! settings. Each invocation appends one compact line to `events_path()`;
//! the TUI tail-follows that file like a transcript and keeps the newest
//! state per session. This replaces the mtime heuristics where available and
//! adds what transcripts cannot show: a pending permission prompt, and the
//! tmux pane a session lives in.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

/// Rotate the event log beyond this size (the previous one is kept as `.1`).
const ROTATE_AT: u64 = 4 * 1024 * 1024;
/// Hook stdin carries full tool inputs (whole files for Write); cap the read.
const STDIN_CAP: u64 = 1024 * 1024;
/// Marker identifying our entries in settings.json for uninstall/idempotency.
const HOOK_ARG: &str = "--hook";

/// (event, matcher) pairs we register. Tool events need a matcher; the rest
/// take none (Notification without one sees every notification type).
const EVENTS: [(&str, Option<&str>); 9] = [
    ("SessionStart", None),
    ("SessionEnd", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some("*")),
    ("PostToolUse", Some("*")),
    ("PermissionRequest", None),
    ("Notification", None),
    ("Stop", None),
    ("SubagentStop", None),
];

pub fn events_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join("agent-tree").join("events.jsonl"))
}

pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join("settings.json"))
}

// ---------------------------------------------------------------------------
// Hook side: runs inside every claude session, must be fast and never fail.

/// `--hook`: stdin JSON → one appended line. Errors are swallowed; a hook
/// must never disturb the session it observes.
pub fn run_hook() {
    let mut input = String::new();
    let _ = std::io::stdin().take(STDIN_CAP).read_to_string(&mut input);
    let Ok(value) = serde_json::from_str::<Value>(&input) else { return };
    let record = hook_record(
        &value,
        std::env::var("TMUX_PANE").ok(),
        std::env::var("TMUX").ok(),
        now_ms(),
    );
    if let Some(path) = events_path() {
        let _ = append_record(&path, &record);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The compact line we persist: no tool inputs, just a one-liner.
fn hook_record(input: &Value, tmux_pane: Option<String>, tmux: Option<String>, ts: i64) -> Value {
    let str_field = |k: &str| input.get(k).and_then(Value::as_str).map(String::from);
    let tool = str_field("tool_name").map(|name| {
        crate::session::tool_one_liner(&name, input.get("tool_input").unwrap_or(&Value::Null))
    });
    // $TMUX is "<socket>,<pid>,<session>"; the socket addresses the server.
    let tmux_socket = tmux.and_then(|t| t.split(',').next().map(String::from));
    let message = str_field("message").map(|m| crate::session::first_line(&m, 160));
    json!({
        "ts": ts,
        "event": str_field("hook_event_name"),
        "session_id": str_field("session_id"),
        "cwd": str_field("cwd"),
        "tool": tool,
        "notification_type": str_field("notification_type"),
        "message": message,
        "tmux_pane": tmux_pane.filter(|p| !p.is_empty()),
        "tmux_socket": tmux_socket.filter(|s| !s.is_empty()),
    })
}

fn append_record(path: &Path, record: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = fs::rename(path, path.with_extension("jsonl.1"));
    }
    // One write_all of a whole line with O_APPEND: concurrent hooks from
    // parallel sessions don't interleave mid-line.
    let mut line = record.to_string();
    line.push('\n');
    fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

// ---------------------------------------------------------------------------
// Install / uninstall into settings.json.

fn is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(|h| {
            h.get("command").and_then(Value::as_str).is_some_and(|c| {
                c.contains("claude-agent-tree") && c.contains(HOOK_ARG)
            })
        }))
}

/// Shell command for settings.json: quoted absolute binary path; `|| true` so
/// a moved/deleted binary degrades to a no-op instead of a hook error.
pub fn hook_command(exe: &Path) -> String {
    let quoted = exe.to_string_lossy().replace('\'', r"'\''");
    format!("'{quoted}' {HOOK_ARG} 2>/dev/null || true")
}

/// Add our entries (replacing older ones of ours, e.g. after the binary
/// moved). Other tools' hooks are left untouched.
pub fn install_into(settings: &mut Value, command: &str) {
    uninstall_from(settings);
    if !settings.is_object() {
        *settings = json!({});
    }
    let hooks = settings
        .as_object_mut()
        .expect("object ensured above")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("object ensured above");
    for (event, matcher) in EVENTS {
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": command, "async": true, "timeout": 5 }]
        });
        if let Some(m) = matcher {
            entry["matcher"] = json!(m);
        }
        let list = hooks.entry(event).or_insert_with(|| json!([]));
        if let Some(arr) = list.as_array_mut() {
            arr.push(entry);
        }
    }
}

/// Remove our entries; drops event lists (and `hooks`) that end up empty.
/// Returns how many entries were removed.
pub fn uninstall_from(settings: &mut Value) -> usize {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return 0 };
    let mut removed = 0;
    for list in hooks.values_mut() {
        if let Some(arr) = list.as_array_mut() {
            let before = arr.len();
            arr.retain(|e| !is_ours(e));
            removed += before - arr.len();
        }
    }
    hooks.retain(|_, list| list.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty()
        && let Some(obj) = settings.as_object_mut()
    {
        obj.remove("hooks");
    }
    removed
}

pub fn has_hooks_installed(settings: &Value) -> bool {
    settings
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|h| h.values().filter_map(Value::as_array).flatten().any(is_ours))
}

/// Read, transform, back up and atomically rewrite settings.json.
pub fn edit_settings(path: &Path, edit: impl FnOnce(&mut Value)) -> anyhow::Result<()> {
    use anyhow::Context;
    let original = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::from("{}"),
        Err(e) => return Err(e).context(format!("reading {}", path.display())),
    };
    let mut settings: Value = serde_json::from_str(&original)
        .with_context(|| format!("{} is not valid JSON; not touching it", path.display()))?;
    edit(&mut settings);
    if path.exists() {
        fs::write(path.with_extension("json.bak-agent-tree"), &original)
            .context("writing backup")?;
    }
    let tmp = path.with_extension("json.tmp-agent-tree");
    fs::write(&tmp, serde_json::to_string_pretty(&settings)? + "\n")?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// TUI side: tail-follow the event log.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    Working,
    /// A permission / elicitation dialog is open.
    NeedsPermission,
    /// Turn finished (Stop) or idle prompt: waiting for the user.
    AwaitingInput,
    Ended,
}

#[derive(Debug, Clone)]
pub struct HookSession {
    pub state: HookState,
    /// Tool one-liner while a tool runs or awaits permission.
    pub detail: Option<String>,
    pub tmux_pane: Option<String>,
    pub tmux_socket: Option<String>,
    pub cwd: Option<String>,
    pub at: SystemTime,
}

#[derive(Debug, Default)]
pub struct HookTracker {
    path: Option<PathBuf>,
    offset: u64,
    pub sessions: HashMap<String, HookSession>,
}

impl HookTracker {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut tracker = Self { path, offset: 0, sessions: HashMap::new() };
        tracker.refresh();
        tracker
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Have we ever seen an event? Used to hint at `--install-hooks`.
    pub fn active(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Consume appended complete lines; a partial last line is re-read next time.
    pub fn refresh(&mut self) {
        let Some(path) = &self.path else { return };
        let Ok(mut file) = fs::File::open(path) else { return };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0; // rotated
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            return;
        }
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
        self.offset += end as u64 + 1;
        for line in String::from_utf8_lossy(&buf[..end]).lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                self.apply(&value);
            }
        }
    }

    fn apply(&mut self, record: &Value) {
        let s = |k: &str| record.get(k).and_then(Value::as_str).map(String::from);
        let Some(id) = s("session_id") else { return };
        let at = record
            .get("ts")
            .and_then(Value::as_i64)
            .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64))
            .unwrap_or_else(SystemTime::now);
        let entry = self.sessions.entry(id).or_insert(HookSession {
            state: HookState::AwaitingInput,
            detail: None,
            tmux_pane: None,
            tmux_socket: None,
            cwd: None,
            at,
        });
        entry.at = at;
        if let Some(pane) = s("tmux_pane") {
            entry.tmux_pane = Some(pane);
            entry.tmux_socket = s("tmux_socket");
        }
        if let Some(cwd) = s("cwd") {
            entry.cwd = Some(cwd);
        }
        let (state, detail) = match s("event").as_deref() {
            Some("SessionStart") | Some("Stop") => (HookState::AwaitingInput, None),
            Some("UserPromptSubmit") | Some("PostToolUse") => (HookState::Working, None),
            Some("PreToolUse") => (HookState::Working, s("tool")),
            Some("PermissionRequest") => (HookState::NeedsPermission, s("tool")),
            Some("Notification") => match s("notification_type").as_deref() {
                Some("permission_prompt") | Some("elicitation_dialog") => {
                    // Keep the tool from PermissionRequest; it says more than
                    // the generic "Claude needs your permission" message.
                    let detail = entry.detail.clone().or_else(|| s("message"));
                    (HookState::NeedsPermission, detail)
                }
                Some("idle_prompt") => (HookState::AwaitingInput, None),
                _ => return, // auth etc.: no state change
            },
            Some("SessionEnd") => (HookState::Ended, None),
            _ => return, // SubagentStop & unknown: only the timestamp moves
        };
        entry.state = state;
        entry.detail = detail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_record_is_compact_and_captures_tmux() {
        let input = json!({
            "session_id": "s1", "hook_event_name": "PreToolUse", "cwd": "/w",
            "tool_name": "Bash", "tool_input": {"command": "cargo test", "description": "x"},
            "transcript_path": "/t.jsonl"
        });
        let rec = hook_record(&input, Some("%12".into()), Some("/tmp/tmux-501/default,123,0".into()), 7);
        assert_eq!(rec["event"], "PreToolUse");
        assert_eq!(rec["tmux_pane"], "%12");
        assert_eq!(rec["tmux_socket"], "/tmp/tmux-501/default");
        // Same one-liner as the timeline: Bash prefers its description.
        assert_eq!(rec["tool"], "Bash: x");
        assert!(rec.get("tool_input").is_none(), "inputs are not persisted");
        // Outside tmux both fields stay null.
        let rec = hook_record(&input, None, None, 7);
        assert!(rec["tmux_pane"].is_null());
    }

    #[test]
    fn install_is_idempotent_and_uninstall_keeps_foreign_hooks() {
        let mut settings = json!({
            "model": "x",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "agent-deck hook-handler" }] }] }
        });
        let cmd = hook_command(Path::new("/bin/claude-agent-tree"));
        install_into(&mut settings, &cmd);
        install_into(&mut settings, &cmd); // re-install replaces, never duplicates
        assert!(has_hooks_installed(&settings));
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert_eq!(settings["hooks"]["Stop"][1]["hooks"][0]["async"], true);
        assert!(settings["hooks"]["Stop"][1].get("matcher").is_none());

        assert_eq!(uninstall_from(&mut settings), EVENTS.len());
        assert!(!has_hooks_installed(&settings));
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1, "foreign hook kept");
        assert!(settings["hooks"].get("PreToolUse").is_none(), "emptied lists dropped");
        assert_eq!(settings["model"], "x");
    }

    #[test]
    fn hook_command_quotes_paths() {
        assert_eq!(
            hook_command(Path::new("/a b/it's/claude-agent-tree")),
            r"'/a b/it'\''s/claude-agent-tree' --hook 2>/dev/null || true"
        );
    }

    #[test]
    fn tracker_follows_state_transitions_and_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = |event: &str, extra: &str| {
            format!(r#"{{"ts":1,"event":"{event}","session_id":"s1","tmux_pane":"%3"{extra}}}"#)
        };
        fs::write(&path, format!("{}\n{}\n", line("UserPromptSubmit", ""), line("PreToolUse", r#","tool":"Bash ls""#))).unwrap();
        let mut t = HookTracker::new(Some(path.clone()));
        let s = &t.sessions["s1"];
        assert_eq!(s.state, HookState::Working);
        assert_eq!(s.detail.as_deref(), Some("Bash ls"));
        assert_eq!(s.tmux_pane.as_deref(), Some("%3"));

        // Permission request, then the matching notification keeps the tool.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", line("PermissionRequest", r#","tool":"Bash rm x""#)).unwrap();
        write!(f, "{}", line("Notification", r#","notification_type":"permission_prompt","message":"needs permission""#)).unwrap();
        t.refresh();
        assert_eq!(t.sessions["s1"].state, HookState::NeedsPermission);
        // The unterminated notification line is not consumed yet.
        writeln!(f).unwrap();
        writeln!(f, "{}", line("SubagentStop", "")).unwrap();
        t.refresh();
        assert_eq!(t.sessions["s1"].detail.as_deref(), Some("Bash rm x"));
        writeln!(f, "{}", line("Stop", "")).unwrap();
        t.refresh();
        assert_eq!(t.sessions["s1"].state, HookState::AwaitingInput);
        writeln!(f, "{}", line("SessionEnd", "")).unwrap();
        t.refresh();
        assert_eq!(t.sessions["s1"].state, HookState::Ended);
    }

    #[test]
    fn tracker_survives_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        fs::write(&path, r#"{"ts":1,"event":"UserPromptSubmit","session_id":"a"}"#.to_string() + "\n").unwrap();
        let mut t = HookTracker::new(Some(path.clone()));
        fs::write(&path, r#"{"ts":2,"event":"Stop","session_id":"a"}"#.to_string() + "\n").unwrap();
        t.refresh(); // shorter file ⇒ re-read from 0
        assert_eq!(t.sessions["a"].state, HookState::AwaitingInput);
    }
}
