//! Find and focus the tmux pane a claude session runs in.
//!
//! Pane ids come from the hook log (`$TMUX_PANE` of the claude process).
//! Sessions without hook data fall back to the one pane running `claude` in
//! the session's working directory, when that match is unambiguous.

use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    /// `%12`: stable for the pane's lifetime, server-unique.
    pub id: String,
    /// `main:2.0`, for messages.
    pub target: String,
    pub command: String,
    pub path: String,
}

impl Pane {
    /// tmux reports the foreground process; claude shows up as `claude`
    /// (or `node` for npm installs).
    pub fn runs_claude(&self) -> bool {
        self.command.contains("claude") || self.command == "node"
    }
}

const FORMAT: &str =
    "#{pane_id}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_current_command}\t#{pane_current_path}";

fn tmux(socket: Option<&str>) -> Command {
    let mut cmd = Command::new("tmux");
    if let Some(s) = socket {
        cmd.args(["-S", s]);
    }
    cmd
}

/// All panes of the server at `socket` (default server if None). Empty when
/// tmux isn't installed or no server runs.
pub fn list_panes(socket: Option<&str>) -> Vec<Pane> {
    let Ok(out) = tmux(socket).args(["list-panes", "-a", "-F", FORMAT]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_panes(&String::from_utf8_lossy(&out.stdout))
}

fn parse_panes(text: &str) -> Vec<Pane> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.splitn(4, '\t');
            Some(Pane {
                id: f.next()?.to_string(),
                target: f.next()?.to_string(),
                command: f.next()?.to_string(),
                path: f.next()?.to_string(),
            })
        })
        .collect()
}

/// The single claude pane whose cwd is `cwd`; None if zero or ambiguous.
pub fn pane_by_cwd<'a>(panes: &'a [Pane], cwd: &str) -> Option<&'a Pane> {
    let mut matches = panes.iter().filter(|p| p.runs_claude() && p.path == cwd);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Make `pane` the visible one: its window becomes current, the pane gets
/// focus, and the tmux client switches to its session. Run from inside tmux
/// that's the client showing this TUI; from outside, tmux picks the most
/// recently active client.
pub fn focus(socket: Option<&str>, pane: &str) -> Result<(), String> {
    for args in [["select-window", "-t", pane], ["select-pane", "-t", pane]] {
        let out = tmux(socket).args(args).output().map_err(|e| format!("tmux: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    // Fails harmlessly without an attached client; the window still moved.
    let _ = tmux(socket).args(["switch-client", "-t", pane]).output();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_panes_and_matches_unique_claude_cwd() {
        let panes = parse_panes(
            "%10\t2:0.0\tclaude\t/w/a\n%11\t2:1.0\tzsh\t/w/a\n%12\t2:2.0\tclaude\t/w/b\n%13\t2:3.0\tclaude\t/w/b\n",
        );
        assert_eq!(panes.len(), 4);
        assert_eq!(panes[0].target, "2:0.0");
        assert_eq!(pane_by_cwd(&panes, "/w/a").map(|p| p.id.as_str()), Some("%10"));
        assert!(pane_by_cwd(&panes, "/w/b").is_none(), "two claude panes: ambiguous");
        assert!(pane_by_cwd(&panes, "/w/c").is_none());
    }
}
