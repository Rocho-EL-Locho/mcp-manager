<div align="center">

<img src="src-tauri/icons/128x128.png" width="76" alt="mcp-manager" />

# mcp-manager

**Schlanke Desktop-App zum Verwalten der lokalen MCP-Server von Claude Code.**

Status auf einen Blick · Konfiguration bearbeiten · an/aus schalten · Scope umschalten ·
neue Server hinzufügen (auch per Link mit Claude-Hilfe) · sauber entfernen.

<sub>Tauri v2 · Rust + TypeScript · Linux</sub>

</div>

---

## Warum

Die MCP-Server von Claude Code liegen verstreut über mehrere Dateien
(`~/.claude.json`, `~/.mcp.json`, `~/.claude/settings*.json`, projektbezogene
`.mcp.json`) und drei Scopes. Man sieht nirgends auf einen Blick, welcher Server
läuft, welcher deaktiviert ist und wo er lebt. **mcp-manager** macht genau das:
eine ruhige, native Oberfläche für den ganzen Bestand.

## Funktionen

- **Überblick & Status** – alle Server nach Scope gruppiert, mit echtem
  Health-Check (verbunden / Fehler / Login nötig / deaktiviert). Der Status wird
  im Hintergrund geladen, die Liste ist sofort da.
- **Projekt-Browser** – alle Claude-Code-Projekte in einer ein-/ausblendbaren
  Seitenleiste; pro Projekt dessen `local`- und `project`-Server. Projekte lassen
  sich entfernen.
- **Bearbeiten / Hinzufügen / Entfernen** – Formular für `stdio` (command/args/env)
  und `http`/`sse` (url/headers). Entfernen zeigt eine nicht-destruktive
  Aufräum-Checkliste (Docker-Image, Cache, OAuth-Logout).
- **An/Aus** – `.mcp.json`-Server über die enable/disable-Listen, globale
  (user-scope) Server über einen sicheren Stash-and-restore-Mechanismus.
- **Scope wechseln** – `user` ↔ `local` ↔ `project`, verifiziert (erst im Ziel
  anlegen, dann aus der Quelle entfernen).
- **Server per Link einrichten** – ein Link (GitHub / npm / PyPI / Doku) genügt:
  ein headless-`claude`-Aufruf liest die Quelle und schlägt eine fertige
  Konfiguration vor, die du im Formular nur noch bestätigst.
- **OAuth** – `login` / `logout` für Connectoren und HTTP/SSE-Server.

## Wie es funktioniert

- **Alle Änderungen laufen über die offizielle `claude`-CLI** (`claude mcp …`) —
  nicht durch direktes Editieren der großen `~/.claude.json`. Das vermeidet
  Race-Conditions mit laufendem Claude Code. Nur kleine Dateien
  (`settings.local.json`, der Stash) werden atomar direkt geschrieben.
- **Das Rust-Backend besitzt die gesamte Logik** (CLI-Aufrufe + Dateizugriff);
  das Web-Frontend spricht ausschließlich über `invoke`-Commands — kein Shell-
  oder FS-Zugriff im Webview.
- **Secrets** (env-Werte, Header, Inline-Tokens in args) werden im Backend
  maskiert, bevor sie das Webview erreichen — Klartext nur auf ausdrückliches
  „anzeigen". Der Stash für deaktivierte Server liegt mit Modus `0600` im
  Nutzer-Config-Verzeichnis.

## Voraussetzungen

- Die `claude`-CLI im PATH (getestet mit 2.1.x). Override möglich per
  `MCP_MANAGER_CLAUDE_PATH`.
- **Rust** ≥ 1.77 und **Node** ≥ 18.
- Tauri-v2-Systempakete (Arch/CachyOS): `webkit2gtk-4.1`, `libsoup3`, `librsvg`,
  `base-devel`.

## Bauen & Starten

```bash
npm install
npm run tauri:dev      # Dev-Modus (Fenster öffnet sich)
npm run tauri:build    # Linux-Bundle (AppImage + deb) in src-tauri/target
```

> **Arch/CachyOS:** `npm run tauri:build` setzt bereits
> `APPIMAGE_EXTRACT_AND_RUN=1`, um den Fehler `failed to run linuxdeploy`
> (fehlendes FUSE2) zu vermeiden. Nur ein deb bauen:
> `npm run tauri build -- --bundles deb`.
>
> **Schlankste Variante:** einfach die Release-Binary
> `src-tauri/target/release/mcp-manager` (~3,6 MB) starten — sie nutzt das
> installierte System-`webkit2gtk`. Das AppImage (~100 MB) ist nur für portable
> Weitergabe nötig.

## Projektstruktur

```
src/                    Frontend (Vanilla TypeScript + Vite)
  ipc.ts                typisierte invoke-Wrapper
  icons.ts              Inline-Linien-Icons
  dom.ts                XSS-sicherer DOM-Helfer
  views/                serverList · serverDetail · serverForm · sidebar · assistant
src-tauri/src/
  claude_cli.rs         Wrapper um `claude` (arg-vec, cwd, Timeout, Prozessgruppen-Kill)
  config_read.rs        liest ~/.claude.json / settings*.json / .mcp.json
  parse.rs              toleranter Parser für `mcp list`/`get`
  mask.rs               Secret-Maskierung
  toggles.rs / stash.rs enable/disable + Ablage deaktivierter user-scope Server
  assistant.rs          headless `claude -p` + JSON-Extraktion
  commands.rs           alle #[tauri::command] (async, off-main-thread)
```

## Tests

Der Standardlauf enthält nur reine Unit-Tests:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Zusätzliche, **opt-in** Integrationstests laufen gegen die echte lokale Umgebung
(lesen bzw. mutieren `~/.claude.json` mit Wegwerf-Servern) und starten teils
`claude`:

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture
```

## Lizenz

[MIT](LICENSE)
