import { h, clear } from "../dom";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { startLogSession, stopLogSession, logSessionBuffer } from "../ipc";
import type { LogLine, MergedServer, Scope } from "../ipc";
import { toast } from "../toast";
import { LOG_RING_CAPACITY } from "../constants";

const EVENT = "mcp-log";

/// Nutzlast eines `mcp-log`-Events. Der Kanalname ist fest, deshalb trägt jedes
/// Batch die Session-Id: eine eben beendete Vorgänger-Session schickt noch ihre
/// `closed`-Zeile hinterher, die nicht als eigenes Ende gedeutet werden darf.
interface LogBatch {
  sessionId: string;
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
  /// Kleinste noch gehaltene `seq`. Alles darunter wurde bereits verdrängt und
  /// darf über einen späteren Backfill nicht erneut (und dann unsortiert)
  /// einlaufen.
  let oldestKeptSeq = 0;
  /// Batches, die eintreffen, bevor die eigene Session-Id bekannt ist. Der
  /// Listener läuft bewusst VOR dem Start, damit keine Handshake-Zeile verloren
  /// geht; verwerfen statt puffern würde sie erst über den Backfill und damit in
  /// falscher Reihenfolge nachliefern.
  let pending: LogBatch[] | null = null;
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

  /// Verdrängt die ältesten Zeilen, sobald die Ring-Kapazität überschritten ist –
  /// Array, Dedup-Set und DOM-Knoten gemeinsam. Der DOM enthält genau die zum
  /// aktuellen Filter passenden Zeilen in derselben Reihenfolge, daher gehört
  /// zum verdrängten Eintrag der erste Knoten – aber nur, wenn er sichtbar war.
  const evictOverflow = () => {
    while (lines.length > LOG_RING_CAPACITY) {
      const dropped = lines.shift();
      if (!dropped) break;
      seen.delete(dropped.seq);
      oldestKeptSeq = dropped.seq + 1;
      if (matches(dropped)) panel.firstElementChild?.remove();
    }
  };

  /// Alles verwerfen (neue Session: `seq` zählt wieder bei 0).
  const resetBuffer = () => {
    lines.length = 0;
    seen.clear();
    oldestKeptSeq = 0;
    clear(panel);
  };

  const addLine = (l: LogLine) => {
    if (l.seq < oldestKeptSeq || seen.has(l.seq)) return;
    seen.add(l.seq);
    lines.push(l);
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
    evictOverflow();
  };

  const handleBatch = (batch: LogLine[]) => {
    for (const l of batch) addLine(l);
  };

  /// Ein eingehendes Event: puffern, solange die eigene Id noch fehlt, sonst
  /// fremde (alte) Sessions verwerfen.
  const handleEvent = (batch: LogBatch) => {
    if (pending) {
      pending.push(batch);
      return;
    }
    if (batch.sessionId !== sessionId) return;
    handleBatch(batch.lines);
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
    // Ab jetzt puffern: die Id der eigenen Session steht erst nach dem Start fest.
    pending = [];
    try {
      // Zuerst lauschen, DANN starten – so gehen keine Handshake-Zeilen verloren.
      if (!unlisten) {
        unlisten = await listen<LogBatch>(EVENT, (e) => handleEvent(e.payload));
      }
      const id = await startLogSession(server.name, scope, server.project_path ?? undefined);
      // Reihenfolge wichtig: erst leeren (die `seq` der neuen Session startet
      // wieder bei 0 und kollidierte sonst mit dem Dedup-Set der alten).
      resetBuffer();
      sessionId = id;
      cb.onStarted(id);
      setRunning(true);
      const buffered = pending;
      pending = null;
      for (const b of buffered) if (b.sessionId === id) handleBatch(b.lines);
      // Ring-Backfill (falls schon Zeilen vor dem Listener anfielen) – dedup per seq.
      handleBatch(await logSessionBuffer(id));
    } catch (e) {
      toast("Diagnose-Session fehlgeschlagen: " + String(e), "error");
      setRunning(false);
    } finally {
      pending = null;
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
      // Andocken: die Id ist bereits bekannt, es muss nicht gepuffert werden.
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
