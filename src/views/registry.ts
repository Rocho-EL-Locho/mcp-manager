import { h, clear } from "../dom";
import { searchRegistry } from "../ipc";
import type { RegistryEntryView, RegistryVariant, Scope } from "../ipc";
import { openModal } from "../modal";
import { openServerForm } from "./serverForm";

export interface RegistryContext {
  projectPath?: string;
  defaultScope?: Scope;
}

/// Öffnet den MCP-Katalog-Browser: Suche in der offiziellen Registry, Ergebnisse
/// mit Varianten-Badges, Detailansicht und „Installieren" (befüllt nur das
/// Formular – kein direkter Schreibzugriff).
export function openRegistryBrowser(onSaved: () => void, ctx: RegistryContext = {}): void {
  const queryInput = h("input", {
    class: "inp",
    placeholder: "z. B. filesystem, github, postgres …",
  }) as HTMLInputElement;
  const searchBtn = h("button", { class: "btn btn-primary" }, "Suchen") as HTMLButtonElement;
  const status = h("div", { class: "form-status" });
  const results = h("div", { class: "registry-results" });
  const moreWrap = h("div", { class: "registry-more" });

  const body = h(
    "div",
    { class: "server-form" },
    h("p", {
      class: "muted",
      text: "Server aus der öffentlichen MCP-Registry. „Installieren“ öffnet nur das Formular – geschrieben wird erst nach deiner Bestätigung.",
    }),
    h("div", { class: "registry-searchbar" }, queryInput, searchBtn),
    status,
    results,
    moreWrap,
  );

  const modal = openModal("MCP-Katalog durchsuchen", body);

  // Laufende Suche markieren, damit spät eintreffende Antworten einer alten
  // Suche eine neuere nicht überschreiben.
  let searchToken = 0;

  const badge = (text: string) =>
    h("span", { class: "badge badge-scope", text });

  const renderVariant = (server: RegistryEntryView, variant: RegistryVariant): HTMLElement => {
    const rows: HTMLElement[] = [];

    // Kurzbeschreibung des Starts (command/args bzw. url).
    const summary = variant.entry.url
      ? variant.entry.url
      : [variant.entry.command, ...(variant.entry.args ?? [])].filter(Boolean).join(" ");
    rows.push(h("div", { class: "mono registry-cmd", text: summary }));

    // Umgebungsvariablen / Header, die der Nutzer ausfüllen muss.
    if (variant.env_vars.length) {
      const list = h("ul", { class: "registry-envlist" });
      for (const ev of variant.env_vars) {
        const tags = [
          ev.required ? "erforderlich" : "optional",
          ev.secret ? "Secret" : null,
        ].filter(Boolean) as string[];
        list.append(
          h(
            "li",
            {},
            h("span", { class: "mono", text: ev.name }),
            h("span", { class: "muted", text: ` — ${tags.join(", ")}` }),
            ev.description ? h("div", { class: "muted registry-envdesc", text: ev.description }) : null,
          ),
        );
      }
      rows.push(list);
    }

    const installBtn = h(
      "button",
      {
        class: "btn btn-primary btn-small",
        onclick: () => {
          modal.close();
          void openServerForm({
            mode: "add",
            prefill: {
              name: suggestName(server.name),
              entry: variant.entry,
              secretKeys: variant.secret_keys,
            },
            projectPath: ctx.projectPath,
            defaultScope: ctx.defaultScope,
            onSaved,
          });
        },
      },
      "Installieren",
    );
    rows.push(h("div", { class: "registry-variant-actions" }, installBtn));

    return h(
      "div",
      { class: "registry-variant" },
      h("div", { class: "registry-variant-head" }, badge(variant.kind), h("span", { class: "mono muted", text: variant.label })),
      ...rows,
    );
  };

  const renderCard = (server: RegistryEntryView): HTMLElement => {
    const badges = h(
      "div",
      { class: "registry-badges" },
      ...server.variants.map((v) => badge(v.kind)),
    );

    const details = h("div", { class: "registry-details" });
    details.style.display = "none";
    let built = false;

    const toggle = h(
      "button",
      {
        class: "btn btn-small",
        onclick: () => {
          if (!built) {
            built = true;
            if (server.variants.length) {
              for (const v of server.variants) details.append(renderVariant(server, v));
            } else {
              details.append(
                h("div", { class: "muted", text: "Keine installierbare Variante angegeben." }),
              );
            }
            const repoUrl = safeHttpUrl(server.repository_url);
            if (repoUrl) {
              details.append(
                h(
                  "div",
                  { class: "registry-repo" },
                  h(
                    "a",
                    { href: repoUrl, target: "_blank", rel: "noopener noreferrer" },
                    "Repository ansehen ↗",
                  ),
                ),
              );
            }
            details.append(
              h("div", {
                class: "registry-trust muted",
                text: "Aus öffentlicher Registry – Herkunft und Repository vor der Installation prüfen.",
              }),
            );
          }
          const showing = details.style.display !== "none";
          details.style.display = showing ? "none" : "";
          toggle.textContent = showing ? "Details" : "Details ausblenden";
        },
      },
      "Details",
    );

    return h(
      "div",
      { class: "registry-card" },
      h(
        "div",
        { class: "registry-card-head" },
        h("span", { class: "registry-card-title", text: server.title }),
        server.version ? badge(`v${server.version}`) : null,
      ),
      h("div", { class: "mono muted registry-card-name", text: server.name }),
      server.description ? h("div", { class: "registry-card-desc", text: server.description }) : null,
      badges,
      h("div", { class: "registry-card-actions" }, toggle),
      details,
    );
  };

  const setBusy = (busy: boolean, text?: string) => {
    searchBtn.disabled = busy;
    clear(status);
    if (busy) {
      status.className = "form-status status-busy";
      status.append(h("span", { class: "spinner" }), h("span", { text: text ?? "Suche läuft…" }));
    } else {
      status.className = "form-status";
      if (text) status.textContent = text;
    }
  };

  const renderMore = (query: string, cursor: string | null) => {
    clear(moreWrap);
    if (!cursor) return;
    const moreBtn = h(
      "button",
      {
        class: "btn btn-small",
        onclick: () => {
          void runSearch(query, cursor, true);
        },
      },
      "Mehr laden",
    );
    moreWrap.append(moreBtn);
  };

  const runSearch = async (query: string, cursor: string | null, append: boolean) => {
    const token = ++searchToken;
    setBusy(true, append ? "Lade weitere…" : "Suche läuft…");
    if (!append) clear(results);
    clear(moreWrap);
    try {
      const page = await searchRegistry(query, cursor ?? undefined);
      if (token !== searchToken) return; // veraltete Antwort verwerfen
      setBusy(false);
      if (!append && !page.servers.length) {
        status.textContent = "Keine Server gefunden.";
      }
      for (const s of page.servers) results.append(renderCard(s));
      renderMore(query, page.next_cursor);
    } catch (e) {
      if (token !== searchToken) return;
      setBusy(false);
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
    }
  };

  const trigger = () => void runSearch(queryInput.value.trim(), null, false);
  searchBtn.addEventListener("click", trigger);
  queryInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") trigger();
  });

  // Initial die Anfangsliste laden (leere Query liefert die ersten Server).
  void runSearch("", null, false);
  queryInput.focus();
}

/// Registry-Namen sind umgekehrt-Domain-artig (z. B. `io.github.foo/bar`); als
/// Servername im Formular den letzten, gut lesbaren Abschnitt vorschlagen.
function suggestName(registryName: string): string {
  const tail = registryName.split("/").pop() ?? registryName;
  return tail || registryName;
}

/// Nur http/https-Links zulassen. Die URL stammt aus der öffentlichen Registry;
/// ein `javascript:`-Schema würde beim Klick im Webview ausgeführt.
function safeHttpUrl(url: string | null): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    return u.protocol === "http:" || u.protocol === "https:" ? url : null;
  } catch {
    return null;
  }
}
