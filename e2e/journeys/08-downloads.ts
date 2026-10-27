// Files: a sheet with data, a formula, a bold header, a chart and two tabs downloads in every format, and every
// format that can be opened opens again as a new sheet with the same values (formulas, formatting and charts kept
// in the formats that hold them). A file also opens as new tabs of an existing sheet or replaces a tab; one tab and
// an old version download on their own; and the Silicon downloads and opens files with the calc command line.

import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { type Sheet, at, plain } from '../lib/calc.ts';
import { FORMATS, type Format, OPENABLE, SALES_FILE, TARGETS_FILE, checkFormat, parseDelimited, salesGrid, totalRevenue } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { salesSheet, tabsOf } from '../lib/sheets.ts';
import { attempt, needs, sameGrid } from '../lib/steps.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

/** Formats that keep formulas (and formatting) when a sheet is written to them. */
const KEEPS_FORMULAS: Format[] = ['xlsx', 'xls', 'ods', 'numbers'];
/** Formats that keep charts. */
const KEEPS_CHARTS: Format[] = ['xlsx', 'ods'];
/** Formats that hold every tab, not only one. */
const MANY_TABS: Format[] = ['xlsx', 'xls', 'ods', 'numbers', 'json'];

const TARGETS = [['region', 'target', 'owner'], ['EU', 150000, 'Ada'], ['US', 270000, 'Grace'], ['APAC', 100000, 'Ada']];

export default journey('Downloads in every format, and opening every format', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: number;
  let targets: number;
  let targetsName = '';
  let vImported: number;
  let hasChart = false;
  let cliSignedIn = false;
  const files = new Map<Format, string>();
  let dir: string;

  j.step('a Carbon and a Silicon it made', async () => {
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
    dir = w.scratch('downloads');
  });

  salesSheet(j, 'the Carbon', () => ada.api, 'Sales 2026', (o) => {
    sheet = o.sheet;
    tab = o.tab.id;
  });

  j.step('the Carbon adds a formula and makes the header bold', async () => {
    vImported = (await ada.api.get(`/v1/sheets/${sheet.id}/tabs`)).version;
    await ada.api.edit(sheet.id, [
      { type: 'set', tab, cells: [{ a: 'G1', input: 'total' }, { a: 'G2', input: '=SUM(E2:E37)' }] },
      { type: 'style', tab, range: 'A1:E1', patch: { bold: true } },
    ]);
    const cells = await ada.api.cells(sheet.id, tab, 'A1:G2');
    assert.equal(plain(at(cells, 'G2')), totalRevenue());
    const s = at(cells, 'A1')?.s;
    assert.equal(s !== undefined ? (cells.styles as any)?.[String(s)]?.bold : undefined, true, 'A1 is bold');
  });

  j.check('the Carbon adds a chart of revenue', async () => {
    await ada.api.edit(sheet.id, [{ type: 'chart', chart: { tab, kind: 'column', title: 'Revenue', range: 'D1:E37', anchor: 'I2', width: 480, height: 300 } }]);
    const cells = await ada.api.cells(sheet.id, tab, 'A1:B2');
    const charts = (cells.charts ?? []) as Array<{ title?: string }>;
    assert.ok(charts.some((c) => c.title === 'Revenue'), `the cells answer lists the chart (${JSON.stringify(charts).slice(0, 200)})`);
    hasChart = true;
  });

  attempt(j, 'the Carbon adds the targets as a second tab', 'opens targets.csv as a new tab: POST {line}/tabs/import', {
    first: async () => {
      const answer = await ada.api.post(`/v1/sheets/${sheet.id}/tabs/import`, { query: { filename: 'targets.csv', mode: 'new_tab' }, body: readFileSync(TARGETS_FILE), contentType: 'text/csv' });
      assert.ok(Array.isArray(answer.tabs), 'the answer lists the tabs');
      return null;
    },
    otherwise: async () => {
      if ((await tabsOf(ada.api, sheet.id)).length < 2) {
        await ada.api.edit(sheet.id, [{ type: 'add_tab', name: 'targets' }]);
        const t = (await tabsOf(ada.api, sheet.id))[1]!;
        await ada.api.edit(sheet.id, [{ type: 'set', tab: t.id, at: 'A1', rows: TARGETS.map((r) => r.map(String)) }]);
      }
      return null;
    },
    note: 'opening targets.csv into the sheet failed, so the table was typed into a new tab',
    then: async () => {
      const tabs = await tabsOf(ada.api, sheet.id);
      assert.equal(tabs.length, 2);
      targets = tabs[1]!.id;
      targetsName = tabs[1]!.name;
      sameGrid(await ada.api.values(sheet.id, targets, 'A1:C4'), TARGETS, 'the targets tab');
      await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [si.id], role: 'viewer', notify: false } });
    },
  });

  for (const format of FORMATS) {
    j.check(`the sheet downloads as ${format}`, async () => {
      const a = await ada.api.raw('GET', `/v1/sheets/${sheet.id}/download`, { query: { format }, accept: '*/*', timeoutMs: 120_000 });
      assert.equal(a.status, 200, `download?format=${format} answered ${a.status}: ${a.text().slice(0, 300)}`);
      const cd = a.headers.get('content-disposition') ?? '';
      assert.match(cd, /attachment/, `Content-Disposition: ${cd}`);
      assert.ok(cd.includes(`Sales 2026.${format}`) || decodeURIComponent(cd).includes(`Sales 2026.${format}`), `the file is named after the sheet (${cd})`);
      j.note(checkFormat(format, a.bytes));
      const file = join(dir, `sales.${format}`);
      writeFileSync(file, a.bytes);
      files.set(format, file);
      if (format === 'csv' || format === 'tsv') {
        const rows = parseDelimited(a.text(), format === 'csv' ? ',' : '\t');
        assert.deepEqual(rows[0]!.slice(0, 5), ['month', 'm', 'region', 'units', 'revenue']);
        assert.equal(rows[1]![6], String(totalRevenue()), 'the formula\'s value is in the file');
      }
      if (format === 'json') {
        const v = JSON.parse(a.text());
        assert.equal(v.tabs.length, 2, 'both tabs');
        assert.deepEqual(v.tabs[0].rows[0].slice(0, 5), ['month', 'm', 'region', 'units', 'revenue']);
      }
    }, { timeoutMs: 180_000 });
  }

  for (const format of OPENABLE) {
    j.check(`a ${format} file opens again as a new sheet with the same values`, async () => {
      const file = files.get(format);
      needs(file, `the ${format} download failed, so there is nothing to open`);
      const s = await ada.api.post<Sheet>('/v1/sheets/import', { query: { filename: `round-trip.${format}`, title: `Round trip ${format}` }, body: readFileSync(file!), timeoutMs: 120_000 }, 201);
      const tabs = await tabsOf(ada.api, s.id);
      assert.equal(tabs.length, MANY_TABS.includes(format) ? 2 : 1, `${tabs.length} tab(s): ${tabs.map((t) => t.name).join(', ')}`);
      const values = await ada.api.values(s.id, tabs[0]!.id, 'A1:E37');
      sameGrid(values, salesGrid(), `the ${format} file's values`);
      const g = await ada.api.cells(s.id, tabs[0]!.id, 'A1:G2');
      assert.equal(plain(at(g, 'G2')), totalRevenue(), 'the formula\'s value');
      if (KEEPS_FORMULAS.includes(format)) {
        assert.equal(at(g, 'G2')?.f, '=SUM(E2:E37)', `${format} keeps the formula`);
        const style = g.styles && at(g, 'A1')?.s !== undefined ? (g.styles as any)[String(at(g, 'A1')!.s)] : null;
        assert.equal(style?.bold, true, `${format} keeps the bold header`);
      }
      if (KEEPS_CHARTS.includes(format) && hasChart) {
        const charts = (g.charts ?? []) as Array<{ title?: string; kind?: string }>;
        assert.ok(charts.some((c) => c.title === 'Revenue'), `${format} keeps the chart (${JSON.stringify(charts).slice(0, 200)})`);
      }
      if (MANY_TABS.includes(format)) {
        sameGrid(await ada.api.values(s.id, tabs[1]!.id, 'A1:C4'), TARGETS, `the ${format} file's second tab`);
      }
    }, { timeoutMs: 180_000 });
  }

  j.check('a file opens as new tabs of an existing sheet', async () => {
    const file = files.get('xlsx');
    needs(file, 'the xlsx download failed');
    const before = (await tabsOf(ada.api, sheet.id)).length;
    const r = await ada.api.post(`/v1/sheets/${sheet.id}/tabs/import`, { query: { filename: 'more.xlsx', mode: 'new_tab' }, body: readFileSync(file!) });
    assert.equal(r.tabs.length, before + 2, 'both of the file\'s tabs were added');
  });

  j.check('a file replaces one tab\'s contents', async () => {
    const csv = 'region,target,owner\nEU,160000,Ada\nUS,280000,Grace\nAPAC,110000,Ada\nLATAM,50000,Grace\n';
    await ada.api.post(`/v1/sheets/${sheet.id}/tabs/import`, { query: { filename: 'targets-v2.csv', mode: 'replace', tab: targets }, body: Buffer.from(csv), contentType: 'text/csv' });
    const values = await ada.api.values(sheet.id, targets, 'A1:C5');
    sameGrid(values, [['region', 'target', 'owner'], ['EU', 160000, 'Ada'], ['US', 280000, 'Grace'], ['APAC', 110000, 'Ada'], ['LATAM', 50000, 'Grace']], 'the replaced tab');
  });

  j.check('one tab downloads on its own', async () => {
    const a = await ada.api.raw('GET', `/v1/sheets/${sheet.id}/download`, { query: { format: 'csv', tab: targets }, accept: '*/*' });
    assert.equal(a.status, 200, `answered ${a.status}: ${a.text().slice(0, 200)}`);
    assert.match(a.text(), /^region,target,owner/);
  });

  j.check('an old version downloads as it was', async () => {
    const a = await ada.api.raw('GET', `/v1/sheets/${sheet.id}/download`, { query: { format: 'csv', version: vImported }, accept: '*/*' });
    assert.equal(a.status, 200, `answered ${a.status}: ${a.text().slice(0, 200)}`);
    const rows = parseDelimited(a.text());
    assert.equal(rows[0]!.length, 5, 'before the total was added, five columns');
  });

  j.check('the JSON download carries formulas when asked', async () => {
    const a = await ada.api.raw('GET', `/v1/sheets/${sheet.id}/download`, { query: { format: 'json', formulas: true }, accept: '*/*' });
    assert.equal(a.status, 200, `answered ${a.status}: ${a.text().slice(0, 200)}`);
    const v = JSON.parse(a.text());
    const g2 = v.tabs[0].rows[1][6];
    assert.equal(g2?.f, '=SUM(E2:E37)', `G2 is ${JSON.stringify(g2)}`);
  });

  j.check('a viewer downloads; the Silicon downloads with the CLI (calc download)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    cliSignedIn = true;
    const out = join(dir, 'by-the-silicon.xlsx');
    await si.cli.json(['download', sheet.id, '--format', 'xlsx', '-o', out]);
    j.note(checkFormat('xlsx', readFileSync(out)));
    const csv = join(dir, 'targets-by-the-silicon.csv');
    await si.cli.json(['download', sheet.id, '--format', 'csv', '--tab', targetsName, '-o', csv]);
    assert.match(readFileSync(csv, 'utf8'), /^region,target,owner/, 'one tab as CSV');
  });

  j.check('the Silicon opens a file as a new sheet with the CLI (calc import)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the check before)');
    const made = await si.cli.json(['import', SALES_FILE, '--title', 'Opened by the Silicon']);
    const id = made.id ?? made.sheet?.id;
    assert.ok(id, `calc import answered ${JSON.stringify(made).slice(0, 200)}`);
    const s = await si.api.get<Sheet>(`/v1/sheets/${id}`);
    assert.equal(s.owner.uuid, si.uuid, 'the Silicon owns what it opened');
    sameGrid(await si.api.values(id, s.tabs![0]!.id, 'A1:E37'), salesGrid(), 'the opened file');
  });

  j.check('a PDF can\'t be opened (download only)', async () => {
    const file = files.get('pdf');
    needs(file, 'the pdf download failed');
    const r = await ada.api.raw('POST', '/v1/sheets/import', { query: { filename: 'sales.pdf' }, body: readFileSync(file!) });
    assert.ok([400, 415, 422].includes(r.status), `opening a PDF answered ${r.status}`);
    const e = r.json()?.error;
    assert.ok(e?.message && e?.hint, `the refusal says why and what to do (${r.text().slice(0, 200)})`);
  });
});
