import { h, clear } from "../dom";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { startLogSession, stopLogSession, logSessionBuffer } from "../ipc";
import type { LogLine, MergedServer, Scope } from "../ipc";
import { toast } from "../toast";
import { LOG_RING_CAPACITY } from "../constants";

/// Event-Kanal aller Log-Batches.
///
/// **Muss synchron zu `EVENT_NAME` in `src-tauri/src/logview.rs` bleiben** –
/// eine Umbenennung auf nur einer Seite bricht die Live-Diagnose stumm (kein
/// Compilerfehler, kein Test schlägt an, das Panel bliebe einfach leer).
///
/// Exportiert, weil `main.ts` denselben Kanal global mithört: das Panel ist bei
/// geschlossenem Modal abgemeldet, das „Diagnose läuft"-Badge muss trotzdem
/// verschwinden, wenn der Prozess stirbt.
export const EVENT = "mcp-log";

/// Payload eines `mcp-log`-Events: Zeilen plus die Session, zu der sie gehören.
export interface LogBatch {
  session: string;
  lines: LogLine[];
}

export interface LogViewCallbacks {
  /// Nach erfolgreichem Start (Session-Id) – für das „Diagnose läuft"-Badge.
  onStarted: (id: string) => void;
  /// Nach Stop/Prozessende – Badge entfernen.
  onStopped: () => void;
}

export interface LogViewHandle {
  element: HTMLElement;
  /// Vom Modal-`onClose` aufrufen: Listener abmelden. Die Session läuft weiter
  /// (bis „Stoppen" oder Timeout) – das Badge in der Liste zeigt das an.
  dispose: () => void;
}

/// Live-Diagnose-Panel für einen stdio-Server. Wenn `activeSessionId` gesetzt
/// ist, dockt es an eine laufende Session an (Backfill); sonst bietet es „Start".
export function createLogView(
  server: MergedServer,
  scope: Scope,
  activeSessionId: string | null,
  cb: LogViewCallbacks,
): LogViewHandle {
  let sessionId: string | null = activeSessionId;
  let unlisten: UnlistenFn | null = null;
  const lines: LogLine[] = [];
  const seen = new Set<number>();
  let filter = "";
  let autoscroll = true;

  const panel = h("div", { class: "logview-panel mono" });

  const kindClass = (kind: string) =>
    kind === "stderr"
      ? "log-stderr"
      : kind === "rpc_out"
        ? "log-rpc-out"
        : kind === "rpc_in"
          ? "log-rpc-in"
          : kind === "closed"
            ? "log-closed"
            : "log-stdout";

  const matches = (l: LogLine) => filter === "" || l.text.toLowerCase().includes(filter);

  const lineNode = (l: LogLine) =>
    h("div", { class: `log-line ${kindClass(l.kind)}` }, h("span", { class: "log-kind", text: l.kind }), l.text);

  const scrollToBottom = () => {
    if (autoscroll) panel.scrollTop = panel.scrollHeight;
  };

  const rebuild = () => {
    clear(panel);
    for (const l of lines) if (matches(l)) panel.append(lineNode(l));
    scrollToBottom();
  };

  /// Überzählige Zeilen vorne verwerfen – Puffer und DOM bleiben bei
  /// `LOG_RING_CAPACITY` gedeckelt (Spiegel von `RING_CAPACITY` im Backend).
  /// `panel` enthält per Invariante genau `lines.filter(matches)` in Reihenfolge
  /// (jede Filteränderung geht durch `rebuild()`), deshalb wird der erste
  /// DOM-Knoten nur entfernt, wenn die verworfene Zeile auch gerendert war.
  const trim = () => {
    while (lines.length > LOG_RING_CAPACITY) {
      const dropped = lines.shift();
      if (!dropped) break;
      seen.delete(dropped.seq);
      if (matches(dropped) && panel.firstChild) panel.removeChild(panel.firstChild);
    }
  };

  const addLine = (l: LogLine) => {
    if (seen.has(l.seq)) return;
    seen.add(l.seq);
    lines.push(l);
    trim();
    if (l.kind === "closed") {
      // Prozess/ Session beendet -> Button zurücksetzen, Badge entfernen.
      sessionId = null;
      cb.onStopped();
      setRunning(false);
    }
    if (matches(l)) {
      panel.append(lineNode(l));
      scrollToBottom();
    }
  };

  const handleBatch = (batch: LogLine[]) => {
    for (const l of batch) addLine(l);
  };

  /// Batch aus dem Event-Kanal: alles verwerfen, was nicht zur eigenen Session
  /// gehört. Eine gerade gekillte Vorgänger-Session sendet ihre `closed`-Zeile
  /// noch über denselben Kanal – ungefiltert würde die UI das für das Ende
  /// DIESER Session halten (Badge weg, Prozess nicht mehr stoppbar). Zudem
  /// zählt `seq` je Session bei 0 neu, die Dedup-Menge kollidierte sonst.
  const handleEvent = (payload: LogBatch) => {
    if (payload.session !== sessionId) return;
    handleBatch(payload.lines);
  };

  // --- Steuerleiste -------------------------------------------------------
  const startBtn = h("button", { class: "btn btn-small btn-primary", type: "button" }) as HTMLButtonElement;
  const filterInput = h("input", {
    class: "inp",
    type: "search",
    placeholder: "Filter…",
  }) as HTMLInputElement;
  filterInput.addEventListener("input", () => {
    filter = filterInput.value.trim().toLowerCase();
    rebuild();
  });
  const copyBtn = h(
    "button",
    { class: "btn btn-small", type: "button", title: "Gesamten Puffer kopieren" },
    "Kopieren",
  );
  copyBtn.addEventListener("click", () => {
    const text = lines.map((l) => `[${l.kind}] ${l.text}`).join("\n");
    void navigator.clipboard
      .writeText(text)
      .then(() => toast("Log kopiert"))
      .catch(() => toast("Kopieren fehlgeschlagen", "error"));
  });

  const setRunning = (running: boolean) => {
    startBtn.textContent = running ? "Stoppen" : "Diagnose-Session starten";
    startBtn.classList.toggle("btn-danger", running);
    startBtn.classList.toggle("btn-primary", !running);
  };

  const doStart = async () => {
    startBtn.disabled = true;
    // Zustand der VORIGEN Session verwerfen. `seq` zählt im Backend je Session
    // bei 0 neu (`logview.rs`); ohne das Leeren wären Backfill und alle
    // Live-Batches der neuen Session bereits in `seen` und würden verworfen –
    // das Panel bliebe beim alten Inhalt stehen und die Diagnose wirkte tot.
    seen.clear();
    lines.length = 0;
    clear(panel);
    try {
      // Zuerst lauschen, DANN starten – so gehen keine Handshake-Zeilen verloren.
      if (!unlisten) {
        unlisten = await listen<LogBatch>(EVENT, (e) => handleEvent(e.payload));
      }
      sessionId = await startLogSession(server.name, scope, server.project_path ?? undefined);
      cb.onStarted(sessionId);
      setRunning(true);
      // Ring-Backfill (falls schon Zeilen vor dem Listener anfielen) – dedup per seq.
      handleBatch(await logSessionBuffer(sessionId));
    } catch (e) {
      toast("Diagnose-Session fehlgeschlagen: " + String(e), "error");
      setRunning(false);
    } finally {
      startBtn.disabled = false;
    }
  };

  const doStop = async () => {
    const id = sessionId;
    if (!id) return;
    startBtn.disabled = true;
    try {
      await stopLogSession(id);
    } catch {
      /* best effort */
    } finally {
      sessionId = null;
      cb.onStopped();
      setRunning(false);
      startBtn.disabled = false;
    }
  };

  startBtn.addEventListener("click", () => void (sessionId ? doStop() : doStart()));

  // Autoscroll bei manuellem Hochscrollen aus, am unteren Rand wieder an.
  panel.addEventListener("scroll", () => {
    const atBottom = panel.scrollHeight - panel.scrollTop - panel.clientHeight < 24;
    autoscroll = atBottom;
  });

  const controls = h("div", { class: "logview-controls" }, startBtn, filterInput, copyBtn);
  const notice = h("div", {
    class: "muted logview-notice",
    text: "Beobachtet wird eine eigene, frisch gestartete Instanz – nicht der Prozess, den Claude Code benutzt.",
  });
  const element = h("div", { class: "logview" }, controls, notice, panel);

  // Bei bereits laufender Session andocken (Backfill + live).
  if (activeSessionId) {
    setRunning(true);
    void (async () => {
      if (!unlisten) {
        unlisten = await listen<LogBatch>(EVENT, (e) => handleEvent(e.payload));
      }
      handleBatch(await logSessionBuffer(activeSessionId));
    })();
  } else {
    setRunning(false);
  }

  return {
    element,
    dispose: () => {
      unlisten?.();
      unlisten = null;
    },
  };
}
