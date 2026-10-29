# claude-agent-tree

Terminal UI for visualizing Claude Code sessions from `~/.claude/projects`:
projects → sessions (with title, cost, size) → timeline of every agent action,
plus a flow graph of the main agent and its subagents. Running sessions
update live (file-system watcher, tail-follow).

## Installation

**Requirements**

- Rust ≥ 1.88 (edition 2024, let-chains) — `rustup update stable`
- Claude Code (`claude` on the `PATH`) — only needed for `n` (new session) and `R` (resume)
- optional: tmux — for `w` (jump to a running session)

**1. Install the binary**

```
git clone <repo-url> claude-agent-tree && cd claude-agent-tree
cargo install --path .        # → ~/.cargo/bin/claude-agent-tree
```

`~/.cargo/bin` must be on your `PATH` (rustup usually sets this up).
To run without installing: `cargo build --release`, then start
`./target/release/claude-agent-tree` directly.

**2. Set up hooks (optional, recommended)**

```
claude-agent-tree --install-hooks
```

Run this only *after* step 1: the hook is registered with the **absolute path**
of the binary that is currently running. If you use
`cargo run -- --install-hooks` instead, the hooks point at `target/release` —
that works, but breaks on `cargo clean`. Afterwards, restart any running Claude
sessions (new sessions are picked up immediately). To verify: after the first
event, `⚓ hooks` appears in the TUI's status line. See
[Hooks](#hooks-optional-recommended) for details.

**3. Run**

```
claude-agent-tree
```

**Updating**

```
git pull && cargo install --path . --force
```

The binary path stays the same, so the hooks keep working. Only if the path
changes (binary moved, different install method) do you need to run
`claude-agent-tree --install-hooks` again — until then the hooks have no
effect but do no harm either (`|| true`).

**Uninstalling**

```
claude-agent-tree --uninstall-hooks   # first, while the binary still exists
cargo uninstall claude-agent-tree
rm -rf ~/.claude/agent-tree           # hook event log
```

## Usage

Once installed, use `claude-agent-tree` in place of `cargo run --release --`.

```
cargo run --release            # TUI
cargo run --release -- --list  # projects/sessions as a table
cargo run --release -- --dump <sessionIdPrefix>    # timeline + agent tree as text
cargo run --release -- --export <sessionIdPrefix>  # session as a Markdown file
cargo run --release -- --root <path> ...           # alternative projects directory
cargo run --release -- --install-hooks             # Claude Code hooks for exact status
cargo run --release -- --uninstall-hooks           # …and remove them again
```

### Hooks (optional, recommended)

`--install-hooks` adds `claude-agent-tree --hook` to `~/.claude/settings.json`
(backup: `settings.json.bak-agent-tree`; other hooks are left untouched, and
running it more than once is idempotent). The hooks run `async` and always exit
with 0 — they can neither slow down nor block Claude. Each session then writes
one compact line per event to `~/.claude/agent-tree/events.jsonl` (rotated at
4 MB; no tool inputs, one-liners only). What you get:

- exact status instead of an mtime heuristic, including for sessions in other terminals
- `⚠` **waiting for permission** (permission dialog), including the tool call in question
- the tmux pane of every session → `w` jumps straight to it

Running sessions only pick up new hooks after a restart.

## Keys

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | move selection |
| `g` / `G` | top / bottom |
| `enter` | open; Detail: jump between spawn event ↔ agent graph, otherwise full view |
| `o` | Detail: toggle timeline order (default: newest first) |
| `O` | Detail: event in a scrollable full view (untruncated, loaded lazily from disk) |
| `c` | Detail: cost/token panel (per model, cache, API vs. tool duration) |
| `t` | Detail: show the Agents pane as time lanes (parallel agents become visible) |
| `R` | open the session via `claude --resume` in an embedded terminal (Browse + Detail) |
| `w` | jump to the session: its embedded terminal or its tmux pane (Browse + Detail) |
| mouse wheel, `shift-PgUp/PgDn` | scroll back through the session's output (10k lines); any other key jumps back to live |
| `ctrl-q` | leave the terminal view — the session keeps running in the background |
| `ctrl-n` | any view: focus the session tab bar at the top (all running/waiting sessions with status); `←`/`→` select, `1`–`9` or `enter` switch, `esc` back. Embedded sessions open their terminal, others the Detail view |
| `s` | Browse: cycle sort order (mtime/cost/size/duration) |
| `a` | Browse: analytics overlay (cost/tokens across all projects) |
| `A` | Browse: only show projects with a running / waiting session (toggle) |
| `f` | Browse: fleet overlay (sessions active in the last 5 min, enter = jump there) |
| `/` | Browse: filter the focused list (projects or sessions); Detail: search the timeline |
| `n` | Browse: new, empty `claude` session in the selected project's directory (embedded terminal) |
| `N` | Browse: pick a directory (tab completes) and start a new session there; if a project already exists there it is selected, otherwise a new one is created (missing folders are created) |
| `n` / `N` | Detail: next / previous search match |
| `e` | Detail: jump to the next error/denial |
| `T` | Detail: show/hide thinking blocks |
| `x` | Detail: export the session as Markdown (also CLI `--export`) |
| `?` | keyboard help |
| `q` | quit; if embedded `claude` sessions are running, a popup asks first (`y` = quit and close the sessions, `n`/`esc` = cancel) |
| `esc` / `h` | back / close overlay or search |
| `tab` | switch pane; with the Agents pane focused, the Detail pane shows that agent |
| `u`/`d`, PgUp/PgDn | fast scroll (timeline, full view) |
| `r` | reload manually |

## Status symbols

`✓` ok · `✗` error · `⊘` denied · `…`/`◌` still running · `⚒` tool call ·
`⑂` subagent spawn · `▸` user prompt · `✻` assistant text

Session list: `●` yellow = currently working (transcript is being written) ·
`▶` green = attached terminal is idle — done or waiting for input.

Project list: `⚠` red = a session is waiting for permission (hooks only) · `●` yellow = a session/subagent is currently working ·
`▶` green = waiting for input · `○` cyan = active within the last hour ·
`·` gray = only older sessions.

Live agents panel (Browse, bottom right): every session of the selected project
that was active in the last 5 min, shown as a tree with its (nested)
subagents — `●` running · `◌` in a tool call · `✓` finished · `⊘` aborted.
Older finished agents are collapsed into `… +N earlier finished`.

## Architecture

- `parser.rs` — tolerant streaming JSONL reader; an incomplete last line (a
  file being written live) is not consumed but re-read on the next watch event.
- `index.rs` — lists come from a tail scan (last 64 KB, backwards) for
  `ai-title`/`cost-state`, never from a full parse; 48 MB sessions cost
  nothing to list.
- `session.rs` — full parse on open: builds events, joins tool_use↔tool_result,
  and merges subagent transcripts (`<sessionId>/subagents/agent-*.jsonl`) in
  chronologically.
- `agent_tree.rs` — agent tree via `meta.json.toolUseId` ↔ `tool_use.id`, at
  any nesting depth.
- `watch.rs` / `app.rs` / `ui/` — notify debouncer (250 ms), event loop with
  incremental reload (file offsets), ratatui rendering with window slicing.
- Full view (`O`): events store byte spans of their JSONL lines; the
  untruncated content is only read back from disk when opened (memory stays
  flat; the 4/8/16 KB caps only apply to the list display).
- Cost (`c`): fully parses `cost-state` including `modelUsage`
  (tokens/cache/cost per model) and API/tool durations.
- Agents pane: under each agent a `↳` line with its latest own action (tool
  call including status, text, spawn; thinking only as a fallback) — running
  agents in color, finished ones dimmed. Derived from the timeline at draw
  time, so it follows the tail reload live.
- Agent detail: with the Agents pane focused, the Detail pane shows the agent's
  model (`resolvedModel`), runtime, prompt and final report (last assistant
  text of its transcript; fallback: `outputFile`).
- Resume (`R`): `claude --resume <id>` runs on a PTY (`src/term.rs`,
  portable-pty + vt100) and is rendered as an embedded terminal view
  (`src/ui/term.rs`). `ctrl-q` only detaches the view — the process keeps
  running, you can still switch projects, and the Detail view of the same
  session follows live. Attached processes are terminated when the TUI exits
  (no orphans).
- Activity indicator: transcript mtime < 10 s ⇒ "working"; PTY attached but
  idle ⇒ "waiting for input" (session list + status bar).
- Search (`/`, `n`/`N`, `e`): substring over the (capped) event content,
  visible events only; matches are underlined in yellow.
- Thinking (`T`): `thinking` blocks are always parsed (`EventKind::Thinking`)
  but hidden by default — the selection stays a raw timeline index, and
  navigation goes through `visible_events()`.
- Edit diffs: the `structuredPatch` from the tool result is rendered as a
  unified diff (+green/−red, `@@` headers cyan) instead of JSON.
- Live graph: `src/live.rs::snapshot` only reads `agent-*.meta.json`
  (type, description, `toolUseId`, `spawnDepth`), transcript mtimes and the
  last line of each subagent transcript (`stop_reason: end_turn` ⇒ finished).
  Parents of nested agents: the transcript one level up that contains the
  `toolUseId`. Rebuilt on project switch and on every rescan; the state
  (running/waiting) is derived from the mtimes at draw time.
- Hooks (`src/hooks.rs`): tail-follow of `events.jsonl` just like transcripts
  (offset, incomplete last line is left in place, rotation ⇒ start over).
  A hook status only counts while it is credible: not ended, "working"
  < 1 h or waiting < 12 h old, the tmux pane is still running `claude`,
  and no newer session has taken over the same pane — otherwise the mtime
  heuristic applies.
- tmux (`src/tmux.rs`, `w`): `select-window` + `select-pane` +
  `switch-client` to the pane id from `$TMUX_PANE`; without hook data, the
  unique `claude` pane in the session's cwd.
- Analytics (`a`) / Fleet (`f`): pure aggregation over the index
  (`src/analytics.rs`), no extra file I/O.
- Background loading: `enter` loads sessions on a worker thread
  (`AppEvent::Loaded`, stale results are dropped by session id) — even 48 MB
  sessions no longer block the UI.
- Export: `src/export.rs::export_markdown` (prompts/replies untruncated via
  byte-span re-read, tool calls as one-liners).
