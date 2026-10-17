// Protected ranges: the owner keeps the data a Carbon loaded untouched. A Silicon invited as an editor can't change
// the protected cells (not with the API, not with the calc command line, not by sorting or deleting rows across
// them), can't lift the protection, and still edits everything around it. The owner can; Carbons and Silicons the
// owner chooses can; a whole tab can be protected; and the owner takes the protection off again.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { type Sheet, CalcApi, at, plain } from '../lib/calc.ts';
import { SALES_FILE, revenueOf } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

async function value(api: CalcApi, sheet: string, tab: number, a: string): Promise<unknown> {
  return plain(at(await api.cells(sheet, tab, a), a));
}

async function head(api: CalcApi, sheet: string): Promise<number> {
  return (await api.get(`/v1/sheets/${sheet}/tabs`)).version;
}

export default journey('Protected ranges keep the original data the Carbon loaded', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: { id: number; name: string };
  let protectionId: string;

  const editsBy = async (api: CalcApi, edits: unknown[]) => ({ base_version: await head(api, sheet.id), edits });

  j.step('a Carbon opens the sales CSV and invites a Silicon as an editor', async () => {
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
    sheet = await ada.api.post<Sheet>('/v1/sheets/import', { query: { filename: 'sales-2026.csv', title: 'Sales 2026' }, body: readFileSync(SALES_FILE), contentType: 'text/csv' }, 201);
    const t = (await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`)).tabs![0]!;
    tab = { id: t.id, name: t.name };
    await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [si.id], role: 'editor', notify: false } });
    await si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'G1', input: 'before any protection' }] }]);
  });

  j.step('the owner protects the original data', async () => {
    await ada.api.edit(sheet.id, [{ type: 'protect', protection: { tab: tab.id, range: 'A1:E37', editors: [], description: 'Original data' } }]);
    const cells = await ada.api.cells(sheet.id, tab.id, 'A1:E2');
    const p = (cells.protections ?? []).find((x) => x.description === 'Original data');
    assert.ok(p, `the cells answer lists the protection (${JSON.stringify(cells.protections)})`);
    assert.equal(p.range, 'A1:E37');
    protectionId = p.id;
  });

  j.step('the Silicon can\'t change the protected cells', async () => {
    const e = await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'set', tab: tab.id, cells: [{ a: 'E2', input: '1' }] }]) }, 403, 'protected');
    assert.match(e.message, /Original data/, `the refusal names the protection (${e.message})`);
    assert.ok(e.hint, 'and says what to do');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E2'), revenueOf('January', 'EU'), 'the data is untouched');
  });

  j.check('the calc command line refuses it too, and says why', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const r = await si.cli.fails(['write', sheet.id, 'E2', '1', '--tab', tab.name]);
    assert.match(r.stderr, /protect/i, `calc says the cells are protected (${r.stderr.trim().slice(0, 200)})`);
  });

  j.check('an edit request that touches protected cells applies nothing at all', async () => {
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'set', tab: tab.id, cells: [{ a: 'H1', input: 'outside' }] }, { type: 'set', tab: tab.id, cells: [{ a: 'A2', input: 'inside' }] }]) }, 403, 'protected');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'H1'), null, 'the edit outside wasn\'t applied either');
  });

  j.check('the Silicon can\'t delete rows or sort across the protected range', async () => {
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'delete_rows', tab: tab.id, at: 3, count: 1 }]) }, 403, 'protected');
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'sort', tab: tab.id, range: 'A1:E37', by: [{ col: 'E', asc: false }], header: true }]) }, 403, 'protected');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'A4'), 'January', 'the rows are where they were');
  });

  j.step('the Silicon still edits outside the protected range', async () => {
    await si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'G2', input: 'my notes' }, { a: 'A40', input: 'below the data' }] }]);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'G2'), 'my notes');
  });

  j.step('the Silicon can\'t lift or change the protection', async () => {
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'unprotect', id: protectionId }]) }, 403, 'not_allowed');
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'protect', protection: { id: protectionId, tab: tab.id, range: 'A1:E37', editors: [si.id], description: 'Original data' } }]) }, 403, 'not_allowed');
  });

  j.step('the owner can still change protected cells', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E2', input: '12450' }] }]);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E2'), 12450);
  });

  j.step('the owner lets the Silicon change them: it can', async () => {
    await ada.api.edit(sheet.id, [{ type: 'protect', protection: { id: protectionId, tab: tab.id, range: 'A1:E37', editors: [si.id], description: 'Original data' } }]);
    await si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E3', input: '21100' }] }]);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E3'), 21100);
  });

  j.check('a whole tab can be protected', async () => {
    const added = await ada.api.edit(sheet.id, [{ type: 'add_tab', name: 'Notes' }]);
    const notes = (await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`)).tabs!.find((t) => t.name === 'Notes');
    assert.ok(notes, `the tab was added (${JSON.stringify(added.changes)})`);
    await ada.api.edit(sheet.id, [{ type: 'protect', protection: { tab: notes.id, editors: [], description: 'Only Ada' } }]);
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'set', tab: notes.id, cells: [{ a: 'Z99', input: 'x' }] }]) }, 403, 'protected');
  });

  j.step('the owner takes the protection off; the Silicon edits the data again', async () => {
    await ada.api.edit(sheet.id, [{ type: 'protect', protection: { id: protectionId, tab: tab.id, range: 'A1:E37', editors: [], description: 'Original data' } }]);
    await si.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: await editsBy(si.api, [{ type: 'set', tab: tab.id, cells: [{ a: 'E4', input: '1' }] }]) }, 403, 'protected');
    await ada.api.edit(sheet.id, [{ type: 'unprotect', id: protectionId }]);
    await si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E4', input: '7700' }] }]);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E4'), 7700);
    const cells = await ada.api.cells(sheet.id, tab.id, 'A1:E2');
    assert.ok(!(cells.protections ?? []).some((p) => p.id === protectionId), 'the protection is gone');
  });

  j.check('code can\'t write into protected cells either', async () => {
    await ada.api.edit(sheet.id, [{ type: 'protect', protection: { tab: tab.id, range: 'A1:E37', editors: [], description: 'Original data' } }]);
    const r = await si.api.raw('POST', `${CalcApi.line(sheet.id)}/run`, { json: { language: 'sql', code: 'select 1 as one', into: { tab: tab.id, at: 'A1' } } });
    assert.equal(r.status, 403, `writing code results over protected cells answers ${r.status}: ${r.text().slice(0, 200)}`);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'A1'), 'month');
  });
});
