//! Raw wire format of ~/.claude/projects JSONL files.
//!
//! Two families of lines exist:
//! - Conversation entries: have a `uuid` and a `type` of user/assistant/attachment/system.
//! - Bookkeeping entries: no `uuid`, keyed by `sessionId` (ai-title, cost-state, last-prompt, ...).
//!   Later occurrences supersede earlier ones.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug)]
pub enum RawLine {
    Conversation(Box<ConvEntry>),
    /// Any line without a uuid; `kind` is its `type` field ("ai-title", "cost-state", ...).
    Bookkeeping { kind: String, value: Value },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // mirrors the wire format; not every field is displayed yet
pub struct ConvEntry {
    pub uuid: String,
    #[serde(default)]
    pub parent_uuid: Option<String>,
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Anthropic API message object (user/assistant entries).
    #[serde(default)]
    pub message: Option<Value>,
    /// Rich structured tool result (user entries carrying a tool_result).
    #[serde(default)]
    pub tool_use_result: Option<Value>,
    #[serde(default)]
    pub tool_denial_kind: Option<String>,
    /// system entries: turn_duration, away_summary, ...
    #[serde(default)]
    pub subtype: Option<String>,
    /// system entries payload.
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub is_meta: bool,
    /// Byte span (start, len) of this entry's source line; filled by the session
    /// loader so truncated content can be lazily re-read from disk.
    #[serde(skip)]
    pub src_start: u64,
    #[serde(skip)]
    pub src_len: u64,
}

impl ConvEntry {
    pub fn parsed_timestamp(&self) -> Option<jiff::Timestamp> {
        self.timestamp.as_deref().and_then(|t| t.parse().ok())
    }
}

/// The `cost-state` bookkeeping entry: per-session totals, last occurrence wins.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostState {
    // rename_all would produce "totalCostUsd"; the file says "totalCostUSD".
    #[serde(default, rename = "totalCostUSD")]
    pub total_cost_usd: f64,
    /// Wall-clock duration in ms.
    #[serde(default)]
    pub total_duration: u64,
    #[serde(default)]
    pub total_lines_added: u64,
    #[serde(default)]
    pub total_lines_removed: u64,
    // Same casing quirk as totalCostUSD: the file says "totalAPIDuration".
    #[serde(default, rename = "totalAPIDuration")]
    pub total_api_duration: u64,
    /// Time spent executing tools, in ms.
    #[serde(default)]
    pub total_tool_duration: u64,
    /// Session start, epoch ms.
    #[serde(default)]
    pub start_time: Option<i64>,
    /// Per-model token/cost breakdown, keyed by model id.
    #[serde(default)]
    pub model_usage: HashMap<String, ModelUsage>,
}

/// One model's slice of `cost-state.modelUsage`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // mirrors the wire format; not every field is displayed yet
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub web_search_requests: u64,
    #[serde(default, rename = "costUSD")]
    pub cost_usd: f64,
}

/// Sidecar `subagents/agent-<id>.meta.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // mirrors the wire format; not every field is displayed yet
pub struct SubagentMeta {
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub spawn_depth: u32,
}

const CONV_TYPES: [&str; 4] = ["user", "assistant", "attachment", "system"];

/// Parse one JSONL line. `Err` only for invalid JSON; unknown shapes become `Bookkeeping`.
pub fn parse_line(line: &str) -> Result<RawLine, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
    if value.get("uuid").is_some() && CONV_TYPES.contains(&ty) {
        // All ConvEntry fields except uuid/type are defaulted, so this practically
        // cannot fail; fall back to Bookkeeping if the shape is unexpected anyway.
        match serde_json::from_value::<ConvEntry>(value.clone()) {
            Ok(entry) => return Ok(RawLine::Conversation(Box::new(entry))),
            Err(_) => return Ok(RawLine::Bookkeeping { kind: ty.to_string(), value }),
        }
    }
    let kind = if ty.is_empty() { "unknown".to_string() } else { ty.to_string() };
    Ok(RawLine::Bookkeeping { kind, value })
}
