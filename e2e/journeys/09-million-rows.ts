// Size: a sheet holds a million rows and stays quick. A Carbon opens a 1,000,000-row CSV; its last rows read back
// exactly; a Silicon asks questions of all of it with SQL (count, sums, groups, a new tab), with Python and with the
// calc command line; a formula sums the million rows; the rows sort. Every answer is checked against the file's
// own totals, and every step says how long it took.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { type Sheet, type Tab, at, plain } from '../lib/calc.ts';
import { type Million, million } from '../lib/fixtures.ts';
import { journey, seconds } from '../lib/journey.ts';
import { num } from '../lib/steps.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

const ROWS = Number(process.env.E2E_MILLION_ROWS ?? 1_000_000);
const LONG = 20 * 60_000;

export default journey(`A ${ROWS.toLocaleString('en-US')}-row CSV is imported and queried`, (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let m: Million;
  let sheet: Sheet;
  let tab: Tab;
  const quoted = () => `"${tab.name.replace(/"/g, '""')}"`;
  const last = ROWS + 1;

  j.step(`the ${ROWS.toLocaleString('en-US')}-row CSV is ready, with its totals`, async () => {
    const t0 = performance.now();
    m = await million(join(w.dir, 'fixtures'), ROWS);
    j.note(`${m.file}: ${(m.bytes / 1e6).toFixed(1)} MB, sum(amount) ${m.total}, made or found in ${seconds(performance.now() - t0)}`);
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
  });

  j.step('the Carbon opens it as a new sheet', async () => {
    const t0 = performance.now();
    sheet = await ada.api.post<Sheet>('/v1/sheets/import', { query: { filename: 'million.csv', title: 'A million rows' }, body: readFileSync(m.file), contentType: 'text/csv', timeoutMs: LONG }, 201);
    j.note(`imported in ${seconds(performance.now() - t0)}`);
    const full = await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`);
    tab = full.tabs![0]!;
    assert.equal(tab.used, `A1:D${last}`, `the used area is every row (${tab.used})`);
    assert.ok(tab.rows >= last, `the tab shows ${tab.rows} rows`);
    await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [si.id], role: 'editor', notify: false } });
  }, { timeoutMs: LONG });

  j.step('the last rows read back exactly', async () => {
    const t0 = performance.now();
    const values = await si.api.values(sheet.id, tab.id, `A${last - 6}:D${last}`);
    j.note(`read in ${seconds(performance.now() - t0)}`);
    assert.deepEqual(values, m.tail, 'the last seven rows');
  });

  j.check('the first screen of rows reads quickly', async () => {
    const t0 = performance.now();
    const cells = await ada.api.cells(sheet.id, tab.id, 'A1:D60');
    const ms = performance.now() - t0;
    j.note(`A1:D60 in ${seconds(ms)}`);
    assert.equal(cells.cells.length, 240);
    assert.ok(ms < 5000, `reading the first screen took ${seconds(ms)}`);
  });

  j.step('SQL counts and sums the million rows', async () => {
    const t0 = performance.now();
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'sql', code: `select count(*) as n, sum(amount) as total from ${quoted()}`, into: null }, timeoutMs: LONG });
    j.note(`first query in ${seconds(performance.now() - t0)} (service says ${r.duration_ms} ms)`);
    assert.deepEqual(r.columns, ['n', 'total']);
    assert.equal(num(r.rows[0][0]), ROWS);
    assert.equal(num(r.rows[0][1]), m.total);
  }, { timeoutMs: LONG });

  j.step('SQL groups the million rows by region', async () => {
    const t0 = performance.now();
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'sql', code: `select region, count(*) as n, sum(amount) as total from ${quoted()} group by region order by region` }, timeoutMs: LONG });
    j.note(`second query in ${seconds(performance.now() - t0)} (service says ${r.duration_ms} ms; the tab's table is cached)`);
    const got = Object.fromEntries(r.rows.map((x: unknown[]) => [x[0], [num(x[1]), num(x[2])]]));
    for (const region of Object.keys(m.byRegion)) assert.deepEqual(got[region], [m.countByRegion[region], m.byRegion[region]], region);
  }, { timeoutMs: LONG });

  j.check('SQL writes its answer into a new tab', async () => {
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'sql', code: `select region, sum(amount) as total from ${quoted()} group by region order by total desc`, into: { new_tab: 'By region' } }, timeoutMs: LONG });
    assert.equal(r.tab?.name, 'By region');
    const values = await ada.api.values(sheet.id, r.tab.id, 'A1:B5');
    assert.equal(values.length, 5);
    assert.equal(num(values[1]![1]), Math.max(...Object.values(m.byRegion)));
  }, { timeoutMs: LONG });

  j.check('the Silicon asks the million rows with the CLI (calc run sql)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const t0 = performance.now();
    const out = await si.cli.json(['run', 'sql', sheet.id, '-c', `select max(amount) as top, count(distinct region) as regions from ${quoted()}`], { timeoutMs: LONG });
    j.note(`calc run sql in ${seconds(performance.now() - t0)}`);
    const rows = out.rows ?? out.result?.rows;
    assert.equal(num(rows[0][0]), m.maxAmount);
    assert.equal(num(rows[0][1]), 4);
  }, { timeoutMs: LONG });

  j.check('the Silicon reads the end of the tab with the CLI (calc read)', async () => {
    const t0 = performance.now();
    const csv = await si.cli.text(['read', sheet.id, `A${last - 2}:D${last}`, '--format', 'csv'], { timeoutMs: LONG });
    j.note(`calc read in ${seconds(performance.now() - t0)}`);
    assert.deepEqual(csv.trim().split(/\r?\n/), m.tail.slice(-3).map((r) => r.join(',')));
  }, { timeoutMs: LONG });

  j.check('Python reads the million rows as a DataFrame', async () => {
    const t0 = performance.now();
    const code = `df = calc.tab(${JSON.stringify(tab.name)})\nresult = [[len(df), int(df["amount"].sum())]]`;
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'python', code }, timeoutMs: LONG });
    j.note(`Python in ${seconds(performance.now() - t0)}`);
    assert.equal(num(r.rows[0][0]), ROWS);
    assert.equal(num(r.rows[0][1]), m.total);
  }, { timeoutMs: LONG });

  j.check('a formula sums the million rows', async () => {
    const t0 = performance.now();
    await ada.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'F1', input: `=SUM(D2:D${last})` }, { a: 'F2', input: `=COUNTIF(C2:C${last},"EU")` }] }]);
    j.note(`set and calculated in ${seconds(performance.now() - t0)}`);
    const cells = await ada.api.cells(sheet.id, tab.id, 'F1:F2');
    assert.equal(plain(at(cells, 'F1')), m.total);
    assert.equal(plain(at(cells, 'F2')), m.countByRegion.EU);
  }, { timeoutMs: LONG });

  j.check('the million rows sort', async () => {
    const t0 = performance.now();
    await si.api.edit(sheet.id, [{ type: 'sort', tab: tab.id, range: `A1:D${last}`, by: [{ col: 'D', asc: false }, { col: 'A', asc: true }], header: true }]);
    j.note(`sorted in ${seconds(performance.now() - t0)}`);
    const top = await ada.api.values(sheet.id, tab.id, 'A1:D3');
    assert.deepEqual(top[0], ['id', 'day', 'region', 'amount'], 'the header stays on top');
    assert.equal(top[1]![3], m.maxAmount);
    assert.ok(Number(top[1]![0]) < Number(top[2]![0]), 'ties keep id order (the second key)');
    const cells = await ada.api.cells(sheet.id, tab.id, 'F1');
    assert.equal(plain(at(cells, 'F1')), m.total, 'the sum is the same after the sort');
  }, { timeoutMs: LONG });
});
