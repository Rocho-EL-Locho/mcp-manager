import { h, clear } from "../dom";
import { runClaudeAssistant } from "../ipc";
import type { AssistantResult, Scope } from "../ipc";
import { openModal } from "../modal";
import { openServerForm, field } from "./serverForm";

export interface AssistantContext {
  projectPath?: string;
  defaultScope?: Scope;
}

export function openAssistant(onSaved: () => void, ctx: AssistantContext = {}): void {
  const urlInput = h("input", {
    class: "inp",
    placeholder: "https://github.com/… oder npm-/PyPI-Link",
  }) as HTMLInputElement;
  const ctxInput = h("textarea", {
    class: "inp mono",
    rows: "2",
    placeholder: "optionaler Hinweis, z. B. stdio via uvx",
  }) as HTMLTextAreaElement;
  const analyzeBtn = h("button", { class: "btn btn-primary" }, "Analysieren") as HTMLButtonElement;
  const status = h("div", { class: "form-status" });
  const resultBox = h("div", { class: "assistant-result" });

  const body = h(
    "div",
    { class: "server-form" },
    h("p", {
      class: "muted",
      text: "Claude liest die Quelle (README/Doku) und schlägt eine Konfiguration vor. Es wird nichts geschrieben, bis du im Formular bestätigst.",
    }),
    field("Link", urlInput),
    field("Kontext (optional)", ctxInput),
    h("div", {}, analyzeBtn),
    status,
    resultBox,
  );

  const modal = openModal("Server per Link einrichten (Claude)", body);

  const renderResult = (res: AssistantResult) => {
    clear(resultBox);
    if (res.notes) resultBox.append(h("div", { class: "assistant-notes", text: res.notes }));

    if (res.entry) {
      const summary =
        res.entry.url ??
        [res.entry.command, ...(res.entry.args ?? [])].filter(Boolean).join(" ");
      // Effektiv leerer Vorschlag: weder url noch command/args -> Hinweis statt leerer Zeile.
      const isEmpty = !res.entry.url && !res.entry.command && !(res.entry.args ?? []).length;
      const detailNode = isEmpty
        ? h("div", { class: "muted", text: "Unvollständiger Vorschlag – bitte im Formular ergänzen." })
        : h("div", { class: "mono", text: summary });
      resultBox.append(
        h(
          "div",
          { class: "assistant-preview" },
          h("div", {}, h("strong", { text: res.name ?? "(ohne Namen)" })),
          detailNode,
        ),
        h(
          "button",
          {
            class: "btn btn-primary",
            onclick: () => {
              modal.close();
              void openServerForm({
                mode: "add",
                prefill: { name: res.name ?? undefined, entry: res.entry ?? undefined },
                projectPath: ctx.projectPath,
                defaultScope: ctx.defaultScope,
                onSaved,
              });
            },
          },
          "Ins Formular übernehmen",
        ),
      );
    } else {
      resultBox.append(h("div", { class: "form-status error", text: res.error ?? "Kein Vorschlag." }));
    }

    const rawPre = h("pre", { class: "assistant-raw mono" }, res.raw);
    rawPre.style.display = "none";
    const rawToggle = h(
      "button",
      {
        class: "btn btn-small",
        onclick: () => {
          rawPre.style.display = rawPre.style.display === "none" ? "" : "none";
        },
      },
      "Rohausgabe anzeigen",
    );
    resultBox.append(rawToggle, rawPre);
  };

  analyzeBtn.addEventListener("click", async () => {
    const url = urlInput.value.trim();
    if (!url) {
      status.className = "form-status error";
      status.textContent = "Bitte einen Link angeben.";
      return;
    }
    analyzeBtn.disabled = true;
    clear(resultBox);

    // Sichtbarer Fortschritt: Spinner + Sekundenzähler (claude -p liefert keinen
    // Zwischenstand, daher zeigen wir vergangene Zeit).
    const start = Date.now();
    const label = h("span", { text: "Claude liest die Quelle… (0 s)" });
    clear(status);
    status.className = "form-status status-busy";
    status.append(h("span", { class: "spinner" }), label);
    const timer = window.setInterval(() => {
      const s = Math.round((Date.now() - start) / 1000);
      label.textContent = `Claude liest die Quelle und überlegt… (${s} s)`;
    }, 1000);

    try {
      const res = await runClaudeAssistant(url, ctxInput.value.trim() || undefined);
      clear(status);
      status.className = "form-status";
      renderResult(res);
    } catch (e) {
      clear(status);
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
    } finally {
      window.clearInterval(timer);
      analyzeBtn.disabled = false;
    }
  });
}
