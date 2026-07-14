// Statischer, typisierter Katalog gängiger MCP-Server.
// Bewusst rein im Frontend: keine Netzabfrage, kein Backend-Command nötig.
//
// Leitplanke: Presets enthalten NIEMALS echte Secret-Werte — nur Schlüssel mit
// leerem Wert (siehe `secretKeys`). Platzhalter in Args stehen in spitzen
// Klammern (<PFAD>, <CONNECTION_STRING>) und blockieren das Speichern, bis sie
// ersetzt sind (Auswertung im Formular).
import type { ServerEntry } from "./ipc";
import { transportOfEntry } from "./transport";

export interface ServerPreset {
  /** Stabile ID, zugleich Vorschlag für den Servernamen ("github", "filesystem", …). */
  id: string;
  /** Anzeigename. */
  label: string;
  /** Eine kurze Beschreibung (deutsch). */
  description: string;
  /** Offizielle Doku/Repo-URL — bleibt prominent, weil Presets veralten können. */
  docsUrl: string;
  /** Vorlage; env-/header-Werte für Secrets sind LEER (""). */
  entry: ServerEntry;
  /** env-/header-Keys, die der Nutzer füllen muss (werden maskiert gespeichert). */
  secretKeys?: string[];
}

/// Transport eines Presets ableiten (für Badges im Auswahl-Schritt).
export function presetTransport(p: ServerPreset): "stdio" | "http" | "sse" {
  return transportOfEntry(p.entry) ?? "stdio";
}

/// Tiefe Kopie der Vorlage, damit das Formular den Katalog nie mutiert.
export function presetEntry(p: ServerPreset): ServerEntry {
  const e = p.entry;
  return {
    ...(e.type != null ? { type: e.type } : {}),
    ...(e.command != null ? { command: e.command } : {}),
    ...(e.args ? { args: [...e.args] } : {}),
    ...(e.env ? { env: { ...e.env } } : {}),
    ...(e.url != null ? { url: e.url } : {}),
    ...(e.headers ? { headers: { ...e.headers } } : {}),
  };
}

export const PRESETS: ServerPreset[] = [
  {
    id: "filesystem",
    label: "Filesystem",
    description: "Lese-/Schreibzugriff auf ein oder mehrere lokale Verzeichnisse.",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem",
    entry: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-filesystem", "<PFAD>"],
    },
  },
  {
    id: "github",
    label: "GitHub",
    description:
      "Repos, Issues und PRs über den gehosteten GitHub-MCP-Server. Authorization-Header: „Bearer <PAT>“ (oder leer lassen für OAuth).",
    docsUrl: "https://github.com/github/github-mcp-server",
    entry: {
      type: "http",
      url: "https://api.githubcopilot.com/mcp/",
      headers: { Authorization: "" },
    },
  },
  {
    id: "memory",
    label: "Memory",
    description: "Persistenter Wissensgraph als Langzeitgedächtnis für das Modell.",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
    entry: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-memory"],
    },
  },
  {
    id: "fetch",
    label: "Fetch",
    description: "Ruft Webseiten ab und wandelt sie für das Modell in Markdown um.",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
    entry: {
      command: "uvx",
      args: ["mcp-server-fetch"],
    },
  },
  {
    id: "playwright",
    label: "Playwright",
    description: "Browser-Automatisierung: Seiten öffnen, klicken, ausfüllen, auslesen.",
    docsUrl: "https://github.com/microsoft/playwright-mcp",
    entry: {
      command: "npx",
      args: ["-y", "@playwright/mcp@latest"],
    },
  },
  {
    id: "postgres",
    label: "PostgreSQL",
    description: "Nur-Lese-Zugriff auf eine PostgreSQL-Datenbank (Schema & Queries).",
    docsUrl: "https://github.com/modelcontextprotocol/servers-archived/tree/main/src/postgres",
    entry: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-postgres", "<CONNECTION_STRING>"],
    },
  },
  {
    id: "sqlite",
    label: "SQLite",
    description: "Abfragen und Analyse einer lokalen SQLite-Datenbankdatei.",
    docsUrl: "https://github.com/modelcontextprotocol/servers-archived/tree/main/src/sqlite",
    entry: {
      command: "uvx",
      args: ["mcp-server-sqlite", "--db-path", "<PFAD>"],
    },
  },
  {
    id: "sentry",
    label: "Sentry",
    description: "Fehler und Issues aus Sentry abfragen (gehosteter HTTP-Server, OAuth).",
    docsUrl: "https://github.com/getsentry/sentry-mcp",
    entry: {
      type: "http",
      url: "https://mcp.sentry.dev/mcp",
    },
  },
  {
    id: "context7",
    label: "Context7",
    description: "Aktuelle Bibliotheks-Dokumentation direkt in den Kontext holen.",
    docsUrl: "https://github.com/upstash/context7",
    entry: {
      command: "npx",
      args: ["-y", "@upstash/context7-mcp"],
    },
  },
];
