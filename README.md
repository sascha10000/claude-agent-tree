# claude-agent-tree

Terminal-UI zur Visualisierung von Claude-Code-Sessions aus `~/.claude/projects`:
Projekte → Sessions (mit Titel, Kosten, Größe) → Timeline aller Agent-Aktionen
plus Flow-Graph des Hauptagenten mit seinen Subagenten. Laufende Sessions
aktualisieren sich live (Dateisystem-Watcher, Tail-Follow).

## Nutzung

```
cargo run --release            # TUI
cargo run --release -- --list  # Projekte/Sessions als Tabelle
cargo run --release -- --dump <sessionIdPrefix>  # Timeline + Agent-Baum als Text
```

## Tasten

| Taste | Aktion |
|---|---|
| `j`/`k`, `↓`/`↑` | Auswahl bewegen |
| `g` / `G` | Anfang / Ende |
| `enter` | öffnen; in der Detail-Ansicht: Sprung Timeline ↔ Agent-Graph |
| `esc` / `h` | zurück |
| `tab` | Pane wechseln |
| `/` | Sessions filtern |
| `u`/`d`, PgUp/PgDn | schnell scrollen (Timeline) |
| `r` | manuell neu laden |
| `q` | beenden |

## Statuszeichen

`✓` ok · `✗` Fehler · `⊘` verweigert · `…`/`◌` läuft noch · `⚒` Tool-Call ·
`⑂` Subagent-Spawn · `▸` User-Prompt · `✻` Assistant-Text

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
