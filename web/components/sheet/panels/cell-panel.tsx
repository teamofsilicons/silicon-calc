"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import * as calc from "@/lib/calc/api";
import { cellName } from "@/lib/sheet/address";
import { useControllerRevision, useSessionRevision } from "@/lib/sheet/react";
import { valueText } from "@/lib/sheet/values";
import { formatDateTime } from "@/lib/format";
import { useEditor } from "../editor-context";
import styles from "./panels.module.css";

export function CellPanel() {
  const { line, controller, session, isOwner, canEdit } = useEditor();
  useControllerRevision(controller); useSessionRevision(session);
  const address = cellName(controller.selection.active.row, controller.selection.active.col);
  const detail = useQuery({ queryKey: ["cell", line.sheet, line.copy, controller.activeTab, address, session.version], queryFn: ({ signal }) => calc.getCell(line, controller.activeTab, address, signal) });
  const [editors, setEditors] = useState("");
  const [description, setDescription] = useState("");
  const [scope, setScope] = useState("range");
  const [busy, setBusy] = useState(false);
  return <div className={styles.panel}>
    <h3>{controller.tab?.name}!{address}</h3>
    {detail.isPending ? <p role="status">Reading cell history…</p> : null}
    {detail.isError ? <p role="alert">Could not read cell history. <Button size="sm" variant="ghost" onClick={() => void detail.refetch()}>Try again</Button></p> : null}
    <ul className={styles.list}>{detail.data?.history.map(entry => <li key={entry.version} className={styles.item}><span>{valueText(entry.before) || "Empty"} → {valueText(entry.after) || "Empty"}</span><span className={styles.muted}>{typeof entry.by === "string" ? session.accounts.get(entry.by)?.id ?? "An account" : entry.by.id} · {formatDateTime(entry.at)} · version {entry.version}</span></li>)}</ul>
    {detail.data && !detail.data.history.length ? <p className={styles.secondary}>No recorded changes to this cell.</p> : null}
    <h3>Protected ranges</h3>
    <ul className={styles.list}>{controller.data?.protections.map(protection => <li key={protection.id} className={styles.item}><span>{protection.description || "Protected cells"} · {protection.range ?? "Whole tab"}</span><span className={styles.muted}>{protection.editors.map(account => account.id).join(", ") || "Only the owner"}</span>{isOwner && canEdit ? <HoldToConfirm label="Hold to remove protection" onConfirm={() => void controller.unprotect(protection.id)} /> : null}</li>)}</ul>
    {isOwner && canEdit ? <form className={styles.panel} onSubmit={event => { event.preventDefault(); setBusy(true); void controller.protect({ range: scope === "tab" ? null : controller.apiRange(), editors: editors.split(/[,\s]+/).filter(Boolean), description }).then(ok => { if (ok) { setDescription(""); setEditors(""); } }).finally(() => setBusy(false)); }}>
      <Select label="Protect" value={scope} onValueChange={setScope} options={[{ value: "range", label: controller.apiRange() }, { value: "tab", label: "Whole tab" }]} /><Input label="Description" value={description} onChange={event => setDescription(event.target.value)} /><Input label="Ids allowed to edit" description="The owner can always edit. Separate c: or si: ids with commas." value={editors} onChange={event => setEditors(event.target.value)} /><Button loading={busy} type="submit">Protect cells</Button>
    </form> : null}
  </div>;
}
