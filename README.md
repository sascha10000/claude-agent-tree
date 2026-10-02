# claude-agent-tree

Terminal-UI zur Visualisierung von Claude-Code-Sessions aus `~/.claude/projects`:
Projekte → Sessions (mit Titel, Kosten, Größe) → Timeline aller Agent-Aktionen
plus Flow-Graph des Hauptagenten mit seinen Subagenten. Laufende Sessions
aktualisieren sich live (Dateisystem-Watcher, Tail-Follow).

## Installation

**Voraussetzungen**

- Rust ≥ 1.88 (Edition 2024, let-chains) — `rustup update stable`
- Claude Code (`claude` im `PATH`) — nur für `n` (neue Session) und `R` (Resume)
- optional: tmux — für `w` (zur laufenden Session springen)

**1. Binary installieren**

```
git clone <repo-url> claude-agent-tree && cd claude-agent-tree
cargo install --path .        # → ~/.cargo/bin/claude-agent-tree
```

`~/.cargo/bin` muss im `PATH` sein (rustup richtet das normalerweise ein).
Alternativ ohne Installation: `cargo build --release` und
`./target/release/claude-agent-tree` direkt starten.

**2. Hooks einrichten (optional, empfohlen)**

```
claude-agent-tree --install-hooks
```

Erst *nach* Schritt 1 ausführen: der Hook wird mit dem **absoluten Pfad** des
gerade laufenden Binaries eingetragen. Wer stattdessen
`cargo run -- --install-hooks` nutzt, bindet die Hooks an `target/release` —
funktioniert, bricht aber bei `cargo clean`. Danach laufende Claude-Sessions
neu starten (neue Sessions sind sofort erfasst). Kontrolle: im TUI erscheint
nach dem ersten Ereignis `⚓ hooks` in der Statuszeile. Details siehe
[Hooks](#hooks-optional-empfohlen).

**3. Starten**

```
claude-agent-tree
```

**Aktualisieren**

```
git pull && cargo install --path . --force
```

Der Binary-Pfad bleibt gleich, die Hooks laufen also weiter. Nur wenn sich der
Pfad ändert (Binary verschoben, anderer Installationsweg),
`claude-agent-tree --install-hooks` erneut ausführen — bis dahin sind die
Hooks wirkungslos, stören aber nicht (`|| true`).

**Deinstallieren**

```
claude-agent-tree --uninstall-hooks   # zuerst, solange das Binary noch da ist
cargo uninstall claude-agent-tree
rm -rf ~/.claude/agent-tree           # Hook-Ereignislog
```

## Nutzung

Nach der Installation statt `cargo run --release --` einfach
`claude-agent-tree` verwenden.

```
cargo run --release            # TUI
cargo run --release -- --list  # Projekte/Sessions als Tabelle
cargo run --release -- --dump <sessionIdPrefix>    # Timeline + Agent-Baum als Text
cargo run --release -- --export <sessionIdPrefix>  # Session als Markdown-Datei
cargo run --release -- --root <pfad> ...           # alternatives Projekt-Verzeichnis
cargo run --release -- --install-hooks             # Claude-Code-Hooks für exakten Status
cargo run --release -- --uninstall-hooks           # …und wieder entfernen
```

### Hooks (optional, empfohlen)

`--install-hooks` trägt `claude-agent-tree --hook` in `~/.claude/settings.json`
ein (Backup: `settings.json.bak-agent-tree`; fremde Hooks bleiben unberührt,
mehrfaches Ausführen ist idempotent). Die Hooks laufen `async` und enden immer
mit Exit 0 — sie können Claude weder bremsen noch blockieren. Jede Session
schreibt dann eine kompakte Zeile pro Ereignis nach
`~/.claude/agent-tree/events.jsonl` (rotiert bei 4 MB; keine Tool-Inputs,
nur Einzeiler). Gewinn:

- exakter Status statt mtime-Heuristik, auch für Sessions in anderen Terminals
- `⚠` **wartet auf Erlaubnis** (Permission-Dialog) inkl. des betroffenen Tool-Aufrufs
- tmux-Pane jeder Session → `w` springt direkt hin

Laufende Sessions übernehmen neue Hooks erst nach einem Neustart.

## Tasten

| Taste | Aktion |
|---|---|
| `j`/`k`, `↓`/`↑` | Auswahl bewegen |
| `g` / `G` | Anfang / Ende |
| `enter` | öffnen; Detail: Spawn-Event ↔ Agent-Graph springen, sonst Vollansicht |
| `o` | Detail: Timeline-Reihenfolge umschalten (Standard: neueste oben) |
| `O` | Detail: Event in scrollbarer Vollansicht (ungekürzt, lazy von Platte) |
| `c` | Detail: Kosten/Token-Panel (pro Modell, Cache, API- vs. Tool-Dauer) |
| `t` | Detail: Agents-Pane als Zeit-Lanes (parallele Agenten sichtbar) |
| `R` | Session per `claude --resume` als eingebettetes Terminal öffnen (Browse + Detail) |
| `w` | zur Session springen: eingebettetes Terminal oder ihr tmux-Pane (Browse + Detail) |
| `ctrl-q` | Terminal-Ansicht verlassen — die Session läuft im Hintergrund weiter |
| `s` | Browse: Sortierung wechseln (mtime/cost/size/duration) |
| `a` | Browse: Analytics-Overlay (Kosten/Tokens über alle Projekte) |
| `A` | Browse: nur Projekte mit laufender / wartender Session zeigen (Toggle) |
| `f` | Browse: Fleet-Overlay (in den letzten 5 min aktive Sessions, enter = hinspringen) |
| `/` | Browse: Sessions filtern; Detail: Timeline durchsuchen |
| `n` | Browse: neue, leere `claude`-Session im Verzeichnis des gewählten Projekts (eingebettetes Terminal) |
| `n` / `N` | Detail: nächster / voriger Suchtreffer |
| `e` | Detail: zum nächsten Fehler/Denied springen |
| `T` | Detail: Thinking-Blöcke ein-/ausblenden |
| `x` | Detail: Session als Markdown exportieren (auch CLI `--export`) |
| `?` | Tastaturhilfe |
| `esc` / `h` | zurück / Overlay bzw. Suche schließen |
| `tab` | Pane wechseln; Detail-Pane zeigt bei Agents-Fokus den Agenten |
| `u`/`d`, PgUp/PgDn | schnell scrollen (Timeline, Vollansicht) |
| `r` | manuell neu laden |
| `q` | beenden |

## Statuszeichen

`✓` ok · `✗` Fehler · `⊘` verweigert · `…`/`◌` läuft noch · `⚒` Tool-Call ·
`⑂` Subagent-Spawn · `▸` User-Prompt · `✻` Assistant-Text

Session-Liste: `●` gelb = arbeitet gerade (Transcript wird geschrieben) ·
`▶` grün = angehängtes Terminal ist still — fertig bzw. wartet auf Eingabe.

Projekt-Liste: `⚠` rot = eine Session wartet auf Erlaubnis (nur mit Hooks) · `●` gelb = eine Session/ein Subagent arbeitet gerade ·
`▶` grün = wartet auf Eingabe · `○` cyan = in der letzten Stunde aktiv ·
`·` grau = nur ältere Sessions.

Live-Agents-Panel (Browse, unten rechts): alle in den letzten 5 min aktiven
Sessions des gewählten Projekts als Baum mit ihren (verschachtelten)
Subagenten — `●` läuft · `◌` im Tool-Call · `✓` fertig · `⊘` abgebrochen.
Ältere fertige Agenten werden zu `… +N earlier finished` zusammengefasst.

## Architektur

- `parser.rs` — toleranter Streaming-JSONL-Reader; eine unvollständige letzte
  Zeile (live geschriebene Datei) wird nicht konsumiert, sondern beim nächsten
  Watch-Event nachgelesen.
- `index.rs` — Listen kommen aus einem Tail-Scan (letzte 64 KB rückwärts) nach
  `ai-title`/`cost-state`, nie aus einem Vollparse; 48-MB-Sessions bleiben in
  der Liste kostenlos.
- `session.rs` — Vollparse beim Öffnen: Events bauen, tool_use↔tool_result
  joinen, Subagent-Transcripts (`<sessionId>/subagents/agent-*.jsonl`)
  chronologisch einmischen.
- `agent_tree.rs` — Agent-Baum über `meta.json.toolUseId` ↔ `tool_use.id`,
  beliebige Verschachtelungstiefe.
- `watch.rs` / `app.rs` / `ui/` — notify-Debouncer (250 ms), Event-Loop mit
  inkrementellem Reload (Datei-Offsets), ratatui-Rendering mit Fenster-Slicing.
- Vollansicht (`O`): Events speichern Byte-Spans ihrer JSONL-Zeilen; der
  ungekürzte Inhalt wird erst beim Öffnen von Platte nachgelesen (Speicher
  bleibt flach, 4/8/16-KB-Caps gelten nur für die Listendarstellung).
- Kosten (`c`): parst `cost-state` vollständig inkl. `modelUsage`
  (Token/Cache/Kosten pro Modell) und API-/Tool-Dauern.
- Agents-Pane: unter jedem Agenten eine `↳`-Zeile mit seiner neuesten
  eigenen Aktion (Tool-Call inkl. Status, Text, Spawn; Thinking nur als
  Fallback) — laufende Agenten farbig, fertige gedimmt. Wird zur Zeichenzeit
  aus der Timeline abgeleitet und folgt damit live dem Tail-Reload.
- Agent-Detail: bei Fokus auf dem Agents-Pane zeigt das Detail-Pane Modell
  (`resolvedModel`), Laufzeit, Prompt und den finalen Report des Agenten
  (letzter Assistant-Text seines Transcripts; Fallback: `outputFile`).
- Resume (`R`): `claude --resume <id>` läuft auf einem PTY (`src/term.rs`,
  portable-pty + vt100) und wird als eingebettete Terminal-Ansicht gerendert
  (`src/ui/term.rs`). `ctrl-q` löst nur die Ansicht — der Prozess lebt weiter,
  Projekte bleiben wechselbar, die Detail-Ansicht derselben Session folgt live.
  Beim Beenden des TUI werden angehängte Prozesse beendet (keine Waisen).
- Aktivitäts-Indikator: mtime des Transcripts < 10 s ⇒ „arbeitet“; PTY
  angehängt, aber still ⇒ „wartet auf Eingabe“ (Session-Liste + Statusbar).
- Suche (`/`, `n`/`N`, `e`): Substring über den (gekappten) Event-Inhalt,
  nur sichtbare Events; Treffer sind gelb unterstrichen.
- Thinking (`T`): `thinking`-Blöcke werden immer geparst
  (`EventKind::Thinking`), aber standardmäßig ausgeblendet — die Auswahl
  bleibt ein roher Timeline-Index, Navigation läuft über `visible_events()`.
- Edit-Diffs: `structuredPatch` aus dem Tool-Result wird als Unified Diff
  gerendert (+grün/−rot, `@@`-Header cyan) statt als JSON.
- Live-Graph: `src/live.rs::snapshot` liest nur `agent-*.meta.json`
  (Typ, Beschreibung, `toolUseId`, `spawnDepth`), Transcript-mtimes und die
  letzte Zeile jedes Subagent-Transcripts (`stop_reason: end_turn` ⇒ fertig).
  Eltern verschachtelter Agenten: das Transcript eine Ebene höher, das die
  `toolUseId` enthält. Neuaufbau bei Projektwechsel und jedem Rescan; der
  Zustand (läuft/wartet) wird beim Zeichnen aus den mtimes abgeleitet.
- Hooks (`src/hooks.rs`): Tail-Follow von `events.jsonl` wie bei Transcripts
  (Offset, unvollständige letzte Zeile bleibt liegen, Rotation ⇒ von vorn).
  Ein Hook-Status zählt nur, solange er glaubwürdig ist: nicht beendet,
  „working“ < 1 h bzw. wartend < 12 h alt, das tmux-Pane läuft noch `claude`,
  und keine neuere Session hat dasselbe Pane übernommen — sonst greift die
  mtime-Heuristik.
- tmux (`src/tmux.rs`, `w`): `select-window` + `select-pane` +
  `switch-client` auf die Pane-Id aus `$TMUX_PANE`; ohne Hook-Daten das
  eindeutige `claude`-Pane im cwd der Session.
- Analytics (`a`) / Fleet (`f`): reine Aggregation über den Index
  (`src/analytics.rs`), kein zusätzliches Datei-I/O.
- Hintergrund-Laden: `enter` lädt Sessions auf einem Worker-Thread
  (`AppEvent::Loaded`, veraltete Ergebnisse werden per Session-Id verworfen) —
  auch 48-MB-Sessions blockieren das UI nicht mehr.
- Export: `src/export.rs::export_markdown` (Prompts/Antworten ungekürzt via
  Byte-Span-Re-Read, Tool-Aufrufe als Einzeiler).
