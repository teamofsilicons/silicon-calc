"use client";

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { useSessionView } from "@/lib/client/session";
import { enqueueImports, subscribeImports, importSnapshot, emptyImports, startImports, removeImport, retryImport, resumeImport, type ImportView } from "@/lib/client/imports";
import styles from "./import-progress.module.css";

export function useFileImport() {
  return { enqueue: enqueueImports };
}

/** Mounted in the root layout, so moving between the workspace and editor keeps
 * both the queue and its progress visible. It never navigates on completion. */
export function ImportProgress() {
  const session = useSessionView();
  const jobs = useSyncExternalStore(subscribeImports, importSnapshot, emptyImports);
  useEffect(() => { startImports(session.account?.uuid ?? null); }, [session.account?.uuid]);
  if (!session.signedIn || !jobs.length) return null;
  return <ImportPanel jobs={jobs} />;
}
function ImportPanel({ jobs }: { jobs: ImportView[] }) {
  const [collapsed, setCollapsed] = useState(false);
  const pending = jobs.filter(job => !["completed", "failed"].includes(job.state)).length;
  const title = pending ? `Importing ${pending} file${pending === 1 ? "" : "s"}` : "Imports";
  return <section className={styles.panel} data-sq="surface" aria-label="Import progress">
    <button className={styles.heading} onClick={() => setCollapsed(!collapsed)} aria-expanded={!collapsed} aria-label={`${title} · ${collapsed ? "Expand" : "Minimize"}`}>
      <strong>{title}</strong>{collapsed ? <ChevronUp size={16} /> : <ChevronDown size={16} />}
    </button>
    {!collapsed ? <div className={styles.items}>{jobs.map(job => <ImportItem key={job.id} job={job} onOpen={() => setCollapsed(true)} />)}</div> : null}
  </section>;
}
function ImportItem({ job, onOpen }: { job: ImportView; onOpen: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const action = async (run: () => Promise<void>) => {
    setBusy(true); setError(null);
    try { await run(); } catch (error) { setError(error instanceof Error ? error.message : String(error)); } finally { setBusy(false); }
  };
  const uploading = job.state === "uploading";
  const failed = job.state === "failed";
  const done = job.state === "completed";
  const stage = job.saving ? "Saving file for recovery…" : job.message ?? (done ? "Imported" : failed ? job.error?.message ?? "Import failed" : job.state === "queued" ? "Queued for processing…" : job.state === "processing" ? "Processing…" : uploading && !job.local && !job.saving ? "Choose the original file to resume this upload." : "Uploading…");
  const result = job.result;
  const href = result ? `/s/${result.sheet}${result.copy ? `/c/${result.copy}` : ""}${result.tabs[0] ? `?tab=${result.tabs[0]}` : ""}` : "";
  return <div className={styles.item} aria-label={job.request.filename}>
    <div className={styles.row}><p className={styles.filename} title={job.request.filename}>{job.request.filename}</p>{uploading ? <span>{job.percent}%</span> : null}{done ? <button className={styles.icon} disabled={busy} aria-label={`Dismiss ${job.request.filename}`} onClick={() => void action(() => removeImport(job.id))}><X size={14} /></button> : null}</div>
    {!done && !failed ? <div className={styles.track} role="progressbar" aria-label={uploading ? "File upload" : "File import"} aria-valuemin={0} aria-valuemax={100} aria-valuenow={uploading ? job.percent : undefined} aria-valuetext={uploading ? `${job.percent}% uploaded` : stage}>
      <span className={uploading ? styles.fill : styles.indeterminate} style={uploading ? { width: `${job.percent}%` } : undefined} />
    </div> : null}
    <p className={styles.stage} role="status">{stage}</p>
    {uploading && job.local ? <p className={styles.hint}>{job.durable ? "Saved on this device. Safe to refresh or open another sheet." : "Keep Calc open until the upload finishes; this browser could not save a local copy."}</p> : job.state === "queued" || job.state === "processing" ? <p className={styles.hint}>You can close Calc. Processing continues in the background.</p> : null}
    {failed && job.error?.hint ? <p className={styles.hint}>{job.error.hint}</p> : null}
    {result?.warnings?.length ? <details className={styles.hint}><summary>Import notes ({result.warnings.length})</summary><ul>{result.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></details> : null}
    {error ? <p className={styles.hint} role="alert">{error}</p> : null}
    <div className={styles.actions}>
      {done && result ? <Link href={href} onClick={onOpen}>Open sheet</Link> : null}
      {failed || uploading && job.message && job.local ? <button disabled={busy} onClick={() => void action(() => retryImport(job.id))}>Retry</button> : null}
      {uploading && !job.local && !job.saving ? <><button disabled={busy} onClick={() => input.current?.click()}>Choose file to resume</button><input ref={input} hidden type="file" onChange={event => { const file = event.target.files?.[0]; if (file) void action(() => resumeImport(job.id, file)); event.target.value = ""; }} /></> : null}
      {uploading && !job.saving || job.state === "queued" || failed ? <button disabled={busy} onClick={() => void action(() => removeImport(job.id))}>{failed ? "Dismiss" : "Cancel import"}</button> : null}
    </div>
  </div>;
}
