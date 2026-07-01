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
}

export interface ProjectInfo {
  path: string;
  server_count: number;
  exists: boolean;
  is_home: boolean;
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
): Promise<void> {
  return invoke("remove_server", { name, scope, projectPath: projectPath ?? null });
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

export async function toggleUserServer(name: string, enabled: boolean): Promise<void> {
  return invoke("toggle_user_server", { name, enabled, entry: null });
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
