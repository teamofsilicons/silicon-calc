/**
 * Formula text as the editor sees it, without evaluating anything (the service does that): a tokenizer for Excel
 * syntax, the references a formula names (with a colour each, for highlighting them in the text and on the grid), what
 * function the caret is in (for its argument hint), whether the caret is where a reference can be pointed at, and
 * moving relative references when a formula is pasted or filled somewhere else.
 */
import { cellName, colIndex, colName, MAX_COLS, MAX_ROWS, quoteTab, type Rect } from "./address";

export type TokenKind = "string" | "number" | "bool" | "error" | "ref" | "func" | "name" | "op" | "paren" | "sep" | "space" | "unknown";

export interface Token {
  kind: TokenKind;
  text: string;
  start: number;
  end: number;
}

/** A reference in a formula: where it is in the text and what it points at. */
export interface RefToken extends Token {
  kind: "ref";
  /** The tab it names (null: the formula's own tab). */
  tab: string | null;
  rect: Rect;
  /** The reference without its tab part, as written (`$A$1:B2`). */
  body: string;
}

const ERRORS = ["#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!", "#SPILL!", "#CALC!", "#GETTING_DATA"];

const CELL = String.raw`\$?[A-Za-z]{1,3}\$?\d{1,8}`;
const COLS = String.raw`\$?[A-Za-z]{1,3}:\$?[A-Za-z]{1,3}`;
const ROWS = String.raw`\$?\d{1,8}:\$?\d{1,8}`;
const REF_BODY = new RegExp(`^(?:${CELL}(?::${CELL})?|${COLS}|${ROWS})`);
const TAB_PLAIN = /^[\p{L}_][\p{L}\p{N}_.]*!/u;
const IDENT = /^[\p{L}_\\][\p{L}\p{N}_.]*/u;
const NUMBER = /^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/;

function refRect(body: string): Rect | null {
  const clean = body.replace(/\$/g, "");
  const [a, b = a] = clean.split(":") as [string, string?];
  const end = b ?? a;
  const ca = colIndex(a);
  const cb = colIndex(end);
  if (ca !== null && cb !== null) return { r1: 0, c1: Math.min(ca, cb), r2: MAX_ROWS - 1, c2: Math.max(ca, cb) };
  if (/^\d+$/.test(a) && /^\d+$/.test(end)) {
    const ra = Number(a) - 1;
    const rb = Number(end) - 1;
    if (ra < 0 || rb < 0) return null;
    return { r1: Math.min(ra, rb), c1: 0, r2: Math.max(ra, rb), c2: MAX_COLS - 1 };
  }
  const p = /^([A-Za-z]{1,3})(\d+)$/.exec(a);
  const q = /^([A-Za-z]{1,3})(\d+)$/.exec(end);
  if (!p || !q) return null;
  const c1 = colIndex(p[1]!);
  const c2 = colIndex(q[1]!);
  const r1 = Number(p[2]) - 1;
  const r2 = Number(q[2]) - 1;
  if (c1 === null || c2 === null || r1 < 0 || r2 < 0 || r1 >= MAX_ROWS || r2 >= MAX_ROWS) return null;
  return { r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
}

/** Splits formula text into tokens (the leading `=` is an operator token). Never throws; unknown text is "unknown". */
export function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;
  const push = (kind: TokenKind, end: number, extra: Partial<RefToken> = {}) => {
    tokens.push({ kind, text: text.slice(i, end), start: i, end, ...extra } as Token);
    i = end;
  };
  while (i < text.length) {
    const rest = text.slice(i);
    const char = text[i]!;
    if (/\s/.test(char)) {
      let j = i + 1;
      while (j < text.length && /\s/.test(text[j]!)) j++;
      push("space", j);
      continue;
    }
    if (char === '"') {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === '"') {
          if (text[j + 1] === '"') {
            j += 2;
            continue;
          }
          j++;
          break;
        }
        j++;
      }
      push("string", Math.min(j, text.length));
      continue;
    }
    if (char === "#") {
      const error = ERRORS.find(name => rest.toUpperCase().startsWith(name));
      if (error) {
        push("error", i + error.length);
        continue;
      }
      push("op", i + 1);
      continue;
    }
    // A quoted tab name: 'My tab'!A1
    if (char === "'") {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === "'") {
          if (text[j + 1] === "'") {
            j += 2;
            continue;
          }
          break;
        }
        j++;
      }
      if (text[j] === "'" && text[j + 1] === "!") {
        const tabName = text.slice(i + 1, j).replace(/''/g, "'");
        const bodyMatch = REF_BODY.exec(text.slice(j + 2));
        if (bodyMatch && !identContinues(text, j + 2 + bodyMatch[0].length)) {
          const rectValue = refRect(bodyMatch[0]);
          if (rectValue) {
            push("ref", j + 2 + bodyMatch[0].length, { tab: tabName, rect: rectValue, body: bodyMatch[0] });
            continue;
          }
        }
      }
      push("unknown", Math.min(j + 1, text.length));
      continue;
    }
    // Plain tab name: Sales!A1
    const tabMatch = TAB_PLAIN.exec(rest);
    if (tabMatch) {
      const after = rest.slice(tabMatch[0].length);
      const bodyMatch = REF_BODY.exec(after);
      if (bodyMatch && !identContinues(text, i + tabMatch[0].length + bodyMatch[0].length)) {
        const rectValue = refRect(bodyMatch[0]);
        if (rectValue) {
          push("ref", i + tabMatch[0].length + bodyMatch[0].length, { tab: tabMatch[0].slice(0, -1), rect: rectValue, body: bodyMatch[0] });
          continue;
        }
      }
    }
    const bodyMatch = REF_BODY.exec(rest);
    if (bodyMatch && !identContinues(text, i + bodyMatch[0].length) && !(text[i + bodyMatch[0].length] === "(")) {
      const rectValue = refRect(bodyMatch[0]);
      // "LOG10" is a function name, "A1" a reference: a reference is never followed by "(".
      if (rectValue) {
        push("ref", i + bodyMatch[0].length, { tab: null, rect: rectValue, body: bodyMatch[0] });
        continue;
      }
    }
    const numberMatch = NUMBER.exec(rest);
    if (numberMatch) {
      push("number", i + numberMatch[0].length);
      continue;
    }
    const identMatch = IDENT.exec(rest);
    if (identMatch) {
      const end = i + identMatch[0].length;
      const upper = identMatch[0].toUpperCase();
      if (text[end] === "(") push("func", end);
      else if (upper === "TRUE" || upper === "FALSE") push("bool", end);
      else push("name", end);
      continue;
    }
    if (char === "(" || char === ")" || char === "{" || char === "}") {
      push("paren", i + 1);
      continue;
    }
    if (char === "," || char === ";") {
      push("sep", i + 1);
      continue;
    }
    const two = rest.slice(0, 2);
    if (two === "<>" || two === "<=" || two === ">=") {
      push("op", i + 2);
      continue;
    }
    if ("+-*/^&=<>%:@!".includes(char)) {
      push("op", i + 1);
      continue;
    }
    push("unknown", i + 1);
  }
  return tokens;
}

/** True when the text at `at` continues an identifier (so `A1B` is a name, not a reference). */
function identContinues(text: string, at: number): boolean {
  const next = text[at];
  return next !== undefined && /[\p{L}\p{N}_.]/u.test(next);
}

/** A formula is text that starts with `=` (or `+`/`-` followed by more, as Excel accepts). */
export const isFormula = (text: string) => text.startsWith("=");

/** The references a formula names, in order. */
export function references(text: string): RefToken[] {
  if (!isFormula(text)) return [];
  return tokenize(text).filter((token): token is RefToken => token.kind === "ref");
}

/** The highlight colours references cycle through (Google Sheets' order, tuned for 3:1 on white). */
export const REF_COLORS = ["#1F5FB8", "#C2410C", "#7A4FC4", "#17806B", "#B42318", "#93560A", "#0E7490", "#BE185D"];

/** Each distinct reference gets one colour; the same reference written twice keeps its colour. */
export function referenceColors(refs: RefToken[]): Map<RefToken, string> {
  const byKey = new Map<string, string>();
  const out = new Map<RefToken, string>();
  for (const ref of refs) {
    const key = `${(ref.tab ?? "").toLowerCase()}!${ref.rect.r1}:${ref.rect.c1}:${ref.rect.r2}:${ref.rect.c2}`;
    let color = byKey.get(key);
    if (!color) {
      color = REF_COLORS[byKey.size % REF_COLORS.length]!;
      byKey.set(key, color);
    }
    out.set(ref, color);
  }
  return out;
}

/** The function the caret sits inside and which argument (0-based), for the signature hint. */
export function functionAt(text: string, caret: number): { name: string; argument: number } | null {
  if (!isFormula(text)) return null;
  const tokens = tokenize(text.slice(0, caret));
  const stack: Array<{ name: string | null; argument: number }> = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i]!;
    if (token.kind === "paren" && token.text === "(") {
      const previous = tokens[i - 1];
      stack.push({ name: previous?.kind === "func" ? previous.text.toUpperCase() : null, argument: 0 });
    } else if (token.kind === "paren" && token.text === ")") stack.pop();
    else if (token.kind === "sep" && stack.length) stack[stack.length - 1]!.argument += 1;
  }
  for (let i = stack.length - 1; i >= 0; i--) {
    const entry = stack[i]!;
    if (entry.name) return { name: entry.name, argument: entry.argument };
  }
  return null;
}

/** The function name being typed at the caret (`=SU|` → "SU"), for suggestions; null when not typing one. */
export function partialFunctionAt(text: string, caret: number): { prefix: string; start: number } | null {
  if (!isFormula(text)) return null;
  const before = text.slice(0, caret);
  const match = /([A-Za-z_][A-Za-z0-9_.]*)$/.exec(before);
  if (!match) return null;
  const start = caret - match[1]!.length;
  const previous = before[start - 1];
  if (previous !== undefined && !/[=+\-*/^&(,;<>:% {]/.test(previous)) return null;
  // A cell address being typed (A1) is a reference, not a function.
  if (/^[A-Za-z]{1,3}\d+$/.test(match[1]!)) return null;
  const after = text[caret];
  if (after !== undefined && /[A-Za-z0-9_.(]/.test(after)) return null;
  return { prefix: match[1]!, start };
}

/**
 * Whether the caret is where a reference can be pointed at with the mouse or the arrow keys: right after `=`, an
 * operator, `(` or a separator (or replacing a reference that was just pointed at, `pointed`).
 */
export function canPointAt(text: string, caret: number, pointed: { start: number; end: number } | null = null): boolean {
  if (!isFormula(text)) return false;
  if (pointed && pointed.end === caret) return true;
  const before = text.slice(0, caret).replace(/\s+$/, "");
  const last = before[before.length - 1];
  if (last === undefined) return false;
  if (!/[=+\-*/^&(,;<>:%]/.test(last)) return false;
  const after = text.slice(caret).replace(/^\s+/, "")[0];
  return after === undefined || /[)+\-*/^&,;<>%]/.test(after);
}

/** A reference's text for inserting into a formula: `B7`, `A1:C9`, `Sales!B7` when it is on another tab. */
export function referenceText(rect: Rect, tab: string | null): string {
  const body = rect.r1 === rect.r2 && rect.c1 === rect.c2 ? cellName(rect.r1, rect.c1) : `${cellName(rect.r1, rect.c1)}:${cellName(rect.r2, rect.c2)}`;
  return tab ? `${quoteTab(tab)}!${body}` : body;
}

/** Puts `insert` in place of `start..end` and gives the new text and caret. */
export function replaceText(text: string, start: number, end: number, insert: string): { text: string; caret: number } {
  return { text: text.slice(0, start) + insert + text.slice(end), caret: start + insert.length };
}

/* ---- moving references ------------------------------------------------------------------------------------------- */

function shiftCellRef(ref: string, dr: number, dc: number): string {
  const match = /^(\$?)([A-Za-z]{1,3})(\$?)(\d+)$/.exec(ref);
  if (!match) return ref;
  const [, colAbs, colText, rowAbs, rowText] = match as unknown as [string, string, string, string, string];
  const col = colIndex(colText);
  const row = Number(rowText) - 1;
  if (col === null) return ref;
  const nextCol = colAbs ? col : col + dc;
  const nextRow = rowAbs ? row : row + dr;
  if (nextCol < 0 || nextRow < 0 || nextCol >= MAX_COLS || nextRow >= MAX_ROWS) return "#REF!";
  return `${colAbs}${colName(nextCol)}${rowAbs}${nextRow + 1}`;
}

function shiftColRef(ref: string, dc: number): string {
  const match = /^(\$?)([A-Za-z]{1,3})$/.exec(ref);
  if (!match) return ref;
  const col = colIndex(match[2]!);
  if (col === null) return ref;
  const next = match[1] ? col : col + dc;
  if (next < 0 || next >= MAX_COLS) return "#REF!";
  return `${match[1]}${colName(next)}`;
}

function shiftRowRef(ref: string, dr: number): string {
  const match = /^(\$?)(\d+)$/.exec(ref);
  if (!match) return ref;
  const row = Number(match[2]) - 1;
  const next = match[1] ? row : row + dr;
  if (next < 0 || next >= MAX_ROWS) return "#REF!";
  return `${match[1]}${next + 1}`;
}

function shiftBody(body: string, dr: number, dc: number): string {
  const parts = body.split(":");
  if (parts.length === 2 && /^\$?[A-Za-z]{1,3}$/.test(parts[0]!) && /^\$?[A-Za-z]{1,3}$/.test(parts[1]!)) {
    const out = [shiftColRef(parts[0]!, dc), shiftColRef(parts[1]!, dc)];
    return out.includes("#REF!") ? "#REF!" : out.join(":");
  }
  if (parts.length === 2 && /^\$?\d+$/.test(parts[0]!) && /^\$?\d+$/.test(parts[1]!)) {
    const out = [shiftRowRef(parts[0]!, dr), shiftRowRef(parts[1]!, dr)];
    return out.includes("#REF!") ? "#REF!" : out.join(":");
  }
  const out = parts.map(part => shiftCellRef(part, dr, dc));
  return out.includes("#REF!") ? "#REF!" : out.join(":");
}

/**
 * The formula as it reads when copied `dr` rows down and `dc` columns right: relative parts of every reference move,
 * absolute (`$`) parts stay, and a reference pushed off the grid becomes `#REF!` (as in Excel).
 */
export function shiftFormula(text: string, dr: number, dc: number): string {
  if (!isFormula(text) || (dr === 0 && dc === 0)) return text;
  let out = "";
  for (const token of tokenize(text)) {
    if (token.kind !== "ref") {
      out += token.text;
      continue;
    }
    const ref = token as RefToken;
    const prefix = token.text.slice(0, token.text.length - ref.body.length);
    const moved = shiftBody(ref.body, dr, dc);
    out += moved === "#REF!" ? "#REF!" : prefix + moved;
  }
  return out;
}

/* ---- R1C1 (Google Sheets' clipboard) ----------------------------------------------------------------------------- */

/**
 * Converts an R1C1 formula (`=R[0]C[-1]*2`, as Google Sheets puts in its clipboard) to A1 as written at `row`, `col`
 * (zero-based). Text inside strings is left alone.
 */
export function r1c1ToA1(text: string, row: number, col: number): string {
  let out = "";
  let i = 0;
  const part = (raw: string | undefined, base: number): { value: number; abs: boolean } | null => {
    if (raw === undefined || raw === "") return { value: base, abs: false };
    if (raw.startsWith("[")) return { value: base + Number(raw.slice(1, -1)), abs: false };
    return { value: Number(raw) - 1, abs: true };
  };
  while (i < text.length) {
    const char = text[i]!;
    if (char === '"') {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === '"' && text[j + 1] === '"') j += 2;
        else if (text[j] === '"') {
          j++;
          break;
        } else j++;
      }
      out += text.slice(i, j);
      i = j;
      continue;
    }
    const previous = text[i - 1];
    const boundary = previous === undefined || !/[\p{L}\p{N}_.$]/u.test(previous);
    const match = boundary ? /^R(\[-?\d+\]|\d+)?C(\[-?\d+\]|\d+)?(?![\p{L}\p{N}_.(])/u.exec(text.slice(i)) : null;
    if (match) {
      const r = part(match[1], row);
      const c = part(match[2], col);
      if (r && c && r.value >= 0 && c.value >= 0) {
        out += `${c.abs ? "$" : ""}${colName(c.value)}${r.abs ? "$" : ""}${r.value + 1}`;
      } else out += "#REF!";
      i += match[0].length;
      continue;
    }
    out += char;
    i++;
  }
  return out;
}
