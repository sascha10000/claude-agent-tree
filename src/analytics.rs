//! Cross-project aggregation over already-parsed session metadata.
//!
//! Everything here is a pure fold over `ProjectIndex` — the tail scan has
//! already extracted `cost-state` per session, so no file I/O happens.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::index::ProjectIndex;

#[derive(Debug, Default, Clone)]
pub struct ModelAgg {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost: f64,
}

#[derive(Debug, Default)]
pub struct Analytics {
    pub total_cost: f64,
    pub costed_sessions: usize,
    pub total_sessions: usize,
    /// (display_path, cost, costed sessions), cost desc.
    pub per_project: Vec<(String, f64, usize)>,
    /// (title, display_path, cost), cost desc, top 10.
    pub top_sessions: Vec<(String, String, f64)>,
    /// (model id, totals), cost desc.
    pub per_model: Vec<(String, ModelAgg)>,
    pub cache_read_total: u64,
    pub cache_creation_total: u64,
}

pub fn aggregate(index: &ProjectIndex) -> Analytics {
    let mut out = Analytics::default();
    let mut models: HashMap<String, ModelAgg> = HashMap::new();
    for project in &index.projects {
        let mut project_cost = 0.0;
        let mut project_costed = 0usize;
        for s in &project.sessions {
            out.total_sessions += 1;
            let Some(cost) = &s.cost else { continue };
            out.costed_sessions += 1;
            project_costed += 1;
            project_cost += cost.total_cost_usd;
            out.total_cost += cost.total_cost_usd;
            out.top_sessions.push((
                s.title.clone(),
                project.display_path.clone(),
                cost.total_cost_usd,
            ));
            for (model, mu) in &cost.model_usage {
                let agg = models.entry(model.clone()).or_default();
                agg.input += mu.input_tokens;
                agg.output += mu.output_tokens;
                agg.cache_read += mu.cache_read_input_tokens;
                agg.cache_creation += mu.cache_creation_input_tokens;
                agg.cost += mu.cost_usd;
                out.cache_read_total += mu.cache_read_input_tokens;
                out.cache_creation_total += mu.cache_creation_input_tokens;
            }
        }
        if project_costed > 0 {
            out.per_project.push((project.display_path.clone(), project_cost, project_costed));
        }
    }
    out.per_project.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.top_sessions.sort_by(|a, b| b.2.total_cmp(&a.2));
    out.top_sessions.truncate(10);
    out.per_model = models.into_iter().collect();
    out.per_model.sort_by(|a, b| b.1.cost.total_cmp(&a.1.cost));
    out
}

/// One row of the fleet view: a session with recent write activity.
#[derive(Debug, Clone)]
pub struct FleetEntry {
    /// Identity for jumping, stable across index rescans.
    pub project_dir: PathBuf,
    pub session_id: String,
    pub title: String,
    pub display_path: String,
    pub mtime: SystemTime,
    pub cost: Option<f64>,
    pub subagents: usize,
}

/// Sessions written to within `window`, across all projects, newest first.
pub fn fleet(index: &ProjectIndex, window: Duration) -> Vec<FleetEntry> {
    let mut out = Vec::new();
    for project in &index.projects {
        for s in &project.sessions {
            // elapsed() errs for future mtimes (clock skew): treat as active.
            let active = s.mtime.elapsed().map(|e| e <= window).unwrap_or(true);
            if !active {
                continue;
            }
            out.push(FleetEntry {
                project_dir: project.dir.clone(),
                session_id: s.id.clone(),
                title: s.title.clone(),
                display_path: project.display_path.clone(),
                mtime: s.mtime,
                cost: s.cost.as_ref().map(|c| c.total_cost_usd),
                subagents: s.subagent_count,
            });
        }
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ProjectEntry, SessionMeta, TitleSource};
    use crate::model::{CostState, ModelUsage};
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn session(id: &str, cost: Option<f64>, model_tokens: &[(&str, u64, f64)]) -> SessionMeta {
        let cost = cost.map(|c| CostState {
            total_cost_usd: c,
            model_usage: model_tokens
                .iter()
                .map(|(m, tokens, usd)| {
                    (
                        m.to_string(),
                        ModelUsage {
                            input_tokens: *tokens,
                            cache_read_input_tokens: *tokens * 10,
                            cost_usd: *usd,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        });
        SessionMeta {
            id: id.into(),
            path: PathBuf::from(format!("/x/{id}.jsonl")),
            title: id.into(),
            title_source: TitleSource::SessionId,
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
            cost,
            cwd: None,
            subagent_count: 0,
            subagent_mtime: None,
        }
    }

    fn project(path: &str, sessions: Vec<SessionMeta>) -> ProjectEntry {
        ProjectEntry { dir: PathBuf::from(path), display_path: path.into(), sessions }
    }

    #[test]
    fn aggregates_and_sorts_across_projects() {
        let index = ProjectIndex {
            root: PathBuf::new(),
            projects: vec![
                project(
                    "/p1",
                    vec![
                        session("a", Some(1.0), &[("fable", 100, 0.8)]),
                        session("b", None, &[]), // no cost data: skipped
                    ],
                ),
                project(
                    "/p2",
                    vec![
                        session("c", Some(5.0), &[("fable", 50, 4.0), ("haiku", 10, 0.1)]),
                        session("d", Some(2.0), &[("haiku", 30, 0.4)]),
                    ],
                ),
            ],
        };
        let a = aggregate(&index);
        assert_eq!(a.total_sessions, 4);
        assert_eq!(a.costed_sessions, 3);
        assert!((a.total_cost - 8.0).abs() < 1e-9);
        // Projects sorted by cost desc; p2 (7.0) before p1 (1.0).
        assert_eq!(a.per_project[0].0, "/p2");
        assert_eq!(a.per_project[0].2, 2);
        // Top sessions sorted desc.
        assert_eq!(a.top_sessions[0].0, "c");
        assert_eq!(a.top_sessions[2].0, "a");
        // Same model id merges across sessions and projects.
        assert_eq!(a.per_model[0].0, "fable"); // 4.8 > 0.5
        assert_eq!(a.per_model[0].1.input, 150);
        assert!((a.per_model[1].1.cost - 0.5).abs() < 1e-9);
        assert_eq!(a.cache_read_total, (100 + 50 + 10 + 30) * 10);
    }

    #[test]
    fn fleet_filters_by_recency_and_sorts_newest_first() {
        let now = SystemTime::now();
        let mut old = session("old", Some(1.0), &[]);
        old.mtime = now - Duration::from_secs(3600);
        let mut fresh = session("fresh", None, &[]);
        fresh.mtime = now - Duration::from_secs(30);
        let mut fresher = session("fresher", Some(2.0), &[]);
        fresher.mtime = now - Duration::from_secs(5);
        let index = ProjectIndex {
            root: PathBuf::new(),
            projects: vec![project("/p1", vec![old, fresh]), project("/p2", vec![fresher])],
        };
        let entries = fleet(&index, Duration::from_secs(300));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id, "fresher");
        assert_eq!(entries[1].session_id, "fresh");
        assert_eq!(entries[0].project_dir, PathBuf::from("/p2"));
    }

    #[test]
    fn top_sessions_truncate_to_ten() {
        let sessions = (0..15).map(|i| session(&format!("s{i}"), Some(i as f64), &[])).collect();
        let index =
            ProjectIndex { root: PathBuf::new(), projects: vec![project("/p", sessions)] };
        let a = aggregate(&index);
        assert_eq!(a.top_sessions.len(), 10);
        assert_eq!(a.top_sessions[0].0, "s14");
    }
}
