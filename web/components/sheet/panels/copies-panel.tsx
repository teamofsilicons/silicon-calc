"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { InlineEdit } from "@/components/silicon-ui/inline-edit/inline-edit";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import * as calc from "@/lib/calc/api";
import type { CopyCompare } from "@/lib/calc/types";
import { notify, notifyError } from "@/lib/notify";
import { ApiError } from "@/lib/errors";
import { overlayOf, summaryText } from "@/lib/sheet/values";
import { useEditor } from "../editor-context";
import { ComparePanel } from "./history-panel";
import styles from "./panels.module.css";

export function CopiesPanel() {
  const { sheet, line, copy, canEdit, controller, session, hrefOf } = useEditor();
  const router = useRouter();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [comparison, setComparison] = useState<CopyCompare | null>(null);
  const [scope, setScope] = useState("all");
  const [resolutions, setResolutions] = useState<Record<string, "copy" | "sheet">>({});
  const copies = useQuery({ queryKey: ["copies", sheet.id], queryFn: ({ signal }) => calc.listCopies(sheet.id, "all", signal) });
  useEffect(() => () => controller.setDiff(null), [controller]);
  const act = async (work: () => Promise<void>) => { setBusy(true); try { await work(); } catch (error) { notifyError(error); } finally { setBusy(false); } };
  return <div className={styles.panel}>
    <p className={styles.secondary}>Try changes in a copy. Compare them with its parent and choose what to merge back.</p>
    {canEdit ? <form className={styles.panel} onSubmit={event => { event.preventDefault(); void act(async () => { const created = await calc.makeCopy(line, name.trim() || "Untitled copy"); router.push(hrefOf({ copy: created.id })); }); }}><Input label="Name of the copy" value={name} onChange={event => setName(event.target.value)} /><Button type="submit" loading={busy}>Make a copy</Button></form> : null}
    {copies.isPending ? <p role="status">Reading copies…</p> : null}
    {copies.isError ? <p role="alert">{ApiError.from(copies.error).message}</p> : null}
    <Link href={hrefOf({ copy: null })}>Main sheet</Link>
    <ul className={styles.list}>{copies.data?.copies.map(entry => <li key={entry.id} className={styles.item}>
      <Link href={hrefOf({ copy: entry.id })}>{entry.name}</Link>
      <span className={styles.muted}>{entry.status} · {entry.created_by.id}{entry.parent_id ? ` · from ${copies.data.copies.find(parent => parent.id === entry.parent_id)?.name ?? "another copy"}` : " · from the main sheet"}</span>
      {canEdit && entry.status === "open" ? <><InlineEdit label={`Rename ${entry.name}`} value={entry.name} validate={value => value.trim() ? null : "Enter a name."} onSave={async value => { await calc.renameCopy(sheet.id, entry.id, value); await copies.refetch(); }} /><HoldToConfirm label={`Hold to delete ${entry.name}`} tone="danger" onConfirm={() => void act(async () => { await calc.deleteCopy(sheet.id, entry.id); await copies.refetch(); if (entry.id === copy?.id) router.push(hrefOf({ copy: entry.parent_id })); })} /></> : null}
    </li>)}</ul>
    {copy ? <Button variant="secondary" loading={busy} onClick={() => void act(async () => { const result = await calc.compareCopy(sheet.id, copy.id); setComparison(result); setResolutions({}); controller.setDiff(overlayOf(result.diff, result.conflicts)); })}>Compare with parent</Button> : null}
    {comparison && copy ? <>
      <ComparePanel version={copy.forked_at} diff={comparison.diff} title="Changes in this copy" onClose={() => { setComparison(null); controller.setDiff(null); }} />
      {comparison.conflicts.map(conflict => <div className={styles.item} key={`${conflict.tab}:${conflict.a}`}><p>{conflict.tab_name}!{conflict.a}</p><p className={styles.secondary}>Parent: {summaryText(conflict.sheet)} · Copy: {summaryText(conflict.copy)}</p><Select label={`Keep at ${conflict.a}`} value={resolutions[`${conflict.tab}:${conflict.a}`] ?? "sheet"} options={[{ value: "sheet", label: "Parent value" }, { value: "copy", label: "Copy value" }]} onValueChange={value => setResolutions(current => ({ ...current, [`${conflict.tab}:${conflict.a}`]: value as "sheet" | "copy" }))} /></div>)}
      {canEdit ? <><Select label="Merge" value={scope} onValueChange={setScope} options={[{ value: "all", label: "The whole copy" }, { value: "tab", label: "This tab" }, { value: "range", label: `Only ${controller.apiRange()}` }]} /><Button loading={busy} onClick={() => void act(async () => {
        const result = await calc.mergeCopy(sheet.id, copy.id, { ...(scope === "tab" ? { tabs: [controller.activeTab] } : scope === "range" ? { range: { tab: controller.activeTab, range: controller.apiRange() } } : {}), default: "sheet", resolutions: comparison.conflicts.map(conflict => ({ tab: conflict.tab, a: conflict.a, take: resolutions[`${conflict.tab}:${conflict.a}`] ?? "sheet" })) });
        notify.success("Merged into the parent", `${result.cells_taken} cells taken. The copy stays open.`); await session.refreshTabs(); router.push(hrefOf({ copy: copy.parent_id }));
      })}>Merge chosen changes</Button></> : null}
    </> : null}
  </div>;
}
