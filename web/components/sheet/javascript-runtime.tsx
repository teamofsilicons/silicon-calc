"use client";
import { useEffect } from "react";
import { ApiError } from "@/lib/errors";
import { SandboxStartupError } from "@/lib/sheet/browser-code";
import { commitResults, evaluateCall, getFunctions, getLibrary, neededLibrary, saveFunction, type JavascriptAnswer } from "@/lib/sheet/javascript";
import { useEditor } from "./editor-context";

/** Recalculates in a bounded, isolated browser worker while an editor has the sheet open. */
export function JavascriptRuntime() {
  const { session, canEdit } = useEditor();
  useEffect(() => {
    if (!canEdit) return;
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined, busy = false, lastVersion = -1, retryDelay = 250;
    const schedule = () => { if (!busy && !abort.signal.aborted) { clearTimeout(timer); timer = setTimeout(() => void run(), retryDelay); } };
    const run = async () => {
      if (busy || abort.signal.aborted || session.pendingEdits || session.version === lastVersion) return;
      busy = true;
      try {
        for (let round = 0; round < 64 && !abort.signal.aborted; round++) {
          if (session.pendingEdits) break;
          const state = await getFunctions(session.line, abort.signal);
          retryDelay = 250;
          const library = (state.names.length || state.calls.some(c => c.name === "JS")) ? (await getLibrary(abort.signal)).functions : [];
          const needed = neededLibrary(state, library);
          const imported = library.find(f => needed.has(f.name) && !state.functions.some(s => s.name === f.name && !s.account) && !state.functions.some(s => s.name === f.name && s.account === f.account && s.code === f.code && s.parameters.join() === f.parameters.join()));
          if (imported) { await saveFunction(session.line, imported, state.version); continue; }
          const pending = state.calls.filter(c => !c.result);
          if (!pending.length) { lastVersion = state.version; break; }
          const results: Array<[number, JavascriptAnswer]> = [];
          let cells = 0, bytes = 0;
          for (const call of pending) {
            try {
              let answer = await evaluateCall(session.line, call, state.functions, abort.signal);
              let size = new TextEncoder().encode(JSON.stringify(answer)).length;
              if (size > 6 * 1024 * 1024 || answer.rows.some(row => row.some(v => v.t === "text" && new TextEncoder().encode(v.v).length > 32768))) {
                answer = { rows: [[{ t: "error", v: "#CALC!" }]], error: "Return at most 6 MB of values, with no more than 32 KB in a text cell." };
                size = 256;
              }
              const count = answer.rows.reduce((n, row) => n + row.length, 0);
              if (results.length && (cells + count > 200_000 || bytes + size > 6 * 1024 * 1024)) break;
              results.push([call.id, answer]); cells += count; bytes += size;
              if (results.length >= 64) break;
            }
            catch (error) { if (abort.signal.aborted || error instanceof SandboxStartupError) throw error; }
          }
          // All remaining calls read each other: expose a circular calculation, never spin forever.
          if (!results.length) for (const call of pending.slice(0,64)) results.push([call.id, { rows: [[{ t: "error", v: "#CIRC!" }]], error: "These JavaScript cells wait on each other. Pass independent inputs to break the cycle." }]);
          await commitResults(session.line, state, results, abort.signal);
        }
      } catch (error) {
        if (!abort.signal.aborted) {
          if (error instanceof ApiError && error.status > 0 && error.status < 500 && error.status !== 409 && error.status !== 429) { lastVersion = session.version; session.lastError = error; session.notify(); }
          else retryDelay = Math.min(15_000, retryDelay * 4);
        }
      } finally { busy = false; if (!abort.signal.aborted && lastVersion !== session.version) schedule(); }
    };
    const unsubscribe = session.subscribe(schedule);
    schedule();
    return () => { abort.abort(); clearTimeout(timer); unsubscribe(); };
  }, [session, canEdit]);
  return null;
}
