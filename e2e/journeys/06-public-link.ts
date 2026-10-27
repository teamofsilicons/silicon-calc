// Public links: reading a public sheet never needs signing in. Before the link is on, a signed-out request is
// refused; with it on, anyone reads the sheet, its cells, its comments and its versions and downloads it, but
// can't change anything; a signed-in Carbon who wasn't invited is a viewer; the owner can let signed-in accounts
// comment through the link; and when the link goes off, the sheet is private again.

import assert from 'node:assert/strict';
import { type Sheet, plain, at } from '../lib/calc.ts';
import { salesGrid } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { salesSheet } from '../lib/sheets.ts';
import { sameGrid } from '../lib/steps.ts';
import { type CarbonPerson, world } from '../lib/world.ts';

export default journey('A public link shows the sheet to anyone, without signing in', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let grace: CarbonPerson;
  let sheet: Sheet;
  let tab: number;

  j.step('a Carbon, and a second Carbon who is not invited', async () => {
    ada = await w.carbon('Ada');
    grace = await w.carbon('Grace');
  });

  salesSheet(j, 'the Carbon', () => ada.api, 'Sales 2026 (public)', (o) => {
    sheet = o.sheet;
    tab = o.tab.id;
  });

  j.step('the Carbon comments on the sheet', async () => {
    await ada.api.post(`/v1/sheets/${sheet.id}/comments`, { json: { tab, range: 'E8', body: 'March is low because of a late invoice.' } }, 201);
  });

  j.step('before the link is on, signed out and uninvited requests are refused', async () => {
    await w.anonymous.refused('GET', `/v1/sheets/${sheet.id}`, {}, [401, 404]);
    await w.anonymous.refused('GET', `/v1/sheets/${sheet.id}/tabs/${tab}/cells`, { query: { range: 'A1:B2' } }, [401, 404]);
    await grace.api.refused('GET', `/v1/sheets/${sheet.id}`, {}, [403, 404]);
  });

  j.step('the owner turns the public link on, for viewing', async () => {
    const p = await ada.api.put(`/v1/sheets/${sheet.id}/public`, { json: { enabled: true, access: 'view' } });
    assert.equal(p.enabled, true);
    assert.equal(p.access, 'view');
    assert.ok(String(p.url).includes(sheet.id), `the link (${p.url})`);
    j.note(`link: ${p.url}`);
  });

  j.step('anyone reads the sheet and its cells without signing in', async () => {
    const s = await w.anonymous.get<Sheet>(`/v1/sheets/${sheet.id}`);
    assert.equal(s.title, 'Sales 2026 (public)');
    assert.equal(s.role, 'viewer');
    assert.equal(s.public.enabled, true);
    assert.equal(s.tabs?.length, 1);
    const values = await w.anonymous.values(sheet.id, tab, 'A1:E37');
    sameGrid(values, salesGrid(), 'what a signed-out reader sees');
  });

  j.check('signed out, the comments and versions are readable and the sheet downloads', async () => {
    const c = await w.anonymous.get(`/v1/sheets/${sheet.id}/comments`, { query: { status: 'all' } });
    assert.equal(c.threads.length, 1);
    const v = await w.anonymous.get(`/v1/sheets/${sheet.id}/versions`);
    assert.ok(v.versions.length >= 1);
    const d = await w.anonymous.raw('GET', `/v1/sheets/${sheet.id}/download`, { query: { format: 'csv' } });
    assert.equal(d.status, 200, `download answered ${d.status}: ${d.text().slice(0, 200)}`);
    assert.match(d.text(), /^month,m,region,units,revenue/);
  });

  j.step('signed out, nothing can be changed', async () => {
    const tabs = await w.anonymous.get(`/v1/sheets/${sheet.id}/tabs`);
    await w.anonymous.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: { base_version: tabs.version, edits: [{ type: 'set', tab, cells: [{ a: 'A1', input: 'x' }] }] } }, 401, 'signed_out');
    await w.anonymous.refused('POST', `/v1/sheets/${sheet.id}/comments`, { json: { tab, range: 'A1', body: 'hi' } }, 401, 'signed_out');
    await w.anonymous.refused('PUT', `/v1/sheets/${sheet.id}/public`, { json: { enabled: false, access: 'view' } }, 401, 'signed_out');
    assert.equal(plain(at(await ada.api.cells(sheet.id, tab, 'A1'), 'A1')), 'month');
  });

  j.step('a signed-in Carbon who wasn\'t invited is a viewer through the link', async () => {
    const s = await grace.api.get<Sheet>(`/v1/sheets/${sheet.id}`);
    assert.equal(s.role, 'viewer');
    const tabs = await grace.api.get(`/v1/sheets/${sheet.id}/tabs`);
    await grace.api.refused('POST', `/v1/sheets/${sheet.id}/edits`, { json: { base_version: tabs.version, edits: [{ type: 'set', tab, cells: [{ a: 'A1', input: 'x' }] }] } }, 403, 'not_allowed');
    await grace.api.refused('POST', `/v1/sheets/${sheet.id}/comments`, { json: { tab, range: 'A1', body: 'hi' } }, 403, 'not_allowed');
  });

  j.check('with comment access, signed-in accounts comment through the link; signed out still can\'t', async () => {
    await ada.api.put(`/v1/sheets/${sheet.id}/public`, { json: { enabled: true, access: 'comment' } });
    const t = await grace.api.post(`/v1/sheets/${sheet.id}/comments`, { json: { tab, range: 'A1', body: 'Found through the link.' } }, 201);
    assert.equal(t.comments[0].author.uuid, grace.uuid);
    await w.anonymous.refused('POST', `/v1/sheets/${sheet.id}/comments`, { json: { tab, range: 'A1', body: 'hi' } }, 401, 'signed_out');
    assert.equal((await w.anonymous.get<Sheet>(`/v1/sheets/${sheet.id}`)).role, 'viewer', 'signed out, the link still only shows');
  });

  j.check('a folder has no public link', async () => {
    const f = await ada.api.post('/v1/folders', { json: { name: 'Shared stuff' } }, 201);
    await ada.api.refused('PUT', `/v1/folders/${f.id}/public`, { json: { enabled: true, access: 'view' } }, 400);
  });

  j.step('the owner turns the link off: the sheet is private again', async () => {
    const p = await ada.api.put(`/v1/sheets/${sheet.id}/public`, { json: { enabled: false, access: 'view' } });
    assert.equal(p.enabled, false);
    await w.anonymous.refused('GET', `/v1/sheets/${sheet.id}`, {}, [401, 404]);
    await grace.api.refused('GET', `/v1/sheets/${sheet.id}`, {}, [403, 404]);
  });
});
