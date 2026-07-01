import { h, clear } from "../dom";
import { icon, setIcon } from "../icons";
import type { MergedServer, ServerEntry, Scope } from "../ipc";
import { revealServerEntry, setScope } from "../ipc";
import { openModal } from "../modal";
import { openConfirm } from "../confirm";
import { toast } from "../toast";
import { statusMeta } from "./serverList";

const ALL_SCOPES: Scope[] = ["user", "local", "project"];

function row(label: string, value: Node | string): HTMLElement {
  return h(
    "div",
    { class: "kv-row" },
    h("div", { class: "kv-key", text: label }),
    h("div", { class: "kv-val" }, typeof value === "string" ? document.createTextNode(value) : value),
  );
}

function mapRows(map: Record<string, string> | undefined): HTMLElement {
  const wrap = h("div", { class: "kv-map" });
  const entries = Object.entries(map ?? {});
  if (entries.length === 0) {
    wrap.append(h("span", { class: "muted", text: "—" }));
    return wrap;
  }
  for (const [k, v] of entries) {
    wrap.append(
      h(
        "div",
        { class: "kv-sub" },
        h("span", { class: "mono kv-subkey", text: k }),
        h("span", { class: "mono kv-subval", text: v }),
      ),
    );
  }
  return wrap;
}

function effectiveType(entry: ServerEntry): string {
  if (entry.type) return entry.type;
  if (entry.url) return "http/sse";
  if (entry.command) return "stdio";
  return "?";
}

function definitionBody(entry: ServerEntry): HTMLElement {
  const wrap = h("div", { class: "kv-table" });
  wrap.append(row("Typ", effectiveType(entry)));
  if (entry.command) wrap.append(row("Command", h("span", { class: "mono", text: entry.command })));
  if (entry.args && entry.args.length) {
    wrap.append(row("Args", h("span", { class: "mono", text: entry.args.join(" ") })));
  }
  if (entry.url) wrap.append(row("URL", h("span", { class: "mono", text: entry.url })));
  wrap.append(row("Env", mapRows(entry.env)));
  wrap.append(row("Headers", mapRows(entry.headers)));
  return wrap;
}

export interface DetailOptions {
  onChanged?: () => void;
}

export function openDetail(server: MergedServer, opts: DetailOptions = {}): void {
  const st = statusMeta(server.status);

  const meta = h(
    "div",
    { class: "detail-meta" },
    h("span", { class: "badge badge-scope", text: server.origin }),
    h("span", { class: `badge ${st.cls}`, title: st.title }, st.label),
    server.enabled
      ? h("span", { class: "badge badge-ok", text: "aktiv" })
      : h("span", { class: "badge badge-muted", text: "deaktiviert" }),
    server.collision
      ? h("span", { class: "badge badge-warn", text: "Namens-Kollision" })
      : null,
  );

  const defWrap = h("div");
  let revealed = false;
  let revealedEntry: ServerEntry | null = null;

  const revealIcon = icon("eye");
  const revealLabel = h("span", { text: "Secrets anzeigen" });
  const revealBtn = h("button", { class: "btn btn-small" }, revealIcon, revealLabel) as HTMLButtonElement;

  const renderDef = () => {
    clear(defWrap);
    if (!server.entry) {
      defWrap.append(h("p", { class: "muted" }, "Extern verwaltet – keine lokale Definition vorhanden."));
      if (server.summary) defWrap.append(h("div", { class: "mono", text: server.summary }));
      return;
    }
    const entry = revealed && revealedEntry ? revealedEntry : server.entry;
    defWrap.append(definitionBody(entry));
  };

  revealBtn.addEventListener("click", async () => {
    if (!server.scope) return;
    if (!revealed) {
      try {
        revealBtn.disabled = true;
        revealedEntry = await revealServerEntry(server.scope, server.name, server.project_path ?? undefined);
        revealed = true;
        setIcon(revealIcon, "eye-off");
        revealLabel.textContent = "Secrets verbergen";
      } catch (e) {
        revealLabel.textContent = "Fehler beim Anzeigen";
        console.error(e);
      } finally {
        revealBtn.disabled = false;
      }
    } else {
      revealed = false;
      setIcon(revealIcon, "eye");
      revealLabel.textContent = "Secrets anzeigen";
    }
    renderDef();
  });

  renderDef();

  // Scope-Wechsel (nur für editierbare Server mit bekanntem Scope).
  let scopeSection: HTMLElement | null = null;
  if (server.editable && server.scope) {
    const currentScope = server.scope;
    const select = h(
      "select",
      { class: "inp" },
      ...ALL_SCOPES.filter((s) => s !== currentScope).map((s) => h("option", { value: s }, s)),
    ) as HTMLSelectElement;
    const moveBtn = h("button", { class: "btn btn-small" }, "Verschieben");
    moveBtn.addEventListener("click", () => {
      const target = select.value as Scope;
      openConfirm({
        title: `Scope ändern: ${server.name}`,
        message: `„${server.name}" von ${currentScope} nach ${target} verschieben? Zuerst im Ziel anlegen, dann aus der Quelle entfernen.`,
        confirmLabel: "Verschieben",
        onConfirm: async () => {
          await setScope(server.name, currentScope, target, server.project_path ?? undefined, undefined);
        },
        onDone: () => {
          toast(`Scope → ${target}`);
          modal.close();
          opts.onChanged?.();
        },
      });
    });
    scopeSection = h(
      "div",
      { class: "detail-scope" },
      h("h3", { text: "Scope ändern" }),
      h("div", { class: "scope-row" }, select, moveBtn),
    );
  }

  const body = h(
    "div",
    { class: "detail" },
    meta,
    server.project_path
      ? h("p", { class: "muted mono", text: `Projekt: ${server.project_path}` })
      : null,
    h(
      "div",
      { class: "detail-defhead" },
      h("h3", { text: "Definition" }),
      server.editable && server.has_secrets ? revealBtn : null,
    ),
    defWrap,
    scopeSection,
  );

  const modal = openModal(server.name, body);
}
