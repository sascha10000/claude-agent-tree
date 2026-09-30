//! Markdown export of a loaded session (used by the `x` key and `--export`).

use std::fmt::Write as _;

use crate::agent_tree::flatten;
use crate::session::{event_label, full_event_content, status_glyph, EventKind, LoadedSession};

pub fn export_markdown(session: &LoadedSession) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", session.meta.title);
    let _ = writeln!(out, "- session: `{}`", session.meta.id);
    if let Some(cwd) = &session.meta.cwd {
        let _ = writeln!(out, "- cwd: `{cwd}`");
    }
    if let Some(cost) = &session.cost {
        if let Some(start) = cost.start_time
            && let Ok(ts) = jiff::Timestamp::from_millisecond(start)
        {
            let local = ts.to_zoned(jiff::tz::TimeZone::system());
            let _ = writeln!(out, "- started: {}", local.strftime("%Y-%m-%d %H:%M:%S"));
        }
        let _ = writeln!(
            out,
            "- cost: ${:.2} · +{}/-{} lines · wall {}s · api {}s · tools {}s",
            cost.total_cost_usd,
            cost.total_lines_added,
            cost.total_lines_removed,
            cost.total_duration / 1000,
            cost.total_api_duration / 1000,
            cost.total_tool_duration / 1000
        );
        if !cost.model_usage.is_empty() {
            out.push_str("\n| model | in | out | cache-r | cache-w | cost |\n");
            out.push_str("|---|---:|---:|---:|---:|---:|\n");
            let mut models: Vec<_> = cost.model_usage.iter().collect();
            models.sort_by(|a, b| b.1.cost_usd.total_cmp(&a.1.cost_usd));
            for (model, mu) in models {
                let _ = writeln!(
                    out,
                    "| {model} | {} | {} | {} | {} | ${:.2} |",
                    mu.input_tokens,
                    mu.output_tokens,
                    mu.cache_read_input_tokens,
                    mu.cache_creation_input_tokens,
                    mu.cost_usd
                );
            }
        }
    }

    out.push_str("\n## Agent tree\n\n```\n");
    for row in flatten(&session.agent_tree) {
        let duration = row
            .duration
            .map(|d| format!(" ({})", crate::ui::format_duration(d)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{}{} {}{} {}",
            row.prefix,
            status_glyph(row.status),
            row.agent_type,
            duration,
            row.description
        );
    }
    out.push_str("```\n\n## Timeline\n");

    for event in &session.timeline {
        let ts = event
            .timestamp
            .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into());
        match &event.kind {
            // Prompts and answers get their full, uncapped text.
            EventKind::UserPrompt { .. } => {
                let _ =
                    writeln!(out, "\n### {ts} user\n\n{}", full_event_content(session, event));
            }
            EventKind::AssistantText { .. } => {
                let _ = writeln!(
                    out,
                    "\n### {ts} assistant\n\n{}",
                    full_event_content(session, event)
                );
            }
            // Tool traffic stays one line each; full I/O would explode the file.
            _ => {
                let indent = "  ".repeat(event.agent_path.len());
                let _ = writeln!(out, "- `{ts}` {indent}{}", event_label(&event.kind));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{SessionMeta, TitleSource};
    use std::io::Write;
    use std::path::Path;
    use std::time::SystemTime;

    fn meta_for(path: &Path) -> SessionMeta {
        SessionMeta {
            id: path.file_stem().unwrap().to_string_lossy().to_string(),
            path: path.to_path_buf(),
            title: "My Session".into(),
            title_source: TitleSource::SessionId,
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
            cost: None,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        }
    }

    #[test]
    fn exports_headers_tree_and_full_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let long = "y".repeat(20 * 1024); // beyond TEXT_CAP: must appear uncapped
        writeln!(
            f,
            r#"{{"uuid":"u1","type":"user","timestamp":"2026-01-01T10:00:00Z","message":{{"role":"user","content":"do the thing"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"uuid":"a1","type":"assistant","timestamp":"2026-01-01T10:00:01Z","message":{{"id":"m1","role":"assistant","content":[{{"type":"text","text":"{long}"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"uuid":"a2","type":"assistant","timestamp":"2026-01-01T10:00:02Z","message":{{"id":"m2","role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{"description":"List"}}}}]}}}}"#
        )
        .unwrap();
        drop(f);

        let session = LoadedSession::load(meta_for(&path)).unwrap();
        let md = export_markdown(&session);
        assert!(md.starts_with("# My Session\n"));
        assert!(md.contains("## Agent tree"));
        assert!(md.contains(" user\n\ndo the thing"));
        assert!(md.contains(&long)); // uncapped assistant text
        assert!(!md.contains("truncated"));
        assert!(md.contains("⚒ Bash: List")); // tool call as one-liner
    }
}
