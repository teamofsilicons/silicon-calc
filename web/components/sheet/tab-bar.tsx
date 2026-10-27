"use client";

/**
 * The tab bar: add a tab, open one, rename it (double click or the menu), move it (drag, or the menu), colour it,
 * delete it (with Undo), and see which tabs code made. At the end, the selection's sum, average and count, as in
 * every spreadsheet.
 */
import { forwardRef, useEffect, useMemo, useRef, useState, type ComponentPropsWithoutRef, type DragEvent, type KeyboardEvent } from "react";
import { ArrowLeft, ArrowRight, Braces, ChevronDown, Palette, Pencil, Plus, Trash2 } from "lucide-react";
import {
  DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger, DropdownMenuTrigger,
} from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import type { Tab } from "@/lib/calc/types";
import { notify } from "@/lib/notify";
import { useControllerRevision, useSessionRevision } from "@/lib/sheet/react";
import { formatCount } from "@/lib/format";
import { useEditor } from "./editor-context";
import { TEXT_COLORS } from "./toolbar";
import styles from "./editor.module.css";

const TAB_COLORS = TEXT_COLORS.slice(8, 16);

/** Keep Radix's anchor and ARIA wiring, but open on click. Its pointer-down toggle prevents native dragging. */
const TabTrigger = forwardRef<HTMLButtonElement, ComponentPropsWithoutRef<"button">>(function TabTrigger(props, ref) {
  return <button {...props} ref={ref} onPointerDown={undefined} />;
});

function renamePosition(button: HTMLButtonElement) {
  const bounds = button.getBoundingClientRect();
  const width = Math.min(Math.max(140, bounds.width), window.innerWidth - 16);
  return { left: Math.max(8, Math.min(bounds.left, window.innerWidth - width - 8)), top: bounds.top, width, height: bounds.height };
}

export function TabBar() {
  const { controller, session, canEdit } = useEditor();
  useControllerRevision(controller);
  useSessionRevision(session);
  const tabs = session.tabs;
  const activeTab = controller.activeTab;
  const [renaming, setRenaming] = useState<number | null>(null);
  const [renameBounds, setRenameBounds] = useState({ left: 0, top: 0, width: 140, height: 32 });
  const [menuFor, setMenuFor] = useState<number | null>(null);
  const [deleting, setDeleting] = useState<Tab | null>(null);
  const [drag, setDrag] = useState<{ id: number; over: number | null; side: "before" | "after" } | null>(null);
  const renamingTab = tabs.find(tab => tab.id === renaming);
  const tabButtons = useRef(new Map<number, HTMLButtonElement>());
  const menuReturnFocus = useRef(new Map<number, HTMLElement>());
  const renameId = useRef<number | null>(null);

  const revealTab = (id: number) => {
    tabButtons.current.get(id)?.closest("[data-tab-item]")?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };
  const focusTab = (id: number) => {
    tabButtons.current.get(id)?.focus({ preventScroll: true });
    revealTab(id);
  };

  useEffect(() => { revealTab(activeTab); }, [activeTab]);
  useEffect(() => {
    const button = renaming === null ? null : tabButtons.current.get(renaming);
    if (!button) return;
    const update = () => {
      const next = renamePosition(button);
      setRenameBounds(previous => previous.left === next.left && previous.top === next.top && previous.width === next.width && previous.height === next.height ? previous : next);
    };
    const observer = new ResizeObserver(update);
    observer.observe(button);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [renaming]);

  const selectTab = (id: number) => {
    // The outside click must both dismiss an open menu and select its tab in one gesture.
    menuReturnFocus.current.clear();
    setMenuFor(null);
    controller.openTab(id);
  };

  const openMenu = (id: number) => {
    // Selection and actions are one gesture, including when another menu is already open.
    menuReturnFocus.current.clear();
    controller.openTab(id);
    const button = tabButtons.current.get(id);
    if (button) menuReturnFocus.current.set(id, button);
    setMenuFor(id);
  };

  const startRename = (id: number) => {
    if (!canEdit) return;
    menuReturnFocus.current.clear();
    setMenuFor(null);
    const button = tabButtons.current.get(id);
    if (button) setRenameBounds(renamePosition(button));
    renameId.current = id;
    setRenaming(id);
  };

  const finishRename = (id: number, value: string, cancel: boolean, restoreFocus: boolean) => {
    // Enter/Escape unmount the input, which may also blur. Commit at most once, and never after Escape.
    if (renameId.current !== id) return;
    renameId.current = null;
    if (!cancel && canEdit) controller.renameTab(id, value);
    setRenaming(null);
    if (restoreFocus) requestAnimationFrame(() => focusTab(id));
  };

  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, tab: Tab) => {
    const index = tabs.findIndex(entry => entry.id === tab.id);
    let next: Tab | undefined;
    if (event.key === "ArrowRight") next = tabs[(index + 1) % tabs.length];
    else if (event.key === "ArrowLeft") next = tabs[(index + tabs.length - 1) % tabs.length];
    else if (event.key === "Home") next = tabs[0];
    else if (event.key === "End") next = tabs[tabs.length - 1];
    if (next) {
      event.preventDefault();
      selectTab(next.id);
      focusTab(next.id);
    } else if (event.key === "Enter" || event.key === " " || event.key === "ArrowDown" || event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      event.preventDefault();
      openMenu(tab.id);
    } else if (event.key === "F2" && canEdit) {
      event.preventDefault();
      startRename(tab.id);
    }
  };

  const remove = (tab: Tab) => {
    if (!canEdit) return;
    if (tabs.length <= 1) {
      notify.info("A sheet keeps at least one tab", "Add another tab first, then delete this one.");
      return;
    }
    controller.deleteTab(tab.id);
    notify.toast({ type: "info", title: `Deleted the tab ${tab.name}`, description: "It stays in the history.", action: { label: "Undo", onClick: () => void session.undo() } });
  };

  const move = (tab: Tab, by: number) => {
    if (!canEdit) return;
    const index = Math.max(0, Math.min(tabs.length - 1, tab.index + by));
    if (index !== tab.index) controller.moveTab(tab.id, index);
  };

  const onDragOver = (event: DragEvent<HTMLElement>, tab: Tab) => {
    if (!canEdit || !drag || drag.id === tab.id) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    const box = event.currentTarget.getBoundingClientRect();
    const side = event.clientX < box.left + box.width / 2 ? "before" : "after";
    if (drag.over !== tab.id || drag.side !== side) setDrag({ ...drag, over: tab.id, side });
  };

  const onDrop = (event: DragEvent<HTMLElement>, tab: Tab) => {
    event.preventDefault();
    if (!canEdit || !drag || drag.id === tab.id) return setDrag(null);
    const from = tabs.findIndex(t => t.id === drag.id);
    const box = event.currentTarget.getBoundingClientRect();
    let to = tabs.findIndex(t => t.id === tab.id) + (event.clientX >= box.left + box.width / 2 ? 1 : 0);
    if (from < to) to -= 1;
    setDrag(null);
    if (from >= 0 && from !== to) controller.moveTab(drag.id, to);
  };

  return (
    <div className={styles.tabBar}>
      {canEdit ? (
        <Tooltip content="Add a tab">
          <button type="button" className={styles.iconButton} data-sq="surface" aria-label="Add a tab" onClick={() => void controller.addTab()}>
            <Plus size={18} strokeWidth={1.75} aria-hidden="true" />
          </button>
        </Tooltip>
      ) : null}
      <div className={styles.tabs} role="tablist" aria-label="Tabs" aria-orientation="horizontal">
        {tabs.map(tab => {
          const selected = tab.id === activeTab;
          return (
            <DropdownMenuRoot key={tab.id} open={menuFor === tab.id} onOpenChange={open => {
              setMenuFor(current => open ? tab.id : current === tab.id ? null : current);
            }} modal={false}>
              <div
                className={styles.tabItem}
                role="presentation"
                data-tab-item=""
                data-selected={selected ? "" : undefined}
                data-menu-open={menuFor === tab.id ? "" : undefined}
                data-dragging={drag?.id === tab.id ? "" : undefined}
                onDragOver={event => onDragOver(event, tab)}
                onDrop={event => onDrop(event, tab)}
              >
                <DropdownMenuTrigger asChild>
                    <TabTrigger
                      ref={node => { if (node) tabButtons.current.set(tab.id, node); else tabButtons.current.delete(tab.id); }}
                      type="button"
                      role="tab"
                      aria-label={tab.name}
                      aria-selected={selected}
                      aria-keyshortcuts={canEdit ? "ArrowLeft ArrowRight Home End ArrowDown Shift+F10 F2" : "ArrowLeft ArrowRight Home End ArrowDown Shift+F10"}
                      tabIndex={selected && renaming !== tab.id ? 0 : -1}
                      className={styles.tab}
                      data-renaming={renaming === tab.id ? "" : undefined}
                      data-sq="surface"
                      data-testid={`tab-${tab.name}`}
                      draggable={canEdit}
                      onClick={() => openMenu(tab.id)}
                      onKeyDown={event => onTabKeyDown(event, tab)}
                      onDoubleClick={() => startRename(tab.id)}
                      onContextMenu={event => { event.preventDefault(); openMenu(tab.id); }}
                      onDragStart={event => {
                        if (!canEdit) { event.preventDefault(); return; }
                        menuReturnFocus.current.clear();
                        setMenuFor(null);
                        event.dataTransfer.effectAllowed = "move";
                        event.dataTransfer.setData("text/plain", tab.name);
                        setDrag({ id: tab.id, over: null, side: "before" });
                      }}
                      onDragEnd={() => setDrag(null)}
                    >
                      {tab.code ? <span className={styles.tabCode} aria-hidden="true"><Braces size={14} strokeWidth={1.75} /></span> : null}
                      <span className={styles.tabName}>{tab.name}</span>
                      <ChevronDown className={styles.tabChevron} data-tab-chevron="" size={15} strokeWidth={1.75} aria-hidden="true" />
                    </TabTrigger>
                </DropdownMenuTrigger>
                {drag?.over === tab.id ? <span className={styles.tabDrop} data-side={drag.side} aria-hidden="true" /> : null}
                {tab.color ? <span className={styles.tabColor} style={{ ["--tab-color" as string]: tab.color }} aria-hidden="true" /> : null}
              </div>
              <DropdownMenuContent
                className={styles.tabActions}
                side="top"
                align="start"
                onInteractOutside={() => { menuReturnFocus.current.delete(tab.id); }}
                onCloseAutoFocus={event => {
                  event.preventDefault();
                  // Radix defers this callback. A menu reopened in the meantime already owns focus.
                  if (tabButtons.current.get(tab.id)?.getAttribute("aria-expanded") === "true") return;
                  menuReturnFocus.current.get(tab.id)?.focus({ preventScroll: true });
                  menuReturnFocus.current.delete(tab.id);
                }}
              >
                <DropdownMenuItem icon={<Pencil size={16} strokeWidth={1.75} />} shortcut="F2" disabled={!canEdit} onSelect={() => startRename(tab.id)}>Rename</DropdownMenuItem>
                <DropdownMenuSub>
                  <DropdownMenuSubTrigger icon={<Palette size={16} strokeWidth={1.75} />} disabled={!canEdit}>Colour</DropdownMenuSubTrigger>
                  <DropdownMenuSubContent>
                    <DropdownMenuItem disabled={!canEdit} onSelect={() => { if (canEdit) controller.colorTab(tab.id, null); }}>No colour</DropdownMenuItem>
                    {TAB_COLORS.map(color => (
                      <DropdownMenuItem key={color} disabled={!canEdit} icon={<span style={{ display: "block", width: 14, height: 14, borderRadius: 4, background: color }} />} onSelect={() => { if (canEdit) controller.colorTab(tab.id, color); }}>
                        {COLOR_NAMES[color] ?? color}
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuSubContent>
                </DropdownMenuSub>
                <DropdownMenuItem icon={<ArrowLeft size={16} strokeWidth={1.75} />} disabled={!canEdit || tab.index === 0} onSelect={() => move(tab, -1)}>Move left</DropdownMenuItem>
                <DropdownMenuItem icon={<ArrowRight size={16} strokeWidth={1.75} />} disabled={!canEdit || tab.index >= tabs.length - 1} onSelect={() => move(tab, 1)}>Move right</DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem destructive icon={<Trash2 size={16} strokeWidth={1.75} />} disabled={!canEdit || tabs.length <= 1} onSelect={() => { menuReturnFocus.current.clear(); setDeleting(tab); }}>Delete tab</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuRoot>
          );
        })}
      </div>
      {/* The inline editor is outside the tablist's accessibility tree; only tabs belong to a tablist. */}
      {renamingTab ? (
        <input
          className={styles.tabRename}
          style={renameBounds}
          data-sq-native=""
          aria-label={`New name for the tab ${renamingTab.name}`}
          defaultValue={renamingTab.name}
          autoFocus
          onFocus={event => event.target.select()}
          onBlur={event => finishRename(renamingTab.id, event.target.value, false, false)}
          onKeyDown={event => {
            if (event.key === "Enter" || event.key === "Escape") {
              event.preventDefault();
              finishRename(renamingTab.id, event.currentTarget.value, event.key === "Escape", true);
            }
          }}
        />
      ) : null}
      <SelectionSummary />
      <Dialog open={!!deleting} onOpenChange={open => { if (!open) setDeleting(null); }}><DialogContent title="Delete tab" description={`Delete ${deleting?.name ?? "this tab"}? Its previous contents stay in the history.`}><HoldToConfirm tone="danger" label="Hold to delete tab" onConfirm={() => { if (deleting) remove(deleting); setDeleting(null); }} /></DialogContent></Dialog>
    </div>
  );
}

const COLOR_NAMES: Record<string, string> = {
  "#B42318": "Red",
  "#C2410C": "Orange",
  "#93560A": "Amber",
  "#18703F": "Green",
  "#17806B": "Teal",
  "#1F5FB8": "Blue",
  "#7A4FC4": "Violet",
  "#BE185D": "Pink",
};

/** Sum, average and count of the selection's loaded numbers (the cells on screen and around them). */
function SelectionSummary() {
  const { controller, session } = useEditor();
  const revision = useControllerRevision(controller);
  const sessionRevision = useSessionRevision(session);
  const summary = useMemo(() => {
    void revision;
    void sessionRevision;
    const data = controller.data;
    const r = controller.selection.range;
    if (!data || (r.r1 === r.r2 && r.c1 === r.c2)) return null;
    let sum = 0;
    let numbers = 0;
    let values = 0;
    let scanned = 0;
    for (const { cell } of data.cellsIn(r)) {
      if (++scanned > 200_000) break;
      if (cell.v.t === "empty") continue;
      values++;
      if (cell.v.t === "number") {
        sum += cell.v.v;
        numbers++;
      }
    }
    if (!values) return null;
    return { sum, numbers, values };
  }, [controller, revision, sessionRevision]);
  if (!summary) return null;
  const round = (value: number) => (Math.abs(value) >= 1e15 ? value.toExponential(4) : new Intl.NumberFormat("en-US", { maximumFractionDigits: 6 }).format(value));
  return (
    <p className={styles.summary} aria-live="polite">
      {summary.numbers ? (
        <>
          Sum <strong>{round(summary.sum)}</strong> · Average <strong>{round(summary.sum / summary.numbers)}</strong> ·{" "}
        </>
      ) : null}
      Count <strong>{formatCount(summary.values)}</strong>
    </p>
  );
}
