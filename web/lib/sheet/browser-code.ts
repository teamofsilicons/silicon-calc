import * as api from "../calc/api";
import type { RunAnswer, Tab } from "../calc/types";
import { parseRange } from "./address";

export const COMMAND_TIMEOUT_MS = 60_000;
const SANDBOX_STARTUP_TIMEOUT_MS = 30_000;
export class SandboxStartupError extends Error {}
const MAX_RESULT_CELLS = 200_000;
type Scalar = string | number | boolean | null;
export type CommandTable = { columns: string[]; rows: Scalar[][] };
export type JavascriptDependencies = { reads: Array<{ tab: number; range: string | null }>; tabs: boolean };
export type BrowserCommandResult = CommandTable & { base_version: number; inputs: number[]; duration_ms: number; stdout: string; dependencies: JavascriptDependencies };

/** Validate again outside the worker: a command can send any message, so it never defines the API payload shape. */
export function commandTable(value: unknown): CommandTable {
  let columns: unknown, rows: unknown;
  if (Array.isArray(value)) {
    if (!value.length) return { columns: [], rows: [] };
    if (!value.every(row => row !== null && typeof row === "object" && !Array.isArray(row))) throw new Error("Return { columns, rows } or an array of records.");
    const keys = [...new Set(value.flatMap(row => Object.keys(row as object)))];
    columns = keys;
    rows = value.map(row => keys.map(key => (row as Record<string, unknown>)[key] ?? null));
  } else if (value && typeof value === "object") ({ columns, rows } = value as { columns?: unknown; rows?: unknown });
  if (!Array.isArray(columns) || !columns.every(column => typeof column === "string") || !Array.isArray(rows)) throw new Error("Return { columns: [\"Name\"], rows: [[\"Value\"]] } or an array of records.");
  if (columns.length > 16_384 || (rows.length + 1) * columns.length > MAX_RESULT_CELLS) throw new Error("The result is too large. Return at most 200,000 cells, including the header.");
  for (const row of rows) {
    if (!Array.isArray(row) || row.length !== columns.length) throw new Error("Every result row must have one value for each column.");
    for (const cell of row) if (!(cell === null || typeof cell === "string" || typeof cell === "boolean" || (typeof cell === "number" && Number.isFinite(cell)))) throw new Error("Result cells must be text, finite numbers, booleans or null. Convert dates to ISO strings.");
  }
  return { columns: columns as string[], rows: rows as Scalar[][] };
}

export function previewAnswer(result: BrowserCommandResult): RunAnswer {
  return { ...result, rows: result.rows.slice(0, 1000), row_count: result.rows.length, truncated: result.rows.length > 1000, stderr: "", version: null, tab: null };
}

async function stopSandbox(frame: HTMLIFrameElement, nonce: string) {
  await new Promise<void>(resolve => {
    const done = () => { clearTimeout(timer); window.removeEventListener("message", stopped); resolve(); };
    const stopped = (event: MessageEvent) => { if (event.source === frame.contentWindow && event.data?.nonce === nonce && event.data?.kind === "stopped") done(); };
    const timer = setTimeout(done, 150);
    window.addEventListener("message", stopped);
    frame.contentWindow?.postMessage({ kind: "stop", nonce }, "*");
  });
  frame.remove();
}

/** The only bridge from isolated command code to sheet data. Every read uses one immutable version. */
export async function runBrowserCommand(line: api.LineRef, code: string, options: { signal?: AbortSignal; timeoutMs?: number; context?: { tab: number; cell: string }; formula?: boolean } = {}): Promise<BrowserCommandResult> {
  const started = performance.now();
  const timeoutMs = options.timeoutMs ?? COMMAND_TIMEOUT_MS;
  const abort = new AbortController();
  // Loading an isolated frame is network/browser scheduling, not user code execution.
  // In a background tab this can take longer than the formula's execution budget.
  let timeout = setTimeout(() => abort.abort(new SandboxStartupError("JavaScript could not start yet. Please try again.")), SANDBOX_STARTUP_TIMEOUT_MS);
  let executing = false;
  const onAbort = () => abort.abort(new Error("The command was stopped."));
  options.signal?.addEventListener("abort", onAbort, { once: true });
  if (options.signal?.aborted) onAbort();
  let iframe: HTMLIFrameElement | null = null;
  const nonce = crypto.randomUUID();
  let cleanup = () => {};
  try {
    if (abort.signal.aborted) throw abort.signal.reason;
    const snapshot = await api.getTabs(line, abort.signal);
    const inputs = new Set<number>();
    const reads = new Map<string, { tab: number; range: string | null }>();
    const pending = new Set<number>();
    const frame = document.createElement("iframe");
    iframe = frame;
    frame.sandbox.add("allow-scripts");
    frame.title = "JavaScript command sandbox";
    frame.hidden = true;
    frame.src = "/code-sandbox";
    const tabFor = (ref: unknown): Tab => {
      const tab = snapshot.tabs.find(tab => typeof ref === "number" ? tab.id === ref : typeof ref === "string" && tab.name === ref);
      if (!tab) throw new Error("Choose a tab by its name or id from calc.tabs().");
      return tab;
    };
    return await new Promise<BrowserCommandResult>((resolve, reject) => {
      const aborted = () => reject(abort.signal.reason);
      abort.signal.addEventListener("abort", aborted, { once: true });
      const onMessage = (event: MessageEvent) => {
        if (event.source !== frame.contentWindow || !event.data || typeof event.data !== "object") return;
        const message = event.data as Record<string, unknown>;
        if (message.kind === "ready") {
          frame.contentWindow?.postMessage({ kind: "start", nonce, code, timeoutMs, context: options.context, tabs: snapshot.tabs.map(tab => ({ id: tab.id, name: tab.name, used: tab.used })) }, "*");
          return;
        }
        if (message.nonce !== nonce) return;
        if (message.kind === "started" && !executing) {
          executing = true;
          clearTimeout(timeout);
          // The frame owns the execution watchdog, alongside the worker's message
          // queue. Its scheduling can lag behind this document in background tabs.
          // A lost bridge is retryable; it is not evidence that user code timed out.
          timeout = setTimeout(() => abort.abort(new SandboxStartupError("The JavaScript browser connection was interrupted. Please try again.")), timeoutMs + SANDBOX_STARTUP_TIMEOUT_MS);
        }
        if (message.kind === "error") reject(new Error(typeof message.message === "string" ? message.message : "The command could not run."));
        if (message.kind === "result") {
          try { resolve({ ...commandTable(message.result), base_version: snapshot.version, inputs: [...inputs], duration_ms: Math.round(performance.now() - started), stdout: typeof message.stdout === "string" ? message.stdout.slice(0, 8192) : "", dependencies: { reads: [...reads.values()], tabs: message.readsTabs === true } }); } catch (error) { reject(error); }
        }
        if (message.kind !== "read" || !Number.isSafeInteger(message.id) || pending.has(message.id as number)) return;
        const id = message.id as number;
        pending.add(id);
        void (async () => {
          if (pending.size > 8) throw new Error("Read at most eight ranges at a time.");
          if (message.method !== "tab" && message.method !== "values") throw new Error("Use calc.tab(name), calc.values(name, range), or calc.tabs().");
          const tab = tabFor(message.tab);
          const range = message.range === undefined ? tab.used : message.range;
          if (range !== null && (typeof range !== "string" || !parseRange(range))) throw new Error("Use a cell range such as A1:C100.");
          inputs.add(tab.id);
          reads.set(`${tab.id}:${message.range ?? "*"}`, { tab: tab.id, range: message.range === undefined ? null : range as string });
          if (reads.size > 1024) throw new Error("Read at most 1,024 ranges per formula.");
          const values = range ? (await api.getValues(line, tab.id, range as string, abort.signal, snapshot.version)).values : [];
          if (options.formula && values.flat().some(value => value === "#BUSY!")) throw new Error("CALC_INPUT_PENDING");
          if (message.method === "values") return values;
          return { columns: (values[0] ?? []).map((cell, index) => String(cell ?? `Column ${index + 1}`)), rows: values.slice(1) };
        })().then(value => frame.contentWindow?.postMessage({ kind: "read-result", nonce, id, value }, "*"), error => frame.contentWindow?.postMessage({ kind: "read-result", nonce, id, error: error instanceof Error ? error.message : "Could not read the cells." }, "*")).finally(() => pending.delete(id));
      };
      window.addEventListener("message", onMessage);
      cleanup = () => { window.removeEventListener("message", onMessage); abort.signal.removeEventListener("abort", aborted); };
      document.body.append(frame);
      if (abort.signal.aborted) aborted();
    });
  } catch (error) {
    if (abort.signal.aborted) throw abort.signal.reason;
    throw error;
  } finally {
    clearTimeout(timeout);
    options.signal?.removeEventListener("abort", onAbort);
    cleanup();
    abort.abort();
    if (iframe) await stopSandbox(iframe, nonce);
  }
}
