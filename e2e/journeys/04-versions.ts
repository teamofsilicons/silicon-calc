// Versions: everything is kept with who made it and when. The owner names a version, a Silicon changes cells, the
// owner changes more; the versions say who changed what; an old version opens as it was and compares with now;
// everything the Silicon changed since the named version is undone while the owner's later changes stay; a
// version is restored, as a new version, without losing what came after it; one range is restored; a version
// becomes a new sheet; and an account undoes and redoes its own last change.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { type Sheet, type Version, CalcApi, at, plain } from '../lib/calc.ts';
import { SALES_FILE, revenueOf } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

async function versions(api: CalcApi, sheet: string): Promise<Version[]> {
  return (await api.get<{ versions: Version[] }>(`/v1/sheets/${sheet}/versions`, { query: { limit: 100 } })).versions;
}

async function value(api: CalcApi, sheet: string, tab: number, a: string, version?: number): Promise<unknown> {
  if (version !== undefined) {
    const c = await api.get(`/v1/sheets/${sheet}/versions/${version}/tabs/${tab}/cells`, { query: { range: a } });
    return plain(at(c, a));
  }
  return plain(at(await api.cells(sheet, tab, a), a));
}

async function head(api: CalcApi, sheet: string): Promise<number> {
  return (await api.get(`/v1/sheets/${sheet}/tabs`)).version;
}

export default journey('Versions: name, open, compare, undo one account, restore', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: number;
  let vImport: number;
  let vNamed: number;
  let vBeforeUndo: number;
  let vBeforeRestore: number;
  const E2 = revenueOf('January', 'EU');
  const E3 = revenueOf('January', 'US');

  j.step('a Carbon opens the sales CSV and shares it with a Silicon as an editor', async () => {
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
    sheet = await ada.api.post<Sheet>('/v1/sheets/import', { query: { filename: 'sales-2026.csv', title: 'Sales 2026' }, body: readFileSync(SALES_FILE), contentType: 'text/csv' }, 201);
    tab = (await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`)).tabs![0]!.id;
    vImport = await head(ada.api, sheet.id);
    assert.ok(vImport >= 1, 'opening a file is a version');
    await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [si.id], role: 'editor', notify: false } });
  });

  j.step('the Carbon reviews the data and names that version', async () => {
    vNamed = (await ada.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'A39', input: 'Checked by Ada' }] }])).version;
    await ada.api.post(`/v1/sheets/${sheet.id}/versions/${vNamed}/name`, { json: { name: 'Before the Silicon' } });
    const named = (await versions(ada.api, sheet.id)).find((v) => v.name === 'Before the Silicon');
    assert.ok(named, 'the versions list shows the name');
    assert.equal(named.version, vNamed);
  });

  j.step('the Silicon changes cells', async () => {
    await si.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'E2', input: '1' }] }]);
    await si.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'E3', input: '2' }] }]);
    await si.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'G1', input: 'si scratch' }] }]);
    assert.equal(await value(ada.api, sheet.id, tab, 'E2'), 1);
  });

  j.step('then the Carbon changes another cell', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'A40', input: 'Ada again' }] }]);
    vBeforeUndo = await head(ada.api, sheet.id);
  });

  j.step('the versions say who changed what, newest first', async () => {
    const list = await versions(ada.api, sheet.id);
    assert.equal(list[0]!.version, vBeforeUndo, 'the newest version is the sheet as it is now');
    assert.equal(list[0]!.by.uuid, ada.uuid);
    for (let i = 1; i < list.length; i++) assert.ok(list[i - 1]!.version > list[i]!.version, 'newest first');
    const bySilicon = list.filter((v) => v.by.uuid === si.uuid);
    assert.ok(bySilicon.length >= 1, 'the Silicon\'s changes are a version');
    assert.equal(bySilicon[0]!.by.id, si.id, 'shown by si: id');
    assert.equal(bySilicon[0]!.kind, 'edit');
    j.note(bySilicon.map((v) => `${v.from_version}..${v.version}: ${v.summary}`).join(' | '));
  });

  j.check('changes by one account close together are one version', async () => {
    const bySilicon = (await versions(ada.api, sheet.id)).filter((v) => v.by.uuid === si.uuid);
    assert.equal(bySilicon.length, 1, `the Silicon's three edits in a row are one version (got ${bySilicon.length})`);
    assert.ok(bySilicon[0]!.from_version < bySilicon[0]!.version);
    assert.ok(bySilicon[0]!.summary.includes(si.id), `the summary names the Silicon by si: id (${bySilicon[0]!.summary})`);
  });

  j.step('the named version opens as it was', async () => {
    assert.equal(await value(ada.api, sheet.id, tab, 'E2', vNamed), E2);
    assert.equal(await value(ada.api, sheet.id, tab, 'A39', vNamed), 'Checked by Ada');
    assert.equal(await value(ada.api, sheet.id, tab, 'A40', vNamed), null);
    const tabs = await ada.api.get(`/v1/sheets/${sheet.id}/versions/${vNamed}/tabs`);
    assert.ok(Array.isArray(tabs.tabs) && tabs.tabs.length === 1);
  });

  j.step('the named version compares with now', async () => {
    const cmp = await ada.api.get(`/v1/sheets/${sheet.id}/compare`, { query: { from: vNamed, to: 'head' } });
    const cells = new Map<string, any>((cmp.cells ?? []).map((c: any) => [c.address, c]));
    for (const a of ['E2', 'E3', 'G1', 'A40']) assert.ok(cells.has(a), `${a} changed (the comparison shows ${[...cells.keys()].join(', ')})`);
    const by = (a: string) => {
      const b = cells.get(a)?.by;
      return typeof b === 'string' ? b : b?.uuid;
    };
    assert.equal(by('E2'), si.uuid, 'E2 was the Silicon\'s');
    assert.equal(by('A40'), ada.uuid, 'A40 was the Carbon\'s');
    assert.equal(cmp.accounts?.[si.uuid]?.id, si.id);
  });

  j.step('everything the Silicon changed since the named version is undone; the Carbon\'s changes stay', async () => {
    const r = await ada.api.post(`/v1/sheets/${sheet.id}/versions/${vNamed}/undo-account`, { json: { account: si.id } });
    assert.equal(r.cells_changed, 3, 'E2, E3 and G1');
    assert.ok(r.version > vBeforeUndo, 'the undo is a new version');
    assert.equal(await value(ada.api, sheet.id, tab, 'E2'), E2);
    assert.equal(await value(ada.api, sheet.id, tab, 'E3'), E3);
    assert.equal(await value(ada.api, sheet.id, tab, 'G1'), null);
    assert.equal(await value(ada.api, sheet.id, tab, 'A40'), 'Ada again', 'the Carbon\'s later change is kept');
    assert.equal(await value(ada.api, sheet.id, tab, 'A39'), 'Checked by Ada');
  });

  j.check('the undo is a version of its own, by who asked for it', async () => {
    const newest = (await versions(ada.api, sheet.id))[0]!;
    assert.equal(newest.kind, 'undo');
    assert.equal(newest.by.uuid, ada.uuid);
  });

  j.step('restoring a version makes a new version; nothing after it is lost', async () => {
    vBeforeRestore = await head(ada.api, sheet.id);
    const r = await ada.api.post(`/v1/sheets/${sheet.id}/versions/${vImport}/restore`, { json: {} });
    assert.ok(r.version > vBeforeRestore, 'the restore is a new version');
    assert.equal(await value(ada.api, sheet.id, tab, 'A39'), null, 'the sheet is as it was when the file was opened');
    assert.equal(await value(ada.api, sheet.id, tab, 'A40'), null);
    assert.equal((await versions(ada.api, sheet.id))[0]!.kind, 'restore');
    assert.equal(await value(ada.api, sheet.id, tab, 'A40', vBeforeRestore), 'Ada again', 'the version before the restore is still there');
  });

  j.check('one range is restored on its own', async () => {
    await ada.api.post(`/v1/sheets/${sheet.id}/versions/${vBeforeRestore}/restore`, { json: { tab, range: 'A40' } });
    assert.equal(await value(ada.api, sheet.id, tab, 'A40'), 'Ada again');
    assert.equal(await value(ada.api, sheet.id, tab, 'A39'), null, 'only A40 came back');
  });

  j.check('a version becomes a new sheet', async () => {
    const s = await ada.api.post<Sheet>(`/v1/sheets/${sheet.id}/versions/${vNamed}/new-sheet`, { json: { title: 'Sales at review' } }, 201);
    assert.equal(s.title, 'Sales at review');
    const t = (await ada.api.get<Sheet>(`/v1/sheets/${s.id}`)).tabs![0]!.id;
    assert.equal(await value(ada.api, s.id, t, 'A39'), 'Checked by Ada');
    assert.equal(await value(ada.api, s.id, t, 'E2'), E2);
  });

  j.check('a name is taken off a version', async () => {
    await ada.api.del(`/v1/sheets/${sheet.id}/versions/${vNamed}/name`);
    assert.ok(!(await versions(ada.api, sheet.id)).some((v) => v.name === 'Before the Silicon'));
  });

  j.check('an account undoes and redoes its own last change', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'A41', input: 'typo' }] }]);
    await si.api.edit(sheet.id, [{ type: 'set', tab, cells: [{ a: 'H1', input: 'the Silicon\'s' }] }]);
    await ada.api.post(`${CalcApi.line(sheet.id)}/edits`, { json: { base_version: await head(ada.api, sheet.id), edits: [{ type: 'undo' }] } });
    assert.equal(await value(ada.api, sheet.id, tab, 'A41'), null, 'the Carbon\'s own last change is undone');
    assert.equal(await value(ada.api, sheet.id, tab, 'H1'), 'the Silicon\'s', 'not the Silicon\'s later one');
    await ada.api.post(`${CalcApi.line(sheet.id)}/edits`, { json: { base_version: await head(ada.api, sheet.id), edits: [{ type: 'redo' }] } });
    assert.equal(await value(ada.api, sheet.id, tab, 'A41'), 'typo');
  });

  j.check('a cell shows its own history', async () => {
    const c = await ada.api.get(`/v1/sheets/${sheet.id}/tabs/${tab}/cells/E2`);
    const history = c.history as Array<{ by: any }>;
    assert.ok(Array.isArray(history) && history.length >= 2, `E2 changed at least twice (history: ${JSON.stringify(history).slice(0, 200)})`);
    const who = history.map((h) => (typeof h.by === 'string' ? h.by : h.by?.uuid));
    assert.ok(who.includes(si.uuid), 'the Silicon\'s change is in it');
  });

  j.check('the Silicon lists the versions with the CLI (calc versions ls)', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    const out = await si.cli.json(['versions', 'ls', sheet.id]);
    assert.ok(JSON.stringify(out).includes(String(vImport)) || JSON.stringify(out).includes('restore'));
  });
});
