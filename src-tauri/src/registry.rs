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
//!
//! Zwei Eigenheiten des Antwortformats, an denen der Katalog schon einmal leer
//! blieb:
//!
//! * Die Liste ist **zweistufig**: `{"servers":[{"server":{…},"_meta":{…}}]}` –
//!   der Server steckt unter `server`, nicht direkt im Array.
//! * Ohne `version=latest` liefert die API **jede je veröffentlichte Version**
//!   eines Servers als eigenen Eintrag.

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

/// Die Antwort der Live-API. `servers` ist Pflicht – fehlt das Feld, ist die
/// Antwort strukturell kaputt, und das soll auffallen statt eine leere Liste zu
/// ergeben.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySearchResponse {
    servers: Vec<RegistryListItem>,
    #[serde(default)]
    metadata: RegistryMetadata,
}

/// Ein Listenelement der Live-API: der eigentliche Server steckt eine Ebene
/// tiefer unter `server`, daneben liegt das registry-eigene `_meta`.
///
/// **Bewusst ohne `#[serde(default)]`**: Ein fehlendes `server` ist ein
/// Formatbruch und muss laut scheitern. Genau daran krankte der erste Versuch –
/// mit `default` wurde jedes Element zu einem leeren `RegistryServer`, die Liste
/// war voll und der Katalog trotzdem leer. Unbekannte **Zusatz**felder (`_meta`,
/// `$schema`, `websiteUrl`, …) ignoriert serde weiterhin stillschweigend, die
/// API darf also wachsen.
#[derive(Debug, Deserialize)]
struct RegistryListItem {
    server: RegistryServer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryMetadata {
    next_cursor: Option<String>,
}

/// `name` ist Pflicht: ein namenloser Eintrag ist nicht installierbar und wäre
/// im Katalog nur eine leere Zeile. Die übrigen Felder bleiben tolerant, weil
/// sie real fehlen (reine Remote-Server haben keine `packages` und umgekehrt).
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
    /// Jedes Package trägt einen eigenen Transport. Wir bauen nur für `stdio`
    /// eine Kommandozeile; fehlt das Feld, gilt stdio (Abwärtskompatibilität).
    transport: Option<RegistryTransport>,
    runtime_hint: Option<String>,
    runtime_arguments: Vec<RegistryArgument>,
    package_arguments: Vec<RegistryArgument>,
    environment_variables: Vec<RegistryEnvVar>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RegistryTransport {
    #[serde(rename = "type")]
    kind: String,
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

/// Startbefehle, die aus dem `runtimeHint` der öffentlichen Registry übernommen
/// werden dürfen. Alles andere ist Fremddaten-gesteuerte Befehlsausführung:
/// ein Katalogeintrag mit `"runtimeHint": "/bin/sh"` und
/// `packageArguments: ["-c", "curl … | sh"]` erzeugte sonst eine fertige
/// Shell-Konfiguration, optisch nicht von einem npm-Server unterscheidbar.
const ALLOWED_RUNTIME_HINTS: &[&str] = &[
    "npx", "node", "uvx", "uv", "python", "python3", "docker", "podman",
];

/// Übernimmt einen `runtimeHint` nur, wenn er auf der Allowlist steht (exakt,
/// ohne Pfadtrenner). Sonst `None` – der Aufrufer fällt auf den Default des
/// Pakettyps zurück (`npx`/`uvx`/`docker`).
fn sanitized_runtime_hint(hint: &Option<String>) -> Option<String> {
    let h = hint.as_deref()?.trim();
    if h.is_empty() || !ALLOWED_RUNTIME_HINTS.contains(&h) {
        return None;
    }
    Some(h.to_string())
}

/// Optionen, die den Interpreter dazu bringen, beliebigen Code auszuführen –
/// oder (docker/podman) das Host-Dateisystem zu öffnen.
///
/// Die Allowlist der `runtimeHint`s begrenzt nur den **Startbefehl**; die
/// Argumente kommen ungefiltert aus dem Katalog und stehen VOR der
/// Paketspezifikation. Ein Eintrag mit `runtimeHint: "node"` und
/// `runtimeArguments: ["-e", "<code>"]` erzeugte sonst `node -e '<code>' pkg@1.0.0`,
/// beschriftet als harmloses „npm · pkg". Analog `python3 -c …`, `uv run --with …`
/// oder `docker run -v /:/host …`.
const DENIED_ARG_OPTIONS: &[&str] = &[
    // node
    "-e", "--eval", "-p", "--print", "--require", "-r", "--import", "--node-options",
    // npx/npm
    "-c", "--call",
    // python
    "-m",
    // uv/uvx
    "--with", "--with-requirements", "--python",
    // docker/podman
    "-v", "--volume", "--mount", "--privileged", "--entrypoint", "-u", "--user",
];

/// Einzelbuchstaben, die in einer zusammengefassten Kurzoption (`-pe`, `-ie`)
/// dieselbe Wirkung hätten wie die entsprechende Einzeloption.
const DENIED_SHORT_FLAGS: &[char] = &['e', 'p', 'c', 'r', 'm', 'v', 'u'];

/// Ist ein einzelnes Argument-Token eine code-ausführende Option?
fn token_is_denied(token: &str) -> bool {
    let t = token.trim();
    if !t.starts_with('-') || t == "-" || t == "--" {
        return false;
    }
    // `--eval=code` bzw. `-e=code` auf den Optionsnamen reduzieren.
    let name = t.split('=').next().unwrap_or(t).to_ascii_lowercase();
    if DENIED_ARG_OPTIONS.contains(&name.as_str()) {
        return true;
    }
    // Zusammengefasste Kurzoptionen: `-pe` wirkt wie `-p -e`.
    if !name.starts_with("--") && name.chars().count() > 2 {
        return name[1..].chars().any(|c| DENIED_SHORT_FLAGS.contains(&c));
    }
    false
}

/// Verwirft eine Katalog-Variante, sobald eines ihrer Argumente auf der
/// Denylist steht. Bewusst „alles oder nichts": ein einzelnes Weglassen würde
/// den Rest der Kommandozeile sinnentstellt stehen lassen.
fn args_are_safe(tokens: &[String]) -> bool {
    !tokens.iter().any(|t| token_is_denied(t))
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
    // Ein Identifier, der wie eine Option aussieht, wäre Argument-Injection
    // (`npx -y -e@1.0.0` interpretiert npx als Flag).
    if id.is_empty() || id.starts_with('-') {
        return None;
    }
    // Nicht-stdio-Packages (`streamable-http`/`sse`) beschreiben einen Server,
    // den das Package erst startet; ihre URL ist ein Template mit Platzhaltern
    // (`http://{--host}:{--port}/mcp`). Als stdio-Kommandozeile gemappt ergäbe
    // das eine Konfiguration, die der Katalog nie beschrieben hat.
    if let Some(t) = &pkg.transport {
        let kind = t.kind.trim();
        if !kind.is_empty() && kind != "stdio" {
            return None;
        }
    }
    let ver = pkg.version.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let runtime_args = arg_tokens(&pkg.runtime_arguments);
    let pkg_args = arg_tokens(&pkg.package_arguments);
    // Argumente aus dem Katalog dürfen keinen Code ausführen – sonst wäre die
    // Allowlist der Startbefehle wirkungslos (siehe DENIED_ARG_OPTIONS).
    if !args_are_safe(&runtime_args) || !args_are_safe(&pkg_args) {
        return None;
    }

    let (kind, command, mut args) = match pkg.registry_type.as_str() {
        "npm" => {
            let cmd = sanitized_runtime_hint(&pkg.runtime_hint)
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
            let cmd = sanitized_runtime_hint(&pkg.runtime_hint)
                .unwrap_or_else(|| "uvx".into());
            let mut a = runtime_args;
            a.push(id.to_string());
            ("pypi", cmd, a)
        }
        "oci" => {
            let cmd = sanitized_runtime_hint(&pkg.runtime_hint)
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

/// Parst einen Antwort-Body der Registry in die Frontend-Sicht. Getrennt von
/// `fetch`, damit der Antwort-Aufbau ohne Netzwerk testbar ist.
fn parse_page(body: &str) -> Result<RegistrySearchPage, AppError> {
    let parsed: RegistrySearchResponse = serde_json::from_str(body.trim())
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

pub fn fetch(query: &str, cursor: Option<&str>) -> Result<RegistrySearchPage, AppError> {
    // Redirects erlaubt: öffentliche API ohne Secret-Header (anders als introspect.rs).
    let agent = ureq::AgentBuilder::new().timeout(REGISTRY_TIMEOUT).build();
    // `version=latest` ist Pflicht: ohne den Filter liefert die API JEDE je
    // veröffentlichte Version eines Servers als eigenen Eintrag (gemessen: 40
    // Einträge → 21 eindeutige Namen). Der Nutzer sähe denselben Server mehrfach,
    // teils in veralteten Fassungen. Serverseitig filtern ist besser als
    // clientseitig über `_meta…isLatest`, weil sonst ein Großteil des
    // Seitenlimits für später weggeworfene Duplikate draufginge.
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
    parse_page(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wickelt ein Server-Objekt in die Listen-Hülle der Live-API
    /// (`{"servers":[{"server":{…}}]}`) und liefert die gemappte Sicht – so
    /// laufen die Mapping-Tests über denselben Pfad wie `fetch`.
    fn view(server_json: &str) -> RegistryEntryView {
        let body = format!(r#"{{"servers":[{{"server":{server_json}}}]}}"#);
        parse_page(&body)
            .expect("Fixture muss parsen")
            .servers
            .into_iter()
            .next()
            .expect("genau ein Eintrag")
    }

    #[test]
    fn maps_npm_package() {
        let view = view(
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
        let view = view(
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

    /// Ein `runtimeHint` außerhalb der Allowlist darf NICHT zum Startbefehl
    /// werden – sonst liefert der Katalog eine fertige Shell-Konfiguration.
    #[test]
    fn hostile_runtime_hint_falls_back_to_default() {
        let view = view(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "runtimeHint":"/bin/sh",
                "packageArguments":[{"value":"--stdio"}]}]}"#,
        );
        let v = &view.variants[0];
        assert_eq!(
            v.entry.command.as_deref(),
            Some("npx"),
            "verworfener Hint muss auf den Typ-Default zurückfallen"
        );
    }

    /// Regression P3-9: Die Allowlist der Startbefehle allein genügt nicht –
    /// die erlaubten Hints sind allesamt Interpreter mit Inline-Code-Flags.
    /// Solche Argumente müssen die ganze Variante verwerfen.
    #[test]
    fn code_ausfuehrende_argumente_verwerfen_die_variante() {
        // node -e '<code>' pkg@1.0.0, etikettiert als „npm · pkg"
        let node_eval = view(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "runtimeHint":"node",
                "runtimeArguments":[{"value":"-e"},{"value":"require('child_process').exec('x')"}]}]}"#,
        );
        assert!(
            node_eval.variants.is_empty(),
            "node -e muss verworfen werden: {:?}",
            node_eval.variants
        );

        // python3 -c … über packageArguments
        let py = view(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"pypi","identifier":"pkg",
                "runtimeHint":"python3",
                "packageArguments":[{"value":"-c"},{"value":"import os"}]}]}"#,
        );
        assert!(py.variants.is_empty());

        // docker run -v /:/host …
        let docker = view(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"oci","identifier":"img","version":"1",
                "runtimeArguments":[{"type":"named","name":"-v","value":"/:/host"}]}]}"#,
        );
        assert!(docker.variants.is_empty());

        // Zusammengefasste Kurzoption `-pe` wirkt wie `-p -e`.
        assert!(token_is_denied("-pe"));
        assert!(token_is_denied("--eval=1+1"));
        assert!(token_is_denied("--WITH"));
        // Harmlose Optionen bleiben erlaubt.
        for ok in ["--directory", "/data", "-y", "--rm", "--stdio", "-", "--"] {
            assert!(!token_is_denied(ok), "„{ok}\" darf nicht abgelehnt werden");
        }
    }

    #[test]
    fn runtime_hint_allowlist() {
        for ok in ["npx", "node", "uvx", "uv", "python", "python3", "docker", "podman"] {
            assert_eq!(
                sanitized_runtime_hint(&Some(format!(" {ok} "))).as_deref(),
                Some(ok)
            );
        }
        for bad in ["/bin/sh", "sh", "bash", "./npx", "../npx", "npx -y", "", "NPX"] {
            assert!(
                sanitized_runtime_hint(&Some(bad.to_string())).is_none(),
                "„{bad}\" darf nicht durchgehen"
            );
        }
        assert!(sanitized_runtime_hint(&None).is_none());
    }

    #[test]
    fn named_argument_keeps_flag_and_value() {
        let view = view(
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
        let view = view(
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
        let view = view(
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
    fn maps_remote_streamable_http_and_sse() {
        let view = view(
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
        let view = view(r#"{"name":"only.name","description":"","version":""}"#);
        // title fällt auf name zurück
        assert_eq!(view.title, "only.name");
        assert!(view.variants.is_empty());
        assert!(view.repository_url.is_none());
    }

    #[test]
    fn reads_next_cursor_from_metadata_camelcase() {
        let page = parse_page(r#"{"servers":[],"metadata":{"nextCursor":"abc:1.0.1","count":0}}"#)
            .expect("parse");
        assert_eq!(page.next_cursor.as_deref(), Some("abc:1.0.1"));
    }

    #[test]
    fn repository_url_extracted() {
        let view = view(
            r#"{"name":"n","description":"d","version":"1",
              "repository":{"url":"https://github.com/x/y"}}"#,
        );
        assert_eq!(view.repository_url.as_deref(), Some("https://github.com/x/y"));
    }

    /// Ausschnitt einer echten Antwort von
    /// `GET /v0/servers?limit=2&version=latest` (gekürzte Beschreibungen).
    /// Entscheidend ist die **Wrapper-Ebene**: jedes Listenelement hat exakt die
    /// Schlüssel `server` und `_meta`, der eigentliche Server steckt unter
    /// `server`. Dazu Felder, die wir bewusst ignorieren (`$schema`,
    /// `websiteUrl`, `registryBaseUrl`, `transport`, `format`) – sie dürfen das
    /// Parsen nicht stören.
    const LIVE_RESPONSE_FIXTURE: &str = r#"{
      "servers": [
        {
          "server": {
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "ai.agenttrust/mcp-server",
            "description": "AgentTrust MCP server for agent identity",
            "title": "AgentTrust",
            "version": "1.1.1",
            "websiteUrl": "https://agenttrust.ai",
            "repository": {
              "url": "https://github.com/agenttrust/mcp-server",
              "source": "github",
              "subfolder": "packages/server"
            },
            "packages": [
              {
                "registryType": "npm",
                "registryBaseUrl": "https://registry.npmjs.org",
                "identifier": "@agenttrust/mcp-server",
                "version": "1.1.1",
                "transport": { "type": "stdio" },
                "environmentVariables": [
                  {
                    "description": "Your AgentTrust API key",
                    "isRequired": true,
                    "isSecret": true,
                    "format": "string",
                    "name": "AGENTTRUST_API_KEY"
                  }
                ]
              }
            ]
          },
          "_meta": {
            "io.modelcontextprotocol.registry/official": {
              "status": "active",
              "publishedAt": "2026-04-13T17:32:20.852269Z",
              "isLatest": true
            }
          }
        },
        {
          "server": {
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "ac.inference.sh/mcp",
            "description": "Run 150+ AI apps - image, video, audio, LLMs.",
            "title": "inference.sh",
            "version": "2.0.0",
            "remotes": [
              { "type": "streamable-http", "url": "https://api.inference.sh/mcp" }
            ]
          },
          "_meta": {
            "io.modelcontextprotocol.registry/official": {
              "status": "active",
              "isLatest": true
            }
          }
        }
      ],
      "metadata": { "nextCursor": "ac.inference.sh/mcp:2.0.0", "count": 2 }
    }"#;

    /// Regression: Die Listenelemente sind eine Ebene tiefer verschachtelt
    /// (`{"server": {…}, "_meta": {…}}`). Wurde das ignoriert, entstanden
    /// namenlose Einträge ohne Varianten – die Registry-Ansicht blieb leer.
    #[test]
    fn parst_verschachtelte_listenelemente_der_live_api() {
        let page = parse_page(LIVE_RESPONSE_FIXTURE).expect("Fixture muss parsen");
        assert_eq!(page.servers.len(), 2);

        let pkg = &page.servers[0];
        assert_eq!(pkg.name, "ai.agenttrust/mcp-server");
        assert_eq!(pkg.title, "AgentTrust");
        assert_eq!(pkg.description, "AgentTrust MCP server for agent identity");
        assert_eq!(pkg.version, "1.1.1");
        assert_eq!(
            pkg.repository_url.as_deref(),
            Some("https://github.com/agenttrust/mcp-server")
        );
        assert_eq!(pkg.variants.len(), 1, "npm-Package muss eine Variante ergeben");
        let v = &pkg.variants[0];
        assert_eq!(v.kind, "npm");
        assert_eq!(v.entry.command.as_deref(), Some("npx"));
        assert_eq!(
            v.entry.args.as_deref().unwrap(),
            &["-y".to_string(), "@agenttrust/mcp-server@1.1.1".to_string()]
        );
        assert_eq!(
            v.entry.env.as_ref().unwrap().get("AGENTTRUST_API_KEY").map(String::as_str),
            Some("")
        );
        assert_eq!(v.secret_keys, vec!["AGENTTRUST_API_KEY".to_string()]);

        let remote = &page.servers[1];
        assert_eq!(remote.name, "ac.inference.sh/mcp");
        assert_eq!(remote.variants.len(), 1, "Remote muss eine Variante ergeben");
        assert_eq!(remote.variants[0].kind, "http");
        assert_eq!(
            remote.variants[0].entry.url.as_deref(),
            Some("https://api.inference.sh/mcp")
        );

        assert_eq!(page.next_cursor.as_deref(), Some("ac.inference.sh/mcp:2.0.0"));
    }

    /// Ein Package trägt ein eigenes `transport`-Objekt. In der Live-Registry
    /// ist das fast immer `stdio`, es gibt aber Einträge mit
    /// `streamable-http`, deren URL ein Template mit Platzhaltern ist
    /// (`http://{--host}:{--port}/mcp`). Daraus eine Kommandozeile zu bauen
    /// ergäbe eine Konfiguration, die es so nie gab.
    #[test]
    fn package_mit_nicht_stdio_transport_wird_verworfen() {
        let http_pkg = view(
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "transport":{"type":"streamable-http","url":"http://{--host}:{--port}/mcp"}}]}"#,
        );
        assert!(
            http_pkg.variants.is_empty(),
            "Nicht-stdio-Package darf keine stdio-Variante ergeben: {:?}",
            http_pkg.variants
        );

        // Explizites stdio und ein fehlendes transport-Feld bleiben gültig.
        for json in [
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0",
                "transport":{"type":"stdio"}}]}"#,
            r#"{"name":"n","description":"d","version":"1",
              "packages":[{"registryType":"npm","identifier":"pkg","version":"1.0.0"}]}"#,
        ] {
            assert_eq!(view(json).variants.len(), 1, "stdio muss durchgehen: {json}");
        }
    }

    /// Ein Formatbruch darf nicht wieder still als „alles leer" durchgehen:
    /// fehlt die `server`-Hülle oder der Name, muss das Parsen scheitern.
    #[test]
    fn formatbruch_scheitert_laut() {
        // Server-Objekt direkt im Array (das alte, falsche Format).
        let flach = r#"{"servers":[{"name":"io.example/fs","description":"d","version":"1"}],
                        "metadata":{}}"#;
        assert!(
            parse_page(flach).is_err(),
            "flaches Format muss einen Fehler liefern statt leerer Einträge"
        );

        // Hülle vorhanden, aber ohne Namen.
        let ohne_name = r#"{"servers":[{"server":{"description":"d","version":"1"}}]}"#;
        assert!(parse_page(ohne_name).is_err(), "fehlender Name muss scheitern");

        // Leere Trefferliste bleibt gültig.
        let leer = parse_page(r#"{"servers":[],"metadata":{"count":0}}"#).expect("leer ist gültig");
        assert!(leer.servers.is_empty());
        assert!(leer.next_cursor.is_none());
    }

    #[test]
    #[ignore = "erfordert Netzwerkzugriff auf die Live-Registry"]
    fn live_search_smoke() {
        let page = fetch("filesystem", None).expect("fetch");
        assert!(!page.servers.is_empty(), "Registry sollte Server liefern");

        // Die alte Fassung prüfte nur die Länge – und ging deshalb durch, als
        // jeder Eintrag namenlos und variantenlos war. Der Inhalt muss stimmen.
        for s in &page.servers {
            assert!(!s.name.trim().is_empty(), "Eintrag ohne Namen: {s:?}");
            assert!(!s.title.trim().is_empty(), "Eintrag ohne Titel: {}", s.name);
        }
        assert!(
            page.servers.iter().any(|s| !s.variants.is_empty()),
            "mindestens ein Server muss eine installierbare Variante haben"
        );

        // `version=latest` filtert serverseitig: keine Namensdubletten je Seite.
        let mut namen: Vec<&str> = page.servers.iter().map(|s| s.name.as_str()).collect();
        namen.sort_unstable();
        let gesamt = namen.len();
        namen.dedup();
        assert_eq!(namen.len(), gesamt, "Seite enthält denselben Server mehrfach");
    }
}
