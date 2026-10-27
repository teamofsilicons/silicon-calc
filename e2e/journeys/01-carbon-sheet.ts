// A Carbon signs in, makes a sheet, types values and formulas, and brings in data: a CSV opened as a new sheet and
// another added to it as a tab. Every cell says who last changed it and when; the sheets are listed and found. The
// Carbon also signs in to the calc command line with the device flow (approving the code in its browser) and uses it.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { approveDevice } from '../lib/accounts.ts';
import { type CellsAnswer, type Sheet, at, plain } from '../lib/calc.ts';
import { SALES_FILE, TARGETS_FILE, salesGrid } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { importCsv, tabsOf } from '../lib/sheets.ts';
import { needs, sameGrid } from '../lib/steps.ts';
import { type CarbonPerson, world } from '../lib/world.ts';

export default journey('A Carbon makes a sheet, types formulas and imports a CSV', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let budget: Sheet;
  let tab1: number;
  let sales: Sheet;
  let salesTab: { id: number; name: string };
  let cliSignedIn = false;

  j.step('a Carbon signs up with an email code and signs in to Calc', async () => {
    ada = await w.carbon('Ada');
    const me = await ada.api.get('/v1/me');
    assert.equal(me.account.uuid, ada.uuid, 'GET /v1/me names the signed-in Carbon by uuid');
    assert.equal(me.account.id, ada.id);
    assert.equal(me.account.kind, 'carbon');
    j.note(`${ada.id} (${ada.uuid})`);
  });

  j.check('the service says what it is and its limits (GET /v1/meta)', async () => {
    const meta = await ada.api.get('/v1/meta');
    assert.equal(meta.app_id, 'calc');
    assert.ok(meta.limits?.max_rows >= 1_000_000, `max_rows is ${meta.limits?.max_rows}`);
    assert.ok(meta.limits?.max_cols >= 16384, `max_cols is ${meta.limits?.max_cols}`);
    assert.ok(meta.limits?.max_upload_bytes > 0);
  });

  j.check('a signed-out request is refused with signed_out', async () => {
    const e = await w.anonymous.refused('GET', '/v1/me', {}, 401, 'signed_out');
    assert.ok(e.hint, `the refusal says what to do (${e.message})`);
    await w.anonymous.refused('POST', '/v1/sheets', { json: { title: 'x' } }, 401, 'signed_out');
  });

  j.part('typing values and formulas');

  j.step('the Carbon makes a blank sheet with one tab', async () => {
    budget = await ada.api.post<Sheet>('/v1/sheets', { json: { title: 'Household budget' } }, 201);
    assert.match(budget.id, /^[a-z0-9]{12}$/, 'sheet ids are 12 characters of a-z and 0-9');
    assert.equal(budget.title, 'Household budget');
    assert.equal(budget.owner.uuid, ada.uuid);
    assert.equal(budget.owner.id, ada.id);
    assert.equal(budget.role, 'owner');
    const full = await ada.api.get<Sheet>(`/v1/sheets/${budget.id}`);
    assert.equal(full.tabs?.length, 1, 'a new sheet has one tab');
    assert.equal(full.tabs![0]!.name, 'Tab 1');
    tab1 = full.tabs![0]!.id;
  });

  j.step('the Carbon types values and formulas', async () => {
    const answer = await ada.api.edit(budget.id, [
      { type: 'set', tab: tab1, at: 'A1', rows: [['item', 'cost'], ['rent', '1200'], ['food', '450'], ['travel', '300']] },
      { type: 'set', tab: tab1, cells: [{ a: 'A5', input: 'total' }, { a: 'B5', input: '=SUM(B2:B4)' }, { a: 'A6', input: 'average' }, { a: 'B6', input: '=AVERAGE(B2:B4)' }, { a: 'C2', input: '=B2/$B$5' }] },
    ]);
    assert.ok(answer.version >= 1, `the edit made version ${answer.version}`);
    const cells = await ada.api.cells(budget.id, tab1, 'A1:C6');
    assert.equal(plain(at(cells, 'B5')), 1950, 'B5 =SUM(B2:B4)');
    assert.equal(at(cells, 'B5')?.f, '=SUM(B2:B4)', 'B5 keeps its formula');
    assert.equal(plain(at(cells, 'B6')), 650, 'B6 =AVERAGE(B2:B4)');
    assert.ok(Math.abs(Number(plain(at(cells, 'C2'))) - 1200 / 1950) < 1e-9, 'C2 =B2/$B$5');
    assert.equal(plain(at(cells, 'A2')), 'rent');
    assert.equal(plain(at(cells, 'B2')), 1200, 'typed numbers are numbers');
  });

  j.check('every cell says who last changed it and when', async () => {
    const cells = await ada.api.cells(budget.id, tab1, 'A1:B6');
    assert.ok(cells.cells.length >= 12, `${cells.cells.length} cells`);
    for (const c of cells.cells) {
      assert.equal(c.by, ada.uuid, `${c.a} was changed by ${c.by}`);
      assert.ok(!Number.isNaN(Date.parse(c.at)), `${c.a} has no time of change (${c.at})`);
    }
    assert.equal(cells.accounts[ada.uuid]?.id, ada.id, 'the answer shows the account by its c: id');
  });

  j.step('changing a value recalculates the formulas that use it', async () => {
    await ada.api.edit(budget.id, [{ type: 'set', tab: tab1, cells: [{ a: 'B3', input: '500' }] }]);
    const cells: CellsAnswer = await ada.api.cells(budget.id, tab1, 'B2:C6');
    assert.equal(plain(at(cells, 'B5')), 2000);
    assert.equal(plain(at(cells, 'B6')), 2000 / 3);
    assert.ok(Math.abs(Number(plain(at(cells, 'C2'))) - 0.6) < 1e-9);
  });

  j.check('a formula that can\'t be read is refused with bad_formula', async () => {
    const tabs = await ada.api.get(`/v1/sheets/${budget.id}/tabs`);
    const e = await ada.api.refused('POST', `/v1/sheets/${budget.id}/edits`, { json: { base_version: tabs.version, edits: [{ type: 'set', tab: tab1, cells: [{ a: 'D1', input: '=SUM(' }] }] } }, 422, 'bad_formula');
    assert.match(`${e.message} ${JSON.stringify(e.details ?? {})}`, /D1/, `the refusal names the cell (${e.message})`);
  });

  j.check('one cell answers with its history', async () => {
    const cell = await ada.api.get(`/v1/sheets/${budget.id}/tabs/${tab1}/cells/B3`);
    const c = cell.cell ?? cell;
    assert.equal(plain(c), 500);
    assert.ok(Array.isArray(cell.history), 'the cell answer has history');
    assert.ok(cell.history.length >= 2, `B3 changed twice; history has ${cell.history.length}`);
  });

  j.check('the Carbon renames the sheet', async () => {
    const renamed = await ada.api.patch<Sheet>(`/v1/sheets/${budget.id}`, { json: { title: 'Budget 2026' } });
    assert.equal(renamed.title, 'Budget 2026');
  });

  j.part('opening files');

  j.step('the Carbon opens a CSV as a new sheet', async () => {
    const t0 = performance.now();
    sales = await importCsv(ada.api, SALES_FILE, 'Sales 2026');
    j.note(`imported in ${Math.round(performance.now() - t0)} ms`);
    assert.equal(sales.title, 'Sales 2026');
    assert.equal(sales.owner.uuid, ada.uuid);
    const tabs = await tabsOf(ada.api, sales.id);
    assert.equal(tabs.length, 1, 'a CSV is one tab');
    salesTab = { id: tabs[0]!.id, name: tabs[0]!.name };
    assert.equal(tabs[0]!.used, 'A1:E37', 'the used area is the CSV: 5 columns, a header and 36 rows');
    j.note(`tab "${salesTab.name}"`);
  });

  j.step('the CSV\'s values come back as they were, numbers as numbers', async () => {
    const values = await ada.api.values(sales.id, salesTab.id, 'A1:E37');
    sameGrid(values, salesGrid(), 'the imported CSV');
    assert.equal(typeof values[1]![4], 'number', 'revenue is a number');
    assert.equal(typeof values[1]![0], 'string', 'month is text');
  });

  j.check('a range reads as CSV (format=csv)', async () => {
    const a = await ada.api.raw('GET', `/v1/sheets/${sales.id}/tabs/${salesTab.id}/cells`, { query: { range: 'A1:C3', format: 'csv' }, accept: 'text/csv' });
    assert.equal(a.status, 200, `answered ${a.status}: ${a.text().slice(0, 200)}`);
    assert.match(a.headers.get('content-type') ?? '', /text\/csv/);
    const lines = a.text().trim().split(/\r?\n/);
    assert.deepEqual(lines, ['month,m,region', 'January,1,EU', 'January,1,US']);
  });

  j.check('the Carbon adds another CSV to the sheet as a new tab', async () => {
    const answer = await ada.api.post(`/v1/sheets/${sales.id}/tabs/import`, { query: { filename: 'targets.csv', mode: 'new_tab' }, body: readFileSync(TARGETS_FILE), contentType: 'text/csv' });
    assert.ok(Array.isArray(answer.tabs), 'the answer lists the tabs');
    const tabs = await tabsOf(ada.api, sales.id);
    assert.equal(tabs.length, 2);
    const targets = tabs.find((t) => t.id !== salesTab.id)!;
    const values = await ada.api.values(sales.id, targets.id, 'A1:C4');
    sameGrid(values, [['region', 'target', 'owner'], ['EU', 150000, 'Ada'], ['US', 270000, 'Grace'], ['APAC', 100000, 'Ada']], 'the targets tab');
  });

  j.check('the Carbon\'s sheets are listed', async () => {
    const list = await ada.api.get('/v1/sheets', { query: { scope: 'mine' } });
    const ids = list.sheets.map((s: Sheet) => s.id);
    assert.ok(ids.includes(sales.id), `listed: ${ids.join(', ')}`);
    if (budget) assert.ok(ids.includes(budget.id), `the blank sheet is listed too: ${ids.join(', ')}`);
  });

  j.check('search finds sheets by title and by cell text', async () => {
    const byTitle = await ada.api.get('/v1/search', { query: { q: 'Sales 2026' } });
    assert.ok(byTitle.results.some((r: any) => r.sheet.id === sales.id), 'found by title');
    const byCell = await ada.api.get('/v1/search', { query: { q: 'September' } });
    const hit = byCell.results.find((r: any) => r.sheet.id === sales.id);
    assert.ok(hit, 'the sales sheet is found by a word in its cells');
    assert.ok(hit.matches.some((m: any) => /September/.test(m.text)), 'the match shows the cell text');
  });

  j.part('the calc command line, as a Carbon');

  j.step('the Carbon signs in to the calc command line with the device flow, approving the code in its browser (calc login)', async () => {
    const s = await ada.cli.loginCarbonDevice((code) => approveDevice(ada.carbon, code));
    assert.equal(s.id, ada.id, `signed in as ${s.id}`);
    assert.equal(s.kind, 'carbon');
    const who = await ada.cli.json(['whoami']);
    assert.equal(who.id ?? who.account?.id, ada.id, 'calc whoami names the Carbon');
    cliSignedIn = true;
  });

  j.check('the Carbon makes a sheet and writes into it with the CLI (calc new, calc write, calc read)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in');
    const made = await ada.cli.json(['new', 'Made in the terminal']);
    const id = made.id ?? made.sheet?.id;
    assert.ok(id, `calc new answered ${JSON.stringify(made).slice(0, 200)}`);
    await ada.cli.json(['write', id, 'A1', 'item', 'cost']);
    await ada.cli.json(['write', id, 'A2', 'rent', '1200']);
    await ada.cli.json(['write', id, 'B3', '=B2*2']);
    const csv = await ada.cli.text(['read', id, 'A1:B3', '--format', 'csv']);
    assert.deepEqual(csv.trim().split(/\r?\n/), ['item,cost', 'rent,1200', ',2400']);
    const formulas = await ada.cli.text(['read', id, 'B3', '--format', 'csv', '--formulas']);
    assert.equal(formulas.trim(), '=B2*2', 'calc read --formulas shows the formula');
  });

  j.check('the Carbon signs out of the calc command line (calc logout)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in');
    await ada.cli.json(['logout']);
    const r = await ada.cli.run(['--json', 'login', 'status']);
    assert.equal(JSON.parse(r.stdout.trim()).authenticated, false, 'calc login status says it is signed out');
  });
});
