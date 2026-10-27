"use client";

/**
 * The grid's context menu (right click, long press, the context menu key): cut, copy and paste, inserting and
 * deleting rows and columns, sorting, hiding and showing, clearing, commenting, protecting and the cell's history.
 * What it offers follows what was under the pointer: cells, row headers or column headers.
 */
import {
  ArrowDownWideNarrow, ArrowUpNarrowWide, BetweenHorizontalStart, BetweenVerticalStart, ClipboardPaste, Copy, Eraser, Eye, EyeOff, History, Lock, MessageSquarePlus,
  Scissors, Trash2,
} from "lucide-react";
import {
  DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger, DropdownMenuTrigger,
} from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { colName } from "@/lib/sheet/address";
import { useEditor } from "./editor-context";
import styles from "./editor.module.css";

export interface GridMenuState {
  x: number;
  y: number;
  area: "cell" | "row-header" | "col-header" | "corner";
}

const I = { size: 16, strokeWidth: 1.75 } as const;

/** Copies or cuts through the grid's own handler (it writes TSV and HTML). */
function clipboardCommand(focusGrid: (() => void) | null, command: "copy" | "cut"): boolean {
  focusGrid?.();
  try {
    return document.execCommand(command);
  } catch {
    return false;
  }
}

export function GridMenu({ state, onClose, onProtect }: { state: GridMenuState | null; onClose: () => void; onProtect: () => void }) {
  const { controller, canEdit, canComment, isOwner, startComment, openPanel } = useEditor();
  const sel = controller.selection;
  const r = sel.range;
  const rows = r.r2 - r.r1 + 1;
  const cols = r.c2 - r.c1 + 1;
  const data = controller.data;
  const rowsWord = (n: number) => (n === 1 ? "row" : `${n.toLocaleString("en-US")} rows`);
  const colsWord = (n: number) => (n === 1 ? "column" : `${n.toLocaleString("en-US")} columns`);
  const wholeRows = sel.mode === "rows" || state?.area === "row-header";
  const wholeCols = sel.mode === "cols" || state?.area === "col-header";
  const hiddenNearRows = !!data && (data.rows.isHidden(r.r1 - 1) || data.rows.isHidden(r.r2 + 1) || [...Array(Math.min(rows, 500)).keys()].some(i => data.rows.isHidden(r.r1 + i)));
  const hiddenNearCols = !!data && (data.cols.isHidden(r.c1 - 1) || data.cols.isHidden(r.c2 + 1) || [...Array(Math.min(cols, 500)).keys()].some(i => data.cols.isHidden(r.c1 + i)));
  const colRange = r.c1 === r.c2 ? colName(r.c1) : `${colName(r.c1)}–${colName(r.c2)}`;

  const paste = async () => {
    controller.focusGrid?.();
    try {
      const text = await navigator.clipboard.readText();
      const transfer = new DataTransfer();
      transfer.setData("text/plain", text);
      controller.paste(new ClipboardEvent("paste", { clipboardData: transfer }));
    } catch {
      controller.notice({ kind: "info", title: "Paste with the keyboard", description: "This browser lets pages read the clipboard only from the keyboard: press Ctrl+V (⌘V on a Mac)." });
    }
  };

  return (
    <DropdownMenuRoot open={state !== null} onOpenChange={open => { if (!open) onClose(); }} modal={false}>
      <DropdownMenuTrigger asChild>
        <span className={styles.menuAnchor} style={{ left: state?.x ?? 0, top: state?.y ?? 0 }} aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="bottom" sideOffset={2} aria-label="Cell actions" onCloseAutoFocus={event => { event.preventDefault(); controller.focusGrid?.(); }}>
        <DropdownMenuItem icon={<Scissors {...I} />} disabled={!canEdit} onSelect={() => { if (!clipboardCommand(controller.focusGrid, "cut")) controller.notice({ kind: "info", title: "Cut with the keyboard", description: "Press Ctrl+X (⌘X on a Mac)." }); }}>Cut</DropdownMenuItem>
        <DropdownMenuItem icon={<Copy {...I} />} onSelect={() => { if (!clipboardCommand(controller.focusGrid, "copy")) controller.notice({ kind: "info", title: "Copy with the keyboard", description: "Press Ctrl+C (⌘C on a Mac)." }); }}>Copy</DropdownMenuItem>
        <DropdownMenuItem icon={<ClipboardPaste {...I} />} disabled={!canEdit} onSelect={() => void paste()}>Paste</DropdownMenuItem>
        <DropdownMenuSeparator />
        {!wholeCols ? (
          <>
            <DropdownMenuItem icon={<BetweenHorizontalStart {...I} />} disabled={!canEdit} onSelect={() => controller.insert("rows", "before")}>Insert {rowsWord(rows)} above</DropdownMenuItem>
            <DropdownMenuItem icon={<BetweenHorizontalStart {...I} />} disabled={!canEdit} onSelect={() => controller.insert("rows", "after")}>Insert {rowsWord(rows)} below</DropdownMenuItem>
          </>
        ) : null}
        {!wholeRows ? (
          <>
            <DropdownMenuItem icon={<BetweenVerticalStart {...I} />} disabled={!canEdit} onSelect={() => controller.insert("cols", "before")}>Insert {colsWord(cols)} left</DropdownMenuItem>
            <DropdownMenuItem icon={<BetweenVerticalStart {...I} />} disabled={!canEdit} onSelect={() => controller.insert("cols", "after")}>Insert {colsWord(cols)} right</DropdownMenuItem>
          </>
        ) : null}
        {!wholeCols ? <DropdownMenuItem icon={<Trash2 {...I} />} disabled={!canEdit} onSelect={() => controller.remove("rows")}>Delete {rows === 1 ? `row ${r.r1 + 1}` : `rows ${r.r1 + 1}–${r.r2 + 1}`}</DropdownMenuItem> : null}
        {!wholeRows ? <DropdownMenuItem icon={<Trash2 {...I} />} disabled={!canEdit} onSelect={() => controller.remove("cols")}>Delete {cols === 1 ? `column ${colRange}` : `columns ${colRange}`}</DropdownMenuItem> : null}
        <DropdownMenuSeparator />
        <DropdownMenuSub>
          <DropdownMenuSubTrigger icon={<ArrowUpNarrowWide {...I} />} disabled={!canEdit}>Sort</DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            <DropdownMenuItem icon={<ArrowUpNarrowWide {...I} />} onSelect={() => controller.sort(true)}>{wholeCols ? `Sort the tab by ${colName(sel.active.col)}, A to Z` : `Sort the range by ${colName(sel.active.col)}, A to Z`}</DropdownMenuItem>
            <DropdownMenuItem icon={<ArrowDownWideNarrow {...I} />} onSelect={() => controller.sort(false)}>{wholeCols ? `Sort the tab by ${colName(sel.active.col)}, Z to A` : `Sort the range by ${colName(sel.active.col)}, Z to A`}</DropdownMenuItem>
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        {wholeRows ? <DropdownMenuItem icon={<EyeOff {...I} />} disabled={!canEdit} onSelect={() => controller.hide("rows", false)}>Hide {rows === 1 ? "row" : "rows"}</DropdownMenuItem> : null}
        {wholeRows && hiddenNearRows ? <DropdownMenuItem icon={<Eye {...I} />} disabled={!canEdit} onSelect={() => controller.hide("rows", true)}>Show hidden rows</DropdownMenuItem> : null}
        {wholeCols ? <DropdownMenuItem icon={<EyeOff {...I} />} disabled={!canEdit} onSelect={() => controller.hide("cols", false)}>Hide {cols === 1 ? "column" : "columns"}</DropdownMenuItem> : null}
        {wholeCols && hiddenNearCols ? <DropdownMenuItem icon={<Eye {...I} />} disabled={!canEdit} onSelect={() => controller.hide("cols", true)}>Show hidden columns</DropdownMenuItem> : null}
        <DropdownMenuSub>
          <DropdownMenuSubTrigger icon={<Eraser {...I} />} disabled={!canEdit}>Clear</DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            <DropdownMenuItem onSelect={() => controller.clearSelection("contents")}>Contents</DropdownMenuItem>
            <DropdownMenuItem onSelect={() => controller.clearSelection("formats")}>Formats</DropdownMenuItem>
            <DropdownMenuItem onSelect={() => controller.clearSelection("all")}>Contents and formats</DropdownMenuItem>
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuSeparator />
        <DropdownMenuItem icon={<MessageSquarePlus {...I} />} disabled={!canComment} onSelect={startComment}>Comment</DropdownMenuItem>
        {isOwner ? <DropdownMenuItem icon={<Lock {...I} />} disabled={!canEdit} onSelect={onProtect}>Protect {wholeRows ? "rows" : wholeCols ? "columns" : "range"}</DropdownMenuItem> : null}
        {sel.mode === "cells" && r.r1 === r.r2 && r.c1 === r.c2 ? <DropdownMenuItem icon={<History {...I} />} onSelect={() => openPanel("cell")}>Cell history</DropdownMenuItem> : null}
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}
