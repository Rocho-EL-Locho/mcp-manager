//! Maskierung von Geheimnissen, BEVOR Daten das Backend Richtung Webview verlassen.
//!
//! Grundsatz: `claude mcp list/get` und die Config-Dateien enthalten Tokens im
//! Klartext (env-Werte, headers, inline in args wie `-e TOKEN=...`). Standardmäßig
//! wird alles maskiert; Klartext gibt es nur bei explizitem `reveal = true`.

use crate::models::{Introspection, PlaygroundResult, ServerEntry};

pub const MASK: &str = "••••••••";

/// Schlüssel-Namen, deren Wert als geheim gilt (case-insensitive, Teilstring).
const SECRET_KEY_HINTS: &[&str] = &[
    "TOKEN", "KEY", "SECRET", "PASSWORD", "PASSWD", "PASS", "AUTH", "CREDENTIAL", "COOKIE",
];

fn key_looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_HINTS.iter().any(|h| upper.contains(h))
}

/// Bekannte Token-Präfixe (case-sensitive geprüft, wie in freier Wildbahn).
const SECRET_TOKEN_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxp-", "xoxa-",
    "xoxr-", "glpat-", "AKIA",
];

/// Query-Parameter-Namen, deren Wert als geheim gilt (case-insensitive, Teilstring).
const SECRET_QUERY_HINTS: &[&str] = &[
    "token", "key", "secret", "apikey", "api_key", "password", "passwd", "auth", "access_token",
    "credential", "sig", "signature",
];

/// Sieht ein einzelner Wert wie ein Geheimnis aus
/// (JWT/Bearer/Basic/bekannte Präfixe/langer opaker String)?
fn value_looks_secret(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    if v.starts_with("eyJ") {
        return true; // JWT
    }
    let lower = v.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return true;
    }
    // Bekannte Token-Präfixe.
    if SECRET_TOKEN_PREFIXES.iter().any(|p| v.starts_with(p)) {
        return true;
    }
    // Lange opake Strings ohne Whitespace, überwiegend base64/hex-artig.
    if v.len() >= 24 && !v.chars().any(|c| c.is_whitespace()) && looks_opaque(v) {
        return true;
    }
    false
}

/// Heuristik für ein bare Token (base64url/hex-artig). Bewusst KONSERVATIV, um
/// legitime Werte nicht zu übermaskieren: erlaubt sind nur Alnum, `-` und `_`
/// (also KEIN `/`, `.`, `:` …). Damit fallen Dateipfade (`/home/…`), Domains
/// (`.`) und URLs heraus. Zusätzlich wird sowohl mindestens eine Ziffer ALS AUCH
/// mindestens ein Buchstabe verlangt, damit reine Wörter/Pfad-Segmente und reine
/// Zahlen nicht greifen. Strukturierte Secrets (JWT, Bearer/Basic, bekannte
/// Präfixe, KEY=VALUE) werden ohnehin separat erkannt.
fn looks_opaque(v: &str) -> bool {
    let mut has_digit = false;
    let mut has_alpha = false;
    for c in v.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            if c.is_ascii_digit() {
                has_digit = true;
            } else if c.is_ascii_alphabetic() {
                has_alpha = true;
            }
        } else {
            return false; // Fremdzeichen (/, ., :, …) -> kein bare Token
        }
    }
    has_digit && has_alpha
}

/// Sieht ein Token wie eine URL mit Query-Anteil aus?
fn looks_like_url_with_query(token: &str) -> bool {
    let scheme = token.starts_with("http://")
        || token.starts_with("https://")
        || token.contains("://");
    scheme && token.contains('?')
}

/// Maskiert in einer URL die Werte geheim wirkender Query-Parameter.
/// Gibt (maskierte_url, wurde_etwas_maskiert) zurück. Der Basis-URL-Teil bleibt.
fn mask_url_query(url: &str) -> (String, bool) {
    let Some(qpos) = url.find('?') else {
        return (url.to_string(), false);
    };
    let (base, query) = url.split_at(qpos);
    let query = &query[1..]; // '?' überspringen
    let mut masked_any = false;
    let mut out_pairs: Vec<String> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            out_pairs.push(pair.to_string());
            continue;
        }
        if let Some(eq) = pair.find('=') {
            let (k, v) = pair.split_at(eq);
            let v = &v[1..];
            let key_lower = k.to_ascii_lowercase();
            let key_secret = SECRET_QUERY_HINTS.iter().any(|h| key_lower.contains(h));
            if !v.is_empty() && (key_secret || value_looks_secret(v)) {
                out_pairs.push(format!("{k}={MASK}"));
                masked_any = true;
                continue;
            }
        }
        out_pairs.push(pair.to_string());
    }
    if masked_any {
        (format!("{base}?{}", out_pairs.join("&")), true)
    } else {
        (url.to_string(), false)
    }
}

/// Maskiert das Passwort in der URI-Userinfo (`schema://user:passwort@host/…`).
///
/// Schließt die Lücke, die weder `looks_like_url_with_query` (verlangt ein `?`)
/// noch `looks_opaque` (bricht bei `:` und `/` ab) abdeckt. Kein Randfall: das
/// mitgelieferte `postgres`-Preset erzeugt genau dieses Muster
/// (`postgresql://user:geheim@host/db`), und Server geben ihre Connection-URL
/// gern nach stderr aus. Gibt (maskiert, wurde_maskiert) zurück; Schema, Nutzer
/// und Host bleiben lesbar, damit die Zeile diagnostisch brauchbar bleibt.
fn mask_uri_userinfo(token: &str) -> (String, bool) {
    let Some(sep) = token.find("://") else {
        return (token.to_string(), false);
    };
    let auth_start = sep + 3;
    // Die Authority endet beim ersten Pfad-/Query-/Fragment-Trenner.
    let auth_end = token[auth_start..]
        .find(['/', '?', '#'])
        .map(|i| auth_start + i)
        .unwrap_or(token.len());
    let authority = &token[auth_start..auth_end];
    // Userinfo ist alles vor dem LETZTEN `@` der Authority.
    let Some(at) = authority.rfind('@') else {
        return (token.to_string(), false);
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return (token.to_string(), false); // nur Nutzername, kein Passwort
    };
    if userinfo[colon + 1..].is_empty() {
        return (token.to_string(), false); // leeres Passwort -> nichts zu maskieren
    }
    let masked = format!(
        "{}{}:{MASK}{}",
        &token[..auth_start],
        &userinfo[..colon],
        &token[auth_start + at..]
    );
    (masked, true)
}

/// Maskiert beide Secret-Stellen einer URL: Userinfo-Passwort und geheime
/// Query-Parameter. Gemeinsamer Eingang für args, `entry.url` und Freitext.
fn mask_url_secrets(token: &str) -> (String, bool) {
    let (mut out, mut changed) = mask_uri_userinfo(token);
    if looks_like_url_with_query(&out) {
        let (q, q_changed) = mask_url_query(&out);
        if q_changed {
            out = q;
            changed = true;
        }
    }
    (out, changed)
}

/// Maskiert den Wertteil eines `KEY=VALUE`-Arguments, wenn der Schlüssel geheim wirkt.
/// Gibt (maskiertes_arg, war_geheim) zurück.
fn mask_kv_arg(arg: &str) -> (String, bool) {
    // Ganzes Argument ist eine URL mit Credentials/geheimem Query-Anteil?
    let (masked, changed) = mask_url_secrets(arg);
    if changed {
        return (masked, true);
    }
    if let Some(eq) = arg.find('=') {
        let (key, val) = arg.split_at(eq);
        let val = &val[1..];
        if !val.is_empty() {
            if key_looks_secret(key) || value_looks_secret(val) {
                return (format!("{key}={MASK}"), true);
            }
            // Der WERT ist eine URL mit Credentials/Query-Secret, der Schlüssel
            // aber unauffällig (`DATABASE_URL=postgresql://user:geheim@host/db`).
            let (masked_val, val_changed) = mask_url_secrets(val);
            if val_changed {
                return (format!("{key}={masked_val}"), true);
            }
        }
    }
    if value_looks_secret(arg) {
        return (MASK.to_string(), true);
    }
    (arg.to_string(), false)
}

/// Länge, ab der ein bekannter Klartext-Wert literal aus Logs entfernt wird.
/// Kurze Werte (`1`, `true`, `info`, `debug`) würden sonst überall im Text
/// getroffen und die Diagnose unlesbar machen.
const LITERAL_MIN_LEN: usize = 8;

/// Sammelt die Klartext-Werte aus `env` und `headers` einer Definition.
///
/// Diese Werte kennt das Backend beim Starten des Prozesses ohnehin – sie
/// zusätzlich LITERAL aus stderr/Logs zu entfernen macht die Redaktion
/// unabhängig davon, ob die Heuristik den Wert als Secret erkennt.
pub fn secret_literals(entry: &ServerEntry) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for map in [entry.env.as_ref(), entry.headers.as_ref()]
        .into_iter()
        .flatten()
    {
        for v in map.values() {
            let t = v.trim();
            if t.len() >= LITERAL_MIN_LEN {
                out.push(t.to_string());
            }
        }
    }
    // Längste zuerst: sonst zerschneidet der Treffer eines kurzen Werts einen
    // längeren, der ihn enthält.
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    out.dedup();
    out
}

/// Ersetzt bekannte Klartext-Werte (aus [`secret_literals`]) literal durch `MASK`.
pub fn redact_literals(text: &str, literals: &[String]) -> String {
    let mut out = text.to_string();
    for lit in literals {
        if out.contains(lit.as_str()) {
            out = out.replace(lit.as_str(), MASK);
        }
    }
    out
}

/// Enthält die Definition Geheimnisse (env/headers/verdächtige args/URL)?
pub fn entry_has_secrets(entry: &ServerEntry) -> bool {
    if entry.env.as_ref().is_some_and(|m| !m.is_empty()) {
        return true;
    }
    if entry.headers.as_ref().is_some_and(|m| !m.is_empty()) {
        return true;
    }
    if let Some(args) = &entry.args {
        if args.iter().any(|a| mask_kv_arg(a).1) {
            return true;
        }
    }
    // Remote-Endpunkte tragen Tokens gern in der URL selbst (Query-Parameter
    // oder Userinfo) – sonst fehlte für sie das Secret-Badge.
    if entry.url.as_deref().is_some_and(|u| mask_url_secrets(u).1) {
        return true;
    }
    false
}

/// Liefert eine (ggf.) maskierte Kopie der Definition.
pub fn mask_entry(entry: &ServerEntry, reveal: bool) -> ServerEntry {
    if reveal {
        return entry.clone();
    }
    let mut out = entry.clone();
    if let Some(env) = out.env.as_mut() {
        for v in env.values_mut() {
            *v = MASK.to_string();
        }
    }
    if let Some(headers) = out.headers.as_mut() {
        for v in headers.values_mut() {
            *v = MASK.to_string();
        }
    }
    if let Some(args) = out.args.as_mut() {
        for a in args.iter_mut() {
            *a = mask_kv_arg(a).0;
        }
    }
    // Auch die URL selbst kann Secrets tragen (`?api_key=…`, `user:pw@host`).
    // Ohne diesen Schritt stünde derselbe Wert, den die Listenzeile über
    // `mask_summary` maskiert, in Detail-Ansicht und Formular im Klartext.
    if let Some(url) = out.url.as_deref().map(|u| mask_url_secrets(u).0) {
        out.url = Some(url);
    }
    out
}

/// Maskiert eine freie Zusammenfassungszeile (z. B. aus `claude mcp list`).
pub fn mask_summary(summary: &str, reveal: bool) -> String {
    if reveal {
        return summary.to_string();
    }
    summary
        .split_whitespace()
        .map(|tok| mask_kv_arg(tok).0)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ersetzt in beliebigem Freitext geheim aussehende Fragmente durch `MASK`.
///
/// Erfasst zwei Klassen: `KEY=VALUE`-Vorkommen mit geheimem Key (bzw. geheimem
/// Wert) sowie einzelne Tokens, die `value_looks_secret` erfüllen
/// (Bearer/Basic/JWT/bekannte Präfixe/lange opake Strings). Robust und ohne
/// Panik – arbeitet whitespace-tokenweise und lässt Nicht-Secrets unangetastet.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // Über Whitespace-Grenzen iterieren, dabei die originalen Trenner erhalten.
    while !rest.is_empty() {
        // Führenden Whitespace unverändert übernehmen.
        let ws_end = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        if ws_end > 0 {
            out.push_str(&rest[..ws_end]);
            rest = &rest[ws_end..];
            if rest.is_empty() {
                break;
            }
        }
        // Nächstes Token bis zum nächsten Whitespace.
        let tok_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..tok_end];
        out.push_str(&redact_token(token));
        rest = &rest[tok_end..];
    }
    // Zweiter Durchlauf: eingebettete Signatur-Tokens (JWT, bekannte Präfixe)
    // auch OHNE Whitespace-Trennung maskieren – z. B. in kompaktem JSON wie
    // {"env":{"TOKEN":"ghp_…"}}, das eine fehlschlagende CLI zurückgeben könnte.
    redact_embedded_signatures(&out)
}

/// Zeichen, die zu einem Token-Kern gehören (ASCII, secret-typisch).
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

/// Maskiert eingebettete Secrets, die an einem JWT- (`eyJ`) oder bekannten
/// Präfix (ghp_, sk-, …) beginnen – unabhängig von umgebenden Trennzeichen.
/// Erfasst so auch in dichten Strings (kompaktes JSON) verborgene Tokens.
fn redact_embedded_signatures(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut idx = 0usize;
    while idx < text.len() {
        let rest = &text[idx..];
        let hit = rest.starts_with("eyJ")
            || SECRET_TOKEN_PREFIXES.iter().any(|p| rest.starts_with(p));
        if hit {
            // Token-Kern ab hier bis zum ersten Nicht-Token-Zeichen konsumieren.
            let mut end = idx;
            for (off, c) in rest.char_indices() {
                if is_token_char(c) {
                    end = idx + off + c.len_utf8();
                } else {
                    break;
                }
            }
            if end - idx >= 8 {
                out.push_str(MASK);
                idx = end;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        idx += c.len_utf8();
    }
    out
}

/// Redigiert ein einzelnes (whitespace-freies) Token; behält umgebende
/// Satzzeichen/Klammern bei und maskiert nur den geheimen Kern.
fn redact_token(token: &str) -> String {
    // Umgebende „Rand"-Zeichen (Anführungszeichen, Klammern, Satzzeichen) abschälen.
    let trim_chars: &[char] = &['"', '\'', '`', '(', ')', '[', ']', '{', '}', ',', ';', '<', '>'];
    let stripped_front = token.trim_start_matches(|c| trim_chars.contains(&c));
    let core = stripped_front.trim_end_matches(|c| trim_chars.contains(&c));
    if core.is_empty() {
        return token.to_string();
    }
    // Byte-Offsets des Kerns im Original bestimmen (Ränder bleiben erhalten).
    let lead_len = token.len() - stripped_front.len();
    let lead = &token[..lead_len];
    let trail = &token[lead_len + core.len()..];

    // Der (ggf. schon bestehende) mask_kv_arg-Pfad deckt KEY=VALUE, URL-Query
    // und einzelne Secret-Tokens ab.
    let (masked, changed) = mask_kv_arg(core);
    if changed {
        format!("{lead}{masked}{trail}")
    } else {
        token.to_string()
    }
}

/// Redigiert rekursiv jeden String-Blattwert eines JSON-Werts über
/// `redact_secrets`. Für Tool-Schemata/Beschreibungen aus der Introspektion,
/// damit versehentlich eingebettete Secrets nicht ins UI gelangen. Struktur und
/// Objekt-Schlüssel bleiben unangetastet.
pub fn redact_json(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(redact_secrets(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(redact_json).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), redact_json(v))).collect())
        }
        other => other.clone(),
    }
}

/// Kurzbeschreibung für die Listenzeile aus einer Definition ableiten.
pub fn summarize_entry(entry: &ServerEntry) -> String {
    if let Some(url) = &entry.url {
        return url.clone();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(cmd) = &entry.command {
        parts.push(cmd.clone());
    }
    if let Some(args) = &entry.args {
        parts.extend(args.iter().cloned());
    }
    parts.join(" ")
}

/// Ersetzt große Binärinhalte (base64) durch eine kurze Zusammenfassung, damit
/// das UI nicht mit Megabyte-Blobs geflutet wird und `redact_json` nicht sinnlos
/// über riesige Datenstrings läuft. Betrifft MCP-`content`-Items vom Typ
/// `image`/`audio` (Feld `data`) sowie Resource-`blob`-Felder.
pub fn summarize_blobs(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    /// Ersetzt `map[key]` durch die Größenangabe, falls dort ein String steht.
    fn summarize(map: &mut serde_json::Map<String, Value>, key: &str) {
        if let Some(Value::String(s)) = map.get(key) {
            let kb = (s.len() as f64 / 1024.0).round() as u64;
            let summary = format!("<Binärdaten, ~{kb} KB (nicht angezeigt)>");
            map.insert(key.to_string(), Value::String(summary));
        }
    }

    match value {
        Value::Object(mut map) => {
            // `data` nur bei image/audio – bei anderen content-Typen ist es Nutztext.
            let is_binary = matches!(
                map.get("type").and_then(|t| t.as_str()),
                Some("image") | Some("audio")
            );
            if is_binary {
                summarize(&mut map, "data");
            }
            // `blob` (resources/read) ist per Definition binär.
            summarize(&mut map, "blob");
            Value::Object(map.into_iter().map(|(k, v)| (k, summarize_blobs(v))).collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(summarize_blobs).collect()),
        other => other,
    }
}

/// Redigiert ein Playground-Ergebnis vor Verlassen des Backends: Ergebnis-JSON
/// (String-Blätter) via `redact_json`, große Blob-/Bild-Inhalte werden
/// zusammengefasst; Fehler/Logs/Notizen via `redact_secrets`.
pub fn mask_playground(r: &mut PlaygroundResult) {
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

/// Maskiert geheim aussehende Werte in Tool-Schemata, Beschreibungen und Notizen,
/// bevor das Introspektions-Ergebnis das Backend verlässt.
pub fn mask_introspection(intro: &mut Introspection) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pfade_bleiben_sichtbar() {
        // Regression #3: normale Pfade/Wörter dürfen NICHT maskiert werden.
        for s in [
            "/home/user/projects/mcp-manager",
            "/usr/bin/python3",
            "@modelcontextprotocol/server-filesystem",
            "mcp/grafana:latest",
        ] {
            assert_eq!(mask_kv_arg(s).0, s, "sollte unverändert bleiben: {s}");
            assert!(!value_looks_secret(s), "kein Secret: {s}");
        }
    }

    #[test]
    fn echte_secrets_werden_erkannt() {
        assert!(value_looks_secret("ghp_1234567890abcdefGHIJ"));
        assert!(value_looks_secret("EXAMPLEexample1234567890abcdef"));
        assert!(value_looks_secret("eyJhbGciOiJIUzI1NiJ9.abc.def"));
        assert_eq!(mask_kv_arg("API_TOKEN=EXAMPLEexample1234").0, "API_TOKEN=••••••••");
    }

    #[test]
    fn redact_secrets_erfasst_kompaktes_json() {
        // Regression #4: Token in whitespace-freiem JSON muss redigiert werden.
        let leaked = "error: invalid config {\"env\":{\"TOKEN\":\"ghp_ABC123def456ghi789\"}}";
        let red = redact_secrets(leaked);
        assert!(!red.contains("ghp_ABC123def456ghi789"), "Token darf nicht durchrutschen: {red}");
        assert!(red.contains(MASK));
        // Freitext mit Pfad bleibt lesbar.
        let plain = "spawn failed for /home/user/mcp-servers/example-mcp";
        assert_eq!(redact_secrets(plain), plain);
    }

    #[test]
    fn redact_json_maskiert_verschachtelte_secrets() {
        use serde_json::json;
        let schema = json!({
            "type": "object",
            "properties": {
                "api_key": { "type": "string", "default": "ghp_ABC123def456ghi789" },
                "path": { "type": "string", "default": "/home/user/data" }
            },
            "examples": ["eyJhbGciOiJIUzI1NiJ9.abc.def"]
        });
        let red = redact_json(&schema);
        let s = red.to_string();
        assert!(!s.contains("ghp_ABC123def456ghi789"), "Token darf nicht durchrutschen: {s}");
        assert!(!s.contains("eyJhbGciOiJIUzI1NiJ9"), "JWT darf nicht durchrutschen: {s}");
        assert!(s.contains(MASK));
        // Strukturelle Schlüssel und harmlose Werte bleiben erhalten.
        assert!(s.contains("properties"));
        assert!(s.contains("/home/user/data"));
        assert!(s.contains("object"));
    }

    /// Regression P3-8: Credentials in der URI-Userinfo (`user:passwort@host`)
    /// wurden von keinem Zweig erfasst – das `postgres`-Preset erzeugt genau
    /// dieses Muster.
    #[test]
    fn userinfo_passwort_wird_maskiert() {
        let url = "postgresql://appuser:s3hrGeheim@db.example:5432/kunden";
        let (masked, changed) = mask_kv_arg(url);
        assert!(changed, "Userinfo muss als geheim gelten");
        assert!(!masked.contains("s3hrGeheim"), "Passwort durchgerutscht: {masked}");
        assert_eq!(masked, format!("postgresql://appuser:{MASK}@db.example:5432/kunden"));

        // Auch als Wertteil eines unauffälligen KEY=VALUE-Arguments.
        let kv = "DATABASE_URL=mysql://root:hunter2xyz@localhost/db";
        let (m2, c2) = mask_kv_arg(kv);
        assert!(c2);
        assert!(!m2.contains("hunter2xyz"), "Passwort durchgerutscht: {m2}");

        // Und in freiem stderr-Text (Server echot seine Connection-URL).
        let log = "connecting to postgresql://appuser:s3hrGeheim@db.example/kunden …";
        let red = redact_secrets(log);
        assert!(!red.contains("s3hrGeheim"), "Passwort durchgerutscht: {red}");

        // Ohne Passwort bzw. ohne Userinfo bleibt alles unverändert.
        for harmlos in [
            "postgresql://appuser@db.example/kunden",
            "https://api.example.com:443/mcp",
            "mcp/grafana:latest",
        ] {
            assert_eq!(mask_kv_arg(harmlos).0, harmlos, "unverändert erwartet: {harmlos}");
        }
    }

    /// Regression P3-7: `entry.url` lief unmaskiert ins Webview und zählte auch
    /// nicht als Secret (kein Badge).
    #[test]
    fn entry_url_wird_maskiert_und_zaehlt_als_secret() {
        let entry = ServerEntry {
            transport: Some("http".into()),
            url: Some("https://api.example/mcp?api_key=EXAMPLEexample1234567890".into()),
            ..Default::default()
        };
        assert!(entry_has_secrets(&entry), "Secret-Badge fehlt");
        let masked = mask_entry(&entry, false);
        let url = masked.url.as_deref().unwrap();
        assert!(!url.contains("EXAMPLEexample1234567890"), "Token durchgerutscht: {url}");
        assert!(url.starts_with("https://api.example/mcp?api_key="));
        // Mit reveal bleibt der Klartext erhalten.
        assert_eq!(mask_entry(&entry, true).url, entry.url);

        // Userinfo in der URL ebenso.
        let creds = ServerEntry {
            url: Some("https://nutzer:geheimwort@remote.example/mcp".into()),
            ..Default::default()
        };
        assert!(entry_has_secrets(&creds));
        let m = mask_entry(&creds, false);
        assert!(!m.url.as_deref().unwrap().contains("geheimwort"));
    }

    /// Literale Redaktion: die echten env-Werte kennt das Backend beim Start –
    /// sie fliegen unabhängig von der Heuristik aus stderr/Logs.
    #[test]
    fn literale_env_werte_fliegen_aus_logs() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("PGPASSWORD".to_string(), "korrekt-pferd-batterie".to_string());
        env.insert("LOG_LEVEL".to_string(), "debug".to_string()); // zu kurz -> bleibt
        let entry = ServerEntry { env: Some(env), ..Default::default() };
        let lits = secret_literals(&entry);
        assert_eq!(lits, vec!["korrekt-pferd-batterie".to_string()]);

        let text = "psql: FATAL: password 'korrekt-pferd-batterie' rejected (debug)";
        let red = redact_literals(text, &lits);
        assert!(!red.contains("korrekt-pferd-batterie"), "Wert durchgerutscht: {red}");
        assert!(red.contains("debug"), "kurze Werte dürfen nicht maskiert werden: {red}");
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
}
