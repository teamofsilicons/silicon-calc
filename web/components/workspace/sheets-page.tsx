"use client";

import { useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { Ellipsis, FileSpreadsheet, Folder as FolderIcon, Plus, Upload } from "lucide-react";
import { Page, PageHeader, Cluster } from "@/components/foundation/layout/layout";
import { AccountChip } from "@/components/foundation/account/account-chip";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { SearchField } from "@/components/silicon-ui/search-field/search-field";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import { DropdownMenuRoot, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { ShareDialog } from "@/components/sheet/share-dialog";
import { useFileImport } from "@/components/import/import-progress";
import * as calc from "@/lib/calc/api";
import type { Folder, Sheet } from "@/lib/calc/types";
import { ApiError } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { notify, notifyError } from "@/lib/notify";
import styles from "./workspace.module.css";

type Item = { kind: "sheet"; value: Sheet } | { kind: "folder"; value: Folder };
const titleOf = (item: Item) => item.kind === "sheet" ? item.value.title : item.value.name;
type Action = { kind: "rename" | "trash" | "delete" | "move" | "transfer"; item: Item } | { kind: "folder" } | null;

export function SheetsPage({ scope = "mine", folderId = null }: { scope?: "all" | "mine" | "shared" | "trash"; folderId?: string | null }) {
  const router = useRouter();
  const client = useQueryClient();
  const fileInput = useRef<HTMLInputElement>(null);
  const [search, setSearch] = useState("");
  const [busy, setBusy] = useState(false);
  const fileImport = useFileImport();
  const [action, setAction] = useState<Action>(null);
  const [sharing, setSharing] = useState<Item | null>(null);
  const folder = useQuery({ queryKey: ["folder", folderId], queryFn: ({ signal }) => calc.getFolder(folderId!, signal), enabled: !!folderId });
  const listing = useInfiniteQuery({ queryKey: ["sheets", scope, folderId, search], queryFn: ({ pageParam, signal }) => calc.listSheets({ scope, folder: folderId, q: search, cursor: pageParam }, signal), initialPageParam: null as string | null, getNextPageParam: page => page.next_cursor, enabled: !folderId });
  const transfers = useQuery({ queryKey: ["transfers"], queryFn: ({ signal }) => calc.listTransfers(signal) });
  const refresh = async () => { await Promise.all([client.invalidateQueries({ queryKey: ["sheets"] }), client.invalidateQueries({ queryKey: ["folder"] }), client.invalidateQueries({ queryKey: ["transfers"] })]); };
  const work = async (run: () => Promise<void>) => { setBusy(true); try { await run(); await refresh(); } catch (error) { notifyError(error); } finally { setBusy(false); } };
  const result = folderId ? folder.data : { sheets: listing.data?.pages.flatMap(page => page.sheets) ?? [], folders: listing.data?.pages.flatMap(page => page.folders) ?? [] };
  const sheets = (result?.sheets ?? []).filter(sheet => !folderId || sheet.title.toLowerCase().includes(search.toLowerCase()));
  const folders = (result?.folders ?? []).filter(item => !folderId || item.name.toLowerCase().includes(search.toLowerCase()));
  const pending = folderId ? folder.isPending : listing.isPending;
  const error = folderId ? folder.error : listing.error;
  const title = folderId ? folder.data?.folder.name ?? "Folder" : scope === "shared" ? "Shared with me" : scope === "trash" ? "Trash" : "Sheets";
  const mayCreate = (scope === "all" || scope === "mine") && (!folderId || folder.data?.folder.role === "owner" || folder.data?.folder.role === "editor");
  const newSheet = () => void work(async () => { const sheet = await calc.createSheet({ folder_id: folderId }); router.push(`/s/${sheet.id}`); });
  const importFile = (file: File) => void fileImport.enqueue([file], { folder_id: folderId }).catch(error => notifyError(error, "Could not queue the import"));
  const itemActions = (item: Item) => <DropdownMenuRoot><DropdownMenuTrigger asChild><button className={styles.icon} data-sq="surface" aria-label={`Actions for ${titleOf(item)}`}><Ellipsis size={18} aria-hidden="true" /></button></DropdownMenuTrigger><DropdownMenuContent align="end">
    {scope !== "trash" ? <><DropdownMenuItem onSelect={() => setSharing(item)}>Sharing</DropdownMenuItem>{item.value.role === "owner" || item.value.role === "editor" ? <DropdownMenuItem onSelect={() => setAction({ kind: "rename", item })}>Rename</DropdownMenuItem> : null}{item.value.role === "owner" ? <><DropdownMenuItem onSelect={() => setAction({ kind: "move", item })}>Move</DropdownMenuItem><DropdownMenuItem onSelect={() => setAction({ kind: "transfer", item })}>Transfer ownership</DropdownMenuItem><DropdownMenuItem onSelect={() => setAction({ kind: "trash", item })}>Move to trash</DropdownMenuItem></> : null}</> : <><DropdownMenuItem onSelect={() => void work(async () => { if (item.kind === "sheet") await calc.restoreSheet(item.value.id); else await calc.restoreFolder(item.value.id); notify.success("Restored"); })}>Restore</DropdownMenuItem><DropdownMenuItem onSelect={() => setAction({ kind: "delete", item })}>Delete forever</DropdownMenuItem></>}
  </DropdownMenuContent></DropdownMenuRoot>;
  return <Page>
    {folder.data ? <nav aria-label="Folder path" className={styles.breadcrumb}><Link href="/sheets">Sheets</Link>{folder.data.path.filter(item => item.id !== folderId).map(item => <Link key={item.id} href={`/folders/${item.id}`}>{item.name}</Link>)}</nav> : null}
    <PageHeader title={title} description={scope === "trash" ? "Restore sheets and folders, or remove them permanently." : scope === "shared" ? "Sheets and folders shared with your account." : scope === "all" ? "Your sheets and those shared with you." : "Your spreadsheets, together in one place."} actions={mayCreate ? <Cluster gap={2}><Button variant="secondary" disabled={busy} onClick={() => setAction({ kind: "folder" })}>New folder</Button><Button variant="secondary" disabled={busy} onClick={() => fileInput.current?.click()}><Upload size={16} aria-hidden="true" /> Import</Button><Button loading={busy} onClick={newSheet}><Plus size={16} aria-hidden="true" /> New sheet</Button></Cluster> : undefined} />
    {folder.data ? <Cluster><AccountChip account={folder.data.folder.owner} note="Owner" /><Button variant="ghost" size="sm" onClick={() => setSharing({ kind: "folder", value: folder.data!.folder })}>Sharing</Button>{itemActions({ kind: "folder", value: folder.data.folder })}</Cluster> : null}
    {(transfers.data?.incoming.length || transfers.data?.outgoing.length) ? <section className={styles.transfers} aria-label="Ownership transfers"><h2>Ownership transfers</h2>{[...transfers.data.incoming, ...transfers.data.outgoing].filter(transfer => transfer.status === "pending").map(transfer => <div className={styles.row} key={transfer.id}><span>{transfer.title} · {transfer.from.id} → {transfer.to.id}</span>{transfers.data.incoming.some(incoming => incoming.id === transfer.id) ? <><Button size="sm" variant="secondary" onClick={() => void work(async () => { await calc.answerTransfer(transfer.id, "accept"); })}>Accept</Button><Button size="sm" variant="ghost" onClick={() => void work(async () => { await calc.answerTransfer(transfer.id, "decline"); })}>Decline</Button></> : <Button size="sm" variant="ghost" onClick={() => void work(async () => { await calc.answerTransfer(transfer.id, "cancel"); })}>Cancel transfer</Button>}</div>)}</section> : null}
    <SearchField label="Search sheets and folders" placeholder="Search by name" value={search} onValueChange={setSearch} />
    {pending ? <div className={styles.empty} role="status">Reading sheets…</div> : error ? <div className={styles.empty} role="alert"><p>{ApiError.from(error).message} {ApiError.from(error).hint}</p><Button variant="secondary" onClick={() => void (folderId ? folder.refetch() : listing.refetch())}>Try again</Button></div> : !sheets.length && !folders.length ? <div className={styles.empty}><FileSpreadsheet size={32} strokeWidth={1.5} aria-hidden="true" /><h2>{search ? "No matches" : scope === "trash" ? "Trash is empty" : scope === "shared" ? "Nothing shared yet" : "Your first sheet starts here"}</h2><p>{search ? "Try another name." : (scope === "mine" || scope === "all") ? "Create a blank sheet or bring in a spreadsheet file." : "Sheets and folders will appear here."}</p></div> : <ul className={styles.list} aria-label="Sheets and folders">
      {folders.map(item => <li key={item.id} className={styles.item}><FolderIcon size={22} aria-hidden="true" /><div className={styles.itemText}>{scope === "trash" ? <span>{item.name}</span> : <Link href={`/folders/${item.id}`}>{item.name}</Link>}<span className={styles.meta}>{item.owner.id} · Folder</span></div>{itemActions({ kind: "folder", value: item })}</li>)}
      {sheets.map(sheet => <li key={sheet.id} className={styles.item}><FileSpreadsheet size={22} aria-hidden="true" /><div className={styles.itemText}>{scope === "trash" ? <span>{sheet.title}</span> : <Link href={`/s/${sheet.id}`}>{sheet.title}</Link>}<span className={styles.meta}>{sheet.owner.id} · {formatRelative(sheet.updated_at)}{sheet.public.enabled ? " · Public" : ""}</span></div><span className={styles.role}>{sheet.role}</span>{itemActions({ kind: "sheet", value: sheet })}</li>)}
    </ul>}
    {!folderId && listing.hasNextPage ? <Button variant="secondary" loading={listing.isFetchingNextPage} onClick={() => void listing.fetchNextPage()}>Show more</Button> : null}
    <input ref={fileInput} hidden type="file" accept=".csv,.tsv,.xlsx,.xls,.ods,.numbers,.json,.parquet" onChange={event => { const file = event.target.files?.[0]; if (file) importFile(file); event.target.value = ""; }} />
    {action ? <ActionDialog key={action.kind + ("item" in action ? action.item.value.id : "")} action={action} folderId={folderId} onClose={() => setAction(null)} onDone={refresh} /> : null}
    {sharing ? <ShareDialog item={{ kind: sharing.kind, id: sharing.value.id }} title={titleOf(sharing)} open onOpenChange={open => { if (!open) setSharing(null); }} canManage={sharing.value.role === "owner" || sharing.value.role === "editor"} /> : null}
  </Page>;
}

function ActionDialog({ action, folderId, onClose, onDone }: { action: Exclude<Action, null>; folderId: string | null; onClose: () => void; onDone: () => Promise<void> }) {
  const item = "item" in action ? action.item : null;
  const [text, setText] = useState(action.kind === "rename" ? titleOf(action.item) : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [destination, setDestination] = useState<string | null>(null);
  const destinations = useQuery({ queryKey: ["move-folders", destination], queryFn: ({ signal }) => calc.listSheets({ scope: "mine", folder: destination }, signal), enabled: action.kind === "move" });
  const chosen = useQuery({ queryKey: ["move-parent", destination], queryFn: ({ signal }) => calc.getFolder(destination!, signal), enabled: action.kind === "move" && !!destination });
  const run = async () => {
    setBusy(true); setError(null);
    try {
      if (action.kind === "folder") await calc.createFolder({ name: text.trim(), parent_id: folderId });
      else if (item) {
        const id = item.value.id;
        if (action.kind === "rename") { if (item.kind === "sheet") await calc.updateSheet(id, { title: text.trim() }); else await calc.updateFolder(id, { name: text.trim() }); }
        if (action.kind === "move") { if (item.kind === "sheet") await calc.updateSheet(id, { folder_id: destination }); else await calc.updateFolder(id, { parent_id: destination }); }
        if (action.kind === "transfer") { if (item.kind === "sheet") await calc.transferSheet(id, text.trim()); else await calc.transferFolder(id, text.trim()); }
        if (action.kind === "trash") { if (item.kind === "sheet") await calc.trashSheet(id); else await calc.trashFolder(id); }
        if (action.kind === "delete") { if (item.kind === "sheet") await calc.deleteSheetForever(id, titleOf(item)); else await calc.deleteFolderForever(id, titleOf(item)); }
      }
      await onDone(); onClose();
    } catch (failure) { const problem = ApiError.from(failure); setError([problem.message, problem.hint].filter(Boolean).join(" ")); } finally { setBusy(false); }
  };
  const title = action.kind === "folder" ? "New folder" : action.kind === "rename" ? "Rename" : action.kind === "move" ? "Move" : action.kind === "transfer" ? "Transfer ownership" : action.kind === "trash" ? "Move to trash" : "Delete forever";
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}><DialogContent title={title} description={item ? titleOf(item) : "Keep related sheets together."}><form className={styles.form} onSubmit={event => { event.preventDefault(); void run(); }}>
    {action.kind === "folder" || action.kind === "rename" || action.kind === "transfer" ? <Input autoFocus label={action.kind === "transfer" ? "New owner's c: or si: id" : "Name"} value={text} onChange={event => setText(event.target.value)} /> : null}
    {action.kind === "transfer" ? <p>The recipient must accept before ownership changes.</p> : null}
    {action.kind === "move" ? <><p>Destination: {chosen.data?.folder.name ?? "My sheets"}</p>{destination ? <Button type="button" variant="ghost" onClick={() => setDestination(chosen.data?.folder.parent_id ?? null)}>Up one folder</Button> : null}{destinations.data?.folders.filter(folder => folder.id !== item?.value.id).map(folder => <Button type="button" key={folder.id} variant="ghost" onClick={() => setDestination(folder.id)}><FolderIcon size={16} aria-hidden="true" />{folder.name}</Button>)}</> : null}
    {action.kind === "trash" ? <p>You can restore it from the trash.{item?.kind === "folder" ? " Everything in this folder moves with it." : ""}</p> : null}
    {action.kind === "delete" ? <p>This permanently removes {item ? titleOf(item) : "this item"} and its history. It cannot be restored.</p> : null}
    {error ? <p role="alert">{error}</p> : null}
    <Cluster justify="end"><Button type="button" variant="ghost" onClick={onClose}>Cancel</Button>{action.kind === "trash" || action.kind === "delete" ? <HoldToConfirm label={action.kind === "trash" ? "Hold to move to trash" : "Hold to delete forever"} disabled={busy} tone="danger" onConfirm={() => void run()} /> : <Button loading={busy} type="submit" disabled={action.kind !== "move" && !text.trim()}>{title}</Button>}</Cluster>
  </form></DialogContent></Dialog>;
}
