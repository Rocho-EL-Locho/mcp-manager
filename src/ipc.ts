// Typisierte Wrapper um die Tauri-invoke-Aufrufe.
// Das Frontend spricht ausschließlich hierüber mit dem Rust-Backend.
// Tauri wandelt camelCase-Keys (JS) automatisch in snake_case (Rust) um.
import { invoke } from "@tauri-apps/api/core";

export interface ClaudeInfo {
  path: string;
  version: string;
  ok: boolean;
}

export type Scope = "user" | "local" | "project";

export interface ServerEntry {
  type?: string;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
}

export type ServerStatus =
  | { kind: "connected" }
  | { kind: "failed"; detail?: string | null }
  | { kind: "needs_auth" }
  | { kind: "pending_approval" }
  | { kind: "disabled" }
  | { kind: "unknown" };

export interface MergedServer {
  name: string;
  scope: Scope | null;
  origin: string;
  project_path: string | null;
  entry: ServerEntry | null;
  summary: string;
  status: ServerStatus;
  enabled: boolean;
  editable: boolean;
  has_secrets: boolean;
  collision: boolean;
  /** Nur gesetzt, wenn der Server bereits introspiziert wurde (aus dem Cache). */
  tool_count?: number;
  resource_count?: number;
  prompt_count?: number;
  /** Verbindungs-/Startzeit (ms) aus dem letzten erfolgreichen Handshake. */
  connect_ms?: number;
  /** Preflight: benötigte Laufzeit (aus `command`) fehlt auf PATH.
   *  undefined => nicht zutreffend (HTTP/SSE oder extern), false => vorhanden,
   *  true => fehlt (Warnung). */
  runtime_missing?: boolean;
}

export interface McpTool {
  name: string;
  description?: string;
  inputSchema?: unknown;
}

export interface McpResource {
  uri: string;
  name?: string;
  description?: string;
  mimeType?: string;
}

export interface McpPrompt {
  name: string;
  description?: string;
}

export interface Introspection {
  tools: McpTool[];
  resources: McpResource[];
  prompts: McpPrompt[];
  serverName?: string;
  serverVersion?: string;
  notes: string[];
  /** Redigierter stderr-Auszug des Server-Subprozesses (nur stdio). */
  logs?: string;
  /** Fehlermeldung, falls Start/Handshake fehlschlug (redigiert). */
  error?: string;
  /** Verbindungs-/Startzeit (ms): Prozessstart bis initialize (nur stdio-Erfolg). */
  connectMs?: number;
  introspectedAt: number;
}

export interface RuntimePreflight {
  /** Der geprüfte Befehl, wie in der Definition ("npx", "/usr/bin/python3"). */
  command: string;
  /** Menschlicher Name der Laufzeit ("Node.js", "Python", "Docker", …). */
  runtime: string;
  /** Auf PATH gefunden bzw. (Pfad-Befehl) existent und ausführbar. */
  found: boolean;
  /** Aufgelöster Pfad, falls gefunden. */
  path?: string;
  /** Erkannte Version (best effort), falls ermittelbar. */
  version?: string;
  /** Umsetzbarer Hinweis – nur gesetzt, wenn nicht gefunden. */
  hint?: string;
}

export interface ProjectInfo {
  path: string;
  server_count: number;
  exists: boolean;
  is_home: boolean;
}

export type Theme = "system" | "light" | "dark";

/** Persistente App-Einstellungen. Feldnamen = snake_case (serde-Serialisierung). */
export interface AppSettings {
  /** Pfad zur claude-CLI; null = automatische Auflösung. */
  claude_path: string | null;
  list_timeout_secs: number;
  mut_timeout_secs: number;
  /** Auto-Refresh-Intervall in Minuten (0 = aus) — Konsument: Feature 09. */
  auto_refresh_minutes: number;
  /** Benachrichtigungen — Konsument: Feature 09. */
  notifications: boolean;
  /** Aufbewahrte Snapshots — Konsument: Feature 05. */
  snapshot_retention: number;
  /** UI-Sprache; null = System — Konsument: Feature 21. */
  language: string | null;
  theme: Theme;
}

export async function checkClaude(): Promise<ClaudeInfo> {
  return invoke<ClaudeInfo>("check_claude");
}

export async function listProjects(): Promise<ProjectInfo[]> {
  return invoke<ProjectInfo[]>("list_projects");
}

export async function deleteProject(path: string): Promise<void> {
  return invoke("delete_project", { path });
}

export async function listServers(
  reveal = false,
  projectPath?: string,
  withStatus = true,
): Promise<MergedServer[]> {
  return invoke<MergedServer[]>("list_servers", {
    projectPath: projectPath ?? null,
    reveal,
    withStatus,
  });
}

export async function healthCheck(
  name: string,
  projectPath?: string,
): Promise<ServerStatus> {
  return invoke<ServerStatus>("health_check", {
    name,
    projectPath: projectPath ?? null,
  });
}

export async function revealServerEntry(
  scope: Scope,
  name: string,
  projectPath?: string,
): Promise<ServerEntry | null> {
  return invoke<ServerEntry | null>("reveal_server_entry", {
    scope,
    name,
    projectPath: projectPath ?? null,
  });
}

export async function introspectServer(
  name: string,
  scope: Scope,
  projectPath?: string,
  refresh = false,
): Promise<Introspection> {
  return invoke<Introspection>("introspect_server", {
    name,
    scope,
    projectPath: projectPath ?? null,
    refresh,
  });
}

/// Laufzeit-Preflight: prüft, ob der benötigte Befehl auf PATH verfügbar ist.
/// Startet den Server NICHT. Null für HTTP/SSE-Server (kein lokaler Befehl).
export async function preflightServer(
  name: string,
  scope: Scope,
  projectPath?: string,
): Promise<RuntimePreflight | null> {
  return invoke<RuntimePreflight | null>("preflight_server", {
    name,
    scope,
    projectPath: projectPath ?? null,
  });
}

/// Gecachtes Introspektions-Ergebnis abrufen, ohne den Server-Prozess zu starten.
export async function peekIntrospection(
  name: string,
  scope: Scope,
  projectPath?: string,
): Promise<Introspection | null> {
  return invoke<Introspection | null>("peek_introspection", {
    name,
    scope,
    projectPath: projectPath ?? null,
  });
}

/** Ergebnis eines Playground-Aufrufs (Spiegel von Rust `PlaygroundResult`). */
export interface PlaygroundResult {
  ok: boolean;
  isError: boolean;
  result?: unknown;
  error?: string;
  notes: string[];
  logs?: string;
  durationMs?: number;
}

/** Playground-Operation (serde tag "kind", snake_case). */
export type PlaygroundRequest =
  | { kind: "call_tool"; name: string; arguments: unknown }
  | { kind: "read_resource"; uri: string }
  | { kind: "get_prompt"; name: string; arguments: Record<string, string> };

async function playgroundCall(
  name: string,
  scope: Scope,
  projectPath: string | undefined,
  request: PlaygroundRequest,
): Promise<PlaygroundResult> {
  return invoke<PlaygroundResult>("playground_call", {
    name,
    scope,
    projectPath: projectPath ?? null,
    request,
  });
}

export function callTool(
  name: string,
  scope: Scope,
  projectPath: string | undefined,
  toolName: string,
  args: unknown,
): Promise<PlaygroundResult> {
  return playgroundCall(name, scope, projectPath, { kind: "call_tool", name: toolName, arguments: args });
}

export function readResource(
  name: string,
  scope: Scope,
  projectPath: string | undefined,
  uri: string,
): Promise<PlaygroundResult> {
  return playgroundCall(name, scope, projectPath, { kind: "read_resource", uri });
}

export function getPrompt(
  name: string,
  scope: Scope,
  projectPath: string | undefined,
  promptName: string,
  args: Record<string, string>,
): Promise<PlaygroundResult> {
  return playgroundCall(name, scope, projectPath, { kind: "get_prompt", name: promptName, arguments: args });
}

export async function addServer(
  name: string,
  scope: Scope,
  entry: ServerEntry,
  projectPath?: string,
): Promise<void> {
  return invoke("add_server", { name, scope, projectPath: projectPath ?? null, entry });
}

export async function updateServer(
  name: string,
  scope: Scope,
  entry: ServerEntry,
  projectPath?: string,
): Promise<void> {
  return invoke("update_server", { name, scope, projectPath: projectPath ?? null, entry });
}

export async function removeServer(
  name: string,
  scope: Scope,
  projectPath?: string,
  skipSnapshot = false,
): Promise<void> {
  return invoke("remove_server", {
    name,
    scope,
    projectPath: projectPath ?? null,
    skipSnapshot,
  });
}

export async function loginServer(name: string): Promise<void> {
  return invoke("login_server", { name });
}

export async function logoutServer(name: string): Promise<void> {
  return invoke("logout_server", { name });
}

export async function resetProjectChoices(projectPath?: string): Promise<void> {
  return invoke("reset_project_choices", { projectPath: projectPath ?? null });
}

export async function toggleMcpjsonServer(
  name: string,
  enabled: boolean,
  projectPath?: string,
): Promise<void> {
  return invoke("toggle_mcpjson_server", { name, projectPath: projectPath ?? null, enabled });
}

export async function toggleUserServer(
  name: string,
  enabled: boolean,
  skipSnapshot = false,
): Promise<void> {
  return invoke("toggle_user_server", { name, enabled, entry: null, skipSnapshot });
}

export interface AssistantResult {
  name: string | null;
  entry: ServerEntry | null;
  notes: string | null;
  raw: string;
  error: string | null;
}

export async function runClaudeAssistant(
  url: string,
  extraContext?: string,
): Promise<AssistantResult> {
  return invoke<AssistantResult>("run_claude_assistant", {
    url,
    extraContext: extraContext ?? null,
  });
}

export async function getSettings(): Promise<AppSettings> {
  return invoke<AppSettings>("get_settings");
}

/// Validiert + persistiert die Einstellungen und liefert die normalisierte Fassung.
export async function setSettings(settings: AppSettings): Promise<AppSettings> {
  return invoke<AppSettings>("set_settings", { settings });
}

export async function setScope(
  name: string,
  fromScope: Scope,
  toScope: Scope,
  fromProject?: string,
  toProject?: string,
): Promise<void> {
  return invoke("set_scope", {
    name,
    fromScope,
    toScope,
    fromProject: fromProject ?? null,
    toProject: toProject ?? null,
  });
}

export async function cloneServer(
  name: string,
  fromScope: Scope,
  newName: string,
  toScope: Scope,
  fromProject?: string,
  toProject?: string,
): Promise<void> {
  return invoke("clone_server", {
    name,
    fromScope,
    newName,
    toScope,
    fromProject: fromProject ?? null,
    toProject: toProject ?? null,
  });
}

/** Eine im Snapshot gesicherte Datei (Feldnamen = snake_case, serde). */
export interface BackupFile {
  original_path: string;
  stored: string;
  existed: boolean;
  size: number;
}

/** Ein Snapshot der MCP-Konfiguration (Spiegel von Rust `SnapshotManifest`). */
export interface BackupInfo {
  id: string;
  created_at: number;
  note: string | null;
  auto: boolean;
  files: BackupFile[];
  /** Manifest fehlte/war unlesbar – dann ist nur Löschen sinnvoll. */
  corrupt: boolean;
}

export async function listBackups(): Promise<BackupInfo[]> {
  return invoke<BackupInfo[]>("list_snapshots");
}

export async function createBackup(note?: string, auto = false): Promise<BackupInfo> {
  return invoke<BackupInfo>("create_snapshot", { note: note ?? null, auto });
}

export async function restoreBackup(id: string, onlyPaths?: string[]): Promise<void> {
  return invoke("restore_snapshot", { id, onlyPaths: onlyPaths ?? null });
}

export async function deleteBackup(id: string): Promise<void> {
  return invoke("delete_snapshot", { id });
}

/** Eine Definition innerhalb eines Namenskonflikts (Spiegel von Rust `ConflictDefinition`). */
export interface ConflictDefinition {
  scope: Scope;
  project_path: string | null;
  summary: string;
  /** Fingerprint nur informativ; der Gleichheitsvergleich kommt aus `identical`. */
  fingerprint: number;
}

/** Namenskonflikt über Scopes (Spiegel von Rust `ConflictInfo`). */
export interface ConflictInfo {
  name: string;
  definitions: ConflictDefinition[];
  /** Scope, dessen Definition effektiv verwendet wird (local > project > user). */
  effective_scope: Scope;
  identical: boolean;
}

export async function listConflicts(projectPath?: string): Promise<ConflictInfo[]> {
  return invoke<ConflictInfo[]>("list_conflicts", { projectPath: projectPath ?? null });
}

export async function renameServer(
  name: string,
  scope: Scope,
  newName: string,
  projectPath?: string,
): Promise<void> {
  return invoke("rename_server", { name, scope, newName, projectPath: projectPath ?? null });
}
