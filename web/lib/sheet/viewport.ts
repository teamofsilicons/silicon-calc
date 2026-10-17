/**
 * Virtualization math for the grid: which rows and columns a viewport shows, where each one lands on screen, what sits
 * under the pointer, and how far to scroll to bring a cell into view. The grid has up to four panes: frozen rows and
 * columns stay put, the rest scrolls. Scroll offsets here are in content pixels of the scrolling pane (0 = the first
 * row and column after the frozen ones), never DOM scroll positions (see geometry.ts `scrollMap`).
 */
import type { AxisSizes } from "./geometry";

export interface GridLayout {
  rows: AxisSizes;
  cols: AxisSizes;
  frozenRows: number;
  frozenCols: number;
  /** The row header's width and the column header's height (px). */
  headerWidth: number;
  headerHeight: number;
}

export interface ViewportState {
  /** The canvas' size in CSS px. */
  width: number;
  height: number;
  scrollX: number;
  scrollY: number;
}

export interface Span {
  start: number;
  end: number;
}

export interface Visible {
  /** Frozen positions (empty when none: end < start). */
  frozenRows: Span;
  frozenCols: Span;
  /** The scrolling positions in view. */
  rows: Span;
  cols: Span;
}

export const frozenHeight = (layout: GridLayout) => layout.rows.offsetOf(layout.frozenRows);
export const frozenWidth = (layout: GridLayout) => layout.cols.offsetOf(layout.frozenCols);

/** The scrolling pane's size on screen. */
export function bodySize(layout: GridLayout, view: ViewportState): { width: number; height: number } {
  return {
    width: Math.max(0, view.width - layout.headerWidth - frozenWidth(layout)),
    height: Math.max(0, view.height - layout.headerHeight - frozenHeight(layout)),
  };
}

/** How far the scrolling pane can scroll: the last row and column can reach the far edge, never further. */
export function scrollLimits(layout: GridLayout, view: ViewportState): { maxX: number; maxY: number } {
  const body = bodySize(layout, view);
  const contentH = layout.rows.total() - frozenHeight(layout);
  const contentW = layout.cols.total() - frozenWidth(layout);
  return { maxX: Math.max(0, contentW - body.width), maxY: Math.max(0, contentH - body.height) };
}

export function clampScroll(layout: GridLayout, view: ViewportState, x: number, y: number): { x: number; y: number } {
  const { maxX, maxY } = scrollLimits(layout, view);
  return { x: Math.min(Math.max(0, x), maxX), y: Math.min(Math.max(0, y), maxY) };
}

function visibleSpan(axis: AxisSizes, frozen: number, scroll: number, size: number): Span {
  if (axis.count <= frozen) return { start: frozen, end: frozen - 1 };
  const top = axis.offsetOf(frozen) + scroll;
  const start = Math.max(frozen, axis.indexAt(top));
  const end = Math.min(axis.count - 1, axis.indexAt(top + Math.max(0, size - 1)));
  return { start, end: Math.max(start, end) };
}

/** Which rows and columns are on screen (frozen ones plus the scrolling ones in view). */
export function visibleRange(layout: GridLayout, view: ViewportState): Visible {
  const body = bodySize(layout, view);
  return {
    frozenRows: { start: 0, end: Math.min(layout.frozenRows, layout.rows.count) - 1 },
    frozenCols: { start: 0, end: Math.min(layout.frozenCols, layout.cols.count) - 1 },
    rows: visibleSpan(layout.rows, layout.frozenRows, view.scrollY, body.height),
    cols: visibleSpan(layout.cols, layout.frozenCols, view.scrollX, body.width),
  };
}

/** Where a row starts on screen (CSS px from the canvas' top). */
export function rowTop(layout: GridLayout, view: ViewportState, row: number): number {
  if (row < layout.frozenRows) return layout.headerHeight + layout.rows.offsetOf(row);
  return layout.headerHeight + frozenHeight(layout) + layout.rows.offsetOf(row) - layout.rows.offsetOf(layout.frozenRows) - view.scrollY;
}

/** Where a column starts on screen (CSS px from the canvas' left). */
export function colLeft(layout: GridLayout, view: ViewportState, col: number): number {
  if (col < layout.frozenCols) return layout.headerWidth + layout.cols.offsetOf(col);
  return layout.headerWidth + frozenWidth(layout) + layout.cols.offsetOf(col) - layout.cols.offsetOf(layout.frozenCols) - view.scrollX;
}

export interface ScreenRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A block of cells on screen (it may run under the frozen panes or off screen; callers clip). */
export function rangeOnScreen(layout: GridLayout, view: ViewportState, r1: number, c1: number, r2: number, c2: number): ScreenRect {
  const x = colLeft(layout, view, c1);
  const y = rowTop(layout, view, r1);
  const right = colLeft(layout, view, c2) + layout.cols.sizeOf(c2);
  const bottom = rowTop(layout, view, r2) + layout.rows.sizeOf(r2);
  // A block that spans the frozen edge is measured in content space so it does not fold over itself.
  const width = c1 < layout.frozenCols && c2 >= layout.frozenCols ? layout.cols.offsetOf(c2 + 1) - layout.cols.offsetOf(c1) : right - x;
  const height = r1 < layout.frozenRows && r2 >= layout.frozenRows ? layout.rows.offsetOf(r2 + 1) - layout.rows.offsetOf(r1) : bottom - y;
  return { x, y, width, height };
}

export type HitArea = "corner" | "col-header" | "row-header" | "cell";

export interface Hit {
  area: HitArea;
  row: number;
  col: number;
}

function axisHit(axis: AxisSizes, frozen: number, scroll: number, header: number, frozenSize: number, at: number): number {
  const local = at - header;
  if (local < frozenSize) return axis.indexAt(local);
  return axis.indexAt(axis.offsetOf(frozen) + scroll + (local - frozenSize));
}

/** What is under a point on the canvas (CSS px). Off-grid points clamp to the nearest row and column. */
export function hitTest(layout: GridLayout, view: ViewportState, x: number, y: number): Hit {
  const inColHeader = y < layout.headerHeight;
  const inRowHeader = x < layout.headerWidth;
  const row = axisHit(layout.rows, layout.frozenRows, view.scrollY, layout.headerHeight, frozenHeight(layout), Math.max(layout.headerHeight, y));
  const col = axisHit(layout.cols, layout.frozenCols, view.scrollX, layout.headerWidth, frozenWidth(layout), Math.max(layout.headerWidth, x));
  if (inColHeader && inRowHeader) return { area: "corner", row: 0, col: 0 };
  if (inColHeader) return { area: "col-header", row, col };
  if (inRowHeader) return { area: "row-header", row, col };
  return { area: "cell", row, col };
}

/** The edge of a column or row header a pointer is on (to resize it), within `slop` px; null when none. */
export function resizeEdge(layout: GridLayout, view: ViewportState, x: number, y: number, slop = 4): { axis: "col" | "row"; index: number } | null {
  if (y < layout.headerHeight && x > layout.headerWidth) {
    const hit = hitTest(layout, view, x, y);
    const left = colLeft(layout, view, hit.col);
    const right = left + layout.cols.sizeOf(hit.col);
    if (Math.abs(x - right) <= slop) return { axis: "col", index: hit.col };
    if (Math.abs(x - left) <= slop && hit.col > 0) {
      const previous = layout.cols.nextVisible(hit.col, -1);
      if (previous !== hit.col) return { axis: "col", index: previous };
    }
  }
  if (x < layout.headerWidth && y > layout.headerHeight) {
    const hit = hitTest(layout, view, x, y);
    const top = rowTop(layout, view, hit.row);
    const bottom = top + layout.rows.sizeOf(hit.row);
    if (Math.abs(y - bottom) <= slop) return { axis: "row", index: hit.row };
    if (Math.abs(y - top) <= slop && hit.row > 0) {
      const previous = layout.rows.nextVisible(hit.row, -1);
      if (previous !== hit.row) return { axis: "row", index: previous };
    }
  }
  return null;
}

/**
 * The scroll offsets that bring a cell into view with the least movement (frozen rows and columns are always in view,
 * so their axis stays where it is).
 */
export function scrollToShow(layout: GridLayout, view: ViewportState, row: number, col: number): { x: number; y: number } {
  const body = bodySize(layout, view);
  let { scrollX: x, scrollY: y } = view;
  if (row >= layout.frozenRows) {
    const top = layout.rows.offsetOf(row) - layout.rows.offsetOf(layout.frozenRows);
    const bottom = top + layout.rows.sizeOf(row);
    if (top < y) y = top;
    // A row taller than the pane shows its top.
    else if (bottom > y + body.height) y = bottom - top >= body.height ? top : bottom - body.height;
  }
  if (col >= layout.frozenCols) {
    const left = layout.cols.offsetOf(col) - layout.cols.offsetOf(layout.frozenCols);
    const right = left + layout.cols.sizeOf(col);
    if (left < x) x = left;
    else if (right > x + body.width) x = right - left >= body.width ? left : right - body.width;
  }
  return clampScroll(layout, view, x, y);
}
