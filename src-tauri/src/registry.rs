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

/// Die API liefert jeden Treffer als Hülle `{"server": {…}, "_meta": {…}}`.
/// `server` ist daher PFLICHT – fehlt es, ist die Antwort formatverletzend und
/// soll laut fehlschlagen statt still Leerhüllen zu erzeugen.
#[derive(Debug, Deserialize)]
struct RegistryListItem {
    server: RegistryServer,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySearchResponse {
    /// Pflichtfeld: ohne `servers` ist die Antwort kein Suchergebnis.
    servers: Vec<RegistryListItem>,
    #[serde(default)]
    metadata: RegistryMetadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryMetadata {
    next_cursor: Option<String>,
}

/// Kein `default` auf Struct-Ebene: `name` ist Pflicht, damit ein Formatbruch
/// auffällt statt namenlose Einträge zu erzeugen. Alle übrigen Felder fehlen in
/// der Praxis regelmäßig und bleiben daher tolerant.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryServer {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    repository: Option<RegistryRepository>,
    #[serde(default)]
    packages: Vec<RegistryPackage>,
    #[serde(default)]
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
    /// Manche Pakete starten den Server NICHT über stdio, sondern lauschen auf
    /// einem HTTP-Port (URL oft nur eine Vorlage wie `http://{--host}:{--port}/mcp`).
    /// Ohne dieses Feld würde daraus stumm eine unbrauchbare stdio-Zeile gebaut.
    transport: Option<RegistryTransport>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryTransport {
    #[serde(rename = "type")]
    kind: Option<String>,
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

/// Kommandos, die als `runtimeHint` übernommen werden dürfen. Ein Katalog-
/// eintrag darf sich kein beliebiges Startkommando aussuchen – ein
/// `"runtimeHint": "/bin/sh"` ergäbe sonst eine fertige Shell-Konfiguration,
/// die optisch nicht von einem npm-Server zu unterscheiden ist.
const ALLOWED_RUNTIMES: &[&str] = &[
    "npx", "node", "uvx", "uv", "python", "python3", "docker", "podman",
];

/// Übernimmt den `runtimeHint` nur, wenn er auf der Allowlist steht; sonst
/// gilt der Default des Paket-Typs.
fn resolve_command(hint: Option<&str>, default_cmd: &str) -> String {
    match hint.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) if ALLOWED_RUNTIMES.contains(&c) => c.to_string(),
        _ => default_cmd.to_string(),
    }
}

/// Optionen, mit denen sich über das jeweilige Kommando beliebiger Code
/// ausführen lässt: `(Langoptionen, Kurzflag-Buchstaben)`.
///
/// Die Allowlist aus [`ALLOWED_RUNTIMES`] allein genügt nicht – alle erlaubten
/// Kommandos sind Interpreter mit Inline-Code-Flag (`node -e '<code>'`).
fn denied_options(cmd: &str) -> (&'static [&'static str], &'static str) {
    match cmd {
        "node" => (
            &["eval", "print", "require", "import", "node-options"],
            "epr",
        ),
        "npx" | "npm" => (&["call"], "c"),
        "python" | "python3" => (&["command"], "cm"),
        "uv" | "uvx" => (
            &["with", "with-editable", "with-requirements", "python"],
            "p",
        ),
        "docker" | "podman" => (
            &["volume", "mount", "privileged", "entrypoint", "user"],
            "vu",
        ),
        _ => (&[], ""),
    }
}

/// Prüft Argument-Tokens, die das Startkommando selbst auswertet, gegen die
/// Denylist. Deckt die `--opt=wert`-Form und zusammengefasste bzw. mit dem Wert
/// verklebte Kurzflags (`-pe`, `-c<code>`) mit ab.
fn args_are_safe(cmd: &str, args: &[String]) -> bool {
    let (long, short) = denied_options(cmd);
    for token in args {
        if let Some(rest) = token.strip_prefix("--") {
            // Ein reines `--` trennt nur Optionen von Positionalen.
            if rest.is_empty() {
                continue;
            }
            let opt = rest.split('=').next().unwrap_or(rest);
            if long.contains(&opt) {
                return false;
            }
        } else if let Some(rest) = token.strip_prefix('-') {
            // Ein einzelnes `-` steht für stdin, ist also kein Flag.
            if rest.is_empty() {
                continue;
            }
            if rest.chars().any(|c| short.contains(c)) {
                return false;
            }
        }
    }
    true
}

fn package_variant(pkg: &RegistryPackage) -> Option<RegistryVariant> {
    let id = pkg.identifier.trim();
    // Ein Identifier mit führendem `-` würde vom Startkommando als Option
    // gelesen statt als Paketname.
    if id.is_empty() || id.starts_with('-') {
        return None;
    }
    // Pakete mit Nicht-stdio-Transport starten keinen stdio-Server; ihre URL ist
    // meist nur eine Vorlage (`http://{--host}:{--port}/mcp`). Aus ihnen darf
    // keine Kommandozeile entstehen. Fehlendes Feld heißt weiterhin stdio.
    if let Some(t) = pkg.transport.as_ref().and_then(|t| t.kind.as_deref()) {
        let t = t.trim();
        if !t.is_empty() && t != "stdio" {
            return None;
        }
    }

    let (kind, default_cmd) = match pkg.registry_type.as_str() {
        "npm" => ("npm", "npx"),
        "pypi" => ("pypi", "uvx"),
        "oci" => ("oci", "docker"),
        _ => return None,
    };
    let command = resolve_command(pkg.runtime_hint.as_deref(), default_cmd);

    let ver = pkg.version.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let runtime_args = arg_tokens(&pkg.runtime_arguments);
    // Die runtime-Argumente stehen VOR der Paketangabe und werden deshalb vom
    // Startkommando selbst ausgewertet – dort darf kein Inline-Code stehen.
    // Die package-Argumente folgen der Paketangabe und gehen an das gestartete
    // Programm, nicht an den Interpreter.
    if !args_are_safe(&command, &runtime_args) {
        return None;
    }
    let pkg_args = arg_tokens(&pkg.package_arguments);

    let mut args = match kind {
        "npm" => {
            // npx braucht -y für nicht-interaktiven Start, wenn die Registry
            // keine eigenen Runtime-Argumente vorgibt.
            let mut a = if runtime_args.is_empty() && command == "npx" {
                vec!["-y".to_string()]
            } else {
                runtime_args
            };
            a.push(match ver {
                Some(v) => format!("{id}@{v}"),
                None => id.to_string(),
            });
            a
        }
        "pypi" => {
            let mut a = runtime_args;
            a.push(id.to_string());
            a
        }
        // oci: der Container wird immer über `docker run --rm -i <image>`
        // gestartet, eigene Runtime-Argumente bleiben bewusst außen vor.
        _ => {
            let image = match ver {
                Some(v) => format!("{id}:{v}"),
                None => id.to_string(),
            };
            vec!["run".into(), "--rm".into(), "-i".into(), image]
        }
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
    // `version=latest` ist zwingend: ohne den Parameter liefert die API JEDE je
    // publizierte Version als eigenen Treffer (gemessen: 40 Treffer, 21
    // eindeutige Namen). Serverseitig filtern statt clientseitig – sonst
    // verschwendet man das Seitenlimit. Kompatibel mit `search` und `cursor`.
    let mut req = agent
        .get(BASE_URL)
        .query("limit", PAGE_LIMIT)
        .query("version", "latest");
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

    let servers = parsed
        .servers
        .into_iter()
        .map(|item| to_view(item.server))
        .collect();
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

    /// Hüllt ein einzelnes Server-Objekt in das reale Listenformat der API
    /// (`{"servers":[{"server":{…},"_meta":{…}}]}`) und liefert die View.
    fn view_of(server_json: &str) -> RegistryEntryView {
        let wrapped = format!(
            r#"{{"servers":[{{"server":{server_json},
               "_meta":{{"io.modelcontextprotocol.registry/official":{{"status":"active"}}}}}}]}}"#
        );
        let resp = parse(&wrapped);
        to_view(resp.servers.into_iter().next().expect("ein Eintrag").server)
    }

    #[test]
    fn unwraps_list_item_envelope() {
        // Die API verschachtelt jeden Treffer unter `server`; unbekannte
        // Zusatzfelder (`_meta`, `$schema`) müssen ignoriert werden.
        let resp = parse(
            r#"{"servers":[
                 {"_meta":{"io.modelcontextprotocol.registry/official":{"isLatest":true}},
                  "server":{"$schema":"https://example/schema.json",
                    "name":"io.example/fs","title":"Dateisystem","description":"Zugriff auf Dateien",
                    "version":"1.2.3",
                    "packages":[{"registryType":"npm","identifier":"server-fs","version":"1.2.3"}]}}],
               "metadata":{"nextCursor":"io.example/fs:1.2.3","count":1}}"#,
        );
        assert_eq!(resp.servers.len(), 1);
        assert_eq!(resp.metadata.next_cursor.as_deref(), Some("io.example/fs:1.2.3"));

        let view = to_view(resp.servers.into_iter().next().unwrap().server);
        assert_eq!(view.name, "io.example/fs");
        assert_eq!(view.title, "Dateisystem");
        assert_eq!(view.description, "Zugriff auf Dateien");
        assert_eq!(view.version, "1.2.3");
        assert_eq!(view.variants.len(), 1);
        assert_eq!(view.variants[0].entry.command.as_deref(), Some("npx"));
    }

    #[test]
    fn flat_format_without_envelope_is_rejected() {
        // Das frühere (falsch angenommene) Format darf NICHT still zu
        // Leerhüllen führen, sondern muss als Formatbruch auffallen.
        let flat = r#"{"servers":[{"name":"io.example/fs","description":"d","version":"1"}]}"#;
        assert!(serde_json::from_str::<RegistrySearchResponse>(flat).is_err());
    }

    #[test]
    fn missing_name_is_rejected() {
        let broken = r#"{"servers":[{"server":{"description":"d","version":"1"}}]}"#;
        assert!(serde_json::from_str::<RegistrySearchResponse>(broken).is_err());
    }

    #[test]
    fn maps_npm_package() {
        let view = view_of(
            r#"{"name":"io.example/fs","description":"d","version":"1.0.0",
              "packages":[{"registryType":"npm","identifier":"server-fs","version":"0.1.5",
                "environmentVariables":[
                  {"name":"TOKEN","isRequired":true,"isSecret":true},
                  {"name":"ROOT","isRequired":true}]}]}"#,
        );
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
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"2.0.0",
                "runtimeHint":"node","runtimeArguments":[{"value":"--flag","type":"named"}]}]}"#,
        );
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
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "runtimeArguments":[{"type":"named","name":"--directory","value":"/data"}]}]}"#,
        );
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
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"pypi","identifier":"mcp-server-fetch"}]}"#,
        );
        let v = &view.variants[0];
        assert_eq!(v.kind, "pypi");
        assert_eq!(v.entry.command.as_deref(), Some("uvx"));
        assert_eq!(v.entry.args.as_deref().unwrap(), &["mcp-server-fetch".to_string()]);
    }

    #[test]
    fn maps_oci_package() {
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"oci","identifier":"ghcr.io/x/y","version":"1.2.3"}]}"#,
        );
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
    fn package_with_non_stdio_transport_is_dropped() {
        // `http://{--host}:{--port}/mcp` ist eine Vorlage, kein stdio-Server –
        // daraus darf keine npx/docker-Zeile gebaut werden.
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"oci","identifier":"ghcr.io/x/y","version":"1",
                "transport":{"type":"streamable-http","url":"http://{--host}:{--port}/mcp"}}]}"#,
        );
        assert!(view.variants.is_empty());
    }

    #[test]
    fn package_with_explicit_stdio_transport_is_kept() {
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1",
                "transport":{"type":"stdio"}}]}"#,
        );
        assert_eq!(view.variants.len(), 1);
        assert_eq!(view.variants[0].entry.command.as_deref(), Some("npx"));
    }

    #[test]
    fn runtime_hint_outside_allowlist_falls_back_to_default() {
        // `/bin/sh` würde sonst eine fertige Shell-Konfiguration ergeben.
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1",
                "runtimeHint":"/bin/sh"}]}"#,
        );
        assert_eq!(view.variants[0].entry.command.as_deref(), Some("npx"));
        // Fallback auf npx ⇒ auch das nicht-interaktive -y wieder aktiv
        assert_eq!(
            view.variants[0].entry.args.as_deref().unwrap(),
            &["-y".to_string(), "pkg@1".to_string()]
        );
    }

    #[test]
    fn runtime_args_with_inline_code_drop_the_variant() {
        // `node -e '<code>' pkg@1.0.0`, beschriftet als „npm · pkg".
        for arg in [
            r#"{"type":"named","name":"-e","value":"require('child_process')"}"#,
            r#"{"type":"named","name":"--eval","value":"x"}"#,
            r#"{"type":"positional","value":"--eval=x"}"#,
            r#"{"type":"positional","value":"-pe"}"#,
            r#"{"type":"named","name":"--require","value":"./x.js"}"#,
        ] {
            let view = view_of(&format!(
                r#"{{"name":"n","description":"d","version":"1",
                  "packages":[{{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                    "runtimeHint":"node","runtimeArguments":[{arg}]}}]}}"#
            ));
            assert!(view.variants.is_empty(), "nicht verworfen: {arg}");
        }
    }

    #[test]
    fn docker_mount_options_drop_the_variant() {
        for hint_args in [
            (r#""docker""#, r#"[{"type":"positional","value":"-v"},{"type":"positional","value":"/:/host"}]"#),
            (r#""podman""#, r#"[{"type":"positional","value":"--privileged"}]"#),
            (r#""docker""#, r#"[{"type":"positional","value":"--mount=type=bind,src=/,dst=/host"}]"#),
        ] {
            let (hint, args) = hint_args;
            // pypi-Typ, damit die Runtime-Argumente nicht (wie bei oci) verworfen werden.
            let view = view_of(&format!(
                r#"{{"name":"n","description":"d","version":"1",
                  "packages":[{{"registryType":"pypi","identifier":"pkg",
                    "runtimeHint":{hint},"runtimeArguments":{args}}}]}}"#
            ));
            assert!(view.variants.is_empty(), "nicht verworfen: {args}");
        }
    }

    #[test]
    fn harmless_runtime_args_survive() {
        // -y (npx) und --directory dürfen nicht in die Denylist laufen.
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"pypi","identifier":"pkg","runtimeHint":"uvx",
                "runtimeArguments":[{"type":"named","name":"--directory","value":"/data"},
                                    {"type":"positional","value":"-q"}]}]}"#,
        );
        assert_eq!(
            view.variants[0].entry.args.as_deref().unwrap(),
            &[
                "--directory".to_string(),
                "/data".to_string(),
                "-q".to_string(),
                "pkg".to_string()
            ]
        );
    }

    #[test]
    fn identifier_with_leading_dash_is_dropped() {
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"--eval"}]}"#,
        );
        assert!(view.variants.is_empty());
    }

    #[test]
    fn maps_remote_streamable_http_and_sse() {
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "remotes":[
                {"type":"streamable-http","url":"https://api.example/mcp",
                  "headers":[{"name":"Authorization"}]},
                {"type":"sse","url":"https://api.example/sse"}]}"#,
        );
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
        let view = view_of(r#"{"name":"only.name","description":"","version":""}"#);
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
    fn missing_metadata_is_tolerated() {
        let resp = parse(r#"{"servers":[]}"#);
        assert!(resp.metadata.next_cursor.is_none());
    }

    #[test]
    fn repository_url_extracted() {
        let view = view_of(
            r#"{"name":"n","description":"d","version":"1",
              "repository":{"url":"https://github.com/x/y"}}"#,
        );
        assert_eq!(view.repository_url.as_deref(), Some("https://github.com/x/y"));
    }

    #[test]
    #[ignore = "erfordert Netzwerkzugriff auf die Live-Registry"]
    fn live_search_smoke() {
        let page = fetch("filesystem", None).expect("fetch");
        assert!(!page.servers.is_empty(), "Registry sollte Server liefern");
        // Eine volle Liste allein sagt nichts: bei falschem Antwort-Parsing sind
        // alle Einträge Leerhüllen. Also Inhalte prüfen.
        assert!(
            page.servers.iter().all(|s| !s.name.trim().is_empty()),
            "jeder Eintrag braucht einen Namen"
        );
        assert!(
            page.servers.iter().any(|s| !s.variants.is_empty()),
            "mindestens ein Eintrag braucht eine installierbare Variante"
        );
        // `version=latest` ⇒ keine Namensdubletten mehr.
        let mut names: Vec<&str> = page.servers.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(total, names.len(), "Namen sollten eindeutig sein");
    }
}
