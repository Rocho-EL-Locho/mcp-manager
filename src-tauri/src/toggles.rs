//! Atomares Editieren der enable/disable-Arrays für .mcp.json-Server.
//!
//! Geschrieben wird in ~/.claude/settings.local.json (nutzer-privat, nicht
//! eingecheckt). Unbekannte Keys bleiben erhalten; der Write ist atomar
//! (Temp-Datei + rename).

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::config_read::read_json_value;
use crate::models::AppError;

pub fn atomic_write_json(path: &Path, value: &Value) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("kein übergeordnetes Verzeichnis".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::Io(e.to_string()))?;
    let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("tmp");
    let tmp = parent.join(format!(".{fname}.mcpmgr.tmp"));

    let mut text = serde_json::to_string_pretty(value).map_err(|e| AppError::Parse(e.to_string()))?;
    text.push('\n');
    std::fs::write(&tmp, &text).map_err(|e| AppError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

fn string_vec(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Aktiviert/deaktiviert einen .mcp.json-Server über die enable/disable-Arrays
/// der angegebenen settings-Datei (`settings_path`).
pub fn toggle_mcpjson(settings_path: &Path, name: &str, enabled: bool) -> Result<(), AppError> {
    let path = settings_path;
    let mut root = read_json_value(path).unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();

    let mut en = string_vec(obj, "enabledMcpjsonServers");
    let mut dis = string_vec(obj, "disabledMcpjsonServers");

    if enabled {
        dis.retain(|s| s != name);
        if !en.iter().any(|s| s == name) {
            en.push(name.to_string());
        }
    } else {
        en.retain(|s| s != name);
        if !dis.iter().any(|s| s == name) {
            dis.push(name.to_string());
        }
    }

    obj.insert("enabledMcpjsonServers".into(), json!(en));
    obj.insert("disabledMcpjsonServers".into(), json!(dis));
    atomic_write_json(path, &root)
}
