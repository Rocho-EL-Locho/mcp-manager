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
