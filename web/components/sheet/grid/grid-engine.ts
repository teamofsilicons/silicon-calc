/**
 * The grid's engine, outside React: it owns the scroll position, sizes the canvas, draws only what is on screen
 * (render.ts) once per animation frame, asks the session for the blocks in view, and turns the pointer and the keys into
 * the controller's selection, editing and changes (lib/sheet/controller.ts).
 *
 * The DOM it drives:
 *   root      positioned box the grid fills (resize observed)
 *   canvas    the drawing, sized to the scroller's client area (scrollbars are never drawn over)
 *   scroller  native scrolling over a spacer as big as the tab (capped: geometry.ts scrollMap), so wheels, trackpads
 *             and touch scroll as smoothly as the device can; its offsets are mapped to content pixels
 *   input     one textarea that is the keyboard's home: hidden over the active cell while selecting (IME windows open
 *             there), and the in-cell editor while editing (transparent text over `mirror`, which shows the formula with
 *             each reference in its colour)
 */
import type { Style } from "@/lib/calc/types";
import { cellKey, type CellPos, type Rect } from "@/lib/sheet/address";
import type { SheetController } from "@/lib/sheet/controller";
import { referenceColors, references, tokenize, isFormula, type RefToken } from "@/lib/sheet/formula";
import { MAX_ROWS, MAX_COLS } from "@/lib/sheet/address";
import { scrollMap, toContent, toScroll, type ScrollMap } from "@/lib/sheet/geometry";
import type { Direction } from "@/lib/sheet/selection";
import { TabData as TabDataClass, type TabData } from "@/lib/sheet/store";
import { clampScroll, colLeft, frozenHeight, frozenWidth, type GridLayout, hitTest, rangeOnScreen, resizeEdge, rowTop, scrollToShow, type ViewportState, visibleRange } from "@/lib/sheet/viewport";
import { clearMeasureCache, COLORS, drawGrid, fontOf, type Highlight, type Scene } from "./render";

export const HEADER_HEIGHT = 24;
const PAD = 4;

export interface GridCallbacks {
  /** Right click, long press, the context menu key: where (client px) and what was under it. */
  onContextMenu: (x: number, y: number, area: "cell" | "row-header" | "col-header" | "corner") => void;
  /** The pointer rests on a cell (for comment previews), or left it. */
  onHover: (hover: { row: number; col: number; x: number; y: number; width: number; height: number } | null) => void;
  /** Ctrl/Cmd + Alt + M: comment on the selection. */
  onComment: () => void;
  /** A file dropped on the grid. */
  onDropFiles: (files: File[]) => void;
}

export interface Elements {
  root: HTMLDivElement;
  canvas: HTMLCanvasElement;
  scroller: HTMLDivElement;
  spacer: HTMLDivElement;
  input: HTMLTextAreaElement;
  mirror: HTMLDivElement;
}

type Drag =
  | { kind: "cells"; pointer: number }
  | { kind: "rows"; pointer: number }
  | { kind: "cols"; pointer: number }
  | { kind: "point"; pointer: number; anchor: CellPos }
  | { kind: "fill"; pointer: number }
  | { kind: "resize"; pointer: number; axis: "col" | "row"; index: number; start: number; origin: number; size: number };

const isApple = () => typeof navigator !== "undefined" && /mac|iphone|ipad|ipod/i.test((navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ?? navigator.platform ?? "");

const escapeHtml = (text: string) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** The editor's text as HTML: every reference of a formula in its highlight colour (the grid outlines it the same). */
export function highlightedHtml(text: string): string {
  if (!isFormula(text)) return `${escapeHtml(text)}​`;
  const tokens = tokenize(text);
  const colors = referenceColors(tokens.filter((t): t is RefToken => t.kind === "ref"));
  let html = "";
  for (const token of tokens) {
    const color = token.kind === "ref" ? colors.get(token as RefToken) : null;
    if (color) html += `<span style="color:${color}">${escapeHtml(token.text)}</span>`;
    else if (token.kind === "string") html += `<span data-kind="string">${escapeHtml(token.text)}</span>`;
    else if (token.kind === "number" || token.kind === "bool") html += `<span data-kind="number">${escapeHtml(token.text)}</span>`;
    else if (token.kind === "func") html += `<span data-kind="func">${escapeHtml(token.text)}</span>`;
    else html += escapeHtml(token.text);
  }
  // A trailing line break needs a character after it to take up its line.
  return `${html}​`;
}

/** A stable colour for another viewer (by their uuid). */
export function viewerColor(uuid: string): string {
  const palette = ["#7A4FC4", "#17806B", "#C2410C", "#0E7490", "#BE185D", "#93560A", "#4F46E5", "#15803D"];
  let hash = 0;
  for (const char of uuid) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return palette[hash % palette.length]!;
}

export class GridEngine {
  readonly el: Elements;
  controller: SheetController;
  callbacks: GridCallbacks;
  scrollX = 0;
  scrollY = 0;
  width = 0;
  height = 0;
  private dpr = 1;
  private mapX: ScrollMap = { domSize: 0, ratio: 1 };
  private mapY: ScrollMap = { domSize: 0, ratio: 1 };
  private frame = 0;
  private drag: Drag | null = null;
  private autoScroll = 0;
  private lastPointer = { x: 0, y: 0 };
  private hoverTimer: ReturnType<typeof setTimeout> | null = null;
  private hoverKey = -1;
  private touch: { id: number; x: number; y: number; at: number } | null = null;
  private lastTap = { at: 0, row: -1, col: -1 };
  private resizeGuide: { axis: "col" | "row"; at: number } | null = null;
  private editorShown = "";
  private mirrorText: string | null = null;
  private lastTab = -1;
  private viewListeners = new Set<() => void>();
  private commentCells = new Set<number>();
  private disposers: Array<() => void> = [];
  private measureCtx: CanvasRenderingContext2D | null = null;
  private readonly apple = isApple();
  /** Bumped when the viewport moves or resizes (React overlays follow it). */
  viewRevision = 0;
  private spillCache: { key: string; area: Rect | null } = { key: "", area: null };
  private fallback: TabData | null = null;

  constructor(el: Elements, controller: SheetController, callbacks: GridCallbacks) {
    this.el = el;
    this.controller = controller;
    this.callbacks = callbacks;
    this.measureCtx = document.createElement("canvas").getContext("2d");
    this.attach();
    this.measure();
    this.schedule();
  }

  /* ---- wiring -------------------------------------------------------------------------------------------------- */

  private on<K extends keyof HTMLElementEventMap>(target: HTMLElement | Window | Document, type: K, handler: (event: HTMLElementEventMap[K]) => void, options?: AddEventListenerOptions): void {
    target.addEventListener(type, handler as EventListener, options);
    this.disposers.push(() => target.removeEventListener(type, handler as EventListener, options));
  }

  private attach(): void {
    const { scroller, input, root } = this.el;
    this.on(scroller, "scroll", () => this.onScroll(), { passive: true });
    this.on(scroller, "pointerdown", event => this.onPointerDown(event));
    this.on(scroller, "pointermove", event => this.onPointerMove(event));
    this.on(scroller, "pointerup", event => this.onPointerUp(event));
    this.on(scroller, "pointercancel", event => this.onPointerUp(event, true));
    this.on(scroller, "pointerleave", () => this.setHover(null));
    this.on(scroller, "dblclick", event => this.onDoubleClick(event));
    this.on(scroller, "contextmenu", event => this.onContextMenu(event));
    this.on(scroller, "dragover", event => {
      if (event.dataTransfer?.types.includes("Files")) {
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
      }
    });
    this.on(scroller, "drop", event => {
      const files = [...(event.dataTransfer?.files ?? [])];
      if (!files.length) return;
      event.preventDefault();
      this.callbacks.onDropFiles(files);
    });
    this.on(input, "keydown", event => this.onKeyDown(event));
    this.on(input, "input", () => this.onInput());
    this.on(input, "copy", event => {
      if (this.controller.editor.open) return;
      this.controller.copy(event as ClipboardEvent);
    });
    this.on(input, "cut", event => {
      if (this.controller.editor.open) return;
      if (!this.controller.canEdit) {
        this.controller.copy(event as ClipboardEvent);
        return;
      }
      this.controller.copy(event as ClipboardEvent, true);
    });
    this.on(input, "paste", event => {
      if (this.controller.editor.open) return;
      this.controller.paste(event as ClipboardEvent);
    });
    this.on(input, "compositionstart", () => {
      if (!this.controller.editor.open) this.controller.startEdit({ text: "" });
    });
    this.on(input, "select", () => this.syncCaret());
    this.on(input, "keyup", () => this.syncCaret());
    this.on(input, "pointerup", () => this.syncCaret());
    const observer = new ResizeObserver(() => {
      this.measure();
      this.schedule();
    });
    observer.observe(root);
    this.disposers.push(() => observer.disconnect());
    this.disposers.push(this.controller.subscribe(() => this.onControllerChange()));
    this.disposers.push(this.controller.session.subscribe(() => this.schedule()));
    if (typeof document !== "undefined" && document.fonts) {
      void document.fonts.ready.then(() => {
        clearMeasureCache();
        this.schedule();
      });
    }
    this.controller.reveal = (row, col) => this.reveal(row, col);
    this.controller.focusGrid = () => this.focus();
  }

  destroy(): void {
    cancelAnimationFrame(this.frame);
    cancelAnimationFrame(this.autoScroll);
    if (this.hoverTimer) clearTimeout(this.hoverTimer);
    for (const dispose of this.disposers.splice(0)) dispose();
    if (this.controller.reveal) this.controller.reveal = null;
    this.controller.focusGrid = null;
    this.viewListeners.clear();
  }

  /** React overlays (charts, suggestions) follow the viewport through this. */
  subscribeView(listener: () => void): () => void {
    this.viewListeners.add(listener);
    return () => this.viewListeners.delete(listener);
  }

  setCommentCells(cells: Set<number>): void {
    this.commentCells = cells;
    this.schedule();
  }

  focus(): void {
    this.el.input.focus({ preventScroll: true });
  }

  /* ---- geometry ------------------------------------------------------------------------------------------------ */

  get data(): TabData | null {
    return this.controller.data;
  }

  layout(): GridLayout {
    const data = this.data;
    if (data) return this.layoutOf(data);
    // No tab yet (the tab list is loading again): an empty grid of the default size.
    this.fallback ??= new TabDataClass({ id: -1, name: "", index: 0, rows: 1000, cols: 26, used: null, frozen_rows: 0, frozen_cols: 0, color: null, code: null });
    return this.layoutOf(this.fallback);
  }

  /** The spill area of the active cell's formula, worked out once per selection and change. */
  private spill(): Rect | null {
    const c = this.controller;
    const key = `${c.activeTab}:${c.selection.active.row}:${c.selection.active.col}:${c.session.revision}`;
    if (this.spillCache.key !== key) this.spillCache = { key, area: c.spillArea() };
    return this.spillCache.area;
  }

  private layoutOf(data: TabData): GridLayout {
    const digits = String(Math.max(1000, data.rows.count)).length;
    return { rows: data.rows, cols: data.cols, frozenRows: data.frozenRows, frozenCols: data.frozenCols, headerWidth: Math.max(46, Math.ceil(digits * 7.4 + 16)), headerHeight: HEADER_HEIGHT };
  }

  view(): ViewportState {
    return { width: this.width, height: this.height, scrollX: this.scrollX, scrollY: this.scrollY };
  }

  private measure(): void {
    const { scroller, canvas } = this.el;
    const width = scroller.clientWidth;
    const height = scroller.clientHeight;
    const dpr = Math.max(1, Math.min(3, window.devicePixelRatio || 1));
    if (width !== this.width || height !== this.height || dpr !== this.dpr) {
      this.width = width;
      this.height = height;
      this.dpr = dpr;
      canvas.width = Math.max(1, Math.round(width * dpr));
      canvas.height = Math.max(1, Math.round(height * dpr));
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
    }
    this.updateSpacer();
  }

  /** The spacer spans the whole tab (capped), so the scrollbars show where in the tab the view is. */
  private updateSpacer(): void {
    if (!this.data) return;
    const layout = this.layout();
    const contentW = layout.headerWidth + layout.cols.total();
    const contentH = layout.headerHeight + layout.rows.total();
    this.mapX = scrollMap(contentW, this.width);
    this.mapY = scrollMap(contentH, this.height);
    const w = `${Math.ceil(this.mapX.domSize)}px`;
    const h = `${Math.ceil(this.mapY.domSize)}px`;
    if (this.el.spacer.style.width !== w) this.el.spacer.style.width = w;
    if (this.el.spacer.style.height !== h) this.el.spacer.style.height = h;
  }

  /** Grows the grid as it is scrolled to its end (as Excel's endless sheet does), up to the engine's limits. */
  private growIfNearEnd(): void {
    const data = this.data;
    if (!data) return;
    const layout = this.layout();
    const visible = visibleRange(layout, this.view());
    let grew = false;
    if (visible.rows.end >= data.rows.count - 30 && data.rows.count < MAX_ROWS) {
      data.rows.count = Math.min(MAX_ROWS, data.rows.count + 1000);
      grew = true;
    }
    if (visible.cols.end >= data.cols.count - 2 && data.cols.count < MAX_COLS) {
      data.cols.count = Math.min(MAX_COLS, data.cols.count + 10);
      grew = true;
    }
    if (grew) this.updateSpacer();
  }

  private onScroll(): void {
    const { scroller } = this.el;
    const x = toContent(this.mapX, scroller.scrollLeft);
    const y = toContent(this.mapY, scroller.scrollTop);
    // A programmatic scroll lands within a pixel of the offset that was asked for; keep the exact offset then.
    if (Math.abs(x - this.scrollX) > this.mapX.ratio) this.scrollX = x;
    if (Math.abs(y - this.scrollY) > this.mapY.ratio) this.scrollY = y;
    if (this.data) {
      const clamped = clampScroll(this.layout(), this.view(), this.scrollX, this.scrollY);
      this.scrollX = clamped.x;
      this.scrollY = clamped.y;
    }
    this.growIfNearEnd();
    this.setHover(null);
    this.schedule();
  }

  /** Scrolls to content offsets (kept exact, whatever the DOM can represent). */
  scrollTo(x: number, y: number): void {
    if (!this.data) return;
    const clamped = clampScroll(this.layout(), this.view(), x, y);
    this.scrollX = clamped.x;
    this.scrollY = clamped.y;
    this.el.scroller.scrollLeft = toScroll(this.mapX, clamped.x);
    this.el.scroller.scrollTop = toScroll(this.mapY, clamped.y);
    this.schedule();
  }

  /** Brings a cell into view with the least movement. */
  reveal(row: number, col: number): void {
    if (!this.data) return;
    this.data.ensureSize(row + 1, col + 1);
    this.updateSpacer();
    const target = scrollToShow(this.layout(), this.view(), row, col);
    if (target.x !== this.scrollX || target.y !== this.scrollY) this.scrollTo(target.x, target.y);
    else this.schedule();
  }

  /** A cell's box on screen, relative to the root (merged cells whole). */
  cellRect(row: number, col: number): { x: number; y: number; width: number; height: number } {
    const layout = this.layout();
    const merge = this.data?.mergeAt(row, col);
    const r = merge ?? { r1: row, c1: col, r2: row, c2: col };
    const box = rangeOnScreen(layout, this.view(), r.r1, r.c1, r.r2, r.c2);
    return { x: box.x, y: box.y, width: box.width, height: box.height };
  }

  /** The scrolling pane's bounds on screen (the area below and right of the headers and frozen panes). */
  bodyBounds(): { left: number; top: number; right: number; bottom: number } {
    const layout = this.layout();
    return { left: layout.headerWidth + frozenWidth(layout), top: layout.headerHeight + frozenHeight(layout), right: this.width, bottom: this.height };
  }

  /* ---- drawing ------------------------------------------------------------------------------------------------- */

  schedule(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.draw();
    });
  }

  private draw(): void {
    const data = this.data;
    const ctx = this.el.canvas.getContext("2d");
    if (!ctx || !data || this.width === 0 || this.height === 0) return;
    if (this.lastTab !== this.controller.activeTab) {
      this.lastTab = this.controller.activeTab;
      this.updateSpacer();
    }
    const layout = this.layout();
    const view = this.view();
    const controller = this.controller;
    const editor = controller.editor;
    const editingHere = editor.open && editor.tab === controller.activeTab;
    const highlights: Highlight[] = [];
    // Other viewers' selections, with their id.
    for (const viewer of controller.session.othersOn(controller.activeTab)) {
      const r = parseRangeSafe(viewer.range, data);
      if (r) highlights.push({ rect: r, color: viewerColor(viewer.account.uuid), label: viewer.account.id });
    }
    // The references of the formula being edited, in their colours.
    let pointing: Rect | null = null;
    if (editor.open && isFormula(editor.text)) {
      const refs = references(editor.text);
      const colors = referenceColors(refs);
      const here = controller.tab?.name.toLowerCase() ?? "";
      const editingTab = controller.session.tabs.find(t => t.id === editor.tab)?.name.toLowerCase() ?? "";
      for (const ref of refs) {
        const onThisTab = ref.tab ? ref.tab.toLowerCase() === here : editingTab === here;
        if (!onThisTab) continue;
        const rect = { r1: ref.rect.r1, c1: ref.rect.c1, r2: Math.min(ref.rect.r2, data.rows.count - 1), c2: Math.min(ref.rect.c2, data.cols.count - 1) };
        highlights.push({ rect, color: colors.get(ref) ?? COLORS.selection, fill: true });
      }
      if (editor.pointed && (editor.pointed.tab ? editor.pointed.tab.toLowerCase() === here : editingTab === here)) pointing = editor.pointed.rect;
    }
    const diffTab = controller.diff?.get(controller.activeTab) ?? null;
    const scene: Scene = {
      layout,
      view,
      data,
      styles: controller.session.styles,
      selection: editor.open && !editingHere ? null : controller.selection,
      editing: editingHere,
      highlights,
      copyRange: controller.copyRange && controller.copyRange.tab === controller.activeTab ? controller.copyRange.range : null,
      fillPreview: controller.fillPreview,
      pointing,
      commentCells: this.commentCells,
      diff: diffTab,
      spill: editor.open ? null : this.spill(),
      showLoading: true,
      fillHandle: controller.canEdit && !editor.open,
    };
    drawGrid(ctx, scene, this.dpr);
    if (this.resizeGuide) {
      ctx.fillStyle = COLORS.selection;
      if (this.resizeGuide.axis === "col") ctx.fillRect(Math.round(this.resizeGuide.at) - 1, 0, 2, this.height);
      else ctx.fillRect(0, Math.round(this.resizeGuide.at) - 1, this.width, 2);
    }
    this.requestBlocks(layout, view, data);
    this.positionEditor(layout, data);
    this.viewRevision++;
    for (const listener of [...this.viewListeners]) listener();
  }

  /** Asks the session for the blocks on screen, then a block's worth around them (frozen panes separately). */
  private requestBlocks(layout: GridLayout, view: ViewportState, data: TabData): void {
    const visible = visibleRange(layout, view);
    const rowsSpan = visible.rows;
    const colsSpan = visible.cols;
    const shown: Rect[] = [];
    const around: Rect[] = [];
    const lastRow = data.rows.count - 1;
    const lastCol = data.cols.count - 1;
    const grow = (r: Rect, dr: number, dc: number): Rect => ({ r1: Math.max(0, r.r1 - dr), c1: Math.max(0, r.c1 - dc), r2: Math.min(lastRow, r.r2 + dr), c2: Math.min(lastCol, r.c2 + dc) });
    // Text in columns left of the pane can overflow into it: read a few columns early.
    const bodyCols = { start: Math.max(0, colsSpan.start - 8), end: colsSpan.end };
    if (rowsSpan.end >= rowsSpan.start && colsSpan.end >= colsSpan.start) {
      const body = { r1: rowsSpan.start, c1: bodyCols.start, r2: rowsSpan.end, c2: bodyCols.end };
      shown.push(body);
      around.push(grow(body, 96, 6));
    }
    if (visible.frozenRows.end >= 0 && colsSpan.end >= colsSpan.start) shown.push({ r1: 0, c1: bodyCols.start, r2: visible.frozenRows.end, c2: bodyCols.end });
    if (visible.frozenCols.end >= 0 && rowsSpan.end >= rowsSpan.start) shown.push({ r1: rowsSpan.start, c1: 0, r2: rowsSpan.end, c2: visible.frozenCols.end });
    if (visible.frozenRows.end >= 0 && visible.frozenCols.end >= 0) shown.push({ r1: 0, c1: 0, r2: visible.frozenRows.end, c2: visible.frozenCols.end });
    this.controller.session.view(this.controller.activeTab, shown, around);
  }

  /* ---- the in-cell editor -------------------------------------------------------------------------------------- */

  private onControllerChange(): void {
    this.syncEditor();
    this.schedule();
  }

  /** Puts the controller's editor state into the textarea (text, caret) when it changed elsewhere. */
  private syncEditor(): void {
    const { input } = this.el;
    const editor = this.controller.editor;
    const key = editor.open ? `${editor.tab}:${editor.row}:${editor.col}` : "";
    if (!editor.open) {
      if (this.editorShown) {
        this.editorShown = "";
        input.value = "";
        this.mirrorText = null;
      }
      return;
    }
    if (input.value !== editor.text) input.value = editor.text;
    if (key !== this.editorShown) {
      this.editorShown = key;
    }
    if (editor.where === "cell" && document.activeElement === input && (input.selectionStart !== editor.caret || input.selectionEnd !== editor.caret)) {
      // Only move the caret when the controller moved it (pointing, suggestions), never while text is selected by hand.
      if (input.selectionStart === input.selectionEnd || input.selectionEnd !== editor.caret) input.setSelectionRange(editor.caret, editor.caret);
    }
    if (editor.where === "cell" && document.activeElement !== input && !this.controller.isPointing()) this.focus();
  }

  private onInput(): void {
    const { input } = this.el;
    const controller = this.controller;
    if (!controller.editor.open) {
      // Typing reached the textarea before the editor opened (a composition, a fast key): open it with what was typed.
      const typed = input.value;
      if (!typed) return;
      if (!controller.startEdit({ text: typed })) {
        input.value = "";
        return;
      }
    }
    controller.setEditorText(input.value, input.selectionStart ?? input.value.length);
  }

  private syncCaret(): void {
    const { input } = this.el;
    const editor = this.controller.editor;
    if (!editor.open || editor.where !== "cell") return;
    const caret = input.selectionEnd ?? input.value.length;
    if (caret !== editor.caret && input.value === editor.text) this.controller.setEditorText(editor.text, caret);
  }

  /** The editor over its cell, growing right as the text grows, then down once it reaches the grid's edge. */
  private positionEditor(layout: GridLayout, data: TabData): void {
    const { input, mirror } = this.el;
    const controller = this.controller;
    const editor = controller.editor;
    const editingHere = editor.open && editor.tab === controller.activeTab;
    const row = editingHere ? editor.row : controller.selection.active.row;
    const col = editingHere ? editor.col : controller.selection.active.col;
    const box = this.cellRect(row, col);
    const bounds = { left: layout.headerWidth, top: layout.headerHeight, right: this.width, bottom: this.height };
    const cell = data.get(row, col);
    const style: Style | undefined = cell?.s ? controller.session.styles.get(cell.s) : undefined;
    if (!editingHere) {
      // Hidden, but over the active cell: an input method's window opens next to it.
      input.dataset.editing = "false";
      mirror.style.display = "none";
      input.style.transform = `translate(${Math.max(bounds.left, Math.min(box.x, bounds.right - 4))}px, ${Math.max(bounds.top, Math.min(box.y, bounds.bottom - 4))}px)`;
      input.style.width = "2px";
      input.style.height = `${Math.max(16, Math.min(box.height, 40))}px`;
      input.inputMode = "none";
      return;
    }
    input.dataset.editing = "true";
    input.inputMode = "text";
    const { font, px } = fontOf(style);
    const lineHeight = Math.round(px * 1.35);
    const text = editor.text;
    if (this.mirrorText !== text) {
      this.mirrorText = text;
      mirror.innerHTML = highlightedHtml(text);
    }
    let widest = 0;
    if (this.measureCtx) {
      this.measureCtx.font = font;
      for (const line of text.split("\n")) widest = Math.max(widest, this.measureCtx.measureText(line).width);
    }
    const x = Math.max(bounds.left, box.x);
    const y = Math.max(bounds.top, box.y);
    const maxWidth = Math.max(box.width, bounds.right - x - 2);
    const wanted = Math.ceil(widest + 2 * PAD + 8);
    const wrap = wanted > maxWidth || !!style?.wrap;
    const width = Math.min(maxWidth, Math.max(box.width + 1, style?.wrap ? box.width + 1 : wanted));
    const common = { font, lineHeight: `${lineHeight}px`, paddingTop: `${Math.max(2, Math.round((Math.min(box.height, lineHeight + 6) - lineHeight) / 2))}px` };
    for (const node of [input, mirror] as HTMLElement[]) {
      node.style.font = common.font;
      node.style.lineHeight = common.lineHeight;
      node.style.paddingTop = common.paddingTop;
      node.style.whiteSpace = wrap ? "pre-wrap" : "pre";
      node.style.width = `${width}px`;
      node.style.transform = `translate(${x - 1}px, ${y - 1}px)`;
    }
    mirror.style.display = "block";
    mirror.style.height = "auto";
    const height = Math.min(Math.max(box.height + 1, mirror.scrollHeight), Math.max(box.height + 1, bounds.bottom - y - 2));
    mirror.style.height = `${height}px`;
    input.style.height = `${height}px`;
  }

  /* ---- pointer ------------------------------------------------------------------------------------------------- */

  private local(event: { clientX: number; clientY: number }): { x: number; y: number } {
    const rect = this.el.root.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  /** True when the point is on the scroller's own scrollbars (not the grid). */
  private onScrollbar(x: number, y: number): boolean {
    return x >= this.el.scroller.clientWidth || y >= this.el.scroller.clientHeight;
  }

  private fillHandleAt(x: number, y: number): boolean {
    const controller = this.controller;
    if (!controller.canEdit || controller.editor.open || !this.data) return false;
    const r = controller.selection.range;
    const layout = this.layout();
    const view = this.view();
    const r2 = Math.min(r.r2, this.data.rows.count - 1);
    const c2 = Math.min(r.c2, this.data.cols.count - 1);
    const hx = colLeft(layout, view, c2) + layout.cols.sizeOf(c2) - 1;
    const hy = rowTop(layout, view, r2) + layout.rows.sizeOf(r2) - 1;
    return Math.abs(x - hx) <= 5 && Math.abs(y - hy) <= 5;
  }

  private onPointerDown(event: PointerEvent): void {
    const { x, y } = this.local(event);
    if (this.onScrollbar(x, y) || !this.data) return;
    this.setHover(null);
    if (event.pointerType === "touch") {
      // Touch scrolls natively; a tap selects (on pointer up).
      this.touch = { id: event.pointerId, x, y, at: performance.now() };
      return;
    }
    if (event.button === 2) return; // the contextmenu event follows
    if (event.button !== 0) return;
    const controller = this.controller;
    const layout = this.layout();
    const view = this.view();
    const hit = hitTest(layout, view, x, y);
    this.lastPointer = { x, y };

    // Pointing at a reference while typing a formula: the editor keeps focus.
    if (controller.editor.open && controller.isPointing() && hit.area === "cell") {
      event.preventDefault();
      const anchor = { row: hit.row, col: hit.col };
      if (event.shiftKey && controller.editor.pointed) {
        const p = controller.editor.pointed.rect;
        controller.point({ r1: Math.min(p.r1, hit.row), c1: Math.min(p.c1, hit.col), r2: Math.max(p.r1, hit.row), c2: Math.max(p.c1, hit.col) });
      } else controller.point({ r1: hit.row, c1: hit.col, r2: hit.row, c2: hit.col });
      this.capture(event, { kind: "point", pointer: event.pointerId, anchor });
      return;
    }
    event.preventDefault();
    const edge = resizeEdge(layout, view, x, y);
    if (edge) {
      const axisSizes = edge.axis === "col" ? layout.cols : layout.rows;
      const start = edge.axis === "col" ? colLeft(layout, view, edge.index) : rowTop(layout, view, edge.index);
      this.resizeGuide = { axis: edge.axis, at: start + axisSizes.sizeOf(edge.index) };
      this.capture(event, { kind: "resize", pointer: event.pointerId, axis: edge.axis, index: edge.index, start, origin: edge.axis === "col" ? x : y, size: axisSizes.sizeOf(edge.index) });
      this.schedule();
      return;
    }
    if (this.fillHandleAt(x, y)) {
      this.capture(event, { kind: "fill", pointer: event.pointerId });
      return;
    }
    if (controller.editor.open) controller.commit("none");
    this.focus();
    if (hit.area === "corner") {
      controller.selectAll();
      return;
    }
    if (hit.area === "col-header") {
      controller.clickColHeader(hit.col, event.shiftKey);
      this.capture(event, { kind: "cols", pointer: event.pointerId });
      return;
    }
    if (hit.area === "row-header") {
      controller.clickRowHeader(hit.row, event.shiftKey);
      this.capture(event, { kind: "rows", pointer: event.pointerId });
      return;
    }
    controller.clickCell(hit.row, hit.col, event.shiftKey);
    this.capture(event, { kind: "cells", pointer: event.pointerId });
  }

  private capture(event: PointerEvent, drag: Drag): void {
    this.drag = drag;
    try {
      this.el.scroller.setPointerCapture(event.pointerId);
    } catch {
      // Capture is a nicety (dragging outside the grid); without it the drag ends at the edge.
    }
  }

  private onPointerMove(event: PointerEvent): void {
    const { x, y } = this.local(event);
    this.lastPointer = { x, y };
    if (!this.data) return;
    const drag = this.drag;
    if (!drag) {
      if (event.pointerType !== "touch") this.updateCursor(x, y);
      return;
    }
    if (drag.kind === "resize") {
      const delta = (drag.axis === "col" ? x : y) - drag.origin;
      const size = Math.max(drag.axis === "col" ? 24 : 12, drag.size + delta);
      this.resizeGuide = { axis: drag.axis, at: drag.start + size };
      this.schedule();
      return;
    }
    this.dragTo(x, y);
    this.updateAutoScroll(x, y);
  }

  private dragTo(x: number, y: number): void {
    const drag = this.drag;
    if (!drag || !this.data) return;
    const layout = this.layout();
    const view = this.view();
    const clampedX = Math.max(layout.headerWidth + 1, Math.min(this.width - 1, x));
    const clampedY = Math.max(layout.headerHeight + 1, Math.min(this.height - 1, y));
    const hit = hitTest(layout, view, clampedX, clampedY);
    const controller = this.controller;
    if (drag.kind === "cells") controller.dragTo(hit.row, hit.col);
    else if (drag.kind === "rows") controller.clickRowHeader(hit.row, true);
    else if (drag.kind === "cols") controller.clickColHeader(hit.col, true);
    else if (drag.kind === "point") {
      const a = drag.anchor;
      controller.point({ r1: Math.min(a.row, hit.row), c1: Math.min(a.col, hit.col), r2: Math.max(a.row, hit.row), c2: Math.max(a.col, hit.col) });
    } else if (drag.kind === "fill") {
      const r = controller.selection.range;
      // Fill extends along the axis the pointer has moved furthest on, out of the selection.
      const downBy = hit.row > r.r2 ? hit.row - r.r2 : hit.row < r.r1 ? hit.row - r.r1 : 0;
      const rightBy = hit.col > r.c2 ? hit.col - r.c2 : hit.col < r.c1 ? hit.col - r.c1 : 0;
      let target: Rect = r;
      if (Math.abs(downBy) >= Math.abs(rightBy) && downBy !== 0) target = downBy > 0 ? { ...r, r2: hit.row } : { ...r, r1: hit.row };
      else if (rightBy !== 0) target = rightBy > 0 ? { ...r, c2: hit.col } : { ...r, c1: hit.col };
      controller.fillPreview = target === r ? null : target;
      controller.notify();
    }
  }

  /** Dragging past the edge of the pane scrolls it, faster the further out the pointer is. */
  private updateAutoScroll(x: number, y: number): void {
    const bounds = this.bodyBounds();
    const dx = x > bounds.right - 8 ? x - (bounds.right - 8) : x < bounds.left && this.drag?.kind !== "rows" ? x - bounds.left : 0;
    const dy = y > bounds.bottom - 8 ? y - (bounds.bottom - 8) : y < bounds.top && this.drag?.kind !== "cols" ? y - bounds.top : 0;
    if (!dx && !dy) {
      cancelAnimationFrame(this.autoScroll);
      this.autoScroll = 0;
      return;
    }
    if (this.autoScroll) return;
    const tick = () => {
      if (!this.drag || this.drag.kind === "resize") {
        this.autoScroll = 0;
        return;
      }
      const { x: px, y: py } = this.lastPointer;
      const b = this.bodyBounds();
      const sx = px > b.right - 8 ? px - (b.right - 8) : px < b.left && this.drag.kind !== "rows" ? px - b.left : 0;
      const sy = py > b.bottom - 8 ? py - (b.bottom - 8) : py < b.top && this.drag.kind !== "cols" ? py - b.top : 0;
      if (!sx && !sy) {
        this.autoScroll = 0;
        return;
      }
      const speed = (d: number) => Math.sign(d) * Math.min(80, 4 + Math.abs(d) * 0.6);
      this.scrollTo(this.scrollX + (this.drag.kind === "rows" ? 0 : speed(sx)), this.scrollY + (this.drag.kind === "cols" ? 0 : speed(sy)));
      this.dragTo(px, py);
      this.autoScroll = requestAnimationFrame(tick);
    };
    this.autoScroll = requestAnimationFrame(tick);
  }

  private onPointerUp(event: PointerEvent, cancelled = false): void {
    const { x, y } = this.local(event);
    cancelAnimationFrame(this.autoScroll);
    this.autoScroll = 0;
    if (this.touch && event.pointerId === this.touch.id) {
      const moved = Math.hypot(x - this.touch.x, y - this.touch.y);
      const quick = performance.now() - this.touch.at < 600;
      this.touch = null;
      if (!cancelled && moved < 8 && quick) this.tap(x, y);
      return;
    }
    const drag = this.drag;
    this.drag = null;
    try {
      this.el.scroller.releasePointerCapture(event.pointerId);
    } catch {
      // Not captured.
    }
    if (!drag || cancelled) {
      if (this.controller.fillPreview) {
        this.controller.fillPreview = null;
        this.controller.notify();
      }
      this.resizeGuide = null;
      this.schedule();
      return;
    }
    const controller = this.controller;
    if (drag.kind === "resize") {
      const delta = (drag.axis === "col" ? x : y) - drag.origin;
      const size = Math.round(Math.max(drag.axis === "col" ? 24 : 12, drag.size + delta));
      this.resizeGuide = null;
      if (Math.abs(delta) >= 1) this.applyResize(drag.axis, drag.index, size);
      this.schedule();
      return;
    }
    if (drag.kind === "fill") {
      const target = controller.fillPreview;
      controller.fillPreview = null;
      controller.notify();
      if (target) controller.fill(target);
      return;
    }
    if (drag.kind === "point") return;
    this.focus();
  }

  /** A resize applies to every selected whole column (or row) when the dragged one is among them. */
  private applyResize(axis: "col" | "row", index: number, size: number): void {
    const controller = this.controller;
    const data = this.data;
    if (!data) return;
    const sel = controller.selection;
    const whole = axis === "col" ? sel.mode === "cols" || sel.mode === "all" : sel.mode === "rows" || sel.mode === "all";
    const inSelection = axis === "col" ? index >= sel.range.c1 && index <= sel.range.c2 : index >= sel.range.r1 && index <= sel.range.r2;
    const span: [number, number] = whole && inSelection ? (axis === "col" ? [sel.range.c1, Math.min(sel.range.c2, data.cols.count - 1)] : [sel.range.r1, Math.min(sel.range.r2, data.rows.count - 1)]) : [index, index];
    const sizes = axis === "col" ? data.cols : data.rows;
    // Shown at once; the service's answer confirms it.
    if (span[1] - span[0] <= 5_000) for (let i = span[0]; i <= span[1]; i++) sizes.setSize(i, size);
    this.updateSpacer();
    if (!controller.canEdit) {
      // A viewer may still widen a column for themselves; nothing is sent.
      return;
    }
    if (axis === "col") controller.colWidth(span, size);
    else controller.rowHeight(span, size);
  }

  private tap(x: number, y: number): void {
    if (!this.data) return;
    const controller = this.controller;
    const hit = hitTest(this.layout(), this.view(), x, y);
    if (controller.editor.open && controller.isPointing() && hit.area === "cell") {
      controller.point({ r1: hit.row, c1: hit.col, r2: hit.row, c2: hit.col });
      return;
    }
    if (controller.editor.open) controller.commit("none");
    const now = performance.now();
    const double = now - this.lastTap.at < 400 && this.lastTap.row === hit.row && this.lastTap.col === hit.col;
    this.lastTap = { at: now, row: hit.row, col: hit.col };
    if (hit.area === "corner") controller.selectAll();
    else if (hit.area === "col-header") controller.clickColHeader(hit.col, false);
    else if (hit.area === "row-header") controller.clickRowHeader(hit.row, false);
    else {
      controller.clickCell(hit.row, hit.col, false);
      if (double && controller.canEdit) {
        this.el.input.inputMode = "text";
        this.focus();
        controller.startEdit({ mode: "edit" });
      }
    }
  }

  private updateCursor(x: number, y: number): void {
    const { scroller } = this.el;
    if (this.onScrollbar(x, y)) {
      scroller.style.cursor = "";
      return;
    }
    const layout = this.layout();
    const view = this.view();
    let cursor = "cell";
    const edge = resizeEdge(layout, view, x, y);
    if (edge) cursor = edge.axis === "col" ? "col-resize" : "row-resize";
    else if (this.fillHandleAt(x, y)) cursor = "crosshair";
    else if (y < layout.headerHeight || x < layout.headerWidth) cursor = "default";
    if (scroller.style.cursor !== cursor) scroller.style.cursor = cursor;
    // Comment previews: the pointer resting on a cell that has a thread.
    const hit = hitTest(layout, view, x, y);
    if (hit.area === "cell" && this.commentCells.size) {
      const key = cellKey(hit.row, hit.col);
      if (this.commentCells.has(key)) {
        if (this.hoverKey !== key) {
          this.hoverKey = key;
          if (this.hoverTimer) clearTimeout(this.hoverTimer);
          this.hoverTimer = setTimeout(() => {
            const box = this.cellRect(hit.row, hit.col);
            this.callbacks.onHover({ row: hit.row, col: hit.col, x: box.x, y: box.y, width: box.width, height: box.height });
          }, 350);
        }
        return;
      }
    }
    this.setHover(null);
  }

  private setHover(hover: null): void {
    if (this.hoverTimer) clearTimeout(this.hoverTimer);
    this.hoverTimer = null;
    if (this.hoverKey !== -1) {
      this.hoverKey = -1;
      this.callbacks.onHover(hover);
    }
  }

  private onDoubleClick(event: MouseEvent): void {
    const { x, y } = this.local(event);
    if (this.onScrollbar(x, y) || !this.data) return;
    const layout = this.layout();
    const view = this.view();
    const edge = resizeEdge(layout, view, x, y);
    const controller = this.controller;
    if (edge) {
      event.preventDefault();
      if (edge.axis === "col") {
        const ctx = this.measureCtx;
        const width = controller.fitWidth(edge.index, (text, style) => {
          if (!ctx) return text.length * 7;
          ctx.font = fontOf(style).font;
          return Math.max(...text.split("\n").map(line => ctx.measureText(line).width));
        });
        this.applyResize("col", edge.index, width);
      } else this.applyResize("row", edge.index, 21);
      return;
    }
    const hit = hitTest(layout, view, x, y);
    if (hit.area !== "cell") return;
    if (controller.editor.open && controller.isPointing()) return;
    event.preventDefault();
    controller.clickCell(hit.row, hit.col, false);
    this.focus();
    controller.startEdit({ mode: "edit" });
  }

  private onContextMenu(event: MouseEvent): void {
    const { x, y } = this.local(event);
    if (this.onScrollbar(x, y) || !this.data) return;
    event.preventDefault();
    const controller = this.controller;
    if (controller.editor.open) controller.commit("none");
    const hit = hitTest(this.layout(), this.view(), x, y);
    const sel = controller.selection;
    const inside = hit.row >= sel.range.r1 && hit.row <= sel.range.r2 && hit.col >= sel.range.c1 && hit.col <= sel.range.c2;
    if (hit.area === "cell" && !inside) controller.clickCell(hit.row, hit.col, false);
    if (hit.area === "col-header" && !(sel.mode === "cols" && inside) && sel.mode !== "all") controller.clickColHeader(hit.col, false);
    if (hit.area === "row-header" && !(sel.mode === "rows" && inside) && sel.mode !== "all") controller.clickRowHeader(hit.row, false);
    if (hit.area === "corner") controller.selectAll();
    this.focus();
    this.callbacks.onContextMenu(event.clientX, event.clientY, hit.area);
  }

  /* ---- keys ---------------------------------------------------------------------------------------------------- */

  private onKeyDown(event: KeyboardEvent): void {
    const controller = this.controller;
    const mod = this.apple ? event.metaKey : event.ctrlKey;
    if (event.isComposing || event.keyCode === 229) {
      if (!controller.editor.open) controller.startEdit({ text: "" });
      return;
    }
    if (controller.editor.open) {
      this.editorKey(event, mod);
      return;
    }
    const key = event.key;
    const shift = event.shiftKey;
    const dir = arrow(key);
    if (dir) {
      event.preventDefault();
      void controller.move(dir, { jump: mod || (this.apple && event.ctrlKey), extend: shift });
      return;
    }
    if (mod && !event.altKey) {
      const lower = key.toLowerCase();
      if (lower === "a") return void (event.preventDefault(), controller.selectAll());
      if (lower === "z") return void (event.preventDefault(), shift ? controller.session.redo() : controller.session.undo());
      if (lower === "y") return void (event.preventDefault(), controller.session.redo());
      if (lower === "b") return void (event.preventDefault(), controller.toggle("bold"));
      if (lower === "i") return void (event.preventDefault(), controller.toggle("italic"));
      if (lower === "u") return void (event.preventDefault(), controller.toggle("underline"));
      if (lower === "5" || (shift && lower === "x")) return void (event.preventDefault(), controller.toggle("strike"));
      if (lower === "d") return void (event.preventDefault(), controller.fillFrom("down"));
      if (lower === "r") return void (event.preventDefault(), controller.fillFrom("right"));
      if (lower === "\\") return void (event.preventDefault(), controller.clearSelection("formats"));
      if (key === "Home") return void (event.preventDefault(), controller.home(true, shift));
      if (key === "End") return void (event.preventDefault(), controller.end(shift));
      if (key === ";" || key === ":") {
        event.preventDefault();
        const now = new Date();
        const pad = (n: number) => String(n).padStart(2, "0");
        const text = shift || key === ":" ? `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}` : `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
        controller.startEdit({ text });
        return;
      }
      if (key === " " && !this.apple) return void (event.preventDefault(), this.selectColumns());
      // Copy, cut and paste arrive as their own events (the textarea holds focus); everything else is the browser's.
      return;
    }
    if (mod && event.altKey && key.toLowerCase() === "m") {
      event.preventDefault();
      this.callbacks.onComment();
      return;
    }
    if (event.altKey && shift && (key === "5" || key === "%")) {
      event.preventDefault();
      controller.toggle("strike");
      return;
    }
    if (event.ctrlKey && key === " ") {
      event.preventDefault();
      this.selectColumns();
      return;
    }
    switch (key) {
      case "Enter":
        event.preventDefault();
        if (shift) controller.advance("up");
        else controller.startEdit({ mode: "edit" });
        return;
      case "Tab":
        event.preventDefault();
        controller.advance(shift ? "left" : "right");
        return;
      case "F2":
        event.preventDefault();
        controller.startEdit({ mode: "edit" });
        return;
      case "Delete":
      case "Backspace":
        event.preventDefault();
        if (controller.canEdit) controller.clearSelection("contents");
        else controller.startEdit();
        return;
      case "Escape":
        controller.clearCopyOutline();
        return;
      case "Home":
        event.preventDefault();
        controller.home(false, shift);
        return;
      case "End":
        event.preventDefault();
        controller.end(shift);
        return;
      case "PageDown":
      case "PageUp": {
        event.preventDefault();
        const layout = this.layout();
        const rows = Math.max(1, Math.floor((this.height - layout.headerHeight - frozenHeight(layout)) / 21) - 1);
        controller.page(key === "PageDown" ? "down" : "up", rows, shift);
        return;
      }
      case "ContextMenu": {
        event.preventDefault();
        this.menuAtActive();
        return;
      }
      case "F10":
        if (shift) {
          event.preventDefault();
          this.menuAtActive();
        }
        return;
      case " ":
        if (shift) {
          event.preventDefault();
          this.selectRowsOfSelection();
          return;
        }
        break;
      default:
        break;
    }
    if (key.length === 1 && !event.ctrlKey && !event.metaKey) {
      // Typing replaces the cell: open the editor empty and let the browser put the key into it.
      if (!controller.startEdit({ text: "" })) event.preventDefault();
    }
  }

  private selectColumns(): void {
    const c = this.controller;
    const r = c.selection.range;
    c.clickColHeader(r.c1, false);
    if (r.c2 > r.c1) c.clickColHeader(r.c2, true);
  }

  private selectRowsOfSelection(): void {
    const c = this.controller;
    const r = c.selection.range;
    c.clickRowHeader(r.r1, false);
    if (r.r2 > r.r1) c.clickRowHeader(r.r2, true);
  }

  private menuAtActive(): void {
    const { row, col } = this.controller.selection.active;
    const box = this.cellRect(row, col);
    const rect = this.el.root.getBoundingClientRect();
    this.callbacks.onContextMenu(rect.left + box.x + Math.min(box.width, 40), rect.top + box.y + box.height, "cell");
  }

  /** Keys while the in-cell editor is open. */
  private editorKey(event: KeyboardEvent, mod: boolean): void {
    const controller = this.controller;
    const key = event.key;
    const suggestions = controller.suggestions();
    if (suggestions.length) {
      if (key === "ArrowDown" || key === "ArrowUp") {
        event.preventDefault();
        controller.moveSuggestion(key === "ArrowDown" ? 1 : -1);
        return;
      }
      if (key === "Tab" || (key === "Enter" && !event.shiftKey && !mod && !event.altKey)) {
        event.preventDefault();
        controller.acceptSuggestion();
        return;
      }
      if (key === "Escape") {
        event.preventDefault();
        controller.dismissSuggestions();
        return;
      }
    }
    if (key === "Enter") {
      event.preventDefault();
      if (event.altKey || (event.ctrlKey && this.apple)) controller.insertNewline();
      else if (mod) controller.commit("none", true);
      else controller.commit(event.shiftKey ? "up" : "down");
      this.focus();
      return;
    }
    if (key === "Tab") {
      event.preventDefault();
      controller.commit(event.shiftKey ? "left" : "right");
      this.focus();
      return;
    }
    if (key === "Escape") {
      event.preventDefault();
      controller.cancelEdit();
      this.focus();
      return;
    }
    if (key === "F2") {
      event.preventDefault();
      controller.editor = { ...controller.editor, mode: controller.editor.mode === "enter" ? "edit" : "enter" };
      controller.notify();
      return;
    }
    if (key === "F4") {
      event.preventDefault();
      controller.cycleReference();
      return;
    }
    const dir = arrow(key);
    if (dir && controller.editor.mode === "enter" && !mod) {
      event.preventDefault();
      if (controller.isPointing()) controller.pointMove(dir, event.shiftKey);
      else {
        controller.commit(dir === "up" ? "up" : dir === "down" ? "down" : dir === "left" ? "left" : "right");
        this.focus();
      }
      return;
    }
    if (mod && key.toLowerCase() === "z") {
      // Undo inside the editor is the textarea's own.
      return;
    }
  }
}

function arrow(key: string): Direction | null {
  if (key === "ArrowUp") return "up";
  if (key === "ArrowDown") return "down";
  if (key === "ArrowLeft") return "left";
  if (key === "ArrowRight") return "right";
  return null;
}

/** A viewer's range (A1 text) as a rectangle on this tab. */
function parseRangeSafe(text: string, data: TabData): Rect | null {
  const clean = text.replace(/\$/g, "").trim();
  const m = /^([A-Z]{1,3})(\d+)(?::([A-Z]{1,3})(\d+))?$/i.exec(clean);
  const col = (letters: string) => {
    let n = 0;
    for (const ch of letters.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
    return n - 1;
  };
  if (m) {
    const r1 = Number(m[2]) - 1;
    const c1 = col(m[1]!);
    const r2 = m[4] ? Number(m[4]) - 1 : r1;
    const c2 = m[3] ? col(m[3]) : c1;
    return { r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
  }
  const cols = /^([A-Z]{1,3}):([A-Z]{1,3})$/i.exec(clean);
  if (cols) return { r1: 0, c1: Math.min(col(cols[1]!), col(cols[2]!)), r2: data.rows.count - 1, c2: Math.max(col(cols[1]!), col(cols[2]!)) };
  const rows = /^(\d+):(\d+)$/.exec(clean);
  if (rows) return { r1: Math.min(Number(rows[1]), Number(rows[2])) - 1, c1: 0, r2: Math.max(Number(rows[1]), Number(rows[2])) - 1, c2: data.cols.count - 1 };
  return null;
}
