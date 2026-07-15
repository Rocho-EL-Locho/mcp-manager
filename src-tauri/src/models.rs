use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Ergebnis der Startprüfung: wo liegt die claude-CLI und welche Version.
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeInfo {
    pub path: String,
    pub version: String,
    pub ok: bool,
}

/// Konfigurations-Scope in Claude Code.
/// user = global, local = projekt-privat (~/.claude.json projects[pfad]),
/// project = eingecheckt (.mcp.json im Projekt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    Local,
    Project,
}

impl Scope {
    /// CLI-Wert für das `--scope`-Flag.
    pub fn cli_value(&self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Local => "local",
            Scope::Project => "project",
        }
    }
}

/// Autoritative Server-Definition, gelesen aus den JSON-Dateien.
/// `transport` bleibt bewusst ein String (tolerant gegen unbekannte Typen).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerEntry {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

/// Aus `claude mcp list`/`get` geparster Health-Status.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerStatus {
    Connected,
    Failed { detail: Option<String> },
    NeedsAuth,
    PendingApproval,
    Disabled,
    Unknown,
}

/// Eine Zeile aus `claude mcp list`, tolerant geparst.
#[derive(Debug, Clone)]
pub struct ListItem {
    pub name: String,
    pub summary: String,
    pub status: ServerStatus,
}

/// Ein vom Server bereitgestelltes Tool (aus `tools/list`).
#[derive(Debug, Clone, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-Schema der Eingabe; maskiert, falls es geheim aussehende Werte enthält.
    #[serde(rename = "inputSchema", skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

/// Eine vom Server bereitgestellte Ressource (aus `resources/list`).
#[derive(Debug, Clone, Serialize)]
pub struct McpResource {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Ein vom Server bereitgestellter Prompt (aus `prompts/list`).
#[derive(Debug, Clone, Serialize)]
pub struct McpPrompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Ergebnis des MCP-Handshakes: was ein Server tatsächlich bereitstellt.
#[derive(Debug, Clone, Serialize)]
pub struct Introspection {
    pub tools: Vec<McpTool>,
    pub resources: Vec<McpResource>,
    pub prompts: Vec<McpPrompt>,
    /// Name/Version aus `serverInfo` des `initialize`-Ergebnisses.
    #[serde(rename = "serverName", skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(rename = "serverVersion", skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// Nicht-fatale Hinweise (z. B. „resources nicht unterstützt", HTTP/SSE-Hinweis).
    pub notes: Vec<String>,
    /// Redigierter stderr-Auszug des Server-Subprozesses (nur stdio). None wenn leer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
    /// Fehlermeldung, falls Start/Handshake fehlschlug (redigiert). None bei Erfolg.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Dauer (ms) von Prozessstart bis initialize-Antwort (nur bei stdio-Erfolg).
    /// None bei Fehlschlag oder HTTP/SSE (kein Prozess-Start).
    #[serde(rename = "connectMs", skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// Unix-Zeitstempel (Sekunden) der Introspektion.
    #[serde(rename = "introspectedAt")]
    pub introspected_at: u64,
}

/// Was das Frontend tatsächlich rendert: Definition (aus JSON) + Status (aus CLI).
#[derive(Debug, Clone, Serialize)]
pub struct MergedServer {
    pub name: String,
    /// None => extern verwaltet (claude.ai-Connector / Plugin), nicht editierbar.
    pub scope: Option<Scope>,
    /// Herkunft für die Anzeige: "user" | "local" | "project" | "connector" | "plugin" | "unbekannt".
    pub origin: String,
    pub project_path: Option<String>,
    /// Definition; None bei externen Servern ohne lokale JSON-Definition.
    pub entry: Option<ServerEntry>,
    /// Maskierte Kurzbeschreibung (command bzw. url) für die Listenzeile.
    pub summary: String,
    pub status: ServerStatus,
    pub enabled: bool,
    pub editable: bool,
    pub has_secrets: bool,
    /// true, wenn derselbe Name in mehreren Scopes existiert.
    pub collision: bool,
    /// Anzahl introspizierter Tools/Ressourcen/Prompts (nur gesetzt, wenn der
    /// Server bereits introspiziert wurde – aus dem Introspektions-Cache).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_count: Option<usize>,
    /// Verbindungs-/Startzeit (ms) aus dem letzten erfolgreichen Introspektions-
    /// Handshake (Prozessstart bis initialize). Nur gesetzt, wenn introspiziert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    /// Preflight: benötigte Laufzeit (aus `command`) fehlt auf PATH.
    /// None => nicht zutreffend (HTTP/SSE oder extern ohne Definition),
    /// Some(false) => vorhanden, Some(true) => fehlt -> Warnung.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_missing: Option<bool>,
}

/// Eine einzelne Definition innerhalb eines Namenskonflikts.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictDefinition {
    pub scope: Scope,
    pub project_path: Option<String>,
    /// Maskierte Kurzbeschreibung (keine Secrets ins Webview).
    pub summary: String,
    /// Hash über die normalisierte Definition – nur für den Gleichheitsvergleich.
    pub fingerprint: u64,
}

/// Ein Namenskonflikt: derselbe Servername in mehreren Scopes.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictInfo {
    pub name: String,
    pub definitions: Vec<ConflictDefinition>,
    /// Scope, dessen Definition Claude Code tatsächlich nutzt (Präzedenz
    /// local > project > user).
    pub effective_scope: Scope,
    /// true, wenn alle Definitionen inhaltsgleich sind (nur Duplikat, kein echter Konflikt).
    pub identical: bool,
}

/// Ein Claude-Code-Projekt (Eintrag unter `projects` in ~/.claude.json).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub path: String,
    /// Anzahl der local-scope Server dieses Projekts.
    pub server_count: usize,
    /// Existiert das Verzeichnis noch auf der Platte?
    pub exists: bool,
    pub is_home: bool,
}

/// Zentraler Fehlertyp. Wird als Klartext-String ans Frontend serialisiert.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("claude-CLI nicht gefunden")]
    ClaudeNotFound,
    #[error("claude-Aufruf fehlgeschlagen (Code {code:?}): {stderr}")]
    CliFailed { code: Option<i32>, stderr: String },
    #[error("Zeitüberschreitung beim Aufruf von claude")]
    Timeout,
    #[error("E/A-Fehler: {0}")]
    Io(String),
    #[error("Parse-Fehler: {0}")]
    Parse(String),
    #[error("Keine gesicherte Definition im Stash gefunden")]
    StashMissing,
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
