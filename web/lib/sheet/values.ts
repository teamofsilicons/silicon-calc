/**
 * Cell values and summaries as words, for the panels that list changes (history, compare, merge conflicts, a cell's
 * history), and the compare overlay the grid tints.
 */
import type { CellDiff, CellSummary, Value, WorkbookDiff } from "../calc/types";
import { cellKey, parseCell } from "./address";
import type { DiffOverlay } from "./controller";

/** A value as a spreadsheet shows it unformatted: numbers in full, TRUE/FALSE, the error's text. */
export function valueText(value: Value | string | null | undefined): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  switch (value.t) {
    case "number":
      return Number.isInteger(value.v) ? String(value.v) : String(Number(value.v.toPrecision(15)));
    case "text":
      return value.v;
    case "bool":
      return value.v ? "TRUE" : "FALSE";
    case "error":
      return value.v;
    default:
      return "";
  }
}

/** What a cell held, in a few words: its formula (with the result), or its value; "empty" when nothing. */
export function summaryText(summary: CellSummary | null | undefined): string {
  if (!summary) return "empty";
  const value = valueText(summary.value ?? null);
  if (summary.formula) return value ? `${summary.formula} → ${value}` : summary.formula;
  if (value) return value;
  return summary.style ? "only formatting" : "empty";
}

const isEmpty = (summary: CellSummary | null | undefined) => !summary || ((!summary.value || summary.value.t === "empty") && !summary.formula);

export function diffKind(cell: CellDiff): "changed" | "added" | "removed" {
  if (isEmpty(cell.before) && !isEmpty(cell.after)) return "added";
  if (!isEmpty(cell.before) && isEmpty(cell.after)) return "removed";
  return "changed";
}

/** The grid's tints for a compare: every changed cell by tab. */
export function overlayOf(diff: WorkbookDiff, conflicts: Array<{ tab: number; a: string }> = []): DiffOverlay {
  const overlay: DiffOverlay = new Map();
  const put = (tab: number, row: number, col: number, kind: "changed" | "added" | "removed" | "conflict") => {
    let cells = overlay.get(tab);
    if (!cells) overlay.set(tab, (cells = new Map()));
    cells.set(cellKey(row, col), kind);
  };
  // `row` and `col` in a CellDiff are the tab's stable row and column ids, not positions: the address is where it is.
  for (const cell of diff.cells ?? []) {
    const pos = parseCell(cell.address);
    if (pos) put(cell.tab, pos.row, pos.col, diffKind(cell));
  }
  for (const conflict of conflicts) {
    const pos = parseCell(conflict.a);
    if (pos) put(conflict.tab, pos.row, pos.col, "conflict");
  }
  return overlay;
}
