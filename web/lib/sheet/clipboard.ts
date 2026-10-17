/**
 * The clipboard in the formats Excel and Google Sheets read and write: tab-separated text (TSV, with Excel's quoting
 * for fields holding tabs, line breaks or quotes) and an HTML table (with each cell's look inline, so bold, colours,
 * fills, alignment and wrapping come across). Parsing works without a DOM (it runs in unit tests too): it reads the
 * first table, its rows and cells (colspan and rowspan included), the text with line breaks, Excel's class styles
 * from its <style> block, and Google Sheets' R1C1 formulas from `data-sheets-formula`.
 */
import type { HAlign, Style, StylePatch, VAlign } from "../calc/types";
import { r1c1ToA1 } from "./formula";

/* ---- TSV --------------------------------------------------------------------------------------------------------- */

function quoteField(value: string): string {
  return /[\t\n\r"]/.test(value) ? `"${value.replace(/"/g, '""')}"` : value;
}

/** Rows of text as TSV, the way Excel writes it. */
export function toTSV(rows: string[][]): string {
  return rows.map(row => row.map(quoteField).join("\t")).join("\n");
}

/** TSV as Excel and Google Sheets write it (quoted fields may hold tabs and line breaks). A final line break ends the last row. */
export function parseTSV(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let i = 0;
  let quoted = false;
  let fieldStart = true;
  const endField = () => {
    row.push(field);
    field = "";
    fieldStart = true;
  };
  const endRow = () => {
    endField();
    rows.push(row);
    row = [];
  };
  while (i < text.length) {
    const char = text[i]!;
    if (quoted) {
      if (char === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i += 2;
          continue;
        }
        quoted = false;
        i++;
        continue;
      }
      field += char;
      i++;
      continue;
    }
    if (char === '"' && fieldStart) {
      quoted = true;
      fieldStart = false;
      i++;
      continue;
    }
    if (char === "\t") {
      endField();
      i++;
      continue;
    }
    if (char === "\r" || char === "\n") {
      endRow();
      i += char === "\r" && text[i + 1] === "\n" ? 2 : 1;
      continue;
    }
    field += char;
    fieldStart = false;
    i++;
  }
  if (field !== "" || !fieldStart || row.length > 0) endRow();
  // Rows of unequal length are padded so the block is a rectangle.
  const width = rows.reduce((max, r) => Math.max(max, r.length), 0);
  for (const r of rows) while (r.length < width) r.push("");
  return rows;
}

/* ---- HTML out ---------------------------------------------------------------------------------------------------- */

const escapeHtml = (text: string) => text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const BORDER_CSS: Record<string, string> = { thin: "1px solid", medium: "2px solid", thick: "3px solid", dashed: "1px dashed", dotted: "1px dotted", double: "3px double" };

/** A style as inline CSS for a <td>. */
export function styleToCss(style: Style | undefined | null): string {
  if (!style) return "";
  const css: string[] = [];
  if (style.bold) css.push("font-weight:bold");
  if (style.italic) css.push("font-style:italic");
  const decorations = [style.underline ? "underline" : "", style.strike ? "line-through" : ""].filter(Boolean);
  if (decorations.length) css.push(`text-decoration:${decorations.join(" ")}`);
  if (style.color) css.push(`color:${style.color}`);
  if (style.fill) css.push(`background-color:${style.fill}`);
  if (style.font_size) css.push(`font-size:${style.font_size}pt`);
  if (style.font) css.push(`font-family:${style.font.replace(/[;"<>]/g, "")}`);
  if (style.h_align && style.h_align !== "general") css.push(`text-align:${style.h_align}`);
  if (style.v_align) css.push(`vertical-align:${style.v_align === "middle" ? "middle" : style.v_align}`);
  if (style.wrap) css.push("white-space:normal");
  for (const side of ["top", "bottom", "left", "right"] as const) {
    const border = style[`border_${side}`];
    if (border && border.style !== "none") css.push(`border-${side}:${BORDER_CSS[border.style] ?? "1px solid"} ${border.color ?? "#000000"}`);
  }
  if (style.num_fmt) css.push(`mso-number-format:"${style.num_fmt.replace(/"/g, '\\"')}"`);
  return css.join(";");
}

export interface HtmlCell {
  text: string;
  style?: Style | null;
  /** The formula, kept for pasting back into Calc. */
  formula?: string;
}

/** A block of cells as an HTML table that Excel, Google Sheets and Calc read back with their look. */
export function toHTML(rows: HtmlCell[][]): string {
  const body = rows
    .map(row => `<tr>${row.map(cell => {
      const css = styleToCss(cell.style);
      const formula = cell.formula ? ` data-calc-formula="${escapeHtml(cell.formula)}"` : "";
      return `<td${css ? ` style="${escapeHtml(css)}"` : ""}${formula}>${escapeHtml(cell.text).replace(/\r?\n/g, "<br>")}</td>`;
    }).join("")}</tr>`)
    .join("");
  return `<meta charset="utf-8"><table data-calc="1" style="border-collapse:collapse"><tbody>${body}</tbody></table>`;
}

/* ---- HTML in ----------------------------------------------------------------------------------------------------- */

export interface ParsedCell {
  text: string;
  /** The look the source gave it (only what it set). */
  style: StylePatch | null;
  /** A formula the source carried (Calc's own, or Google Sheets' converted from R1C1 at its place in the block). */
  formula: string | null;
}

export interface ParsedTable {
  rows: ParsedCell[][];
  /** Merged areas, relative to the block's top-left cell. */
  merges: Array<{ r1: number; c1: number; r2: number; c2: number }>;
  /** True when the block came from Calc itself (formulas are A1 relative to the source). */
  fromCalc: boolean;
  /** True when it came from Google Sheets. */
  fromGoogle: boolean;
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ", ndash: "–", mdash: "—", hellip: "…", rsquo: "’", lsquo: "‘", rdquo: "”", ldquo: "“", euro: "€", pound: "£", yen: "¥", copy: "©", reg: "®", deg: "°", times: "×", divide: "÷" };

export function decodeEntities(text: string): string {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (whole, name: string) => {
    if (name[0] === "#") {
      const code = name[1] === "x" || name[1] === "X" ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code < 0x110000 ? String.fromCodePoint(code) : whole;
    }
    return ENTITIES[name.toLowerCase()] ?? whole;
  });
}

function attributes(tag: string): Record<string, string> {
  const out: Record<string, string> = {};
  const pattern = /([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*(?:=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g;
  const inner = tag.replace(/^<\s*[a-zA-Z0-9]+/, "").replace(/\/?>$/, "");
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(inner))) out[match[1]!.toLowerCase()] = decodeEntities(match[2] ?? match[3] ?? match[4] ?? "");
  return out;
}

function parseCss(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of text.split(";")) {
    const at = part.indexOf(":");
    if (at < 0) continue;
    const name = part.slice(0, at).trim().toLowerCase();
    const value = part.slice(at + 1).trim();
    if (name) out[name] = value;
  }
  return out;
}

/** Excel's <style> rules: `.xl65 {mso-number-format:"0\.00"; font-weight:700;}` by class name. */
function classRules(html: string): Map<string, Record<string, string>> {
  const rules = new Map<string, Record<string, string>>();
  const styleBlocks = html.match(/<style[^>]*>([\s\S]*?)<\/style>/gi) ?? [];
  for (const block of styleBlocks) {
    const css = block.replace(/<\/?style[^>]*>/gi, "").replace(/<!--|-->/g, "").replace(/\/\*[\s\S]*?\*\//g, "");
    const pattern = /([^{}]+)\{([^}]*)\}/g;
    let match: RegExpExecArray | null;
    while ((match = pattern.exec(css))) {
      const declarations = parseCss(match[2]!);
      for (const selector of match[1]!.split(",")) {
        const name = /\.([A-Za-z0-9_-]+)\s*$/.exec(selector.trim())?.[1];
        if (name) rules.set(name, { ...(rules.get(name) ?? {}), ...declarations });
      }
    }
  }
  return rules;
}

function cssColor(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const v = value.trim().toLowerCase();
  if (v === "transparent" || v === "inherit" || v === "initial" || v === "none" || v === "windowtext" || v === "auto") return undefined;
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/.exec(v);
  if (hex) {
    const h = hex[1]!;
    return `#${(h.length === 3 ? h.split("").map(c => c + c).join("") : h).toUpperCase()}`;
  }
  const rgb = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)(?:[\s,/]+([\d.]+%?))?\s*\)$/.exec(v);
  if (rgb) {
    if (rgb[4] !== undefined && parseFloat(rgb[4]) === 0) return undefined;
    return `#${[rgb[1], rgb[2], rgb[3]].map(n => Math.min(255, Number(n)).toString(16).padStart(2, "0")).join("").toUpperCase()}`;
  }
  const named: Record<string, string> = { black: "#000000", white: "#FFFFFF", red: "#FF0000", green: "#008000", blue: "#0000FF", yellow: "#FFFF00", gray: "#808080", grey: "#808080", orange: "#FFA500", purple: "#800080" };
  return named[v];
}

function styleFromCss(css: Record<string, string>): StylePatch | null {
  const patch: StylePatch = {};
  const weight = css["font-weight"];
  if (weight && (weight === "bold" || weight === "bolder" || Number(weight) >= 600)) patch.bold = true;
  if (css["font-style"] === "italic") patch.italic = true;
  const decoration = `${css["text-decoration"] ?? ""} ${css["text-decoration-line"] ?? ""}`;
  if (/underline/.test(decoration) || (css["text-underline-style"] && css["text-underline-style"] !== "none")) patch.underline = true;
  if (/line-through/.test(decoration)) patch.strike = true;
  const color = cssColor(css.color);
  if (color && color !== "#000000") patch.color = color;
  const fill = cssColor(css["background-color"] ?? css.background?.split(/\s+/)[0]);
  if (fill && fill !== "#FFFFFF") patch.fill = fill;
  const align = css["text-align"];
  if (align === "left" || align === "center" || align === "right") patch.h_align = align as HAlign;
  const vertical = css["vertical-align"];
  if (vertical === "top" || vertical === "middle" || vertical === "bottom") patch.v_align = vertical as VAlign;
  if (css["white-space"] === "normal" || css["white-space"] === "pre-wrap" || css["overflow-wrap"] === "break-word") patch.wrap = true;
  const format = css["mso-number-format"];
  if (format) {
    const code = format.replace(/^["']|["']$/g, "").replace(/\\(.)/g, "$1");
    if (code && code !== "General" && code !== "@") patch.num_fmt = code;
  }
  return Object.keys(patch).length ? patch : null;
}

/** The text inside a cell's HTML: tags dropped, <br> and block ends as line breaks, entities decoded. */
function cellText(html: string): string {
  const text = html
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|li)>\s*/gi, "\n")
    .replace(/<[^>]+>/g, "");
  return decodeEntities(text)
    .replace(/ /g, " ")
    .replace(/[ \t\r]*\n[ \t\r]*/g, "\n")
    .replace(/^\n+|\n+$/g, "")
    .replace(/[ \t\r]+/g, match => (match.includes("\t") ? match : " "));
}

/**
 * Reads the first table of clipboard HTML (Excel's, Google Sheets', Numbers', a web page's, or Calc's own). Null when
 * there is no table.
 */
export function parseHTMLTable(html: string): ParsedTable | null {
  const start = html.search(/<table[\s>]/i);
  if (start < 0) return null;
  const end = html.toLowerCase().indexOf("</table>", start);
  const table = html.slice(start, end < 0 ? undefined : end);
  const rules = classRules(html);
  const fromCalc = /<table[^>]*data-calc=/i.test(table);
  const fromGoogle = /google-sheets-html-origin|data-sheets-/i.test(html);

  const rows: ParsedCell[][] = [];
  const merges: ParsedTable["merges"] = [];
  // Cells taken by a rowspan from a row above: occupied[row] = set of columns.
  const occupied = new Map<number, Set<number>>();
  const isTaken = (r: number, c: number) => occupied.get(r)?.has(c) ?? false;
  const take = (r: number, c: number) => {
    let set = occupied.get(r);
    if (!set) occupied.set(r, (set = new Set()));
    set.add(c);
  };

  const rowPattern = /<tr\b[^>]*>([\s\S]*?)(?=<tr\b|<\/tbody>|<\/thead>|<\/tfoot>|$)/gi;
  let rowMatch: RegExpExecArray | null;
  let r = 0;
  while ((rowMatch = rowPattern.exec(table))) {
    const rowHtml = rowMatch[1]!.replace(/<\/tr>[\s\S]*$/i, "");
    const rowTag = /^<tr\b[^>]*>/i.exec(rowMatch[0])?.[0] ?? "<tr>";
    const rowCss = parseCss(attributes(rowTag).style ?? "");
    const row: ParsedCell[] = rows[r] ?? [];
    rows[r] = row;
    const cellPattern = /<(td|th)\b([^>]*)>([\s\S]*?)(?=<td\b|<th\b|$)/gi;
    let cellMatch: RegExpExecArray | null;
    let c = 0;
    while ((cellMatch = cellPattern.exec(rowHtml))) {
      while (isTaken(r, c)) c++;
      const attrs = attributes(`<td${cellMatch[2]}>`);
      const inner = cellMatch[3]!.replace(/<\/t[dh]>[\s\S]*$/i, "");
      const css = { ...rowCss };
      for (const name of (attrs.class ?? "").split(/\s+/)) Object.assign(css, rules.get(name) ?? {});
      Object.assign(css, parseCss(attrs.style ?? ""));
      let style = styleFromCss(css);
      if (cellMatch[1]!.toLowerCase() === "th" && !css["font-weight"]) style = { ...(style ?? {}), bold: true };
      // Bold or italic set by inline elements around the whole text (<b>, <strong>, <i>, <em>).
      if (/^\s*<(b|strong)\b/i.test(inner)) style = { ...(style ?? {}), bold: true };
      if (/^\s*<(i|em)\b/i.test(inner)) style = { ...(style ?? {}), italic: true };
      let formula: string | null = null;
      if (attrs["data-calc-formula"]) formula = attrs["data-calc-formula"];
      else if (attrs["data-sheets-formula"]) formula = r1c1ToA1(attrs["data-sheets-formula"], r, c);
      const colspan = Math.max(1, Math.min(1000, Number(attrs.colspan) || 1));
      const rowspan = Math.max(1, Math.min(10000, Number(attrs.rowspan) || 1));
      row[c] = { text: cellText(inner), style, formula };
      if (colspan > 1 || rowspan > 1) merges.push({ r1: r, c1: c, r2: r + rowspan - 1, c2: c + colspan - 1 });
      for (let dr = 0; dr < rowspan; dr++) {
        for (let dc = 0; dc < colspan; dc++) {
          if (dr === 0 && dc === 0) continue;
          take(r + dr, c + dc);
          if (dr === 0) row[c + dc] = { text: "", style: null, formula: null };
        }
      }
      c += colspan;
    }
    r++;
  }
  // Fill the gaps (cells covered by rowspans, short rows) so the block is a rectangle.
  const width = rows.reduce((max, row) => Math.max(max, row.length), 0);
  for (const row of rows) for (let c = 0; c < width; c++) if (!row[c]) row[c] = { text: "", style: null, formula: null };
  while (rows.length && rows[rows.length - 1]!.every(cell => cell.text === "" && !cell.formula && !cell.style)) rows.pop();
  if (!rows.length) return null;
  return { rows, merges, fromCalc, fromGoogle };
}
