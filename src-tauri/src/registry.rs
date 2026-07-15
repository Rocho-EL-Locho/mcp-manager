//! MCP-Registry-Browser: Suche im offiziellen Katalog
//! (`registry.modelcontextprotocol.io`) und Übersetzung eines Katalog-Eintrags
//! in ein `ServerEntry` fürs Formular.
//!
//! Es wird NICHTS geschrieben – ein „Installieren" befüllt nur das Formular.
//! Analog zum Link-Assistenten (`assistant.rs`) werden env/header-WERTE nie aus
//! der Registry übernommen: nur die Keys (mit leerem Wert) landen im Formular,
//! der Nutzer trägt Secrets selbst ein.
//!
//! Die Live-API nutzt durchgängig camelCase (`registryType`, `runtimeHint`,
//! `environmentVariables`, `metadata.nextCursor`); Felder sind oft abwesend
//! (reine Remote-Server haben keine `packages`) – daher tolerant parsen.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::models::{AppError, ServerEntry};

const BASE_URL: &str = "https://registry.modelcontextprotocol.io/v0/servers";
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(15);
/// Obergrenze für den gelesenen Antwort-Body (OOM-Schutz).
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const PAGE_LIMIT: &str = "30";

// ---------------------------------------------------------------------------
// Deserialisierung der Registry-Antwort (camelCase, tolerant)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistrySearchResponse {
    servers: Vec<RegistryServer>,
    metadata: RegistryMetadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryMetadata {
    next_cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryServer {
    name: String,
    title: Option<String>,
    description: String,
    version: String,
    repository: Option<RegistryRepository>,
    packages: Vec<RegistryPackage>,
    remotes: Vec<RegistryRemote>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryRepository {
    url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryPackage {
    registry_type: String,
    identifier: String,
    version: Option<String>,
    runtime_hint: Option<String>,
    runtime_arguments: Vec<RegistryArgument>,
    package_arguments: Vec<RegistryArgument>,
    environment_variables: Vec<RegistryEnvVar>,
}

/// Ein Positional-/Named-Argument der Registry. Wir übernehmen den `value`
/// (bzw. ersatzweise `name`) als rohes Argument-Token.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryArgument {
    value: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryEnvVar {
    name: String,
    description: Option<String>,
    is_required: bool,
    is_secret: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryRemote {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    headers: Vec<RegistryEnvVar>,
}

// ---------------------------------------------------------------------------
// Frontend-freundliche Ausgabe (Mapping passiert hier im Backend)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EnvVarInfo {
    pub name: String,
    pub required: bool,
    pub secret: bool,
    pub description: Option<String>,
}

/// Eine installierbare Variante eines Katalog-Servers (ein Package oder ein
/// Remote). `entry` befüllt das Formular; `secret_keys` markiert die zu
/// maskierenden Felder.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryVariant {
    pub kind: String,
    pub label: String,
    pub entry: ServerEntry,
    pub env_vars: Vec<EnvVarInfo>,
    pub secret_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryEntryView {
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    pub repository_url: Option<String>,
    pub variants: Vec<RegistryVariant>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistrySearchPage {
    pub servers: Vec<RegistryEntryView>,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Mapping-Helfer
// ---------------------------------------------------------------------------

/// Argument-Tokens: benannte Argumente ergeben `name` gefolgt von `value`
/// (z. B. `--directory /pfad`), positionale nur `value`, reine Flags nur `name`.
fn arg_tokens(args: &[RegistryArgument]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        let name = a.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let value = a.value.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match (name, value) {
            (Some(n), Some(v)) => {
                out.push(n.to_string());
                out.push(v.to_string());
            }
            (Some(n), None) => out.push(n.to_string()),
            (None, Some(v)) => out.push(v.to_string()),
            (None, None) => {}
        }
    }
    out
}

/// env-Keys mit LEEREM Wert (Werte kommen nie aus der Registry).
fn env_keys_empty(vars: &[RegistryEnvVar]) -> Option<BTreeMap<String, String>> {
    if vars.is_empty() {
        return None;
    }
    Some(
        vars.iter()
            .filter(|v| !v.name.is_empty())
            .map(|v| (v.name.clone(), String::new()))
            .collect(),
    )
}

fn env_infos(vars: &[RegistryEnvVar], force_secret: bool) -> Vec<EnvVarInfo> {
    vars.iter()
        .filter(|v| !v.name.is_empty())
        .map(|v| EnvVarInfo {
            name: v.name.clone(),
            required: v.is_required,
            secret: force_secret || v.is_secret,
            description: v.description.clone(),
        })
        .collect()
}

/// Ein Package (npm/pypi/oci) → stdio-Variante. `None` bei unbekanntem Typ
/// oder fehlendem Identifier.
fn package_variant(pkg: &RegistryPackage) -> Option<RegistryVariant> {
    let id = pkg.identifier.trim();
    if id.is_empty() {
        return None;
    }
    let ver = pkg.version.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let runtime_args = arg_tokens(&pkg.runtime_arguments);
    let pkg_args = arg_tokens(&pkg.package_arguments);

    let (kind, command, mut args) = match pkg.registry_type.as_str() {
        "npm" => {
            let cmd = pkg
                .runtime_hint
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "npx".into());
            // npx braucht -y für nicht-interaktiven Start, wenn die Registry
            // keine eigenen Runtime-Argumente vorgibt.
            let mut a = if runtime_args.is_empty() && cmd == "npx" {
                vec!["-y".to_string()]
            } else {
                runtime_args
            };
            let spec = match ver {
                Some(v) => format!("{id}@{v}"),
                None => id.to_string(),
            };
            a.push(spec);
            ("npm", cmd, a)
        }
        "pypi" => {
            let cmd = pkg
                .runtime_hint
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "uvx".into());
            let mut a = runtime_args;
            a.push(id.to_string());
            ("pypi", cmd, a)
        }
        "oci" => {
            let cmd = pkg
                .runtime_hint
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "docker".into());
            let image = match ver {
                Some(v) => format!("{id}:{v}"),
                None => id.to_string(),
            };
            ("oci", cmd, vec!["run".into(), "--rm".into(), "-i".into(), image])
        }
        _ => return None,
    };
    args.extend(pkg_args);

    let entry = ServerEntry {
        // stdio: kein `type`-Key (command impliziert stdio), spiegelt Presets.
        transport: None,
        command: Some(command),
        args: Some(args),
        env: env_keys_empty(&pkg.environment_variables),
        url: None,
        headers: None,
    };
    let secret_keys = pkg
        .environment_variables
        .iter()
        .filter(|v| v.is_secret && !v.name.is_empty())
        .map(|v| v.name.clone())
        .collect();

    Some(RegistryVariant {
        kind: kind.into(),
        label: format!("{kind} · {id}"),
        entry,
        env_vars: env_infos(&pkg.environment_variables, false),
        secret_keys,
    })
}

/// Ein Remote (streamable-http/sse) → http/sse-Variante. Header-Keys gelten
/// als sensibel (Authorization etc.), Werte bleiben leer.
fn remote_variant(remote: &RegistryRemote) -> Option<RegistryVariant> {
    let url = remote.url.trim();
    if url.is_empty() {
        return None;
    }
    let transport = if remote.kind == "sse" { "sse" } else { "http" };
    let headers = if remote.headers.is_empty() {
        None
    } else {
        Some(
            remote
                .headers
                .iter()
                .filter(|h| !h.name.is_empty())
                .map(|h| (h.name.clone(), String::new()))
                .collect(),
        )
    };
    let secret_keys = remote
        .headers
        .iter()
        .filter(|h| !h.name.is_empty())
        .map(|h| h.name.clone())
        .collect();

    let entry = ServerEntry {
        transport: Some(transport.into()),
        command: None,
        args: None,
        env: None,
        url: Some(url.to_string()),
        headers,
    };
    Some(RegistryVariant {
        kind: transport.into(),
        label: format!("{transport} · {url}"),
        entry,
        env_vars: env_infos(&remote.headers, true),
        secret_keys,
    })
}

fn to_view(server: RegistryServer) -> RegistryEntryView {
    let mut variants = Vec::new();
    for p in &server.packages {
        if let Some(v) = package_variant(p) {
            variants.push(v);
        }
    }
    for r in &server.remotes {
        if let Some(v) = remote_variant(r) {
            variants.push(v);
        }
    }
    let title = server
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| server.name.clone());
    let repository_url = server
        .repository
        .and_then(|r| r.url)
        .filter(|u| !u.trim().is_empty());

    RegistryEntryView {
        name: server.name,
        title,
        description: server.description,
        version: server.version,
        repository_url,
        variants,
    }
}

/// Übersetzt HTTP-Fehlerstatus in verständliche Meldungen.
fn http_status_error(code: u16) -> AppError {
    match code {
        429 => AppError::Io("Registry: zu viele Anfragen (429) – bitte kurz warten".into()),
        500..=599 => AppError::Io(format!("Registry-Serverfehler (HTTP {code})")),
        _ => AppError::Io(format!("Registry antwortete mit HTTP {code}")),
    }
}

/// Führt die eigentliche Registry-Suche aus (blockierend). `query` leer ⇒
/// Anfangsliste; `cursor` für die Paginierung.
pub fn fetch(query: &str, cursor: Option<&str>) -> Result<RegistrySearchPage, AppError> {
    // Redirects erlaubt: öffentliche API ohne Secret-Header (anders als introspect.rs).
    let agent = ureq::AgentBuilder::new().timeout(REGISTRY_TIMEOUT).build();
    let mut req = agent.get(BASE_URL).query("limit", PAGE_LIMIT);
    let q = query.trim();
    if !q.is_empty() {
        req = req.query("search", q);
    }
    if let Some(c) = cursor.map(str::trim).filter(|c| !c.is_empty()) {
        req = req.query("cursor", c);
    }

    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(http_status_error(code)),
        Err(e) => {
            return Err(AppError::Io(format!(
                "Registry nicht erreichbar (offline?): {e}"
            )))
        }
    };

    let mut buf = String::new();
    resp.into_reader()
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut buf)
        .map_err(|e| AppError::Io(e.to_string()))?;
    let parsed: RegistrySearchResponse = serde_json::from_str(buf.trim())
        .map_err(|e| AppError::Parse(format!("ungültige Registry-Antwort: {e}")))?;

    let servers = parsed.servers.into_iter().map(to_view).collect();
    Ok(RegistrySearchPage {
        servers,
        next_cursor: parsed.metadata.next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> RegistrySearchResponse {
        serde_json::from_str(json).expect("parse")
    }

    #[test]
    fn maps_npm_package() {
        let resp = parse(
            r#"{"servers":[{"name":"io.example/fs","description":"d","version":"1.0.0",
              "packages":[{"registryType":"npm","identifier":"server-fs","version":"0.1.5",
                "environmentVariables":[
                  {"name":"TOKEN","isRequired":true,"isSecret":true},
                  {"name":"ROOT","isRequired":true}]}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        assert_eq!(view.variants.len(), 1);
        let v = &view.variants[0];
        assert_eq!(v.kind, "npm");
        assert_eq!(v.entry.command.as_deref(), Some("npx"));
        assert_eq!(
            v.entry.args.as_deref().unwrap(),
            &["-y".to_string(), "server-fs@0.1.5".to_string()]
        );
        // env-Keys vorhanden, Werte leer
        let env = v.entry.env.as_ref().unwrap();
        assert_eq!(env.get("TOKEN").map(String::as_str), Some(""));
        assert_eq!(env.get("ROOT").map(String::as_str), Some(""));
        // nur das Secret ist markiert
        assert_eq!(v.secret_keys, vec!["TOKEN".to_string()]);
    }

    #[test]
    fn maps_npm_with_runtime_hint_and_runtime_args() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"2.0.0",
                "runtimeHint":"node","runtimeArguments":[{"value":"--flag","type":"named"}]}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        let v = &view.variants[0];
        assert_eq!(v.entry.command.as_deref(), Some("node"));
        // eigene runtimeArguments statt automatischem -y
        assert_eq!(
            v.entry.args.as_deref().unwrap(),
            &["--flag".to_string(), "pkg@2.0.0".to_string()]
        );
    }

    #[test]
    fn named_argument_keeps_flag_and_value() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "runtimeArguments":[{"type":"named","name":"--directory","value":"/data"}]}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        let v = &view.variants[0];
        assert_eq!(
            v.entry.args.as_deref().unwrap(),
            &[
                "--directory".to_string(),
                "/data".to_string(),
                "pkg@1.0.0".to_string()
            ]
        );
    }

    #[test]
    fn maps_pypi_package() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"pypi","identifier":"mcp-server-fetch"}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        let v = &view.variants[0];
        assert_eq!(v.kind, "pypi");
        assert_eq!(v.entry.command.as_deref(), Some("uvx"));
        assert_eq!(v.entry.args.as_deref().unwrap(), &["mcp-server-fetch".to_string()]);
    }

    #[test]
    fn maps_oci_package() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"oci","identifier":"ghcr.io/x/y","version":"1.2.3"}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        let v = &view.variants[0];
        assert_eq!(v.kind, "oci");
        assert_eq!(v.entry.command.as_deref(), Some("docker"));
        assert_eq!(
            v.entry.args.as_deref().unwrap(),
            &[
                "run".to_string(),
                "--rm".to_string(),
                "-i".to_string(),
                "ghcr.io/x/y:1.2.3".to_string()
            ]
        );
    }

    #[test]
    fn maps_remote_streamable_http_and_sse() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "remotes":[
                {"type":"streamable-http","url":"https://api.example/mcp",
                  "headers":[{"name":"Authorization"}]},
                {"type":"sse","url":"https://api.example/sse"}]}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        assert_eq!(view.variants.len(), 2);

        let http = &view.variants[0];
        assert_eq!(http.kind, "http");
        assert_eq!(http.entry.transport.as_deref(), Some("http"));
        assert_eq!(http.entry.url.as_deref(), Some("https://api.example/mcp"));
        let headers = http.entry.headers.as_ref().unwrap();
        assert_eq!(headers.get("Authorization").map(String::as_str), Some(""));
        // Header-Keys gelten als secret
        assert_eq!(http.secret_keys, vec!["Authorization".to_string()]);

        let sse = &view.variants[1];
        assert_eq!(sse.kind, "sse");
        assert_eq!(sse.entry.transport.as_deref(), Some("sse"));
    }

    #[test]
    fn tolerant_parse_missing_optional_fields() {
        // Kein title/packages/remotes/environmentVariables – darf nicht scheitern.
        let resp = parse(r#"{"servers":[{"name":"only.name","description":"","version":""}]}"#);
        let view = to_view(resp.servers.into_iter().next().unwrap());
        // title fällt auf name zurück
        assert_eq!(view.title, "only.name");
        assert!(view.variants.is_empty());
        assert!(view.repository_url.is_none());
    }

    #[test]
    fn reads_next_cursor_from_metadata_camelcase() {
        let resp = parse(r#"{"servers":[],"metadata":{"nextCursor":"abc:1.0.1","count":0}}"#);
        assert_eq!(resp.metadata.next_cursor.as_deref(), Some("abc:1.0.1"));
    }

    #[test]
    fn repository_url_extracted() {
        let resp = parse(
            r#"{"servers":[{"name":"n","description":"d","version":"1",
              "repository":{"url":"https://github.com/x/y"}}]}"#,
        );
        let view = to_view(resp.servers.into_iter().next().unwrap());
        assert_eq!(view.repository_url.as_deref(), Some("https://github.com/x/y"));
    }

    #[test]
    #[ignore = "erfordert Netzwerkzugriff auf die Live-Registry"]
    fn live_search_smoke() {
        let page = fetch("filesystem", None).expect("fetch");
        assert!(!page.servers.is_empty(), "Registry sollte Server liefern");
    }
}
