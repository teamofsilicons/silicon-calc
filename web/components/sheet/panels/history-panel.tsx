"use client";

/**
 * History: every version of the sheet (runs of one account's changes, plus named versions), newest first, grouped by
 * day. A version can be opened read-only, compared with now (the changed cells tinted on the grid and listed here),
 * named, restored (the whole sheet, one tab, or the selected range, as a new version), used to undo everything one
 * account changed since it (keeping everyone else's changes), or made into a new sheet.
 */
import { useEffect, useMemo, useState } from "react";
import { useRouter } from "next/navigation";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { Ellipsis, GitCompare, History as HistoryIcon } from "lucide-react";
import {
  DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger, DropdownMenuTrigger,
} from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Badge } from "@/components/silicon-ui/badge/badge";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import * as calc from "@/lib/calc/api";
import type { Account, Version, WorkbookDiff } from "@/lib/calc/types";
import { ApiError } from "@/lib/errors";
import { formatCount, formatDate, formatDateTime, formatTime, plural } from "@/lib/format";
import { notify, notifyError } from "@/lib/notify";
import { overlayOf, summaryText } from "@/lib/sheet/values";
import { useEditor } from "../editor-context";
import styles from "./panels.module.css";

const KIND_WORDS: Partial<Record<Version["kind"], string>> = { import: "Import", code: "Code", merge: "Merge", restore: "Restore", undo: "Undo", redo: "Redo", create: "Created" };

function dayLabel(iso: string): string {
  const date = new Date(iso);
  const today = new Date();
  const yesterday = new Date(Date.now() - 86_400_000);
  const same = (a: Date, b: Date) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (same(date, today)) return "Today";
  if (same(date, yesterday)) return "Yesterday";
  return formatDate(iso);
}

function accountWords(account: Account): string {
  return account.display_name && account.display_name !== account.id ? `${account.display_name} (${account.id})` : account.id;
}

type Ask = { kind: "name"; version: Version } | { kind: "new-sheet"; version: Version } | null;

export function HistoryPanel() {
  const { line, session, controller, atVersion, canEdit, hrefOf, sheet } = useEditor();
  const router = useRouter();
  const client = useQueryClient();
  const [comparing, setComparing] = useState<{ version: number; diff: WorkbookDiff } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [ask, setAsk] = useState<Ask>(null);
  const key = ["versions", line.sheet, line.copy];
  const versions = useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam, signal }) => calc.listVersions(line, pageParam, 50, signal),
    initialPageParam: null as string | null,
    getNextPageParam: last => last.next_cursor,
  });

  // New versions arrive with the live stream: read the first page again (a little later, so bursts read once).
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const previous = session.onChange;
    const stop = session.observe({ onChange: event => {
      previous?.(event);
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => void client.invalidateQueries({ queryKey: key }), 1200);
    } });
    return () => {
      stop();
      if (timer) clearTimeout(timer);
    };
    // The key is the line's; the session is the line's.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, client]);

  // Leaving the panel stops comparing.
  useEffect(() => () => controller.setDiff(null), [controller]);

  const all = useMemo(() => versions.data?.pages.flatMap(page => page.versions) ?? [], [versions.data]);
  const groups = useMemo(() => {
    const out: Array<{ label: string; versions: Version[] }> = [];
    for (const version of all) {
      const label = dayLabel(version.ended_at || version.started_at);
      const last = out[out.length - 1];
      if (last && last.label === label) last.versions.push(version);
      else out.push({ label, versions: [version] });
    }
    return out;
  }, [all]);
  const head = all[0]?.version ?? session.version;

  const compare = async (version: number) => {
    setBusy(`compare:${version}`);
    try {
      const diff = await calc.compare(line, { from: version, to: "head", limit: 500 });
      setComparing({ version, diff });
      controller.setDiff(overlayOf(diff));
    } catch (error) {
      notifyError(error, "Could not compare");
    } finally {
      setBusy(null);
    }
  };

  const restore = async (version: Version, scope: "sheet" | "tab" | "range") => {
    setBusy(`restore:${version.version}`);
    try {
      const body = scope === "tab" ? { tab: controller.activeTab } : scope === "range" ? { tab: controller.activeTab, range: controller.apiRange() } : {};
      const answer = await calc.restoreVersion(line, version.version, body);
      notify.toast({
        type: "success",
        title: scope === "sheet" ? `Restored the sheet as it was at version ${version.version}` : scope === "tab" ? `Restored ${controller.tab?.name ?? "the tab"} as it was at version ${version.version}` : `Restored ${controller.apiRange()} as it was at version ${version.version}`,
        description: `That is version ${answer.version} now; nothing was lost.`,
        action: { label: "Undo", onClick: () => void session.undo() },
      });
      if (atVersion !== null) router.push(hrefOf({ copy: line.copy }));
      void client.invalidateQueries({ queryKey: key });
    } catch (error) {
      notifyError(error, "Could not restore that version");
    } finally {
      setBusy(null);
    }
  };

  const undoAccount = async (version: Version, account: Account) => {
    setBusy(`undo:${version.version}`);
    try {
      const answer = await calc.undoAccount(line, version.version, account.id);
      notify.success(
        answer.cells_changed ? `Undid ${account.id}'s changes since version ${version.version}` : `${account.id} changed nothing since version ${version.version}`,
        answer.cells_changed ? `${plural(answer.cells_changed, "cell")} went back; everyone else's changes stayed.` : undefined,
      );
      void client.invalidateQueries({ queryKey: key });
    } catch (error) {
      notifyError(error, "Could not undo those changes");
    } finally {
      setBusy(null);
    }
  };

  /** Who changed something after a version (for "undo one account's changes since"). */
  const changedSince = (version: Version) => {
    const out = new Map<string, Account>();
    for (const later of all) if (later.version > version.version && later.by) out.set(later.by.uuid, later.by);
    return [...out.values()];
  };

  if (versions.isError) {
    const failure = ApiError.from(versions.error);
    return (
      <div className={styles.panel}>
        <p role="alert" className={styles.secondary}>{failure.message} {failure.hint}</p>
        <Button size="sm" variant="secondary" onClick={() => void versions.refetch()}>Try again</Button>
      </div>
    );
  }

  return (
    <div className={styles.panel}>
      {comparing ? (
        <ComparePanel version={comparing.version} diff={comparing.diff} onClose={() => { setComparing(null); controller.setDiff(null); }} />
      ) : null}
      {versions.isPending ? <p className={styles.muted}>Reading the history…</p> : null}
      {!versions.isPending && !all.length ? (
        <div className={styles.empty}>
          <HistoryIcon size={20} strokeWidth={1.5} aria-hidden="true" />
          <span className={styles.emptyTitle}>No changes yet</span>
          <span>Every change is kept here with who made it and when, as soon as anyone edits.</span>
        </div>
      ) : null}
      {groups.map(group => (
        <section key={group.label} className={styles.group} aria-label={group.label}>
          <h3 className={styles.groupTitle}>{group.label}</h3>
          <ul className={styles.list}>
            {group.versions.map(version => {
              const isHead = version.version === head;
              const current = atVersion === version.version || (atVersion === null && isHead);
              const kind = KIND_WORDS[version.kind];
              return (
                <li key={version.version} className={styles.entry} data-sq="surface" data-current={current ? "" : undefined} data-testid="version">
                  <div className={styles.row}>
                    <div className={styles.grow}>
                      {version.name ? <div className={styles.wrapTitle}>{version.name}</div> : null}
                      <div className={version.name ? styles.secondary : styles.wrapTitle}>{version.summary}</div>
                    </div>
                    {kind ? <Badge size="sm" className={styles.badge}>{kind}</Badge> : null}
                    <DropdownMenuRoot modal={false}>
                      <DropdownMenuTrigger asChild>
                        <button type="button" className={styles.place} data-sq="surface" aria-label={`Actions for version ${version.version}`} style={{ color: "var(--text-secondary)" }}>
                          <Ellipsis size={16} strokeWidth={1.75} aria-hidden="true" />
                        </button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem onSelect={() => router.push(hrefOf({ copy: line.copy, version: isHead ? null : version.version, tab: controller.activeTab }))}>{isHead ? "Open the current version" : "Open this version"}</DropdownMenuItem>
                        {!isHead ? <DropdownMenuItem icon={<GitCompare size={16} strokeWidth={1.75} />} onSelect={() => void compare(version.version)}>Compare with now</DropdownMenuItem> : null}
                        {canEdit || atVersion !== null ? (
                          <>
                            <DropdownMenuItem disabled={!(sheet.role === "owner" || sheet.role === "editor")} onSelect={() => setAsk({ kind: "name", version })}>{version.name ? "Rename this version" : "Name this version"}</DropdownMenuItem>
                            {version.name ? <DropdownMenuItem onSelect={async () => { try { await calc.unnameVersion(line, version.version); void client.invalidateQueries({ queryKey: key }); } catch (error) { notifyError(error, "Could not remove the name"); } }}>Remove its name</DropdownMenuItem> : null}
                          </>
                        ) : null}
                        {!isHead && (sheet.role === "owner" || sheet.role === "editor") ? (
                          <>
                            <DropdownMenuSeparator />
                            <DropdownMenuSub>
                              <DropdownMenuSubTrigger>Restore</DropdownMenuSubTrigger>
                              <DropdownMenuSubContent>
                                <DropdownMenuItem onSelect={() => void restore(version, "sheet")}>The whole sheet</DropdownMenuItem>
                                <DropdownMenuItem onSelect={() => void restore(version, "tab")}>Only {controller.tab?.name ?? "this tab"}</DropdownMenuItem>
                                <DropdownMenuItem onSelect={() => void restore(version, "range")}>Only {controller.apiRange()}</DropdownMenuItem>
                              </DropdownMenuSubContent>
                            </DropdownMenuSub>
                            {changedSince(version).length ? (
                              <DropdownMenuSub>
                                <DropdownMenuSubTrigger>Undo one account&apos;s changes since</DropdownMenuSubTrigger>
                                <DropdownMenuSubContent>
                                  {changedSince(version).map(account => <DropdownMenuItem key={account.uuid} onSelect={() => void undoAccount(version, account)}>{accountWords(account)}</DropdownMenuItem>)}
                                </DropdownMenuSubContent>
                              </DropdownMenuSub>
                            ) : null}
                          </>
                        ) : null}
                        <DropdownMenuSeparator />
                        <DropdownMenuItem onSelect={() => setAsk({ kind: "new-sheet", version })}>New sheet from this version</DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenuRoot>
                  </div>
                  <span className={styles.muted}>
                    {accountWords(version.by)} · {formatTime(version.ended_at || version.started_at)} · version {version.version}
                    {isHead ? " · current" : ""}
                  </span>
                  {!isHead && atVersion !== version.version ? (
                    <div className={styles.entryActions}>
                      <Button size="xs" variant="ghost" loading={busy === `compare:${version.version}`} onClick={() => void compare(version.version)}>Compare</Button>
                      <Button size="xs" variant="ghost" onClick={() => router.push(hrefOf({ copy: line.copy, version: version.version, tab: controller.activeTab }))}>Open</Button>
                      {sheet.role === "owner" || sheet.role === "editor" ? <Button size="xs" variant="ghost" loading={busy === `restore:${version.version}`} onClick={() => void restore(version, "sheet")}>Restore</Button> : null}
                    </div>
                  ) : null}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
      {versions.hasNextPage ? <Button size="sm" variant="secondary" loading={versions.isFetchingNextPage} onClick={() => void versions.fetchNextPage()}>Show older versions</Button> : null}
      {ask ? <AskDialog key={`${ask.kind}:${ask.version.version}`} ask={ask} onClose={() => setAsk(null)} onNamed={() => void client.invalidateQueries({ queryKey: key })} /> : null}
    </div>
  );
}

/** Naming a version, or making a new sheet from one. */
function AskDialog({ ask, onClose, onNamed }: { ask: Ask; onClose: () => void; onNamed: () => void }) {
  const { line, sheet } = useEditor();
  const router = useRouter();
  const [text, setText] = useState(ask?.kind === "name" ? ask.version.name ?? "" : `${sheet.title} (version ${ask?.version.version})`);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const submit = async () => {
    if (!ask || !text.trim()) return;
    setBusy(true);
    setProblem(null);
    try {
      if (ask.kind === "name") {
        await calc.nameVersion(line, ask.version.version, text.trim());
        onNamed();
        onClose();
      } else {
        const created = await calc.newSheetFromVersion(line, ask.version.version, text.trim());
        onClose();
        notify.toast({ type: "success", title: `Made ${created.title}`, description: `A new sheet from version ${ask.version.version}.`, action: { label: "Open", onClick: () => router.push(`/s/${created.id}`) } });
      }
    } catch (error) {
      const failure = ApiError.from(error);
      setProblem([failure.message, failure.hint].filter(Boolean).join(" "));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={ask !== null} onOpenChange={open => { if (!open) onClose(); }}>
      <DialogContent title={ask?.kind === "new-sheet" ? "New sheet from this version" : "Name this version"} description={ask ? `Version ${ask.version.version}: ${ask.version.summary}, ${formatDateTime(ask.version.ended_at)}.` : undefined}>
        <form style={{ display: "grid", gap: "var(--space-4)" }} onSubmit={event => { event.preventDefault(); void submit(); }}>
          <Input label={ask?.kind === "new-sheet" ? "Title of the new sheet" : "Name"} value={text} onChange={event => setText(event.target.value)} error={problem ?? undefined} autoFocus />
          <div className={styles.actions}>
            <Button type="button" variant="ghost" onClick={onClose}>Cancel</Button>
            <Button type="submit" loading={busy} disabled={!text.trim()}>{ask?.kind === "new-sheet" ? "Make the sheet" : "Save name"}</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** What changed between a version and now: tinted on the grid, listed here, each change a jump to its cell. */
export function ComparePanel({ version, diff, onClose, title }: { version: number; diff: WorkbookDiff; onClose: () => void; title?: string }) {
  const { controller, session } = useEditor();
  const tabName = (id: number) => session.tabs.find(tab => tab.id === id)?.name ?? diff.tabs.find(tab => tab.tab === id)?.name ?? `Tab ${id}`;
  const tabsChanged = diff.tabs.filter(tab => tab.added || tab.removed || tab.renamed_from || tab.rows_added || tab.rows_removed || tab.cols_added || tab.cols_removed);
  return (
    <section className={styles.composer} data-sq="surface" aria-label={title ?? `Changes since version ${version}`}>
      <div className={styles.row}>
        <div className={styles.grow}>
          <div className={styles.wrapTitle}>{title ?? `Changes since version ${version}`}</div>
          <span className={styles.muted}>{diff.cells_changed ? `${plural(diff.cells_changed, "cell")} changed${diff.truncated ? `, the first ${formatCount(diff.cells.length)} listed` : ""}` : "No cells changed"}. Changed cells are tinted on the grid.</span>
        </div>
        <Button size="xs" variant="ghost" onClick={onClose}>Done</Button>
      </div>
      {tabsChanged.length ? (
        <ul className={styles.list}>
          {tabsChanged.map(tab => (
            <li key={tab.tab} className={styles.secondary}>
              {tab.added ? `Added the tab ${tab.name}` : tab.removed ? `Removed the tab ${tab.name}` : tab.renamed_from ? `Renamed ${tab.renamed_from} to ${tab.name}` : tab.name}
              {tab.rows_added ? `, ${plural(tab.rows_added, "row")} added` : ""}
              {tab.rows_removed ? `, ${plural(tab.rows_removed, "row")} removed` : ""}
              {tab.cols_added ? `, ${plural(tab.cols_added, "column")} added` : ""}
              {tab.cols_removed ? `, ${plural(tab.cols_removed, "column")} removed` : ""}
            </li>
          ))}
        </ul>
      ) : null}
      {diff.cells.length ? (
        <ul className={styles.changes} aria-label="Changed cells">
          {diff.cells.slice(0, 500).map(cell => {
            const by = cell.by ? diff.accounts?.[cell.by] : undefined;
            return (
              <li key={`${cell.tab}:${cell.address}`} className={styles.change}>
                <button type="button" className={styles.changeAddress} data-sq="surface" onClick={() => controller.focusRange(cell.tab, cell.address)} aria-label={`Go to ${tabName(cell.tab)}!${cell.address}`}>
                  {session.tabs.length > 1 ? `${tabName(cell.tab)}!` : ""}{cell.address}
                </button>
                <span className={styles.changeText}>
                  <span className={styles.before}>{summaryText(cell.before)}</span>
                  <span className={styles.after}>{summaryText(cell.after)}</span>
                  {by ? <span className={styles.muted}>by {by.id}</span> : null}
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
    </section>
  );
}
