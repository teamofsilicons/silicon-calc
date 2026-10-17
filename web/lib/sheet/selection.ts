/**
 * The selection and how keys move it, as in Google Sheets and Excel: one range with an active cell inside it, whole
 * rows and columns, select all, shift to extend, ctrl/cmd + arrow to jump to the edge of the data, merged cells taken
 * whole, hidden rows and columns skipped. Pure functions over a small view of the tab (`SelectionGrid`), so they are
 * tested without a browser.
 */
import { type CellPos, type Rect, rectContains } from "./address";

export type SelectionMode = "cells" | "rows" | "cols" | "all";

export interface Selection {
  /** The cell the range was started from (shift-extends from here). */
  anchor: CellPos;
  /** The cell typing goes into. */
  active: CellPos;
  range: Rect;
  mode: SelectionMode;
}

/** What selection needs to know about a tab. */
export interface SelectionGrid {
  rowCount: number;
  colCount: number;
  rowHidden(row: number): boolean;
  colHidden(col: number): boolean;
  mergeAt(row: number, col: number): Rect | null;
  /** Whether a cell holds something; undefined when not loaded (the caller loads and asks again). */
  filled(row: number, col: number): boolean | undefined;
}

export type Direction = "up" | "down" | "left" | "right";

export const cellSelection = (row: number, col: number): Selection => ({ anchor: { row, col }, active: { row, col }, range: { r1: row, c1: col, r2: row, c2: col }, mode: "cells" });

/** Grows a rectangle until every merge it touches is inside it. */
export function expandToMerges(r: Rect, merges: Rect[]): Rect {
  let out = { ...r };
  for (let pass = 0; pass < 8; pass++) {
    let grew = false;
    for (const m of merges) {
      if (m.r1 <= out.r2 && m.r2 >= out.r1 && m.c1 <= out.c2 && m.c2 >= out.c1) {
        const next = { r1: Math.min(out.r1, m.r1), c1: Math.min(out.c1, m.c1), r2: Math.max(out.r2, m.r2), c2: Math.max(out.c2, m.c2) };
        if (next.r1 !== out.r1 || next.c1 !== out.c1 || next.r2 !== out.r2 || next.c2 !== out.c2) {
          out = next;
          grew = true;
        }
      }
    }
    if (!grew) break;
  }
  return out;
}

/** Selects one cell (its whole merge, when it is merged). */
export function selectCell(grid: SelectionGrid, row: number, col: number): Selection {
  const merge = grid.mergeAt(row, col);
  const active = merge ? { row: merge.r1, col: merge.c1 } : { row, col };
  return { anchor: active, active, range: merge ?? { r1: row, c1: col, r2: row, c2: col }, mode: "cells" };
}

/** Extends from the anchor to a cell (shift-click, shift-arrow), taking merges whole. */
export function extendTo(grid: SelectionGrid, selection: Selection, row: number, col: number, merges: Rect[]): Selection {
  if (selection.mode === "rows") {
    const r1 = Math.min(selection.anchor.row, row);
    const r2 = Math.max(selection.anchor.row, row);
    return { ...selection, range: { r1, c1: 0, r2, c2: grid.colCount - 1 } };
  }
  if (selection.mode === "cols") {
    const c1 = Math.min(selection.anchor.col, col);
    const c2 = Math.max(selection.anchor.col, col);
    return { ...selection, range: { r1: 0, c1, r2: grid.rowCount - 1, c2 } };
  }
  const base = { r1: Math.min(selection.anchor.row, row), c1: Math.min(selection.anchor.col, col), r2: Math.max(selection.anchor.row, row), c2: Math.max(selection.anchor.col, col) };
  return { ...selection, mode: "cells", range: expandToMerges(base, merges) };
}

export function selectRows(grid: SelectionGrid, from: number, to: number, activeCol = 0): Selection {
  const r1 = Math.min(from, to);
  const r2 = Math.max(from, to);
  return { anchor: { row: from, col: activeCol }, active: { row: from, col: activeCol }, range: { r1, c1: 0, r2, c2: grid.colCount - 1 }, mode: "rows" };
}

export function selectCols(grid: SelectionGrid, from: number, to: number, activeRow = 0): Selection {
  const c1 = Math.min(from, to);
  const c2 = Math.max(from, to);
  return { anchor: { row: activeRow, col: from }, active: { row: activeRow, col: from }, range: { r1: 0, c1, r2: grid.rowCount - 1, c2 }, mode: "cols" };
}

export function selectAll(grid: SelectionGrid, active: CellPos = { row: 0, col: 0 }): Selection {
  return { anchor: active, active, range: { r1: 0, c1: 0, r2: grid.rowCount - 1, c2: grid.colCount - 1 }, mode: "all" };
}

/** One step from a cell, over hidden rows and columns and out of the merge it is in. */
export function step(grid: SelectionGrid, pos: CellPos, dir: Direction): CellPos {
  let { row, col } = pos;
  const merge = grid.mergeAt(row, col);
  if (merge) {
    if (dir === "down") row = merge.r2;
    if (dir === "up") row = merge.r1;
    if (dir === "right") col = merge.c2;
    if (dir === "left") col = merge.c1;
  }
  const dr = dir === "down" ? 1 : dir === "up" ? -1 : 0;
  const dc = dir === "right" ? 1 : dir === "left" ? -1 : 0;
  let r = row + dr;
  let c = col + dc;
  while (r >= 0 && r < grid.rowCount && grid.rowHidden(r)) r += dr || 1;
  while (c >= 0 && c < grid.colCount && grid.colHidden(c)) c += dc || 1;
  if (r < 0 || r >= grid.rowCount) r = pos.row;
  if (c < 0 || c >= grid.colCount) c = pos.col;
  const target = grid.mergeAt(r, c);
  return target ? { row: target.r1, col: target.c1 } : { row: r, col: c };
}

/**
 * Where ctrl/cmd + arrow lands (Excel's rule): from a filled cell whose neighbour is filled, the last filled cell
 * before an empty one; otherwise the next filled cell; the grid's edge when there is none. `null` when a cell on the
 * way is not loaded yet (the caller loads it and asks again).
 */
export function jump(grid: SelectionGrid, pos: CellPos, dir: Direction, limit = 1_000_000): CellPos | null {
  const dr = dir === "down" ? 1 : dir === "up" ? -1 : 0;
  const dc = dir === "right" ? 1 : dir === "left" ? -1 : 0;
  const inside = (r: number, c: number) => r >= 0 && r < grid.rowCount && c >= 0 && c < grid.colCount;
  const hidden = (r: number, c: number) => (dr ? grid.rowHidden(r) : grid.colHidden(c));
  let r = pos.row;
  let c = pos.col;
  const next = () => {
    do {
      r += dr;
      c += dc;
    } while (inside(r, c) && hidden(r, c));
  };
  const here = grid.filled(r, c);
  if (here === undefined) return null;
  next();
  if (!inside(r, c)) return pos;
  const first = grid.filled(r, c);
  if (first === undefined) return null;
  let steps = 0;
  if (here && first) {
    // Run to the end of the filled block.
    let lastR = r;
    let lastC = c;
    for (;;) {
      next();
      if (!inside(r, c)) return { row: lastR, col: lastC };
      const value = grid.filled(r, c);
      if (value === undefined) return null;
      if (!value) return { row: lastR, col: lastC };
      lastR = r;
      lastC = c;
      if (++steps > limit) return { row: lastR, col: lastC };
    }
  }
  // Find the next filled cell (or the edge).
  if (first) return { row: r, col: c };
  for (;;) {
    const lastR = r;
    const lastC = c;
    next();
    if (!inside(r, c)) return { row: lastR, col: lastC };
    const value = grid.filled(r, c);
    if (value === undefined) return null;
    if (value) return { row: r, col: c };
    if (++steps > limit) return { row: r, col: c };
  }
}

/** Moves the active cell inside a multi-cell selection (Enter and Tab walk it, wrapping round). */
export function walkInside(selection: Selection, dir: "down" | "up" | "right" | "left"): Selection {
  const { range } = selection;
  let { row, col } = selection.active;
  if (dir === "down" || dir === "up") {
    row += dir === "down" ? 1 : -1;
    if (row > range.r2) {
      row = range.r1;
      col = col + 1 > range.c2 ? range.c1 : col + 1;
    } else if (row < range.r1) {
      row = range.r2;
      col = col - 1 < range.c1 ? range.c2 : col - 1;
    }
  } else {
    col += dir === "right" ? 1 : -1;
    if (col > range.c2) {
      col = range.c1;
      row = row + 1 > range.r2 ? range.r1 : row + 1;
    } else if (col < range.c1) {
      col = range.c2;
      row = row - 1 < range.r1 ? range.r2 : row - 1;
    }
  }
  return { ...selection, active: { row, col } };
}

/** True when the selection is more than one cell (or one merge). */
export function isMulti(selection: Selection, grid: SelectionGrid): boolean {
  const { range } = selection;
  if (range.r1 === range.r2 && range.c1 === range.c2) return false;
  const merge = grid.mergeAt(selection.active.row, selection.active.col);
  return !(merge && merge.r1 === range.r1 && merge.c1 === range.c1 && merge.r2 === range.r2 && merge.c2 === range.c2);
}

export const selectionContains = (selection: Selection, row: number, col: number) => rectContains(selection.range, row, col);
