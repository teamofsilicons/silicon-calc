/**
 * Cell addresses by position, as Calc's service writes them (docs/API.md: `B7`, `A1:C9`, `A:C`, `3:5`): column
 * letters, zero-based positions, inclusive rectangles, whole rows and columns, and tab-qualified references
 * (`Sales!A1`, `'My tab'!A1:B2`). Mirrors crates/calc-engine/src/addr.rs.
 */

/** Excel's last column (XFD). */
export const MAX_COLS = 16_384;
/** The most rows a tab may have (Excel stops at 1,048,576; Calc holds ten times that). */
export const MAX_ROWS = 10_485_760;

/** A cell by position (zero-based). */
export interface CellPos {
  row: number;
  col: number;
}

/** A rectangle of cells by position, inclusive on both ends, always normalised (r1 <= r2, c1 <= c2). */
export interface Rect {
  r1: number;
  c1: number;
  r2: number;
  c2: number;
}

/** `0` → `A`, `25` → `Z`, `26` → `AA`. */
export function colName(col: number): string {
  let n = Math.max(0, Math.floor(col));
  let out = "";
  for (;;) {
    out = String.fromCharCode(65 + (n % 26)) + out;
    if (n < 26) break;
    n = Math.floor(n / 26) - 1;
  }
  return out;
}

/** `A` → `0`, `AA` → `26`, case-insensitive; null when it is not 1 to 3 letters inside XFD. */
export function colIndex(name: string): number | null {
  if (!/^[A-Za-z]{1,3}$/.test(name)) return null;
  let n = 0;
  for (const char of name.toUpperCase()) n = n * 26 + (char.charCodeAt(0) - 64);
  const index = n - 1;
  return index < MAX_COLS ? index : null;
}

/** `{row: 6, col: 1}` → `B7`. */
export function cellName(row: number, col: number): string {
  return `${colName(col)}${row + 1}`;
}

/** `B7` (or `$B$7`) → `{row: 6, col: 1}`; null when it is not a cell address. */
export function parseCell(text: string): CellPos | null {
  const match = /^\s*\$?([A-Za-z]{1,3})\$?(\d{1,8})\s*$/.exec(text);
  if (!match) return null;
  const col = colIndex(match[1]!);
  const row = Number(match[2]);
  if (col === null || row < 1 || row > MAX_ROWS) return null;
  return { row: row - 1, col };
}

export function rect(r1: number, c1: number, r2: number, c2: number): Rect {
  return { r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
}

export const cellRect = (pos: CellPos): Rect => ({ r1: pos.row, c1: pos.col, r2: pos.row, c2: pos.col });

/**
 * `A1:C9`, `B7`, `A:C`, `3:5`, `A1:A` (open-ended down the column). `maxRows` and `maxCols` bound whole columns and
 * rows (the tab's grid size, or the engine's limits). Null when the text is not a range.
 */
export function parseRange(text: string, maxRows = MAX_ROWS, maxCols = MAX_COLS): Rect | null {
  const clean = text.trim().replace(/\$/g, "");
  if (!clean) return null;
  const [a, b = a, ...rest] = clean.split(":");
  if (rest.length || !a || !b) return null;
  const endRow = Math.max(0, maxRows - 1);
  const endCol = Math.max(0, maxCols - 1);
  const ca = colIndex(a);
  const cb = colIndex(b);
  if (ca !== null && cb !== null) return rect(0, ca, endRow, cb);
  if (/^\d+$/.test(a) && /^\d+$/.test(b)) {
    const ra = Number(a);
    const rb = Number(b);
    if (ra < 1 || rb < 1 || ra > MAX_ROWS || rb > MAX_ROWS) return null;
    return rect(ra - 1, 0, rb - 1, endCol);
  }
  const p1 = parseCell(a);
  if (!p1) return null;
  if (cb !== null) return rect(p1.row, p1.col, endRow, cb);
  const p2 = parseCell(b);
  if (!p2) return null;
  return rect(p1.row, p1.col, p2.row, p2.col);
}

/**
 * The A1 text of a rectangle: `B7` for one cell, `A1:C9`, and whole columns (`A:C`) or rows (`3:5`) when it spans the
 * grid (`maxRows`/`maxCols`: the tab's size or the engine's limits).
 */
export function formatRange(r: Rect, maxRows = MAX_ROWS, maxCols = MAX_COLS): string {
  const wholeCols = r.r1 === 0 && r.r2 >= maxRows - 1;
  const wholeRows = r.c1 === 0 && r.c2 >= maxCols - 1;
  if (wholeCols && !wholeRows) return `${colName(r.c1)}:${colName(r.c2)}`;
  if (wholeRows && !wholeCols) return `${r.r1 + 1}:${r.r2 + 1}`;
  if (r.r1 === r.r2 && r.c1 === r.c2) return cellName(r.r1, r.c1);
  return `${cellName(r.r1, r.c1)}:${cellName(r.r2, r.c2)}`;
}

/** The plain A1 text of a rectangle, never folded into whole rows or columns (for the service's `range=`). */
export function a1(r: Rect): string {
  if (r.r1 === r.r2 && r.c1 === r.c2) return cellName(r.r1, r.c1);
  return `${cellName(r.r1, r.c1)}:${cellName(r.r2, r.c2)}`;
}

export const sameRect = (a: Rect, b: Rect) => a.r1 === b.r1 && a.c1 === b.c1 && a.r2 === b.r2 && a.c2 === b.c2;

export const rectContains = (r: Rect, row: number, col: number) => row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2;

export const rectsIntersect = (a: Rect, b: Rect) => a.r1 <= b.r2 && b.r1 <= a.r2 && a.c1 <= b.c2 && b.c1 <= a.c2;

export function intersection(a: Rect, b: Rect): Rect | null {
  if (!rectsIntersect(a, b)) return null;
  return { r1: Math.max(a.r1, b.r1), c1: Math.max(a.c1, b.c1), r2: Math.min(a.r2, b.r2), c2: Math.min(a.c2, b.c2) };
}

export const rectRows = (r: Rect) => r.r2 - r.r1 + 1;
export const rectCols = (r: Rect) => r.c2 - r.c1 + 1;
export const rectSize = (r: Rect) => rectRows(r) * rectCols(r);

/** The smallest rectangle holding both. */
export const union = (a: Rect, b: Rect): Rect => ({ r1: Math.min(a.r1, b.r1), c1: Math.min(a.c1, b.c1), r2: Math.max(a.r2, b.r2), c2: Math.max(a.c2, b.c2) });

/** A tab name as a formula writes it: quoted when it needs to be (`Sales`, `'My tab'`, `'It''s'`). */
export function quoteTab(name: string): string {
  const plain = /^[\p{L}_][\p{L}\p{N}_.]*$/u.test(name) && !parseCell(name) && !/^(true|false)$/i.test(name) && !/^r\d*c\d*$/i.test(name);
  return plain ? name : `'${name.replace(/'/g, "''")}'`;
}

/** `Sales!A1:B2` → `{tab: "Sales", ref: "A1:B2"}`; `'It''s'!A1` → `{tab: "It's", ref: "A1"}`; no tab: `{tab: null}`. */
export function splitTab(text: string): { tab: string | null; ref: string } {
  const s = text.trim();
  if (s.startsWith("'")) {
    let name = "";
    for (let i = 1; i < s.length; i++) {
      const char = s[i]!;
      if (char === "'") {
        if (s[i + 1] === "'") {
          name += "'";
          i++;
          continue;
        }
        if (s[i + 1] === "!") return { tab: name, ref: s.slice(i + 2) };
        return { tab: null, ref: s };
      }
      name += char;
    }
    return { tab: null, ref: s };
  }
  const bang = s.lastIndexOf("!");
  if (bang > 0) return { tab: s.slice(0, bang), ref: s.slice(bang + 1) };
  return { tab: null, ref: s };
}

/** A key for one cell in maps (rows up to 10,485,760 times 16,384 columns stays well inside 2^53). */
export const cellKey = (row: number, col: number) => row * MAX_COLS + col;
export const keyRow = (key: number) => Math.floor(key / MAX_COLS);
export const keyCol = (key: number) => key % MAX_COLS;
