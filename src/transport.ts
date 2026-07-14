import type { ServerEntry } from "./ipc";

export type Transport = "stdio" | "http" | "sse";

/// Transport aus einem rohen ServerEntry ableiten (eine Quelle für Liste,
/// Detail und Presets).
export function transportOfEntry(e: ServerEntry): Transport | null {
  if (e.type === "stdio" || e.type === "http" || e.type === "sse") return e.type;
  // type fehlt: SSE-Endpunkte enden konventionell auf „/sse" – sonst http annehmen.
  if (e.url) return /\/sse\/?$/i.test(e.url) ? "sse" : "http";
  if (e.command) return "stdio";
  return null;
}
