/**
 * The sheet editor's state outside React: which tab is open, the selection, the cell editor (its text, caret, whether
 * it was opened by typing or to edit, the reference being pointed at), the copy outline and what this browser last
 * copied, a compare overlay, and every change the toolbar, menus and keys make, sent through the line's session.
 *
 * Components read it with useController (lib/sheet/react.ts); the canvas redraws on `subscribe`.
 */
import { ApiError } from "../errors";
import type { Account, CellControl, Edit, Style, StylePatch, Tab } from "../calc/types";
import { checkboxState, checkboxValue, literalInput } from "./cell-controls";
import { a1, cellName, colName, formatRange, MAX_COLS, MAX_ROWS, parseCell, parseRange, quoteTab, type Rect, rectsIntersect, splitTab } from "./address";
import { parseHTMLTable, parseTSV, toHTML, toTSV, type HtmlCell } from "./clipboard";
import { canPointAt, isFormula, partialFunctionAt, referenceText, references, replaceText, shiftFormula, tokenize, type RefToken } from "./formula";
import { type FunctionInfo, suggestFunctions } from "./functions";
import { cellSelection, type Direction, expandToMerges, extendTo, isMulti, jump, type Selection, type SelectionGrid, selectAll, selectCell, selectCols, selectRows, step, walkInside } from "./selection";
import type { PendingSet, SheetSession } from "./session";
import { BLOCK_ROWS, type TabData } from "./store";

export type EditorMode = "enter" | "edit";

export interface EditorState {
  open: boolean;
  text: string;
  caret: number;
  /** "enter": opened by typing (arrows commit or point); "edit": opened with F2, Enter or a double click (arrows move the caret). */
  mode: EditorMode;
  /** Where typing happens: in the cell, or in the formula bar. */
  where: "cell" | "bar";
  /** The reference just pointed at (replaced as the pointer or arrows move on). */
  pointed: { start: number; end: number; rect: Rect; tab: string | null } | null;
  /** The cell being edited and its tab (fixed while the editor is open, even if the selection moves to point). */
  row: number;
  col: number;
  tab: number;
}

const CLOSED: EditorState = { open: false, text: "", caret: 0, mode: "enter", where: "cell", pointed: null, row: 0, col: 0, tab: 0 };

export interface ClipboardRecord {
  tsv: string;
  tab: number;
  range: Rect;
  /** Each cell's input as typed (formulas included), row by row. */
  inputs: string[][];
  styles: Array<Array<number | undefined>>;
  cut: boolean;
}

export interface Notice {
  kind: "error" | "info" | "success";
  title: string;
  description?: string;
}

/** A compare overlay: changed cells by tab, as the grid tints them. */
export type DiffOverlay = Map<number, Map<number, "changed" | "added" | "removed" | "conflict">>;

/** A paste or fill is refused above this many cells (the service's own limit is in GET /v1/meta). */
export const MAX_CELLS_PER_EDIT = 200_000;

type Listener = () => void;

export class SheetController {
  session: SheetSession;
  activeTab: number;
  selection: Selection = cellSelection(0, 0);
  editor: EditorState = CLOSED;
  copyRange: { tab: number; range: Rect } | null = null;
  clipboard: ClipboardRecord | null = null;
  diff: DiffOverlay | null = null;
  /** The fill handle's target while dragging. */
  fillPreview: Rect | null = null;
  revision = 0;
  /** Shows a short message (wired to the toast stack by the page). */
  notice: (notice: Notice) => void = () => undefined;
  /** Asks the grid to bring a cell into view. */
  reveal: ((row: number, col: number) => void) | null = null;
  /** Where focus goes back after a toolbar action (the grid's input). */
  focusGrid: (() => void) | null = null;

  private readonly listeners = new Set<Listener>();
  private readonly tabSelections = new Map<number, Selection>();

  constructor(session: SheetSession, activeTab?: number) {
    this.session = session;
    this.activeTab = activeTab ?? session.firstTab()?.id ?? 0;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  setNoticeHandler(handler: (notice: Notice) => void): void { this.notice = handler; }

  notify(): void {
    this.revision++;
    for (const listener of [...this.listeners]) listener();
  }

  /* ---- what the grid shows ----------------------------------------------------------------------------------------- */

  get data(): TabData | null {
    return this.session.tab(this.activeTab);
  }

  get tab(): Tab | null {
    return this.session.tabs.find(tab => tab.id === this.activeTab) ?? null;
  }

  get canEdit(): boolean {
    return !this.session.readOnly && this.session.atVersion === null;
  }

  grid(): SelectionGrid {
    const data = this.data;
    return {
      rowCount: data?.rows.count ?? 1000,
      colCount: data?.cols.count ?? 26,
      rowHidden: row => data?.rows.isHidden(row) ?? false,
      colHidden: col => data?.cols.isHidden(col) ?? false,
      mergeAt: (row, col) => data?.mergeAt(row, col) ?? null,
      filled: (row, col) => {
        if (!data) return false;
        if (data.pendingAt(row, col)?.input) return true;
        const cell = data.get(row, col);
        if (cell) return cell.v.t !== "empty" || !!cell.f;
        return data.isLoaded(row, col) ? false : undefined;
      },
    };
  }

  /** The range text of the selection (`B2`, `A1:C9`, `A:C`, `3:5`). */
  selectionText(): string {
    const data = this.data;
    return formatRange(this.selection.range, data?.rows.count ?? MAX_ROWS, data?.cols.count ?? MAX_COLS);
  }

  /** The selection as the service reads it: whole rows and columns as `A:C` / `3:5` (all of them, not just the grid). */
  apiRange(r: Rect = this.selection.range): string {
    const data = this.data;
    const rows = data?.rows.count ?? MAX_ROWS;
    const cols = data?.cols.count ?? MAX_COLS;
    const wholeCols = r.r1 === 0 && r.r2 >= rows - 1;
    const wholeRows = r.c1 === 0 && r.c2 >= cols - 1;
    if (wholeCols && wholeRows) return `A:${colName(MAX_COLS - 1)}`;
    if (wholeCols) return `${colName(r.c1)}:${colName(r.c2)}`;
    if (wholeRows) return `${r.r1 + 1}:${r.r2 + 1}`;
    return a1(r);
  }

  activeStyle(): Style {
    const data = this.data;
    const cell = data?.get(this.selection.active.row, this.selection.active.col);
    return (cell?.s ? this.session.styles.get(cell.s) : undefined) ?? {};
  }

  /** The active cell's input as it would be edited: its formula, or its value. */
  activeInput(): string {
    const { row, col } = this.selection.active;
    return this.inputAt(row, col);
  }

  inputAt(row: number, col: number, tab = this.activeTab): string {
    const data = this.session.tab(tab);
    const pending = data?.pendingAt(row, col);
    if (pending) return pending.input;
    const cell = data?.get(row, col);
    if (!cell) return "";
    if (cell.f) return cell.f;
    if (cell.spill) return cell.t;
    const v = cell.v;
    if (v.t === "number") {
      // Percentages and dates edit as they show; plain numbers as their full value.
      if (/%$/.test(cell.t)) return cell.t;
      const style = cell.s ? this.session.styles.get(cell.s) : undefined;
      if (style?.num_fmt && /[dmyhs]/i.test(style.num_fmt.replace(/"[^"]*"/g, ""))) return cell.t;
      return String(v.v);
    }
    if (v.t === "text") return /^[=+\-]/.test(v.v) || /^[\d.,%$€£-]+$/.test(v.v) ? `'${v.v}` : v.v;
    if (v.t === "bool") return v.v ? "TRUE" : "FALSE";
    if (v.t === "error") return v.v;
    return "";
  }

  /** The spill area of the active cell's formula (or of the formula that spilled into it). */
  spillArea(): Rect | null {
    const data = this.data;
    if (!data) return null;
    let { row, col } = this.selection.active;
    let cell = data.get(row, col);
    if (!cell) return null;
    // From a spilled cell, walk back to the formula that wrote it.
    if (cell.spill) {
      let guard = 0;
      while (col > 0 && data.get(row, col - 1)?.spill && guard++ < 16_384) col--;
      if (col > 0 && data.get(row, col - 1)?.f) col--;
      guard = 0;
      while (row > 0 && data.get(row - 1, col)?.spill && guard++ < 100_000) row--;
      if (row > 0 && data.get(row - 1, col)?.f) row--;
      cell = data.get(row, col);
    }
    if (!cell?.f) return null;
    let c2 = col;
    while (c2 + 1 < data.cols.count && data.get(row, c2 + 1)?.spill && c2 - col < 16_384) c2++;
    let r2 = row;
    while (r2 + 1 < data.rows.count && (data.get(r2 + 1, col)?.spill || data.get(r2 + 1, c2)?.spill) && r2 - row < 1_000_000) r2++;
    if (r2 === row && c2 === col) return null;
    return { r1: row, c1: col, r2, c2 };
  }

  /* ---- tabs ------------------------------------------------------------------------------------------------------ */

  openTab(id: number): void {
    if (id === this.activeTab || !this.session.tab(id)) return;
    if (this.editor.open && !this.isPointing()) this.commit("none");
    // A formula being pointed at still owns the original tab's selection. Do not
    // store it as the selection of every tab visited while choosing a reference.
    if (!this.editor.open || this.activeTab === this.editor.tab) this.tabSelections.set(this.activeTab, this.selection);
    for (const tab of this.tabSelections.keys()) if (!this.session.tab(tab)) this.tabSelections.delete(tab);
    this.activeTab = id;
    if (!this.editor.open) this.selection = this.tabSelections.get(id) ?? cellSelection(0, 0);
    this.notify();
    this.session.setPresence(id, this.apiRange(this.selection.range));
  }

  /* ---- selection ------------------------------------------------------------------------------------------------- */

  setSelection(selection: Selection, reveal = true): void {
    this.selection = selection;
    this.notify();
    if (reveal) this.reveal?.(selection.active.row, selection.active.col);
    this.session.setPresence(this.activeTab, this.apiRange(selection.range));
  }

  clickCell(row: number, col: number, shift: boolean): void {
    const grid = this.grid();
    const data = this.data;
    if (shift) this.setSelection(extendTo(grid, this.selection, row, col, data?.merges ?? []), false);
    else this.setSelection(selectCell(grid, row, col), false);
  }

  dragTo(row: number, col: number): void {
    this.setSelection(extendTo(this.grid(), this.selection, row, col, this.data?.merges ?? []), false);
  }

  clickRowHeader(row: number, shift: boolean): void {
    const grid = this.grid();
    if (shift && this.selection.mode === "rows") this.setSelection(extendTo(grid, this.selection, row, 0, []), false);
    else this.setSelection(selectRows(grid, row, row, this.selection.active.col), false);
  }

  clickColHeader(col: number, shift: boolean): void {
    const grid = this.grid();
    if (shift && this.selection.mode === "cols") this.setSelection(extendTo(grid, this.selection, 0, col, []), false);
    else this.setSelection(selectCols(grid, col, col, this.selection.active.row), false);
  }

  selectAll(): void {
    this.setSelection(selectAll(this.grid(), this.selection.active), false);
  }

  /** Arrow keys: one step, or to the data's edge with ctrl/cmd; shift extends. */
  async move(dir: Direction, options: { jump?: boolean; extend?: boolean } = {}): Promise<void> {
    const grid = this.grid();
    const from = options.extend ? this.extentCorner(dir) : this.selection.active;
    let target = options.jump ? await this.jumpFrom(from, dir) : step(grid, from, dir);
    if (!target) return;
    const data = this.data;
    if (data) data.ensureSize(target.row + 1, target.col + 1);
    target = { row: Math.min(target.row, grid.rowCount - 1), col: Math.min(target.col, grid.colCount - 1) };
    if (options.extend) {
      const next = extendTo(grid, this.selection, target.row, target.col, data?.merges ?? []);
      this.selection = next;
      this.notify();
      this.reveal?.(target.row, target.col);
      this.session.setPresence(this.activeTab, this.apiRange(next.range));
      return;
    }
    this.setSelection(selectCell(grid, target.row, target.col));
  }

  /** The corner of the selection that moves when extending (the one opposite the anchor). */
  private extentCorner(dir: Direction): { row: number; col: number } {
    const { range, anchor } = this.selection;
    const row = dir === "up" || dir === "down" ? (anchor.row === range.r1 ? range.r2 : range.r1) : anchor.row === range.r1 ? range.r2 : range.r1;
    const col = dir === "left" || dir === "right" ? (anchor.col === range.c1 ? range.c2 : range.c1) : anchor.col === range.c1 ? range.c2 : range.c1;
    return { row, col };
  }

  /** Ctrl/cmd + arrow, loading what lies on the way when it is not loaded yet. */
  private async jumpFrom(from: { row: number; col: number }, dir: Direction): Promise<{ row: number; col: number } | null> {
    const grid = this.grid();
    const quick = jump(grid, from, dir);
    if (quick) return quick;
    const data = this.data;
    if (!data) return null;
    // Load the column (or row) ahead in growing windows of values, then decide.
    const filled = new Map<number, boolean>();
    const vertical = dir === "up" || dir === "down";
    const sign = dir === "down" || dir === "right" ? 1 : -1;
    let windowSize = 2_048;
    let start = vertical ? from.row : from.col;
    const total = vertical ? data.rows.count : data.cols.count;
    const used = data.usedRect();
    for (let round = 0; round < 6; round++) {
      const a = sign > 0 ? start : Math.max(0, start - windowSize);
      const b = sign > 0 ? Math.min(total - 1, start + windowSize) : start;
      const rect: Rect = vertical ? { r1: a, c1: from.col, r2: b, c2: from.col } : { r1: from.row, c1: a, r2: from.row, c2: b };
      try {
        const values = await this.session.values(this.activeTab, rect);
        values.forEach((line, i) => line.forEach((value, j) => filled.set(vertical ? a + i : a + j, value !== null && value !== "")));
      } catch {
        return null;
      }
      const lookup: SelectionGrid = {
        ...grid,
        filled: (row, col) => {
          const index = vertical ? row : col;
          if (filled.has(index)) return filled.get(index)!;
          const local = grid.filled(row, col);
          if (local !== undefined) return local;
          // Past the used area everything is empty.
          if (used && (vertical ? row > used.r2 : col > used.c2)) return false;
          return undefined;
        },
      };
      const found = jump(lookup, from, dir);
      if (found) return found;
      start = sign > 0 ? b : a;
      windowSize *= 4;
    }
    // Still running on: the edge of the used area.
    if (used) return vertical ? { row: sign > 0 ? used.r2 : 0, col: from.col } : { row: from.row, col: sign > 0 ? used.c2 : 0 };
    return null;
  }

  /** Page Up / Page Down by about a screen of rows. */
  page(dir: "up" | "down", rows: number, extend: boolean): void {
    const grid = this.grid();
    let pos = extend ? this.extentCorner(dir) : this.selection.active;
    for (let i = 0; i < Math.max(1, rows); i++) pos = step(grid, pos, dir);
    if (extend) this.setSelection(extendTo(grid, this.selection, pos.row, pos.col, this.data?.merges ?? []));
    else this.setSelection(selectCell(grid, pos.row, pos.col));
  }

  /** Home (start of the row), ctrl/cmd + Home (A1), ctrl/cmd + End (the last used cell). */
  home(toStart: boolean, extend: boolean): void {
    const grid = this.grid();
    const row = toStart ? 0 : this.selection.active.row;
    if (extend) this.setSelection(extendTo(grid, this.selection, row, 0, this.data?.merges ?? []));
    else this.setSelection(selectCell(grid, row, 0));
  }

  end(extend: boolean): void {
    const used = this.data?.usedRect();
    const grid = this.grid();
    const target = used ? { row: used.r2, col: used.c2 } : { row: 0, col: 0 };
    if (extend) this.setSelection(extendTo(grid, this.selection, target.row, target.col, this.data?.merges ?? []));
    else this.setSelection(selectCell(grid, target.row, target.col));
  }

  /** Enter and Tab after typing: down or right, inside a multi-cell selection when there is one. */
  advance(dir: "down" | "up" | "right" | "left"): void {
    const grid = this.grid();
    if (isMulti(this.selection, grid)) {
      this.selection = walkInside(this.selection, dir);
      this.notify();
      this.reveal?.(this.selection.active.row, this.selection.active.col);
      return;
    }
    const next = step(grid, this.selection.active, dir);
    this.data?.ensureSize(next.row + 1, next.col + 1);
    this.setSelection(selectCell(grid, next.row, next.col));
  }

  /** Jumps to an address typed in the name box (`B7`, `A1:C9`, `Sales!A1`). False when it is not one. */
  goTo(text: string): boolean {
    const { tab, ref } = splitTab(text.trim());
    if (tab) {
      const target = this.session.tabs.find(t => t.name.toLowerCase() === tab.toLowerCase());
      if (!target) return false;
      this.openTab(target.id);
    }
    const data = this.data;
    const r = parseRange(ref, data?.rows.count ?? MAX_ROWS, data?.cols.count ?? MAX_COLS);
    if (!r) return false;
    data?.ensureSize(r.r2 + 1, r.c2 + 1);
    const grid = this.grid();
    const whole = r.r1 === 0 && r.r2 >= grid.rowCount - 1 ? "cols" : r.c1 === 0 && r.c2 >= grid.colCount - 1 ? "rows" : "cells";
    this.setSelection({ anchor: { row: r.r1, col: r.c1 }, active: { row: r.r1, col: r.c1 }, range: expandToMerges(r, data?.merges ?? []), mode: whole });
    return true;
  }

  /* ---- the editor ------------------------------------------------------------------------------------------------ */

  /** Opens the editor on the active cell: with its contents (F2, Enter, double click) or with typed text. */
  startEdit(options: { text?: string; mode?: EditorMode; where?: "cell" | "bar" } = {}): boolean {
    if (!this.canEdit) {
      this.notice({ kind: "info", title: "This sheet is open read-only", description: this.readOnlyReason() });
      return false;
    }
    const { row, col } = this.selection.active;
    const data = this.data;
    const protection = data?.protectionAt(row, col);
    if (protection && !this.mayEditProtected(protection)) {
      this.notice({ kind: "error", title: "This range is protected", description: `${protection.description ? `${protection.description}. ` : ""}Only the owner and the editors they chose can change it.` });
      return false;
    }
    const text = options.text ?? this.inputAt(row, col);
    this.editor = { open: true, text, caret: text.length, mode: options.mode ?? (options.text !== undefined ? "enter" : "edit"), where: options.where ?? "cell", pointed: null, row, col, tab: this.activeTab };
    this.copyRange = null;
    this.notify();
    return true;
  }

  readOnlyReason(): string {
    if (this.session.atVersion !== null) return "You are looking at an earlier version. Go back to the current version to edit.";
    if (this.session.line.copy && this.session.readOnly) return "This copy was merged or deleted; it stays readable in the history.";
    const role = this.session.sheet.role;
    if (!this.session.me) return "Sign in to edit, if the sheet was shared with you.";
    if (role === "commenter") return "You can comment on this sheet. Ask its owner to make you an editor to change it.";
    return "You can view this sheet. Ask its owner to make you an editor to change it.";
  }

  mayEditProtected(protection: { editors: Account[] }): boolean {
    const me = this.session.me;
    if (!me) return false;
    if (this.session.sheet.role === "owner") return true;
    return protection.editors.some(editor => editor.uuid === me.uuid || editor.id === me.id);
  }

  setEditorText(text: string, caret: number): void {
    if (!this.editor.open) return;
    const pointed = this.editor.pointed && text.slice(this.editor.pointed.start, this.editor.pointed.end) === referenceText(this.editor.pointed.rect, this.editor.pointed.tab) ? this.editor.pointed : null;
    this.editor = { ...this.editor, text, caret, pointed: pointed && pointed.end === caret ? pointed : null };
    this.notify();
  }

  setEditorWhere(where: "cell" | "bar"): void {
    if (this.editor.where === where) return;
    this.editor = { ...this.editor, where, mode: "edit" };
    this.notify();
  }

  cancelEdit(): void {
    if (!this.editor.open) return;
    const { row, col, tab } = this.editor;
    this.editor = CLOSED;
    if (tab !== this.activeTab) this.activeTab = tab;
    this.setSelection(selectCell(this.grid(), row, col));
  }

  /** True while choosing a reference with the pointer or arrows. */
  isPointing(): boolean {
    return this.editor.open && canPointAt(this.editor.text, this.editor.caret, this.editor.pointed);
  }

  /** Point mode: puts (or replaces) a reference at the caret. `tab` is set when pointing at another tab. */
  point(rect: Rect): void {
    if (!this.editor.open) return;
    const tabName = this.activeTab !== this.editor.tab ? this.tab?.name ?? null : null;
    const insert = referenceText(rect, tabName);
    const { text, caret, pointed } = this.editor;
    const start = pointed && pointed.end === caret ? pointed.start : caret;
    const next = replaceText(text, start, caret, insert);
    this.editor = { ...this.editor, text: next.text, caret: next.caret, pointed: { start, end: next.caret, rect, tab: tabName } };
    this.notify();
  }

  /** Arrow keys in point mode move the pointed reference (shift grows it). */
  pointMove(dir: Direction, extend: boolean): void {
    const grid = this.grid();
    const current = this.editor.pointed;
    if (!current) {
      const from = this.activeTab === this.editor.tab ? { row: this.editor.row, col: this.editor.col } : { row: 0, col: 0 };
      const target = step(grid, from, dir);
      this.point({ r1: target.row, c1: target.col, r2: target.row, c2: target.col });
      return;
    }
    const r = current.rect;
    if (extend) {
      const corner = { row: dir === "up" || dir === "down" ? r.r2 : r.r2, col: r.c2 };
      const target = step(grid, corner, dir);
      this.point({ r1: Math.min(r.r1, target.row), c1: Math.min(r.c1, target.col), r2: Math.max(r.r1, target.row), c2: Math.max(r.c1, target.col) });
      return;
    }
    const target = step(grid, { row: r.r1, col: r.c1 }, dir);
    this.point({ r1: target.row, c1: target.col, r2: target.row, c2: target.col });
  }

  /** The references of the formula being edited, on the tab in view (for outlining them on the grid). */
  editorReferences(): Array<{ rect: Rect; tab: string | null }> {
    if (!this.editor.open || !isFormula(this.editor.text)) return [];
    const tabName = this.tab?.name.toLowerCase() ?? "";
    const editingTab = this.session.tabs.find(t => t.id === this.editor.tab)?.name.toLowerCase() ?? "";
    return references(this.editor.text)
      .filter(ref => (ref.tab ? ref.tab.toLowerCase() === tabName : editingTab === tabName))
      .map(ref => ({ rect: ref.rect, tab: ref.tab }));
  }

  /** The function name being typed (for suggestions). */
  partialFunction(): { prefix: string; start: number } | null {
    return this.editor.open ? partialFunctionAt(this.editor.text, this.editor.caret) : null;
  }

  /** The highlighted suggestion, and where the last dismissed one started (Escape hides them for that word). */
  suggestIndex = 0;
  private suggestDismissed: number | null = null;
  private suggestKey = "";

  /** Functions offered for the name being typed (none once dismissed for this word). */
  suggestions(): FunctionInfo[] {
    const partial = this.partialFunction();
    if (!partial || partial.start === this.suggestDismissed) return [];
    const items = suggestFunctions(partial.prefix, 8);
    const key = `${partial.start}:${partial.prefix}`;
    if (key !== this.suggestKey) {
      this.suggestKey = key;
      this.suggestIndex = 0;
    }
    if (this.suggestIndex >= items.length) this.suggestIndex = 0;
    return items;
  }

  moveSuggestion(delta: number): void {
    const items = this.suggestions();
    if (!items.length) return;
    this.suggestIndex = (this.suggestIndex + delta + items.length) % items.length;
    this.notify();
  }

  dismissSuggestions(): void {
    const partial = this.partialFunction();
    if (!partial) return;
    this.suggestDismissed = partial.start;
    this.notify();
  }

  /** Inserts a line break at the caret (Alt + Enter). */
  insertNewline(): void {
    if (!this.editor.open) return;
    const { text, caret } = this.editor;
    const next = replaceText(text, caret, caret, "\n");
    this.editor = { ...this.editor, text: next.text, caret: next.caret, pointed: null };
    this.notify();
  }

  /** F4: the reference at the caret cycles through $A$1, A$1, $A1 and A1. */
  cycleReference(): void {
    if (!this.editor.open || !isFormula(this.editor.text)) return;
    const { text, caret } = this.editor;
    const ref = tokenize(text).find((token): token is RefToken => token.kind === "ref" && caret >= token.start && caret <= token.end);
    if (!ref) return;
    const cycle = (part: string) => {
      const m = /^(\$?)([A-Za-z]{1,3})(\$?)(\d+)$/.exec(part);
      if (!m) return part;
      const [, colAbs, col, rowAbs, row] = m as unknown as [string, string, string, string, string];
      // $A$1 → A$1 → $A1 → A1 → $A$1
      if (colAbs && rowAbs) return `${col}$${row}`;
      if (!colAbs && rowAbs) return `$${col}${row}`;
      if (colAbs && !rowAbs) return `${col}${row}`;
      return `$${col}$${row}`;
    };
    const prefix = ref.text.slice(0, ref.text.length - ref.body.length);
    const body = ref.body.split(":").map(cycle).join(":");
    const next = replaceText(text, ref.start, ref.end, prefix + body);
    this.editor = { ...this.editor, text: next.text, caret: next.caret, pointed: null };
    this.notify();
  }

  /** Takes a suggested function: `=SU` becomes `=SUM(`. */
  acceptFunction(name: string): void {
    const partial = this.partialFunction();
    if (!partial) return;
    const next = replaceText(this.editor.text, partial.start, this.editor.caret, `${name}(`);
    this.editor = { ...this.editor, text: next.text, caret: next.caret, pointed: null };
    this.notify();
  }

  /** Takes the highlighted suggestion; false when none is offered. */
  acceptSuggestion(): boolean {
    const items = this.suggestions();
    const item = items[this.suggestIndex];
    if (!item) return false;
    this.acceptFunction(item.name);
    return true;
  }

  /**
   * Commits the editor's text into its cell and moves on. `fillSelection` (ctrl/cmd + Enter) writes it into every
   * selected cell, formulas moved relative to each.
   */
  commit(moveAfter: "down" | "up" | "right" | "left" | "none", fillSelection = false): void {
    if (!this.editor.open) return;
    const { row, col, tab } = this.editor;
    const input = this.editor.text;
    const original = this.inputAt(row, col, tab);
    this.editor = CLOSED;
    if (tab !== this.activeTab) this.activeTab = tab;
    const target = this.selection;
    this.setSelection(fillSelection ? target : selectCell(this.grid(), row, col), false);
    const data = this.session.tab(tab);
    if (fillSelection && isMulti(target, this.grid())) {
      const cells: Array<{ a: string; input: string }> = [];
      const pending: PendingSet[] = [];
      const r = target.range;
      const used = data?.usedRect();
      const r2 = Math.min(r.r2, Math.max(used?.r2 ?? r.r1, r.r1 + 9_999));
      const c2 = Math.min(r.c2, Math.max(used?.c2 ?? r.c1, r.c1 + 255));
      for (let rr = r.r1; rr <= r2; rr++) {
        for (let cc = r.c1; cc <= c2; cc++) {
          const text = isFormula(input) ? shiftFormula(input, rr - row, cc - col) : input;
          cells.push({ a: cellName(rr, cc), input: text });
          pending.push({ tab, row: rr, col: cc, input: text });
        }
      }
      void this.send([{ type: "set", tab, cells }], pending);
      return;
    }
    if (input !== original) void this.send([{ type: "set", tab, cells: [{ a: cellName(row, col), input }] }], [{ tab, row, col, input }]);
    if (moveAfter !== "none") this.advance(moveAfter);
    else this.reveal?.(row, col);
  }

  /* ---- sending changes --------------------------------------------------------------------------------------------- */

  /** Sends edits; failures toast in the service's words. Resolves true when they were applied. */
  async send(edits: Edit[], pending: PendingSet[] = []): Promise<boolean> {
    if (!this.canEdit) {
      this.notice({ kind: "info", title: "This sheet is open read-only", description: this.readOnlyReason() });
      return false;
    }
    const outcome = await this.session.edit(edits, pending);
    return outcome.error === null;
  }

  private range(): string {
    return this.apiRange(this.selection.range);
  }

  clearSelection(what: "contents" | "formats" | "all" = "contents"): void {
    const pending = this.pendingForArea(this.selection.range, "");
    void this.send([{ type: "clear", tab: this.activeTab, range: this.range(), what }], what === "formats" ? [] : pending);
  }

  /** Blank pending inputs for the loaded cells of an area (so a clear shows at once). */
  private pendingForArea(r: Rect, input: string): PendingSet[] {
    const data = this.data;
    if (!data) return [];
    const out: PendingSet[] = [];
    for (const { row, col, cell } of data.cellsIn(r)) {
      if (cell.v.t !== "empty" || cell.f) out.push({ tab: this.activeTab, row, col, input });
      if (out.length > 5_000) break;
    }
    return out;
  }

  style(patch: StylePatch): void {
    void this.send([{ type: "style", tab: this.activeTab, range: this.range(), patch }]);
  }

  toggle(part: "bold" | "italic" | "underline" | "strike"): void {
    const current = !!this.activeStyle()[part];
    this.style({ [part]: !current } as StylePatch);
  }

  setNumberFormat(code: string | null): void {
    this.style({ num_fmt: code });
  }

  setCellControl(control: CellControl | null): Promise<boolean> {
    return this.send([{ type: "style", tab: this.activeTab, range: this.range(), patch: { control } }]);
  }

  /** Uses literal values: a dropdown option starting with '=' must never execute as a formula. */
  setControlValue(row: number, col: number, value: boolean | string | number): void {
    const cell = this.data?.get(row, col);
    if (!this.canEdit || cell?.f || cell?.spill) return;
    const tab = this.activeTab;
    void this.send([{ type: "values", tab, at: cellName(row, col), rows: [[value]] }], [{ tab, row, col, input: literalInput(value) }]);
  }

  toggleCheckbox(row = this.selection.active.row, col = this.selection.active.col): boolean {
    const cell = this.data?.get(row, col);
    const control = cell?.s ? this.session.styles.get(cell.s)?.control : undefined;
    if (control?.kind !== "checkbox") return false;
    const state = checkboxState(control, cell?.v, this.data?.pendingAt(row, col)?.input);
    this.setControlValue(row, col, checkboxValue(control, state !== true));
    return true;
  }

  /** Borders: all, outer, inner, top, bottom, left, right, none. */
  borders(kind: "all" | "outer" | "inner" | "top" | "bottom" | "left" | "right" | "none", border: { style: "thin" | "medium" | "thick" | "dashed" | "dotted" | "double"; color?: string } = { style: "thin", color: "#000000" }): void {
    const r = this.selection.range;
    const tab = this.activeTab;
    const edits: Edit[] = [];
    const area = (rect: Rect) => this.apiRange(rect);
    if (kind === "none") {
      edits.push({ type: "style", tab, range: area(r), patch: { border_top: null, border_bottom: null, border_left: null, border_right: null } });
    } else if (kind === "all") {
      edits.push({ type: "style", tab, range: area(r), patch: { border_top: border, border_bottom: border, border_left: border, border_right: border } });
    } else if (kind === "inner") {
      if (r.r2 > r.r1) edits.push({ type: "style", tab, range: area({ ...r, r2: r.r2 - 1 }), patch: { border_bottom: border } });
      if (r.c2 > r.c1) edits.push({ type: "style", tab, range: area({ ...r, c2: r.c2 - 1 }), patch: { border_right: border } });
    } else {
      const sides = kind === "outer" ? (["top", "bottom", "left", "right"] as const) : ([kind] as const);
      for (const side of sides) {
        const edge = side === "top" ? { ...r, r2: r.r1 } : side === "bottom" ? { ...r, r1: r.r2 } : side === "left" ? { ...r, c2: r.c1 } : { ...r, c1: r.c2 };
        edits.push({ type: "style", tab, range: area(edge), patch: { [`border_${side}`]: border } as StylePatch });
      }
    }
    if (edits.length) void this.send(edits);
  }

  mergeBy(axis: "rows" | "cols"): void {
    const r = this.selection.range;
    const count = axis === "rows" ? r.r2-r.r1+1 : r.c2-r.c1+1;
    if (count > 1000) { this.notice({kind:"error",title:"Select at most 1,000 rows or columns to merge at once."}); return; }
    const edits: Edit[] = (this.data?.merges.filter(m=>rectsIntersect(m,r)) ?? []).map(m=>({type:"unmerge",tab:this.activeTab,range:a1(m)}));
    for(let i=0;i<count;i++) edits.push({type:"merge",tab:this.activeTab,range:a1(axis === "rows" ? {...r,r1:r.r1+i,r2:r.r1+i} : {...r,c1:r.c1+i,c2:r.c1+i})});
    void this.send(edits);
  }

  merge(): void {
    const r = this.selection.range;
    const data = this.data;
    const existing = data?.merges.filter(m => rectsIntersect(m, r)) ?? [];
    if (existing.length) {
      void this.send(existing.map(m => ({ type: "unmerge" as const, tab: this.activeTab, range: a1(m) })));
      return;
    }
    if (r.r1 === r.r2 && r.c1 === r.c2) return;
    void this.send([{ type: "merge", tab: this.activeTab, range: a1(r) }]);
  }

  freeze(rows: number | null, cols: number | null): void {
    const tab = this.tab;
    if (!tab) return;
    void this.send([{ type: "tab_props", tab: this.activeTab, ...(rows !== null ? { frozen_rows: rows } : {}), ...(cols !== null ? { frozen_cols: cols } : {}) }]);
  }

  insert(axis: "rows" | "cols", where: "before" | "after"): void {
    const r = this.selection.range;
    const count = axis === "rows" ? r.r2 - r.r1 + 1 : r.c2 - r.c1 + 1;
    const at = axis === "rows" ? (where === "before" ? r.r1 : r.r2 + 1) : where === "before" ? r.c1 : r.c2 + 1;
    void this.send([{ type: axis === "rows" ? "insert_rows" : "insert_cols", tab: this.activeTab, at, count: Math.min(count, 10_000) }]);
  }

  remove(axis: "rows" | "cols"): void {
    const r = this.selection.range;
    const at = axis === "rows" ? r.r1 : r.c1;
    const count = axis === "rows" ? r.r2 - r.r1 + 1 : r.c2 - r.c1 + 1;
    void this.send([{ type: axis === "rows" ? "delete_rows" : "delete_cols", tab: this.activeTab, at, count }]).then(ok => {
      if (ok) this.setSelection(selectCell(this.grid(), axis === "rows" ? r.r1 : this.selection.active.row, axis === "cols" ? r.c1 : this.selection.active.col));
    });
  }

  hide(axis: "rows" | "cols", show: boolean): void {
    const r = this.selection.range;
    const range = axis === "rows" ? `${r.r1 + 1}:${r.r2 + 1}` : `${colName(r.c1)}:${colName(r.c2)}`;
    const type = show ? (axis === "rows" ? "show_rows" : "show_cols") : axis === "rows" ? "hide_rows" : "hide_cols";
    void this.send([{ type, tab: this.activeTab, range }]);
  }

  /** Shows hidden rows or columns next to a header (the mark between two headers). */
  unhideAround(axis: "rows" | "cols", index: number): void {
    const data = this.data;
    if (!data) return;
    const sizes = axis === "rows" ? data.rows : data.cols;
    let end = index + 1;
    while (end < sizes.count && sizes.isHidden(end)) end++;
    if (end === index + 1) return;
    const range = axis === "rows" ? `${index + 2}:${end}` : `${colName(index + 1)}:${colName(end - 1)}`;
    void this.send([{ type: axis === "rows" ? "show_rows" : "show_cols", tab: this.activeTab, range }]);
  }

  /** Sorts the selection's rows (or the used area under a column) by the active column. */
  sort(ascending: boolean): void {
    const data = this.data;
    let r = this.selection.range;
    const used = data?.usedRect();
    if (this.selection.mode === "cols" || this.selection.mode === "all") {
      if (!used) return;
      r = { r1: 0, c1: 0, r2: used.r2, c2: used.c2 };
    }
    const header = this.selection.mode === "cols" || this.selection.mode === "all" ? (data?.frozenRows ?? 0) > 0 : false;
    void this.send([{ type: "sort", tab: this.activeTab, range: a1(r), by: [{ col: colName(this.selection.active.col), asc: ascending }], header }]);
  }

  /** The fill handle dropped on `to` (a rectangle that contains the selection). */
  fill(to: Rect): void {
    const from = this.selection.range;
    if (to.r1 === from.r1 && to.c1 === from.c1 && to.r2 === from.r2 && to.c2 === from.c2) return;
    let target: Rect;
    if (to.r2 > from.r2) target = { r1: from.r2 + 1, c1: from.c1, r2: to.r2, c2: from.c2 };
    else if (to.r1 < from.r1) target = { r1: to.r1, c1: from.c1, r2: from.r1 - 1, c2: from.c2 };
    else if (to.c2 > from.c2) target = { r1: from.r1, c1: from.c2 + 1, r2: from.r2, c2: to.c2 };
    else target = { r1: from.r1, c1: to.c1, r2: from.r2, c2: from.c1 - 1 };
    void this.send([{ type: "fill", tab: this.activeTab, from: a1(from), to: a1(target) }]).then(ok => {
      if (ok) this.setSelection({ ...this.selection, range: { r1: Math.min(from.r1, target.r1), c1: Math.min(from.c1, target.c1), r2: Math.max(from.r2, target.r2), c2: Math.max(from.c2, target.c2) } }, false);
    });
  }

  /** Ctrl/cmd + D and R: fill the selection down or right from its first row or column. */
  fillFrom(dir: "down" | "right"): void {
    const r = this.selection.range;
    if (dir === "down" && r.r2 > r.r1) void this.send([{ type: "fill", tab: this.activeTab, from: a1({ ...r, r2: r.r1 }), to: a1({ ...r, r1: r.r1 + 1 }) }]);
    if (dir === "right" && r.c2 > r.c1) void this.send([{ type: "fill", tab: this.activeTab, from: a1({ ...r, c2: r.c1 }), to: a1({ ...r, c1: r.c1 + 1 }) }]);
  }

  colWidth(cols: [number, number], width: number | null): void {
    void this.send([{ type: "col_width", tab: this.activeTab, cols: `${colName(cols[0])}:${colName(cols[1])}`, width: width === null ? null : Math.round(width) }]);
  }

  rowHeight(rows: [number, number], height: number | null): void {
    void this.send([{ type: "row_height", tab: this.activeTab, rows: `${rows[0] + 1}:${rows[1] + 1}`, height: height === null ? null : Math.round(height) }]);
  }

  /** The width that fits a column's loaded text (double click on a header edge). */
  fitWidth(col: number, measure: (text: string, style: Style | undefined) => number): number {
    const data = this.data;
    if (!data) return 100;
    let widest = 0;
    for (const { cell } of data.cellsIn({ r1: 0, c1: col, r2: data.rows.count - 1, c2: col })) {
      const style = cell.s ? this.session.styles.get(cell.s) : undefined;
      if (style?.wrap) continue;
      widest = Math.max(widest, measure(cell.t, style));
    }
    return Math.max(40, Math.min(800, Math.ceil(widest + 12)));
  }

  /* ---- tabs ------------------------------------------------------------------------------------------------------ */

  async addTab(name?: string): Promise<void> {
    const base = name ?? "Tab";
    let n = this.session.tabs.length + 1;
    let candidate = name ?? `${base} ${n}`;
    const taken = new Set(this.session.tabs.map(tab => tab.name.toLowerCase()));
    while (taken.has(candidate.toLowerCase())) candidate = `${base} ${++n}`;
    const ok = await this.send([{ type: "add_tab", name: candidate, index: this.session.tabs.length }]);
    if (!ok) return;
    await this.session.refreshTabs().catch(() => undefined);
    const created = this.session.tabs.find(tab => tab.name === candidate);
    if (created) this.openTab(created.id);
  }

  renameTab(id: number, name: string): void {
    const clean = name.trim();
    if (!clean || this.session.tabs.find(t => t.id === id)?.name === clean) return;
    void this.send([{ type: "rename_tab", tab: id, name: clean }]).then(() => this.session.refreshTabs().catch(() => undefined));
  }

  moveTab(id: number, index: number): void {
    void this.send([{ type: "move_tab", tab: id, index }]).then(() => this.session.refreshTabs().catch(() => undefined));
  }

  colorTab(id: number, color: string | null): void {
    void this.send([{ type: "tab_props", tab: id, color }]).then(() => this.session.refreshTabs().catch(() => undefined));
  }

  deleteTab(id: number): void {
    if (this.session.tabs.length <= 1) {
      this.notice({ kind: "info", title: "A sheet keeps at least one tab", description: "Add another tab first, then delete this one." });
      return;
    }
    const index = this.session.tabs.findIndex(t => t.id === id);
    void this.send([{ type: "delete_tab", tab: id }]).then(async ok => {
      if (!ok) return;
      await this.session.refreshTabs().catch(() => undefined);
      this.tabSelections.delete(id);
      if (this.editor.open && this.editor.tab === id) this.editor = CLOSED;
      if (this.activeTab === id) {
        const next = this.session.tabs[Math.max(0, index - 1)] ?? this.session.tabs[0];
        if (next) this.openTab(next.id);
      }
    });
  }

  /* ---- clipboard ------------------------------------------------------------------------------------------------- */

  /** Copies the selection as TSV and HTML (shown text) and remembers its inputs for pasting back into Calc. */
  copy(event: ClipboardEvent, cut = false): void {
    const data = this.data;
    if (!data || !event.clipboardData) return;
    const used = data.usedRect();
    const r = this.selection.range;
    // Whole rows and columns copy the part that has data.
    const area: Rect = { r1: r.r1, c1: r.c1, r2: Math.min(r.r2, Math.max(r.r1, used?.r2 ?? r.r1)), c2: Math.min(r.c2, Math.max(r.c1, used?.c2 ?? r.c1)) };
    const cells = (area.r2 - area.r1 + 1) * (area.c2 - area.c1 + 1);
    if (cells > MAX_CELLS_PER_EDIT) {
      event.preventDefault();
      this.notice({ kind: "error", title: "That is too much to copy at once", description: `The selection holds ${cells.toLocaleString("en-US")} cells; copy at most ${MAX_CELLS_PER_EDIT.toLocaleString("en-US")}, or download the tab instead.` });
      return;
    }
    const shown: string[][] = [];
    const inputs: string[][] = [];
    const styles: Array<Array<number | undefined>> = [];
    const html: HtmlCell[][] = [];
    for (let row = area.r1; row <= area.r2; row++) {
      if (data.rows.isHidden(row)) continue;
      const shownRow: string[] = [];
      const inputRow: string[] = [];
      const styleRow: Array<number | undefined> = [];
      const htmlRow: HtmlCell[] = [];
      for (let col = area.c1; col <= area.c2; col++) {
        if (data.cols.isHidden(col)) continue;
        const cell = data.get(row, col);
        const pending = data.pendingAt(row, col)?.input;
        const text = pending ?? cell?.t ?? "";
        shownRow.push(text);
        inputRow.push(this.inputAt(row, col));
        styleRow.push(cell?.s);
        htmlRow.push({ text, style: cell?.s ? this.session.styles.get(cell.s) : null, formula: cell?.f });
      }
      shown.push(shownRow);
      inputs.push(inputRow);
      styles.push(styleRow);
      html.push(htmlRow);
    }
    const tsv = toTSV(shown);
    event.clipboardData.setData("text/plain", tsv);
    event.clipboardData.setData("text/html", toHTML(html));
    event.preventDefault();
    this.clipboard = { tsv, tab: this.activeTab, range: area, inputs, styles, cut };
    this.copyRange = { tab: this.activeTab, range: area };
    this.notify();
    const missing = cells > 0 && [...Array(area.r2 - area.r1 + 1).keys()].some(i => !data.isLoaded(area.r1 + i * 1, area.c1));
    if (missing && cells > 4_096) this.notice({ kind: "info", title: "Copied what is loaded", description: "Scroll through the rest first, or download the tab to take all of it." });
  }

  /** Pastes TSV or HTML from Excel, Google Sheets or Calc at the active cell (or fills the selection with one value). */
  paste(event: ClipboardEvent): void {
    if (!this.canEdit) {
      this.notice({ kind: "info", title: "This sheet is open read-only", description: this.readOnlyReason() });
      return;
    }
    const data = event.clipboardData;
    if (!data) return;
    event.preventDefault();
    const text = data.getData("text/plain");
    const html = data.getData("text/html");
    const at = { row: this.selection.range.r1, col: this.selection.range.c1 };
    const own = this.clipboard && text.replace(/\r\n/g, "\n") === this.clipboard.tsv ? this.clipboard : null;
    let block: string[][];
    let patches: Array<Array<StylePatch | null>> | null = null;
    let merges: Rect[] = [];
    if (own) {
      const dr = at.row - own.range.r1;
      const dc = at.col - own.range.c1;
      block = own.inputs.map(row => row.map(input => (isFormula(input) && !own.cut ? shiftFormula(input, dr, dc) : input)));
    } else {
      const table = html ? parseHTMLTable(html) : null;
      if (table) {
        block = table.rows.map(row => row.map(cell => cell.formula ?? (cell.text.startsWith("=") ? `'${cell.text}` : cell.text)));
        patches = table.rows.map(row => row.map(cell => cell.style));
        merges = table.merges.map(m => ({ r1: m.r1 + at.row, c1: m.c1 + at.col, r2: m.r2 + at.row, c2: m.c2 + at.col }));
      } else {
        block = parseTSV(text);
      }
    }
    if (!block.length || !block[0]!.length) return;
    // One value pasted over a multi-cell selection fills it (as Excel does).
    const sel = this.selection.range;
    if (block.length === 1 && block[0]!.length === 1 && isMulti(this.selection, this.grid())) {
      const value = block[0]![0]!;
      const rows = Math.min(sel.r2 - sel.r1 + 1, 5_000);
      const cols = Math.min(sel.c2 - sel.c1 + 1, 200);
      block = Array.from({ length: rows }, (_, i) => Array.from({ length: cols }, (_, j) => (isFormula(value) ? shiftFormula(value, i, j) : value)));
    }
    const rows = block.length;
    const cols = Math.max(...block.map(row => row.length));
    if (rows * cols > MAX_CELLS_PER_EDIT) {
      this.notice({ kind: "error", title: "That is too much to paste at once", description: `The clipboard holds ${(rows * cols).toLocaleString("en-US")} cells; paste at most ${MAX_CELLS_PER_EDIT.toLocaleString("en-US")} at a time, or open the file as a new tab.` });
      return;
    }
    const tab = this.activeTab;
    const target: Rect = { r1: at.row, c1: at.col, r2: at.row + rows - 1, c2: at.col + cols - 1 };
    this.data?.ensureSize(target.r2 + 1, target.c2 + 1);
    const edits: Edit[] = [{ type: "set", tab, at: cellName(at.row, at.col), rows: block.map(row => [...row, ...Array(cols - row.length).fill("")]) }];
    if (own) {
      // Calc to Calc: the looks come along too, cell by cell, grouped by style.
      const byStyle = new Map<number, Rect[]>();
      own.styles.forEach((row, i) => row.forEach((id, j) => {
        if (!id) return;
        const list = byStyle.get(id) ?? [];
        list.push({ r1: at.row + i, c1: at.col + j, r2: at.row + i, c2: at.col + j });
        byStyle.set(id, list);
      }));
      edits.unshift({ type: "clear", tab, range: a1(target), what: "formats" });
      for (const [id, rects] of byStyle) {
        const style = this.session.styles.get(id);
        if (!style) continue;
        for (const rect of rects.slice(0, 5_000)) edits.push({ type: "style", tab, range: a1(rect), patch: { clear: true, ...style, control: style?.control ?? null } as StylePatch });
      }
      if (own.cut) {
        const source = own.range;
        const sameTab = own.tab === tab;
        const overlap = sameTab && rectsIntersect(source, target);
        if (!overlap) edits.unshift({ type: "clear", tab: own.tab, range: a1(source), what: "all" });
        this.clipboard = null;
        this.copyRange = null;
      }
    } else if (patches) {
      patches.forEach((row, i) => row.forEach((patch, j) => {
        if (patch) edits.push({ type: "style", tab, range: cellName(at.row + i, at.col + j), patch });
      }));
      for (const m of merges) edits.push({ type: "merge", tab, range: a1(m) });
    }
    const pending: PendingSet[] = [];
    block.forEach((row, i) => row.forEach((input, j) => {
      if (pending.length < 20_000) pending.push({ tab, row: at.row + i, col: at.col + j, input });
    }));
    void this.send(edits, pending);
    this.setSelection({ anchor: { ...at }, active: { ...at }, range: target, mode: "cells" }, false);
  }

  clearCopyOutline(): void {
    if (!this.copyRange) return;
    this.copyRange = null;
    this.notify();
  }

  /* ---- protection and charts --------------------------------------------------------------------------------------- */

  protect(options: { range: string | null; editors: string[]; description: string; id?: string }): Promise<boolean> {
    return this.send([{ type: "protect", protection: { ...(options.id ? { id: options.id } : {}), tab: this.activeTab, ...(options.range ? { range: options.range } : {}), editors: options.editors, description: options.description } }]);
  }

  unprotect(id: string): Promise<boolean> {
    return this.send([{ type: "unprotect", id }]);
  }

  /** A chart of the selection, anchored just right of it. */
  addChart(kind: "line" | "bar" | "column" | "area" | "pie" | "scatter", title: string): Promise<boolean> {
    const r = this.selection.range;
    const used = this.data?.usedRect();
    const area: Rect = { r1: r.r1, c1: r.c1, r2: Math.min(r.r2, used?.r2 ?? r.r2), c2: Math.min(r.c2, used?.c2 ?? r.c2) };
    const anchor = cellName(area.r1, area.c2 + 2);
    return this.send([{ type: "chart", chart: { tab: this.activeTab, kind, title, range: a1(area), anchor, width: 480, height: 300 } }]);
  }

  deleteChart(id: string): Promise<boolean> {
    return this.send([{ type: "delete_chart", id }]);
  }

  /* ---- references for other panels ------------------------------------------------------------------------------- */

  /** The selection as a comment's place: a cell or range, whole rows or columns, or null for the whole tab. */
  commentRange(): string | null {
    if (this.selection.mode === "all") return null;
    return this.apiRange(this.selection.range);
  }

  /** The selection as a table reference for code (`Sales` for a whole tab, `Sales!A1:D100` for a range). */
  codeReference(): string {
    const tab = this.tab;
    if (!tab) return "";
    if (this.selection.mode === "all" || (this.selection.range.r1 === this.selection.range.r2 && this.selection.range.c1 === this.selection.range.c2)) return quoteTab(tab.name);
    return `${quoteTab(tab.name)}!${this.apiRange()}`;
  }

  /** Brings a place into view and selects it (from a comment, a search hit, a compare row). */
  focusRange(tabId: number, range: string | null): void {
    if (tabId !== this.activeTab) this.openTab(tabId);
    if (!range) return;
    const data = this.data;
    const r = parseRange(range, data?.rows.count ?? MAX_ROWS, data?.cols.count ?? MAX_COLS);
    if (!r) {
      const cell = parseCell(range);
      if (cell) this.setSelection(selectCell(this.grid(), cell.row, cell.col));
      return;
    }
    data?.ensureSize(Math.min(r.r2, r.r1 + BLOCK_ROWS) + 1, r.c2 + 1);
    this.setSelection({ anchor: { row: r.r1, col: r.c1 }, active: { row: r.r1, col: r.c1 }, range: r, mode: r.r1 === 0 && r.r2 >= (data?.rows.count ?? MAX_ROWS) - 1 ? "cols" : r.c1 === 0 && r.c2 >= (data?.cols.count ?? MAX_COLS) - 1 ? "rows" : "cells" });
  }

  setDiff(diff: DiffOverlay | null): void {
    this.diff = diff;
    this.notify();
  }
}

/** Turns an edit failure into the words a toast shows. */
export function failureNotice(error: ApiError, fallback: string): Notice {
  return { kind: "error", title: fallback, description: [error.message, error.hint].filter(Boolean).join(" ") };
}
