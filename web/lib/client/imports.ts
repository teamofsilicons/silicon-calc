/** Durable import queue. Source files live in account-scoped IndexedDB until the
 * service has all chunks; afterward the server owns the work. No credentials are
 * stored. A Web Lock elects one uploader across tabs; server receipts make every
 * request safe to replay even where Web Locks are unavailable. */
import { api } from "./api";
import { ApiError } from "../errors";

export interface ImportDestination { sheet?: string; copy?: string | null; folder_id?: string | null }
interface ImportRequest extends ImportDestination { filename: string; size: number }
export interface ImportJob {
  id: string; request: ImportRequest;
  state: "uploading" | "queued" | "processing" | "completed" | "failed" | "cancelled";
  result?: { sheet: string; copy: string | null; tabs: number[]; warnings: string[] };
  error?: { message: string; hint?: string };
  dismissed?: boolean; chunk_size: number; chunks: number[]; uploaded: number;
  updated_at?: number;
}
interface Source { id: string; owner: string; request: ImportRequest; file: Blob; verify?: boolean; blocked?: string; cancel?: boolean }
export interface ImportView extends ImportJob { percent: number; message?: string; local?: boolean; durable?: boolean; saving?: boolean }
const DB = "calc-imports-v1";
const EMPTY: ImportView[] = [];
let owner: string | null = null;
let generation = 0;
let snapshot: ImportView[] = EMPTY;
const listeners = new Set<() => void>();
const sources = new Map<string, Source>();
const durable = new Set<string>();
const jobs = new Map<string, ImportJob>();
const notices = new Map<string, string>();
const progress = new Map<string, number>();
let active: AbortController | null = null;
let uploading = false;
let timer: ReturnType<typeof setInterval> | null = null;
let visibilityTimer: ReturnType<typeof setTimeout> | null = null;
const RECENT_RESULT_MS = 2 * 60 * 1000;
let database: Promise<IDBDatabase> | null = null;
let refreshing: Promise<void> | null = null;
const completed = new Set<string>();
const saving = new Set<string>();

function db(): Promise<IDBDatabase> {
  return database ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => { request.result.createObjectStore("files", { keyPath: "id" }).createIndex("owner", "owner"); };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => { database = null; reject(request.error); };
    request.onblocked = () => { database = null; reject(new Error("Import storage is busy in another tab.")); };
  });
}
async function storage<T>(mode: IDBTransactionMode, action: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const database = await db();
  return new Promise((resolve, reject) => {
    const tx = database.transaction("files", mode);
    const request = action(tx.objectStore("files"));
    tx.oncomplete = () => resolve(request.result);
    tx.onerror = tx.onabort = () => reject(tx.error ?? request.error);
  });
}
const save = (source: Source) => storage("readwrite", store => store.put(source));
const forget = (id: string) => storage("readwrite", store => store.delete(id));
function emit() {
  if (visibilityTimer) clearTimeout(visibilityTimer);
  visibilityTimer = null;
  const now = Date.now();
  let nextExpiry = Infinity;
  snapshot = Array.from(jobs.values()).filter(job => {
    if (job.dismissed || job.state === "cancelled") return false;
    if (job.state === "completed" || job.state === "failed") {
      // Use the saved completion time, never the time a tab happens to read it.
      const expires = (job.updated_at ?? 0) + RECENT_RESULT_MS;
      if (expires <= now) return false;
      nextExpiry = Math.min(nextExpiry, expires);
    }
    return true;
  }).map(job => {
    const source = sources.get(job.id);
    return { ...job, percent: job.state === "uploading" ? Math.min(99, Math.floor(100 * (progress.get(job.id) ?? job.uploaded) / Math.max(1, job.request.size))) : 100,
      message: source?.cancel ? "Cancelling when connected…" : source?.blocked ?? notices.get(job.id), local: !!source, durable: durable.has(job.id), saving: saving.has(job.id) };
  });
  if (Number.isFinite(nextExpiry)) visibilityTimer = setTimeout(emit, nextExpiry - now);
  for (const listener of listeners) listener();
}
export const subscribeImports = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
export const importSnapshot = () => snapshot;
export const emptyImports = () => EMPTY;
const path = (id: string) => `/v1/imports/${encodeURIComponent(id)}`;
function keep(job: ImportJob) {
  jobs.set(job.id, job);
  if (job.state !== "uploading") progress.delete(job.id);
  if (job.state === "completed" && !completed.has(job.id)) {
    completed.add(job.id);
    window.dispatchEvent(new CustomEvent("calc:import-complete", { detail: job }));
  }
}
function placeholder(source: Source): ImportJob {
  return { id: source.id, request: source.request, state: "uploading", chunk_size: 1024 * 1024, chunks: [], uploaded: 0 };
}
async function refresh(account: string, epoch: number) {
  if (refreshing) return refreshing;
  refreshing = (async () => {
    const stored = await storage<Source[]>("readonly", store => store.index("owner").getAll(account)).catch(() => []);
    if (epoch !== generation) return;
    for (const source of stored) { sources.set(source.id, source); durable.add(source.id); if (!jobs.has(source.id)) jobs.set(source.id, placeholder(source)); }
    emit();
    const response = await api.get<{ imports: ImportJob[] }>("/v1/imports");
    if (epoch !== generation) return;
    const visible = new Set(response.imports.map(job => job.id));
    for (const job of response.imports) {
      keep(job);
      if (job.state !== "uploading") {
        // Failed conversion remains retryable on the server; the browser no longer needs the source.
        sources.delete(job.id); durable.delete(job.id); await forget(job.id).catch(() => undefined);
      }
    }
    for (const id of jobs.keys()) if (!visible.has(id) && !sources.has(id) && !saving.has(id)) jobs.delete(id);
    emit();
  })().finally(() => { refreshing = null; });
  return refreshing;
}
async function upload(account: string, epoch: number) {
  if (uploading || epoch !== generation || !navigator.onLine) return;
  uploading = true;
  try {
    const work = async () => {
      if (epoch !== generation) return;
      // Read again after obtaining the lock: another tab may already have finished.
      await refresh(account, epoch);
      for (const source of sources.values()) {
        if (epoch !== generation || source.owner !== account) return;
        if (source.blocked && !source.cancel) continue;
        active = new AbortController();
        try {
          let job = await api.put<ImportJob>(path(source.id), source.request, { signal: active.signal });
          if (epoch !== generation) return;
          keep(job);
          if (source.cancel) {
            keep(await api.delete<ImportJob>(path(source.id), { signal: active.signal }));
          } else if (job.state === "uploading") {
            for (let part = 0; part * job.chunk_size < source.request.size; part++) {
              if (!source.verify && job.chunks.includes(part)) continue;
              if (epoch !== generation || sources.get(source.id)?.cancel) return;
              const offset = part * job.chunk_size;
              const confirmed = job.uploaded;
              job = await api.post<ImportJob>(`${path(source.id)}/chunks/${part}`, undefined, {
                raw: source.file.slice(offset, offset + job.chunk_size), contentType: "application/octet-stream", signal: active.signal,
                onUploadProgress: ({ loaded }) => { if (epoch !== generation) return; progress.set(source.id, confirmed + (job.chunks.includes(part) ? 0 : loaded)); notices.delete(source.id); emit(); },
              });
              if (epoch !== generation) return;
              keep(job); progress.set(source.id, job.uploaded); emit();
            }
            if (epoch !== generation || sources.get(source.id)?.cancel) return;
            job = await api.post<ImportJob>(`${path(source.id)}/start`, undefined, { signal: active.signal });
            if (epoch !== generation) return;
            keep(job);
          }
          notices.delete(source.id);
          if (job.state !== "uploading" || source.cancel) {
            sources.delete(source.id); durable.delete(source.id); await forget(source.id).catch(() => undefined);
          }
        } catch (error) {
          if (epoch !== generation) return;
          const failure = ApiError.from(error);
          if (failure.status === 0 || failure.status >= 500 || failure.status === 429) {
            notices.set(source.id, navigator.onLine ? "Connection interrupted. Retrying automatically…" : "Offline. Upload will resume when connected.");
            break;
          }
          // A parallel uploader may have submitted the job while this tab sent its last chunk.
          if (failure.status === 409) {
            const current = await api.get<ImportJob>(path(source.id)).catch(() => null);
            if (current && current.state !== "uploading") { keep(current); continue; }
          }
          source.blocked = [failure.message, failure.hint].filter(Boolean).join(" ");
          await save(source).catch(() => undefined);
        } finally { active = null; emit(); }
      }
    };
    if (navigator.locks) await navigator.locks.request(`calc-imports:${account}`, { ifAvailable: true }, lock => lock ? work() : undefined);
    else await work();
  } catch {
    for (const source of sources.values()) notices.set(source.id, "Waiting for a connection. Your saved upload will resume automatically.");
    emit();
  } finally { uploading = false; }
}
function tick() {
  if (!owner) return;
  const account = owner, epoch = generation;
  void refresh(account, epoch).catch(() => undefined);
  void upload(account, epoch);
}
export function startImports(account: string | null) {
  if (owner === account) return;
  owner = account; generation++; active?.abort();
  if (timer) clearInterval(timer);
  jobs.clear(); sources.clear(); durable.clear(); progress.clear(); notices.clear(); completed.clear(); saving.clear(); emit();
  if (account) { tick(); timer = setInterval(tick, 2000); }
}
export async function enqueueImports(files: File[], destination: ImportDestination) {
  if (!owner) throw new Error("Sign in before importing a file.");
  const account = owner;
  // Ask the browser to retain this queue where persistent storage is supported.
  void navigator.storage?.persist?.().catch(() => false);
  for (const file of files) {
    const source: Source = { id: crypto.randomUUID(), owner: account, request: { ...destination, filename: file.name, size: file.size }, file };
    saving.add(source.id); jobs.set(source.id, placeholder(source)); emit();
    try { await save(source); durable.add(source.id); } catch { /* Keep an in-memory copy, with an explicit warning in the panel. */ }
    if (account !== owner) return;
    saving.delete(source.id); sources.set(source.id, source); jobs.set(source.id, placeholder(source)); emit();
  }
  tick();
}
export async function resumeImport(id: string, file: File) {
  const job = jobs.get(id);
  if (!job || !owner) return;
  if (file.name !== job.request.filename || file.size !== job.request.size) throw new Error("Choose the original file with the same name and size.");
  const source: Source = { id, owner, request: job.request, file, verify: true };
  await save(source); durable.add(id); sources.set(id, source); notices.delete(id); tick();
}
export async function retryImport(id: string) {
  const source = sources.get(id);
  if (source) { delete source.blocked; await save(source).catch(() => undefined); notices.delete(id); emit(); tick(); }
  else { keep(await api.post<ImportJob>(`${path(id)}/retry`)); emit(); tick(); }
}
export async function removeImport(id: string) {
  const source = sources.get(id);
  if (source) {
    source.cancel = true; await save(source).catch(() => undefined); active?.abort(); emit(); tick();
  } else { keep(await api.delete<ImportJob>(path(id))); emit(); }
}
if (typeof window !== "undefined") {
  window.addEventListener("online", tick);
  document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") tick(); });
}
