//! Liest die MCP-Definitionen aus Claude Codes Config-Dateien (nur lesend).
//!
//! Definitionen sind autoritativ (command/args/env/url/headers/scope); der
//! Status kommt separat aus der CLI (siehe parse.rs). Geschrieben wird hier nie.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::claude_cli::home_dir;
use crate::models::{Scope, ServerEntry};

/// Eine Definition inkl. Scope-Zuordnung.
pub struct ScopedEntry {
    pub scope: Scope,
    pub name: String,
    pub entry: ServerEntry,
    pub project_path: Option<String>,
}

/// enable/disable-Zustand für .mcp.json-Server (project scope).
#[derive(Default)]
pub struct DisabledInfo {
    pub disabled: HashSet<String>,
    pub enabled: HashSet<String>,
    pub enable_all: bool,
}

pub fn claude_json_path() -> PathBuf {
    home_dir().unwrap_or_default().join(".claude.json")
}

pub fn settings_path() -> PathBuf {
    home_dir().unwrap_or_default().join(".claude/settings.json")
}

pub fn settings_local_path() -> PathBuf {
    home_dir().unwrap_or_default().join(".claude/settings.local.json")
}

/// Projekt-lokale settings.local.json: `<projekt>/.claude/settings.local.json`.
pub fn project_settings_local_path(project_path: &Path) -> PathBuf {
    project_path.join(".claude/settings.local.json")
}

/// Standard-Projektpfad, wenn keiner angegeben ist (Home-Verzeichnis).
pub fn default_project_path() -> PathBuf {
    home_dir().unwrap_or_default()
}

fn read_value(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Öffentlicher Lese-Helfer (für toggles.rs): JSON-Datei -> Value.
pub fn read_json_value(path: &Path) -> Option<Value> {
    read_value(path)
}

/// Deserialisiert ein `mcpServers`-Objekt tolerant in (Name, ServerEntry).
fn parse_servers_map(v: &Value) -> Vec<(String, ServerEntry)> {
    let mut out = Vec::new();
    if let Some(obj) = v.as_object() {
        for (name, def) in obj {
            let entry = serde_json::from_value::<ServerEntry>(def.clone()).unwrap_or_default();
            out.push((name.clone(), entry));
        }
    }
    out
}

/// Sammelt alle Definitionen für user-, local- (Projekt) und project-Scope (.mcp.json).
pub fn collect_definitions(project_path: &Path) -> Vec<ScopedEntry> {
    let mut out = Vec::new();
    let key = project_path.to_string_lossy().to_string();

    if let Some(root) = read_value(&claude_json_path()) {
        // user scope
        if let Some(mcp) = root.get("mcpServers") {
            for (name, entry) in parse_servers_map(mcp) {
                out.push(ScopedEntry {
                    scope: Scope::User,
                    name,
                    entry,
                    project_path: None,
                });
            }
        }
        // local scope: projects[<pfad>].mcpServers
        if let Some(pentry) = root
            .get("projects")
            .and_then(|p| p.as_object())
            .and_then(|m| m.get(&key))
        {
            if let Some(mcp) = pentry.get("mcpServers") {
                for (name, entry) in parse_servers_map(mcp) {
                    out.push(ScopedEntry {
                        scope: Scope::Local,
                        name,
                        entry,
                        project_path: Some(key.clone()),
                    });
                }
            }
        }
    }

    // project scope: <projekt>/.mcp.json
    if let Some(root) = read_value(&project_path.join(".mcp.json")) {
        if let Some(mcp) = root.get("mcpServers") {
            for (name, entry) in parse_servers_map(mcp) {
                out.push(ScopedEntry {
                    scope: Scope::Project,
                    name,
                    entry,
                    project_path: Some(key.clone()),
                });
            }
        }
    }

    out
}

fn merge_arrays(v: &Value, info: &mut DisabledInfo) {
    if let Some(arr) = v.get("disabledMcpjsonServers").and_then(|a| a.as_array()) {
        for s in arr.iter().filter_map(|x| x.as_str()) {
            info.disabled.insert(s.to_string());
        }
    }
    if let Some(arr) = v.get("enabledMcpjsonServers").and_then(|a| a.as_array()) {
        for s in arr.iter().filter_map(|x| x.as_str()) {
            info.enabled.insert(s.to_string());
        }
    }
    if let Some(true) = v.get("enableAllProjectMcpServers").and_then(|b| b.as_bool()) {
        info.enable_all = true;
    }
}

/// Ermittelt enable/disable-Zustand aus settings(.local).json und dem Projekteintrag.
pub fn collect_disabled(project_path: &Path) -> DisabledInfo {
    let mut info = DisabledInfo::default();
    let key = project_path.to_string_lossy().to_string();

    // Reihenfolge: globale settings, dann projekt-lokale settings.local.json,
    // dann die projects[key]-Arrays aus ~/.claude.json.
    for p in [
        settings_path(),
        settings_local_path(),
        project_settings_local_path(project_path),
    ] {
        if let Some(v) = read_value(&p) {
            merge_arrays(&v, &mut info);
        }
    }
    if let Some(pentry) = read_value(&claude_json_path())
        .as_ref()
        .and_then(|r| r.get("projects"))
        .and_then(|p| p.as_object())
        .and_then(|m| m.get(&key))
    {
        merge_arrays(pentry, &mut info);
    }
    info
}

impl DisabledInfo {
    /// Ist ein .mcp.json-Server (project scope) effektiv aktiv?
    pub fn is_enabled(&self, name: &str) -> bool {
        if self.disabled.contains(name) {
            return false;
        }
        self.enable_all || self.enabled.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in-Smoketest gegen die echte ~/.claude.json auf dieser Maschine.
    /// Läuft nur mit `cargo test -- --ignored --nocapture` und startet KEIN claude.
    #[test]
    #[ignore]
    fn dump_real_definitions() {
        let dir = default_project_path();
        eprintln!("Projekt: {}", dir.display());
        let defs = collect_definitions(&dir);
        for d in &defs {
            eprintln!(
                "  [{:?}] {}  (cmd={:?} url={:?} env={} args={})",
                d.scope,
                d.name,
                d.entry.command,
                d.entry.url,
                d.entry.env.as_ref().map(|m| m.len()).unwrap_or(0),
                d.entry.args.as_ref().map(|a| a.len()).unwrap_or(0),
            );
        }
        eprintln!("Definitionen gesamt: {}", defs.len());
        let dis = collect_disabled(&dir);
        eprintln!(
            "disabled={:?} enabled={:?} enable_all={}",
            dis.disabled, dis.enabled, dis.enable_all
        );
        assert!(!defs.is_empty(), "es sollten Definitionen gefunden werden");
    }
}
