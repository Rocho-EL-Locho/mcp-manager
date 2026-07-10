//! Alle `#[tauri::command]`-Funktionen. Dünne Orchestrierungsschicht über
//! claude_cli / config_read / mask / parse.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::State;

use crate::claude_cli::{home_dir, resolve_claude, run_claude};
use crate::config_read::{
    claude_json_path, collect_definitions, collect_disabled, default_project_path,
    project_settings_local_path, read_json_value, settings_local_path,
};
use crate::mask::{
    entry_has_secrets, mask_entry, mask_summary, redact_json, redact_secrets, summarize_entry,
};
use crate::models::{
    AppError, ClaudeInfo, Introspection, MergedServer, ProjectInfo, Scope, ServerEntry,
    ServerStatus,
};
use crate::parse::{failure_detail, parse_list, status_from_text};
use crate::preflight::RuntimePreflight;

const LIST_TIMEOUT: Duration = Duration::from_secs(45);
const GET_TIMEOUT: Duration = Duration::from_secs(20);
const MUT_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// Zeitbudget für den Introspektions-Handshake. Großzügig, weil der erste
/// `npx`/`uvx`-Start (Download/Cold-Start) spürbar dauern kann.
const INTROSPECT_TIMEOUT: Duration = Duration::from_secs(20);

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

#[derive(Default)]
pub struct AppState {
    status_cache: StatusCache,
    introspection_cache: IntrospectionCache,
}

/// Stabiler Cache-Schlüssel für die Introspektion eines Servers.
fn introspection_key(scope: Scope, name: &str, project_path: &Option<String>) -> String {
    let dir = resolve_project_dir(project_path.clone());
    format!("{}::{}::{}", scope.cli_value(), name, dir.to_string_lossy())
}

/// Prüft beim Start, ob die claude-CLI verfügbar ist, und liefert ihre Version.
#[tauri::command]
pub async fn check_claude() -> Result<ClaudeInfo, AppError> {
    let Some(path) = resolve_claude() else {
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
    let mut servers = gather_servers(&state.status_cache, project_path, reveal, with_status)?;
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
        if let Some(claude) = resolve_claude() {
            if let Ok(out) = run_claude(&claude, &["mcp", "list"], Some(&dir), LIST_TIMEOUT) {
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

/// Einzelnen Server neu health-checken via `claude mcp get <name>`.
#[tauri::command]
pub async fn health_check(
    name: String,
    project_path: Option<String>,
) -> Result<ServerStatus, AppError> {
    let dir = resolve_project_dir(project_path);
    let Some(claude) = resolve_claude() else {
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

    // Unmaskierte Definition auflösen (wie reveal_server_entry): der Handshake
    // braucht die echten env/args, um den Prozess zu starten.
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

    let mut introspection = if entry.command.is_some() {
        // Gibt immer eine Introspection zurück; Start-/Handshake-Fehler stehen in
        // `error`/`logs` (statt als Err), damit die Detail-Ansicht sie zeigen kann.
        crate::introspect::introspect_stdio(&entry, INTROSPECT_TIMEOUT)
    } else {
        // HTTP/SSE: in dieser Version nicht unterstützt (kein Prozess-Start).
        Introspection {
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
            server_name: None,
            server_version: None,
            notes: vec![
                "Introspektion wird derzeit nur für stdio-Server unterstützt (HTTP/SSE folgt)."
                    .into(),
            ],
            logs: None,
            error: None,
            connect_ms: None,
            introspected_at: crate::introspect::unix_now(),
        }
    };

    mask_introspection(&mut introspection);

    state
        .introspection_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, introspection.clone());

    Ok(introspection)
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
fn run_mut(args: &[&str], cwd: Option<PathBuf>, timeout: Duration) -> Result<(), AppError> {
    let claude = resolve_claude().ok_or(AppError::ClaudeNotFound)?;
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
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    add_server_impl(name, scope, project_path, entry)
}

fn add_server_impl(
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    let json = serde_json::to_string(&entry).map_err(|e| AppError::Parse(e.to_string()))?;
    let cwd = cwd_for(scope, &project_path);
    run_mut(
        &["mcp", "add-json", "-s", scope.cli_value(), &name, &json],
        cwd,
        MUT_TIMEOUT,
    )
}

/// Bearbeitet einen Server: alten Stand sichern -> entfernen -> neu anlegen.
/// Schlägt das Neu-Anlegen fehl, wird der alte Stand zurückgerollt.
#[tauri::command]
pub async fn update_server(
    name: String,
    scope: Scope,
    project_path: Option<String>,
    entry: ServerEntry,
) -> Result<(), AppError> {
    update_server_impl(name, scope, project_path, entry)
}

fn update_server_impl(
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
    let new_json = serde_json::to_string(&entry).map_err(|e| AppError::Parse(e.to_string()))?;

    // Alten Eintrag entfernen (falls vorhanden).
    let _ = run_mut(
        &["mcp", "remove", "-s", scope.cli_value(), &name],
        cwd.clone(),
        MUT_TIMEOUT,
    );

    // Neuen Eintrag anlegen.
    match run_mut(
        &["mcp", "add-json", "-s", scope.cli_value(), &name, &new_json],
        cwd.clone(),
        MUT_TIMEOUT,
    ) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Rollback auf den alten Stand.
            if let Some(old_entry) = old {
                if let Ok(old_json) = serde_json::to_string(&old_entry) {
                    let _ = run_mut(
                        &["mcp", "add-json", "-s", scope.cli_value(), &name, &old_json],
                        cwd,
                        MUT_TIMEOUT,
                    );
                }
            }
            Err(err)
        }
    }
}

/// Entfernt einen Server via `claude mcp remove`.
#[tauri::command]
pub async fn remove_server(
    name: String,
    scope: Scope,
    project_path: Option<String>,
) -> Result<(), AppError> {
    remove_server_impl(name, scope, project_path)
}

fn remove_server_impl(
    name: String,
    scope: Scope,
    project_path: Option<String>,
) -> Result<(), AppError> {
    let cwd = cwd_for(scope, &project_path);
    run_mut(
        &["mcp", "remove", "-s", scope.cli_value(), &name],
        cwd,
        MUT_TIMEOUT,
    )
}

/// OAuth-Anmeldung für HTTP/SSE-Server bzw. Connectoren. Öffnet ggf. den Browser.
#[tauri::command]
pub async fn login_server(name: String) -> Result<(), AppError> {
    run_mut(&["mcp", "login", &name], None, LOGIN_TIMEOUT)
}

/// Gespeicherte OAuth-Credentials eines Servers löschen.
#[tauri::command]
pub async fn logout_server(name: String) -> Result<(), AppError> {
    run_mut(&["mcp", "logout", &name], None, MUT_TIMEOUT)
}

/// Setzt Freigabe-/Ablehnungs-Entscheidungen für .mcp.json-Server im Projekt zurück.
#[tauri::command]
pub async fn reset_project_choices(project_path: Option<String>) -> Result<(), AppError> {
    let cwd = Some(resolve_project_dir(project_path));
    run_mut(&["mcp", "reset-project-choices"], cwd, MUT_TIMEOUT)
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
    name: String,
    enabled: bool,
    entry: Option<ServerEntry>,
) -> Result<(), AppError> {
    toggle_user_server_impl(name, enabled, entry)
}

fn toggle_user_server_impl(
    name: String,
    enabled: bool,
    entry: Option<ServerEntry>,
) -> Result<(), AppError> {
    if enabled {
        // Reaktivieren: Definition aus dem Stash zurückspielen.
        let item = crate::stash::peek(&name).ok_or(AppError::StashMissing)?;
        let json = serde_json::to_string(&item.entry).map_err(|e| AppError::Parse(e.to_string()))?;
        run_mut(
            &["mcp", "add-json", "-s", "user", &name, &json],
            None,
            MUT_TIMEOUT,
        )?;
        crate::stash::remove(&name)?; // erst nach Erfolg
        Ok(())
    } else {
        // Deaktivieren: aktuelle Definition sichern, DANN entfernen.
        let dir = default_project_path();
        let current = collect_definitions(&dir)
            .into_iter()
            .find(|d| d.scope == Scope::User && d.name == name)
            .map(|d| d.entry)
            .or(entry)
            .ok_or_else(|| AppError::Io("Server-Definition nicht gefunden".into()))?;
        crate::stash::upsert(&name, current)?;
        run_mut(&["mcp", "remove", "-s", "user", &name], None, MUT_TIMEOUT)?;
        Ok(())
    }
}

/// Verschiebt einen Server in einen anderen Scope: verifiziertes Anlegen im
/// Ziel-Scope, danach Entfernen aus dem Quell-Scope (nie umgekehrt).
#[tauri::command]
pub async fn set_scope(
    name: String,
    from_scope: Scope,
    to_scope: Scope,
    from_project: Option<String>,
    to_project: Option<String>,
) -> Result<(), AppError> {
    set_scope_impl(name, from_scope, to_scope, from_project, to_project)
}

fn set_scope_impl(
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
    let to_dir = resolve_project_dir(to_project.clone());

    let entry = collect_definitions(&from_dir)
        .into_iter()
        .find(|d| d.scope == from_scope && d.name == name)
        .map(|d| d.entry)
        .ok_or_else(|| AppError::Io("Quell-Definition nicht gefunden".into()))?;
    let json = serde_json::to_string(&entry).map_err(|e| AppError::Parse(e.to_string()))?;

    // 1. Im Ziel-Scope anlegen.
    run_mut(
        &["mcp", "add-json", "-s", to_scope.cli_value(), &name, &json],
        cwd_for(to_scope, &to_project),
        MUT_TIMEOUT,
    )?;

    // 2. Erfolg verifizieren.
    let landed = collect_definitions(&to_dir)
        .into_iter()
        .any(|d| d.scope == to_scope && d.name == name);
    if !landed {
        return Err(AppError::Io(
            "Anlegen im Ziel-Scope wurde nicht bestätigt – Quelle bleibt unangetastet.".into(),
        ));
    }

    // 3. Erst jetzt aus dem Quell-Scope entfernen.
    if let Err(e) = run_mut(
        &["mcp", "remove", "-s", from_scope.cli_value(), &name],
        cwd_for(from_scope, &from_project),
        MUT_TIMEOUT,
    ) {
        return Err(AppError::Io(format!(
            "In Ziel-Scope kopiert, aber alte Kopie ({}) konnte nicht entfernt werden: {}",
            from_scope.cli_value(),
            e
        )));
    }

    Ok(())
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
pub fn delete_project(path: String) -> Result<(), AppError> {
    let cj = claude_json_path();
    let mut root = read_json_value(&cj).ok_or_else(|| AppError::Io("~/.claude.json nicht lesbar".into()))?;
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
    // Sicherung ist verpflichtend: scheitert sie, wird NICHT geschrieben.
    // (Die Race gegen ein laufendes Claude Code bleibt inhärent – ein Lock ist
    // hier nicht möglich.)
    let bak = cj.with_file_name(".claude.json.mcpmgr.bak");
    std::fs::copy(&cj, &bak).map_err(|e| AppError::Io(format!("Backup fehlgeschlagen: {e}")))?;
    crate::toggles::atomic_write_json(&cj, &root)
}

/// Startet den Claude-Assistenten, der aus einem Link einen Config-Vorschlag
/// erzeugt. Schreibt nichts – der Vorschlag geht ans Formular.
#[tauri::command]
pub async fn run_claude_assistant(
    url: String,
    extra_context: Option<String>,
) -> Result<crate::assistant::AssistantResult, AppError> {
    crate::assistant::run_assistant(&url, extra_context.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Die Commands sind jetzt async; die Tests rufen die synchronen `_impl`-
    // Funktionen auf. Diese Aliase überschatten die (async) Glob-Importe.
    use super::{
        add_server_impl as add_server, remove_server_impl as remove_server,
        set_scope_impl as set_scope, toggle_user_server_impl as toggle_user_server,
        update_server_impl as update_server,
    };

    /// Opt-in-Integrationstest: ruft echtes `claude mcp list` auf (health-checkt
    /// alle Server, ~langsam) und prüft, dass Merge, Maskierung und Extern-
    /// Klassifizierung sinnvoll sind. Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_servers_end_to_end() {
        let servers =
            gather_servers(&Mutex::new(HashMap::new()), None, false, true).expect("list_servers");
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
            gather_servers(&Mutex::new(HashMap::new()), None, false, true).expect("list");
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

    /// Opt-in: echter Assistent-Aufruf gegen einen bekannten MCP-Server (ruft
    /// `claude -p`, netzabhängig, kostet Tokens). Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn assistant_smoke() {
        let res = crate::assistant::run_assistant(
            "https://www.npmjs.com/package/@modelcontextprotocol/server-filesystem",
            Some("stdio server, started via npx"),
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
