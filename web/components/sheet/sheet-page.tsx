"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { appConfig } from "@/lib/app.config";
import { Braces, Download, GitBranch, History, Home, MessageSquare, MoreHorizontal, Upload, X } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { AvatarGroup } from "@/components/silicon-ui/avatar-group/avatar-group";
import { InlineEdit } from "@/components/silicon-ui/inline-edit/inline-edit";
import { DropdownMenuRoot, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem, DropdownMenuSub, DropdownMenuSubTrigger, DropdownMenuSubContent, DropdownMenuSeparator, DropdownMenuLabel } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import type { LoadedLine } from "@/lib/server/calc";
import * as calc from "@/lib/calc/api";
import type { DownloadFormat, Thread } from "@/lib/calc/types";
import { notify, notifyError } from "@/lib/notify";
import { cellKey, parseRange } from "@/lib/sheet/address";
import { DOWNLOAD_FORMATS, EXTRA_DOWNLOAD_FORMATS, downloadOptions } from "@/lib/sheet/downloads";
import { getFunctions } from "@/lib/sheet/javascript";
import { useControllerRevision, useOpenLine, useSessionRevision } from "@/lib/sheet/react";
import { EditorContext, type PanelName } from "./editor-context";
import { Toolbar } from "./toolbar";
import { FormulaBar } from "./formula-bar";
import { TabBar } from "./tab-bar";
import { Grid } from "./grid/grid";
import { GridMenu, type GridMenuState } from "./grid-menu";
import { ChartsLayer, InsertChartDialog } from "./charts-layer";
import { CommentsPanel, type CommentTarget } from "./panels/comments-panel";
import { HistoryPanel } from "./panels/history-panel";
import { CopiesPanel } from "./panels/copies-panel";
import { JavascriptRuntime } from "./javascript-runtime";
import { CodePanel } from "./panels/code-panel";
import { CellPanel } from "./panels/cell-panel";
import { EmailGate } from "./email-gate";
import { ShareDialog } from "./share-dialog";
import { useFileImport } from "@/components/import/import-progress";
import styles from "./editor.module.css";

type Ready = Extract<LoadedLine, { kind: "ok" }>;
const PANELS: Record<PanelName, string> = { comments: "Comments", history: "History", copies: "Copies", code: "Code", cell: "Cell details" };

export function SheetPage({ loaded, sheetId, copyId }: { loaded: LoadedLine; sheetId: string; copyId: string | null }) {
  const router = useRouter();
  if (loaded.kind === "email_required") return <EmailGate sheetId={sheetId} />;
  if (loaded.kind === "ok") return <Editor key={`${sheetId}:${copyId}:${loaded.atVersion}`} loaded={loaded} sheetId={sheetId} copyId={copyId} />;
  const title = loaded.kind === "sign_in" ? "Sign in to open this sheet" : loaded.kind === "not_found" ? "Sheet not found" : "Could not open this sheet";
  const description = loaded.kind === "sign_in" ? "This sheet is private. Sign in with the account it was shared with." : loaded.kind === "not_found" ? "This sheet or copy is unavailable. Check the link or ask its owner for access." : [loaded.error.message, loaded.error.hint].filter(Boolean).join(" ");
  return <main id="main" className={styles.state}><div className={styles.stateCard} data-sq="surface"><h1 className={styles.stateTitle}>{title}</h1><p className={styles.stateText}>{description}</p><div className={styles.stateActions}>{loaded.kind === "sign_in" ? <a href={`/auth/sign-in?return_to=${encodeURIComponent(`/s/${sheetId}${copyId ? `/c/${copyId}` : ""}`)}`}>Sign in</a> : <Button variant="secondary" onClick={() => router.refresh()}>Try again</Button>}<Link href="/sheets">Back to sheets</Link></div></div></main>;
}

function Editor({ loaded, sheetId, copyId }: { loaded: Ready; sheetId: string; copyId: string | null }) {
  const [sheet, setSheet] = useState(loaded.sheet);
  useEffect(() => { document.title = `${sheet.title} · ${appConfig.name}`; }, [sheet.title]);
  const [panel, setPanel] = useState<PanelName | null>(null);
  const [composing, setComposing] = useState<CommentTarget | null>(null);
  const [share, setShare] = useState(false);
  const [chart, setChart] = useState(false);
  const [menu, setMenu] = useState<GridMenuState | null>(null);
  const fileImport = useFileImport();
  const fileInput = useRef<HTMLInputElement>(null);
  const client = useQueryClient();
  const line = useMemo(() => ({ sheet: sheetId, copy: copyId }), [sheetId, copyId]);
  const atVersion = loaded.atVersion;
  const canEdit = !!loaded.me && (sheet.role === "owner" || sheet.role === "editor") && atVersion === null && (!loaded.copy || loaded.copy.status === "open");
  const canComment = !!loaded.me && sheet.role !== "viewer" && atVersion === null && (!loaded.copy || loaded.copy.status === "open");
  const { session, controller } = useOpenLine({ line, sheet, tabs: loaded.tabs, version: loaded.version, me: loaded.me, atVersion, readOnly: !canEdit, initialCells: loaded.initialCells, initialTab: loaded.initialCells?.tab?.id });
  useSessionRevision(session); useControllerRevision(controller);
  const threadsQuery = useQuery({ queryKey: ["threads", sheetId, copyId], queryFn: ({ signal }) => calc.listThreads(line, "all", signal) });
  const threads = threadsQuery.data?.threads ?? [];
  const upsertThread = useCallback((thread: Thread) => client.setQueryData<{ threads: Thread[] }>(["threads", sheetId, copyId], previous => ({ threads: [...(previous?.threads ?? []).filter(entry => entry.id !== thread.id), thread] })), [client, sheetId, copyId]);
  useEffect(() => {
    const stop = session.observe({ onThread: upsertThread,
      onCopy: () => { void client.invalidateQueries({ queryKey: ["copies", sheetId] }); },
      onMeta: () => { void calc.getSheet(sheetId).then(next => { setSheet(next); session.setSheet(next); }).catch(error => notifyError(error)); },
      onOverwrite: changes => { notify.warning("An earlier change was replaced", `${changes.length} cells had changed since you last read them. Their earlier values remain in history.`); },
    });
    controller.setNoticeHandler(notice => { notify.toast({ type: notice.kind, title: notice.title, description: notice.description }); });
    return () => { stop(); controller.setNoticeHandler(() => undefined); };
  }, [session, controller, client, sheetId, upsertThread]);
  const hrefOf = ({ copy, version, tab }: { copy: string | null; version?: number | null; tab?: number | null }) => {
    const query = new URLSearchParams();
    if (version !== undefined && version !== null) query.set("version", String(version));
    if (tab) query.set("tab", String(tab));
    return `/s/${sheetId}${copy ? `/c/${copy}` : ""}${query.size ? `?${query}` : ""}`;
  };
  const startComment = () => { setComposing({ tab: controller.activeTab, tabName: controller.tab?.name ?? "Tab", range: controller.commentRange() }); setPanel("comments"); };
  const threadsByCell = new Map<number, Thread>();
  for (const thread of threads) { if (thread.tab !== controller.activeTab || thread.status !== "open") continue; const range = thread.range ? parseRange(thread.range) : null; if (range) threadsByCell.set(cellKey(range.r1, range.c1), thread); }
  const importFiles = async (files: File[]) => {
    if (!canEdit) return;
    await fileImport.enqueue(files, line).catch(error => notifyError(error, "Could not queue the import"));
  };
  const download = async (format: DownloadFormat) => {
    if (session.pendingEdits) { notify.info("Saving your changes", "Wait for your changes to be saved, then download again."); return; }
    try {
      const options = downloadOptions(format, controller.activeTab, atVersion);
      if (atVersion === null) {
        const state = await getFunctions(line);
        if (state.calls.some(call => !call.result && (options.tab === undefined || call.tab === options.tab))) {
          notify.info("JavaScript is still calculating", "Keep the sheet open with edit access and download after calculation finishes.");
          return;
        }
        if (state.calls.length) notify.info("JavaScript exports as values", ["xlsx", "ods"].includes(format) ? "The file also keeps the code for direct reimport into Calc. Other apps may remove it when saving." : "This format keeps calculated results. Keep the Calc sheet to rerun the code.");
      }
      window.location.assign(calc.downloadUrl(line, format, options));
    } catch (error) { notifyError(error, "Could not download the sheet"); }
  };
  const context = { session, controller, line, sheet, copy: loaded.copy, me: loaded.me, role: sheet.role, canEdit, canComment, isOwner: sheet.role === "owner", atVersion, panel, openPanel: setPanel, threads, upsertThread, startComment, openShare: () => setShare(true), hrefOf };
  return <EditorContext.Provider value={context}>
    <JavascriptRuntime />
    <a href="#main" className="skip-link">Skip to sheet</a>
    <main id="main" tabIndex={-1} className={styles.editor}>
      <header className={styles.header}>
        <Link href="/sheets" className={styles.home} data-sq="surface" aria-label="Back to sheets"><Home size={20} aria-hidden="true" /></Link>
        <div className={styles.titleBlock}>{canEdit && !copyId ? <InlineEdit as="h1" className={styles.title} label="Sheet title" value={sheet.title} validate={value => value.trim() ? null : "Enter a title."} onSave={async title => { const next = await calc.updateSheet(sheetId, { title }); setSheet(next); session.setSheet(next); }} /> : <h1 className={`${styles.title} ${styles.titleText}`}>{sheet.title}</h1>}<span className={styles.status} role="status">{atVersion !== null ? `Version ${atVersion}` : session.pendingEdits ? "Saving…" : session.live === "live" ? "All changes kept" : session.live === "reconnecting" ? "Reconnecting…" : "Connecting…"}</span></div>
        <div className={styles.headerEnd}>
          {session.viewers.length ? <div className={styles.viewers}><AvatarGroup label="Viewing this sheet" size="sm" max={3} members={Array.from(new Map(session.viewers.map(viewer => [viewer.account.uuid, viewer.account])).values()).map(account => ({ name: `${account.display_name} · ${account.id}`, src: account.pfp_url ?? undefined, status: "online" }))} /></div> : null}
          {([{ name: "comments", icon: MessageSquare }, { name: "history", icon: History }, { name: "copies", icon: GitBranch }, { name: "code", icon: Braces }] as const).map(({ name, icon: Icon }) => <button key={name} className={styles.iconButton} data-sq="surface" aria-label={PANELS[name]} aria-pressed={panel === name} onClick={() => setPanel(panel === name ? null : name)}><Icon size={18} aria-hidden="true" /></button>)}
          <DropdownMenuRoot><DropdownMenuTrigger asChild><button className={styles.iconButton} data-sq="surface" aria-label="Sheet actions"><MoreHorizontal size={18} aria-hidden="true" /></button></DropdownMenuTrigger><DropdownMenuContent align="end">{canEdit ? <DropdownMenuItem icon={<Upload size={16} />} onSelect={() => fileInput.current?.click()}>Import as new tabs</DropdownMenuItem> : null}<DropdownMenuSub><DropdownMenuSubTrigger icon={<Download size={16} />}>Download</DropdownMenuSubTrigger><DropdownMenuSubContent><DropdownMenuLabel>Workbook · CSV and TSV use this tab</DropdownMenuLabel>{DOWNLOAD_FORMATS.map(({ format, label }) => <DropdownMenuItem key={format} onSelect={() => void download(format)}>{label}</DropdownMenuItem>)}<DropdownMenuSeparator /><DropdownMenuSub><DropdownMenuSubTrigger>More formats</DropdownMenuSubTrigger><DropdownMenuSubContent>{EXTRA_DOWNLOAD_FORMATS.map(({ format, label }) => <DropdownMenuItem key={format} onSelect={() => void download(format)}>{label}</DropdownMenuItem>)}</DropdownMenuSubContent></DropdownMenuSub></DropdownMenuSubContent></DropdownMenuSub><DropdownMenuItem onSelect={() => setPanel("cell")}>Cell details and protections</DropdownMenuItem></DropdownMenuContent></DropdownMenuRoot>
          {loaded.me ? <Button size="sm" className={styles.shareButton} onClick={() => setShare(true)}>Share</Button> : <a href={`/auth/sign-in?return_to=${encodeURIComponent(hrefOf({ copy: copyId }))}`}>Sign in</a>}
        </div>
      </header>
      <div>{atVersion !== null || loaded.copy || !canEdit ? <div className={styles.banner}><span className={styles.bannerText}>{atVersion !== null ? `Earlier version ${atVersion} · read only` : loaded.copy ? `Copy: ${loaded.copy.name} · ${loaded.copy.status}` : canComment ? "You can comment on this sheet" : "Viewing this sheet"}</span>{atVersion !== null ? <Link href={hrefOf({ copy: copyId })}>Open current version</Link> : loaded.copy ? <Link href={hrefOf({ copy: loaded.copy.parent_id })}>Open parent</Link> : canComment ? <Button size="xs" variant="ghost" onClick={startComment}>Comment on selection</Button> : null}</div> : null}</div>
      <Toolbar onChart={() => setChart(true)} />
      <FormulaBar />
      <div className={styles.body}>
        <div className={styles.gridColumn}>{session.lastError ? <p role="alert" className={styles.banner}>{session.lastError.message} {session.lastError.hint}</p> : null}<Grid controller={controller} threadsByCell={threadsByCell} onContextMenu={(x, y, area) => setMenu({ x, y, area })} onComment={startComment} onOpenThread={() => setPanel("comments")} onDropFiles={files => void importFiles(files)} overlay={engine => <ChartsLayer engine={engine} />} /></div>
        {panel ? <aside className={styles.side} aria-label={PANELS[panel]}><div className={styles.sideHead}><h2 className={styles.sideTitle}>{PANELS[panel]}</h2>{panel === "comments" && canComment ? <Button size="xs" variant="ghost" onClick={startComment}>New comment</Button> : null}<button className={styles.iconButton} data-sq="surface" aria-label="Close panel" onClick={() => setPanel(null)}><X size={18} aria-hidden="true" /></button></div><div className={styles.sideBody}>{panel === "comments" ? <>{threadsQuery.isError ? <p role="alert">Could not read comments. <Button size="xs" variant="ghost" onClick={() => void threadsQuery.refetch()}>Try again</Button></p> : null}<CommentsPanel composing={composing} onComposed={() => setComposing(null)} /></> : panel === "history" ? <HistoryPanel /> : panel === "copies" ? <CopiesPanel /> : panel === "code" ? <CodePanel key={controller.activeTab} /> : <CellPanel />}</div></aside> : null}
      </div>
      <TabBar />
    </main>
    <input ref={fileInput} type="file" hidden multiple accept=".csv,.tsv,.xlsx,.xls,.ods,.numbers,.json,.parquet" onChange={event => { void importFiles(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
    <GridMenu state={menu} onClose={() => setMenu(null)} onProtect={() => setPanel("cell")} />
    {chart ? <InsertChartDialog open onOpenChange={setChart} /> : null}
    <ShareDialog item={{ kind: "sheet", id: sheetId }} title={sheet.title} open={share} onOpenChange={setShare} canManage={sheet.role === "owner" || sheet.role === "editor"} />
  </EditorContext.Provider>;
}
