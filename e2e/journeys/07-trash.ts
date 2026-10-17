// The trash: the owner moves a sheet to the trash and back, with its cells intact; an editor can't trash it; a
// folder goes to the trash with everything in it and comes back the same way; and a sheet is deleted forever only
// from the trash and only with its title as confirmation. The Silicon does the same with the calc command line.

import assert from 'node:assert/strict';
import { type Sheet, at, plain } from '../lib/calc.ts';
import { journey } from '../lib/journey.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

export default journey('Trash and restore', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let plan: Sheet;
  let tab: number;
  let folder: { id: string; name: string };
  let inFolder: Sheet;

  const listed = async (scope: string, id: string, who = ada) => {
    const l = await who.api.get('/v1/sheets', { query: { scope } });
    return l.sheets.some((s: Sheet) => s.id === id);
  };

  j.step('a Carbon has a sheet with data, shared with a Silicon, and a folder with a sheet in it', async () => {
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
    plan = await ada.api.post<Sheet>('/v1/sheets', { json: { title: 'Old plan' } }, 201);
    tab = (await ada.api.get<Sheet>(`/v1/sheets/${plan.id}`)).tabs![0]!.id;
    await ada.api.edit(plan.id, [{ type: 'set', tab, at: 'A1', rows: [['step', 'done'], ['draft', 'yes']] }]);
    await ada.api.post(`/v1/sheets/${plan.id}/sharing`, { json: { invite: [si.id], role: 'editor', notify: false } });
    folder = await ada.api.post('/v1/folders', { json: { name: 'Archive' } }, 201);
    inFolder = await ada.api.post<Sheet>('/v1/sheets', { json: { title: 'Filed away', folder_id: folder.id } }, 201);
    assert.equal(inFolder.folder_id, folder.id);
  });

  j.step('an editor can\'t move the owner\'s sheet to the trash', async () => {
    await si.api.refused('DELETE', `/v1/sheets/${plan.id}`, {}, 403, 'not_allowed');
  });

  j.step('the owner moves the sheet to the trash', async () => {
    await ada.api.del(`/v1/sheets/${plan.id}`, {}, 204);
    assert.equal(await listed('mine', plan.id), false, 'no longer among the Carbon\'s sheets');
    assert.equal(await listed('trash', plan.id), true, 'in the trash');
    const s = await ada.api.get<Sheet>(`/v1/sheets/${plan.id}`);
    assert.ok(s.trashed_at, 'the sheet says when it was trashed');
    assert.equal(await listed('shared', plan.id, si), false, 'gone from the Silicon\'s shared sheets too');
  });

  j.step('the owner restores it, with its cells intact', async () => {
    const s = await ada.api.post<Sheet>(`/v1/sheets/${plan.id}/restore`, {});
    assert.equal(s.trashed_at, null);
    assert.equal(await listed('mine', plan.id), true);
    assert.equal(await listed('trash', plan.id), false);
    assert.equal(plain(at(await ada.api.cells(plan.id, tab, 'B2'), 'B2')), 'yes');
    assert.equal(await listed('shared', plan.id, si), true, 'the Silicon has it again');
  });

  j.step('a folder goes to the trash with everything in it, and comes back the same way', async () => {
    await ada.api.del(`/v1/folders/${folder.id}`, {}, 204);
    const trash = await ada.api.get('/v1/sheets', { query: { scope: 'trash' } });
    assert.ok(trash.folders.some((f: any) => f.id === folder.id), 'the folder is in the trash');
    const s = await ada.api.get<Sheet>(`/v1/sheets/${inFolder.id}`);
    assert.ok(s.trashed_at, 'and the sheet in it');
    await ada.api.post(`/v1/folders/${folder.id}/restore`, {});
    assert.equal((await ada.api.get<Sheet>(`/v1/sheets/${inFolder.id}`)).trashed_at, null);
    const f = await ada.api.get(`/v1/folders/${folder.id}`);
    assert.ok(f.sheets.some((x: Sheet) => x.id === inFolder.id), 'the sheet is back in its folder');
  });

  j.step('deleting forever works only from the trash, and only with the title', async () => {
    await ada.api.refused('DELETE', `/v1/sheets/${plan.id}/forever`, { query: { confirm: 'Old plan' } }, [400, 409], undefined);
    await ada.api.del(`/v1/sheets/${plan.id}`, {}, 204);
    await ada.api.refused('DELETE', `/v1/sheets/${plan.id}/forever`, { query: { confirm: 'old plan!' } }, [400, 422]);
    await si.api.refused('DELETE', `/v1/sheets/${plan.id}/forever`, { query: { confirm: 'Old plan' } }, [403, 404]);
    await ada.api.del(`/v1/sheets/${plan.id}/forever`, { query: { confirm: 'Old plan' } }, 204);
    await ada.api.refused('GET', `/v1/sheets/${plan.id}`, {}, 404, 'not_found');
    assert.equal(await listed('trash', plan.id), false);
  });

  j.check('the Silicon uses the trash with the CLI for its own sheet (calc rm, calc trash, calc restore)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const own = await si.api.post<Sheet>('/v1/sheets', { json: { title: 'Scratch by the Silicon' } }, 201);
    await si.cli.json(['rm', own.id]);
    assert.ok((await si.api.get<Sheet>(`/v1/sheets/${own.id}`)).trashed_at, 'calc rm moved it to the trash');
    const trash = await si.cli.json(['trash']);
    assert.ok(JSON.stringify(trash).includes(own.id), 'calc trash lists it');
    await si.cli.json(['restore', own.id]);
    assert.equal((await si.api.get<Sheet>(`/v1/sheets/${own.id}`)).trashed_at, null, 'calc restore brought it back');
  });
});
