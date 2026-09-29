//! Agent flow tree: main agent at the root, one node per subagent, built from
//! transcripts + spawn records, with ranges into the merged timeline.

use std::collections::HashMap;

use jiff::Timestamp;

use crate::session::{SpawnRecord, TimelineEvent, ToolStatus, TranscriptState};

#[derive(Debug, Clone)]
pub struct AgentNode {
    /// None = main agent.
    pub agent_id: Option<String>,
    pub agent_type: String,
    pub description: String,
    pub status: ToolStatus,
    pub started: Option<Timestamp>,
    pub finished: Option<Timestamp>,
    /// First/last index of this agent's own events in the merged timeline.
    pub event_range: Option<(usize, usize)>,
    /// From the spawn's tool result: the model the agent actually ran on.
    pub resolved_model: Option<String>,
    /// Spawn prompt (capped copy from the spawn record).
    pub prompt: String,
    /// Path to the agent's /tmp output transcript, if reported.
    pub output_file: Option<String>,
    pub is_async: bool,
    pub children: Vec<AgentNode>,
}

impl AgentNode {
    pub fn main() -> Self {
        Self {
            agent_id: None,
            agent_type: "main".into(),
            description: String::new(),
            status: ToolStatus::Ok,
            started: None,
            finished: None,
            event_range: None,
            resolved_model: None,
            prompt: String::new(),
            output_file: None,
            is_async: false,
            children: Vec::new(),
        }
    }

    pub fn duration(&self) -> Option<jiff::SignedDuration> {
        Some(self.finished?.duration_since(self.started?))
    }
}

pub fn build_agent_tree(
    transcripts: &[TranscriptState],
    paths: &[Vec<String>],
    timeline: &[TimelineEvent],
    spawns: &HashMap<String, SpawnRecord>,
) -> AgentNode {
    // Own-event ranges per agent path.
    let mut range_of: HashMap<&[String], (usize, usize)> = HashMap::new();
    for (idx, event) in timeline.iter().enumerate() {
        range_of
            .entry(event.agent_path.as_slice())
            .and_modify(|(_, last)| *last = idx)
            .or_insert((idx, idx));
    }

    let mut root = AgentNode::main();
    root.started = timeline.iter().find_map(|e| e.timestamp);
    root.finished = timeline.iter().rev().find_map(|e| e.timestamp);
    root.event_range = if timeline.is_empty() { None } else { Some((0, timeline.len() - 1)) };

    // Build one node per subagent transcript, then attach by path depth.
    let mut nodes: Vec<(Vec<String>, AgentNode)> = Vec::new();
    for (i, t) in transcripts.iter().enumerate() {
        let Some(agent_id) = &t.agent_id else { continue };
        let spawn = t
            .meta
            .as_ref()
            .and_then(|m| m.tool_use_id.as_ref())
            .and_then(|tid| spawns.get(tid))
            .or_else(|| {
                spawns.values().find(|s| s.result_agent_id.as_deref() == Some(agent_id.as_str()))
            });
        let own_range = range_of.get(paths[i].as_slice()).copied();
        let started = t.entries.first().and_then(|e| e.parsed_timestamp());
        // Async agents report their tool result immediately ("async_launched"),
        // so the later of result time and last transcript entry is the real end.
        let last_entry = t.entries.last().and_then(|e| e.parsed_timestamp());
        let finished = match (spawn.and_then(|s| s.finished), last_entry) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let status = spawn.map_or(ToolStatus::Pending, |s| s.status);
        nodes.push((
            paths[i].clone(),
            AgentNode {
                agent_id: Some(agent_id.clone()),
                agent_type: t
                    .meta
                    .as_ref()
                    .map(|m| m.agent_type.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "agent".into()),
                description: t
                    .meta
                    .as_ref()
                    .map(|m| m.description.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| spawn.map(|s| s.description.clone()))
                    .unwrap_or_default(),
                status,
                started,
                finished: if status == ToolStatus::Pending { None } else { finished },
                event_range: own_range,
                resolved_model: spawn.and_then(|s| s.resolved_model.clone()),
                prompt: spawn.map(|s| s.prompt.clone()).unwrap_or_default(),
                output_file: spawn.and_then(|s| s.output_file.clone()),
                is_async: spawn.is_some_and(|s| s.is_async),
                children: Vec::new(),
            },
        ));
    }

    // Deepest first so children exist before their parents collect them.
    nodes.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    let mut pending: Vec<(Vec<String>, AgentNode)> = Vec::new();
    for (path, node) in nodes {
        let mut node = node;
        // Adopt already-processed nodes whose parent path is this node's path.
        let (mine, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(p, _)| p.len() == path.len() + 1 && p.starts_with(&path));
        for (_, child) in mine {
            node.children.push(child);
        }
        pending = rest;
        pending.push((path, node));
    }
    for (_, node) in pending {
        root.children.push(node);
    }
    sort_children(&mut root);
    root
}

fn sort_children(node: &mut AgentNode) {
    node.children.sort_by_key(|c| c.started);
    for child in &mut node.children {
        sort_children(child);
    }
}

/// One row of the rendered agent graph.
#[derive(Debug, Clone)]
pub struct FlatAgentRow {
    /// Box-drawing prefix, e.g. `"│ └─"`.
    pub prefix: String,
    pub agent_id: Option<String>,
    pub agent_type: String,
    pub description: String,
    pub status: ToolStatus,
    pub started: Option<Timestamp>,
    pub finished: Option<Timestamp>,
    pub duration: Option<jiff::SignedDuration>,
    pub event_range: Option<(usize, usize)>,
    pub resolved_model: Option<String>,
    pub prompt: String,
    pub output_file: Option<String>,
    pub is_async: bool,
}

impl FlatAgentRow {
    fn from_node(node: &AgentNode, prefix: String) -> Self {
        Self {
            prefix,
            agent_id: node.agent_id.clone(),
            agent_type: node.agent_type.clone(),
            description: node.description.clone(),
            status: node.status,
            started: node.started,
            finished: node.finished,
            duration: node.duration(),
            event_range: node.event_range,
            resolved_model: node.resolved_model.clone(),
            prompt: node.prompt.clone(),
            output_file: node.output_file.clone(),
            is_async: node.is_async,
        }
    }
}

/// Flatten the tree into rows with box-drawing prefixes for the graph pane.
pub fn flatten(root: &AgentNode) -> Vec<FlatAgentRow> {
    let mut rows = vec![FlatAgentRow::from_node(root, String::new())];
    flatten_into(&root.children, "", &mut rows);
    rows
}

fn flatten_into(children: &[AgentNode], indent: &str, rows: &mut Vec<FlatAgentRow>) {
    for (i, child) in children.iter().enumerate() {
        let last = i == children.len() - 1;
        rows.push(FlatAgentRow::from_node(
            child,
            format!("{indent}{}", if last { "└─" } else { "├─" }),
        ));
        let next_indent = format!("{indent}{}", if last { "  " } else { "│ " });
        flatten_into(&child.children, &next_indent, rows);
    }
}
