/**
 * Draws the grid on a canvas: only what is on screen, in up to four panes (frozen rows and columns stay put), with the
 * row and column headers, cell fills, gridlines, merged cells, text in the cell's look (bold, italic, underline,
 * strike, colour, alignment, wrapping; text overflows into empty neighbours as in Excel, numbers that do not fit show
 * ###), borders, spill outlines, comment markers, protected ranges, compare highlights, other viewers' selections,
 * the formula's references while editing, the copy outline, the fill handle and the selection.
 *
 * Pure drawing: everything it needs arrives in `Scene`. Light only; the colours are Calc's tokens.
 */
import type { Style } from "@/lib/calc/types";
import { cellKey, colName, type Rect } from "@/lib/sheet/address";
import type { CellRecord, TabData } from "@/lib/sheet/store";
import { colLeft, frozenHeight, frozenWidth, type GridLayout, rowTop, type Span, type ViewportState, visibleRange } from "@/lib/sheet/viewport";

export const COLORS = {
  background: "#FFFFFF",
  gridline: "#E4E7EC",
  header: "#F7F8FA",
  headerLine: "#DDE1E7",
  headerText: "#5C6370",
  headerActive: "#E4ECF7",
  headerActiveText: "#174C96",
  headerSelected: "#1F5FB8",
  selectionFill: "rgba(31, 95, 184, 0.09)",
  selection: "#1F5FB8",
  frozenLine: "#C4CAD3",
  text: "#292929",
  pendingText: "#5C6370",
  error: "#B42318",
  comment: "#E39B2D",
  spill: "#1F5FB8",
  loading: "#EEF0F4",
  protectedHatch: "rgba(147, 86, 10, 0.16)",
  diffChanged: "rgba(31, 95, 184, 0.16)",
  diffAdded: "rgba(24, 112, 63, 0.16)",
  diffRemoved: "rgba(180, 35, 24, 0.14)",
  conflict: "rgba(147, 86, 10, 0.22)",
  copy: "#1F5FB8",
} as const;

export const FONT_FAMILY = '-apple-system, BlinkMacSystemFont, "SF Pro Text", "BDO Grotesk", system-ui, sans-serif';
/** The text size of a cell without its own (10 pt, as in Google Sheets). */
export const DEFAULT_FONT_PX = 13;
const PAD = 4;

export type DiffKind = "changed" | "added" | "removed" | "conflict";

export interface Highlight {
  rect: Rect;
  color: string;
  /** A label drawn on the top edge (another viewer's name). */
  label?: string;
  dashed?: boolean;
  /** Fill the area lightly (formula references). */
  fill?: boolean;
}

export interface SelectionState {
  active: { row: number; col: number };
  range: Rect;
  mode: "cells" | "rows" | "cols" | "all";
}

export interface Scene {
  layout: GridLayout;
  view: ViewportState;
  data: TabData;
  styles: Map<number, Style>;
  selection: SelectionState | null;
  /** True while the cell editor is open (the active cell's text is the editor's). */
  editing: boolean;
  highlights: Highlight[];
  copyRange: Rect | null;
  fillPreview: Rect | null;
  /** Point mode's current reference (dashed, while choosing it). */
  pointing: Rect | null;
  commentCells: Set<number>;
  diff: Map<number, DiffKind> | null;
  /** The spill area of the active cell's formula, when it spills. */
  spill: Rect | null;
  /** Draws placeholders where blocks are still loading. */
  showLoading: boolean;
  /** The fill handle (editors only). */
  fillHandle: boolean;
}

/* ---- text measuring ---------------------------------------------------------------------------------------------- */

const widths = new Map<string, number>();

/** Forgets measured text widths (after a web font finished loading, the same text measures differently). */
export function clearMeasureCache(): void {
  widths.clear();
}

function measure(ctx: CanvasRenderingContext2D, font: string, text: string): number {
  const key = `${font}\u0000${text}`;
  let width = widths.get(key);
  if (width === undefined) {
    if (widths.size > 40_000) widths.clear();
    ctx.font = font;
    width = ctx.measureText(text).width;
    widths.set(key, width);
  }
  return width;
}

export function fontOf(style: Style | undefined): { font: string; px: number } {
  const px = style?.font_size ? Math.round((style.font_size * 96) / 72 * 10) / 10 : DEFAULT_FONT_PX;
  const family = style?.font ? `"${style.font.replace(/"/g, "")}", ${FONT_FAMILY}` : FONT_FAMILY;
  return { font: `${style?.italic ? "italic " : ""}${style?.bold ? 600 : 400} ${px}px ${family}`, px };
}

/** The text a cell shows: the service's formatted text, or the input this browser is still sending. */
export function shownText(cell: CellRecord | undefined): string {
  if (!cell) return "";
  if (cell.t !== undefined && cell.t !== null) return cell.t;
  const v = cell.v;
  if (v.t === "number") return String(v.v);
  if (v.t === "text" || v.t === "error") return v.v;
  if (v.t === "bool") return v.v ? "TRUE" : "FALSE";
  return "";
}

type Kind = "number" | "text" | "bool" | "error" | "empty";

function kindOf(cell: CellRecord | undefined, pending: string | undefined): Kind {
  if (pending !== undefined) {
    if (/^[-+]?(\d[\d,]*\.?\d*|\.\d+)(e[-+]?\d+)?%?$/i.test(pending.trim())) return "number";
    return pending.trim() ? "text" : "empty";
  }
  if (!cell) return "empty";
  return cell.v.t;
}

/** Breaks text into lines that fit `width` (words first, then characters for a word longer than a line). */
export function wrapLines(ctx: CanvasRenderingContext2D, font: string, text: string, width: number): string[] {
  const lines: string[] = [];
  for (const paragraph of text.split("\n")) {
    const words = paragraph.split(/(\s+)/);
    let line = "";
    for (const word of words) {
      const candidate = line + word;
      if (!line || measure(ctx, font, candidate) <= width) {
        line = candidate;
        continue;
      }
      lines.push(line.trimEnd());
      line = word.trimStart();
      while (line && measure(ctx, font, line) > width) {
        let cut = line.length - 1;
        while (cut > 1 && measure(ctx, font, line.slice(0, cut)) > width) cut--;
        lines.push(line.slice(0, cut));
        line = line.slice(cut);
      }
    }
    lines.push(line);
  }
  return lines;
}

/* ---- drawing ----------------------------------------------------------------------------------------------------- */

interface Pane {
  rows: Span;
  cols: Span;
  x: number;
  y: number;
  width: number;
  height: number;
}

function panes(scene: Scene): Pane[] {
  const { layout, view } = scene;
  const visible = visibleRange(layout, view);
  const fh = frozenHeight(layout);
  const fw = frozenWidth(layout);
  const x0 = layout.headerWidth;
  const y0 = layout.headerHeight;
  const out: Pane[] = [];
  const bodyW = Math.max(0, view.width - x0 - fw);
  const bodyH = Math.max(0, view.height - y0 - fh);
  if (visible.rows.end >= visible.rows.start && visible.cols.end >= visible.cols.start) out.push({ rows: visible.rows, cols: visible.cols, x: x0 + fw, y: y0 + fh, width: bodyW, height: bodyH });
  if (layout.frozenCols > 0 && visible.rows.end >= visible.rows.start) out.push({ rows: visible.rows, cols: visible.frozenCols, x: x0, y: y0 + fh, width: fw, height: bodyH });
  if (layout.frozenRows > 0 && visible.cols.end >= visible.cols.start) out.push({ rows: visible.frozenRows, cols: visible.cols, x: x0 + fw, y: y0, width: bodyW, height: fh });
  if (layout.frozenRows > 0 && layout.frozenCols > 0) out.push({ rows: visible.frozenRows, cols: visible.frozenCols, x: x0, y: y0, width: fw, height: fh });
  return out;
}

const crisp = (v: number) => Math.round(v) + 0.5;

export function drawGrid(ctx: CanvasRenderingContext2D, scene: Scene, dpr: number): void {
  const { view } = scene;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.fillStyle = COLORS.background;
  ctx.fillRect(0, 0, view.width, view.height);
  for (const pane of panes(scene)) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(pane.x, pane.y, pane.width, pane.height);
    ctx.clip();
    drawPane(ctx, scene, pane);
    ctx.restore();
  }
  drawOverlays(ctx, scene);
  drawHeaders(ctx, scene);
  drawFrozenLines(ctx, scene);
}

/** Merges that touch a pane's visible rows and columns. */
function mergesIn(data: TabData, pane: Pane): Rect[] {
  if (!data.merges.length) return [];
  return data.merges.filter(m => m.r1 <= pane.rows.end && m.r2 >= pane.rows.start && m.c1 <= pane.cols.end && m.c2 >= pane.cols.start);
}

function cellBox(scene: Scene, row: number, col: number): { x: number; y: number; w: number; h: number } {
  const { layout, view } = scene;
  return { x: colLeft(layout, view, col), y: rowTop(layout, view, row), w: layout.cols.sizeOf(col), h: layout.rows.sizeOf(row) };
}

function mergeBox(scene: Scene, m: Rect): { x: number; y: number; w: number; h: number } {
  const { layout, view } = scene;
  const x = colLeft(layout, view, m.c1);
  const y = rowTop(layout, view, m.r1);
  return { x, y, w: layout.cols.offsetOf(m.c2 + 1) - layout.cols.offsetOf(m.c1), h: layout.rows.offsetOf(m.r2 + 1) - layout.rows.offsetOf(m.r1) };
}

function drawPane(ctx: CanvasRenderingContext2D, scene: Scene, pane: Pane): void {
  const { data, styles, layout } = scene;
  const merges = mergesIn(data, pane);
  const covered = (row: number, col: number) => merges.some(m => row >= m.r1 && row <= m.r2 && col >= m.c1 && col <= m.c2);
  const right = pane.x + pane.width;
  const bottom = pane.y + pane.height;

  // 1. Fills (and placeholders where blocks are still loading).
  const used = scene.showLoading ? data.usedRect() : null;
  for (let row = pane.rows.start; row <= pane.rows.end; row++) {
    const h = layout.rows.sizeOf(row);
    if (!h) continue;
    const y = rowTop(layout, scene.view, row);
    for (let col = pane.cols.start; col <= pane.cols.end; col++) {
      const w = layout.cols.sizeOf(col);
      if (!w) continue;
      const cell = data.get(row, col);
      const style = cell?.s ? styles.get(cell.s) : undefined;
      if (style?.fill && !covered(row, col)) {
        ctx.fillStyle = style.fill;
        ctx.fillRect(colLeft(layout, scene.view, col), y, w, h);
      }
    }
  }
  // 2. Gridlines.
  ctx.beginPath();
  ctx.strokeStyle = COLORS.gridline;
  ctx.lineWidth = 1;
  for (let row = pane.rows.start; row <= pane.rows.end; row++) {
    const h = layout.rows.sizeOf(row);
    if (!h) continue;
    const y = crisp(rowTop(layout, scene.view, row) + h - 1);
    ctx.moveTo(pane.x, y);
    ctx.lineTo(right, y);
  }
  for (let col = pane.cols.start; col <= pane.cols.end; col++) {
    const w = layout.cols.sizeOf(col);
    if (!w) continue;
    const x = crisp(colLeft(layout, scene.view, col) + w - 1);
    ctx.moveTo(x, pane.y);
    ctx.lineTo(x, bottom);
  }
  ctx.stroke();
  // 3. Merged cells: one surface over their inner gridlines.
  for (const m of merges) {
    const box = mergeBox(scene, m);
    const top = data.get(m.r1, m.c1);
    const style = top?.s ? styles.get(top.s) : undefined;
    ctx.fillStyle = style?.fill ?? COLORS.background;
    ctx.fillRect(box.x, box.y, box.w - 1, box.h - 1);
  }
  // 4. Placeholders for blocks not loaded yet (inside the used area only, so empty tabs stay clean).
  if (used) {
    ctx.fillStyle = COLORS.loading;
    for (let row = pane.rows.start; row <= pane.rows.end; row++) {
      if (row > used.r2) break;
      const h = layout.rows.sizeOf(row);
      if (!h) continue;
      const y = rowTop(layout, scene.view, row);
      for (let col = pane.cols.start; col <= Math.min(pane.cols.end, used.c2); col++) {
        if (data.isLoaded(row, col)) continue;
        const w = layout.cols.sizeOf(col);
        if (!w) continue;
        const x = colLeft(layout, scene.view, col);
        const barW = Math.max(8, Math.min(w - 2 * PAD, (w - 2 * PAD) * (0.35 + (((row * 7 + col * 13) % 10) / 20))));
        roundRect(ctx, x + PAD, y + h / 2 - 3, barW, 6, 3);
        ctx.fill();
      }
    }
  }
  // 5. Text, borders and markers.
  const editingKey = scene.editing && scene.selection ? cellKey(scene.selection.active.row, scene.selection.active.col) : -1;
  for (let row = pane.rows.start; row <= pane.rows.end; row++) {
    if (!layout.rows.sizeOf(row)) continue;
    // Text in columns to the left of the pane can overflow into it: start a few columns early.
    const firstCol = Math.max(0, pane.cols.start - 8);
    for (let col = firstCol; col <= pane.cols.end; col++) {
      if (!layout.cols.sizeOf(col)) continue;
      if (covered(row, col)) continue;
      const key = cellKey(row, col);
      if (key === editingKey) continue;
      const cell = data.get(row, col);
      const pending = data.pendingAt(row, col)?.input;
      if (!cell && pending === undefined) continue;
      const box = cellBox(scene, row, col);
      drawCellText(ctx, scene, cell, pending, row, col, box, pane, false);
    }
  }
  for (const m of merges) {
    const top = data.get(m.r1, m.c1);
    const pending = data.pendingAt(m.r1, m.c1)?.input;
    if ((top || pending !== undefined) && cellKey(m.r1, m.c1) !== editingKey) drawCellText(ctx, scene, top, pending, m.r1, m.c1, mergeBox(scene, m), pane, true);
  }
  drawBorders(ctx, scene, pane, merges);
  drawMarkers(ctx, scene, pane, merges);
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  ctx.beginPath();
  if (typeof ctx.roundRect === "function") ctx.roundRect(x, y, w, h, r);
  else ctx.rect(x, y, w, h);
}

/** Whether a cell is empty enough for text to overflow into it. */
function blank(scene: Scene, row: number, col: number): boolean {
  const cell = scene.data.get(row, col);
  if (scene.data.pendingAt(row, col)) return false;
  if (!cell) return !scene.data.mergeAt(row, col);
  return (cell.v.t === "empty" || (cell.v.t === "text" && cell.v.v === "")) && !cell.f && !scene.data.mergeAt(row, col);
}

function drawCellText(ctx: CanvasRenderingContext2D, scene: Scene, cell: CellRecord | undefined, pending: string | undefined, row: number, col: number, box: { x: number; y: number; w: number; h: number }, pane: Pane, merged: boolean): void {
  const style = cell?.s ? scene.styles.get(cell.s) : undefined;
  const text = pending !== undefined ? pending : shownText(cell);
  if (!text) return;
  const kind = kindOf(cell, pending);
  const { font, px } = fontOf(style);
  ctx.font = font;
  let align: "left" | "center" | "right" = "left";
  const h = style?.h_align ?? "general";
  if (h === "left" || h === "center" || h === "right") align = h;
  else if (kind === "number") align = "right";
  else if (kind === "bool" || kind === "error") align = "center";
  const color = pending !== undefined && pending.startsWith("=") ? COLORS.pendingText : kind === "error" ? COLORS.error : style?.color ?? COLORS.text;
  const inner = box.w - 2 * PAD;
  const lineHeight = Math.round(px * 1.3);
  const valign = style?.v_align ?? "bottom";

  if (style?.wrap) {
    const lines = wrapLines(ctx, font, text, Math.max(4, inner));
    ctx.save();
    ctx.beginPath();
    ctx.rect(box.x, box.y, box.w - 1, box.h - 1);
    ctx.clip();
    const total = lines.length * lineHeight;
    let y = valign === "top" ? box.y + 3 : valign === "middle" ? box.y + (box.h - total) / 2 : box.y + box.h - total - 3;
    for (const line of lines) {
      drawLine(ctx, line, align, box.x, box.w, y + lineHeight / 2, style, color, font, px);
      y += lineHeight;
    }
    ctx.restore();
    return;
  }

  const textWidth = measure(ctx, font, text);
  const baseline = valign === "top" ? box.y + 3 + lineHeight / 2 : valign === "middle" ? box.y + box.h / 2 : box.y + box.h - 3 - lineHeight / 2;
  // Numbers that do not fit are never cut: they show ### (as Excel does).
  if (kind === "number" && textWidth > inner) {
    const hashWidth = measure(ctx, font, "#");
    const count = Math.max(1, Math.floor(inner / Math.max(1, hashWidth)));
    ctx.save();
    ctx.beginPath();
    ctx.rect(box.x, box.y, box.w - 1, box.h - 1);
    ctx.clip();
    drawLine(ctx, "#".repeat(count), "right", box.x, box.w, baseline, style, color, font, px);
    ctx.restore();
    return;
  }
  let left = box.x;
  let right = box.x + box.w;
  if (textWidth > inner && !merged && kind === "text") {
    const { layout } = scene;
    // Overflow into empty neighbours, the way the text is aligned.
    if (align === "left" || align === "center") {
      let c = col + 1;
      while (right - box.x - 2 * PAD < textWidth && c < layout.cols.count && blank(scene, row, c)) {
        right += layout.cols.sizeOf(c);
        c++;
        if (right > pane.x + pane.width + 2000) break;
      }
    }
    if (align === "right" || align === "center") {
      let c = col - 1;
      while (right - left - 2 * PAD < textWidth && c >= 0 && blank(scene, row, c)) {
        left -= layout.cols.sizeOf(c);
        c--;
        if (left < pane.x - 2000) break;
      }
    }
    // The gridlines under overflowing text are hidden (painted over in the neighbours' fill).
    if (right > box.x + box.w || left < box.x) {
      ctx.fillStyle = COLORS.background;
      if (right > box.x + box.w) ctx.fillRect(box.x + box.w - 1, box.y, right - (box.x + box.w), box.h - 1);
      if (left < box.x) ctx.fillRect(left, box.y, box.x - left, box.h - 1);
      for (let c = col + 1; c < scene.layout.cols.count; c++) {
        const x = colLeft(scene.layout, scene.view, c);
        if (x >= right) break;
        const neighbour = scene.data.get(row, c);
        const fill = neighbour?.s ? scene.styles.get(neighbour.s)?.fill : undefined;
        if (fill) {
          ctx.fillStyle = fill;
          ctx.fillRect(x, box.y, Math.min(scene.layout.cols.sizeOf(c), right - x) - 1, box.h - 1);
        }
      }
    }
  }
  ctx.save();
  ctx.beginPath();
  ctx.rect(left, box.y, right - left - 1, box.h - 1);
  ctx.clip();
  drawLine(ctx, text, align, left, right - left, baseline, style, color, font, px);
  ctx.restore();
}

function drawLine(ctx: CanvasRenderingContext2D, text: string, align: "left" | "center" | "right", x: number, w: number, y: number, style: Style | undefined, color: string, font: string, px: number): void {
  ctx.font = font;
  ctx.fillStyle = color;
  ctx.textBaseline = "middle";
  ctx.textAlign = align;
  const tx = align === "left" ? x + PAD : align === "right" ? x + w - PAD - 1 : x + w / 2;
  ctx.fillText(text, tx, y);
  if (style?.underline || style?.strike) {
    const width = measure(ctx, font, text);
    const start = align === "left" ? tx : align === "right" ? tx - width : tx - width / 2;
    ctx.strokeStyle = color;
    ctx.lineWidth = Math.max(1, px / 14);
    ctx.beginPath();
    if (style.underline) {
      ctx.moveTo(start, Math.round(y + px * 0.45) + 0.5);
      ctx.lineTo(start + width, Math.round(y + px * 0.45) + 0.5);
    }
    if (style.strike) {
      ctx.moveTo(start, Math.round(y) + 0.5);
      ctx.lineTo(start + width, Math.round(y) + 0.5);
    }
    ctx.stroke();
  }
}

const BORDER_WIDTH: Record<string, number> = { thin: 1, medium: 2, thick: 3, dashed: 1, dotted: 1, double: 3 };

function drawBorders(ctx: CanvasRenderingContext2D, scene: Scene, pane: Pane, merges: Rect[]): void {
  const { data, styles, layout } = scene;
  const sides = ["top", "bottom", "left", "right"] as const;
  const drawSide = (x1: number, y1: number, x2: number, y2: number, border: { style: string; color?: string }) => {
    const width = BORDER_WIDTH[border.style] ?? 1;
    ctx.strokeStyle = border.color ?? "#000000";
    ctx.lineWidth = width;
    ctx.setLineDash(border.style === "dashed" ? [4, 2] : border.style === "dotted" ? [1, 2] : []);
    ctx.beginPath();
    if (border.style === "double") {
      const horizontal = y1 === y2;
      ctx.lineWidth = 1;
      const a = horizontal ? [0, -1] : [-1, 0];
      const b = horizontal ? [0, 1] : [1, 0];
      ctx.moveTo(x1 + a[0]!, y1 + a[1]!);
      ctx.lineTo(x2 + a[0]!, y2 + a[1]!);
      ctx.moveTo(x1 + b[0]!, y1 + b[1]!);
      ctx.lineTo(x2 + b[0]!, y2 + b[1]!);
    } else {
      ctx.moveTo(x1, y1);
      ctx.lineTo(x2, y2);
    }
    ctx.stroke();
  };
  const boxOf = (row: number, col: number) => {
    const m = merges.find(r => r.r1 === row && r.c1 === col);
    return m ? mergeBox(scene, m) : cellBox(scene, row, col);
  };
  for (let row = pane.rows.start; row <= pane.rows.end; row++) {
    if (!layout.rows.sizeOf(row)) continue;
    for (let col = pane.cols.start; col <= pane.cols.end; col++) {
      if (!layout.cols.sizeOf(col)) continue;
      const cell = data.get(row, col);
      if (!cell?.s) continue;
      const style = styles.get(cell.s);
      if (!style || !(style.border_top || style.border_bottom || style.border_left || style.border_right)) continue;
      const box = boxOf(row, col);
      for (const side of sides) {
        const border = style[`border_${side}`];
        if (!border || border.style === "none") continue;
        const left = crisp(box.x - 1);
        const top = crisp(box.y - 1);
        const right = crisp(box.x + box.w - 1);
        const bottom = crisp(box.y + box.h - 1);
        if (side === "top") drawSide(left, top, right, top, border);
        if (side === "bottom") drawSide(left, bottom, right, bottom, border);
        if (side === "left") drawSide(left, top, left, bottom, border);
        if (side === "right") drawSide(right, top, right, bottom, border);
      }
    }
  }
  ctx.setLineDash([]);
}

function drawMarkers(ctx: CanvasRenderingContext2D, scene: Scene, pane: Pane, merges: Rect[]): void {
  const { layout, view, data } = scene;
  // Protected ranges: a quiet diagonal hatch.
  for (const protection of data.protections) {
    const r = protection.range ? rangeOf(protection.range, data) : { r1: 0, c1: 0, r2: layout.rows.count - 1, c2: layout.cols.count - 1 };
    if (!r) continue;
    const r1 = Math.max(r.r1, pane.rows.start);
    const r2 = Math.min(r.r2, pane.rows.end);
    const c1 = Math.max(r.c1, pane.cols.start);
    const c2 = Math.min(r.c2, pane.cols.end);
    if (r1 > r2 || c1 > c2) continue;
    const x = colLeft(layout, view, c1);
    const y = rowTop(layout, view, r1);
    const w = colLeft(layout, view, c2) + layout.cols.sizeOf(c2) - x;
    const h = rowTop(layout, view, r2) + layout.rows.sizeOf(r2) - y;
    ctx.save();
    ctx.beginPath();
    ctx.rect(x, y, w, h);
    ctx.clip();
    ctx.strokeStyle = COLORS.protectedHatch;
    ctx.lineWidth = 1;
    ctx.beginPath();
    const step = 10;
    // Hatch lines anchored to the content (not the screen), so they scroll with the cells.
    const phase = (layout.cols.offsetOf(c1) + layout.rows.offsetOf(r1)) % step;
    for (let d = -h - phase; d < w + h; d += step) {
      ctx.moveTo(x + d, y + h);
      ctx.lineTo(x + d + h, y);
    }
    ctx.stroke();
    ctx.restore();
  }
  // Compare highlights.
  if (scene.diff?.size) {
    for (let row = pane.rows.start; row <= pane.rows.end; row++) {
      if (!layout.rows.sizeOf(row)) continue;
      for (let col = pane.cols.start; col <= pane.cols.end; col++) {
        const kind = scene.diff.get(cellKey(row, col));
        if (!kind || !layout.cols.sizeOf(col)) continue;
        const box = cellBox(scene, row, col);
        ctx.fillStyle = kind === "added" ? COLORS.diffAdded : kind === "removed" ? COLORS.diffRemoved : kind === "conflict" ? COLORS.conflict : COLORS.diffChanged;
        ctx.fillRect(box.x, box.y, box.w - 1, box.h - 1);
      }
    }
  }
  // Comment markers: a small triangle in the top right corner of the cell a thread starts on.
  if (scene.commentCells.size) {
    ctx.fillStyle = COLORS.comment;
    for (const key of scene.commentCells) {
      const row = Math.floor(key / 16_384);
      const col = key % 16_384;
      if (row < pane.rows.start || row > pane.rows.end || col < pane.cols.start || col > pane.cols.end) continue;
      const m = merges.find(r => r.r1 === row && r.c1 === col);
      const box = m ? mergeBox(scene, m) : cellBox(scene, row, col);
      if (!box.w || !box.h) continue;
      const x = box.x + box.w - 1;
      ctx.beginPath();
      ctx.moveTo(x - 7, box.y);
      ctx.lineTo(x, box.y);
      ctx.lineTo(x, box.y + 7);
      ctx.closePath();
      ctx.fill();
    }
  }
}

function rangeOf(text: string, data: TabData): Rect | null {
  const parts = text.replace(/\$/g, "").split(":");
  const parse = (part: string) => {
    const m = /^([A-Za-z]{1,3})(\d+)$/.exec(part);
    if (!m) return null;
    let col = 0;
    for (const ch of m[1]!.toUpperCase()) col = col * 26 + (ch.charCodeAt(0) - 64);
    return { row: Number(m[2]) - 1, col: col - 1 };
  };
  if (parts.length === 2 && /^[A-Za-z]+$/.test(parts[0]!) && /^[A-Za-z]+$/.test(parts[1]!)) {
    const a = parse(`${parts[0]}1`);
    const b = parse(`${parts[1]}1`);
    if (!a || !b) return null;
    return { r1: 0, c1: Math.min(a.col, b.col), r2: data.rows.count - 1, c2: Math.max(a.col, b.col) };
  }
  if (parts.length === 2 && /^\d+$/.test(parts[0]!) && /^\d+$/.test(parts[1]!)) {
    return { r1: Number(parts[0]) - 1, c1: 0, r2: Number(parts[1]) - 1, c2: data.cols.count - 1 };
  }
  const a = parse(parts[0]!);
  const b = parse(parts[1] ?? parts[0]!);
  if (!a || !b) return null;
  return { r1: Math.min(a.row, b.row), c1: Math.min(a.col, b.col), r2: Math.max(a.row, b.row), c2: Math.max(a.col, b.col) };
}

/* ---- overlays: highlights, presence, copy, selection ------------------------------------------------------------- */

/** A rectangle on screen for a block of cells, clipped to the cell area (it may span panes). */
function screenRect(scene: Scene, r: Rect): { x: number; y: number; w: number; h: number } | null {
  const { layout, view } = scene;
  const r2 = Math.min(r.r2, layout.rows.count - 1);
  const c2 = Math.min(r.c2, layout.cols.count - 1);
  const top = rowTop(layout, view, r.r1);
  const left = colLeft(layout, view, r.c1);
  let x = left;
  let y = top;
  let right = colLeft(layout, view, c2) + layout.cols.sizeOf(c2);
  let bottom = rowTop(layout, view, r2) + layout.rows.sizeOf(r2);
  // A block that starts in the frozen pane and ends in the scrolling one runs continuously across the divider.
  if (r.r1 < layout.frozenRows && r2 >= layout.frozenRows) bottom = Math.max(bottom, layout.headerHeight + frozenHeight(layout));
  if (r.c1 < layout.frozenCols && c2 >= layout.frozenCols) right = Math.max(right, layout.headerWidth + frozenWidth(layout));
  // A block scrolled under the frozen panes starts at the divider.
  if (r.r1 >= layout.frozenRows) y = Math.max(y, layout.headerHeight + frozenHeight(layout) - 2);
  if (r.c1 >= layout.frozenCols) x = Math.max(x, layout.headerWidth + frozenWidth(layout) - 2);
  if (right <= x || bottom <= y) return null;
  return { x, y, w: right - x, h: bottom - y };
}

function strokeRect(ctx: CanvasRenderingContext2D, box: { x: number; y: number; w: number; h: number }, color: string, width: number, dash: number[] = []): void {
  ctx.strokeStyle = color;
  ctx.lineWidth = width;
  ctx.setLineDash(dash);
  const inset = width / 2;
  ctx.strokeRect(Math.round(box.x - 1) + inset, Math.round(box.y - 1) + inset, Math.round(box.w) - width + 1, Math.round(box.h) - width + 1);
  ctx.setLineDash([]);
}

function drawOverlays(ctx: CanvasRenderingContext2D, scene: Scene): void {
  const { layout, view } = scene;
  ctx.save();
  ctx.beginPath();
  ctx.rect(layout.headerWidth, layout.headerHeight, view.width - layout.headerWidth, view.height - layout.headerHeight);
  ctx.clip();
  // Other viewers' selections, with their id on a tag.
  for (const highlight of scene.highlights) {
    const box = screenRect(scene, highlight.rect);
    if (!box) continue;
    if (highlight.fill) {
      ctx.fillStyle = `${highlight.color}1A`;
      ctx.fillRect(box.x, box.y, box.w - 1, box.h - 1);
    }
    strokeRect(ctx, box, highlight.color, highlight.label ? 2 : 1.5, highlight.dashed ? [4, 3] : []);
    if (highlight.label) {
      ctx.font = `500 11px ${FONT_FAMILY}`;
      const w = measure(ctx, ctx.font, highlight.label) + 10;
      const tagY = box.y - 1 > layout.headerHeight + 16 ? box.y - 17 : box.y + box.h;
      ctx.fillStyle = highlight.color;
      roundRect(ctx, box.x + box.w - w - 1, tagY, w, 16, 4);
      ctx.fill();
      ctx.fillStyle = "#FFFFFF";
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.fillText(highlight.label, box.x + box.w - w + 4, tagY + 8.5);
    }
  }
  if (scene.spill) {
    const box = screenRect(scene, scene.spill);
    if (box) strokeRect(ctx, box, COLORS.spill, 1, [4, 3]);
  }
  if (scene.copyRange) {
    const box = screenRect(scene, scene.copyRange);
    if (box) strokeRect(ctx, box, COLORS.copy, 2, [6, 4]);
  }
  const selection = scene.selection;
  if (selection) {
    const box = screenRect(scene, selection.range);
    const single = selection.range.r1 === selection.range.r2 && selection.range.c1 === selection.range.c2;
    const merge = scene.data.mergeAt(selection.active.row, selection.active.col);
    const activeRect = merge ?? { r1: selection.active.row, c1: selection.active.col, r2: selection.active.row, c2: selection.active.col };
    const isMergeOnly = merge !== null && merge.r1 === selection.range.r1 && merge.c1 === selection.range.c1 && merge.r2 === selection.range.r2 && merge.c2 === selection.range.c2;
    if (box && !single && !isMergeOnly) {
      // A light tint over the selection, leaving the active cell clear (it is the one being typed into).
      const activeBox = screenRect(scene, activeRect);
      ctx.fillStyle = COLORS.selectionFill;
      ctx.beginPath();
      ctx.rect(box.x, box.y, box.w - 1, box.h - 1);
      if (activeBox) ctx.rect(activeBox.x, activeBox.y, activeBox.w - 1, activeBox.h - 1);
      ctx.fill("evenodd");
      strokeRect(ctx, box, COLORS.selection, 1);
    }
    const activeBox = screenRect(scene, activeRect);
    if (activeBox) strokeRect(ctx, activeBox, COLORS.selection, 2);
    if (scene.pointing) {
      const p = screenRect(scene, scene.pointing);
      if (p) strokeRect(ctx, p, COLORS.selection, 1.5, [5, 3]);
    }
    if (scene.fillPreview) {
      const p = screenRect(scene, scene.fillPreview);
      if (p) strokeRect(ctx, p, "#5C6370", 1, [4, 3]);
    }
    // The fill handle: a small square at the selection's bottom right corner.
    if (box && scene.fillHandle && !scene.editing) {
      const x = Math.round(box.x + box.w - 1);
      const y = Math.round(box.y + box.h - 1);
      ctx.fillStyle = COLORS.background;
      ctx.fillRect(x - 4, y - 4, 8, 8);
      ctx.fillStyle = COLORS.selection;
      ctx.fillRect(x - 3, y - 3, 6, 6);
    }
  }
  ctx.restore();
}

/* ---- headers and frozen dividers --------------------------------------------------------------------------------- */

function drawHeaders(ctx: CanvasRenderingContext2D, scene: Scene): void {
  const { layout, view, selection } = scene;
  const visible = visibleRange(layout, view);
  const hh = layout.headerHeight;
  const hw = layout.headerWidth;
  const fh = frozenHeight(layout);
  const fw = frozenWidth(layout);
  const sel = selection?.range;
  ctx.font = `400 12px ${FONT_FAMILY}`;
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";

  // Column header.
  ctx.save();
  ctx.beginPath();
  ctx.rect(hw, 0, view.width - hw, hh);
  ctx.clip();
  ctx.fillStyle = COLORS.header;
  ctx.fillRect(hw, 0, view.width - hw, hh);
  const colSpans: Array<{ span: Span; clipX: number; clipW: number }> = [
    { span: visible.frozenCols, clipX: hw, clipW: fw },
    { span: visible.cols, clipX: hw + fw, clipW: view.width - hw - fw },
  ];
  for (const { span, clipX, clipW } of colSpans) {
    if (span.end < span.start) continue;
    ctx.save();
    ctx.beginPath();
    ctx.rect(clipX, 0, clipW, hh);
    ctx.clip();
    for (let col = span.start; col <= span.end; col++) {
      const w = layout.cols.sizeOf(col);
      if (!w) continue;
      const x = colLeft(layout, view, col);
      const inSelection = sel && col >= sel.c1 && col <= sel.c2;
      const whole = selection && (selection.mode === "cols" || selection.mode === "all") && inSelection;
      if (inSelection) {
        ctx.fillStyle = whole ? COLORS.headerSelected : COLORS.headerActive;
        ctx.fillRect(x, 0, w, hh);
      }
      ctx.fillStyle = whole ? "#FFFFFF" : inSelection ? COLORS.headerActiveText : COLORS.headerText;
      ctx.font = `${inSelection ? 500 : 400} 12px ${FONT_FAMILY}`;
      ctx.fillText(colName(col), x + w / 2, hh / 2 + 0.5);
      ctx.fillStyle = COLORS.headerLine;
      ctx.fillRect(Math.round(x + w - 1), 0, 1, hh);
      // A hidden column after this one: a small double mark on the edge.
      if (col + 1 < layout.cols.count && layout.cols.isHidden(col + 1)) {
        ctx.fillStyle = COLORS.headerText;
        ctx.fillRect(Math.round(x + w - 3), 5, 1, hh - 10);
        ctx.fillRect(Math.round(x + w + 1), 5, 1, hh - 10);
      }
    }
    ctx.restore();
  }
  ctx.fillStyle = COLORS.headerLine;
  ctx.fillRect(hw, hh - 1, view.width - hw, 1);
  ctx.restore();

  // Row header.
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, hh, hw, view.height - hh);
  ctx.clip();
  ctx.fillStyle = COLORS.header;
  ctx.fillRect(0, hh, hw, view.height - hh);
  const rowSpans: Array<{ span: Span; clipY: number; clipH: number }> = [
    { span: visible.frozenRows, clipY: hh, clipH: fh },
    { span: visible.rows, clipY: hh + fh, clipH: view.height - hh - fh },
  ];
  for (const { span, clipY, clipH } of rowSpans) {
    if (span.end < span.start) continue;
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, clipY, hw, clipH);
    ctx.clip();
    for (let row = span.start; row <= span.end; row++) {
      const h = layout.rows.sizeOf(row);
      if (!h) continue;
      const y = rowTop(layout, view, row);
      const inSelection = sel && row >= sel.r1 && row <= sel.r2;
      const whole = selection && (selection.mode === "rows" || selection.mode === "all") && inSelection;
      if (inSelection) {
        ctx.fillStyle = whole ? COLORS.headerSelected : COLORS.headerActive;
        ctx.fillRect(0, y, hw, h);
      }
      ctx.fillStyle = whole ? "#FFFFFF" : inSelection ? COLORS.headerActiveText : COLORS.headerText;
      ctx.font = `${inSelection ? 500 : 400} 12px ${FONT_FAMILY}`;
      ctx.fillText(String(row + 1), hw / 2, y + h / 2 + 0.5);
      ctx.fillStyle = COLORS.headerLine;
      ctx.fillRect(0, Math.round(y + h - 1), hw, 1);
      if (row + 1 < layout.rows.count && layout.rows.isHidden(row + 1)) {
        ctx.fillStyle = COLORS.headerText;
        ctx.fillRect(5, Math.round(y + h - 3), hw - 10, 1);
        ctx.fillRect(5, Math.round(y + h + 1), hw - 10, 1);
      }
    }
    ctx.restore();
  }
  ctx.fillStyle = COLORS.headerLine;
  ctx.fillRect(hw - 1, hh, 1, view.height - hh);
  ctx.restore();

  // Corner (select all).
  const all = selection?.mode === "all";
  ctx.fillStyle = all ? COLORS.headerSelected : COLORS.header;
  ctx.fillRect(0, 0, hw, hh);
  ctx.fillStyle = COLORS.headerLine;
  ctx.fillRect(hw - 1, 0, 1, hh);
  ctx.fillRect(0, hh - 1, hw, 1);
  ctx.fillStyle = all ? "#FFFFFF" : "#B9BFC9";
  ctx.beginPath();
  ctx.moveTo(hw - 5, hh - 14);
  ctx.lineTo(hw - 5, hh - 5);
  ctx.lineTo(hw - 14, hh - 5);
  ctx.closePath();
  ctx.fill();
}

function drawFrozenLines(ctx: CanvasRenderingContext2D, scene: Scene): void {
  const { layout, view } = scene;
  ctx.fillStyle = COLORS.frozenLine;
  if (layout.frozenRows > 0) ctx.fillRect(0, Math.round(layout.headerHeight + frozenHeight(layout) - 2), view.width, 2);
  if (layout.frozenCols > 0) ctx.fillRect(Math.round(layout.headerWidth + frozenWidth(layout) - 2), 0, 2, view.height);
}
