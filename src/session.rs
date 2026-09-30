//! Full session load: collapse JSONL entries into displayable timeline events,
//! join tool_use ↔ tool_result, merge subagent transcripts, build the agent tree.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::Value;

use crate::agent_tree::{build_agent_tree, AgentNode};
use crate::index::{extract_message_text, SessionMeta};
use crate::model::{ConvEntry, CostState, RawLine, SubagentMeta};
use crate::parser;

const INPUT_CAP: usize = 4 * 1024;
const OUTPUT_CAP: usize = 8 * 1024;
const TEXT_CAP: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Ok,
    Error,
    Denied,
    /// No result yet — live session or dropped entry.
    Pending,
}

#[derive(Debug, Clone, Default)]
pub struct ToolDetail {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // tool_use_id is kept for future cross-referencing
pub enum EventKind {
    UserPrompt {
        text: String,
    },
    AssistantText {
        text: String,
    },
    /// Extended-thinking block; hidden by default, toggled with `T`.
    Thinking {
        text: String,
    },
    ToolCall {
        name: String,
        one_liner: String,
        status: ToolStatus,
        detail: ToolDetail,
        tool_use_id: String,
    },
    SubagentSpawn {
        /// Filled once the meta.json or tool result reveals the agent id.
        agent_id: Option<String>,
        agent_type: String,
        description: String,
        tool_use_id: String,
        status: ToolStatus,
        prompt: String,
    },
    SystemNote {
        text: String,
    },
}

#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub timestamp: Option<Timestamp>,
    /// [] = main agent, ["a1"] = subagent a1, ["a1","a2"] = nested.
    pub agent_path: Vec<String>,
    pub kind: EventKind,
    /// (transcript idx, byte start, byte len) of the source lines backing this
    /// event; lets `full_event_content` re-read them uncapped from disk.
    pub sources: Vec<(usize, u64, u64)>,
}

/// One parsed transcript file (main session or a subagent) kept in memory so an
/// incremental reload only parses appended bytes and then rebuilds cheaply.
#[derive(Debug)]
pub struct TranscriptState {
    pub path: PathBuf,
    /// None = main transcript.
    pub agent_id: Option<String>,
    pub meta: Option<SubagentMeta>,
    pub entries: Vec<ConvEntry>,
    pub offset: u64,
    pub parse_errors: usize,
}

#[derive(Debug)]
pub struct LoadedSession {
    pub meta: SessionMeta,
    pub transcripts: Vec<TranscriptState>,
    pub timeline: Vec<TimelineEvent>,
    pub agent_tree: AgentNode,
    pub cost: Option<CostState>,
    pub parse_errors: usize,
}

impl LoadedSession {
    pub fn load(meta: SessionMeta) -> std::io::Result<Self> {
        let mut session = Self {
            meta,
            transcripts: Vec::new(),
            timeline: Vec::new(),
            agent_tree: AgentNode::main(),
            cost: None,
            parse_errors: 0,
        };
        session.refresh()?;
        Ok(session)
    }

    /// Incremental reload: parse appended bytes of known transcripts, pick up new
    /// subagent files, then rebuild timeline + tree from in-memory entries.
    pub fn refresh(&mut self) -> std::io::Result<()> {
        if self.transcripts.is_empty() {
            self.transcripts.push(TranscriptState {
                path: self.meta.path.clone(),
                agent_id: None,
                meta: None,
                entries: Vec::new(),
                offset: 0,
                parse_errors: 0,
            });
        }
        self.discover_subagents();
        let mut bookkeeping = Vec::new();
        for t in &mut self.transcripts {
            let size = fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
            if size < t.offset {
                // File shrank (rewrite): full reload of this transcript.
                t.entries.clear();
                t.offset = 0;
                t.parse_errors = 0;
            }
            if size == t.offset {
                continue;
            }
            let (lines, offset, errors) = parser::read_all(&t.path, t.offset)?;
            t.offset = offset;
            t.parse_errors += errors;
            let is_main = t.agent_id.is_none();
            for (line, (src_start, src_len)) in lines {
                match line {
                    RawLine::Conversation(mut entry) => {
                        entry.src_start = src_start;
                        entry.src_len = src_len;
                        t.entries.push(*entry);
                    }
                    RawLine::Bookkeeping { kind, value } => {
                        if is_main {
                            bookkeeping.push((kind, value));
                        }
                    }
                }
            }
        }
        for (kind, value) in bookkeeping {
            self.apply_bookkeeping(&kind, value);
        }
        self.parse_errors = self.transcripts.iter().map(|t| t.parse_errors).sum();
        self.rebuild();
        Ok(())
    }

    fn apply_bookkeeping(&mut self, kind: &str, value: Value) {
        match kind {
            "cost-state" => {
                if let Ok(cost) = serde_json::from_value::<CostState>(value) {
                    self.cost = Some(cost);
                }
            }
            "ai-title" => {
                if let Some(title) = value.get("aiTitle").and_then(Value::as_str) {
                    self.meta.title = title.to_string();
                }
            }
            _ => {}
        }
    }

    fn discover_subagents(&mut self) {
        let dir = match self.meta.path.parent() {
            Some(p) => p.join(&self.meta.id).join("subagents"),
            None => return,
        };
        let Ok(entries) = fs::read_dir(&dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            let Some(agent_id) = name.strip_prefix("agent-").and_then(|n| n.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if self.transcripts.iter().any(|t| t.path == path) {
                continue;
            }
            let meta_path = dir.join(format!("agent-{agent_id}.meta.json"));
            let meta = fs::read_to_string(&meta_path)
                .ok()
                .and_then(|s| serde_json::from_str::<SubagentMeta>(&s).ok());
            self.transcripts.push(TranscriptState {
                path,
                agent_id: Some(agent_id.to_string()),
                meta,
                entries: Vec::new(),
                offset: 0,
                parse_errors: 0,
            });
        }
    }

    /// Rebuild timeline and agent tree from the in-memory transcript entries.
    fn rebuild(&mut self) {
        // 1. Per-transcript events + spawn records (Agent tool calls).
        let mut per_transcript: Vec<Vec<TimelineEvent>> = Vec::new();
        let mut spawns: HashMap<String, SpawnRecord> = HashMap::new(); // tool_use_id → record
        for (t_idx, t) in self.transcripts.iter().enumerate() {
            let events = build_events(&t.entries, t_idx, &mut spawns);
            per_transcript.push(events);
        }

        // 2. Resolve each subagent's parent transcript via its meta.toolUseId
        //    (primary) or the agentId reported in the spawn's tool result.
        let mut parent_of: HashMap<String, usize> = HashMap::new(); // agent_id → transcript idx
        for t in &self.transcripts {
            let Some(agent_id) = &t.agent_id else { continue };
            let tool_use_id = t
                .meta
                .as_ref()
                .and_then(|m| m.tool_use_id.clone())
                .or_else(|| {
                    spawns
                        .iter()
                        .find(|(_, s)| s.result_agent_id.as_deref() == Some(agent_id.as_str()))
                        .map(|(id, _)| id.clone())
                });
            let record = tool_use_id.as_ref().and_then(|tid| spawns.get(tid));
            parent_of.insert(agent_id.clone(), record.map_or(0, |r| r.transcript_idx));
            // Backfill the spawn event with the resolved agent id / meta info.
            if let Some(record) = record {
                let event = &mut per_transcript[record.transcript_idx][record.event_idx];
                if let EventKind::SubagentSpawn { agent_id: slot, agent_type, .. } = &mut event.kind
                {
                    *slot = Some(agent_id.clone());
                    if let Some(meta) = &t.meta
                        && !meta.agent_type.is_empty() {
                            *agent_type = meta.agent_type.clone();
                        }
                }
            }
        }

        // 3. agent_path per transcript (walk parent chain; depth is tiny).
        let mut paths: Vec<Vec<String>> = vec![Vec::new(); self.transcripts.len()];
        for (i, t) in self.transcripts.iter().enumerate() {
            let Some(agent_id) = &t.agent_id else { continue };
            let mut path = vec![agent_id.clone()];
            let mut current = parent_of.get(agent_id).copied().unwrap_or(0);
            let mut hops = 0;
            while let Some(pid) = self.transcripts[current].agent_id.clone() {
                path.push(pid.clone());
                current = parent_of.get(&pid).copied().unwrap_or(0);
                hops += 1;
                if hops > 32 {
                    break; // cycle guard for corrupt data
                }
            }
            path.reverse();
            paths[i] = path;
        }
        for (t_idx, events) in per_transcript.iter_mut().enumerate() {
            for event in events.iter_mut() {
                event.agent_path = paths[t_idx].clone();
            }
        }

        // 4. Merge chronologically. Stable sort keeps intra-file order for
        //    entries with equal or missing timestamps.
        let mut timeline: Vec<TimelineEvent> = per_transcript.into_iter().flatten().collect();
        timeline.sort_by_key(|e| e.timestamp);
        self.timeline = timeline;

        // 5. Agent tree over the merged timeline.
        self.agent_tree = build_agent_tree(&self.transcripts, &paths, &self.timeline, &spawns);
    }
}

/// Where an `Agent` tool_use appeared and what its result reported.
#[derive(Debug)]
pub struct SpawnRecord {
    pub transcript_idx: usize,
    pub event_idx: usize,
    pub status: ToolStatus,
    pub result_agent_id: Option<String>,
    pub finished: Option<Timestamp>,
    /// Spawn prompt (capped like the event's copy).
    pub prompt: String,
    pub description: String,
    /// From the tool result: the model the subagent actually ran on.
    pub resolved_model: Option<String>,
    /// From the tool result: path to the subagent's transcript under /tmp.
    pub output_file: Option<String>,
    pub is_async: bool,
}

/// Collapse one transcript's conversation entries into timeline events.
fn build_events(
    entries: &[ConvEntry],
    transcript_idx: usize,
    spawns: &mut HashMap<String, SpawnRecord>,
) -> Vec<TimelineEvent> {
    let mut events: Vec<TimelineEvent> = Vec::new();
    // tool_use_id → index into `events`, for joining results.
    let mut open_calls: HashMap<String, usize> = HashMap::new();
    // message.id of the assistant text event currently being accumulated.
    let mut current_text: Option<(String, usize)> = None;
    // Same, for thinking blocks (they interleave with text in one message).
    let mut current_thinking: Option<(String, usize)> = None;

    for entry in entries {
        let ts = entry.parsed_timestamp();
        let src = (transcript_idx, entry.src_start, entry.src_len);
        match entry.entry_type.as_str() {
            "assistant" => {
                let Some(message) = &entry.message else { continue };
                let msg_id = message.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let Some(Value::Array(blocks)) = message.get("content") else { continue };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if text.trim().is_empty() {
                                continue;
                            }
                            // One API response is split across JSONL lines; glue
                            // text blocks of the same message.id back together.
                            match &current_text {
                                Some((id, idx)) if *id == msg_id => {
                                    let idx = *idx;
                                    if let EventKind::AssistantText { text: existing } =
                                        &mut events[idx].kind
                                    {
                                        existing.push_str("\n\n");
                                        existing.push_str(&cap(text, TEXT_CAP));
                                    }
                                    if events[idx].sources.last() != Some(&src) {
                                        events[idx].sources.push(src);
                                    }
                                }
                                _ => {
                                    events.push(TimelineEvent {
                                        timestamp: ts,
                                        agent_path: Vec::new(),
                                        kind: EventKind::AssistantText {
                                            text: cap(text, TEXT_CAP),
                                        },
                                        sources: vec![src],
                                    });
                                    current_text = Some((msg_id.clone(), events.len() - 1));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name =
                                block.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
                            let id =
                                block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            let kind = if name == "Agent" || name == "Task" {
                                let agent_type = input
                                    .get("subagent_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("general-purpose")
                                    .to_string();
                                let description = input
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let prompt = input
                                    .get("prompt")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                spawns.insert(
                                    id.clone(),
                                    SpawnRecord {
                                        transcript_idx,
                                        event_idx: events.len(),
                                        status: ToolStatus::Pending,
                                        result_agent_id: None,
                                        finished: None,
                                        prompt: cap(&prompt, INPUT_CAP),
                                        description: description.clone(),
                                        resolved_model: None,
                                        output_file: None,
                                        is_async: false,
                                    },
                                );
                                EventKind::SubagentSpawn {
                                    agent_id: None,
                                    agent_type,
                                    description,
                                    tool_use_id: id.clone(),
                                    status: ToolStatus::Pending,
                                    prompt: cap(&prompt, INPUT_CAP),
                                }
                            } else {
                                EventKind::ToolCall {
                                    one_liner: tool_one_liner(&name, &input),
                                    detail: ToolDetail {
                                        input: cap(
                                            &serde_json::to_string_pretty(&input)
                                                .unwrap_or_default(),
                                            INPUT_CAP,
                                        ),
                                        output: String::new(),
                                    },
                                    name,
                                    status: ToolStatus::Pending,
                                    tool_use_id: id.clone(),
                                }
                            };
                            if !id.is_empty() {
                                open_calls.insert(id, events.len());
                            }
                            events.push(TimelineEvent {
                                timestamp: ts,
                                agent_path: Vec::new(),
                                kind,
                                sources: vec![src],
                            });
                        }
                        Some("thinking") => {
                            let text =
                                block.get("thinking").and_then(Value::as_str).unwrap_or("");
                            if text.trim().is_empty() {
                                continue;
                            }
                            match &current_thinking {
                                Some((id, idx)) if *id == msg_id => {
                                    let idx = *idx;
                                    if let EventKind::Thinking { text: existing } =
                                        &mut events[idx].kind
                                    {
                                        existing.push_str("\n\n");
                                        existing.push_str(&cap(text, TEXT_CAP));
                                    }
                                    if events[idx].sources.last() != Some(&src) {
                                        events[idx].sources.push(src);
                                    }
                                }
                                _ => {
                                    events.push(TimelineEvent {
                                        timestamp: ts,
                                        agent_path: Vec::new(),
                                        kind: EventKind::Thinking { text: cap(text, TEXT_CAP) },
                                        sources: vec![src],
                                    });
                                    current_thinking = Some((msg_id.clone(), events.len() - 1));
                                }
                            }
                        }
                        _ => {} // images, redacted_thinking, ...
                    }
                }
            }
            "user" => {
                let Some(message) = &entry.message else { continue };
                let results = tool_results(message);
                if !results.is_empty() {
                    for (tool_use_id, content, is_error) in results {
                        let status = if entry.tool_denial_kind.is_some() {
                            ToolStatus::Denied
                        } else if is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        };
                        if let Some(record) = spawns.get_mut(&tool_use_id) {
                            record.status = status;
                            record.finished = ts;
                            if let Some(rich) = entry.tool_use_result.as_ref() {
                                let get =
                                    |k: &str| rich.get(k).and_then(Value::as_str).map(String::from);
                                record.result_agent_id = get("agentId");
                                record.resolved_model = get("resolvedModel");
                                record.output_file = get("outputFile");
                                record.is_async =
                                    rich.get("isAsync").and_then(Value::as_bool).unwrap_or(false);
                            }
                        }
                        let Some(&event_idx) = open_calls.get(&tool_use_id) else { continue };
                        match &mut events[event_idx].kind {
                            EventKind::ToolCall { status: slot, detail, .. } => {
                                *slot = status;
                                detail.output = render_tool_output(
                                    entry.tool_use_result.as_ref(),
                                    &content,
                                    OUTPUT_CAP,
                                );
                            }
                            EventKind::SubagentSpawn { status: slot, .. } => *slot = status,
                            _ => {}
                        }
                        events[event_idx].sources.push(src);
                    }
                    continue;
                }
                if entry.is_meta {
                    continue;
                }
                if let Some(text) = extract_message_text(message) {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // Synthetic messages (task notifications, command wrappers)
                    // read better as system notes than as human prompts.
                    let kind = if trimmed.starts_with('<') || trimmed.starts_with("[SYSTEM") {
                        EventKind::SystemNote { text: cap(trimmed, INPUT_CAP) }
                    } else {
                        EventKind::UserPrompt { text: cap(trimmed, TEXT_CAP) }
                    };
                    events.push(TimelineEvent {
                        timestamp: ts,
                        agent_path: Vec::new(),
                        kind,
                        sources: vec![src],
                    });
                }
            }
            "system" => {
                if entry.subtype.as_deref() == Some("away_summary")
                    && let Some(text) = entry.content.as_ref().and_then(Value::as_str) {
                        events.push(TimelineEvent {
                            timestamp: ts,
                            agent_path: Vec::new(),
                            kind: EventKind::SystemNote { text: cap(text, INPUT_CAP) },
                            sources: vec![src],
                        });
                    }
            }
            _ => {} // attachment and friends: noise, skipped
        }
        if entry.entry_type != "assistant" {
            current_text = None;
            current_thinking = None;
        }
    }
    events
}

/// (tool_use_id, model-visible content, is_error) triples of a user message.
fn tool_results(message: &Value) -> Vec<(String, String, bool)> {
    let Some(Value::Array(blocks)) = message.get("content") else { return Vec::new() };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|b| {
            let id = b.get("tool_use_id").and_then(Value::as_str)?.to_string();
            let content = match b.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                || content.trim_start().starts_with("Error:");
            Some((id, content, is_error))
        })
        .collect()
}

/// Prefer the rich structured result over the model-visible string.
/// `max` caps the output; `usize::MAX` disables the cap.
fn render_tool_output(rich: Option<&Value>, fallback: &str, max: usize) -> String {
    let Some(rich) = rich else { return cap(fallback, max) };
    match rich {
        Value::String(s) => cap(s, max),
        Value::Object(map) => {
            // Edit-shaped results render as a unified diff; Bash-shaped results
            // as stdout/stderr; anything else pretty-prints.
            if let Some(diff) = structured_patch_text(map) {
                cap(&diff, max)
            } else if map.contains_key("stdout") || map.contains_key("stderr") {
                let stdout = map.get("stdout").and_then(Value::as_str).unwrap_or("");
                let stderr = map.get("stderr").and_then(Value::as_str).unwrap_or("");
                let mut out = String::new();
                if !stdout.trim().is_empty() {
                    out.push_str(stdout);
                }
                if !stderr.trim().is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n--- stderr ---\n");
                    }
                    out.push_str(stderr);
                }
                cap(&out, max)
            } else {
                cap(&serde_json::to_string_pretty(rich).unwrap_or_default(), max)
            }
        }
        _ => cap(fallback, max),
    }
}

/// The selected event's content, uncapped: re-read the backing lines from disk
/// via their byte spans. Falls back to the capped in-memory content if any span
/// can no longer be read (file rewritten since the last rebuild).
pub fn full_event_content(session: &LoadedSession, event: &TimelineEvent) -> String {
    let fallback = || capped_event_content(event);
    if event.sources.is_empty() {
        return fallback();
    }
    let mut out = String::new();
    for &(t_idx, start, len) in &event.sources {
        let Some(t) = session.transcripts.get(t_idx) else { return fallback() };
        let Some(entry) = read_entry_at(&t.path, start, len) else { return fallback() };
        append_full_entry(&mut out, &entry, &event.kind);
    }
    if out.trim().is_empty() {
        fallback()
    } else {
        out
    }
}

/// Re-read and parse one JSONL line by its byte span.
fn read_entry_at(path: &std::path::Path, start: u64, len: u64) -> Option<ConvEntry> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    match crate::model::parse_line(text.trim()).ok()? {
        RawLine::Conversation(entry) => Some(*entry),
        _ => None,
    }
}

/// Append one re-read entry's content, uncapped, shaped for the event kind.
fn append_full_entry(out: &mut String, entry: &ConvEntry, kind: &EventKind) {
    let push_section = |out: &mut String, text: &str| {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(text);
    };
    match kind {
        EventKind::Thinking { .. } => {
            let Some(Value::Array(blocks)) = entry.message.as_ref().and_then(|m| m.get("content"))
            else {
                return;
            };
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
                .filter_map(|b| b.get("thinking").and_then(Value::as_str))
                .collect();
            if !text.is_empty() {
                push_section(out, text.join("\n\n").trim());
            }
        }
        EventKind::UserPrompt { .. }
        | EventKind::AssistantText { .. }
        | EventKind::SystemNote { .. } => {
            let text = if entry.entry_type == "system" {
                entry.content.as_ref().and_then(Value::as_str).map(String::from)
            } else {
                entry.message.as_ref().and_then(extract_message_text)
            };
            if let Some(text) = text {
                push_section(out, text.trim());
            }
        }
        EventKind::ToolCall { tool_use_id, .. } | EventKind::SubagentSpawn { tool_use_id, .. } => {
            match entry.entry_type.as_str() {
                "assistant" => {
                    let blocks = entry.message.as_ref().and_then(|m| m.get("content"));
                    let Some(Value::Array(blocks)) = blocks else { return };
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) == Some("tool_use")
                            && block.get("id").and_then(Value::as_str) == Some(tool_use_id)
                        {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            push_section(out, &format!("{name}\n── input ──"));
                            out.push('\n');
                            out.push_str(&serde_json::to_string_pretty(&input).unwrap_or_default());
                        }
                    }
                }
                "user" => {
                    let results =
                        entry.message.as_ref().map(tool_results).unwrap_or_default();
                    for (id, content, _) in results {
                        if id == *tool_use_id {
                            push_section(out, "── output ──");
                            out.push('\n');
                            out.push_str(&render_tool_output(
                                entry.tool_use_result.as_ref(),
                                &content,
                                usize::MAX,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Unified-diff text for an Edit-style rich result (`structuredPatch` hunks);
/// None when there is no usable patch, falling through to other renderings.
fn structured_patch_text(map: &serde_json::Map<String, Value>) -> Option<String> {
    let Some(Value::Array(hunks)) = map.get("structuredPatch") else { return None };
    if hunks.is_empty() {
        return None;
    }
    let mut out = String::new();
    if let Some(path) = map.get("filePath").and_then(Value::as_str) {
        out.push_str(path);
        out.push('\n');
    }
    let (mut added, mut removed) = (0usize, 0usize);
    let mut body = String::new();
    for hunk in hunks {
        let n = |k: &str| hunk.get(k).and_then(Value::as_u64).unwrap_or(0);
        body.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            n("oldStart"),
            n("oldLines"),
            n("newStart"),
            n("newLines")
        ));
        if let Some(Value::Array(lines)) = hunk.get("lines") {
            for line in lines.iter().filter_map(Value::as_str) {
                match line.as_bytes().first() {
                    Some(b'+') => added += 1,
                    Some(b'-') => removed += 1,
                    _ => {}
                }
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    let mut flags = String::new();
    if map.get("replaceAll").and_then(Value::as_bool).unwrap_or(false) {
        flags.push_str(" (replaceAll)");
    }
    if map.get("userModified").and_then(Value::as_bool).unwrap_or(false) {
        flags.push_str(" (user-modified)");
    }
    out.push_str(&format!("+{added} −{removed}{flags}\n"));
    out.push_str(&body);
    Some(out)
}

/// A subagent's final report: its last assistant text in the merged timeline
/// (uncapped via `full_event_content`), falling back to the spawn result's
/// `outputFile` transcript when the timeline holds none.
pub fn agent_report(session: &LoadedSession, row: &crate::agent_tree::FlatAgentRow) -> String {
    let Some(agent_id) = row.agent_id.as_deref() else { return String::new() };
    if let Some((first, last)) = row.event_range {
        let last = last.min(session.timeline.len().saturating_sub(1));
        for event in session.timeline[first..=last].iter().rev() {
            if event.agent_path.last().map(String::as_str) == Some(agent_id)
                && matches!(event.kind, EventKind::AssistantText { .. })
            {
                return full_event_content(session, event);
            }
        }
    }
    row.output_file
        .as_deref()
        .and_then(|p| report_from_output_file(std::path::Path::new(p)))
        .unwrap_or_default()
}

/// Last assistant text of an agent's `outputFile` JSONL transcript, if the file
/// still exists and is reasonably sized (it lives under /tmp and may vanish).
fn report_from_output_file(path: &std::path::Path) -> Option<String> {
    const MAX_OUTPUT_FILE: u64 = 2 * 1024 * 1024;
    if fs::metadata(path).ok()?.len() > MAX_OUTPUT_FILE {
        return None;
    }
    let (lines, _, _) = parser::read_all(path, 0).ok()?;
    lines.iter().rev().find_map(|(line, _)| {
        let RawLine::Conversation(entry) = line else { return None };
        if entry.entry_type != "assistant" {
            return None;
        }
        entry.message.as_ref().and_then(extract_message_text)
    })
}

/// In-memory (capped) rendering of an event, the popup's fallback content.
fn capped_event_content(event: &TimelineEvent) -> String {
    match &event.kind {
        EventKind::UserPrompt { text }
        | EventKind::AssistantText { text }
        | EventKind::Thinking { text }
        | EventKind::SystemNote { text } => text.clone(),
        EventKind::ToolCall { name, detail, .. } => {
            format!("{name}\n── input ──\n{}\n\n── output ──\n{}", detail.input, detail.output)
        }
        EventKind::SubagentSpawn { agent_type, description, prompt, .. } => {
            format!("Subagent {agent_type}: {description}\n── prompt ──\n{prompt}")
        }
    }
}

/// One display line for a tool call, from the most telling input field.
pub fn tool_one_liner(name: &str, input: &Value) -> String {
    let get = |key: &str| input.get(key).and_then(Value::as_str);
    let detail = match name {
        "Bash" => get("description").or_else(|| get("command")),
        "Read" | "Edit" | "Write" | "NotebookEdit" => get("file_path"),
        "Glob" | "Grep" => get("pattern"),
        "WebFetch" | "WebSearch" => get("url").or_else(|| get("query")),
        "Skill" => get("skill"),
        "ToolSearch" => get("query"),
        _ => get("description")
            .or_else(|| get("prompt"))
            .or_else(|| get("query"))
            .or_else(|| get("file_path")),
    };
    match detail {
        Some(d) => format!("{name}: {}", first_line(d, 80)),
        None => name.to_string(),
    }
}

pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated, {} bytes total)", &s[..end], s.len())
}

/// Short label for a timeline row.
pub fn event_label(kind: &EventKind) -> String {
    match kind {
        EventKind::UserPrompt { text } => format!("user  {}", first_line(text, 70)),
        EventKind::AssistantText { text } => format!("✻ {}", first_line(text, 70)),
        EventKind::Thinking { text } => format!("∴ {}", first_line(text, 70)),
        EventKind::ToolCall { one_liner, status, .. } => {
            format!("⚒ {} {}", one_liner, status_glyph(*status))
        }
        EventKind::SubagentSpawn { agent_type, description, status, .. } => {
            format!("⑂ {agent_type}: {} {}", first_line(description, 50), status_glyph(*status))
        }
        EventKind::SystemNote { text } => format!("· {}", first_line(text, 70)),
    }
}

/// Searchable text of an event — the capped in-memory content, which is all a
/// human could visually scan for anyway.
pub fn event_search_text(kind: &EventKind) -> String {
    match kind {
        EventKind::UserPrompt { text }
        | EventKind::AssistantText { text }
        | EventKind::Thinking { text }
        | EventKind::SystemNote { text } => text.clone(),
        EventKind::ToolCall { name, one_liner, detail, .. } => {
            format!("{name} {one_liner} {} {}", detail.input, detail.output)
        }
        EventKind::SubagentSpawn { agent_type, description, prompt, .. } => {
            format!("{agent_type} {description} {prompt}")
        }
    }
}

/// Tool status of an event, if it has one.
pub fn event_status(kind: &EventKind) -> Option<ToolStatus> {
    match kind {
        EventKind::ToolCall { status, .. } | EventKind::SubagentSpawn { status, .. } => {
            Some(*status)
        }
        _ => None,
    }
}

pub fn status_glyph(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Ok => "✓",
        ToolStatus::Error => "✗",
        ToolStatus::Denied => "⊘",
        ToolStatus::Pending => "…",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::TitleSource;
    use std::io::Write;
    use std::path::Path;
    use std::time::SystemTime;

    fn meta_for(path: &Path) -> SessionMeta {
        SessionMeta {
            id: path.file_stem().unwrap().to_string_lossy().to_string(),
            path: path.to_path_buf(),
            title: "t".into(),
            title_source: TitleSource::SessionId,
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        }
    }

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut f = fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn joins_tool_results_and_groups_split_assistant_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"do it"}}"#,
                r#"{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Part one."}]}}"#,
                r#"{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:02Z","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Part two."}]}}"#,
                r#"{"uuid":"a3","type":"assistant","timestamp":"2026-01-01T10:00:03Z","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"List files"}}]}}"#,
                r#"{"uuid":"u2","type":"user","timestamp":"2026-01-01T10:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"toolUseResult":{"stdout":"file.txt","stderr":""}}"#,
            ],
        );
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        assert_eq!(session.timeline.len(), 3); // prompt, merged text, tool call
        let EventKind::AssistantText { text } = &session.timeline[1].kind else { panic!() };
        assert!(text.contains("Part one.") && text.contains("Part two."));
        let EventKind::ToolCall { status, detail, one_liner, .. } = &session.timeline[2].kind
        else {
            panic!()
        };
        assert_eq!(*status, ToolStatus::Ok);
        assert!(detail.output.contains("file.txt"));
        assert_eq!(one_liner, "Bash: List files");
    }

    #[test]
    fn builds_agent_tree_and_merges_subagent_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:00Z","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Agent","input":{"description":"Explore stuff","subagent_type":"Explore","prompt":"go"}}]}}"#,
                r#"{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:05:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"done"}]},"toolUseResult":{"agentId":"abc123","resolvedModel":"claude-fable-5","isAsync":true,"outputFile":"/tmp/none.jsonl"}}"#,
            ],
        );
        let sub = dir.path().join("s1").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        write_lines(
            &sub.join("agent-abc123.jsonl"),
            &[
                r#"{"uuid":"su1","type":"assistant","isSidechain":true,"agentId":"abc123","timestamp":"2026-01-01T10:01:00Z","message":{"id":"sm1","role":"assistant","content":[{"type":"tool_use","id":"t9","name":"Read","input":{"file_path":"/x/y.rs"}}]}}"#,
                r#"{"uuid":"su2","type":"assistant","isSidechain":true,"agentId":"abc123","timestamp":"2026-01-01T10:02:00Z","message":{"id":"sm2","role":"assistant","content":[{"type":"text","text":"All done."}]}}"#,
            ],
        );
        fs::write(
            sub.join("agent-abc123.meta.json"),
            r#"{"agentType":"Explore","description":"Explore stuff","toolUseId":"toolu_1","spawnDepth":1}"#,
        )
        .unwrap();

        let session = LoadedSession::load(meta_for(&path)).unwrap();
        // Spawn (10:00), subagent Read (10:01) + text (10:02), merged chronologically.
        assert_eq!(session.timeline.len(), 3);
        let EventKind::SubagentSpawn { agent_id, status, .. } = &session.timeline[0].kind else {
            panic!()
        };
        assert_eq!(agent_id.as_deref(), Some("abc123"));
        assert_eq!(*status, ToolStatus::Ok);
        assert_eq!(session.timeline[1].agent_path, vec!["abc123".to_string()]);

        assert_eq!(session.agent_tree.children.len(), 1);
        let child = &session.agent_tree.children[0];
        assert_eq!(child.agent_id.as_deref(), Some("abc123"));
        assert_eq!(child.agent_type, "Explore");
        assert_eq!(child.status, ToolStatus::Ok);
        assert_eq!(child.event_range, Some((1, 2)));
        // Spawn-result extras land on the node.
        assert_eq!(child.resolved_model.as_deref(), Some("claude-fable-5"));
        assert!(child.is_async);
        assert_eq!(child.output_file.as_deref(), Some("/tmp/none.jsonl"));
        assert_eq!(child.prompt, "go");

        // The report is the subagent's last assistant text from the timeline.
        let rows = crate::agent_tree::flatten(&session.agent_tree);
        assert_eq!(agent_report(&session, &rows[1]), "All done.");
    }

    #[test]
    fn orphan_subagent_becomes_pending_child_of_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(&path, &[r#"{"type":"ai-title","aiTitle":"T","sessionId":"s1"}"#]);
        let sub = dir.path().join("s1").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        write_lines(
            &sub.join("agent-zzz.jsonl"),
            &[
                r#"{"uuid":"su1","type":"user","isSidechain":true,"agentId":"zzz","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"task"}}"#,
            ],
        );
        // no meta.json, no matching spawn → orphan
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        assert_eq!(session.agent_tree.children.len(), 1);
        assert_eq!(session.agent_tree.children[0].status, ToolStatus::Pending);
    }

    #[test]
    fn thinking_blocks_become_events_and_merge_per_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:00Z","message":{"id":"m1","role":"assistant","content":[{"type":"thinking","thinking":"hmm part one"}]}}"#,
                r#"{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Answer A."}]}}"#,
                r#"{"uuid":"a3","type":"assistant","timestamp":"2026-01-01T10:00:02Z","message":{"id":"m1","role":"assistant","content":[{"type":"thinking","thinking":"hmm part two"},{"type":"text","text":"Answer B."}]}}"#,
            ],
        );
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        // One merged Thinking event + one merged AssistantText event.
        assert_eq!(session.timeline.len(), 2);
        let EventKind::Thinking { text } = &session.timeline[0].kind else { panic!() };
        assert!(text.contains("part one") && text.contains("part two"));
        // Thinking interleaved between text blocks must not break text merging.
        let EventKind::AssistantText { text } = &session.timeline[1].kind else { panic!() };
        assert!(text.contains("Answer A.") && text.contains("Answer B."));
    }

    #[test]
    fn edit_results_render_as_unified_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:00Z","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x.rs"}}]}}"#,
                r#"{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"toolUseResult":{"filePath":"/x.rs","replaceAll":true,"structuredPatch":[{"oldStart":15,"oldLines":2,"newStart":15,"newLines":3,"lines":[" ctx","-old line","+new line","+added line"]}]}}"#,
                r#"{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:02Z","message":{"id":"m2","role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}}]}}"#,
                r#"{"uuid":"u2","type":"user","timestamp":"2026-01-01T10:00:03Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"x"}]},"toolUseResult":{"structuredPatch":[],"note":"failed"}}"#,
            ],
        );
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        let EventKind::ToolCall { detail, .. } = &session.timeline[0].kind else { panic!() };
        assert!(detail.output.starts_with("/x.rs\n+2 −1 (replaceAll)\n"));
        assert!(detail.output.contains("@@ -15,2 +15,3 @@"));
        assert!(detail.output.contains("-old line"));
        // Empty structuredPatch falls back to pretty JSON.
        let EventKind::ToolCall { detail, .. } = &session.timeline[1].kind else { panic!() };
        assert!(detail.output.contains("\"note\": \"failed\""));
    }

    #[test]
    fn full_event_content_reads_uncapped_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let big = "x".repeat(OUTPUT_CAP + 100);
        write_lines(
            &path,
            &[
                r#"{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:00Z","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#,
                &format!(
                    r#"{{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:01Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok"}}]}},"toolUseResult":{{"stdout":"{big}","stderr":""}}}}"#
                ),
            ],
        );
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        let event = &session.timeline[0];
        let EventKind::ToolCall { detail, .. } = &event.kind else { panic!() };
        assert!(detail.output.contains("truncated")); // in-memory content is capped
        let full = full_event_content(&session, event);
        assert!(full.contains(&big)); // re-read content is not
        assert!(!full.contains("truncated"));
    }

    #[test]
    fn full_event_content_falls_back_when_file_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"hello there"}}"#,
            ],
        );
        let session = LoadedSession::load(meta_for(&path)).unwrap();
        // Rewrite the file shorter than the recorded span without refreshing.
        fs::write(&path, "{}").unwrap();
        let full = full_event_content(&session, &session.timeline[0]);
        assert_eq!(full, "hello there"); // capped in-memory fallback, never garbage
    }

    #[test]
    fn incremental_refresh_appends_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        write_lines(
            &path,
            &[
                r#"{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"first"}}"#,
            ],
        );
        let mut session = LoadedSession::load(meta_for(&path)).unwrap();
        assert_eq!(session.timeline.len(), 1);
        let offset_before = session.transcripts[0].offset;

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"uuid":"u2","type":"user","timestamp":"2026-01-01T10:01:00Z","message":{{"role":"user","content":"second"}}}}"#
        )
        .unwrap();
        session.refresh().unwrap();
        assert_eq!(session.timeline.len(), 2);
        assert!(session.transcripts[0].offset > offset_before);
    }
}
