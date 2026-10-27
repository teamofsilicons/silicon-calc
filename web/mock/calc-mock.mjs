#!/usr/bin/env node
/**
 * A small in-memory stand-in for Calc's service (docs/API.md), for developing and looking at the web app while the
 * real service is not running. It is not the engine: formulas know + - * / ^, &, comparisons, cell and range
 * references and SUM, AVERAGE, COUNT, COUNTA, MIN, MAX, ROUND, ABS, IF, CONCAT and LEN; number formats are a few
 * common ones. Everything is lost when it stops. The real verification is end to end against calc-api.
 *
 *   MOCK_PORT=8749 node mock/calc-mock.mjs        (pnpm mock)
 *
 * Who is calling comes from the bearer token's claims (sub, id, kind), read without verifying it: development only.
 */
import http from "node:http";
import { randomUUID } from "node:crypto";

const PORT = Number(process.env.MOCK_PORT || 8749);
const MAX_ROWS = 10_485_760;
const MAX_COLS = 16_384;

/* ---- helpers ------------------------------------------------------------------------------------------------- */

const now = () => new Date().toISOString();
const id12 = () => Array.from({ length: 12 }, () => "abcdefghijklmnopqrstuvwxyz0123456789"[Math.floor(Math.random() * 36)]).join("");
const colName = col => {
  let n = col;
  let out = "";
  for (;;) {
    out = String.fromCharCode(65 + (n % 26)) + out;
    if (n < 26) break;
    n = Math.floor(n / 26) - 1;
  }
  return out;
};
const colIndex = name => {
  let n = 0;
  for (const ch of name.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
  return n - 1;
};
const a1 = (row, col) => `${colName(col)}${row + 1}`;
const key = (row, col) => row * MAX_COLS + col;
const keyRow = k => Math.floor(k / MAX_COLS);
const keyCol = k => k % MAX_COLS;

function parseRange(text, rows = MAX_ROWS, cols = MAX_COLS) {
  const clean = String(text).replace(/\$/g, "").trim().toUpperCase();
  let m = /^([A-Z]{1,3})(\d+)(?::([A-Z]{1,3})(\d+))?$/.exec(clean);
  if (m) {
    const r1 = Number(m[2]) - 1;
    const c1 = colIndex(m[1]);
    const r2 = m[4] ? Number(m[4]) - 1 : r1;
    const c2 = m[3] ? colIndex(m[3]) : c1;
    return { r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
  }
  m = /^([A-Z]{1,3}):([A-Z]{1,3})$/.exec(clean);
  if (m) return { r1: 0, c1: Math.min(colIndex(m[1]), colIndex(m[2])), r2: rows - 1, c2: Math.max(colIndex(m[1]), colIndex(m[2])) };
  m = /^(\d+):(\d+)$/.exec(clean);
  if (m) return { r1: Math.min(+m[1], +m[2]) - 1, c1: 0, r2: Math.max(+m[1], +m[2]) - 1, c2: cols - 1 };
  return null;
}
const rangeText = r => (r.r1 === r.r2 && r.c1 === r.c2 ? a1(r.r1, r.c1) : `${a1(r.r1, r.c1)}:${a1(r.r2, r.c2)}`);

class Fail extends Error {
  constructor(status, code, message, hint, details) {
    super(message);
    this.status = status;
    this.code = code;
    this.hint = hint;
    this.details = details;
  }
}
const notFound = what => new Fail(404, "not_found", `${what} does not exist, or it was not shared with you.`, "Check the address, or ask its owner to share it.");

/* ---- accounts ------------------------------------------------------------------------------------------------ */

const accounts = new Map();
function accountFrom(req) {
  const header = req.headers.authorization || "";
  const token = header.startsWith("Bearer ") ? header.slice(7) : null;
  if (!token) return null;
  try {
    const payload = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString("utf8"));
    const account = { uuid: payload.sub, id: payload.id, kind: payload.kind, display_name: payload.name || payload.id.replace(/^(c|si):/, ""), pfp_url: null };
    if (!accounts.has(account.uuid)) accounts.set(account.uuid, account);
    return accounts.get(account.uuid);
  } catch {
    return null;
  }
}
function lookupAccount(id) {
  for (const account of accounts.values()) if (account.id === id) return account;
  if (/^(c|si):[a-z0-9_-]{3,30}$/.test(id)) {
    const made = { uuid: randomUUID(), id, kind: id.startsWith("si:") ? "silicon" : "carbon", display_name: id.replace(/^(c|si):/, ""), pfp_url: null };
    accounts.set(made.uuid, made);
    return made;
  }
  return null;
}

/* ---- the workbook model -------------------------------------------------------------------------------------- */

const DEFAULT_STYLE = {};
class Workbook {
  constructor() {
    this.tabs = [];
    this.styles = [DEFAULT_STYLE];
    this.nextTab = 1;
    this.names = [];
  }
  styleId(style) {
    const clean = Object.fromEntries(Object.entries(style).filter(([, v]) => v !== undefined && v !== null && v !== false && v !== "general" && v !== "bottom"));
    const text = JSON.stringify(Object.keys(clean).sort().reduce((o, k) => ((o[k] = clean[k]), o), {}));
    if (text === "{}") return 0;
    const found = this.styles.findIndex(s => JSON.stringify(Object.keys(s).sort().reduce((o, k) => ((o[k] = s[k]), o), {})) === text);
    if (found >= 0) return found;
    this.styles.push(clean);
    return this.styles.length - 1;
  }
  addTab(name, index) {
    const tab = new Tab(this.nextTab++, name);
    if (index === undefined || index > this.tabs.length) this.tabs.push(tab);
    else this.tabs.splice(index, 0, tab);
    return tab;
  }
  tab(ref) {
    const tab = this.tabs.find(t => String(t.id) === String(ref)) ?? this.tabs.find(t => t.name.toLowerCase() === String(ref).toLowerCase());
    if (!tab) throw notFound(`The tab ${ref}`);
    return tab;
  }
  clone() {
    const wb = new Workbook();
    wb.styles = this.styles.map(s => ({ ...s }));
    wb.nextTab = this.nextTab;
    wb.names = [...this.names];
    wb.tabs = this.tabs.map(t => t.clone());
    return wb;
  }
}

class Tab {
  constructor(id, name) {
    this.id = id;
    this.name = name;
    this.color = null;
    this.frozen_rows = 0;
    this.frozen_cols = 0;
    this.cells = new Map();
    this.colWidths = new Map();
    this.rowHeights = new Map();
    this.hiddenRows = new Set();
    this.hiddenCols = new Set();
    this.merges = [];
    this.charts = [];
    this.protections = [];
    this.code = null;
    this.generator = null;
    this.generatedRows = 0;
    this.generatedCols = 0;
  }
  clone() {
    const t = new Tab(this.id, this.name);
    Object.assign(t, { color: this.color, frozen_rows: this.frozen_rows, frozen_cols: this.frozen_cols, code: this.code, generator: this.generator, generatedRows: this.generatedRows, generatedCols: this.generatedCols });
    t.cells = new Map([...this.cells].map(([k, c]) => [k, { ...c }]));
    t.colWidths = new Map(this.colWidths);
    t.rowHeights = new Map(this.rowHeights);
    t.hiddenRows = new Set(this.hiddenRows);
    t.hiddenCols = new Set(this.hiddenCols);
    t.merges = [...this.merges];
    t.charts = this.charts.map(c => ({ ...c }));
    t.protections = this.protections.map(p => ({ ...p }));
    return t;
  }
  get(row, col) {
    const own = this.cells.get(key(row, col));
    if (own) return own.deleted ? null : own;
    if (this.generator && row < this.generatedRows && col < this.generatedCols) return this.generator(row, col);
    return null;
  }
  used() {
    let r2 = -1;
    let c2 = -1;
    for (const [k, c] of this.cells) {
      if (c.deleted) continue;
      r2 = Math.max(r2, keyRow(k));
      c2 = Math.max(c2, keyCol(k));
    }
    if (this.generator) {
      r2 = Math.max(r2, this.generatedRows - 1);
      c2 = Math.max(c2, this.generatedCols - 1);
    }
    return r2 < 0 ? null : { r1: 0, c1: 0, r2, c2 };
  }
  json(index) {
    const used = this.used();
    return {
      id: this.id, name: this.name, index, rows: Math.max((used?.r2 ?? -1) + 2, 1000), cols: Math.max((used?.c2 ?? -1) + 2, 26),
      used: used ? rangeText(used) : null, frozen_rows: this.frozen_rows, frozen_cols: this.frozen_cols, color: this.color, code: this.code,
    };
  }
}

/* ---- values, formulas, formats ------------------------------------------------------------------------------- */

function parseInput(input) {
  const text = String(input ?? "");
  if (text === "") return { v: { t: "empty" } };
  if (text.startsWith("'")) return { v: { t: "text", v: text.slice(1) } };
  if (text.startsWith("=") && text.length > 1) return { f: text, v: { t: "empty" } };
  const t = text.trim();
  if (/^(true|false)$/i.test(t)) return { v: { t: "bool", v: /^true$/i.test(t) } };
  const pct = /^([-+]?\d[\d,]*\.?\d*|[-+]?\.\d+)%$/.exec(t);
  if (pct) return { v: { t: "number", v: Number(pct[1].replace(/,/g, "")) / 100 }, fmt: "0%" };
  const money = /^\$([\d,]+\.?\d*)$/.exec(t);
  if (money) return { v: { t: "number", v: Number(money[1].replace(/,/g, "")) }, fmt: "$#,##0.00" };
  if (/^[-+]?(\d[\d,]*\.?\d*|\.\d+)(e[-+]?\d+)?$/i.test(t) && !/^0\d/.test(t)) return { v: { t: "number", v: Number(t.replace(/,/g, "")) } };
  const date = /^(\d{4})-(\d{2})-(\d{2})$/.exec(t);
  if (date) {
    const serial = (Date.UTC(+date[1], +date[2] - 1, +date[3]) - Date.UTC(1899, 11, 30)) / 86_400_000;
    return { v: { t: "number", v: serial }, fmt: "yyyy-mm-dd" };
  }
  return { v: { t: "text", v: text } };
}

function formatValue(v, fmt) {
  if (!v || v.t === "empty") return "";
  if (v.t === "text" || v.t === "error") return v.v;
  if (v.t === "bool") return v.v ? "TRUE" : "FALSE";
  const n = v.v;
  if (fmt) {
    if (/yyyy-mm-dd/.test(fmt)) {
      const d = new Date(Date.UTC(1899, 11, 30) + Math.round(n * 86_400_000));
      return d.toISOString().slice(0, 10);
    }
    const decimals = (/\.(0+)/.exec(fmt)?.[1].length) ?? 0;
    if (fmt.includes("%")) return `${(n * 100).toFixed(decimals)}%`;
    const grouped = fmt.includes(",");
    let body = Math.abs(n).toLocaleString("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals, useGrouping: grouped });
    if (fmt.includes("$")) body = `$${body}`;
    if (/E\+/i.test(fmt)) body = Math.abs(n).toExponential(decimals).toUpperCase();
    return n < 0 ? `-${body}` : body;
  }
  if (Number.isInteger(n)) return String(n);
  return String(Number(n.toPrecision(10)));
}

function tokenize(src) {
  const out = [];
  let i = 0;
  while (i < src.length) {
    const ch = src[i];
    if (/\s/.test(ch)) { i++; continue; }
    if (ch === '"') {
      let j = i + 1;
      let s = "";
      while (j < src.length) {
        if (src[j] === '"' && src[j + 1] === '"') { s += '"'; j += 2; continue; }
        if (src[j] === '"') break;
        s += src[j++];
      }
      out.push({ k: "str", v: s });
      i = j + 1;
      continue;
    }
    const ref = /^(?:'([^']+)'|([A-Za-z_][\w.]*))?!?\$?([A-Za-z]{1,3})\$?(\d+)(?::\$?([A-Za-z]{1,3})\$?(\d+))?/.exec(src.slice(i));
    if (ref && (ref[0].includes("!") || !ref[1] && !ref[2]) && !/[A-Za-z(]/.test(src[i + ref[0].length] ?? "")) {
      out.push({ k: "ref", tab: ref[1] ?? ref[2] ?? null, r1: +ref[4] - 1, c1: colIndex(ref[3]), r2: ref[6] ? +ref[6] - 1 : +ref[4] - 1, c2: ref[5] ? colIndex(ref[5]) : colIndex(ref[3]) });
      i += ref[0].length;
      continue;
    }
    const num = /^\d+(\.\d+)?(e[-+]?\d+)?/i.exec(src.slice(i));
    if (num) { out.push({ k: "num", v: Number(num[0]) }); i += num[0].length; continue; }
    const name = /^[A-Za-z_][\w.]*/.exec(src.slice(i));
    if (name) { out.push({ k: "name", v: name[0].toUpperCase() }); i += name[0].length; continue; }
    const two = src.slice(i, i + 2);
    if (["<>", "<=", ">="].includes(two)) { out.push({ k: "op", v: two }); i += 2; continue; }
    out.push({ k: "op", v: ch });
    i++;
  }
  return out;
}

class FormulaError extends Error {}

function evaluate(wb, tab, formula, seen = new Set()) {
  const tokens = tokenize(formula.slice(1));
  let p = 0;
  const peek = () => tokens[p];
  const next = () => tokens[p++];
  const expect = v => {
    const t = next();
    if (!t || t.v !== v) throw new FormulaError("#NAME?");
  };
  const cellValue = (t, row, col) => {
    const target = t;
    const k = `${target.id}:${row}:${col}`;
    if (seen.has(k)) throw new FormulaError("#CIRC!");
    const cell = target.get(row, col);
    if (!cell) return { t: "empty" };
    if (cell.f) {
      seen.add(k);
      const v = evaluate(wb, target, cell.f, seen);
      seen.delete(k);
      return v;
    }
    return cell.v;
  };
  const refTab = ref => (ref.tab ? wb.tab(ref.tab) : tab);
  const values = ref => {
    const t = refTab(ref);
    const out = [];
    const r2 = Math.min(ref.r2, (t.used()?.r2 ?? 0));
    const c2 = Math.min(ref.c2, (t.used()?.c2 ?? 0));
    for (let r = ref.r1; r <= r2; r++) for (let c = ref.c1; c <= c2; c++) out.push(cellValue(t, r, c));
    return out;
  };
  const num = v => {
    if (v.t === "number") return v.v;
    if (v.t === "bool") return v.v ? 1 : 0;
    if (v.t === "empty") return 0;
    if (v.t === "error") throw new FormulaError(v.v);
    const n = Number(v.v);
    if (Number.isNaN(n)) throw new FormulaError("#VALUE!");
    return n;
  };
  const text = v => (v.t === "number" ? formatValue(v) : v.t === "bool" ? (v.v ? "TRUE" : "FALSE") : v.t === "empty" ? "" : v.v);
  const primary = () => {
    const t = next();
    if (!t) throw new FormulaError("#NAME?");
    if (t.k === "num") return { t: "number", v: t.v };
    if (t.k === "str") return { t: "text", v: t.v };
    if (t.k === "ref") {
      if (t.r1 === t.r2 && t.c1 === t.c2) return cellValue(refTab(t), t.r1, t.c1);
      return { t: "range", ref: t };
    }
    if (t.k === "op" && t.v === "(") {
      const v = expr();
      expect(")");
      return v;
    }
    if (t.k === "op" && t.v === "-") return { t: "number", v: -num(unwrap(power())) };
    if (t.k === "op" && t.v === "+") return power();
    if (t.k === "name") {
      if (t.v === "TRUE" || t.v === "FALSE") return { t: "bool", v: t.v === "TRUE" };
      expect("(");
      const args = [];
      if (peek()?.v !== ")") {
        args.push(expr());
        while (peek()?.v === "," || peek()?.v === ";") {
          next();
          args.push(expr());
        }
      }
      expect(")");
      return call(t.v, args);
    }
    throw new FormulaError("#NAME?");
  };
  const flat = args => args.flatMap(a => (a.t === "range" ? values(a.ref) : [a]));
  const unwrap = v => (v.t === "range" ? (values(v.ref)[0] ?? { t: "empty" }) : v);
  const call = (name, args) => {
    const all = flat(args);
    const nums = all.filter(v => v.t === "number").map(v => v.v);
    switch (name) {
      case "SUM": return { t: "number", v: nums.reduce((a, b) => a + b, 0) };
      case "AVERAGE": if (!nums.length) throw new FormulaError("#DIV/0!"); return { t: "number", v: nums.reduce((a, b) => a + b, 0) / nums.length };
      case "COUNT": return { t: "number", v: nums.length };
      case "COUNTA": return { t: "number", v: all.filter(v => v.t !== "empty").length };
      case "MIN": return { t: "number", v: nums.length ? Math.min(...nums) : 0 };
      case "MAX": return { t: "number", v: nums.length ? Math.max(...nums) : 0 };
      case "ABS": return { t: "number", v: Math.abs(num(unwrap(args[0]))) };
      case "ROUND": { const d = args[1] ? num(unwrap(args[1])) : 0; const f = 10 ** d; return { t: "number", v: Math.round(num(unwrap(args[0])) * f) / f }; }
      case "IF": { const c = unwrap(args[0]); const yes = c.t === "bool" ? c.v : num(c) !== 0; return unwrap(yes ? args[1] ?? { t: "bool", v: true } : args[2] ?? { t: "bool", v: false }); }
      case "CONCAT": case "CONCATENATE": return { t: "text", v: all.map(text).join("") };
      case "LEN": return { t: "number", v: text(unwrap(args[0])).length };
      case "UPPER": return { t: "text", v: text(unwrap(args[0])).toUpperCase() };
      case "LOWER": return { t: "text", v: text(unwrap(args[0])).toLowerCase() };
      default: throw new FormulaError("#NAME?");
    }
  };
  const power = () => {
    let left = primary();
    while (peek()?.v === "^") {
      next();
      left = { t: "number", v: num(unwrap(left)) ** num(unwrap(primary())) };
    }
    while (peek()?.v === "%") {
      next();
      left = { t: "number", v: num(unwrap(left)) / 100 };
    }
    return left;
  };
  const term = () => {
    let left = power();
    while (peek()?.v === "*" || peek()?.v === "/") {
      const op = next().v;
      const a = num(unwrap(left));
      const b = num(unwrap(power()));
      if (op === "/" && b === 0) throw new FormulaError("#DIV/0!");
      left = { t: "number", v: op === "*" ? a * b : a / b };
    }
    return left;
  };
  const sum = () => {
    let left = term();
    while (peek()?.v === "+" || peek()?.v === "-") {
      const op = next().v;
      const a = num(unwrap(left));
      const b = num(unwrap(term()));
      left = { t: "number", v: op === "+" ? a + b : a - b };
    }
    return left;
  };
  const concat = () => {
    let left = sum();
    while (peek()?.v === "&") {
      next();
      left = { t: "text", v: text(unwrap(left)) + text(unwrap(sum())) };
    }
    return left;
  };
  const expr = () => {
    const left = concat();
    const op = peek();
    if (op && ["=", "<>", "<", ">", "<=", ">="].includes(op.v)) {
      next();
      const a = unwrap(left);
      const b = unwrap(concat());
      const x = a.t === "number" && b.t === "number" ? a.v : text(a).toLowerCase();
      const y = a.t === "number" && b.t === "number" ? b.v : text(b).toLowerCase();
      const r = { "=": x === y, "<>": x !== y, "<": x < y, ">": x > y, "<=": x <= y, ">=": x >= y }[op.v];
      return { t: "bool", v: r };
    }
    return left;
  };
  try {
    const v = unwrap(expr());
    if (p < tokens.length) throw new FormulaError("#NAME?");
    return v;
  } catch (error) {
    if (error instanceof FormulaError) return { t: "error", v: error.message };
    if (error instanceof Fail) return { t: "error", v: "#REF!" };
    throw error;
  }
}

/* ---- sheets, lines, folders ---------------------------------------------------------------------------------- */

const sheets = new Map();
const folders = new Map();
const lines = new Map(); // line id → { sheet, copy, wb, version, log, clients, viewers, threads, undo, redo }

function newLine(sheet, copy, wb) {
  const line = { id: copy ? `${sheet.id}:${copy.id}` : sheet.id, sheet, copy, wb, version: 0, log: [], clients: new Set(), viewers: new Map(), undo: new Map(), redo: new Map(), named: new Map(), snapshots: new Map([[0, wb.clone()]]) };
  lines.set(line.id, line);
  return line;
}

function makeSheet(owner, title, folderId = null) {
  const sheet = { id: id12(), title: title || "Untitled sheet", owner, folder_id: folderId, members: new Map(), public: { enabled: false, access: "view" }, created_at: now(), created_by: owner, updated_at: now(), updated_by: owner, trashed_at: null, copies: [] };
  sheets.set(sheet.id, sheet);
  const wb = new Workbook();
  wb.addTab("Tab 1");
  sheet.main = newLine(sheet, null, wb);
  return sheet;
}

function roleOf(sheet, me) {
  if (!me) return sheet.public.enabled ? "viewer" : null;
  if (sheet.owner.uuid === me.uuid) return "owner";
  const member = sheet.members.get(me.uuid);
  let role = member?.role ?? null;
  for (let folderId = sheet.folder_id; folderId; folderId = folders.get(folderId)?.parent_id ?? null) {
    const folder = folders.get(folderId);
    if (!folder) break;
    if (folder.owner.uuid === me.uuid) return "owner";
    const r = folder.members.get(me.uuid)?.role;
    if (r && rank(r) > rank(role)) role = r;
  }
  if (!role && sheet.public.enabled) role = sheet.public.access === "edit" ? "editor" : sheet.public.access === "comment" ? "commenter" : "viewer";
  return role;
}
const rank = role => ({ viewer: 1, commenter: 2, editor: 3, owner: 4 })[role] ?? 0;

function sheetJson(sheet, me, withTabs = false) {
  const main = sheet.main;
  const out = {
    id: sheet.id, title: sheet.title, owner: sheet.owner, folder_id: sheet.folder_id, role: roleOf(sheet, me), public: sheet.public, version: main.version,
    created_at: sheet.created_at, created_by: sheet.created_by, updated_at: sheet.updated_at, updated_by: sheet.updated_by, trashed_at: sheet.trashed_at,
    copies: sheet.copies.filter(c => c.status === "open").length,
  };
  if (withTabs) out.tabs = main.wb.tabs.map((t, i) => t.json(i));
  return out;
}

function folderJson(folder, me) {
  return { id: folder.id, name: folder.name, parent_id: folder.parent_id, owner: folder.owner, role: folder.owner.uuid === me?.uuid ? "owner" : folder.members.get(me?.uuid)?.role ?? "viewer", created_at: folder.created_at, trashed_at: folder.trashed_at };
}

function need(sheet, me, wanted) {
  const role = roleOf(sheet, me);
  if (!role) throw me ? notFound("This sheet") : new Fail(401, "signed_out", "Sign in to read this sheet: it is not public.", "Sign in, then open it again.");
  if (rank(role) < rank(wanted)) throw new Fail(403, "not_allowed", `You are a ${role} of this sheet, so you cannot do that.`, "Ask its owner to make you an editor.");
  return role;
}

function lineFor(sheetId, copyId, me, wanted = "viewer") {
  const sheet = sheets.get(sheetId);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, wanted);
  if (!copyId) return sheet.main;
  const copy = sheet.copies.find(c => c.id === copyId);
  if (!copy) throw notFound("This copy");
  return lines.get(`${sheet.id}:${copy.id}`);
}

/* ---- cells answers ------------------------------------------------------------------------------------------- */

function recalc(line) {
  // Formulas are evaluated when read; nothing is cached.
  void line;
}

function cellJson(line, tab, row, col, cell) {
  const style = cell.s ? line.wb.styles[cell.s] : null;
  const v = cell.f ? evaluate(line.wb, tab, cell.f) : cell.v;
  const out = { a: a1(row, col), r: row, c: col, v, t: formatValue(v, style?.num_fmt) };
  if (cell.f) out.f = cell.f;
  if (cell.s) out.s = cell.s;
  if (cell.by) out.by = cell.by;
  if (cell.at) out.at = cell.at;
  return out;
}

function cellsAnswer(line, tab, range) {
  const used = tab.used();
  const r = range ? parseRange(range, Math.max((used?.r2 ?? 0) + 1, 1000), Math.max((used?.c2 ?? 0) + 1, 26)) : used ?? { r1: 0, c1: 0, r2: 0, c2: 0 };
  if (!r) throw new Fail(400, "invalid_request", `"${range}" is not a range.`, "Use A1 text such as B7, A1:C9, A:C or 3:5.");
  const cells = [];
  const styles = {};
  const by = {};
  const r2 = Math.min(r.r2, used?.r2 ?? -1);
  const c2 = Math.min(r.c2, used?.c2 ?? -1);
  if ((r2 - r.r1 + 1) * (c2 - r.c1 + 1) > 2_000_000) throw new Fail(413, "too_large", "That range holds more than 2,000,000 cells.", "Read a smaller range.");
  const addCell = (row, col, cell) => {
    const json = cellJson(line, tab, row, col, cell);
    cells.push(json);
    if (json.s) styles[json.s] = line.wb.styles[json.s];
    if (json.by && accounts.has(json.by)) by[json.by] = accounts.get(json.by);
  };
  if (tab.generator && r.r1 < tab.generatedRows) {
    for (let row = r.r1; row <= r2; row++) for (let col = r.c1; col <= c2; col++) {
      const cell = tab.get(row, col);
      if (cell) addCell(row, col, cell);
    }
  } else {
    const found = [];
    for (const [k, cell] of tab.cells) {
      if (cell.deleted) continue;
      const row = keyRow(k);
      const col = keyCol(k);
      if (row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2) found.push([row, col, cell]);
    }
    found.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    for (const [row, col, cell] of found) addCell(row, col, cell);
  }
  const col_widths = {};
  for (const [c, w] of tab.colWidths) if (c >= r.c1 && c <= r.c2) col_widths[colName(c)] = w;
  const row_heights = {};
  for (const [row, h] of tab.rowHeights) if (row >= r.r1 && row <= r.r2) row_heights[String(row + 1)] = h;
  const index = line.wb.tabs.indexOf(tab);
  return {
    version: line.version, tab: tab.json(index), range: rangeText(r), cells, styles, accounts: by, col_widths, row_heights,
    hidden_rows: [...tab.hiddenRows].filter(x => x >= r.r1 && x <= r.r2).map(x => x + 1), hidden_cols: [...tab.hiddenCols].filter(x => x >= r.c1 && x <= r.c2).map(colName),
    merges: tab.merges.filter(m => m.r1 <= r.r2 && m.r2 >= r.r1 && m.c1 <= r.c2 && m.c2 >= r.c1).map(rangeText), frozen_rows: tab.frozen_rows, frozen_cols: tab.frozen_cols,
    charts: tab.charts, protections: tab.protections,
  };
}

/* ---- edits --------------------------------------------------------------------------------------------------- */

function setCell(line, tab, row, col, patch, me, before) {
  const k = key(row, col);
  if (before && !before.has(`${tab.id}:${k}`)) before.set(`${tab.id}:${k}`, { tab: tab.id, k, cell: tab.cells.has(k) ? { ...tab.cells.get(k) } : null, generated: !tab.cells.has(k) ? tab.get(row, col) : null });
  const current = tab.get(row, col) ?? { v: { t: "empty" } };
  const next = { ...current, ...patch, by: me.uuid, at: now() };
  if (next.v?.t === "empty" && !next.f && !next.s) {
    if (tab.generator && row < tab.generatedRows && col < tab.generatedCols) tab.cells.set(k, { deleted: true });
    else tab.cells.delete(k);
  } else {
    delete next.deleted;
    tab.cells.set(k, next);
  }
}

function applyStyle(line, tab, row, col, patch, me, before) {
  const cell = tab.get(row, col);
  const base = patch.clear ? {} : { ...(line.wb.styles[cell?.s ?? 0] ?? {}) };
  for (const [name, value] of Object.entries(patch)) {
    if (name === "clear") continue;
    if (value === null) delete base[name];
    else base[name] = value;
  }
  setCell(line, tab, row, col, { s: line.wb.styleId(base) }, me, before);
}

function protectedBy(tab, row, col, me, sheet) {
  if (sheet.owner.uuid === me.uuid) return null;
  for (const p of tab.protections) {
    const r = p.range ? parseRange(p.range) : { r1: 0, c1: 0, r2: MAX_ROWS, c2: MAX_COLS };
    if (r && row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2 && !p.editors.some(e => e.uuid === me.uuid)) return p;
  }
  return null;
}

function applyEdits(line, me, body) {
  const edits = body.edits ?? [];
  const before = new Map();
  const changes = [];
  const overwrote = [];
  const changed = new Map(); // tab id → rects
  const touched = (tab, r) => {
    changes.push({ tab: tab.id, range: rangeText(r) });
    const list = changed.get(tab.id) ?? [];
    list.push(r);
    changed.set(tab.id, list);
  };
  const base = Number(body.base_version ?? line.version);
  const wb = line.wb;
  let kind = "edit";
  let undoable = true;
  for (const [index, edit] of edits.entries()) {
    try {
      switch (edit.type) {
        case "set": {
          const tab = wb.tab(edit.tab);
          const items = edit.cells ? edit.cells.map(c => ({ ...parseRange(c.a), input: c.input })) : [];
          if (edit.rows) {
            const at = parseRange(edit.at);
            edit.rows.forEach((row, i) => row.forEach((input, j) => items.push({ r1: at.r1 + i, c1: at.c1 + j, input })));
          }
          if (items.length > 200_000) throw new Fail(413, "too_large", `The edit sets ${items.length} cells; at most 200,000 go in one edit.`, "Split it, or import the file as a new tab.");
          for (const item of items) {
            const p = protectedBy(tab, item.r1, item.c1, me, line.sheet);
            if (p) throw new Fail(403, "protected", `${a1(item.r1, item.c1)} is protected (${p.description || "a protected range"}).`, "Ask the sheet's owner to add you to the protection's editors.");
            const parsed = parseInput(item.input);
            const prev = tab.get(item.r1, item.c1);
            if (prev?.by && prev.by !== me.uuid && line.log.some(l => l.version > base && l.by.uuid === prev.by && l.cells?.has(`${tab.id}:${key(item.r1, item.c1)}`))) {
              overwrote.push({ tab: tab.id, a: a1(item.r1, item.c1), by: accounts.get(prev.by) ?? { uuid: prev.by, id: "c:someone", kind: "carbon", display_name: "Someone" }, at: prev.at, before: prev.f ? evaluate(wb, tab, prev.f) : prev.v, version: line.version });
            }
            const patch = { v: parsed.v, f: parsed.f ?? undefined };
            if (!parsed.f) delete patch.f;
            if (parsed.fmt && !(prev?.s && wb.styles[prev.s]?.num_fmt)) {
              const style = { ...(wb.styles[prev?.s ?? 0] ?? {}), num_fmt: parsed.fmt };
              patch.s = wb.styleId(style);
            }
            setCell(line, tab, item.r1, item.c1, { ...patch, f: parsed.f }, me, before);
            if (!parsed.f) {
              const c = tab.cells.get(key(item.r1, item.c1));
              if (c) delete c.f;
            }
            touched(tab, { r1: item.r1, c1: item.c1, r2: item.r1, c2: item.c1 });
          }
          break;
        }
        case "values": {
          const tab = wb.tab(edit.tab);
          const at = parseRange(edit.at);
          edit.rows.forEach((row, i) => row.forEach((value, j) => {
            const v = value === null || value === undefined ? { t: "empty" } : typeof value === "object" && "t" in value ? value : typeof value === "number" ? { t: "number", v: value } : typeof value === "boolean" ? { t: "bool", v: value } : { t: "text", v: String(value) };
            setCell(line, tab, at.r1 + i, at.c1 + j, { v }, me, before);
          }));
          touched(tab, { r1: at.r1, c1: at.c1, r2: at.r1 + edit.rows.length - 1, c2: at.c1 + Math.max(0, ...edit.rows.map(r => r.length)) - 1 });
          break;
        }
        case "style":
        case "clear": {
          const tab = wb.tab(edit.tab);
          const used = tab.used() ?? { r1: 0, c1: 0, r2: 0, c2: 0 };
          const r = parseRange(edit.range, used.r2 + 1, used.c2 + 1);
          const r2 = edit.type === "style" ? r.r2 : Math.min(r.r2, used.r2);
          const c2 = edit.type === "style" ? r.c2 : Math.min(r.c2, used.c2);
          if ((r2 - r.r1 + 1) * (c2 - r.c1 + 1) > 2_000_000) throw new Fail(413, "too_large", "That range is too large for the mock.", "Choose a smaller range.");
          for (let row = r.r1; row <= r2; row++) for (let col = r.c1; col <= c2; col++) {
            if (edit.type === "style") applyStyle(line, tab, row, col, edit.patch ?? {}, me, before);
            else if (tab.get(row, col)) {
              if (edit.what === "formats") setCell(line, tab, row, col, { s: 0 }, me, before);
              else if (edit.what === "all") setCell(line, tab, row, col, { v: { t: "empty" }, f: undefined, s: 0 }, me, before);
              else setCell(line, tab, row, col, { v: { t: "empty" }, f: undefined }, me, before);
              const c = tab.cells.get(key(row, col));
              if (c && edit.what !== "formats") delete c.f;
              if (c && !c.s && c.v?.t === "empty" && !c.f) tab.cells.delete(key(row, col));
            }
          }
          touched(tab, { r1: r.r1, c1: r.c1, r2, c2 });
          break;
        }
        case "insert_rows":
        case "delete_rows":
        case "insert_cols":
        case "delete_cols": {
          const tab = wb.tab(edit.tab);
          const rowsAxis = edit.type.endsWith("rows");
          const insert = edit.type.startsWith("insert");
          const moved = new Map();
          for (const [k, cell] of tab.cells) {
            let row = keyRow(k);
            let col = keyCol(k);
            const pos = rowsAxis ? row : col;
            if (insert && pos >= edit.at) { if (rowsAxis) row += edit.count; else col += edit.count; }
            if (!insert && pos >= edit.at && pos < edit.at + edit.count) continue;
            if (!insert && pos >= edit.at + edit.count) { if (rowsAxis) row -= edit.count; else col -= edit.count; }
            moved.set(key(row, col), cell);
          }
          tab.cells = moved;
          undoable = false;
          changes.push({ tab: tab.id, reload: true });
          break;
        }
        case "sort": {
          const tab = wb.tab(edit.tab);
          const r = parseRange(edit.range);
          const by = edit.by?.[0];
          const col = by ? colIndex(by.col) : r.c1;
          const start = edit.header ? r.r1 + 1 : r.r1;
          const used = tab.used();
          const end = Math.min(r.r2, used?.r2 ?? r.r2);
          const rows = [];
          for (let row = start; row <= end; row++) {
            const cells = [];
            for (let c = r.c1; c <= Math.min(r.c2, used?.c2 ?? r.c2); c++) cells.push(tab.get(row, c));
            rows.push(cells);
          }
          const val = cell => (cell?.f ? evaluate(wb, tab, cell.f) : cell?.v) ?? { t: "empty" };
          rows.sort((x, y) => {
            const a = val(x[col - r.c1]);
            const b = val(y[col - r.c1]);
            if (a.t === "empty") return 1;
            if (b.t === "empty") return -1;
            const cmp = a.t === "number" && b.t === "number" ? a.v - b.v : String(a.v).localeCompare(String(b.v));
            return by?.asc === false ? -cmp : cmp;
          });
          rows.forEach((cells, i) => cells.forEach((cell, j) => {
            const k = key(start + i, r.c1 + j);
            if (!before.has(`${tab.id}:${k}`)) before.set(`${tab.id}:${k}`, { tab: tab.id, k, cell: tab.cells.has(k) ? { ...tab.cells.get(k) } : null });
            if (cell) tab.cells.set(k, { ...cell, by: me.uuid, at: now() });
            else tab.cells.delete(k);
          }));
          touched(tab, { r1: start, c1: r.c1, r2: end, c2: r.c2 });
          break;
        }
        case "fill": {
          const tab = wb.tab(edit.tab);
          const from = parseRange(edit.from);
          const to = parseRange(edit.to);
          const height = from.r2 - from.r1 + 1;
          const width = from.c2 - from.c1 + 1;
          for (let row = to.r1; row <= to.r2; row++) for (let col = to.c1; col <= to.c2; col++) {
            const sr = from.r1 + ((((row - from.r1) % height) + height) % height);
            const sc = from.c1 + ((((col - from.c1) % width) + width) % width);
            const source = tab.get(sr, sc);
            if (!source) continue;
            const dr = row - sr;
            const dc = col - sc;
            const f = source.f ? source.f.replace(/(\$?)([A-Z]{1,3})(\$?)(\d+)/g, (m, ca, c, ra, r) => `${ca}${ca ? c : colName(colIndex(c) + dc)}${ra}${ra ? r : Number(r) + dr}`) : undefined;
            let v = source.v;
            // A number series continues (1, 2 → 3, 4).
            if (!source.f && source.v?.t === "number" && height >= 2 && width === 1 && dc === 0) {
              const first = tab.get(from.r1, sc)?.v;
              const second = tab.get(from.r1 + 1, sc)?.v;
              if (first?.t === "number" && second?.t === "number") v = { t: "number", v: first.v + (second.v - first.v) * (row - from.r1) };
            }
            setCell(line, tab, row, col, { v, f, s: source.s }, me, before);
            if (!f) delete tab.cells.get(key(row, col))?.f;
          }
          touched(tab, to);
          break;
        }
        case "add_tab": {
          if (wb.tabs.some(t => t.name.toLowerCase() === String(edit.name).toLowerCase())) throw new Fail(409, "conflict", `A tab named "${edit.name}" is already in this sheet.`, "Pick another name.");
          const tab = wb.addTab(edit.name, edit.index);
          changes.push({ tabs: true }, { tab: tab.id, added: true });
          undoable = false;
          break;
        }
        case "rename_tab": wb.tab(edit.tab).name = edit.name; changes.push({ tabs: true }); undoable = false; break;
        case "move_tab": {
          const tab = wb.tab(edit.tab);
          wb.tabs.splice(wb.tabs.indexOf(tab), 1);
          wb.tabs.splice(Math.max(0, Math.min(edit.index, wb.tabs.length)), 0, tab);
          changes.push({ tabs: true });
          undoable = false;
          break;
        }
        case "delete_tab": {
          if (wb.tabs.length <= 1) throw new Fail(409, "conflict", "A sheet keeps at least one tab.", "Add another tab first.");
          const tab = wb.tab(edit.tab);
          wb.tabs.splice(wb.tabs.indexOf(tab), 1);
          changes.push({ tabs: true });
          undoable = false;
          break;
        }
        case "tab_props": {
          const tab = wb.tab(edit.tab);
          if (edit.frozen_rows !== undefined) tab.frozen_rows = edit.frozen_rows;
          if (edit.frozen_cols !== undefined) tab.frozen_cols = edit.frozen_cols;
          if (edit.color !== undefined) tab.color = edit.color;
          changes.push({ tabs: true }, { tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "col_width":
        case "row_height": {
          const tab = wb.tab(edit.tab);
          const r = parseRange(edit.type === "col_width" ? edit.cols : edit.rows);
          const size = edit.type === "col_width" ? edit.width : edit.height;
          if (edit.type === "col_width") for (let c = r.c1; c <= r.c2; c++) { if (size === null) tab.colWidths.delete(c); else tab.colWidths.set(c, size); }
          else for (let row = r.r1; row <= r.r2; row++) { if (size === null) tab.rowHeights.delete(row); else tab.rowHeights.set(row, size); }
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "hide_rows": case "show_rows": case "hide_cols": case "show_cols": {
          const tab = wb.tab(edit.tab);
          const r = parseRange(edit.range);
          const set = edit.type.endsWith("rows") ? tab.hiddenRows : tab.hiddenCols;
          const [a, b] = edit.type.endsWith("rows") ? [r.r1, r.r2] : [r.c1, r.c2];
          for (let i = a; i <= Math.min(b, a + 100_000); i++) { if (edit.type.startsWith("hide")) set.add(i); else set.delete(i); }
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "merge": {
          const tab = wb.tab(edit.tab);
          const r = parseRange(edit.range);
          tab.merges = tab.merges.filter(m => !(m.r1 <= r.r2 && m.r2 >= r.r1 && m.c1 <= r.c2 && m.c2 >= r.c1));
          tab.merges.push(r);
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "unmerge": {
          const tab = wb.tab(edit.tab);
          const r = parseRange(edit.range);
          tab.merges = tab.merges.filter(m => !(m.r1 <= r.r2 && m.r2 >= r.r1 && m.c1 <= r.c2 && m.c2 >= r.c1));
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "chart": {
          const tab = wb.tab(edit.chart.tab);
          const chart = { ...edit.chart, id: edit.chart.id ?? randomUUID(), tab: tab.id };
          tab.charts = [...tab.charts.filter(c => c.id !== chart.id), chart];
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "delete_chart": for (const tab of wb.tabs) tab.charts = tab.charts.filter(c => c.id !== edit.id); changes.push({ meta: true }); undoable = false; break;
        case "protect": {
          if (line.sheet.owner.uuid !== me.uuid) throw new Fail(403, "not_allowed", "Only the owner protects ranges.", "Ask the sheet's owner.");
          const tab = wb.tab(edit.protection.tab);
          const editors = (edit.protection.editors ?? []).map(id => lookupAccount(id)).filter(Boolean);
          const p = { id: edit.protection.id ?? randomUUID(), tab: tab.id, range: edit.protection.range ?? null, editors, description: edit.protection.description ?? "" };
          tab.protections = [...tab.protections.filter(x => x.id !== p.id), p];
          changes.push({ tab: tab.id, reload: true });
          undoable = false;
          break;
        }
        case "unprotect": for (const tab of wb.tabs) tab.protections = tab.protections.filter(p => p.id !== edit.id); changes.push({ meta: true }); undoable = false; break;
        case "undo":
        case "redo": {
          const stack = (edit.type === "undo" ? line.undo : line.redo).get(me.uuid) ?? [];
          const entry = stack.pop();
          if (!entry) throw new Fail(409, "conflict", `There is nothing of yours to ${edit.type} on this line.`, "Make a change first.");
          const other = edit.type === "undo" ? line.redo : line.undo;
          const back = new Map();
          for (const item of entry.before.values()) {
            const tab = wb.tab(item.tab);
            back.set(`${item.tab}:${item.k}`, { tab: item.tab, k: item.k, cell: tab.cells.has(item.k) ? { ...tab.cells.get(item.k) } : null });
            if (item.cell) tab.cells.set(item.k, { ...item.cell });
            else tab.cells.delete(item.k);
            touched(tab, { r1: keyRow(item.k), c1: keyCol(item.k), r2: keyRow(item.k), c2: keyCol(item.k) });
          }
          other.set(me.uuid, [...(other.get(me.uuid) ?? []), { before: back }]);
          kind = edit.type;
          return finish(line, me, changes, overwrote, changed, null, kind);
        }
        case "names": wb.names = edit.names; changes.push({ meta: true }); undoable = false; break;
        default: throw new Fail(422, "invalid_field", `The mock does not know the edit "${edit.type}".`, "Use one of the edits in docs/API.md.", { index });
      }
    } catch (error) {
      if (error instanceof Fail) {
        error.details = { ...(error.details ?? {}), index };
        // Nothing applies when one edit fails: put back what was changed.
        for (const item of before.values()) {
          const tab = wb.tabs.find(t => t.id === item.tab);
          if (!tab) continue;
          if (item.cell) tab.cells.set(item.k, item.cell);
          else tab.cells.delete(item.k);
        }
        throw error;
      }
      throw error;
    }
  }
  if (undoable && before.size) {
    line.undo.set(me.uuid, [...(line.undo.get(me.uuid) ?? []), { before }]);
    line.redo.set(me.uuid, []);
  }
  return finish(line, me, changes, overwrote, changed, before, kind);
}

function finish(line, me, changes, overwrote, changed, before, kind) {
  line.version += 1;
  const at = now();
  const cellsChanged = new Set(before ? [...before.keys()] : []);
  line.log.push({ version: line.version, by: me, at, kind, cells: cellsChanged, changes });
  line.snapshots.set(line.version, line.wb.clone());
  if (line.snapshots.size > 200) line.snapshots.delete([...line.snapshots.keys()][1]);
  line.sheet.updated_at = at;
  line.sheet.updated_by = me;
  recalc(line);
  // The changed cells' new contents (and every formula on the tab, since anything may depend on them).
  const cells = [];
  for (const [tabId, rects] of changed) {
    const tab = line.wb.tabs.find(t => t.id === tabId);
    if (!tab) continue;
    for (const r of rects) for (let row = r.r1; row <= Math.min(r.r2, r.r1 + 2000); row++) for (let col = r.c1; col <= Math.min(r.c2, r.c1 + 100); col++) {
      const cell = tab.get(row, col);
      cells.push(cell ? { ...cellJson(line, tab, row, col, cell), tab: tabId } : { a: a1(row, col), r: row, c: col, v: { t: "empty" }, t: "", tab: tabId });
    }
    for (const [k, cell] of tab.cells) {
      if (!cell.f || cell.deleted) continue;
      cells.push({ ...cellJson(line, tab, keyRow(k), keyCol(k), cell), tab: tabId });
      changes.push({ tab: tabId, range: a1(keyRow(k), keyCol(k)) });
    }
  }
  const event = { version: line.version, by: me, at, changes, ...(cells.length <= 2000 ? { cells } : {}) };
  broadcast(line, "change", event);
  return { version: line.version, changes, overwrote, skipped: 0 };
}

/* ---- live ---------------------------------------------------------------------------------------------------- */

function broadcast(line, type, data) {
  const text = `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
  for (const client of line.clients) client.write(text);
}

function presence(line) {
  const cutoff = Date.now() - 45_000;
  for (const [k, v] of line.viewers) if (Date.parse(v.seen_at) < cutoff) line.viewers.delete(k);
  broadcast(line, "presence", { viewers: [...line.viewers.values()] });
}
setInterval(() => {
  for (const line of lines.values()) {
    if (!line.clients.size) continue;
    for (const client of line.clients) client.write("event: ping\ndata: {}\n\n");
    presence(line);
  }
}, 20_000).unref();

/* ---- versions ------------------------------------------------------------------------------------------------ */

function versionsOf(line) {
  const out = [];
  for (const entry of line.log) {
    const last = out[out.length - 1];
    if (last && last.by.uuid === entry.by.uuid && last.kind === entry.kind && Date.parse(entry.at) - Date.parse(last.ended_at) < 300_000 && !line.named.has(last.version)) {
      last.version = entry.version;
      last.ended_at = entry.at;
      last.cells += entry.cells.size;
    } else out.push({ version: entry.version, from_version: entry.version, by: entry.by, started_at: entry.at, ended_at: entry.at, kind: entry.kind, cells: entry.cells.size });
  }
  return out.reverse().map(v => ({ version: v.version, from_version: v.from_version, by: v.by, started_at: v.started_at, ended_at: v.ended_at, summary: `${v.by.id} ${v.kind === "edit" ? `changed ${v.cells} cell${v.cells === 1 ? "" : "s"}` : v.kind === "undo" ? "undid a change" : v.kind}`, name: line.named.get(v.version) ?? null, kind: v.kind }));
}

function diffWorkbooks(a, b, limit = 500) {
  const cells = [];
  const tabs = [];
  let changedCount = 0;
  for (const tb of b.tabs) {
    const ta = a.tabs.find(t => t.id === tb.id);
    let n = 0;
    const keys = new Set([...(ta ? ta.cells.keys() : []), ...tb.cells.keys()]);
    for (const k of keys) {
      const x = ta?.cells.get(k);
      const y = tb.cells.get(k);
      if (JSON.stringify(x?.v) === JSON.stringify(y?.v) && x?.f === y?.f && (x?.s ?? 0) === (y?.s ?? 0)) continue;
      n++;
      changedCount++;
      if (cells.length < limit) cells.push({ tab: tb.id, row: keyRow(k), col: keyCol(k), address: a1(keyRow(k), keyCol(k)), before: x ? { value: x.v, formula: x.f } : {}, after: y ? { value: y.v, formula: y.f } : {}, by: y?.by ?? x?.by });
    }
    tabs.push({ tab: tb.id, name: tb.name, added: !ta, removed: false, renamed_from: ta && ta.name !== tb.name ? ta.name : undefined, rows_added: 0, rows_removed: 0, cols_added: 0, cols_removed: 0, cells_changed: n });
  }
  for (const ta of a.tabs) if (!b.tabs.some(t => t.id === ta.id)) tabs.push({ tab: ta.id, name: ta.name, removed: true, rows_added: 0, rows_removed: 0, cols_added: 0, cols_removed: 0, cells_changed: ta.cells.size });
  const by = {};
  for (const c of cells) if (c.by && accounts.has(c.by)) by[c.by] = accounts.get(c.by);
  return { tabs, cells, cells_changed: changedCount, truncated: changedCount > cells.length, accounts: by };
}

/* ---- comments ------------------------------------------------------------------------------------------------ */

const threads = new Map();
function threadJson(t) {
  return { ...t, comments: t.comments.filter(c => !c.deleted) };
}
function mentionsIn(body) {
  return [...String(body).matchAll(/@((?:c|si):[a-z0-9_-]{3,30})/g)].map(m => lookupAccount(m[1])).filter(Boolean);
}

/* ---- seed data ----------------------------------------------------------------------------------------------- */

function seedFor(me) {
  if ([...sheets.values()].some(s => s.owner.uuid === me.uuid)) return;
  const folder = { id: `f${id12()}`, name: "Finance", parent_id: null, owner: me, members: new Map(), created_at: now(), trashed_at: null };
  folders.set(folder.id, folder);
  const q3 = makeSheet(me, "Q3 revenue");
  const wb = q3.main.wb;
  const tab = wb.tabs[0];
  tab.name = "Sales";
  const rows = [["Region", "July", "August", "September", "Total"], ["North", 1200, 1350, 1410], ["South", 980, 1010, 1190], ["East", 1530, 1490, 1620], ["West", 870, 920, 1005]];
  rows.forEach((row, r) => row.forEach((value, c) => {
    const parsed = parseInput(String(value));
    tab.cells.set(key(r, c), { ...parsed, by: me.uuid, at: now() });
  }));
  for (let r = 1; r <= 4; r++) tab.cells.set(key(r, 4), { f: `=SUM(B${r + 1}:D${r + 1})`, v: { t: "empty" }, by: me.uuid, at: now() });
  tab.cells.set(key(5, 0), { v: { t: "text", v: "Total" }, by: me.uuid, at: now() });
  for (let c = 1; c <= 4; c++) tab.cells.set(key(5, c), { f: `=SUM(${colName(c)}2:${colName(c)}5)`, v: { t: "empty" }, by: me.uuid, at: now() });
  const bold = wb.styleId({ bold: true, fill: "#EEF0F4" });
  for (let c = 0; c <= 4; c++) tab.cells.get(key(0, c)).s = bold;
  const money = wb.styleId({ num_fmt: "#,##0" });
  for (let r = 1; r <= 5; r++) for (let c = 1; c <= 4; c++) { const cell = tab.cells.get(key(r, c)); if (cell) cell.s = money; }
  tab.frozen_rows = 1;
  tab.colWidths.set(0, 140);
  tab.charts.push({ id: randomUUID(), tab: tab.id, kind: "column", title: "Total by region", range: "A1:B5", anchor: "H2", width: 420, height: 260 });
  const big = wb.addTab("Million rows");
  big.generator = (row, col) => {
    if (row === 0) return { v: { t: "text", v: ["id", "item", "amount", "region"][col] } };
    if (col === 0) return { v: { t: "number", v: row } };
    if (col === 1) return { v: { t: "text", v: `Item ${row}` } };
    if (col === 2) return { v: { t: "number", v: ((row * 7919) % 10000) / 100 } };
    return { v: { t: "text", v: ["North", "South", "East", "West"][row % 4] } };
  };
  big.generatedRows = 1_000_001;
  big.generatedCols = 4;
  big.frozen_rows = 1;
  q3.folder_id = null;
  const plan = makeSheet(me, "Hiring plan", folder.id);
  const ptab = plan.main.wb.tabs[0];
  [["Role", "Quarter", "Count"], ["Analyst", "Q4", "2"], ["Engineer", "Q4", "3"]].forEach((row, r) => row.forEach((value, c) => ptab.cells.set(key(r, c), { ...parseInput(value), by: me.uuid, at: now() })));
  makeSheet(me, "Budget 2027");
}

/* ---- the server ---------------------------------------------------------------------------------------------- */

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks);
}

function send(res, status, body, headers = {}) {
  if (status === 204) {
    res.writeHead(204, headers);
    res.end();
    return;
  }
  const text = typeof body === "string" ? body : JSON.stringify(body);
  res.writeHead(status, { "content-type": typeof body === "string" ? "text/plain; charset=utf-8" : "application/json", ...headers });
  res.end(text);
}

function parseCsv(text, delimiter) {
  const rows = [];
  let row = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') { field += '"'; i++; }
      else if (ch === '"') quoted = false;
      else field += ch;
      continue;
    }
    if (ch === '"' && field === "") quoted = true;
    else if (ch === delimiter) { row.push(field); field = ""; }
    else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else field += ch;
  }
  if (field || row.length) { row.push(field); rows.push(row); }
  return rows;
}

function fillTabFrom(tab, filename, body, me) {
  const ext = filename.toLowerCase().split(".").pop();
  if (ext === "csv" || ext === "tsv" || ext === "txt") {
    const rows = parseCsv(body.toString("utf8"), ext === "tsv" ? "\t" : ",");
    rows.forEach((row, r) => row.forEach((value, c) => {
      const parsed = parseInput(value);
      if (parsed.v.t !== "empty" || parsed.f) tab.cells.set(key(r, c), { ...parsed, by: me.uuid, at: now() });
    }));
    return;
  }
  if (ext === "json") {
    const data = JSON.parse(body.toString("utf8"));
    const rows = Array.isArray(data) ? data : data.tabs?.[0]?.rows ?? [];
    const cols = rows.length && !Array.isArray(rows[0]) ? Object.keys(rows[0]) : null;
    const grid = cols ? [cols, ...rows.map(r => cols.map(c => r[c]))] : rows;
    grid.forEach((row, r) => row.forEach((value, c) => tab.cells.set(key(r, c), { ...parseInput(value == null ? "" : String(value)), by: me.uuid, at: now() })));
    return;
  }
  // The mock reads text formats only; the real service reads every format.
  tab.cells.set(key(0, 0), { v: { t: "text", v: `The mock service read ${filename} as a placeholder (${body.length} bytes).` }, by: me.uuid, at: now() });
}

const routes = [];
const route = (method, pattern, handler) => routes.push({ method, pattern: new RegExp(`^${pattern}$`), handler });

const LINE = "/v1/sheets/([a-z0-9]+)(?:/copies/(c[a-z0-9]+))?";

route("GET", "/healthz", () => "ok");
route("GET", "/v1/meta", () => ({ app_id: "calc", version: "0.1.0-mock", accounts_url: "http://localhost:9990", limits: { max_rows: MAX_ROWS, max_cols: MAX_COLS, max_upload_bytes: 524288000, max_cells_per_edit: 200000, browser_command_timeout_seconds: 60, conversion_timeout_seconds: 60 } }));
route("GET", "/v1/me", ({ me }) => ({ account: me }));
route("GET", "/v1/accounts/lookup", ({ url }) => {
  const found = lookupAccount(url.searchParams.get("id") ?? "");
  if (!found) throw new Fail(404, "not_found", `No account has the id ${url.searchParams.get("id")}.`, "Check the c: or si: id.");
  return found;
});

route("GET", "/v1/sheets", ({ me, url }) => {
  seedFor(me);
  const scope = url.searchParams.get("scope") ?? "mine";
  const folder = url.searchParams.get("folder");
  const q = (url.searchParams.get("q") ?? "").toLowerCase();
  const all = [...sheets.values()].filter(s => roleOf(s, me));
  let list;
  let folderList;
  if (scope === "trash") {
    list = all.filter(s => s.owner.uuid === me.uuid && s.trashed_at);
    folderList = [...folders.values()].filter(f => f.owner.uuid === me.uuid && f.trashed_at);
  } else if (scope === "shared") {
    list = all.filter(s => s.owner.uuid !== me.uuid && !s.trashed_at);
    folderList = [...folders.values()].filter(f => f.owner.uuid !== me.uuid && f.members.has(me.uuid) && !f.trashed_at);
  } else if (scope === "all" && !folder) {
    list = all.filter(s => !s.trashed_at && (s.owner.uuid === me.uuid ? (q || !s.folder_id) : s.members.has(me.uuid)));
    folderList = [...folders.values()].filter(f => !f.trashed_at && (f.owner.uuid === me.uuid ? (q || !f.parent_id) : f.members.has(me.uuid)));
  } else {
    list = all.filter(s => s.owner.uuid === me.uuid && !s.trashed_at && (folder ? s.folder_id === folder : !s.folder_id));
    folderList = [...folders.values()].filter(f => f.owner.uuid === me.uuid && !f.trashed_at && (folder ? f.parent_id === folder : !f.parent_id));
  }
  if (q) {
    list = list.filter(s => s.title.toLowerCase().includes(q));
    folderList = folderList.filter(f => f.name.toLowerCase().includes(q));
  }
  list.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  return { sheets: list.map(s => sheetJson(s, me)), folders: folderList.map(f => folderJson(f, me)), next_cursor: null };
});
route("POST", "/v1/sheets", ({ me, body }) => [201, sheetJson(makeSheet(me, body.title, body.folder_id ?? null), me, true)]);
route("POST", "/v1/sheets/import", ({ me, url, raw }) => {
  const filename = url.searchParams.get("filename") ?? "file.csv";
  const sheet = makeSheet(me, url.searchParams.get("title") || filename.replace(/\.[^.]+$/, ""), url.searchParams.get("folder_id") || null);
  fillTabFrom(sheet.main.wb.tabs[0], filename, raw, me);
  sheet.main.wb.tabs[0].name = filename.replace(/\.[^.]+$/, "").slice(0, 60) || "Tab 1";
  sheet.main.snapshots.set(0, sheet.main.wb.clone());
  return [201, sheetJson(sheet, me, true)];
});
route("GET", "/v1/sheets/([a-z0-9]+)", ({ me, m }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "viewer");
  return sheetJson(sheet, me, true);
});
route("PATCH", "/v1/sheets/([a-z0-9]+)", ({ me, m, body }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "editor");
  if (body.title !== undefined) sheet.title = String(body.title).trim() || sheet.title;
  if (body.folder_id !== undefined) sheet.folder_id = body.folder_id;
  sheet.updated_at = now();
  broadcast(sheet.main, "change", { version: sheet.main.version, by: me, at: now(), changes: [{ meta: true }] });
  return sheetJson(sheet, me, true);
});
route("DELETE", "/v1/sheets/([a-z0-9]+)", ({ me, m }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "owner");
  sheet.trashed_at = now();
  return [204, null];
});
route("POST", "/v1/sheets/([a-z0-9]+)/restore", ({ me, m }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "owner");
  sheet.trashed_at = null;
  return sheetJson(sheet, me);
});
route("DELETE", "/v1/sheets/([a-z0-9]+)/forever", ({ me, m, url }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "owner");
  if (!sheet.trashed_at) throw new Fail(409, "conflict", "Only a sheet in the trash can be deleted forever.", "Move it to the trash first.");
  if (url.searchParams.get("confirm") !== sheet.title) throw new Fail(422, "invalid_field", "The confirmation does not match the sheet's title.", "Send confirm= with the exact title.", { fields: { confirm: "Must equal the title." } });
  sheets.delete(sheet.id);
  return [204, null];
});
route("POST", "/v1/sheets/([a-z0-9]+)/transfer", ({ me, m, body }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "owner");
  const to = lookupAccount(body.to);
  if (!to) throw new Fail(422, "invalid_field", `No account has the id ${body.to}.`, "Check the c: or si: id.", { fields: { to: "No such account." } });
  return [201, { id: randomUUID(), kind: "sheet", item_id: sheet.id, title: sheet.title, from: me, to, status: "pending", created_at: now() }];
});
route("GET", "/v1/transfers", () => ({ incoming: [], outgoing: [] }));

route("POST", "/v1/folders", ({ me, body }) => {
  const folder = { id: `f${id12()}`, name: String(body.name || "New folder"), parent_id: body.parent_id ?? null, owner: me, members: new Map(), created_at: now(), trashed_at: null };
  folders.set(folder.id, folder);
  return [201, folderJson(folder, me)];
});
route("GET", "/v1/folders/(f[a-z0-9]+)", ({ me, m }) => {
  const folder = folders.get(m[1]);
  if (!folder) throw notFound("This folder");
  const path = [];
  for (let p = folder.parent_id; p; p = folders.get(p)?.parent_id ?? null) if (folders.get(p)) path.unshift(folderJson(folders.get(p), me));
  return {
    folder: folderJson(folder, me), path,
    sheets: [...sheets.values()].filter(s => s.folder_id === folder.id && !s.trashed_at).map(s => sheetJson(s, me)),
    folders: [...folders.values()].filter(f => f.parent_id === folder.id && !f.trashed_at).map(f => folderJson(f, me)),
  };
});
route("PATCH", "/v1/folders/(f[a-z0-9]+)", ({ me, m, body }) => {
  const folder = folders.get(m[1]);
  if (!folder) throw notFound("This folder");
  if (body.name !== undefined) folder.name = body.name;
  if (body.parent_id !== undefined) folder.parent_id = body.parent_id;
  return folderJson(folder, me);
});
route("DELETE", "/v1/folders/(f[a-z0-9]+)", ({ m }) => {
  const folder = folders.get(m[1]);
  if (!folder) throw notFound("This folder");
  folder.trashed_at = now();
  return [204, null];
});
route("POST", "/v1/folders/(f[a-z0-9]+)/restore", ({ me, m }) => {
  const folder = folders.get(m[1]);
  if (!folder) throw notFound("This folder");
  folder.trashed_at = null;
  return folderJson(folder, me);
});
route("DELETE", "/v1/folders/(f[a-z0-9]+)/forever", ({ m }) => {
  folders.delete(m[1]);
  return [204, null];
});

function sharingOf(item, me) {
  return {
    owner: item.owner,
    members: [...item.members.values()].map(x => ({ account: x.account, role: x.role, via: "direct" })),
    invites: item.invites ?? [],
    public: { ...(item.public ?? { enabled: false, access: "view" }), url: item.public?.enabled ? `http://127.0.0.1:8744/s/${item.id}` : undefined },
    me,
  };
}
const itemOf = (kind, id) => (kind === "sheets" ? sheets.get(id) : folders.get(id));
route("GET", "/v1/(sheets|folders)/([a-z0-9]+)/sharing", ({ me, m }) => {
  const item = itemOf(m[1], m[2]);
  if (!item) throw notFound("This item");
  const out = sharingOf(item, me);
  delete out.me;
  return out;
});
route("POST", "/v1/(sheets|folders)/([a-z0-9]+)/sharing", ({ me, m, body }) => {
  const item = itemOf(m[1], m[2]);
  if (!item) throw notFound("This item");
  const unknown = [];
  for (const who of body.invite ?? []) {
    if (String(who).includes("@")) {
      item.invites = [...(item.invites ?? []).filter(i => i.email !== who), { email: who, role: body.role, invited_by: me, created_at: now() }];
      continue;
    }
    const account = lookupAccount(who);
    if (!account) unknown.push(who);
    else if (account.uuid !== item.owner.uuid) item.members.set(account.uuid, { account, role: body.role });
  }
  if (unknown.length) throw new Fail(422, "invalid_field", `No account has the id ${unknown.join(", ")}.`, "Check the c: or si: ids.", { fields: { invite: unknown.join(", ") } });
  const out = sharingOf(item, me);
  delete out.me;
  return out;
});
route("PATCH", "/v1/(sheets|folders)/([a-z0-9]+)/sharing/([^/]+)", ({ me, m, body }) => {
  const item = itemOf(m[1], m[2]);
  const member = item?.members.get(decodeURIComponent(m[3]));
  if (!member) throw notFound("This member");
  member.role = body.role;
  const out = sharingOf(item, me);
  delete out.me;
  return out;
});
route("DELETE", "/v1/(sheets|folders)/([a-z0-9]+)/sharing/([^/]+)", ({ me, m }) => {
  const item = itemOf(m[1], m[2]);
  item?.members.delete(decodeURIComponent(m[3]));
  const out = sharingOf(item, me);
  delete out.me;
  return out;
});
route("DELETE", "/v1/(sheets|folders)/([a-z0-9]+)/invites/([^/]+)", ({ me, m }) => {
  const item = itemOf(m[1], m[2]);
  item.invites = (item.invites ?? []).filter(i => i.email !== decodeURIComponent(m[3]));
  const out = sharingOf(item, me);
  delete out.me;
  return out;
});
route("PUT", "/v1/sheets/([a-z0-9]+)/public", ({ m, body }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  sheet.public = { enabled: !!body.enabled, access: body.access ?? "view" };
  return { ...sheet.public, url: `http://127.0.0.1:8744/s/${sheet.id}` };
});

route("GET", `${LINE}/tabs`, ({ me, m }) => {
  const line = lineFor(m[1], m[2], me);
  return { version: line.version, tabs: line.wb.tabs.map((t, i) => t.json(i)) };
});
function valuesAnswer(line, tab, answer) {
    const r = parseRange(answer.range);
    const values = [];
    for (let row = r.r1; row <= Math.min(r.r2, r.r1 + 100_000); row++) {
      const line2 = [];
      for (let col = r.c1; col <= r.c2; col++) {
        const cell = tab.get(row, col);
        const v = cell ? (cell.f ? evaluate(line.wb, tab, cell.f) : cell.v) : { t: "empty" };
        line2.push(v.t === "empty" ? null : v.v);
      }
      values.push(line2);
    }
    return { version: line.version, range: answer.range, values };
}
route("GET", `${LINE}/tabs/([^/]+)/cells`, ({ me, m, url }) => {
  const line = lineFor(m[1], m[2], me);
  const tab = line.wb.tab(decodeURIComponent(m[3]));
  const answer = cellsAnswer(line, tab, url.searchParams.get("range"));
  if (url.searchParams.get("format") === "values") return valuesAnswer(line, tab, answer);
  return answer;
});
route("GET", `${LINE}/tabs/([^/]+)/cells/([A-Za-z]+[0-9]+)`, ({ me, m }) => {
  const line = lineFor(m[1], m[2], me);
  const tab = line.wb.tab(decodeURIComponent(m[3]));
  const r = parseRange(m[4]);
  const answer = cellsAnswer(line, tab, m[4]);
  const k = `${tab.id}:${key(r.r1, r.c1)}`;
  const history = line.log.filter(l => l.cells.has(k)).slice(-20).reverse().map(l => ({ version: l.version, by: l.by, at: l.at, before: null, after: null }));
  return { ...answer, cell: answer.cells[0] ?? null, history };
});
route("POST", `${LINE}/edits`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  const edits = body.edits ?? [];
  if (!edits.length) throw new Fail(400, "invalid_request", "The request has no edits.", "Send at least one edit.");
  return applyEdits(line, me, body);
});
route("GET", `${LINE}/events`, ({ me, m, res, url }) => {
  const line = lineFor(m[1], m[2], me);
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store", connection: "keep-alive" });
  const since = Number(url.searchParams.get("since") ?? line.version);
  for (const entry of line.log.filter(l => l.version > since)) res.write(`event: change\ndata: ${JSON.stringify({ version: entry.version, by: entry.by, at: entry.at, changes: entry.changes })}\n\n`);
  res.write(`event: presence\ndata: ${JSON.stringify({ viewers: [...line.viewers.values()] })}\n\n`);
  line.clients.add(res);
  res.on("close", () => line.clients.delete(res));
  return "stream";
});
route("POST", `${LINE}/presence`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me);
  line.viewers.set(body.client_id, { account: me, tab: body.tab, range: body.range, client_id: body.client_id, seen_at: now() });
  presence(line);
  return [204, null];
});

route("GET", `${LINE}/versions`, ({ me, m }) => ({ versions: versionsOf(lineFor(m[1], m[2], me)), next_cursor: null }));
route("POST", `${LINE}/versions/(\\d+)/name`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  line.named.set(Number(m[3]), String(body.name));
  return [204, null];
});
route("DELETE", `${LINE}/versions/(\\d+)/name`, ({ me, m }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  line.named.delete(Number(m[3]));
  return [204, null];
});
const atVersion = (line, v) => {
  const keys = [...line.snapshots.keys()].filter(k => k <= v).sort((a, b) => b - a);
  const wb = line.snapshots.get(keys[0] ?? 0) ?? line.wb;
  return { ...line, wb, version: v };
};
route("GET", `${LINE}/versions/(\\d+)/tabs`, ({ me, m }) => {
  const line = atVersion(lineFor(m[1], m[2], me), Number(m[3]));
  return { version: line.version, tabs: line.wb.tabs.map((t, i) => t.json(i)) };
});
route("GET", `${LINE}/versions/(\\d+)/tabs/([^/]+)/cells`, ({ me, m, url }) => {
  const line = atVersion(lineFor(m[1], m[2], me), Number(m[3]));
  const tab = line.wb.tab(decodeURIComponent(m[4]));
  const answer = cellsAnswer(line, tab, url.searchParams.get("range"));
  return url.searchParams.get("format") === "values" ? valuesAnswer(line, tab, answer) : answer;
});
route("GET", `${LINE}/compare`, ({ me, m, url }) => {
  const line = lineFor(m[1], m[2], me);
  const from = atVersion(line, Number(url.searchParams.get("from") ?? 0)).wb;
  const toParam = url.searchParams.get("to") ?? "head";
  const to = toParam === "head" ? line.wb : atVersion(line, Number(toParam)).wb;
  return diffWorkbooks(from, to, Number(url.searchParams.get("limit") ?? 500));
});
route("POST", `${LINE}/versions/(\\d+)/restore`, ({ me, m }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  const past = atVersion(line, Number(m[3])).wb.clone();
  line.wb.tabs = past.tabs;
  line.wb.styles = past.styles;
  const answer = finish(line, me, [{ tabs: true }, ...line.wb.tabs.map(t => ({ tab: t.id, reload: true }))], [], new Map(), null, "restore");
  return { version: answer.version };
});
route("POST", `${LINE}/versions/(\\d+)/undo-account`, ({ me, m }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  return { version: line.version, cells_changed: 0 };
});
route("POST", `${LINE}/versions/(\\d+)/new-sheet`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me);
  const sheet = makeSheet(me, body.title || `${line.sheet.title} (version ${m[3]})`);
  sheet.main.wb = atVersion(line, Number(m[3])).wb.clone();
  return [201, sheetJson(sheet, me, true)];
});

route("GET", "/v1/sheets/([a-z0-9]+)/copies", ({ me, m }) => {
  const sheet = sheets.get(m[1]);
  if (!sheet) throw notFound("This sheet");
  need(sheet, me, "viewer");
  return { copies: sheet.copies.map(c => ({ ...c, version: lines.get(`${sheet.id}:${c.id}`).version })) };
});
route("POST", `${LINE}/copies`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  const copy = { id: `c${id12()}`, sheet_id: line.sheet.id, name: String(body.name || "Copy"), parent_id: m[2] ?? null, forked_at: line.version, version: 0, created_by: me, created_at: now(), status: "open", merged_at: null, merged_by: null };
  line.sheet.copies.push(copy);
  newLine(line.sheet, copy, line.wb.clone()).base = line.wb.clone();
  broadcast(line, "copy", { copy });
  return [201, copy];
});
route("PATCH", "/v1/sheets/([a-z0-9]+)/copies/(c[a-z0-9]+)", ({ me, m, body }) => {
  const sheet = sheets.get(m[1]);
  need(sheet, me, "editor");
  const copy = sheet.copies.find(c => c.id === m[2]);
  if (!copy) throw notFound("This copy");
  copy.name = String(body.name ?? copy.name);
  return copy;
});
route("DELETE", "/v1/sheets/([a-z0-9]+)/copies/(c[a-z0-9]+)", ({ me, m }) => {
  const sheet = sheets.get(m[1]);
  need(sheet, me, "editor");
  const copy = sheet.copies.find(c => c.id === m[2]);
  if (!copy) throw notFound("This copy");
  copy.status = "deleted";
  broadcast(sheet.main, "copy", { copy });
  return [204, null];
});
route("GET", "/v1/sheets/([a-z0-9]+)/copies/(c[a-z0-9]+)/compare", ({ me, m, url }) => {
  const line = lineFor(m[1], m[2], me);
  const parent = line.copy.parent_id ? lines.get(`${line.sheet.id}:${line.copy.parent_id}`) : line.sheet.main;
  const diff = diffWorkbooks(line.base, line.wb, Number(url.searchParams.get("limit") ?? 500));
  const parentDiff = diffWorkbooks(line.base, parent.wb, 100_000);
  const conflicts = [];
  for (const cell of diff.cells) {
    const other = parentDiff.cells.find(c => c.tab === cell.tab && c.address === cell.address);
    if (other && JSON.stringify(other.after) !== JSON.stringify(cell.after)) conflicts.push({ tab: cell.tab, tab_name: line.wb.tabs.find(t => t.id === cell.tab)?.name ?? "", a: cell.address, base: cell.before, sheet: other.after, copy: cell.after, taken: "copy" });
  }
  return { diff, conflicts, parent_changed: parentDiff.cells_changed > 0 };
});
route("POST", "/v1/sheets/([a-z0-9]+)/copies/(c[a-z0-9]+)/merge", ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  const parent = line.copy.parent_id ? lines.get(`${line.sheet.id}:${line.copy.parent_id}`) : line.sheet.main;
  const diff = diffWorkbooks(line.base, line.wb, 1_000_000);
  const take = (body.resolutions ?? []).reduce((map, r) => map.set(`${r.tab}:${r.a}`, r.take), new Map());
  let taken = 0;
  const changed = new Map();
  const before = new Map();
  for (const cell of diff.cells) {
    if (body.tabs && !body.tabs.includes(cell.tab)) continue;
    if (body.range) {
      const r = parseRange(body.range.range);
      if (String(body.range.tab) !== String(cell.tab) || cell.row < r.r1 || cell.row > r.r2 || cell.col < r.c1 || cell.col > r.c2) continue;
    }
    if ((take.get(`${cell.tab}:${cell.address}`) ?? body.default ?? "copy") === "sheet") continue;
    const tab = parent.wb.tabs.find(t => t.id === cell.tab) ?? parent.wb.addTab(line.wb.tabs.find(t => t.id === cell.tab)?.name ?? "Copied");
    if (!body.dry_run) {
      const source = line.wb.tabs.find(t => t.id === cell.tab).cells.get(key(cell.row, cell.col));
      before.set(`${tab.id}:${key(cell.row, cell.col)}`, { tab: tab.id, k: key(cell.row, cell.col), cell: tab.cells.get(key(cell.row, cell.col)) ?? null });
      if (source) tab.cells.set(key(cell.row, cell.col), { ...source, by: me.uuid, at: now() });
      else tab.cells.delete(key(cell.row, cell.col));
      const list = changed.get(tab.id) ?? [];
      list.push({ r1: cell.row, c1: cell.col, r2: cell.row, c2: cell.col });
      changed.set(tab.id, list);
    }
    taken++;
  }
  let version = parent.version;
  if (!body.dry_run && taken) {
    const changes = [...changed].flatMap(([tab, rects]) => rects.map(r => ({ tab, range: rangeText(r) })));
    version = finish(parent, me, changes, [], changed, before, "merge").version;
    if (body.close) line.copy.status = "merged";
    line.copy.merged_at = now();
    line.copy.merged_by = me;
    line.base = parent.wb.clone();
  }
  return { version, cells_taken: taken, conflicts: [], tabs_added: 0, tabs_removed: 0, rows_added: 0, rows_removed: 0 };
});

route("GET", `${LINE}/comments`, ({ me, m, url }) => {
  const line = lineFor(m[1], m[2], me);
  const status = url.searchParams.get("status") ?? "open";
  return { threads: [...threads.values()].filter(t => t.line === line.id && (status === "all" || t.status === status)).map(threadJson) };
});
route("POST", `${LINE}/comments`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "commenter");
  const tab = line.wb.tab(body.tab);
  const r = body.range ? parseRange(body.range) : null;
  const quote = r && r.r2 - r.r1 < 50 && r.c2 - r.c1 < 50 ? (() => {
    const out = [];
    for (let row = r.r1; row <= r.r2 && out.length < 6; row++) for (let col = r.c1; col <= r.c2 && out.length < 6; col++) {
      const cell = tab.get(row, col);
      if (cell) out.push(cellJson(line, tab, row, col, cell).t);
    }
    return out.join(" | ");
  })() : "";
  const thread = { id: randomUUID(), line: line.id, sheet_id: line.sheet.id, copy_id: line.copy?.id ?? null, tab: tab.id, tab_name: tab.name, range: body.range ?? null, original_range: body.range ?? null, quote, detached: false, status: "open", resolved_by: null, resolved_at: null, comments: [{ id: randomUUID(), author: me, body: body.body, mentions: mentionsIn(body.body), reactions: [], created_at: now(), edited_at: null }], created_at: now() };
  threads.set(thread.id, thread);
  broadcast(line, "comment", { thread: threadJson(thread) });
  return [201, threadJson(thread)];
});
const threadOfComment = id => [...threads.values()].find(t => t.comments.some(c => c.id === id));
route("POST", "/v1/comments/([^/]+)/replies", ({ me, m, body }) => {
  const thread = threads.get(m[1]);
  if (!thread) throw notFound("This thread");
  const comment = { id: randomUUID(), author: me, body: body.body, mentions: mentionsIn(body.body), reactions: [], created_at: now(), edited_at: null };
  thread.comments.push(comment);
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return [201, comment];
});
route("PATCH", "/v1/comments/([^/]+)", ({ m, body }) => {
  const thread = threadOfComment(m[1]);
  if (!thread) throw notFound("This comment");
  const comment = thread.comments.find(c => c.id === m[1]);
  comment.body = body.body;
  comment.edited_at = now();
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return comment;
});
route("DELETE", "/v1/comments/([^/]+)", ({ m }) => {
  const thread = threadOfComment(m[1]);
  if (!thread) throw notFound("This comment");
  thread.comments.find(c => c.id === m[1]).deleted = true;
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return [204, null];
});
route("POST", "/v1/comments/([^/]+)/(resolve|reopen)", ({ me, m }) => {
  const thread = threads.get(m[1]);
  if (!thread) throw notFound("This thread");
  thread.status = m[2] === "resolve" ? "resolved" : "open";
  thread.resolved_by = m[2] === "resolve" ? me : null;
  thread.resolved_at = m[2] === "resolve" ? now() : null;
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return threadJson(thread);
});
route("POST", "/v1/comments/([^/]+)/reactions", ({ me, m, body }) => {
  const thread = threadOfComment(m[1]);
  if (!thread) throw notFound("This comment");
  const comment = thread.comments.find(c => c.id === m[1]);
  const reaction = comment.reactions.find(r => r.emoji === body.emoji) ?? (comment.reactions.push({ emoji: body.emoji, accounts: [] }), comment.reactions[comment.reactions.length - 1]);
  if (!reaction.accounts.some(a => a.uuid === me.uuid)) reaction.accounts.push(me);
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return [204, null];
});
route("DELETE", "/v1/comments/([^/]+)/reactions/([^/]+)", ({ me, m }) => {
  const thread = threadOfComment(m[1]);
  if (!thread) throw notFound("This comment");
  const comment = thread.comments.find(c => c.id === m[1]);
  const emoji = decodeURIComponent(m[2]);
  for (const r of comment.reactions) if (r.emoji === emoji) r.accounts = r.accounts.filter(a => a.uuid !== me.uuid);
  comment.reactions = comment.reactions.filter(r => r.accounts.length);
  broadcast(lines.get(thread.line), "comment", { thread: threadJson(thread) });
  return [204, null];
});

route("POST", `${LINE}/run`, ({ me, m, body }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  if (body.base_version !== line.version) throw new Fail(409, "conflict", "The sheet changed while the command ran.", "Run the command again.");
  const { columns, rows } = body;
  if (!Array.isArray(columns) || !Array.isArray(rows) || !body.into) throw new Fail(422, "invalid_result", "Send a result table and destination.", "Run JavaScript in the browser first.");
  const target = body.into.new_tab ? line.wb.addTab(body.into.new_tab) : line.wb.tab(String(body.into.tab));
  const at = parseRange(body.into.at || "A1");
  if (body.into.replace) target.cells.clear();
  [columns, ...rows].forEach((row, r) => row.forEach((value, c) => {
    if (value !== null) target.cells.set(key(at.r1 + r, at.c1 + c), { v: typeof value === "number" ? { t: "number", v: value } : typeof value === "boolean" ? { t: "bool", v: value } : { t: "text", v: String(value) }, by: me.uuid, at: now() });
  }));
  target.code = { language: "javascript", code: body.code, inputs: body.inputs, run_by: me, run_at: now() };
  const version = finish(line, me, [{ tabs: true }, { tab: target.id, reload: true }], [], new Map(), null, "code").version;
  return { columns, rows: rows.slice(0, 1000), row_count: rows.length, truncated: rows.length > 1000, stdout: "", stderr: "", duration_ms: 0, version, tab: target.json(line.wb.tabs.indexOf(target)) };
});
route("POST", `${LINE}/tabs/import`, ({ me, m, url, raw }) => {
  const line = lineFor(m[1], m[2], me, "editor");
  const filename = url.searchParams.get("filename") ?? "file.csv";
  const mode = url.searchParams.get("mode") ?? "new_tab";
  let tab;
  if (mode === "replace") {
    tab = line.wb.tab(url.searchParams.get("tab"));
    tab.cells.clear();
  } else {
    let name = filename.replace(/\.[^.]+$/, "").slice(0, 60) || "Imported";
    while (line.wb.tabs.some(t => t.name.toLowerCase() === name.toLowerCase())) name += " 2";
    tab = line.wb.addTab(name);
  }
  fillTabFrom(tab, filename, raw, me);
  const answer = finish(line, me, [{ tabs: true }, { tab: tab.id, reload: true }], [], new Map(), null, "import");
  return { version: answer.version, tabs: [tab.json(line.wb.tabs.indexOf(tab))] };
});
route("GET", `${LINE}/download`, ({ me, m, url, res }) => {
  const line = lineFor(m[1], m[2], me);
  const format = url.searchParams.get("format") ?? "csv";
  const tab = url.searchParams.get("tab") ? line.wb.tab(url.searchParams.get("tab")) : line.wb.tabs[0];
  const used = tab.used();
  const rows = [];
  for (let row = 0; row <= Math.min(used?.r2 ?? -1, 100_000); row++) {
    const out = [];
    for (let col = 0; col <= (used?.c2 ?? -1); col++) {
      const cell = tab.get(row, col);
      out.push(cell ? cellJson(line, tab, row, col, cell).t : "");
    }
    rows.push(out);
  }
  const quote = v => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  const body = format === "json" ? JSON.stringify({ tabs: [{ name: tab.name, rows }] }) : rows.map(r => r.map(format === "tsv" ? x => x : quote).join(format === "tsv" ? "\t" : ",")).join("\n");
  res.writeHead(200, { "content-type": format === "json" ? "application/json" : "text/csv", "content-disposition": `attachment; filename="${line.sheet.title}.${format}"` });
  res.end(body);
  return "stream";
});
route("GET", "/v1/search", ({ me, url }) => {
  const q = (url.searchParams.get("q") ?? "").toLowerCase();
  const results = [];
  for (const sheet of sheets.values()) {
    if (!roleOf(sheet, me) || sheet.trashed_at) continue;
    const matches = [];
    for (const tab of sheet.main.wb.tabs) for (const [k, cell] of tab.cells) {
      const text = cell.v?.t === "text" ? cell.v.v : "";
      if (q && text.toLowerCase().includes(q) && matches.length < 5) matches.push({ tab: tab.id, tab_name: tab.name, a: a1(keyRow(k), keyCol(k)), text });
    }
    if (sheet.title.toLowerCase().includes(q) || matches.length) results.push({ sheet: sheetJson(sheet, me), matches });
  }
  return { results };
});

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const me = accountFrom(req);
  try {
    const found = routes.map(r => ({ r, m: req.method === r.method ? r.pattern.exec(url.pathname) : null })).find(x => x.m);
    if (!found) throw new Fail(404, "not_found", `${req.method} ${url.pathname} is not a route of the mock service.`, "Check docs/API.md.");
    const isPublicRead = req.method === "GET";
    if (!me && !(isPublicRead && url.pathname.startsWith("/v1/sheets/")) && url.pathname !== "/healthz") throw new Fail(401, "signed_out", "This request carries no access token.", "Sign in again.");
    const raw = req.method === "GET" ? Buffer.alloc(0) : await readBody(req);
    let body = {};
    if (raw.length && (req.headers["content-type"] ?? "").includes("json")) body = JSON.parse(raw.toString("utf8"));
    const out = await found.r.handler({ me, m: found.m, url, body, raw, req, res });
    if (out === "stream") return;
    if (Array.isArray(out)) send(res, out[0], out[1]);
    else send(res, 200, out);
  } catch (error) {
    if (error instanceof Fail) send(res, error.status, { error: { code: error.code, message: error.message, hint: error.hint, ...(error.details ? { details: error.details } : {}) } });
    else {
      console.error(error);
      send(res, 500, { error: { code: "internal", message: `The mock service failed: ${error.message}`, hint: "Look at its output." } });
    }
  }
});
server.listen(PORT, "127.0.0.1", () => console.log(`calc mock service on http://127.0.0.1:${PORT}`));
