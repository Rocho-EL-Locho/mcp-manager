//! Alle `#[tauri::command]`-Funktionen. Dünne Orchestrierungsschicht über
//! claude_cli / config_read / mask / parse.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use tauri::State;

use crate::claude_cli::{home_dir, resolve_claude, run_claude};
use crate::config_read::{
    claude_json_path, collect_definitions, collect_disabled, default_project_path,
    project_settings_local_path, read_json_value, settings_local_path, ScopedEntry,
};
use crate::mask::{
    entry_has_secrets, mask_entry, mask_summary, redact_json, redact_secrets, summarize_entry,
};
use crate::models::{
    AppError, ClaudeInfo, ConflictDefinition, ConflictInfo, Introspection, MergedServer,
    PlaygroundRequest, PlaygroundResult, ProjectInfo, Scope, ServerEntry, ServerStatus,
};
use crate::parse::{failure_detail, parse_list, status_from_text};
use crate::preflight::RuntimePreflight;
use crate::settings::AppSettings;

const GET_TIMEOUT: Duration = Duration::from_secs(20);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// Zeitbudget für den Introspektions-Handshake. Großzügig, weil der erste
/// `npx`/`uvx`-Start (Download/Cold-Start) spürbar dauern kann.
const INTROSPECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Zeitbudget für einen Playground-Aufruf (Handshake + ein Request). Länger als
/// INTROSPECT_TIMEOUT, da ein `tools/call` echte Arbeit verrichten darf.
const PLAYGROUND_TIMEOUT: Duration = Duration::from_secs(60);

/// Zuletzt bekannter Status + Kurzbeschreibung eines Servers (für den Cache).
#[derive(Clone)]
struct CachedItem {
    status: ServerStatus,
    summary: String,
}

/// Cache: Projektpfad -> (Servername -> letzter Stand). Macht Ansichtswechsel
/// und den Schnell-Start ohne erneuten Health-Check flott.
type StatusCache = Mutex<HashMap<String, HashMap<String, CachedItem>>>;

/// Cache der Introspektions-Ergebnisse. Key: "scope::name::projektpfad".
/// Wird nur auf ausdrückliche Nutzer-Aktion befüllt (startet den Server-Prozess).
type IntrospectionCache = Mutex<HashMap<String, Introspection>>;

pub struct AppState {
    status_cache: StatusCache,
    introspection_cache: IntrospectionCache,
    /// Persistente App-Einstellungen, beim Start geladen und im Speicher gecacht.
    settings: RwLock<AppSettings>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            status_cache: Mutex::new(HashMap::new()),
            introspection_cache: Mutex::new(HashMap::new()),
            // Einstellungen beim Start laden (fehlend/korrupt -> Defaults, nie Fehler).
            settings: RwLock::new(crate::settings::load()),
        }
    }
}

impl AppState {
    /// Kopie der aktuellen Einstellungen (Lese-Snapshot; Guard wird sofort frei).
    fn settings(&self) -> AppSettings {
        self.settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Ermittelt den Transport eines Servers – spiegelt `src/transport.ts`:
/// explizites `type` gewinnt, sonst aus der URL (`…/sse` ⇒ sse, sonst http),
/// sonst `command` ⇒ stdio (Default stdio).
fn transport_of(entry: &ServerEntry) -> &'static str {
    match entry.transport.as_deref() {
        Some("stdio") => return "stdio",
        Some("http") => return "http",
        Some("sse") => return "sse",
        _ => {}
    }
    if let Some(url) = entry.url.as_deref() {
        if url.trim_end_matches('/').ends_with("/sse") {
            return "sse";
        }
        return "http";
    }
    "stdio"
}

/// Löst die **unmaskierte** Definition eines Servers auf (Config, dann Stash für
/// deaktivierte user-Server). Der Handshake/Playground braucht die echten
/// env/args/headers. Fehlt sie, `AppError`.
fn resolve_entry(
    scope: Scope,
    name: &str,
    project_path: &Option<String>,
) -> Result<ServerEntry, AppError> {
    let dir = resolve_project_dir(project_path.clone());
    collect_definitions(&dir)
        .into_iter()
        .find(|d| d.scope == scope && d.name == name)
        .map(|d| d.entry)
        .or_else(|| {
            (scope == Scope::User)
                .then(|| crate::stash::peek(name).map(|i| i.entry))
                .flatten()
        })
        .ok_or_else(|| AppError::Io("Server-Definition nicht gefunden".into()))
}

/// Stabiler Cache-Schlüssel für die Introspektion eines Servers.
fn introspection_key(scope: Scope, name: &str, project_path: &Option<String>) -> String {
    let dir = resolve_project_dir(project_path.clone());
    format!("{}::{}::{}", scope.cli_value(), name, dir.to_string_lossy())
}

/// Prüft beim Start, ob die claude-CLI verfügbar ist, und liefert ihre Version.
#[tauri::command]
pub async fn check_claude(state: State<'_, AppState>) -> Result<ClaudeInfo, AppError> {
    let settings = state.settings();
    let Some(path) = resolve_claude(settings.claude_path()) else {
        return Ok(ClaudeInfo {
            path: String::new(),
            version: String::new(),
            ok: false,
        });
    };

    let out = run_claude(&path, &["--version"], None, Duration::from_secs(10))?;
    let version = out.stdout.trim().to_string();
    Ok(ClaudeInfo {
        path: path.to_string_lossy().into_owned(),
        ok: out.success() && !version.is_empty(),
        version,
    })
}

fn resolve_project_dir(project_path: Option<String>) -> PathBuf {
    project_path
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_project_path)
}

fn origin_for(scope: Scope) -> String {
    match scope {
        Scope::User => "user",
        Scope::Local => "local",
        Scope::Project => "project",
    }
    .to_string()
}

/// Preflight-Kurzform für die Liste: fehlt der (stdio-)Befehl auf PATH?
/// `None` für Server ohne Befehl (HTTP/SSE) – dort gibt es nichts zu prüfen.
/// Rein dateisystembasiert (kein Subprozess), daher auch im Schnellmodus billig.
fn runtime_missing_for(entry: &ServerEntry) -> Option<bool> {
    entry
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|_| crate::preflight::resolve_command(entry).is_none())
}

/// Herkunft eines Servers, der nur in der CLI-Liste, aber in keiner lokalen
/// Definition auftaucht (Connector/Plugin).
fn external_origin(name: &str) -> String {
    if name.starts_with("plugin:") {
        "plugin".to_string()
    } else if name.starts_with("claude.ai ") {
        "connector".to_string()
    } else {
        "unbekannt".to_string()
    }
}

/// Liefert alle Server (Definition aus JSON + Status aus `claude mcp list`),
/// gruppierbar nach Scope. `reveal=false` maskiert alle Geheimnisse.
#[tauri::command]
pub async fn list_servers(
    state: State<'_, AppState>,
    project_path: Option<String>,
    reveal: bool,
    with_status: bool,
) -> Result<Vec<MergedServer>, AppError> {
    let settings = state.settings();
    let mut servers =
        gather_servers(&state.status_cache, &settings, project_path, reveal, with_status)?;
    // Zähler aus bereits vorhandenen Introspektions-Ergebnissen anreichern.
    let cache = state
        .introspection_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for s in &mut servers {
        if let Some(scope) = s.scope {
            if let Some(intro) = cache.get(&introspection_key(scope, &s.name, &s.project_path)) {
                // Ein gecachter Fehlversuch (error gesetzt, leere Listen) darf kein
                // irreführendes „0·0·0"-Badge erzeugen.
                if intro.error.is_none() {
                    s.tool_count = Some(intro.tools.len());
                    s.resource_count = Some(intro.resources.len());
                    s.prompt_count = Some(intro.prompts.len());
                    s.connect_ms = intro.connect_ms;
                }
            }
        }
    }
    Ok(servers)
}

fn gather_servers(
    cache: &StatusCache,
    settings: &AppSettings,
    project_path: Option<String>,
    reveal: bool,
    with_status: bool,
) -> Result<Vec<MergedServer>, AppError> {
    let dir = resolve_project_dir(project_path);
    let dir_key = dir.to_string_lossy().to_string();
    let defs = collect_definitions(&dir);
    let disabled = collect_disabled(&dir);

    // Kollisionen: derselbe Name in mehreren Scopes.
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for d in &defs {
        *name_counts.entry(d.name.clone()).or_insert(0) += 1;
    }

    // Status aus der CLI holen (best effort). Bei with_status=false wird der
    // teure Health-Check (startet alle Server) übersprungen -> sofortiges Rendern.
    let mut status_map: HashMap<String, ServerStatus> = HashMap::new();
    let mut list_summaries: HashMap<String, String> = HashMap::new();
    if with_status {
        if let Some(claude) = resolve_claude(settings.claude_path()) {
            if let Ok(out) = run_claude(&claude, &["mcp", "list"], Some(&dir), settings.list_timeout()) {
                for item in parse_list(&out.stdout) {
                    list_summaries
                        .entry(item.name.clone())
                        .or_insert(item.summary);
                    status_map.insert(item.name, item.status);
                }
            }

            // Der Bulk-Health-Check (`claude mcp list`) startet ALLE Server
            // gleichzeitig. Langsam startende Server (uv/uvx/python) laufen unter
            // Last in claudes internen Timeout und werden fälschlich als „Failed"
            // gemeldet. Solche Fehlschläge daher einzeln, isoliert per
            // `claude mcp get` gegenprüfen (das ist zuverlässig) und korrigieren.
            // Echte Ausfälle bleiben Failed.
            let failed: Vec<String> = status_map
                .iter()
                .filter(|(_, s)| matches!(s, ServerStatus::Failed { .. }))
                .map(|(n, _)| n.clone())
                .collect();
            for name in failed {
                if let Ok(g) = run_claude(&claude, &["mcp", "get", &name], Some(&dir), GET_TIMEOUT) {
                    let combined = format!("{}\n{}", g.stdout, g.stderr);
                    let mut st = status_from_text(&combined);
                    // Bestätigter Fehler: den Grund (redigiert) für die Anzeige anhängen.
                    if let ServerStatus::Failed { detail } = &mut st {
                        *detail = failure_detail(&combined).map(|d| redact_secrets(&d));
                    }
                    if !matches!(st, ServerStatus::Unknown) {
                        status_map.insert(name, st);
                    }
                }
            }
        }
        // Frische Stati in den Cache schreiben.
        let map: HashMap<String, CachedItem> = status_map
            .iter()
            .map(|(n, s)| {
                (
                    n.clone(),
                    CachedItem {
                        status: s.clone(),
                        summary: list_summaries.get(n).cloned().unwrap_or_default(),
                    },
                )
            })
            .collect();
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir_key.clone(), map);
    } else {
        // Schnellmodus: letzten bekannten Stand aus dem Cache übernehmen.
        if let Some(map) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&dir_key) {
            for (name, item) in map {
                status_map.insert(name.clone(), item.status.clone());
                list_summaries
                    .entry(name.clone())
                    .or_insert_with(|| item.summary.clone());
            }
        }
    }

    let mut result: Vec<MergedServer> = Vec::new();

    for d in &defs {
        let has_secrets = entry_has_secrets(&d.entry);
        let summary = mask_summary(&summarize_entry(&d.entry), reveal);

        // Project-scope Server, die deaktiviert sind, tauchen nicht in der Liste auf.
        let status = if d.scope == Scope::Project && !disabled.is_enabled(&d.name) {
            ServerStatus::Disabled
        } else {
            status_map
                .get(&d.name)
                .cloned()
                .unwrap_or(ServerStatus::Unknown)
        };

        let enabled = match d.scope {
            Scope::Project => disabled.is_enabled(&d.name),
            Scope::User | Scope::Local => true,
        };

        result.push(MergedServer {
            name: d.name.clone(),
            scope: Some(d.scope),
            origin: origin_for(d.scope),
            project_path: d.project_path.clone(),
            entry: Some(mask_entry(&d.entry, reveal)),
            summary,
            status,
            enabled,
            editable: true,
            has_secrets,
            collision: name_counts.get(&d.name).copied().unwrap_or(0) > 1,
            tool_count: None,
            resource_count: None,
            prompt_count: None,
            connect_ms: None,
            runtime_missing: runtime_missing_for(&d.entry),
        });
    }

    // Externe Server (nur in der Liste, keine lokale Definition).
    let def_names: std::collections::HashSet<&str> =
        defs.iter().map(|d| d.name.as_str()).collect();
    for (name, status) in &status_map {
        if def_names.contains(name.as_str()) {
            continue;
        }
        let summary = list_summaries.get(name).cloned().unwrap_or_default();
        result.push(MergedServer {
            name: name.clone(),
            scope: None,
            origin: external_origin(name),
            project_path: None,
            entry: None,
            summary: mask_summary(&summary, reveal),
            status: status.clone(),
            enabled: !matches!(status, ServerStatus::PendingApproval),
            editable: false,
            has_secrets: false,
            collision: false,
            tool_count: None,
            resource_count: None,
            prompt_count: None,
            connect_ms: None,
            // Externe Server haben keine lokale Definition -> keine Runtime-Prüfung.
            runtime_missing: None,
        });
    }

    // Deaktivierte user-scope Server aus dem Stash ergänzen (sind nicht mehr in
    // ~/.claude.json vorhanden, sollen aber sichtbar/reaktivierbar sein).
    let existing_user: std::collections::HashSet<String> = result
        .iter()
        .filter(|s| s.scope == Some(Scope::User))
        .map(|s| s.name.clone())
        .collect();
    for (name, item) in crate::stash::all().user {
        if existing_user.contains(&name) {
            continue;
        }
        let has_secrets = entry_has_secrets(&item.entry);
        result.push(MergedServer {
            name,
            scope: Some(Scope::User),
            origin: "user".to_string(),
            project_path: None,
            summary: mask_summary(&summarize_entry(&item.entry), reveal),
            entry: Some(mask_entry(&item.entry, reveal)),
            status: ServerStatus::Disabled,
            enabled: false,
            editable: true,
            has_secrets,
            collision: false,
            tool_count: None,
            resource_count: None,
            prompt_count: None,
            connect_ms: None,
            runtime_missing: runtime_missing_for(&item.entry),
        });
    }

    // Stabil sortieren: erst nach Scope-Rang, dann Name.
    result.sort_by(|a, b| {
        scope_rank(a.scope).cmp(&scope_rank(b.scope)).then_with(|| {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        })
    });

    Ok(result)
}

fn scope_rank(scope: Option<Scope>) -> u8 {
    match scope {
        Some(Scope::User) => 0,
        Some(Scope::Local) => 1,
        Some(Scope::Project) => 2,
        None => 3,
    }
}

/// Präzedenz für den effektiven Scope bei Namenskonflikten (kleiner = gewinnt):
/// local > project > user. Quelle: Claude-Code-Doku
/// (https://code.claude.com/docs/en/mcp) – bei gleichem Namen nutzt Claude Code
/// genau eine Definition (kein Merge), die des höchstpriorisierten Scopes.
/// Achtung: NICHT `scope_rank` (das ist nur die Sortier-Reihenfolge der Liste).
fn scope_precedence(scope: Scope) -> u8 {
    match scope {
        Scope::Local => 0,
        Scope::Project => 1,
        Scope::User => 2,
    }
}

/// Einzelnen Server neu health-checken via `claude mcp get <name>`.
#[tauri::command]
pub async fn health_check(
    state: State<'_, AppState>,
    name: String,
    project_path: Option<String>,
) -> Result<ServerStatus, AppError> {
    let dir = resolve_project_dir(project_path);
    let Some(claude) = resolve_claude(state.settings().claude_path()) else {
        return Err(AppError::ClaudeNotFound);
    };
    let out = run_claude(&claude, &["mcp", "get", &name], Some(&dir), GET_TIMEOUT)?;
    // `get` schreibt Details nach stdout; Status per Textbaustein erkennen.
    let combined = format!("{}\n{}", out.stdout, out.stderr);
    let mut status = status_from_text(&combined);
    // Bei Fehler den Grund (redigiert) anhängen, damit die Detail-Ansicht ihn zeigt.
    if let ServerStatus::Failed { detail } = &mut status {
        *detail = failure_detail(&combined).map(|d| redact_secrets(&d));
    }
    Ok(status)
}

/// Liefert die UNMASKIERTE Definition eines Servers (nur auf ausdrückliche
/// Nutzer-Aktion „anzeigen"). Reveal ist transient und wird nie persistiert.
#[tauri::command]
pub fn reveal_server_entry(
    scope: Scope,
    name: String,
    project_path: Option<String>,
) -> Result<Option<crate::models::ServerEntry>, AppError> {
    let dir = resolve_project_dir(project_path);
    let defs = collect_definitions(&dir);
    if let Some(found) = defs.into_iter().find(|d| d.scope == scope && d.name == name) {
        return Ok(Some(found.entry));
    }
    // Deaktivierte user-scope Server liegen im Stash.
    if scope == Scope::User {
        if let Some(item) = crate::stash::peek(&name) {
            return Ok(Some(item.entry));
        }
    }
    Ok(None)
}

/// Introspiziert einen Server per MCP-Handshake (`tools`/`resources`/`prompts`).
/// Startet dazu den Server-Prozess (nur stdio). `refresh=false` liefert ein
/// gecachtes Ergebnis, falls vorhanden. Secrets in Schemata/Beschreibungen
/// werden vor der Rückgabe maskiert.
#[tauri::command]
pub async fn introspect_server(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    refresh: bool,
) -> Result<Introspection, AppError> {
    let key = introspection_key(scope, &name, &project_path);

    if !refresh {
        if let Some(cached) = state
            .introspection_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(cached);
        }
    }

    // Unmaskierte Definition auflösen: der Handshake braucht die echten env/args.
    let entry = resolve_entry(scope, &name, &project_path)?;

    // Beide Zweige geben immer eine Introspection zurück; Start-/Handshake-Fehler
    // stehen in `error`/`logs` (statt als Err), damit die Detail-Ansicht sie zeigt.
    let mut introspection = match transport_of(&entry) {
        "stdio" => crate::introspect::introspect_stdio(&entry, INTROSPECT_TIMEOUT),
        _ => crate::introspect::introspect_http(&entry, INTROSPECT_TIMEOUT),
    };

    mask_introspection(&mut introspection);

    state
        .introspection_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, introspection.clone());

    Ok(introspection)
}

/// Führt eine testweise Playground-Operation aus (`tools/call`, `resources/read`,
/// `prompts/get`). One-shot: Server frisch starten, Handshake, EIN Request,
/// beenden. **Kein Cache** – jeder Aufruf wird ausgeführt.
#[tauri::command]
pub fn playground_call(
    name: String,
    scope: Scope,
    project_path: Option<String>,
    request: PlaygroundRequest,
) -> Result<PlaygroundResult, AppError> {
    let entry = resolve_entry(scope, &name, &project_path)?;
    let mut result = match transport_of(&entry) {
        "stdio" => crate::introspect::playground_stdio(&entry, &request, PLAYGROUND_TIMEOUT),
        _ => crate::introspect::playground_http(&entry, &request, PLAYGROUND_TIMEOUT),
    };
    mask_playground(&mut result);
    Ok(result)
}

/// Redigiert ein Playground-Ergebnis vor Verlassen des Backends: Ergebnis-JSON
/// (String-Blätter) via `redact_json`, große Blob-/Bild-Inhalte werden
/// zusammengefasst; Fehler/Logs/Notizen via `redact_secrets`.
fn mask_playground(r: &mut PlaygroundResult) {
    if let Some(v) = r.result.take() {
        r.result = Some(redact_json(&summarize_blobs(v)));
    }
    if let Some(e) = &r.error {
        r.error = Some(redact_secrets(e));
    }
    if let Some(l) = &r.logs {
        r.logs = Some(redact_secrets(l));
    }
    for n in &mut r.notes {
        *n = redact_secrets(n);
    }
}

/// Ersetzt große Binärinhalte (base64) durch eine kurze Zusammenfassung, damit
/// das UI nicht mit Megabyte-Blobs geflutet wird und `redact_json` nicht sinnlos
/// über riesige Datenstrings läuft. Betrifft MCP-`content`-Items vom Typ
/// `image`/`audio` (Feld `data`) sowie Resource-`blob`-Felder.
fn summarize_blobs(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(mut map) => {
            let is_binary = matches!(
                map.get("type").and_then(|t| t.as_str()),
                Some("image") | Some("audio")
            );
            // `data` (image/audio) bzw. `blob` (resources/read) zusammenfassen.
            for key in ["data", "blob"] {
                let relevant = key == "blob" || is_binary;
                if relevant {
                    if let Some(Value::String(s)) = map.get(key) {
                        let kb = (s.len() as f64 / 1024.0).round() as u64;
                        let summary = format!("<Binärdaten, ~{kb} KB (nicht angezeigt)>");
                        map.insert(key.to_string(), Value::String(summary));
                    }
                }
            }
            Value::Object(map.into_iter().map(|(k, v)| (k, summarize_blobs(v))).collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(summarize_blobs).collect()),
        other => other,
    }
}

/// Liefert das bereits gecachte Introspektions-Ergebnis eines Servers – **ohne**
/// den Server-Prozess zu starten. Die Detail-Ansicht nutzt das beim Öffnen, um
/// zuvor geladene Fähigkeiten sofort wieder anzuzeigen.
#[tauri::command]
pub fn peek_introspection(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
) -> Result<Option<Introspection>, AppError> {
    let key = introspection_key(scope, &name, &project_path);
    Ok(state
        .introspection_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned())
}

/// Laufzeit-Preflight für einen Server: prüft, ob der benötigte Befehl
/// (`node`/`npx`, `python`/`uvx`, `docker`, …) auf PATH verfügbar ist, und
/// liefert – falls nicht – einen umsetzbaren Hinweis (+ optional die Version).
/// Startet den Server NICHT: nur PATH-Auflösung und – für bekannte Laufzeiten
/// (node/npx/python/uv/docker/…) – ein kurzes, gefahrloses `--version`.
/// Gibt `None` für HTTP/SSE-Server (kein lokaler Befehl).
#[tauri::command]
pub async fn preflight_server(
    name: String,
    scope: Scope,
    project_path: Option<String>,
) -> Result<Option<RuntimePreflight>, AppError> {
    // Unmaskierte Definition auflösen (wie introspect_server): der Preflight
    // braucht den echten command/env-PATH.
    let dir = resolve_project_dir(project_path.clone());
    let entry = collect_definitions(&dir)
        .into_iter()
        .find(|d| d.scope == scope && d.name == name)
        .map(|d| d.entry)
        .or_else(|| {
            (scope == Scope::User)
                .then(|| crate::stash::peek(&name).map(|i| i.entry))
                .flatten()
        })
        .ok_or_else(|| AppError::Io("Server-Definition nicht gefunden".into()))?;

    let mut preflight = crate::preflight::check(&entry);
    // Version stammt aus Subprozess-Ausgabe – defensiv redigieren (Boundary).
    if let Some(pf) = preflight.as_mut() {
        if let Some(v) = pf.version.as_mut() {
            *v = redact_secrets(v);
        }
    }
    Ok(preflight)
}

/// Maskiert geheim aussehende Werte in Tool-Schemata, Beschreibungen und Notizen,
/// bevor das Ergebnis das Backend verlässt.
fn mask_introspection(intro: &mut Introspection) {
    for t in &mut intro.tools {
        t.name = redact_secrets(&t.name);
        if let Some(d) = t.description.as_mut() {
            *d = redact_secrets(d);
        }
        if let Some(schema) = t.input_schema.take() {
            t.input_schema = Some(redact_json(&schema));
        }
    }
    for r in &mut intro.resources {
        // uri kann geheime Query-Parameter enthalten (z. B. ?token=…).
        r.uri = redact_secrets(&r.uri);
        if let Some(n) = r.name.as_mut() {
            *n = redact_secrets(n);
        }
        if let Some(d) = r.description.as_mut() {
            *d = redact_secrets(d);
        }
    }
    for p in &mut intro.prompts {
        p.name = redact_secrets(&p.name);
        if let Some(d) = p.description.as_mut() {
            *d = redact_secrets(d);
        }
    }
    for n in &mut intro.notes {
        *n = redact_secrets(n);
    }
    // Erfasster stderr und Fehlermeldung können Tokens enthalten (z. B. „auth token
    // expired: ghp_…" oder ein geechotes Config-JSON) – vor dem UI redigieren.
    if let Some(l) = intro.logs.as_mut() {
        *l = redact_secrets(l);
    }
    if let Some(e) = intro.error.as_mut() {
        *e = redact_secrets(e);
    }
}

/// Arbeitsverzeichnis für einen Scope: user = Home, local/project = Projektpfad.
fn cwd_for(scope: Scope, project_path: &Option<String>) -> Option<PathBuf> {
    match scope {
        Scope::User => None,
        Scope::Local | Scope::Project => Some(resolve_project_dir(project_path.clone())),
    }
}

/// Führt eine mutierende claude-Operation aus und mappt Nicht-Null-Exit auf CliFailed.
fn run_mut(
    claude_path: Option<&str>,
    args: &[&str],
    cwd: Option<PathBuf>,
    timeout: Duration,
) -> Result<(), AppError> {
    let claude = resolve_claude(claude_path).ok_or(AppError::ClaudeNotFound)?;
    let out = run_claude(&claude, args, cwd.as_deref(), timeout)?;
    if out.success() {
        Ok(())
    } else {
        let detail = if out.stderr.trim().is_empty() {
            out.stdout.trim().to_string()
        } else {
            out.stderr.trim().to_string()
        };
        // `claude mcp add-json` kann das übergebene JSON (mit Secrets) echoen –
        // vor dem Weiterreichen ins UI maskieren.
        Err(AppError::CliFailed {
            code: out.code,
            stderr: redact_secrets(&detail),
        })
    }
}

/// Fügt via `claude mcp add-json` einen Server hinzu (upsert nicht garantiert).
#[tauri::command]
pub async fn add_server(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    add_server_impl(&state.settings(), name, scope, project_path, entry)
}

fn add_server_impl(
    settings: &AppSettings,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    let json = serde_json::to_string(&entry).map_err(|e| AppError::Parse(e.to_string()))?;
    let cwd = cwd_for(scope, &project_path);
    run_mut(
        settings.claude_path(),
        &["mcp", "add-json", "-s", scope.cli_value(), &name, &json],
        cwd,
        settings.mut_timeout(),
    )
}

/// Bearbeitet einen Server: alten Stand sichern -> entfernen -> neu anlegen.
/// Schlägt das Neu-Anlegen fehl, wird der alte Stand zurückgerollt.
#[tauri::command]
pub async fn update_server(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    update_server_impl(&state.settings(), name, scope, project_path, entry)
}

fn update_server_impl(
    settings: &AppSettings,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    let dir = resolve_project_dir(project_path.clone());
    let old = collect_definitions(&dir)
        .into_iter()
        .find(|d| d.scope == scope && d.name == name)
        .map(|d| d.entry);

    let cwd = cwd_for(scope, &project_path);
    let timeout = settings.mut_timeout();
    let claude_path = settings.claude_path();
    let new_json = serde_json::to_string(&entry).map_err(|e| AppError::Parse(e.to_string()))?;

    // Alten Eintrag entfernen (falls vorhanden).
    let _ = run_mut(
        claude_path,
        &["mcp", "remove", "-s", scope.cli_value(), &name],
        cwd.clone(),
        timeout,
    );

    // Neuen Eintrag anlegen.
    match run_mut(
        claude_path,
        &["mcp", "add-json", "-s", scope.cli_value(), &name, &new_json],
        cwd.clone(),
        timeout,
    ) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Rollback auf den alten Stand.
            if let Some(old_entry) = old {
                if let Ok(old_json) = serde_json::to_string(&old_entry) {
                    let _ = run_mut(
                        claude_path,
                        &["mcp", "add-json", "-s", scope.cli_value(), &name, &old_json],
                        cwd,
                        timeout,
                    );
                }
            }
            Err(err)
        }
    }
}

/// Legt einen Auto-Snapshot an, sofern `note` gesetzt ist (`None` überspringt –
/// z. B. wenn der Aufrufer für eine Bulk-Aktion bereits einen gemeinsamen
/// Snapshot angelegt hat). Schlägt die Sicherung fehl, bricht die aufrufende
/// destruktive Aktion ab (lieber nicht ändern als ungesichert ändern).
///
/// Bewusst OHNE Rollback: der Aufrufer legt den Snapshot erst NACH allen
/// billigen Vorbedingungsprüfungen an (kein Waisen-Snapshot bei „nicht
/// gefunden") und unmittelbar VOR der ersten Mutation. Einen einmal angelegten
/// Snapshot wieder zu löschen wäre gefährlich: Schlägt eine mehrstufige Aktion
/// nach einer Teil-Mutation fehl (z. B. set_scope: im Ziel angelegt, aus der
/// Quelle-Entfernen scheitert), ist genau dieser Snapshot die einzige
/// Wiederherstellungsmöglichkeit.
fn auto_snapshot(settings: &AppSettings, note: Option<String>) -> Result<(), AppError> {
    if let Some(n) = note {
        crate::snapshot::create(Some(n), true, settings.snapshot_retention)?;
    }
    Ok(())
}

/// Entfernt einen Server via `claude mcp remove`. `skip_snapshot` unterdrückt den
/// automatischen Snapshot (Bulk-Aktionen sichern einmalig vorab).
#[tauri::command]
pub async fn remove_server(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    skip_snapshot: Option<bool>,
) -> Result<(), AppError> {
    remove_server_impl(
        &state.settings(),
        name,
        scope,
        project_path,
        !skip_snapshot.unwrap_or(false),
    )
}

fn remove_server_impl(
    settings: &AppSettings,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    snapshot: bool,
) -> Result<(), AppError> {
    // Deaktivierte user-scope Server liegen ausschließlich im Stash und nicht
    // in ~/.claude.json – `claude mcp remove` würde sie nicht finden. Solche
    // Einträge direkt aus dem Stash löschen. Steht der Server (kaputte
    // Invariante) trotzdem auch aktiv in ~/.claude.json, NICHT kurzschließen.
    if scope == Scope::User && crate::stash::peek(&name).is_some() {
        let active = collect_definitions(&default_project_path())
            .into_iter()
            .any(|d| d.scope == Scope::User && d.name == name);
        if !active {
            // Reine Stash-Mutation -> vorher sichern, dann entfernen.
            auto_snapshot(settings, snapshot.then(|| format!("auto: remove_server {name}")))?;
            return crate::stash::remove(&name);
        }
    }
    // Auto-Snapshot unmittelbar vor der Mutation (schlägt er fehl, wird nichts
    // entfernt). Kein nachträgliches Löschen – siehe auto_snapshot.
    auto_snapshot(settings, snapshot.then(|| format!("auto: remove_server {name}")))?;
    let cwd = cwd_for(scope, &project_path);
    run_mut(
        settings.claude_path(),
        &["mcp", "remove", "-s", scope.cli_value(), &name],
        cwd,
        settings.mut_timeout(),
    )?;
    // Verwaisten Stash-Eintrag mitentfernen (defensiv; no-op ohne Eintrag).
    if scope == Scope::User {
        let _ = crate::stash::remove(&name);
    }
    Ok(())
}

/// OAuth-Anmeldung für HTTP/SSE-Server bzw. Connectoren. Öffnet ggf. den Browser.
#[tauri::command]
pub async fn login_server(state: State<'_, AppState>, name: String) -> Result<(), AppError> {
    // Login-Timeout bleibt großzügig (Browser-Flow) und ist bewusst nicht konfigurierbar.
    run_mut(state.settings().claude_path(), &["mcp", "login", &name], None, LOGIN_TIMEOUT)
}

/// Gespeicherte OAuth-Credentials eines Servers löschen.
#[tauri::command]
pub async fn logout_server(state: State<'_, AppState>, name: String) -> Result<(), AppError> {
    let settings = state.settings();
    run_mut(settings.claude_path(), &["mcp", "logout", &name], None, settings.mut_timeout())
}

/// Setzt Freigabe-/Ablehnungs-Entscheidungen für .mcp.json-Server im Projekt zurück.
#[tauri::command]
pub async fn reset_project_choices(
    state: State<'_, AppState>,
    project_path: Option<String>,
) -> Result<(), AppError> {
    let cwd = Some(resolve_project_dir(project_path));
    let settings = state.settings();
    run_mut(settings.claude_path(), &["mcp", "reset-project-choices"], cwd, settings.mut_timeout())
}

/// Aktiviert/deaktiviert einen .mcp.json-Server (project scope) über die
/// settings.local.json-Arrays.
#[tauri::command]
pub fn toggle_mcpjson_server(
    name: String,
    project_path: Option<String>,
    enabled: bool,
) -> Result<(), AppError> {
    // Mit gesetztem project_path pro-Projekt in <projekt>/.claude/settings.local.json
    // schreiben; sonst global in ~/.claude/settings.local.json.
    let target = match project_path.filter(|p| !p.is_empty()) {
        Some(p) => project_settings_local_path(&PathBuf::from(p)),
        None => settings_local_path(),
    };
    crate::toggles::toggle_mcpjson(&target, &name, enabled)
}

/// Aktiviert/deaktiviert einen user-scope Server per Stash-and-restore.
#[tauri::command]
pub async fn toggle_user_server(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
    entry: Option<ServerEntry>,
    skip_snapshot: Option<bool>,
) -> Result<(), AppError> {
    toggle_user_server_impl(
        &state.settings(),
        name,
        enabled,
        entry,
        !skip_snapshot.unwrap_or(false),
    )
}

fn toggle_user_server_impl(
    settings: &AppSettings,
    name: String,
    enabled: bool,
    entry: Option<ServerEntry>,
    snapshot: bool,
) -> Result<(), AppError> {
    if enabled {
        // Reaktivieren: Definition aus dem Stash zurückspielen.
        let item = crate::stash::peek(&name).ok_or(AppError::StashMissing)?;
        let json = serde_json::to_string(&item.entry).map_err(|e| AppError::Parse(e.to_string()))?;
        run_mut(
            settings.claude_path(),
            &["mcp", "add-json", "-s", "user", &name, &json],
            None,
            settings.mut_timeout(),
        )?;
        crate::stash::remove(&name)?; // erst nach Erfolg
        Ok(())
    } else {
        // Deaktivieren: erst die Definition ermitteln (Vorbedingung – erzeugt
        // noch keinen Snapshot, falls nicht gefunden), dann sichern, dann
        // mutieren. Kein Rollback: ab stash::upsert wurde bereits geschrieben.
        let dir = default_project_path();
        let current = collect_definitions(&dir)
            .into_iter()
            .find(|d| d.scope == Scope::User && d.name == name)
            .map(|d| d.entry)
            .or(entry)
            .ok_or_else(|| AppError::Io("Server-Definition nicht gefunden".into()))?;
        auto_snapshot(settings, snapshot.then(|| format!("auto: disable {name}")))?;
        crate::stash::upsert(&name, current)?;
        run_mut(
            settings.claude_path(),
            &["mcp", "remove", "-s", "user", &name],
            None,
            settings.mut_timeout(),
        )?;
        Ok(())
    }
}

/// Kopiert eine Server-Definition verifiziert in einen Ziel-Scope: via
/// `claude mcp add-json` anlegen, danach über `collect_definitions` bestätigen.
/// Gemeinsame Basis für `set_scope` (Verschieben) und `clone_server` (Duplizieren) –
/// der Aufrufer entscheidet, ob die Quelle danach entfernt wird.
fn copy_definition(
    settings: &AppSettings,
    entry: &ServerEntry,
    name: &str,
    to_scope: Scope,
    to_project: &Option<String>,
) -> Result<(), AppError> {
    let json = serde_json::to_string(entry).map_err(|e| AppError::Parse(e.to_string()))?;

    // 1. Im Ziel-Scope anlegen.
    run_mut(
        settings.claude_path(),
        &["mcp", "add-json", "-s", to_scope.cli_value(), name, &json],
        cwd_for(to_scope, to_project),
        settings.mut_timeout(),
    )?;

    // 2. Erfolg verifizieren.
    let to_dir = resolve_project_dir(to_project.clone());
    let landed = collect_definitions(&to_dir)
        .into_iter()
        .any(|d| d.scope == to_scope && d.name == name);
    if !landed {
        return Err(AppError::Io(
            "Anlegen im Ziel-Scope wurde nicht bestätigt.".into(),
        ));
    }
    Ok(())
}

/// Verschiebt einen Server in einen anderen Scope: verifiziertes Anlegen im
/// Ziel-Scope, danach Entfernen aus dem Quell-Scope (nie umgekehrt).
#[tauri::command]
pub async fn set_scope(
    state: State<'_, AppState>,
    name: String,
    from_scope: Scope,
    to_scope: Scope,
    from_project: Option<String>,
    to_project: Option<String>,
) -> Result<(), AppError> {
    set_scope_impl(&state.settings(), name, from_scope, to_scope, from_project, to_project)
}

fn set_scope_impl(
    settings: &AppSettings,
    name: String,
    from_scope: Scope,
    to_scope: Scope,
    from_project: Option<String>,
    to_project: Option<String>,
) -> Result<(), AppError> {
    if from_scope == to_scope {
        return Ok(());
    }

    let from_dir = resolve_project_dir(from_project.clone());
    let timeout = settings.mut_timeout();
    let claude_path = settings.claude_path();

    // Vorbedingung zuerst (erzeugt noch keinen Snapshot, falls die Quelle fehlt).
    let entry = collect_definitions(&from_dir)
        .into_iter()
        .find(|d| d.scope == from_scope && d.name == name)
        .map(|d| d.entry)
        .ok_or_else(|| AppError::Io("Quell-Definition nicht gefunden".into()))?;

    // Auto-Snapshot unmittelbar vor der ersten Mutation. NICHT nachträglich
    // löschen: schlägt Schritt 3 (Entfernen aus der Quelle) nach erfolgreichem
    // Anlegen im Ziel fehl, ist der Server dupliziert – dann ist genau dieser
    // Snapshot die einzige Möglichkeit, den sauberen Vorher-Zustand
    // wiederherzustellen.
    auto_snapshot(settings, Some(format!("auto: set_scope {name}")))?;

    // 1.+2. Verifiziert im Ziel-Scope anlegen (Quelle bleibt bei Fehler unberührt).
    copy_definition(settings, &entry, &name, to_scope, &to_project)?;

    // 3. Erst jetzt aus dem Quell-Scope entfernen.
    if let Err(e) = run_mut(
        claude_path,
        &["mcp", "remove", "-s", from_scope.cli_value(), &name],
        cwd_for(from_scope, &from_project),
        timeout,
    ) {
        return Err(AppError::Io(format!(
            "In Ziel-Scope kopiert, aber alte Kopie ({}) konnte nicht entfernt werden: {}",
            from_scope.cli_value(),
            e
        )));
    }
    Ok(())
}

/// Dupliziert einen Server in einen (ggf. anderen) Scope/ein anderes Projekt.
/// Die Quelle bleibt bestehen. Secrets werden backend-seitig aufgelöst und
/// wandern nicht durchs Webview.
#[tauri::command]
pub async fn clone_server(
    state: State<'_, AppState>,
    name: String,
    from_scope: Scope,
    from_project: Option<String>,
    new_name: String,
    to_scope: Scope,
    to_project: Option<String>,
) -> Result<(), AppError> {
    clone_server_impl(
        &state.settings(),
        name,
        from_scope,
        from_project,
        new_name,
        to_scope,
        to_project,
    )
}

fn clone_server_impl(
    settings: &AppSettings,
    name: String,
    from_scope: Scope,
    from_project: Option<String>,
    new_name: String,
    to_scope: Scope,
    to_project: Option<String>,
) -> Result<(), AppError> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err(AppError::Io("Neuer Name darf nicht leer sein".into()));
    }

    let from_dir = resolve_project_dir(from_project.clone());
    let to_dir = resolve_project_dir(to_project.clone());

    // Ziel-Projektverzeichnis muss existieren – sonst könnte die CLI die Definition
    // ins Leere schreiben. User-Scope braucht kein Projektverzeichnis.
    if matches!(to_scope, Scope::Local | Scope::Project) && !to_dir.is_dir() {
        return Err(AppError::Io(format!(
            "Zielprojekt existiert nicht: {}",
            to_dir.display()
        )));
    }

    // 1. Unmaskierte Quell-Definition auflösen (Config zuerst, dann Stash für
    //    deaktivierte User-Server – analog zu reveal_server_entry).
    let from_defs = collect_definitions(&from_dir);
    let entry = from_defs
        .iter()
        .find(|d| d.scope == from_scope && d.name == name)
        .map(|d| d.entry.clone())
        .or_else(|| {
            if from_scope == Scope::User {
                crate::stash::peek(&name).map(|item| item.entry)
            } else {
                None
            }
        })
        .ok_or_else(|| AppError::Io("Quell-Definition nicht gefunden".into()))?;

    // 2. Kollision im Ziel prüfen – niemals überschreiben. Deaktivierte User-Server
    //    liegen ausschließlich im Stash (nicht in der Config) und müssen mitgeprüft
    //    werden, sonst überschreibt ein späteres Deaktivieren den Klon lautlos.
    //    Quell-Definitionen wiederverwenden, wenn Quelle und Ziel dasselbe
    //    Verzeichnis meinen – auch bei unterschiedlicher Schreibweise (Symlink,
    //    Trailing-Slash) via Kanonisierung, sonst nur ein zusätzlicher Read.
    let same_dir = from_dir == to_dir
        || matches!(
            (std::fs::canonicalize(&from_dir), std::fs::canonicalize(&to_dir)),
            (Ok(a), Ok(b)) if a == b
        );
    let to_defs = if same_dir {
        from_defs
    } else {
        collect_definitions(&to_dir)
    };
    let in_config = to_defs.iter().any(|d| d.scope == to_scope && d.name == new_name);
    let in_stash = to_scope == Scope::User && crate::stash::peek(&new_name).is_some();
    if in_config || in_stash {
        let suffix = if in_stash && !in_config {
            " (aktuell deaktiviert)"
        } else {
            ""
        };
        return Err(AppError::Io(format!(
            "Ein Server namens „{}“ existiert im Ziel-Scope ({}) bereits{}",
            new_name,
            to_scope.cli_value(),
            suffix
        )));
    }

    // 3. Verifiziert im Ziel anlegen.
    copy_definition(settings, &entry, &new_name, to_scope, &to_project)
}

/// Hash über die normalisierte Definition. `env`/`headers` sind `BTreeMap` →
/// deterministische JSON-Serialisierung → stabiler Fingerprint (unabhängig von
/// der Key-Reihenfolge). Kein Krypto nötig, nur Gleichheitsvergleich.
fn definition_fingerprint(entry: &ServerEntry) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let json = serde_json::to_string(entry).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

/// Findet Namenskonflikte: Server, deren Name in mehreren Scopes definiert ist.
/// Bezieht deaktivierte user-scope Server aus dem Stash mit ein (sonst entsteht
/// der Konflikt beim Reaktivieren überraschend). Liefert je Konflikt die
/// Definitionen (maskiert), den effektiven Scope (Präzedenz local > project >
/// user) und ob alle Definitionen inhaltsgleich sind.
///
/// **Bewusst pro Projekt-Kontext** (`project_path` bzw. Home): geprüft wird
/// user + local/project GENAU dieses Kontexts – so, wie Claude Code beim
/// Arbeiten in genau diesem Verzeichnis auflöst. Ein „globaler" Konflikt über
/// mehrere Projekte hinweg wäre nicht sinnvoll darstellbar, weil der effektive
/// Scope kontextabhängig ist (local von Projekt A vs. local von Projekt B haben
/// keinen gemeinsamen Gewinner). Konflikte anderer Projekte erscheinen daher
/// erst beim Öffnen des jeweiligen Projekts.
#[tauri::command]
pub fn list_conflicts(project_path: Option<String>) -> Result<Vec<ConflictInfo>, AppError> {
    let dir = resolve_project_dir(project_path);
    let mut defs = collect_definitions(&dir);

    // Stash (deaktivierte user-scope Server) ergänzen, sofern nicht schon aktiv.
    let active_user: std::collections::HashSet<String> = defs
        .iter()
        .filter(|d| d.scope == Scope::User)
        .map(|d| d.name.clone())
        .collect();
    for (name, item) in crate::stash::all().user {
        if !active_user.contains(&name) {
            defs.push(ScopedEntry {
                scope: Scope::User,
                name,
                entry: item.entry,
                project_path: None,
            });
        }
    }

    // Nach Name gruppieren (BTreeMap => stabile, alphabetische Reihenfolge).
    let mut by_name: std::collections::BTreeMap<String, Vec<&ScopedEntry>> =
        std::collections::BTreeMap::new();
    for d in &defs {
        by_name.entry(d.name.clone()).or_default().push(d);
    }

    let mut out = Vec::new();
    for (name, group) in by_name {
        if group.len() < 2 {
            continue; // kein Konflikt
        }
        let definitions: Vec<ConflictDefinition> = group
            .iter()
            .map(|d| ConflictDefinition {
                scope: d.scope,
                project_path: d.project_path.clone(),
                summary: mask_summary(&summarize_entry(&d.entry), false),
                fingerprint: definition_fingerprint(&d.entry),
            })
            .collect();
        let first_fp = definitions[0].fingerprint;
        let identical = definitions.iter().all(|c| c.fingerprint == first_fp);
        // Effektiver Scope = höchste Präzedenz (kleinster Wert).
        let effective_scope = group
            .iter()
            .map(|d| d.scope)
            .min_by_key(|s| scope_precedence(*s))
            .unwrap_or(Scope::User);
        out.push(ConflictInfo {
            name,
            definitions,
            effective_scope,
            identical,
        });
    }
    Ok(out)
}

/// Benennt einen Server innerhalb desselben Scopes um: Kopie unter dem neuen
/// Namen verifiziert anlegen, dann das Original entfernen (Muster wie
/// `clone_server`/`set_scope`). Lehnt ab, wenn der Zielname im selben Scope
/// bereits existiert.
#[tauri::command]
pub async fn rename_server(
    state: State<'_, AppState>,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    new_name: String,
) -> Result<(), AppError> {
    rename_server_impl(&state.settings(), name, scope, project_path, new_name)
}

fn rename_server_impl(
    settings: &AppSettings,
    name: String,
    scope: Scope,
    project_path: Option<String>,
    new_name: String,
) -> Result<(), AppError> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err(AppError::Io("Neuer Name darf nicht leer sein".into()));
    }
    if new_name == name {
        return Ok(()); // No-op
    }

    let dir = resolve_project_dir(project_path.clone());
    let defs = collect_definitions(&dir);

    // Quell-Definition: zuerst die aktive Config, sonst der Stash (deaktivierter
    // user-Server). `from_stash` merkt sich, dass die Quelle deaktiviert war –
    // dann bleibt der umbenannte Server ebenfalls deaktiviert (er wird NICHT
    // durch das Umbenennen unbeabsichtigt aktiviert).
    let config_entry = defs
        .iter()
        .find(|d| d.scope == scope && d.name == name)
        .map(|d| d.entry.clone());
    let from_stash = config_entry.is_none()
        && scope == Scope::User
        && crate::stash::peek(&name).is_some();
    let entry = config_entry
        .or_else(|| {
            if scope == Scope::User {
                crate::stash::peek(&name).map(|item| item.entry)
            } else {
                None
            }
        })
        .ok_or_else(|| AppError::Io("Quell-Definition nicht gefunden".into()))?;

    // Zielname darf im selben Scope nicht existieren (inkl. Stash bei user).
    let in_config = defs.iter().any(|d| d.scope == scope && d.name == new_name);
    let in_stash = scope == Scope::User && crate::stash::peek(&new_name).is_some();
    if in_config || in_stash {
        return Err(AppError::Io(format!(
            "Ein Server namens „{}“ existiert im Scope ({}) bereits",
            new_name,
            scope.cli_value()
        )));
    }

    // Auto-Snapshot vor der Mutation (kein Rollback – siehe auto_snapshot).
    auto_snapshot(
        settings,
        Some(format!("auto: rename_server {name} -> {new_name}")),
    )?;

    if from_stash {
        // Deaktivierten Server umbenennen: unter neuem Namen wieder in den Stash
        // legen und den alten Eintrag entfernen – kein claude-Aufruf, kein
        // Aktivieren. Erst upsert, dann remove (Reihenfolge wie beim Deaktivieren).
        crate::stash::upsert(&new_name, entry)?;
        crate::stash::remove(&name)?;
        return Ok(());
    }

    // Aktiver Server: verifiziert unter neuem Namen anlegen, dann Original
    // entfernen (Snapshot wurde bereits angelegt -> hier überspringen).
    copy_definition(settings, &entry, &new_name, scope, &project_path)?;
    remove_server_impl(settings, name, scope, project_path, false)
}

/// Listet alle Claude-Code-Projekte (Einträge unter `projects` in ~/.claude.json).
#[tauri::command]
pub fn list_projects() -> Result<Vec<ProjectInfo>, AppError> {
    let home = home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = Vec::new();
    if let Some(root) = read_json_value(&claude_json_path()) {
        if let Some(projects) = root.get("projects").and_then(|p| p.as_object()) {
            for (path, entry) in projects {
                let count = entry
                    .get("mcpServers")
                    .and_then(|m| m.as_object())
                    .map(|m| m.len())
                    .unwrap_or(0);
                out.push(ProjectInfo {
                    path: path.clone(),
                    server_count: count,
                    exists: std::path::Path::new(path).is_dir(),
                    is_home: *path == home,
                });
            }
        }
    }
    // Home zuerst, dann alphabetisch.
    out.sort_by(|a, b| {
        b.is_home
            .cmp(&a.is_home)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    Ok(out)
}

/// Entfernt einen Projekteintrag aus ~/.claude.json (inkl. dessen local-scope
/// Server und Verlauf). Direkter, atomarer Edit mit vorheriger Sicherung –
/// es gibt keinen CLI-Befehl dafür.
#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, path: String) -> Result<(), AppError> {
    delete_project_impl(&state.settings(), path)
}

fn delete_project_impl(settings: &AppSettings, path: String) -> Result<(), AppError> {
    // Erst in-memory prüfen und entfernen (noch keine Platte berührt): so wird
    // bei „Projekt nicht gefunden" kein Waisen-Snapshot angelegt.
    let cj = claude_json_path();
    let mut root =
        read_json_value(&cj).ok_or_else(|| AppError::Io("~/.claude.json nicht lesbar".into()))?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::Parse("unerwartetes Format in ~/.claude.json".into()))?;
    let removed = obj
        .get_mut("projects")
        .and_then(|p| p.as_object_mut())
        .map(|m| m.remove(&path).is_some())
        .unwrap_or(false);
    if !removed {
        return Err(AppError::Io("Projekt nicht gefunden".into()));
    }

    // Verpflichtender Auto-Snapshot als Sicherung unmittelbar vor dem Schreiben
    // (löst das frühere einzelne .claude.json.mcpmgr.bak ab, da der Snapshot
    // vollständig und selbst wiederherstellbar ist); schlägt er fehl, wird NICHT
    // geschrieben. Die Race gegen ein laufendes Claude Code bleibt inhärent.
    auto_snapshot(settings, Some(format!("auto: delete_project {path}")))?;
    crate::toggles::atomic_write_json(&cj, &root)
}

/// Startet den Claude-Assistenten, der aus einem Link einen Config-Vorschlag
/// erzeugt. Schreibt nichts – der Vorschlag geht ans Formular.
#[tauri::command]
pub async fn run_claude_assistant(
    state: State<'_, AppState>,
    url: String,
    extra_context: Option<String>,
) -> Result<crate::assistant::AssistantResult, AppError> {
    crate::assistant::run_assistant(
        &url,
        extra_context.as_deref(),
        state.settings().claude_path(),
    )
}

/// Liefert die aktuellen App-Einstellungen (aus dem Speicher-Cache).
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings()
}

/// Validiert und persistiert neue Einstellungen, aktualisiert den Cache und
/// gibt die (normalisierten) Einstellungen zurück.
#[tauri::command]
pub fn set_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSettings, AppError> {
    let settings = crate::settings::normalize(settings);
    crate::settings::validate(&settings)?;
    crate::settings::save(&settings)?;
    *state.settings.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    Ok(settings)
}

/// Erstellt einen Snapshot der aktuellen MCP-Konfiguration. `auto=false`
/// (Default) = manueller Snapshot (bleibt erhalten); `auto=true` = automatische
/// Sicherung (unterliegt der Retention) – z. B. der eine Sammel-Snapshot vor
/// einer Bulk-Aktion.
#[tauri::command]
pub fn create_snapshot(
    state: State<'_, AppState>,
    note: Option<String>,
    auto: Option<bool>,
) -> Result<crate::snapshot::SnapshotManifest, AppError> {
    crate::snapshot::create(note, auto.unwrap_or(false), state.settings().snapshot_retention)
}

/// Listet alle vorhandenen Snapshots, neueste zuerst.
#[tauri::command]
pub fn list_snapshots() -> Result<Vec<crate::snapshot::SnapshotManifest>, AppError> {
    crate::snapshot::list()
}

/// Stellt einen Snapshot wieder her (optional nur ausgewählte Dateien). Legt
/// vorher selbst einen Auto-Snapshot des Ist-Zustands an.
#[tauri::command]
pub fn restore_snapshot(
    state: State<'_, AppState>,
    id: String,
    only_paths: Option<Vec<String>>,
) -> Result<(), AppError> {
    crate::snapshot::restore(&id, only_paths, state.settings().snapshot_retention)
}

/// Löscht einen Snapshot.
#[tauri::command]
pub fn delete_snapshot(id: String) -> Result<(), AppError> {
    crate::snapshot::delete(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Die Commands sind jetzt async und brauchen einen Settings-Snapshot; die
    // Tests rufen die synchronen `_impl`-Funktionen mit Default-Einstellungen auf.
    // Diese Hüllen überschatten die (async) Glob-Importe und halten die vielen
    // Aufrufstellen unverändert.
    fn cfg() -> AppSettings {
        AppSettings::default()
    }
    fn add_server(name: String, scope: Scope, pp: Option<String>, entry: ServerEntry) -> Result<(), AppError> {
        add_server_impl(&cfg(), name, scope, pp, entry)
    }
    fn remove_server(name: String, scope: Scope, pp: Option<String>) -> Result<(), AppError> {
        remove_server_impl(&cfg(), name, scope, pp, true)
    }
    fn update_server(name: String, scope: Scope, pp: Option<String>, entry: ServerEntry) -> Result<(), AppError> {
        update_server_impl(&cfg(), name, scope, pp, entry)
    }
    fn toggle_user_server(name: String, enabled: bool, entry: Option<ServerEntry>) -> Result<(), AppError> {
        toggle_user_server_impl(&cfg(), name, enabled, entry, true)
    }
    fn set_scope(
        name: String,
        from_scope: Scope,
        to_scope: Scope,
        from_project: Option<String>,
        to_project: Option<String>,
    ) -> Result<(), AppError> {
        set_scope_impl(&cfg(), name, from_scope, to_scope, from_project, to_project)
    }
    fn delete_project(path: String) -> Result<(), AppError> {
        delete_project_impl(&cfg(), path)
    }
    #[allow(clippy::too_many_arguments)]
    fn clone_server(
        name: String,
        from_scope: Scope,
        from_project: Option<String>,
        new_name: String,
        to_scope: Scope,
        to_project: Option<String>,
    ) -> Result<(), AppError> {
        clone_server_impl(&cfg(), name, from_scope, from_project, new_name, to_scope, to_project)
    }

    /// Opt-in-Integrationstest: ruft echtes `claude mcp list` auf (health-checkt
    /// alle Server, ~langsam) und prüft, dass Merge, Maskierung und Extern-
    /// Klassifizierung sinnvoll sind. Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_servers_end_to_end() {
        let servers =
            gather_servers(&Mutex::new(HashMap::new()), &cfg(), None, false, true).expect("list_servers");
        for s in &servers {
            let env_preview = s
                .entry
                .as_ref()
                .and_then(|e| e.env.as_ref())
                .map(|m| format!("{:?}", m))
                .unwrap_or_default();
            eprintln!(
                "{:9} {:24} status={:?} enabled={} secrets={} coll={} {}",
                s.origin, s.name, s.status, s.enabled, s.has_secrets, s.collision, env_preview
            );
        }
        eprintln!("Gesamt: {}", servers.len());
        // Maskierung: kein Klartext-Token in env, wenn reveal=false.
        for s in &servers {
            if let Some(env) = s.entry.as_ref().and_then(|e| e.env.as_ref()) {
                for (k, v) in env {
                    assert!(
                        v == crate::mask::MASK,
                        "env-Wert von {} ({}) darf maskiert sein, war: {}",
                        s.name,
                        k,
                        v
                    );
                }
            }
        }
        assert!(!servers.is_empty());
    }

    /// Opt-in: echter add -> update -> remove Roundtrip im user-scope mit einem
    /// harmlosen Wegwerf-Server. Verifiziert die CLI-Schreibwrapper.
    /// Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn add_update_remove_roundtrip() {
        let name = "mcpmgr-selftest".to_string();
        let dir = default_project_path();
        let present = || {
            collect_definitions(&dir)
                .into_iter()
                .find(|d| d.scope == Scope::User && d.name == name)
        };

        // Aufräumen aus evtl. vorherigem Lauf.
        let _ = remove_server(name.clone(), Scope::User, None);

        let e1 = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["hi".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e1).expect("add");
        assert!(present().is_some(), "nach add vorhanden");

        let e2 = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["updated".into()]),
            ..Default::default()
        };
        update_server(name.clone(), Scope::User, None, e2).expect("update");
        let args = present().and_then(|d| d.entry.args).unwrap_or_default();
        assert_eq!(args, vec!["updated".to_string()], "nach update geändert");

        remove_server(name.clone(), Scope::User, None).expect("remove");
        assert!(present().is_none(), "nach remove weg");
        eprintln!("Roundtrip OK (add/update/remove)");
    }

    /// Opt-in: remove_server legt automatisch einen Snapshot an. Nutzt einen
    /// Wegwerf-Server und räumt die dabei erzeugten Snapshots wieder auf.
    /// Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn remove_server_creates_auto_snapshot() {
        let name = "mcpmgr-snaptest".to_string();
        let _ = remove_server(name.clone(), Scope::User, None);

        let before: Vec<String> = crate::snapshot::list()
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e).expect("add");
        remove_server(name.clone(), Scope::User, None).expect("remove");

        let created: Vec<_> = crate::snapshot::list()
            .unwrap()
            .into_iter()
            .filter(|m| !before.contains(&m.id))
            .collect();
        assert!(
            created.iter().any(|m| m.auto),
            "remove_server sollte einen Auto-Snapshot erzeugen"
        );

        // Aufräumen.
        for m in &created {
            let _ = crate::snapshot::delete(&m.id);
        }
        eprintln!("Auto-Snapshot OK ({} neu)", created.len());
    }

    #[test]
    fn summarize_blobs_replaces_binary_keeps_text() {
        use serde_json::json;
        let input = json!({
            "content": [
                { "type": "text", "text": "hallo" },
                { "type": "image", "data": "AAAABBBBCCCC", "mimeType": "image/png" }
            ],
            "contents": [ { "uri": "file://x", "blob": "ZZZZZZZZ" } ]
        });
        let out = summarize_blobs(input);
        // Text bleibt erhalten.
        assert_eq!(out["content"][0]["text"], json!("hallo"));
        // Bild-`data` und Resource-`blob` sind zusammengefasst (kein Roh-base64).
        let img = out["content"][1]["data"].as_str().unwrap();
        assert!(img.starts_with("<Binärdaten"), "data nicht zusammengefasst: {img}");
        assert_eq!(out["content"][1]["mimeType"], json!("image/png"));
        let blob = out["contents"][0]["blob"].as_str().unwrap();
        assert!(blob.starts_with("<Binärdaten"), "blob nicht zusammengefasst: {blob}");
    }

    #[test]
    fn conflict_precedence_and_fingerprint() {
        use std::collections::BTreeMap;

        // Effektiver Scope: local > project > user (kleinste Präzedenz gewinnt).
        let eff = |scopes: &[Scope]| -> Scope {
            scopes
                .iter()
                .copied()
                .min_by_key(|s| scope_precedence(*s))
                .unwrap()
        };
        assert_eq!(eff(&[Scope::User, Scope::Project]), Scope::Project);
        assert_eq!(eff(&[Scope::User, Scope::Local]), Scope::Local);
        assert_eq!(eff(&[Scope::User, Scope::Local, Scope::Project]), Scope::Local);
        assert_eq!(eff(&[Scope::Project, Scope::User, Scope::Local]), Scope::Local);

        // Fingerprint ist unabhängig von der env-Key-Reihenfolge (BTreeMap sortiert).
        let mut e1 = ServerEntry {
            command: Some("srv".into()),
            ..Default::default()
        };
        let mut env_a = BTreeMap::new();
        env_a.insert("A".to_string(), "1".to_string());
        env_a.insert("B".to_string(), "2".to_string());
        e1.env = Some(env_a);

        let mut e2 = ServerEntry {
            command: Some("srv".into()),
            ..Default::default()
        };
        let mut env_b = BTreeMap::new();
        env_b.insert("B".to_string(), "2".to_string());
        env_b.insert("A".to_string(), "1".to_string());
        e2.env = Some(env_b);

        assert_eq!(
            definition_fingerprint(&e1),
            definition_fingerprint(&e2),
            "gleiche Definition (andere Insert-Reihenfolge) -> gleicher Fingerprint"
        );

        let e3 = ServerEntry {
            command: Some("anders".into()),
            ..Default::default()
        };
        assert_ne!(
            definition_fingerprint(&e1),
            definition_fingerprint(&e3),
            "abweichende Definition -> anderer Fingerprint"
        );
    }

    /// Opt-in: legt denselben Server in user- und local-Scope an, prüft, dass
    /// `list_conflicts` ihn findet (effective_scope=Local, identical=true), und
    /// räumt wieder auf. Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_conflicts_finds_cross_scope_duplicate() {
        let tmp = std::path::PathBuf::from("/tmp/mcpmgr-conflicttest");
        std::fs::create_dir_all(&tmp).unwrap();
        let proj = tmp.to_string_lossy().to_string();
        let name = "mcpmgr-conflictsrv".to_string();

        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = remove_server(name.clone(), Scope::Local, Some(proj.clone()));

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e.clone()).expect("add user");
        add_server(name.clone(), Scope::Local, Some(proj.clone()), e).expect("add local");

        let conflicts = list_conflicts(Some(proj.clone())).expect("list_conflicts");
        let c = conflicts
            .iter()
            .find(|c| c.name == name)
            .expect("Konflikt gefunden");
        assert!(c.definitions.len() >= 2, "mindestens zwei Definitionen");
        assert_eq!(c.effective_scope, Scope::Local, "local gewinnt");
        assert!(c.identical, "gleiche Definition -> identical");

        // Aufräumen.
        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = remove_server(name.clone(), Scope::Local, Some(proj));
        eprintln!("list_conflicts OK");
    }

    /// Opt-in: Umbenennen eines DEAKTIVIERTEN user-Servers hält ihn deaktiviert
    /// (bleibt im Stash, wird nicht aktiviert). Nur mit `-- --ignored`.
    #[test]
    #[ignore]
    fn rename_disabled_user_server_stays_disabled() {
        let name = "mcpmgr-renametest".to_string();
        let new_name = "mcpmgr-renametest2".to_string();
        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = crate::stash::remove(&name);
        let _ = crate::stash::remove(&new_name);

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e).expect("add");
        toggle_user_server(name.clone(), false, None).expect("disable"); // -> Stash
        assert!(crate::stash::peek(&name).is_some(), "vor Rename im Stash");

        rename_server_impl(&cfg(), name.clone(), Scope::User, None, new_name.clone())
            .expect("rename");

        assert!(crate::stash::peek(&name).is_none(), "alter Stash-Eintrag entfernt");
        assert!(
            crate::stash::peek(&new_name).is_some(),
            "neuer Name bleibt deaktiviert im Stash"
        );
        let active = collect_definitions(&default_project_path())
            .into_iter()
            .any(|d| d.scope == Scope::User && d.name == new_name);
        assert!(!active, "neuer Name darf NICHT aktiv in der Config sein");

        let _ = crate::stash::remove(&new_name);
        eprintln!("rename disabled OK");
    }

    /// Opt-in: mcpjson-Toggle schreibt korrekt in settings.local.json.
    #[test]
    #[ignore]
    fn toggle_mcpjson_roundtrip() {
        use crate::config_read::collect_disabled;
        let name = "mcpmgr-toggletest";
        let dir = default_project_path();

        toggle_mcpjson_server(name.into(), None, false).expect("disable");
        assert!(collect_disabled(&dir).disabled.contains(name), "in disabled");

        toggle_mcpjson_server(name.into(), None, true).expect("enable");
        let d = collect_disabled(&dir);
        assert!(!d.disabled.contains(name), "nicht mehr disabled");
        assert!(d.enabled.contains(name), "jetzt enabled");
        eprintln!("toggle_mcpjson OK");
    }

    /// Opt-in: user-scope Deaktivieren/Reaktivieren via Stash.
    #[test]
    #[ignore]
    fn user_stash_roundtrip() {
        let name = "mcpmgr-stashtest".to_string();
        let dir = default_project_path();
        let present = || {
            collect_definitions(&dir)
                .into_iter()
                .any(|d| d.scope == Scope::User && d.name == name)
        };

        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = crate::stash::remove(&name);

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e).expect("add");
        assert!(present(), "angelegt");

        toggle_user_server(name.clone(), false, None).expect("disable");
        assert!(!present(), "nach disable weg aus config");
        assert!(crate::stash::peek(&name).is_some(), "im stash");

        let servers =
            gather_servers(&Mutex::new(HashMap::new()), &cfg(), None, false, true).expect("list");
        let found = servers
            .iter()
            .find(|s| s.name == name && s.scope == Some(Scope::User));
        assert!(
            matches!(found.map(|s| &s.status), Some(ServerStatus::Disabled)),
            "Stash-Server als Disabled gelistet"
        );
        assert_eq!(found.map(|s| s.enabled), Some(false));

        toggle_user_server(name.clone(), true, None).expect("enable");
        assert!(present(), "nach enable wieder da");
        assert!(crate::stash::peek(&name).is_none(), "stash geleert");

        remove_server(name.clone(), Scope::User, None).expect("cleanup");
        eprintln!("user_stash OK");
    }

    /// Opt-in: Scope-Wechsel user -> local (verifiziertes Copy-then-remove).
    #[test]
    #[ignore]
    fn set_scope_roundtrip() {
        let name = "mcpmgr-scopetest".to_string();
        let dir = default_project_path();
        let proj = dir.to_string_lossy().to_string();

        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = remove_server(name.clone(), Scope::Local, Some(proj.clone()));

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e).expect("add");

        set_scope(name.clone(), Scope::User, Scope::Local, None, Some(proj.clone()))
            .expect("set_scope");

        let defs = collect_definitions(&dir);
        let in_local = defs.iter().any(|d| d.scope == Scope::Local && d.name == name);
        let in_user = defs.iter().any(|d| d.scope == Scope::User && d.name == name);
        assert!(in_local && !in_user, "in local, nicht mehr in user");

        remove_server(name.clone(), Scope::Local, Some(proj)).expect("cleanup");
        eprintln!("set_scope OK");
    }

    /// Leerer Zielname wird abgelehnt, bevor irgendetwas an der Umgebung passiert.
    #[test]
    fn clone_server_rejects_empty_name() {
        let err = clone_server(
            "irgendwas".into(),
            Scope::User,
            None,
            "   ".into(),
            Scope::User,
            None,
        )
        .expect_err("leerer Name muss abgelehnt werden");
        assert!(matches!(err, AppError::Io(_)));
    }

    /// Opt-in: Duplizieren user -> local. Quelle bleibt bestehen, Klon entsteht
    /// unter neuem Namen.
    #[test]
    #[ignore]
    fn clone_server_roundtrip() {
        let name = "mcpmgr-clonetest".to_string();
        let clone_name = "mcpmgr-clonetest-kopie".to_string();
        let dir = default_project_path();
        let proj = dir.to_string_lossy().to_string();

        let _ = remove_server(name.clone(), Scope::User, None);
        let _ = remove_server(clone_name.clone(), Scope::Local, Some(proj.clone()));

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::User, None, e).expect("add");

        clone_server(
            name.clone(),
            Scope::User,
            None,
            clone_name.clone(),
            Scope::Local,
            Some(proj.clone()),
        )
        .expect("clone_server");

        let defs = collect_definitions(&dir);
        let src_stays = defs.iter().any(|d| d.scope == Scope::User && d.name == name);
        let clone_here = defs.iter().any(|d| d.scope == Scope::Local && d.name == clone_name);
        assert!(src_stays, "Quelle bleibt in user");
        assert!(clone_here, "Klon liegt in local");

        // Kollision: erneutes Klonen auf denselben Zielnamen muss scheitern.
        let dup = clone_server(
            name.clone(),
            Scope::User,
            None,
            clone_name.clone(),
            Scope::Local,
            Some(proj.clone()),
        );
        assert!(dup.is_err(), "Kollision wird abgelehnt");

        remove_server(name, Scope::User, None).expect("cleanup src");
        remove_server(clone_name, Scope::Local, Some(proj)).expect("cleanup clone");
        eprintln!("clone_server OK");
    }

    /// Opt-in: Klonen auf den Namen eines nur deaktivierten (im Stash liegenden)
    /// User-Servers muss als Kollision abgelehnt werden – sonst würde der Klon
    /// den deaktivierten Eintrag beim nächsten Deaktivieren lautlos überschreiben.
    #[test]
    #[ignore]
    fn clone_rejects_stashed_name_collision() {
        let src = "mcpmgr-clonesrc".to_string();
        let target = "mcpmgr-clonestash".to_string();
        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            ..Default::default()
        };

        let _ = remove_server(src.clone(), Scope::User, None);
        let _ = crate::stash::remove(&target);

        add_server(src.clone(), Scope::User, None, e.clone()).expect("add src");
        crate::stash::upsert(&target, e).expect("stash target");

        let res = clone_server(src.clone(), Scope::User, None, target.clone(), Scope::User, None);
        assert!(res.is_err(), "Kollision mit Stash-Eintrag muss abgelehnt werden");

        remove_server(src, Scope::User, None).expect("cleanup src");
        crate::stash::remove(&target).expect("cleanup stash");
        eprintln!("clone_rejects_stashed_name_collision OK");
    }

    /// Opt-in: echter Assistent-Aufruf gegen einen bekannten MCP-Server (ruft
    /// `claude -p`, netzabhängig, kostet Tokens). Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn assistant_smoke() {
        let res = crate::assistant::run_assistant(
            "https://www.npmjs.com/package/@modelcontextprotocol/server-filesystem",
            Some("stdio server, started via npx"),
            None,
        )
        .expect("assistant call");
        eprintln!("name={:?}", res.name);
        eprintln!("entry={:?}", res.entry);
        eprintln!("notes={:?}", res.notes);
        eprintln!("error={:?}", res.error);
        eprintln!("raw(erste 300)={}", &res.raw.chars().take(300).collect::<String>());
        assert!(res.entry.is_some() || res.error.is_some());
    }

    /// Opt-in: legt ein Wegwerf-Projekt an (local-Server in /tmp/…), prüft
    /// list_projects und löscht es via delete_project wieder. Bestätigt, dass
    /// der direkte ~/.claude.json-Edit sauber genau einen Eintrag entfernt.
    #[test]
    #[ignore]
    fn project_delete_roundtrip() {
        let tmp = std::path::PathBuf::from("/tmp/mcpmgr-projtest");
        std::fs::create_dir_all(&tmp).unwrap();
        let proj = tmp.to_string_lossy().to_string();
        let name = "mcpmgr-projsrv".to_string();

        let _ = remove_server(name.clone(), Scope::Local, Some(proj.clone()));

        let e = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("echo".into()),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        add_server(name.clone(), Scope::Local, Some(proj.clone()), e).expect("add local");

        let before = list_projects().expect("list_projects");
        assert!(before.iter().any(|p| p.path == proj), "Projekt gelistet");

        delete_project(proj.clone()).expect("delete_project");

        let after = list_projects().expect("list_projects2");
        assert!(!after.iter().any(|p| p.path == proj), "Projekt entfernt");
        assert_eq!(after.len(), before.len() - 1, "genau ein Projekt weniger");
        eprintln!(
            "project_delete OK (vorher={} nachher={})",
            before.len(),
            after.len()
        );
    }
}
