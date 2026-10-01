//! Cheap "what is running right now" snapshot of one project, for the Browse
//! view's live agent graph.
//!
//! Like the index this never parses whole transcripts: subagents come from
//! their `agent-*.meta.json` sidecars plus transcript mtimes, and "finished"
//! is read off the last line of the transcript (`stop_reason: end_turn`).
//! Nesting (spawnDepth > 1) is resolved by finding the parent transcript that
//! contains the spawning `toolUseId`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::index::{ProjectEntry, SessionMeta};
use crate::model::SubagentMeta;

/// Sessions written to within this window show up in the live graph.
pub const LIVE_WINDOW: Duration = Duration::from_secs(300);
/// Parent lookup reads whole subagent transcripts; skip pathological ones.
const PARENT_SCAN_MAX: u64 = 8 * 1024 * 1024;
const TAIL_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone)]
pub struct LiveAgent {
    #[allow(dead_code)] // identity; only asserted on in tests so far
    pub agent_id: String,
    pub agent_type: String,
    pub description: String,
    pub mtime: SystemTime,
    /// Last transcript entry is an assistant `end_turn`: the agent reported back.
    pub finished: bool,
    pub children: Vec<LiveAgent>,
}

/// Liveness of one subagent, derived at draw time so it decays on the 1 s tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Transcript written seconds ago.
    Running,
    /// Not finished and written recently: most likely inside a long tool call.
    Waiting,
    Done,
    /// Not finished but long quiet: interrupted or killed.
    Stopped,
}

impl LiveAgent {
    pub fn state(&self, fresh_window: Duration) -> AgentState {
        let age = self.mtime.elapsed().unwrap_or_default();
        if self.finished {
            AgentState::Done
        } else if age < fresh_window {
            AgentState::Running
        } else if age < LIVE_WINDOW {
            AgentState::Waiting
        } else {
            AgentState::Stopped
        }
    }

    /// This agent or any descendant is still going.
    pub fn is_active(&self, fresh_window: Duration) -> bool {
        matches!(self.state(fresh_window), AgentState::Running | AgentState::Waiting)
            || self.children.iter().any(|c| c.is_active(fresh_window))
    }
}

#[derive(Debug, Clone)]
pub struct LiveSession {
    pub session_id: String,
    pub agents: Vec<LiveAgent>,
}

/// Recent sessions of `project` (newest first) with their subagent trees.
pub fn snapshot(project: &ProjectEntry) -> Vec<LiveSession> {
    project
        .sessions
        .iter()
        .filter(|s| is_recent(s))
        .map(|s| LiveSession { session_id: s.id.clone(), agents: subagent_tree(s) })
        .collect()
}

pub fn is_recent(session: &SessionMeta) -> bool {
    let recent = |t: SystemTime| t.elapsed().map(|d| d < LIVE_WINDOW).unwrap_or(false);
    recent(session.mtime) || session.subagent_mtime.is_some_and(recent)
}

struct Flat {
    agent: LiveAgent,
    path: PathBuf,
    depth: u32,
    tool_use_id: Option<String>,
}

fn subagent_tree(session: &SessionMeta) -> Vec<LiveAgent> {
    let Some(dir) = session.path.parent().map(|p| p.join(&session.id).join("subagents")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut flat: Vec<Flat> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(agent_id) = name.strip_prefix("agent-").and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let path = entry.path();
        let meta = fs::read_to_string(dir.join(format!("agent-{agent_id}.meta.json")))
            .ok()
            .and_then(|s| serde_json::from_str::<SubagentMeta>(&s).ok());
        let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        flat.push(Flat {
            agent: LiveAgent {
                agent_id: agent_id.to_string(),
                agent_type: meta
                    .as_ref()
                    .map(|m| m.agent_type.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "agent".into()),
                description: meta.as_ref().map(|m| m.description.clone()).unwrap_or_default(),
                mtime,
                finished: ends_with_end_turn(&path),
                children: Vec::new(),
            },
            path,
            depth: meta.as_ref().map_or(1, |m| m.spawn_depth.max(1)),
            tool_use_id: meta.and_then(|m| m.tool_use_id),
        });
    }

    // parent[i] = index of the transcript that issued agent i's spawn.
    let parent: Vec<Option<usize>> = (0..flat.len())
        .map(|i| {
            let tid = flat[i].tool_use_id.as_deref().filter(|_| flat[i].depth > 1)?;
            let needle = format!("\"{tid}\"");
            (0..flat.len()).find(|&j| {
                j != i && flat[j].depth + 1 == flat[i].depth && file_contains(&flat[j].path, &needle)
            })
        })
        .collect();

    // Attach deepest first so a node is complete before its parent takes it.
    let mut order: Vec<usize> = (0..flat.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(flat[i].depth));
    let mut slots: Vec<Option<LiveAgent>> = flat.into_iter().map(|f| Some(f.agent)).collect();
    let mut roots = Vec::new();
    for i in order {
        let Some(mut node) = slots[i].take() else { continue };
        sort_agents(&mut node.children);
        match parent[i].and_then(|p| slots[p].as_mut()) {
            Some(p) => p.children.push(node),
            None => roots.push(node),
        }
    }
    sort_agents(&mut roots);
    roots
}

/// Oldest first, like the detail view's agent tree (mtime ≈ last activity, so
/// still-running agents naturally sink to the bottom, next to "now").
fn sort_agents(agents: &mut [LiveAgent]) {
    agents.sort_by_key(|a| a.mtime);
}

fn file_contains(path: &Path, needle: &str) -> bool {
    if fs::metadata(path).map(|m| m.len() > PARENT_SCAN_MAX).unwrap_or(true) {
        return false;
    }
    fs::read(path).is_ok_and(|bytes| {
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

/// Is the last complete JSONL line an assistant message with `end_turn`?
fn ends_with_end_turn(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = fs::File::open(path) else { return false };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES))).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&buf);
    let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else { return false };
    let Ok(value) = serde_json::from_str::<Value>(last) else { return false };
    value.get("type").and_then(Value::as_str) == Some("assistant")
        && value.pointer("/message/stop_reason").and_then(Value::as_str) == Some("end_turn")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DONE: &str = r#"{"uuid":"x","type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"report"}]}}"#;
    const BUSY: &str = r#"{"uuid":"y","type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_child","name":"Agent","input":{}}]}}"#;

    fn agent(dir: &Path, id: &str, meta: &str, body: &str) {
        fs::write(dir.join(format!("agent-{id}.jsonl")), format!("{body}\n")).unwrap();
        fs::write(dir.join(format!("agent-{id}.meta.json")), meta).unwrap();
    }

    #[test]
    fn builds_nested_tree_and_detects_finished() {
        let root = tempfile::tempdir().unwrap();
        let session_path = root.path().join("s1.jsonl");
        fs::write(&session_path, "{}\n").unwrap();
        let sub = root.path().join("s1").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        agent(&sub, "a", r#"{"agentType":"Explore","description":"look","toolUseId":"toolu_a","spawnDepth":1}"#, BUSY);
        agent(&sub, "b", r#"{"agentType":"Plan","description":"plan","toolUseId":"toolu_b","spawnDepth":1}"#, DONE);
        agent(&sub, "c", r#"{"agentType":"general","description":"nested","toolUseId":"toolu_child","spawnDepth":2}"#, DONE);

        let meta = SessionMeta::tail_scan(&session_path).unwrap();
        let tree = subagent_tree(&meta);
        assert_eq!(tree.len(), 2);
        let a = tree.iter().find(|n| n.agent_id == "a").unwrap();
        assert!(!a.finished);
        assert_eq!(a.children.len(), 1);
        assert_eq!(a.children[0].agent_id, "c");
        assert!(a.children[0].finished);
        // Freshly written and unfinished ⇒ running; the nested one is done.
        assert_eq!(a.state(Duration::from_secs(10)), AgentState::Running);
        assert_eq!(a.children[0].state(Duration::from_secs(10)), AgentState::Done);
        assert!(tree.iter().find(|n| n.agent_id == "b").unwrap().finished);
    }

    #[test]
    fn orphaned_nested_agent_becomes_root() {
        let root = tempfile::tempdir().unwrap();
        let session_path = root.path().join("s2.jsonl");
        fs::write(&session_path, "{}\n").unwrap();
        let sub = root.path().join("s2").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        agent(&sub, "x", r#"{"agentType":"general","toolUseId":"toolu_missing","spawnDepth":2}"#, DONE);
        let meta = SessionMeta::tail_scan(&session_path).unwrap();
        let tree = subagent_tree(&meta);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].agent_type, "general");
    }
}
