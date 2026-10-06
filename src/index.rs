//! Cheap project/session listing: directory scan + tail-scan metadata.
//!
//! List views must never parse a whole transcript (sessions reach 48 MB). Title,
//! cost and cwd are bookkeeping lines appended near the end of the file, so we
//! read a window from the tail and scan lines backwards; the first hit from the
//! end is the last occurrence, which wins.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::model::CostState;

const TAIL_WINDOW: u64 = 64 * 1024;
const TAIL_WINDOW_MAX: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    AiTitle,
    AwaySummary,
    LastPrompt,
    FirstPrompt,
    Slug,
    SessionId,
}

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub title_source: TitleSource,
    pub mtime: SystemTime,
    pub size: u64,
    pub cost: Option<CostState>,
    pub cwd: Option<String>,
    pub subagent_count: usize,
    /// Newest mtime among subagent transcripts; subagents write to their own
    /// files, so the main transcript goes quiet while they run.
    pub subagent_mtime: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub dir: PathBuf,
    /// Human-readable project path: `cwd` from a session if available,
    /// otherwise the de-mangled directory name.
    pub display_path: String,
    pub sessions: Vec<SessionMeta>,
}

impl ProjectEntry {
    /// Last path component (`~/workspace/projects/foo` → `foo`) for lists.
    pub fn name(&self) -> &str {
        Path::new(&self.display_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.display_path)
    }

    /// The directory claude runs in for this project: a recorded session
    /// `cwd` (exact), else the de-mangled dir name (lossy: `-` is ambiguous),
    /// whichever exists on disk.
    pub fn working_dir(&self) -> Option<PathBuf> {
        self.sessions
            .iter()
            .filter_map(|s| s.cwd.as_deref())
            .chain(std::iter::once(self.display_path.as_str()))
            .map(PathBuf::from)
            .find(|p| p.is_dir())
    }
}

#[derive(Debug, Default)]
pub struct ProjectIndex {
    pub root: PathBuf,
    pub projects: Vec<ProjectEntry>,
}

impl ProjectIndex {
    pub fn scan(root: &Path) -> std::io::Result<Self> {
        let mut projects = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let dir = entry.path();
            if !dir.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            projects.push(scan_project(&dir));
        }
        // Most recently active project first; empty projects last, alphabetical.
        projects.sort_by(|a, b| match (a.sessions.first(), b.sessions.first()) {
            (Some(x), Some(y)) => y.mtime.cmp(&x.mtime),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.display_path.cmp(&b.display_path),
        });
        Ok(Self { root: root.to_path_buf(), projects })
    }

}

fn scan_project(dir: &Path) -> ProjectEntry {
    let mut sessions: Vec<SessionMeta> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Some(meta) = SessionMeta::tail_scan(&path) {
                    sessions.push(meta);
                }
        }
    }
    sessions.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    let display_path = sessions
        .iter()
        .filter_map(|s| s.cwd.as_deref())
        .find_map(|cwd| launch_dir(cwd, dir))
        .unwrap_or_else(|| demangle_dir_name(dir));
    ProjectEntry { dir: dir.to_path_buf(), display_path, sessions }
}

/// A session's `cwd` follows the shell, so after a `cd src` it points into a
/// subfolder. The project dir name is the mangled *launch* dir, so walk up the
/// `cwd` until an ancestor mangles to that name.
fn launch_dir(cwd: &str, dir: &Path) -> Option<String> {
    let mangled = dir.file_name()?.to_string_lossy();
    Path::new(cwd)
        .ancestors()
        .map(|p| p.to_string_lossy())
        .find(|p| mangle_path(p) == mangled)
        .map(|p| p.into_owned())
}

/// Claude Code's project dir naming: every non-alphanumeric char becomes `-`.
fn mangle_path(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `-Users-sascha-workspace-projects-foo` → `/Users/sascha/workspace/projects/foo`.
/// Lossy (a literal `-` is indistinguishable); only a fallback when no cwd is known.
fn demangle_dir_name(dir: &Path) -> String {
    let name = dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name.replace('-', "/")
}

impl SessionMeta {
    pub fn tail_scan(path: &Path) -> Option<Self> {
        let fs_meta = fs::metadata(path).ok()?;
        let id = path.file_stem()?.to_string_lossy().to_string();

        let mut window = TAIL_WINDOW;
        let mut found = TailFinds::default();
        loop {
            found = scan_tail_window(path, fs_meta.len(), window).unwrap_or(found);
            let covered_whole_file = window >= fs_meta.len();
            if found.title_candidate().is_some() || covered_whole_file || window >= TAIL_WINDOW_MAX {
                break;
            }
            window *= 4;
        }
        let (title, title_source) = found
            .title_candidate()
            .or_else(|| first_human_prompt(path).map(|t| (t, TitleSource::FirstPrompt)))
            .or_else(|| found.slug.clone().map(|s| (s, TitleSource::Slug)))
            .unwrap_or_else(|| (id.clone(), TitleSource::SessionId));

        let (subagent_count, subagent_mtime) = scan_subagents(path, &id);

        Some(Self {
            id,
            path: path.to_path_buf(),
            title: sanitize_title(&title),
            title_source,
            mtime: fs_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: fs_meta.len(),
            cost: found.cost,
            cwd: found.cwd,
            subagent_count,
            subagent_mtime,
        })
    }
}

#[derive(Debug, Default, Clone)]
struct TailFinds {
    ai_title: Option<String>,
    away_summary: Option<String>,
    last_prompt: Option<String>,
    slug: Option<String>,
    cost: Option<CostState>,
    cwd: Option<String>,
}

impl TailFinds {
    fn title_candidate(&self) -> Option<(String, TitleSource)> {
        if let Some(t) = &self.ai_title {
            return Some((t.clone(), TitleSource::AiTitle));
        }
        if let Some(t) = &self.away_summary {
            return Some((t.clone(), TitleSource::AwaySummary));
        }
        if let Some(t) = &self.last_prompt {
            return Some((t.clone(), TitleSource::LastPrompt));
        }
        None
    }
}

fn scan_tail_window(path: &Path, file_len: u64, window: u64) -> Option<TailFinds> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let start = file_len.saturating_sub(window);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((file_len - start) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // first line of a mid-file window is partial
    }

    let mut finds = TailFinds::default();
    for line in lines.iter().rev() {
        // Cheap substring pre-filter before paying for JSON parsing.
        let interesting = (finds.ai_title.is_none() && line.contains("\"aiTitle\""))
            || (finds.cost.is_none() && line.contains("\"cost-state\""))
            || (finds.last_prompt.is_none() && line.contains("\"lastPrompt\""))
            || (finds.away_summary.is_none() && line.contains("away_summary"))
            || (finds.slug.is_none() && line.contains("\"slug\""))
            || (finds.cwd.is_none() && line.contains("\"cwd\""));
        if !interesting {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "ai-title" => {
                if finds.ai_title.is_none() {
                    finds.ai_title = value.get("aiTitle").and_then(Value::as_str).map(String::from);
                }
            }
            "cost-state" => {
                if finds.cost.is_none() {
                    finds.cost = serde_json::from_value(value.clone()).ok();
                }
            }
            "last-prompt" => {
                if finds.last_prompt.is_none() {
                    finds.last_prompt =
                        value.get("lastPrompt").and_then(Value::as_str).map(String::from);
                }
            }
            "system" => {
                if finds.away_summary.is_none()
                    && value.get("subtype").and_then(Value::as_str) == Some("away_summary")
                {
                    finds.away_summary =
                        value.get("content").and_then(Value::as_str).map(String::from);
                }
            }
            _ => {}
        }
        if finds.slug.is_none() {
            finds.slug = value.get("slug").and_then(Value::as_str).map(String::from);
        }
        if finds.cwd.is_none() {
            finds.cwd = value.get("cwd").and_then(Value::as_str).map(String::from);
        }
        if finds.ai_title.is_some() && finds.cost.is_some() && finds.cwd.is_some() {
            break;
        }
    }
    Some(finds)
}

/// Bounded forward scan: first real human prompt, for old sessions without ai-title.
fn first_human_prompt(path: &Path) -> Option<String> {
    use crate::model::RawLine;
    let mut reader = crate::parser::JsonlReader::open(path, 0).ok()?;
    let mut inspected = 0;
    while let Some(line) = reader.next_line() {
        inspected += 1;
        if inspected > 200 {
            break; // a prompt should appear early; don't crawl a 48 MB file
        }
        if let RawLine::Conversation(entry) = line
            && entry.entry_type == "user" && !entry.is_meta
                && let Some(text) = entry.message.as_ref().and_then(extract_message_text)
                    && !text.trim().is_empty() && !text.trim_start().starts_with('<') {
                        return Some(text);
                    }
    }
    None
}

/// Text of an Anthropic message: string content or concatenated text blocks.
pub fn extract_message_text(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(blocks)) => {
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            if text.is_empty() { None } else { Some(text.join("\n")) }
        }
        _ => None,
    }
}

fn sanitize_title(title: &str) -> String {
    let flat = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result: String = flat.chars().take(120).collect();
    if flat.chars().count() > 120 {
        result.push('…');
    }
    result
}

/// Subagent transcript count and the newest of their mtimes.
fn scan_subagents(session_path: &Path, session_id: &str) -> (usize, Option<SystemTime>) {
    let dir = match session_path.parent() {
        Some(p) => p.join(session_id).join("subagents"),
        None => return (0, None),
    };
    let Ok(entries) = fs::read_dir(dir) else { return (0, None) };
    let mut count = 0;
    let mut newest: Option<SystemTime> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("agent-") && name.ends_with(".jsonl")) {
            continue;
        }
        count += 1;
        if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
            newest = Some(newest.map_or(m, |n| n.max(m)));
        }
    }
    (count, newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn session_file(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("{id}.jsonl"));
        let mut f = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn project_name_and_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("my-proj");
        fs::create_dir(&real).unwrap();
        let mut project = ProjectEntry {
            dir: PathBuf::from("/unused"),
            display_path: real.to_string_lossy().into_owned(),
            sessions: Vec::new(),
        };
        assert_eq!(project.name(), "my-proj");
        // No sessions: falls back to display_path when it exists.
        assert_eq!(project.working_dir(), Some(real.clone()));
        project.display_path = "/does/not/exist".into();
        assert_eq!(project.working_dir(), None);
    }

    #[test]
    fn launch_dir_ignores_later_cd() {
        let dir = Path::new("/x/-Users-me-my-proj");
        assert_eq!(launch_dir("/Users/me/my-proj/src", dir).as_deref(), Some("/Users/me/my-proj"));
        assert_eq!(launch_dir("/Users/me/my-proj", dir).as_deref(), Some("/Users/me/my-proj"));
        assert_eq!(launch_dir("/Users/me/other", dir), None);
    }

    #[test]
    fn title_prefers_last_ai_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_file(
            dir.path(),
            "s1",
            &[
                r#"{"type":"ai-title","aiTitle":"Old title","sessionId":"s1"}"#,
                r#"{"uuid":"u1","type":"user","cwd":"/tmp/proj","message":{"role":"user","content":"hello"}}"#,
                r#"{"type":"ai-title","aiTitle":"New title","sessionId":"s1"}"#,
                r#"{"type":"cost-state","sessionId":"s1","totalCostUSD":1.25,"totalDuration":60000,"totalLinesAdded":3,"totalLinesRemoved":1,"totalAPIDuration":40000,"totalToolDuration":5000,"startTime":1789000000000,"modelUsage":{"claude-fable-5":{"inputTokens":100,"outputTokens":50,"cacheReadInputTokens":2000,"cacheCreationInputTokens":300,"webSearchRequests":0,"costUSD":1.0}}}"#,
            ],
        );
        let meta = SessionMeta::tail_scan(&path).unwrap();
        assert_eq!(meta.title, "New title");
        assert_eq!(meta.title_source, TitleSource::AiTitle);
        assert_eq!(meta.cwd.as_deref(), Some("/tmp/proj"));
        let cost = meta.cost.unwrap();
        assert!((cost.total_cost_usd - 1.25).abs() < f64::EPSILON);
        assert_eq!(cost.total_api_duration, 40000);
        assert_eq!(cost.total_tool_duration, 5000);
        assert_eq!(cost.start_time, Some(1789000000000));
        let mu = &cost.model_usage["claude-fable-5"];
        assert_eq!(mu.input_tokens, 100);
        assert_eq!(mu.cache_read_input_tokens, 2000);
        assert!((mu.cost_usd - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn title_falls_back_to_first_human_prompt_then_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_file(
            dir.path(),
            "s2",
            &[r#"{"uuid":"u1","type":"user","message":{"role":"user","content":"Fix the parser please"}}"#],
        );
        let meta = SessionMeta::tail_scan(&path).unwrap();
        assert_eq!(meta.title, "Fix the parser please");
        assert_eq!(meta.title_source, TitleSource::FirstPrompt);

        let empty = session_file(dir.path(), "s3", &[r#"{"type":"mode","mode":"normal"}"#]);
        let meta = SessionMeta::tail_scan(&empty).unwrap();
        assert_eq!(meta.title, "s3");
        assert_eq!(meta.title_source, TitleSource::SessionId);
    }

    #[test]
    fn scan_handles_empty_projects_and_sorts_nonempty_first() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("-tmp-empty")).unwrap();
        let proj = root.path().join("-tmp-proj");
        fs::create_dir(&proj).unwrap();
        session_file(&proj, "s1", &[r#"{"type":"ai-title","aiTitle":"T","sessionId":"s1"}"#]);

        let index = ProjectIndex::scan(root.path()).unwrap();
        assert_eq!(index.projects.len(), 2);
        assert_eq!(index.projects[0].sessions.len(), 1); // non-empty first
        assert!(index.projects[1].sessions.is_empty());
        assert_eq!(index.projects[1].display_path, "/tmp/empty");
    }

    #[test]
    fn counts_subagents_without_parsing() {
        let root = tempfile::tempdir().unwrap();
        let path = session_file(root.path(), "s1", &[r#"{"type":"ai-title","aiTitle":"T"}"#]);
        let sub = root.path().join("s1").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("agent-abc.jsonl"), "").unwrap();
        fs::write(sub.join("agent-abc.meta.json"), "{}").unwrap();
        let meta = SessionMeta::tail_scan(&path).unwrap();
        assert_eq!(meta.subagent_count, 1);
        assert!(meta.subagent_mtime.is_some());
    }
}
