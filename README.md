# claude-agent-tree

Terminal-UI zur Visualisierung von Claude-Code-Sessions aus `~/.claude/projects`:
Projekte → Sessions (mit Titel, Kosten, Größe) → Timeline aller Agent-Aktionen
plus Flow-Graph des Hauptagenten mit seinen Subagenten. Laufende Sessions
aktualisieren sich live (Dateisystem-Watcher, Tail-Follow).

## Nutzung

```
cargo run --release            # TUI
cargo run --release -- --list  # Projekte/Sessions als Tabelle
cargo run --release -- --dump <sessionIdPrefix>    # Timeline + Agent-Baum als Text
cargo run --release -- --export <sessionIdPrefix>  # Session als Markdown-Datei
cargo run --release -- --root <pfad> ...           # alternatives Projekt-Verzeichnis
```

## Tasten

| Taste | Aktion |
|---|---|
| `j`/`k`, `↓`/`↑` | Auswahl bewegen |
| `g` / `G` | Anfang / Ende |
| `enter` | öffnen; Detail: Spawn-Event ↔ Agent-Graph springen, sonst Vollansicht |
| `o` | Detail: Event in scrollbarer Vollansicht (ungekürzt, lazy von Platte) |
| `c` | Detail: Kosten/Token-Panel (pro Modell, Cache, API- vs. Tool-Dauer) |
| `t` | Detail: Agents-Pane als Zeit-Lanes (parallele Agenten sichtbar) |
| `R` | Session per `claude --resume` als eingebettetes Terminal öffnen (Browse + Detail) |
| `ctrl-q` | Terminal-Ansicht verlassen — die Session läuft im Hintergrund weiter |
| `s` | Browse: Sortierung wechseln (mtime/cost/size/duration) |
| `a` | Browse: Analytics-Overlay (Kosten/Tokens über alle Projekte) |
| `f` | Browse: Fleet-Overlay (in den letzten 5 min aktive Sessions, enter = hinspringen) |
| `/` | Browse: Sessions filtern; Detail: Timeline durchsuchen |
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
- Vollansicht (`o`): Events speichern Byte-Spans ihrer JSONL-Zeilen; der
  ungekürzte Inhalt wird erst beim Öffnen von Platte nachgelesen (Speicher
  bleibt flach, 4/8/16-KB-Caps gelten nur für die Listendarstellung).
- Kosten (`c`): parst `cost-state` vollständig inkl. `modelUsage`
  (Token/Cache/Kosten pro Modell) und API-/Tool-Dauern.
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
- Analytics (`a`) / Fleet (`f`): reine Aggregation über den Index
  (`src/analytics.rs`), kein zusätzliches Datei-I/O.
- Hintergrund-Laden: `enter` lädt Sessions auf einem Worker-Thread
  (`AppEvent::Loaded`, veraltete Ergebnisse werden per Session-Id verworfen) —
  auch 48-MB-Sessions blockieren das UI nicht mehr.
- Export: `src/export.rs::export_markdown` (Prompts/Antworten ungekürzt via
  Byte-Span-Re-Read, Tool-Aufrufe als Einzeiler).
