# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Projektsprache: **Deutsch** — Code-Kommentare, Doc-Comments, Roadmap, UI-Texte und
Commit-Bodies sind deutsch. Nur `README.md`, `SECURITY.md` und die
GitHub-Templates sind englisch (öffentliche Außendarstellung). Neue Kommentare
und UI-Strings daher auf Deutsch schreiben.

## Arbeiten mit Serena

Dieses Projekt wird mit dem **Serena-MCP-Server** entwickelt. Die
Projektkonfiguration unter `.serena/project.yml` (`project_name: mcp-manager`)
ist **nicht eingecheckt** — ein frischer Clone hat sie nicht, Serena legt sie
beim ersten `activate_project` selbst an. Sie ist im Repo aber auch **nicht
ignoriert**: `.gitignore` nennt `.serena/` nicht. Deshalb vor der ersten
Serena-Session `.serena/` in `.git/info/exclude` eintragen (wie `roadmap/`) —
sonst stehen `project.yml`, `.gitignore` und (sobald es Memories gibt)
`memories/` als untracked im `git status`; `cache/` und `project.local.yml`
deckt Serenas eigenes `.serena/.gitignore` bereits ab. Dieser Abschnitt
beschreibt die lokale Arbeitsweise, nicht eingecheckten Repo-Inhalt.

- **Zu Sessionbeginn**: `activate_project` für `mcp-manager` (legt die
  Konfiguration beim ersten Mal an) und einmalig `initial_instructions`
  (Serena Instructions Manual) lesen.
- **TypeScript (`src/`) und Rust (`src-tauri/src/`) sind symbolisch indexiert**
  (`language_servers: [typescript, rust]`, rust-analyzer ist installiert). Für
  Entdeckung und Edits die Serena-Tools statt Read/Edit nutzen:
  `get_symbols_overview` → `find_symbol` (mit `include_body` erst wenn nötig) →
  `find_referencing_symbols`; zum Ändern `replace_symbol_body`,
  `insert_after_symbol`, `replace_content`, `replace_in_files`, `rename_symbol`.
  Das ist hier real relevant, weil `commands.rs` (~2230 Z.), `introspect.rs`
  (~1700 Z.), `src/main.ts` (~740 Z.) und `src/views/serverDetail.ts` (~620 Z.)
  zu groß sind, um sie sinnvoll komplett zu lesen — immer per Symbol einsteigen.
- **Erkenntnisse als Serena-Memories** (`write_memory`) ablegen, statt diese
  Datei aufzublähen — es existieren aktuell noch keine.

## Befehle

```bash
npm install
npm run tauri:dev      # Dev-Modus (startet Vite auf :1420 + Tauri-Fenster)
npm run build          # tsc --noEmit && vite build  — der Frontend-"Lint"
npm run tauri:build    # Linux-Bundle (AppImage + deb) in src-tauri/target

cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml -- mask::tests::echte_secrets_werden_erkannt
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture resolve_claude_precedence
```

- Es gibt **keinen ESLint/Prettier und keinen Frontend-Testrunner**. Die
  Typprüfung aus `npm run build` ist das einzige statische Frontend-Gate
  (`tsconfig.json` ist strict inkl. `noUnusedLocals`/`noUnusedParameters`).
- **Vor jedem PR müssen `cargo test …` und `npm run build` grün sein**
  (steht so in der PR-Checkliste).
- `-- --ignored` sind Opt-in-Tests gegen die **echte lokale Umgebung**: sie lesen
  bzw. mutieren `~/.claude.json` mit Wegwerf-Servern, mutieren Prozess-Env oder
  starten `claude`/echte MCP-Server. Neue Tests dieser Art konsequent mit
  `#[ignore]` markieren (bestehendes Muster in `commands.rs`, `introspect.rs`,
  `settings.rs`, `claude_cli.rs`).
- `MCP_MANAGER_CLAUDE_PATH` überschreibt die `claude`-Auflösung (stärkster
  Override, vor der Einstellung und vor `which`) — der Hebel für Tests/Scripting.
- Schlankster Lauf ohne Bundling: `src-tauri/target/release/mcp-manager`.

## Architektur

Tauri-v2-Desktop-App: **Rust-Backend besitzt die gesamte Logik**, das Webview ist
eine reine Darstellungsschicht ohne Shell-/FS-Rechte
(`src-tauri/capabilities/default.json` erlaubt nur `core:default` +
`notification:default`).

### Der zentrale Datenfluss

```
JSON-Dateien (Definition, autoritativ)   `claude mcp list/get` (Status)
  ~/.claude.json (user + projects[*])            │
  <projekt>/.mcp.json                            │
  ~/.claude/settings{,.local}.json  und          │
  <projekt>/.claude/settings.local.json          │
        │ config_read.rs                         │ claude_cli.rs → parse.rs
        └──────────────► commands.rs ◄───────────┘
                              │  mask.rs (Secrets raus)
                              ▼
                    MergedServer (models.rs)
                              │  #[tauri::command]
                              ▼
                    src/ipc.ts → src/main.ts → src/views/*
```

`MergedServer` ist der einzige Typ, den das Frontend zum Rendern braucht:
Definition + Status + `enabled`/`editable`/`has_secrets`/`collision` +
optionale Introspektions- und Preflight-Zusatzinfos.

### Scopes und Präzedenz

`Scope` = `user` (global, `~/.claude.json`) · `local` (projekt-privat,
`~/.claude.json → projects[pfad]`) · `project` (eingecheckt, `.mcp.json`).
Bei Namensgleichheit gewinnt in Claude Code **local > project > user**; das
bildet `list_conflicts` in `ConflictInfo.effective_scope` ab. Der cwd des
`claude`-Aufrufs entscheidet über den Scope: `user` → Home, `local`/`project` →
Projektpfad (siehe `run_claude(..., cwd, ...)`). Ohne ausgewähltes Projekt ist
der Projektpfad das **Home-Verzeichnis** (`config_read::default_project_path`) —
`project`-Scope liest dann `~/.mcp.json`.

### Einen Command hinzufügen (drei Stellen, immer alle drei)

1. `#[tauri::command]`-Funktion in `src-tauri/src/commands.rs`
2. Eintrag in `tauri::generate_handler![…]` in `src-tauri/src/lib.rs`
3. typisierter Wrapper + Interface in `src/ipc.ts`

`commands.rs` ist bewusst eine dünne Orchestrierungsschicht über
`claude_cli`/`config_read`/`mask`/`parse` — echte Logik gehört in das jeweilige
Fachmodul (`introspect.rs`, `snapshot.rs`, `registry.rs`, `preflight.rs`,
`logview.rs`, `metrics.rs`, `settings.rs`, `stash.rs`, `toggles.rs`,
`conflicts.rs`). Fachlich neutrale Kleinhelfer (`config_dir()`, `unix_now()`)
liegen in `util.rs` — nicht in einem Fachmodul, sonst hängen fremde Module am
falschen Feature.

### Serialisierung der IPC-Payloads

**Standard ist snake_case** (serde-Default, ohne `rename`): so sind alle
Konfigurations-/Verwaltungs-Payloads gehalten (`MergedServer`, `ConflictInfo`,
`ProjectInfo`, `AppSettings`, Snapshot-Typen) und so spiegelt `src/ipc.ts` sie.

**Genau eine Ausnahme, bewusst:** die MCP-Introspektions-Familie schreibt
camelCase, weil sie die Schreibweise des MCP-Protokolls bzw. der bereits
ausgelieferten `Introspection` übernimmt — `Introspection`
(`serverName`/`connectMs`/`introspectedAt`), `McpTool` (`inputSchema`),
`McpResource` (`mimeType`), `PlaygroundResult` (`isError`/`durationMs`) und
`MetricPoint` (`statusKind`/`connectMs`, identisch zu `Introspection.connectMs`,
aus dem es entsteht). `MetricPoint` wird zusätzlich nach `metrics.json`
persistiert — ein Umbenennen macht bestehende Dateien unlesbar.

**Maßgeblich ist der Typ, nicht der Feldname.** `MergedServer` ist vollständig
snake_case — also `connect_ms`, auch wo der Wert unverändert aus
`Introspection.connectMs` stammt (`commands.rs`: `s.connect_ms = intro.connect_ms;`).
Derselbe Messwert heißt in der camelCase-Familie eben anders; das ist kein
Regelbruch, sondern die Typgrenze. Wer `MergedServer` um ein
introspektionsnahes Feld erweitert, schreibt es snake_case.

Neue Payloads bekommen snake_case, außer sie gehören zu der oben aufgezählten
Familie. Zwei weitere Stellen sehen nur wie Ausnahmen aus:
`registry.rs` mit `rename_all = "camelCase"` ist **Deserialisierung der fremden
Registry-API**, kein Vorbild für eigene Payloads; `ServerEntry.transport` trägt
`#[serde(rename = "type")]`, weil der Typ das **fremde Format der
Claude-Config** (`.mcp.json`, `~/.claude.json`) abbildet — der Feldname ist dort
vorgegeben.

### Zustand im Backend

`AppState` (in `commands.rs`, via `.manage()`) hält: Status-Cache
(Projektpfad → Server → letzter Stand), Introspektions-Cache (Key
`scope::name::projektpfad`), In-Memory-Einstellungen, Metrik-Historie, laufende
Log-Sessions und den Registry-Cache (5-min-TTL). Der Exit-Hook in `lib.rs` killt
alle Log-Sessions, damit keine `npx`/`uvx`-Prozesse zurückbleiben.

### Zweiphasiges Laden im Frontend

`refresh()` in `src/main.ts` lädt erst die Liste mit gecachtem Status
(`listServers(..., healthCheck=false)`) und rendert sofort, dann im Hintergrund
den frischen Health-Status (`healthCheck=true`) und meldet Verschlechterungen als
Desktop-Notification. Ein monoton wachsender `refreshSeq`-Zähler verwirft
Ergebnisse veralteter Läufe — **dieses Guard-Muster bei jedem neuen asynchronen
Ladepfad beibehalten**.

### Eigener MCP-Client

`introspect.rs` implementiert den MCP-Handshake selbst (`initialize`,
`tools/list`, `resources/list`, `prompts/list`, `tools/call`, `resources/read`,
`prompts/get`) — stdio über einen Subprozess, HTTP/SSE über `ureq`. Bewusst
**blockierend mit Threads/Channels statt tokio/reqwest**; neue Netz-/Prozesslogik
in dieses Modell einpassen, nicht async einführen.

## Architektur-Leitplanken (nicht verhandelbar)

- **Claude-Code-Konfiguration wird ausschließlich über die `claude`-CLI geändert**
  (`claude_cli.rs` → `claude mcp add/remove/…`), nie durch direktes Schreiben von
  `~/.claude.json` — sonst Race Conditions mit laufendem Claude Code.
  Dokumentierte Ausnahmen, jeweils mit Begründung am Code:
  `delete_project` (die CLI kann keinen `projects[…]`-Eintrag löschen) und
  `snapshot.rs::restore`/`restore_in` (es gibt kein CLI-Äquivalent zum
  Zurückspielen eines Gesamtzustands; abgesichert durch Verfahren statt Technik:
  die UI warnt, dass Claude Code dabei nicht laufen soll, und legt vorher einen
  Auto-Snapshot an). Neue Direktschreibzugriffe brauchen dieselbe Behandlung —
  Doc-Comment am Code **und** ein Eintrag hier.
- **Direkt geschrieben werden nur kleine, eigene Dateien — und zwar atomar**
  (Temp-Datei + `rename`, Muster `toggles::atomic_write_json`): die
  enable/disable-Arrays in `<projekt>/.claude/settings.local.json` bzw. — ohne
  Projektpfad — `~/.claude/settings.local.json`, sowie im Config-Dir
  (`$XDG_CONFIG_HOME/mcp-manager`, sonst `~/.config/mcp-manager`):
  `settings.json`, `stash.json`, `metrics.json`, `snapshots/`.
  Dateien mit Klartext-Secrets bekommen Mode `0600`.
- **Secrets werden im Backend maskiert** (`mask.rs`, `MASK = "••••••••"`), bevor
  sie das Webview erreichen — env-Werte, Header, inline Tokens in args. Klartext
  nur bei explizitem `reveal`. Auch stderr-Auszüge, Introspektions-Schemas und
  Playground-Ergebnisse laufen durch die Redaktion.
- **Prozesse sicher starten**: nur Arg-Vektoren (nie Shell-Strings), eigene
  Prozessgruppe (`process_group(0)`), stdout/stderr nebenläufig leeren (sonst
  Deadlock bei vollem Pipe-Puffer), Timeout mit `killpg(SIGKILL)` auf die ganze
  Gruppe. Referenz: `claude_cli.rs::run_claude`.
- **Destruktionssicher**: erst im Ziel anlegen und verifizieren, dann in der
  Quelle löschen (Muster `set_scope`); vor destruktiven Aktionen sichern
  (Stash-/Snapshot-Muster).
- **Kein Framework im Frontend**: Vanilla TS. DOM ausschließlich über `h()` /
  `clear()` / `svgEl()` aus `src/dom.ts` — die nutzen nur `textContent`/
  `createTextNode`, also **niemals `innerHTML`** (Servernamen, args und env-Werte
  sind Fremddaten). Modals via `modal.ts`, Bestätigungen via `confirm.ts`
  (inkl. Zusatz-Controls über `extra` — keinen Submit-Zyklus von Hand nachbauen),
  Formularfelder via `form.ts`, Scope-Beschriftung/-Auswahl via `scope.ts`
  (`SCOPE_LABEL`/`scopeSelect`), Feedback via `toast.ts`, Icons via `icons.ts`.
  `styles.css` wird sowohl aus `index.html` verlinkt (Dev-Server) als auch in
  `main.ts` importiert (damit Vite es in den Produktions-Build aufnimmt).
  `src/vite-env.d.ts` (`/// <reference types="vite/client" />`) deklariert
  `*.css` für TypeScript und gehört zu diesem Import — nicht entfernen (PR #27).
- **Neue Abhängigkeiten sparsam** und nur mit Begründung im PR. Aktuell bewusst
  minimal: `tauri`, `serde`, `serde_json`, `thiserror`, `wait-timeout`, `libc`,
  `ureq`, `tauri-plugin-notification`; Frontend nur `@tauri-apps/api` +
  `@tauri-apps/plugin-notification`.

### Duplizierte Logik, die synchron bleiben muss

- `src/transport.ts` spiegelt `commands.rs::transport_of` (explizites `type`
  gewinnt, sonst URL mit `/sse` ⇒ sse / sonst http, sonst `command` ⇒ stdio).
- `src/constants.ts` spiegelt die Grenzwerte aus `settings.rs::validate`.
  **Maßgeblich ist immer das Backend**; das Frontend prüft nur Basissanität, damit
  Front- und Backend nicht in Annahme/Ablehnung auseinanderlaufen.
- `src/views/logView.ts::EVENT` spiegelt `logview.rs::EVENT_NAME` (`"mcp-log"`) —
  weichen sie ab, kommt kein einziger Log-Batch im Webview an, ohne Fehlermeldung.
- `src/constants.ts::LOG_RING_CAPACITY` spiegelt `logview.rs::RING_CAPACITY`
  (2000 Zeilen). Der Deckel muss im Webview mitgezogen werden: das Backend
  begrenzt nur seinen Ring, emittiert aber jede Zeile (`BATCH_INTERVAL` ist eine
  maximale Wartezeit, keine Mindestpause) — sonst wachsen Puffer und DOM-Knoten
  des Diagnose-Panels bei einem Server in der stderr-Schleife unbegrenzt.

## Roadmap-Workflow

`roadmap/` enthält je eine ausführbare Feature-Spezifikation (Ziel, Code-Kontext,
Umsetzungsplan, Edge Cases, Tests, Akzeptanzkriterien) plus `roadmap/README.md`
mit Phasenplan und Abhängigkeiten.

- **Der Ordner ist absichtlich lokal**: über `.git/info/exclude` vom Tracking
  ausgeschlossen. Roadmap-Dateien **niemals committen oder in PRs aufnehmen** —
  Statuspflege (`Status: In Arbeit` / `Fertig (PR #NN)`, Checkboxen) bleibt eine
  rein lokale Dateiänderung.
- Vor Beginn eines Features: die Feature-Datei vollständig lesen, dann die unter
  „Kontext im Code" genannten Dateien. Abhängigkeiten unter „Abhängigkeiten"
  müssen `Fertig` sein.
- Für die Umsetzung eines Features/Issues gibt es den lokalen Skill
  **`/feature-pipeline`** (`.claude/skills/feature-pipeline/`): Plan →
  Umsetzung → Review-Schleife → manuelle Testliste; Commit/PR erst nach
  Test-OK des Nutzers.

## Git-Konventionen

- Ein Feature = ein Branch `feat/<dateiname-ohne-nummer>` = ein PR, im PR-Text
  `Closes #NN`.
- Conventional Commits (`feat:`, `fix:`, `chore:`, `ci:`, `chore(deps):`), Betreff
  englisch, PR-Nummer am Ende: `feat: measure and show MCP connect latency (#26)`.
- PR-Checkliste aus `.github/PULL_REQUEST_TEMPLATE.md` abarbeiten; config-mutierende
  Änderungen zusätzlich gegen die echte `claude`-CLI verifizieren
  (`-- --ignored`).
- Release: Tag `v*` pusht → `.github/workflows/release.yml` baut AppImage + deb.
  GitHub-Actions sind auf Commit-SHAs gepinnt (Supply-Chain-Härtung, Dependabot
  hält sie aktuell) — beim Ändern die Pinnung samt Versionskommentar beibehalten.
