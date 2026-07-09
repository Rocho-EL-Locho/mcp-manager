<div align="center">

<img src="src-tauri/icons/128x128.png" width="76" alt="mcp-manager" />

# mcp-manager

**A lean desktop app for managing Claude Code's local MCP servers.**

See status at a glance · edit configuration · toggle on/off · switch scope ·
add new servers (optionally with Claude's help via a link) · remove them cleanly.

<sub>Tauri v2 · Rust + TypeScript · Linux</sub>

</div>

---

## Why

Claude Code's MCP servers are scattered across several files
(`~/.claude.json`, `~/.mcp.json`, `~/.claude/settings*.json`, per-project
`.mcp.json`) and three scopes. There's no single place to see which server is
running, which is disabled, and where it lives. **mcp-manager** is exactly that:
a calm, native interface for the whole set.

## Features

- **Overview & status** – all servers grouped by scope, with a real health
  check (connected / failed / needs auth / disabled). Status loads in the
  background, so the list appears instantly.
- **Runtime preflight** – checks whether the runtime a server needs
  (`node`/`npx`, `python`/`uvx`, `docker`, …) is actually on `PATH`. A missing
  runtime is flagged in the list at a glance and the detail view shows the
  detected version or an actionable install/PATH hint — turning a cryptic
  "failed" into a clear next step.
- **Project browser** – every Claude Code project in a collapsible sidebar; per
  project its `local`- and `project`-scope servers. Projects can be removed.
- **Edit / add / remove** – a form for `stdio` (command/args/env) and
  `http`/`sse` (url/headers). Removal shows a non-destructive cleanup checklist
  (Docker image, cache, OAuth logout).
- **On/off** – `.mcp.json` servers via the enable/disable lists, global
  (user-scope) servers via a safe stash-and-restore mechanism.
- **Change scope** – `user` ↔ `local` ↔ `project`, verified (create in the
  target first, then remove from the source).
- **Set up a server from a link** – paste a link (GitHub / npm / PyPI / docs):
  a headless `claude` call reads the source and proposes a ready-made
  configuration that you just confirm in the form.
- **OAuth** – `login` / `logout` for connectors and HTTP/SSE servers.

## How it works

- **All changes go through the official `claude` CLI** (`claude mcp …`) rather
  than editing the large `~/.claude.json` directly. This avoids race conditions
  with a running Claude Code. Only small files (`settings.local.json`, the
  stash) are written directly, atomically.
- **The Rust backend owns all logic** (CLI calls + file access); the web
  frontend talks to it exclusively through `invoke` commands — no shell or
  filesystem access in the webview.
- **Secrets** (env values, headers, inline tokens in args) are masked in the
  backend before they reach the webview — plaintext only on an explicit
  "reveal". The stash for disabled servers is stored with mode `0600` in the
  user config directory.

## Requirements

- The `claude` CLI on your PATH (tested with 2.1.x). Override with
  `MCP_MANAGER_CLAUDE_PATH`.
- **Rust** ≥ 1.77 and **Node** ≥ 18.
- Tauri v2 system packages (Arch/CachyOS): `webkit2gtk-4.1`, `libsoup3`,
  `librsvg`, `base-devel`.

## Build & run

```bash
npm install
npm run tauri:dev      # dev mode (opens a window)
npm run tauri:build    # Linux bundle (AppImage + deb) in src-tauri/target
```

> **Arch/CachyOS:** `npm run tauri:build` already sets
> `APPIMAGE_EXTRACT_AND_RUN=1` to avoid the `failed to run linuxdeploy` error
> (missing FUSE2). To build only a deb: `npm run tauri build -- --bundles deb`.
>
> **Leanest option:** just run the release binary
> `src-tauri/target/release/mcp-manager` (~3.6 MB) — it uses the system
> `webkit2gtk`. The AppImage (~100 MB) is only needed for portable distribution.

## Project layout

```
src/                    Frontend (vanilla TypeScript + Vite)
  ipc.ts                typed invoke wrappers
  icons.ts              inline line icons
  dom.ts                XSS-safe DOM helper
  views/                serverList · serverDetail · serverForm · sidebar · assistant
src-tauri/src/
  claude_cli.rs         wrapper around `claude` (arg vec, cwd, timeout, process-group kill)
  config_read.rs        reads ~/.claude.json / settings*.json / .mcp.json
  parse.rs              tolerant parser for `mcp list`/`get`
  mask.rs               secret masking
  toggles.rs / stash.rs enable/disable + storage of disabled user-scope servers
  assistant.rs          headless `claude -p` + JSON extraction
  commands.rs           all #[tauri::command] (async, off the main thread)
```

## Tests

The default run contains unit tests only:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Additional, **opt-in** integration tests run against the real local environment
(they read / mutate `~/.claude.json` with throwaway servers) and partly launch
`claude`:

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture
```

## License

[MIT](LICENSE)
