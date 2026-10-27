/** Static, credential-free document used inside an opaque-origin sandbox iframe. Code executes only in its worker. */
export const CODE_SANDBOX_HTML = String.raw`<!doctype html><html><head><meta charset="utf-8"><title>JavaScript command</title></head><body><script>
"use strict";
let worker = null, nonce = null, watchdog = null, executing = false;
const send = data => parent.postMessage({ ...data, nonce }, "*");
function workerProgram(tabs, context) {
  "use strict";
  const send = postMessage.bind(self);
  const waiting = new Map(); let next = 0;
  const read = (method, tab, range) => new Promise((resolve, reject) => { const id = ++next; waiting.set(id, { resolve, reject }); send({kind:"read", id, method, tab, range}); });
  self.onmessage = event => { const item = waiting.get(event.data.id); if (!item) return; waiting.delete(event.data.id); event.data.error ? item.reject(new Error(event.data.error)) : item.resolve(event.data.value); };
  const values = (tab, range) => range === undefined ? read("values", context?.tab ?? tabs[0]?.id, tab) : read("values", tab, range);
  let readsTabs = false;
  const calc = Object.freeze({ tabs: () => { readsTabs = true; return tabs; }, values, range: values, cell: async (tab, address) => (await values(tab, address))[0]?.[0] ?? null, tab: (tab = context?.tab ?? tabs[0]?.id) => read("tab", tab), context: Object.freeze(context ?? {}) });
  let output = "";
  const console = Object.freeze({ log: (...args) => { output = (output + args.map(value => typeof value === "string" ? value : JSON.stringify(value)).join(" ") + "\n").slice(0, 8192); } });
  for (const key of ["fetch", "XMLHttpRequest", "WebSocket", "EventSource", "importScripts", "Worker", "SharedWorker"]) { try { Object.defineProperty(self, key, {value:undefined, configurable:false}); } catch {} }
  send({kind:"started"});
  (async () => { /*COMMAND*/
  })().then(result => send({kind:"result", result, stdout:output, readsTabs})).catch(error => send({kind:"error", message:error instanceof Error ? error.message : String(error)}));
}
addEventListener("message", event => {
  if (event.source !== parent) return;
  const message = event.data;
  if (!message || typeof message !== "object") return;
  if (message.kind === "start" && !worker && typeof message.nonce === "string") {
    nonce = message.nonce;
    const source = "(" + workerProgram.toString().replace("/*COMMAND*/", () => message.code) + ")(" + JSON.stringify(message.tabs) + "," + JSON.stringify(message.context ?? {}) + ")";
    const url = URL.createObjectURL(new Blob([source], {type:"text/javascript"}));
    try {
      worker = new Worker(url);
      worker.onmessage = event => {
        const data = event.data;
        if (data?.kind === "started" && !executing) {
          executing = true;
          const limit = Math.max(1, Math.min(60000, Number(message.timeoutMs) || 60000));
          watchdog = setTimeout(() => {
            worker.terminate();
            send({kind:"error", message:"The command exceeded " + Math.round(limit / 1000) + " seconds and was stopped. Try a smaller range."});
          }, limit);
        }
        if (data?.kind === "result" || data?.kind === "error") clearTimeout(watchdog);
        send(data);
      };
      worker.onerror = event => { clearTimeout(watchdog); send({kind:"error", message:event.message || "The JavaScript could not run. Check its syntax."}); event.preventDefault(); };
    } catch (error) { send({kind:"error", message:error.message}); }
    URL.revokeObjectURL(url);
  } else if (message.nonce === nonce && message.kind === "stop") {
    clearTimeout(watchdog);
    if (worker) worker.terminate();
    send({kind:"stopped"});
  } else if (message.nonce === nonce && message.kind === "read-result" && worker) worker.postMessage(message);
});
addEventListener("pagehide", () => { if (worker) worker.terminate(); });
parent.postMessage({kind:"ready"}, "*");
</script></body></html>`;

export const CODE_SANDBOX_CSP = "default-src 'none'; script-src 'unsafe-inline' blob:; worker-src blob:; connect-src 'none'; img-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
