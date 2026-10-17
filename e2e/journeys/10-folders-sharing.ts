// Folders, sharing and ownership, the way Silicon Write does them: a Carbon keeps sheets in a folder and shares the
// folder with a Silicon (who is told, and gets the folder's role on everything in it); a sheet the Silicon makes in
// that folder belongs to the folder's owner; the higher of two roles wins. A Silicon owns its own sheets and invites
// a Carbon, who comments but can't edit and can leave. An email with no account yet becomes membership when that
// Carbon first signs in. Ownership moves only when the new owner accepts.

import assert from 'node:assert/strict';
import { type Sheet, at, plain } from '../lib/calc.ts';
import { tag } from '../lib/accounts.ts';
import { journey } from '../lib/journey.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

export default journey('Folders, shared folders, Silicons owning sheets, and transfers', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let grace: CarbonPerson;
  let si: SiliconPerson;
  let finance: { id: string; name: string };
  let budget: Sheet;
  let tab: number;
  let notes: Sheet;
  let since = 0;

  const head = async (sheet: string) => (await ada.api.get(`/v1/sheets/${sheet}/tabs`)).version;

  j.step('a Carbon keeps a sheet in a folder, in a folder tree', async () => {
    ada = await w.carbon('Ada');
    grace = await w.carbon('Grace');
    si = await w.silicon(ada, 'Analyst');
    finance = await ada.api.post('/v1/folders', { json: { name: 'Finance' } }, 201);
    const sub = await ada.api.post('/v1/folders', { json: { name: '2026', parent_id: finance.id } }, 201);
    budget = await ada.api.post<Sheet>('/v1/sheets', { json: { title: 'Q3 budget', folder_id: sub.id } }, 201);
    tab = (await ada.api.get<Sheet>(`/v1/sheets/${budget.id}`)).tabs![0]!.id;
    await ada.api.edit(budget.id, [{ type: 'set', tab, at: 'A1', rows: [['line', 'amount'], ['rent', '1200']] }]);
    const f = await ada.api.get(`/v1/folders/${sub.id}`);
    assert.deepEqual(f.path.map((x: any) => x.name), ['Finance'], 'the folder knows its path');
    assert.ok(f.sheets.some((s: Sheet) => s.id === budget.id));
    const top = await ada.api.get(`/v1/folders/${finance.id}`);
    assert.ok(top.folders.some((x: any) => x.id === sub.id));
  });

  j.step('the Carbon shares the folder with the Silicon as a viewer; the Silicon is told', async () => {
    since = await w.ting.seq();
    const sharing = await ada.api.post(`/v1/folders/${finance.id}/sharing`, { json: { invite: [si.id], role: 'viewer', notify: true } });
    assert.equal(sharing.members.find((m: any) => m.account.uuid === si.uuid)?.role, 'viewer');
    const t = await w.ting.wait({ for: si.uuid, type: 'calc.sheet.shared', after: since });
    assert.ok(JSON.stringify(t.data).includes(finance.id), `the ting names the folder (${JSON.stringify(t.data).slice(0, 300)})`);
  });

  j.step('the Silicon reads the sheet in the folder, but can\'t change it', async () => {
    const s = await si.api.get<Sheet>(`/v1/sheets/${budget.id}`);
    assert.equal(s.role, 'viewer', 'the folder\'s role reaches the sheets in it, in its subfolders too');
    assert.equal(plain(at(await si.api.cells(budget.id, tab, 'B2'), 'B2')), 1200);
    await si.api.refused('POST', `/v1/sheets/${budget.id}/edits`, { json: { base_version: await head(budget.id), edits: [{ type: 'set', tab, cells: [{ a: 'B2', input: '1' }] }] } }, 403, 'not_allowed');
  });

  j.step('as an editor of the folder, the Silicon edits, and the sheet\'s sharing says where its role comes from', async () => {
    await ada.api.patch(`/v1/folders/${finance.id}/sharing/${si.uuid}`, { json: { role: 'editor' } });
    await si.api.edit(budget.id, [{ type: 'set', tab, cells: [{ a: 'B3', input: '=B2*2' }] }]);
    assert.equal(plain(at(await ada.api.cells(budget.id, tab, 'B3'), 'B3')), 2400);
    const sharing = await ada.api.get(`/v1/sheets/${budget.id}/sharing`);
    const m = sharing.members.find((x: any) => x.account.uuid === si.uuid);
    assert.equal(m?.role, 'editor');
    assert.equal(m?.via?.folder?.id, finance.id, `the role comes through the folder (${JSON.stringify(m?.via)})`);
  });

  j.step('a sheet the Silicon makes in the Carbon\'s folder belongs to the folder\'s owner', async () => {
    const made = await si.api.post<Sheet>('/v1/sheets', { json: { title: 'Made by the Silicon', folder_id: finance.id } }, 201);
    assert.equal(made.owner.uuid, ada.uuid, 'owned by the folder\'s owner');
    assert.equal(made.created_by.uuid, si.uuid, 'made by the Silicon');
  });

  j.check('the higher of two roles wins', async () => {
    await ada.api.post(`/v1/sheets/${budget.id}/sharing`, { json: { invite: [si.id], role: 'viewer', notify: false } });
    assert.equal((await si.api.get<Sheet>(`/v1/sheets/${budget.id}`)).role, 'editor', 'editor through the folder beats viewer on the sheet');
  });

  j.step('a Silicon owns its own sheet and invites a Carbon as a commenter', async () => {
    since = await w.ting.seq();
    notes = await si.api.post<Sheet>('/v1/sheets', { json: { title: 'Analyst notes' } }, 201);
    assert.equal(notes.owner.uuid, si.uuid);
    assert.equal(notes.role, 'owner');
    await si.api.post(`/v1/sheets/${notes.id}/sharing`, { json: { invite: [grace.id], role: 'commenter', notify: true } });
    const t = await w.ting.wait({ for: grace.uuid, type: 'calc.sheet.shared', after: since });
    assert.equal(t.data.sheet?.id, notes.id);
  });

  j.step('the Carbon comments on the Silicon\'s sheet, can\'t edit it, and the Silicon is told', async () => {
    const ntab = (await grace.api.get<Sheet>(`/v1/sheets/${notes.id}`)).tabs![0]!.id;
    await grace.api.post(`/v1/sheets/${notes.id}/comments`, { json: { tab: ntab, range: 'A1', body: 'Can you add the Q3 totals here?' } }, 201);
    const t = await w.ting.wait({ for: si.uuid, type: 'calc.comment.created', after: since });
    assert.equal(t.data.comment?.author?.id, grace.id);
    const v = (await grace.api.get(`/v1/sheets/${notes.id}/tabs`)).version;
    await grace.api.refused('POST', `/v1/sheets/${notes.id}/edits`, { json: { base_version: v, edits: [{ type: 'set', tab: ntab, cells: [{ a: 'A1', input: 'x' }] }] } }, 403, 'not_allowed');
  });

  j.check('a member leaves a sheet', async () => {
    await grace.api.del(`/v1/sheets/${notes.id}/sharing/${grace.uuid}`, {}, [200, 204]);
    await grace.api.refused('GET', `/v1/sheets/${notes.id}`, {}, [403, 404]);
  });

  j.check('an email with no account yet becomes membership when that Carbon first signs in', async () => {
    const email = `newcomer${tag(6)}@calc-e2e.test`;
    const s = await ada.api.post(`/v1/sheets/${budget.id}/sharing`, { json: { invite: [email], role: 'commenter', notify: true } });
    assert.ok(s.invites.some((i: any) => i.email === email), 'the email is waiting as an invite');
    const newcomer = await w.carbon('Newcomer', { email });
    await newcomer.api.get('/v1/me');
    const shared = await newcomer.api.get('/v1/sheets', { query: { scope: 'shared' } });
    assert.ok(shared.sheets.some((x: Sheet) => x.id === budget.id), 'the sheet is shared with the new Carbon');
    const after = await ada.api.get(`/v1/sheets/${budget.id}/sharing`);
    assert.equal(after.members.find((m: any) => m.account.uuid === newcomer.uuid)?.role, 'commenter');
    assert.ok(!after.invites.some((i: any) => i.email === email), 'the invite turned into membership');
  });

  j.step('ownership moves only when the new owner accepts', async () => {
    const t = await ada.api.post(`/v1/sheets/${budget.id}/transfer`, { json: { to: grace.id } }, 201);
    assert.equal(t.status, 'pending');
    assert.equal(t.to.uuid, grace.uuid);
    assert.equal((await ada.api.get<Sheet>(`/v1/sheets/${budget.id}`)).owner.uuid, ada.uuid, 'still the Carbon\'s until accepted');
    const incoming = await grace.api.get('/v1/transfers');
    assert.ok(incoming.incoming.some((x: any) => x.id === t.id));
    const outgoing = await ada.api.get('/v1/transfers');
    assert.ok(outgoing.outgoing.some((x: any) => x.id === t.id));
    const accepted = await grace.api.post(`/v1/transfers/${t.id}/accept`, {});
    assert.equal(accepted.status, 'accepted');
    const s = await grace.api.get<Sheet>(`/v1/sheets/${budget.id}`);
    assert.equal(s.owner.uuid, grace.uuid);
    assert.equal(s.role, 'owner');
  });

  j.check('a transfer can be declined, or cancelled by who offered it', async () => {
    const other = await ada.api.post<Sheet>('/v1/sheets', { json: { title: 'Keep this' } }, 201);
    const t1 = await ada.api.post(`/v1/sheets/${other.id}/transfer`, { json: { to: grace.id } }, 201);
    assert.equal((await grace.api.post(`/v1/transfers/${t1.id}/decline`, {})).status, 'declined');
    const t2 = await ada.api.post(`/v1/sheets/${other.id}/transfer`, { json: { to: si.id } }, 201);
    assert.equal((await ada.api.post(`/v1/transfers/${t2.id}/cancel`, {})).status, 'cancelled');
    assert.equal((await ada.api.get<Sheet>(`/v1/sheets/${other.id}`)).owner.uuid, ada.uuid);
  });

  j.check('the Silicon sees its sheets and who has them with the CLI (calc ls, calc share ls)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const ls = await si.cli.json(['ls']);
    assert.ok(JSON.stringify(ls).includes(notes.id), 'calc ls lists the Silicon\'s own sheet');
    const share = await si.cli.json(['share', 'ls', notes.id]);
    assert.ok(JSON.stringify(share).includes(si.id), 'calc share ls names the owner');
  });
});
