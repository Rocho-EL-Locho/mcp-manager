import type { ServerEntry } from "./ipc";

export type Transport = "stdio" | "http" | "sse";

/// Transport aus einem rohen ServerEntry ableiten (eine Quelle für Liste,
/// Detail, Formular und Presets).
///
/// **Spiegelt `commands.rs::transport_of` und muss exakt übereinstimmen** –
/// maßgeblich ist das Backend. Weicht die Ableitung ab, widersprechen Badge und
/// Filter im Frontend dem Pfad, den das Backend tatsächlich wählt.
/// Bewusst identisch gehalten: exakte (case-sensitive) Schreibweise von `type`,
/// case-sensitive `/sse`-Erkennung, beliebig viele End-Slashes, leere bzw.
/// blanke `url` zählt nicht als URL, und ohne alles gilt stdio.
export function transportOfEntry(e: ServerEntry): Transport {
  if (e.type === "stdio" || e.type === "http" || e.type === "sse") return e.type;
  // type fehlt: SSE-Endpunkte enden konventionell auf „/sse" – sonst http annehmen.
  const url = e.url?.trim() ?? "";
  if (url !== "") return url.replace(/\/+$/, "").endsWith("/sse") ? "sse" : "http";
  return "stdio";
}
