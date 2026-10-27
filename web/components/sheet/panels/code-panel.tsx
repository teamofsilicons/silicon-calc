"use client";

import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import * as calc from "@/lib/calc/api";
import type { RunAnswer, RunRequest } from "@/lib/calc/types";
import { ApiError } from "@/lib/errors";
import { previewAnswer, runBrowserCommand } from "@/lib/sheet/browser-code";
import { useEditor } from "../editor-context";
import { JavascriptCellPanel } from "./javascript-cell-panel";
import { FunctionsPanel } from "./functions-panel";
import styles from "./panels.module.css";

export function CodePanel() {
  const [mode, setMode] = useState("commands");
  return <><div className={styles.panel}><Select label="JavaScript" value={mode} onValueChange={setMode} options={[{value:"commands",label:"Commands"},{value:"functions",label:"Functions"},{value:"cells",label:"JavaScript cell"}]} /></div>{mode === "functions" ? <FunctionsPanel /> : mode === "cells" ? <JavascriptCellPanel /> : <CommandsPanel />}</>;
}

function CommandsPanel() {
  const { line, controller, session, canEdit } = useEditor();
  const saved = controller.tab?.code;
  const [code, setCode] = useState(saved?.code ?? `return await calc.tab(${JSON.stringify(controller.tab?.name ?? "Tab 1")});`);
  const [destination, setDestination] = useState("preview");
  const [name, setName] = useState("Results");
  const [answer, setAnswer] = useState<RunAnswer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const running = useRef<AbortController | null>(null);
  useEffect(() => () => running.current?.abort(), []);
  const run = async (rerun = false) => {
    if (session.pendingEdits) { setError("Wait for your changes to be kept, then run the command again."); return; }
    const abort = new AbortController();
    const targetTab = controller.activeTab;
    const targetCell = controller.apiRange().split(":")[0]!;
    running.current = abort;
    setBusy(true); setError(null);
    try {
      const result = await runBrowserCommand(line, code, { signal: abort.signal });
      setAnswer(previewAnswer(result));
      let into: RunRequest["into"] | null = null;
      if (rerun) into = { tab: targetTab, at: "A1", replace: true };
      else if (destination === "new") into = { new_tab: name.trim() };
      else if (destination === "selection") into = { tab: targetTab, at: targetCell };
      if (into) {
        setSaving(true);
        const committed = await calc.runCode(line, { code, base_version: result.base_version, inputs: result.inputs, columns: result.columns, rows: result.rows, into }, abort.signal);
        setAnswer({ ...committed, stdout: result.stdout, duration_ms: result.duration_ms });
        await session.refreshTabs();
        for (const tab of session.tabs) await session.reload(tab.id);
        if (committed.tab) controller.openTab(committed.tab.id);
      }
    } catch (failure) {
      setError(failure instanceof ApiError ? [failure.message, failure.hint].filter(Boolean).join(" ") : failure instanceof Error ? failure.message : "The command could not run. Check the code and try again.");
    } finally { running.current = null; setBusy(false); setSaving(false); }
  };
  return <div className={styles.panel}>
    <p className={styles.secondary}>JavaScript runs here in your browser. Commands stop after 60 seconds and have no network access.</p>
    <a href="/docs/javascript" target="_blank" rel="noopener noreferrer">JavaScript guide ↗</a>
    {saved ? <p className={styles.muted}>Made by {saved.run_by.id}. Rerun to refresh its result.</p> : null}
    <Textarea label="JavaScript command" value={code} onChange={event => setCode(event.target.value)} rows={12} spellCheck={false} />
    <Select label="Put the result" options={[{ value: "preview", label: "Preview here" }, { value: "new", label: "In a new tab" }, { value: "selection", label: `At ${controller.apiRange().split(":")[0]}` }]} value={destination} onValueChange={setDestination} />
    {destination === "new" ? <Input label="Tab name" value={name} onChange={event => setName(event.target.value)} /> : null}
    {error ? <p role="alert" className={styles.secondary}>{error}</p> : null}
    <Button disabled={!canEdit || !code.trim() || (destination === "new" && !name.trim())} loading={busy} onClick={() => void run()}>Run command</Button>
    {busy && !saving ? <Button variant="secondary" onClick={() => running.current?.abort()}>Stop command</Button> : null}
    {saved && canEdit ? <Button variant="secondary" disabled={busy || !code.trim()} onClick={() => void run(true)}>Rerun this tab</Button> : null}
    {answer ? <section aria-label="Command result" className={styles.group}><p role="status" className={styles.secondary}>{answer.row_count.toLocaleString()} {answer.row_count === 1 ? "row" : "rows"} · {answer.duration_ms} ms{answer.truncated ? " · first 1,000 shown" : ""}</p><div className={styles.results} data-sq><table className={styles.table}><thead><tr>{answer.columns.map((column, index) => <th key={index} scope="col">{column}</th>)}</tr></thead><tbody>{answer.rows.map((row, index) => <tr key={index}>{row.map((value, col) => <td key={col} data-number={typeof value === "number" ? "" : undefined}>{String(value ?? "")}</td>)}</tr>)}</tbody></table></div>{answer.stdout ? <pre className={styles.output}>{answer.stdout}</pre> : null}</section> : null}
  </div>;
}
