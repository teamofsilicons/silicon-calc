/**
 * What the browser holds of one tab: the cells it has loaded (in blocks of rows and columns, fetched as the viewport
 * reaches them and dropped again when far away and old), the tab's row and column sizes, hidden rows and columns,
 * merges, frozen panes, charts and protections, and the inputs this browser has sent but the service has not answered
 * yet (shown at once, so typing feels instant). It never evaluates anything: values, formulas' results and their
 * shown text (`t`) come from the service, through the cells answer and the live change events.
 *
 * Cells live in one small map per block, so taking in a block's answer, clearing a changed range and dropping an old
 * block cost only that block's cells, whatever else is held.
 */
import type { Account, Cell, CellsAnswer, Change, Chart, Protection, Style, Tab, Value } from "../calc/types";
import { a1, cellKey, colIndex, colName, MAX_COLS, MAX_ROWS, parseRange, type Rect, rectsIntersect } from "./address";
import { AxisSizes } from "./geometry";

export const DEFAULT_ROW_HEIGHT = 21;
export const DEFAULT_COL_WIDTH = 100;
export const BLOCK_ROWS = 128;
export const BLOCK_COLS = 32;
/** Blocks kept before the oldest ones out of view are dropped. */
const MAX_BLOCKS = 600;
const BLOCKS_PER_ROW = Math.ceil(MAX_COLS / BLOCK_COLS);

export interface CellRecord {
  cs?: import("../calc/types").Style;
  v: Value;
  t: string;
  tc?: string;
  f?: string;
  s?: number;
  spill?: boolean;
  by?: string;
  at?: string;
}

/** An input sent but not yet confirmed: drawn as typed until the service's answer replaces it. */
export interface PendingInput {
  input: string;
  /** The edit's id in the queue (cleared when it is answered or fails). */
  edit: number;
}

export interface BlockRef {
  br: number;
  bc: number;
}

export const blockId = (br: number, bc: number) => br * BLOCKS_PER_ROW + bc;
export const blockOf = (row: number, col: number) => blockId(Math.floor(row / BLOCK_ROWS), Math.floor(col / BLOCK_COLS));
export const blockFromId = (id: number): BlockRef => ({ br: Math.floor(id / BLOCKS_PER_ROW), bc: id % BLOCKS_PER_ROW });

export const blockRect = (b: BlockRef): Rect => ({
  r1: b.br * BLOCK_ROWS,
  c1: b.bc * BLOCK_COLS,
  r2: Math.min(MAX_ROWS - 1, b.br * BLOCK_ROWS + BLOCK_ROWS - 1),
  c2: Math.min(MAX_COLS - 1, b.bc * BLOCK_COLS + BLOCK_COLS - 1),
});

/** How many blocks a rectangle touches. */
export const blockCount = (r: Rect) => (Math.floor(r.r2 / BLOCK_ROWS) - Math.floor(r.r1 / BLOCK_ROWS) + 1) * (Math.floor(r.c2 / BLOCK_COLS) - Math.floor(r.c1 / BLOCK_COLS) + 1);

/** The blocks that cover a rectangle. */
export function blocksFor(r: Rect): BlockRef[] {
  const out: BlockRef[] = [];
  const br1 = Math.floor(r.r1 / BLOCK_ROWS);
  const br2 = Math.floor(r.r2 / BLOCK_ROWS);
  const bc1 = Math.floor(r.c1 / BLOCK_COLS);
  const bc2 = Math.floor(r.c2 / BLOCK_COLS);
  for (let br = br1; br <= br2; br++) for (let bc = bc1; bc <= bc2; bc++) out.push({ br, bc });
  return out;
}

const rowFromNumber = (text: string | number) => {
  const n = Number(text);
  return Number.isFinite(n) && n >= 1 ? n - 1 : null;
};

export class TabData {
  tab: Tab;
  rows: AxisSizes;
  cols: AxisSizes;
  /** Cells by block, then by cell key. */
  private readonly blocks = new Map<number, Map<number, CellRecord>>();
  readonly pending = new Map<number, PendingInput>();
  merges: Rect[] = [];
  charts: Chart[] = [];
  protections: Protection[] = [];
  /** Loaded blocks and when they were last used (for dropping the oldest). */
  readonly loaded = new Map<number, number>();
  /** Blocks whose contents are known to be out of date (a change too big to describe). */
  readonly stale = new Set<number>();
  /** The newest version this tab's data reflects. */
  version = 0;

  constructor(tab: Tab) {
    this.tab = tab;
    this.rows = new AxisSizes(DEFAULT_ROW_HEIGHT, Math.max(tab.rows, 1));
    this.cols = new AxisSizes(DEFAULT_COL_WIDTH, Math.max(tab.cols, 1));
  }

  get frozenRows(): number {
    return Math.min(this.tab.frozen_rows ?? 0, this.rows.count);
  }

  get frozenCols(): number {
    return Math.min(this.tab.frozen_cols ?? 0, this.cols.count);
  }

  /** The grid size grows with the tab (never below what is used). */
  setTab(tab: Tab): void {
    this.tab = tab;
    this.rows.count = Math.min(MAX_ROWS, Math.max(this.rows.count, tab.rows || 0, 1));
    this.cols.count = Math.min(MAX_COLS, Math.max(this.cols.count, tab.cols || 0, 1));
  }

  /** Grows the grid so a position exists (typing past the end, jumping to a far address). */
  ensureSize(row: number, col: number): void {
    if (row >= this.rows.count) this.rows.count = Math.min(MAX_ROWS, row + 100);
    if (col >= this.cols.count) this.cols.count = Math.min(MAX_COLS, col + 5);
  }

  get(row: number, col: number): CellRecord | undefined {
    return this.blocks.get(blockOf(row, col))?.get(cellKey(row, col));
  }

  private set(row: number, col: number, record: CellRecord): void {
    const id = blockOf(row, col);
    let block = this.blocks.get(id);
    if (!block) this.blocks.set(id, (block = new Map()));
    block.set(cellKey(row, col), record);
  }

  private remove(row: number, col: number): void {
    this.blocks.get(blockOf(row, col))?.delete(cellKey(row, col));
  }

  /** The held blocks a rectangle touches (walking the held ones when the rectangle is larger than what is held). */
  private heldBlocksIn(r: Rect): Array<[number, Map<number, CellRecord>]> {
    if (blockCount(r) > this.blocks.size) return [...this.blocks.entries()].filter(([id]) => rectsIntersect(blockRect(blockFromId(id)), r));
    const out: Array<[number, Map<number, CellRecord>]> = [];
    for (const b of blocksFor(r)) {
      const id = blockId(b.br, b.bc);
      const block = this.blocks.get(id);
      if (block) out.push([id, block]);
    }
    return out;
  }

  /** Every held cell inside a rectangle. */
  *cellsIn(r: Rect): Generator<{ row: number; col: number; cell: CellRecord }> {
    for (const [, block] of this.heldBlocksIn(r)) {
      for (const [key, cell] of block) {
        const row = Math.floor(key / MAX_COLS);
        const col = key % MAX_COLS;
        if (row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2) yield { row, col, cell };
      }
    }
  }

  /** Empties a rectangle of what is held (the block maps it covers whole are dropped at once). */
  clearArea(r: Rect): void {
    for (const [id, block] of this.heldBlocksIn(r)) {
      const br = blockRect(blockFromId(id));
      if (r.r1 <= br.r1 && r.r2 >= br.r2 && r.c1 <= br.c1 && r.c2 >= br.c2) {
        this.blocks.delete(id);
        continue;
      }
      for (const key of [...block.keys()]) {
        const row = Math.floor(key / MAX_COLS);
        const col = key % MAX_COLS;
        if (row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2) block.delete(key);
      }
    }
  }

  pendingAt(row: number, col: number): PendingInput | undefined {
    return this.pending.size ? this.pending.get(cellKey(row, col)) : undefined;
  }

  isLoaded(row: number, col: number): boolean {
    const id = blockOf(row, col);
    return this.loaded.has(id) && !this.stale.has(id);
  }

  /** The used area (from the tab), or null when the tab is empty. */
  usedRect(): Rect | null {
    return this.tab.used ? parseRange(this.tab.used, this.rows.count, this.cols.count) : null;
  }

  /** The merge that covers a cell, if any. */
  mergeAt(row: number, col: number): Rect | null {
    for (const merge of this.merges) if (row >= merge.r1 && row <= merge.r2 && col >= merge.c1 && col <= merge.c2) return merge;
    return null;
  }

  /** The protection that covers a cell, if any. */
  protectionAt(row: number, col: number): Protection | null {
    for (const protection of this.protections) {
      if (!protection.range) return protection;
      const r = parseRange(protection.range, this.rows.count, this.cols.count);
      if (r && row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2) return protection;
    }
    return null;
  }

  /** Takes in a cells answer for `range` (its cells and what it says about sizes, merges, charts, protections). */
  applyAnswer(answer: CellsAnswer, range: Rect | null, styles: Map<number, Style>, accounts: Map<string, Account>): void {
    if (answer.tab) {
      this.setTab({ ...this.tab, ...answer.tab, frozen_rows: answer.frozen_rows ?? answer.tab.frozen_rows, frozen_cols: answer.frozen_cols ?? answer.tab.frozen_cols });
    }
    for (const [id, style] of Object.entries(answer.styles ?? {})) styles.set(Number(id), style);
    for (const [uuid, account] of Object.entries(answer.accounts ?? {})) accounts.set(uuid, account);
    const area = range ?? (answer.range ? parseRange(answer.range, this.rows.count, this.cols.count) : null);
    // The answer is the truth for its area: cells it does not list there are empty.
    if (area) this.clearArea(area);
    for (const cell of answer.cells ?? []) this.set(cell.r, cell.c, recordOf(cell));
    this.applyLayout(answer, area);
    if (answer.version > this.version) this.version = answer.version;
  }

  /** Sizes, hidden rows and columns, merges, charts and protections from a cells answer. */
  applyLayout(answer: Partial<CellsAnswer>, area: Rect | null): void {
    if (answer.col_widths) {
      if (area) for (const [index] of this.cols.sizedPositions()) if (index >= area.c1 && index <= area.c2) this.cols.setSize(index, null);
      for (const [letters, width] of Object.entries(answer.col_widths)) {
        const index = colIndex(letters);
        if (index !== null && typeof width === "number") this.cols.setSize(index, width);
      }
    }
    if (answer.row_heights) {
      if (area) for (const [index] of this.rows.sizedPositions()) if (index >= area.r1 && index <= area.r2) this.rows.setSize(index, null);
      for (const [row, height] of Object.entries(answer.row_heights)) {
        const index = rowFromNumber(row);
        if (index !== null && typeof height === "number") this.rows.setSize(index, height);
      }
    }
    if (answer.hidden_rows) {
      if (area) for (const index of this.rows.hiddenPositions()) if (index >= area.r1 && index <= area.r2) this.rows.setHidden(index, false);
      for (const row of answer.hidden_rows) {
        const index = rowFromNumber(row);
        if (index !== null) this.rows.setHidden(index, true);
      }
    }
    if (answer.hidden_cols) {
      if (area) for (const index of this.cols.hiddenPositions()) if (index >= area.c1 && index <= area.c2) this.cols.setHidden(index, false);
      for (const letters of answer.hidden_cols) {
        const index = colIndex(String(letters));
        if (index !== null) this.cols.setHidden(index, true);
      }
    }
    if (answer.merges) {
      const reported = answer.merges.map(text => parseRange(text, MAX_ROWS, MAX_COLS)).filter((r): r is Rect => r !== null);
      const kept = area ? this.merges.filter(merge => !rectsIntersect(merge, area)) : [];
      const seen = new Set(kept.map(m => a1(m)));
      this.merges = [...kept, ...reported.filter(m => !seen.has(a1(m)))];
    }
    if (answer.frozen_rows !== undefined || answer.frozen_cols !== undefined) {
      this.tab = { ...this.tab, frozen_rows: answer.frozen_rows ?? this.tab.frozen_rows, frozen_cols: answer.frozen_cols ?? this.tab.frozen_cols };
    }
    if (answer.charts) this.charts = answer.charts.map(normalizeChart).filter((c): c is Chart => c !== null);
    if (answer.protections) this.protections = answer.protections.map(normalizeProtection);
  }

  /** Applies the cells of a change event (every changed cell, when the event carries them). */
  applyCells(cells: Cell[], ranges: Rect[]): void {
    // Changed ranges are cleared first, so a cleared cell (left out of the event) goes empty here too.
    for (const r of ranges) this.clearArea(r);
    for (const cell of cells) {
      if ((!cell.v || cell.v.t === "empty") && !cell.f && !cell.s) this.remove(cell.r, cell.c);
      else this.set(cell.r, cell.c, recordOf(cell));
    }
  }

  /** Marks the blocks under these ranges as out of date (they are fetched again when in view). */
  invalidate(ranges: Rect[]): void {
    for (const r of ranges) {
      if (blockCount(r) > this.loaded.size) {
        for (const id of this.loaded.keys()) {
          const br = blockRect(blockFromId(id));
          if (rectsIntersect(br, r)) this.stale.add(id);
        }
        continue;
      }
      for (const b of blocksFor(r)) {
        const id = blockId(b.br, b.bc);
        if (this.loaded.has(id)) this.stale.add(id);
      }
    }
  }

  /** Forgets every cell and layout detail (a reload). */
  clear(): void {
    this.blocks.clear();
    this.loaded.clear();
    this.stale.clear();
    this.merges = [];
  }

  /** Records that a block's answer arrived. */
  markLoaded(id: number, now: number): void {
    this.loaded.set(id, now);
    this.stale.delete(id);
  }

  touch(id: number, now: number): void {
    if (this.loaded.has(id)) this.loaded.set(id, now);
  }

  /** Drops the least recently used blocks that are out of view when too many are held. */
  evict(keep: Set<number>): void {
    if (this.loaded.size <= MAX_BLOCKS) return;
    const order = [...this.loaded.entries()].filter(([id]) => !keep.has(id)).sort((a, b) => a[1] - b[1]);
    for (const [id] of order.slice(0, this.loaded.size - MAX_BLOCKS)) {
      this.loaded.delete(id);
      this.stale.delete(id);
      this.blocks.delete(id);
    }
  }

  /** How many cells are held (for tests and the status line). */
  heldCells(): number {
    let n = 0;
    for (const block of this.blocks.values()) n += block.size;
    return n;
  }
}

export function recordOf(cell: Cell): CellRecord {
  const record: CellRecord = { v: cell.v ?? { t: "empty" }, t: cell.t ?? "" };
  if (cell.f) record.f = cell.f;
  if (cell.cs) record.cs = cell.cs;
  if (cell.tc) record.tc = cell.tc;
  if (cell.s) record.s = cell.s;
  if (cell.spill) record.spill = true;
  if (cell.by) record.by = cell.by;
  if (cell.at) record.at = cell.at;
  return record;
}

/** Charts in either shape the service may send (`anchor: "E2"`, or the engine's anchor_row/anchor_col). */
export function normalizeChart(raw: unknown): Chart | null {
  if (!raw || typeof raw !== "object") return null;
  const c = raw as Record<string, unknown>;
  let anchor = typeof c.anchor === "string" ? c.anchor : null;
  if (!anchor && typeof c.anchor_row === "number" && typeof c.anchor_col === "number") anchor = `${colName(c.anchor_col)}${c.anchor_row + 1}`;
  if (typeof c.id !== "string" || typeof c.range !== "string") return null;
  return {
    id: c.id,
    tab: Number(c.tab ?? 0),
    kind: (typeof c.kind === "string" ? c.kind : "column") as Chart["kind"],
    title: typeof c.title === "string" ? c.title : "",
    range: c.range,
    anchor: anchor ?? "A1",
    width: typeof c.width === "number" ? c.width : 480,
    height: typeof c.height === "number" ? c.height : 300,
  };
}

/** Protections in either shape (editors as accounts or as ids/uuids; `range` or none for the whole tab). */
export function normalizeProtection(raw: unknown): Protection {
  const p = (raw ?? {}) as Record<string, unknown>;
  const editors = Array.isArray(p.editors)
    ? p.editors.map(editor => (typeof editor === "string" ? { uuid: editor, id: editor, kind: editor.startsWith("si:") ? "silicon" : "carbon", display_name: editor } : editor) as Account)
    : [];
  return { id: String(p.id ?? ""), tab: Number(p.tab ?? 0), range: typeof p.range === "string" && p.range ? p.range : null, editors, description: typeof p.description === "string" ? p.description : "" };
}

/** The rectangles a list of changes covers on one tab (reload and tab-list changes are not ranges). */
export function changeRects(changes: Change[], tab: number, rows = MAX_ROWS, cols = MAX_COLS): Rect[] {
  const out: Rect[] = [];
  for (const change of changes) {
    if (!("tab" in change) || change.tab !== tab || !("range" in change) || typeof change.range !== "string") continue;
    const r = parseRange(change.range, rows, cols);
    if (r) out.push(r);
  }
  return out;
}
