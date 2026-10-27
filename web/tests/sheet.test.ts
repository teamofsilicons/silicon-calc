import { test } from "node:test";
import assert from "node:assert/strict";
import { cellName, parseCell, parseRange, MAX_ROWS, MAX_COLS } from "../lib/sheet/address";
import { AxisSizes } from "../lib/sheet/geometry";
import { parseTSV, toTSV, parseHTMLTable } from "../lib/sheet/clipboard";
import { shiftFormula, references, functionAt, r1c1ToA1 } from "../lib/sheet/formula";
import { getValues } from "../lib/calc/api";
import { chartData, chartExtent, scatterData } from "../lib/sheet/chart-data";
import { commandTable } from "../lib/sheet/browser-code";
import type { CellsAnswer, Sheet, Tab } from "../lib/calc/types";
import { SheetController } from "../lib/sheet/controller";
import { SheetSession } from "../lib/sheet/session";
import { cellSelection } from "../lib/sheet/selection";

const testTabs: Tab[] = [1, 2].map(id => ({ id, name: `Tab ${id}`, index: id - 1, rows: 1000, cols: 26, used: "A1", frozen_rows: 0, frozen_cols: 0, color: null, code: null }));
function sheetSession(options: { readOnly?: boolean; atVersion?: number } = {}) {
  const account = { uuid: "test-owner", id: "c:test", kind: "carbon" as const, display_name: "Test", pfp_url: null };
  const sheet: Sheet = { id: "sheet1234567", title: "Test", owner: account, folder_id: null, role: "owner", public: { enabled: false, access: "view" }, version: 0, created_at: "", created_by: account, updated_at: "", updated_by: account, trashed_at: null, copies: 0 };
  return new SheetSession({ line: { sheet: sheet.id, copy: null }, sheet, tabs: testTabs, version: 0, me: null, ...options });
}
function cellsAnswer(tab: number, text = "cached"): CellsAnswer {
  return { version: 0, tab: testTabs[tab - 1]!, range: "A1:AF128", cells: [{ a: "A1", r: 0, c: 0, v: { t: "text", v: text }, t: text }], styles: {}, accounts: {}, col_widths: {}, row_heights: {}, hidden_rows: [], hidden_cols: [], merges: [], frozen_rows: 0, frozen_cols: 0, charts: [], protections: [] };
}
const jsonResponse = (body: unknown) => new Response(JSON.stringify(body), { headers: { "Content-Type": "application/json" } });
const settled = () => new Promise<void>(resolve => setImmediate(resolve));

test("switching tabs releases every old read slot and ignores late replies without dropping cached cells", async () => {
  const previous = globalThis.fetch;
  const requests: Array<{ url: string; signal: AbortSignal; resolve: (response: Response) => void }> = [];
  globalThis.fetch = (input, init) => new Promise(resolve => { requests.push({ url: String(input), signal: init!.signal!, resolve }); });
  const session = sheetSession();
  const visible = { r1: 0, c1: 0, r2: 10, c2: 5 };
  try {
    session.view(1, { ...visible, r2: 767 }, []);
    await settled();
    assert.equal(requests.length, 6, "all six request slots are occupied by slow tab 1 reads");
    session.view(2, visible, []);
    await settled();
    assert.equal(requests.length, 7, "tab 2 starts without waiting for any tab 1 response");
    assert.ok(requests.slice(0, 6).every(request => request.signal.aborted));
    assert.match(requests[6]!.url, /\/tabs\/2\/cells/);
    requests[6]!.resolve(jsonResponse(cellsAnswer(2)));
    await settled();
    assert.equal(session.tab(2)?.get(0, 0)?.t, "cached");

    session.view(1, visible, []);
    await settled();
    assert.equal(requests.length, 8);
    for (const request of requests.slice(0, 6)) request.resolve(jsonResponse(cellsAnswer(1, "obsolete")));
    await settled();
    assert.equal(session.tab(1)?.get(0, 0), undefined, "aborted responses cannot overwrite a revisited tab");
    session.view(1, visible, []);
    await settled();
    assert.equal(requests.length, 8, "late cleanup cannot discard the replacement read");
    requests[7]!.resolve(jsonResponse(cellsAnswer(1, "current")));
    await settled();
    session.view(2, visible, []);
    await settled();
    assert.equal(requests.length, 8, "revisiting a cached viewport does not fetch again");
    assert.equal(session.tab(2)?.get(0, 0)?.t, "cached");
    await session.reload(1);
    await settled();
    assert.equal(requests.length, 8, "inactive stale blocks do not compete with the visible tab");
    session.view(1, visible, []);
    await settled();
    assert.equal(requests.length, 9, "stale cached data reloads when its tab becomes visible");
    requests[8]!.resolve(jsonResponse(cellsAnswer(1, "refreshed")));
    await settled();
  } finally { session.close(); globalThis.fetch = previous; }
});

test("cached tabs restore independent selections immediately, including read-only historical sheets", () => {
  for (const options of [{}, { readOnly: true }, { atVersion: 3 }]) {
    const session = sheetSession(options);
    const controller = new SheetController(session);
    session.seed(cellsAnswer(1, "first")); session.seed(cellsAnswer(2, "second"));
    const reveals: number[][] = [];
    controller.reveal = (row, col) => reveals.push([row, col]);
    controller.setSelection({ ...cellSelection(7, 3), range: { r1: 7, c1: 3, r2: 10, c2: 5 } }, false);
    const first = controller.selection;
    controller.openTab(2);
    assert.equal(controller.data?.get(0, 0)?.t, "second");
    controller.setSelection(cellSelection(20, 8), false);
    controller.openTab(1);
    assert.deepEqual(controller.selection, first);
    assert.equal(controller.data?.get(0, 0)?.t, "first");
    controller.openTab(2);
    assert.deepEqual(controller.selection, cellSelection(20, 8));
    assert.deepEqual(reveals, [], "switching does not force either viewport back to A1");
    controller.openTab(999);
    assert.equal(controller.activeTab, 2);
    session.close();
  }
});

test("tab switching commits a draft without waiting for its save or losing its pending input", async () => {
  const previous = globalThis.fetch;
  let complete: ((response: Response) => void) | undefined;
  let posted: { edits: Array<{ tab: number; cells: Array<{ a: string; input: string }> }> } | undefined;
  globalThis.fetch = (_input, init) => { posted = JSON.parse(String(init!.body)); return new Promise(resolve => { complete = resolve; }); };
  const session = sheetSession();
  const controller = new SheetController(session);
  try {
    controller.setSelection(cellSelection(4, 2), false);
    controller.startEdit({ text: "unsaved draft" });
    controller.openTab(2);
    assert.equal(controller.activeTab, 2);
    assert.equal(controller.editor.open, false);
    assert.equal(session.pendingEdits, 1);
    assert.equal(session.tab(1)?.pendingAt(4, 2)?.input, "unsaved draft");
    controller.openTab(1);
    assert.deepEqual(controller.selection.active, { row: 4, col: 2 });
    assert.equal(controller.activeInput(), "unsaved draft");
    await settled();
    assert.deepEqual(posted?.edits, [{ type: "set", tab: 1, cells: [{ a: "C5", input: "unsaved draft" }] }]);
    complete!(jsonResponse({ version: 0, changes: [], overwrote: [], skipped: 0 }));
    await settled();
    assert.equal(session.pendingEdits, 0);
  } finally { session.close(); globalThis.fetch = previous; }
});

test("formula pointing across tabs preserves the draft's source cell and the target tab's selection", async () => {
  const session = sheetSession();
  const controller = new SheetController(session);
  const edits: unknown[] = [];
  const presence: Array<{ tab: number; range: string }> = [];
  session.setPresence = (tab, range) => { presence.push({ tab, range }); };
  session.edit = async changes => { edits.push(changes); return { answer: null, error: null }; };
  controller.openTab(2);
  controller.setSelection(cellSelection(14, 5), false);
  controller.openTab(1);
  controller.setSelection(cellSelection(6, 2), false);
  controller.startEdit({ text: "=" });
  controller.openTab(2);
  assert.equal(controller.editor.open, true);
  assert.equal(controller.editor.tab, 1);
  assert.deepEqual(controller.selection.active, { row: 6, col: 2 });
  controller.point({ r1: 3, c1: 3, r2: 3, c2: 3 });
  assert.equal(controller.editor.text, "='Tab 2'!D4");
  controller.commit("none");
  assert.equal(controller.activeTab, 1);
  assert.deepEqual(controller.selection.active, { row: 6, col: 2 });
  assert.deepEqual(presence.at(-1), { tab: 1, range: "C7" });
  assert.deepEqual(edits, [[{ type: "set", tab: 1, cells: [{ a: "C7", input: "='Tab 2'!D4" }] }]]);
  controller.openTab(2);
  assert.deepEqual(controller.selection.active, { row: 14, col: 5 });
  controller.openTab(1);
  controller.startEdit({ text: "=" });
  controller.openTab(2);
  controller.cancelEdit();
  assert.equal(controller.activeTab, 1);
  assert.deepEqual(controller.selection.active, { row: 6, col: 2 });
  assert.deepEqual(presence.at(-1), { tab: 1, range: "C7" });
  assert.equal(edits.length, 1, "cancel never commits the pointing draft");
  controller.setSelection({ ...cellSelection(6, 2), range: { r1: 6, c1: 2, r2: 7, c2: 2 } }, false);
  controller.startEdit({ text: "=" });
  controller.openTab(2);
  controller.point({ r1: 3, c1: 3, r2: 3, c2: 3 });
  controller.commit("none", true);
  assert.deepEqual(presence.at(-1), { tab: 1, range: "C7:C8" });
  assert.deepEqual(edits[1], [{ type: "set", tab: 1, cells: [{ a: "C7", input: "='Tab 2'!D4" }, { a: "C8", input: "='Tab 2'!D5" }] }]);
  session.close();
});

test("deleting the active tab restores the surviving tab's saved selection and presence", async () => {
  const previous = globalThis.fetch;
  globalThis.fetch = async input => String(input).endsWith("/tabs")
    ? jsonResponse({ version: 0, tabs: [testTabs[0]] })
    : jsonResponse({ version: 0, changes: [], overwrote: [], skipped: 0 });
  const session = sheetSession();
  const controller = new SheetController(session);
  const presence: Array<{ tab: number; range: string }> = [];
  session.setPresence = (tab, range) => { presence.push({ tab, range }); };
  try {
    controller.setSelection(cellSelection(119, 25), false);
    controller.openTab(2);
    controller.setSelection(cellSelection(7, 1), false);
    controller.deleteTab(2);
    await settled();
    assert.equal(session.tab(2), null);
    assert.equal(controller.activeTab, 1);
    assert.deepEqual(controller.selection.active, { row: 119, col: 25 });
    assert.deepEqual(presence.at(-1), { tab: 1, range: "Z120" });
  } finally { session.close(); globalThis.fetch = previous; }
});

test("browser command results retain literal values and reject malformed or oversized tables", () => {
  assert.deepEqual(commandTable([{ name: "=1+1", amount: 4 }, { name: "Total", amount: null }]), { columns: ["name", "amount"], rows: [["=1+1", 4], ["Total", null]] });
  assert.throws(() => commandTable({ columns: ["a"], rows: [[1, 2]] }), /one value/);
  assert.throws(() => commandTable({ columns: ["a"], rows: [[Infinity]] }), /finite numbers/);
  assert.throws(() => commandTable({ columns: ["a"], rows: Array.from({ length: 200_000 }, () => [1]) }), /too large/);
});

test("charts retain every measure and use real numeric X coordinates for scatter plots", () => {
  const data = chartData([["Distance", "Time", "Cost"], [1, 20, -3], [100, 40, 8]]);
  const scatter = scatterData(data);
  assert.equal(scatter.xLabel, "Distance");
  assert.deepEqual(scatter.series[0]?.points.map(point => [point.x, point.y]), [[1, 20], [100, 40]]);
  assert.deepEqual(chartExtent(scatter.series[1]!.points.map(point => point.y), true), [-3, 8]);
  assert.equal(chartData([Array.from({ length: 10 }, (_, i) => `Measure ${i}`), Array.from({ length: 10 }, (_, i) => i)]).series.length, 10);
});

test("addresses cover a ten-million-row grid without accepting out-of-bounds cells", () => {
  assert.equal(cellName(MAX_ROWS - 1, MAX_COLS - 1), "XFD10485760");
  assert.deepEqual(parseCell("$XFD$10485760"), { row: MAX_ROWS - 1, col: MAX_COLS - 1 });
  assert.equal(parseCell("XFE1"), null);
  assert.equal(parseCell("A0"), null);
  assert.deepEqual(parseRange("3:5", 1000, 26), { r1: 2, c1: 0, r2: 4, c2: 25 });
});

test("sparse geometry preserves pixel locations through hidden rows, inserts and deletes", () => {
  const axis = new AxisSizes(24, 10_000_000);
  axis.setSize(2, 48); axis.setHidden(3, true);
  assert.equal(axis.offsetOf(4), 96);
  assert.equal(axis.indexAt(95), 2);
  assert.equal(axis.indexAt(96), 4);
  assert.equal(axis.indexAt(axis.offsetOf(9_000_000) + 10), 9_000_000);
  axis.insert(1, 2);
  assert.equal(axis.explicitSize(4), 48);
  assert.equal(axis.isHidden(5), true);
  axis.remove(1, 2);
  assert.equal(axis.total(), 240_000_000);
});

test("pasting formulas shifts relative cells while preserving absolute cells and quoted strings", () => {
  assert.equal(shiftFormula('=SUM(A1,$B2,C$3,$D$4)+"A1"', 2, 3), '=SUM(D3,$B4,F$3,$D$4)+"A1"');
  assert.equal(shiftFormula("=A1", -1, 0), "=#REF!");
  assert.equal(r1c1ToA1("=R[-1]C[2]+R1C1", 3, 2), "=E3+$A$1");
  assert.equal(references('=SUM(\'Q1 sales\'!A1:A5)+"B2"').length, 1);
  assert.deepEqual(functionAt("=IF(A1,SUM(B1,B2),0)", 15), { name: "SUM", argument: 1 });
});

test("clipboard round-trips quotes, tabs and newlines and retains merged HTML cells", () => {
  const rows = [["plain", "a\tb", 'a"b'], ["line\nbreak", "", "3"]];
  assert.deepEqual(parseTSV(toTSV(rows)), rows);
  assert.deepEqual(parseTSV("a\tb\r\nc\r\n"), [["a", "b"], ["c", ""]]);
  const parsed = parseHTMLTable('<table><tr><td rowspan="2" style="font-weight:bold">First</td><td>2</td></tr><tr><td>3</td></tr></table>');
  assert.ok(parsed);
  assert.equal(parsed.rows.length, 2);
  assert.equal(parsed.merges.length, 1);
});

test("charts and clipboard reads of an old version request that version, including inside copies", async () => {
  const previous = globalThis.fetch;
  let requested = "";
  globalThis.fetch = async input => { requested = String(input); return new Response(JSON.stringify({ version: 3, range: "A1", values: [[12]] }), { headers: { "Content-Type": "application/json" } }); };
  try {
    const answer = await getValues({ sheet: "sheet1234567", copy: "copy123" }, 1, "A1", undefined, 3);
    assert.equal(requested, "/api/v1/sheets/sheet1234567/copies/copy123/versions/3/tabs/1/cells?range=A1&format=values");
    assert.deepEqual(answer.values, [[12]]);
  } finally { globalThis.fetch = previous; }
});
