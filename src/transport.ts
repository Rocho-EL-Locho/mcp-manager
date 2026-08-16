import type { ServerEntry } from "./ipc";

export type Transport = "stdio" | "http" | "sse";

/// Transport aus einem rohen ServerEntry ableiten (eine Quelle für Liste,
/// Detail, Formular und Presets).
///
/// **Muss synchron zu `commands.rs::transport_of` bleiben** – maßgeblich ist
/// das Backend. Läuft die Ableitung auseinander, kippt ein unbeteiligter Edit
/// im Formular einen sse-Server stillschweigend auf http.
export function transportOfEntry(e: ServerEntry): Transport {
  if (e.type === "stdio" || e.type === "http" || e.type === "sse") return e.type;
  // type fehlt: SSE-Endpunkte enden konventionell auf „/sse" – sonst http annehmen.
  // Bewusst case-SENSITIV und mit allen End-Slashes abgeschnitten, exakt wie
  // `trim_end_matches('/').ends_with("/sse")` im Backend.
  // Ein leerer bzw. nur aus Whitespace bestehender Wert zählt NICHT als URL –
  // exakt wie `.filter(|u| !u.trim().is_empty())` im Backend. Die /sse-Prüfung
  // läuft danach auf dem UNGETRIMMTEN Wert, ebenfalls wie dort.
  if (e.url && e.url.trim() !== "") {
    const u = e.url.replace(/\/+$/, "");
    return u.endsWith("/sse") ? "sse" : "http";
  }
  // Weder url noch command: das Backend fällt hier ebenfalls auf stdio zurück.
  return "stdio";
}
