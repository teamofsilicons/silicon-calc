// Size: import a million rows, verify the first and last rows, read them through the CLI,
// calculate over the complete range and sort it. JavaScript execution is exercised in the browser suite.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { type Sheet, type Tab, at, plain } from '../lib/calc.ts';
import { type Million, million } from '../lib/fixtures.ts';
import { journey, seconds } from '../lib/journey.ts';
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

  j.check('the Silicon reads the end of the tab with the CLI (calc read)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const t0 = performance.now();
    const csv = await si.cli.text(['read', sheet.id, `A${last - 2}:D${last}`, '--format', 'csv'], { timeoutMs: LONG });
    j.note(`calc read in ${seconds(performance.now() - t0)}`);
    assert.deepEqual(csv.trim().split(/\r?\n/), m.tail.slice(-3).map((r) => r.join(',')));
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
