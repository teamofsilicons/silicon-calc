// Copies: a Silicon makes a copy of the sheet to try a fix without touching it, and a copy of that copy for another
// idea. Meanwhile the owner keeps working on the sheet. A second Carbon (an editor) compares the copy with the
// sheet and merges part of it; the owner gets calc.copy.merged (the owner is told when someone else merges into
// their sheet, not when they merge themselves). Conflicting cells are chosen one by one. A copy that isn't needed
// is deleted and stays readable in the history.

import assert from 'node:assert/strict';
import { type Copy, type Sheet, CalcApi, at, plain } from '../lib/calc.ts';
import { revenueOf } from '../lib/fixtures.ts';
import { journey } from '../lib/journey.ts';
import { salesSheet } from '../lib/sheets.ts';
import { needs, viaCli } from '../lib/steps.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

async function copies(api: CalcApi, sheet: string, status = 'all'): Promise<Copy[]> {
  return (await api.get<{ copies: Copy[] }>(`/v1/sheets/${sheet}/copies`, { query: { status } })).copies;
}

async function value(api: CalcApi, sheet: string, tab: number, a: string, copy?: string | null): Promise<unknown> {
  return plain(at(await api.cells(sheet, tab, a, { copy }), a));
}

export default journey('A Silicon tries ideas in copies; a Carbon compares and merges part of one', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let grace: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: { id: number; name: string };
  let copy: Copy;
  let nested: Copy;
  let since = 0;
  let cliSignedIn = false;
  const copyName = 'Try a March fix';

  j.step('a Carbon who owns the sheet, a second Carbon and a Silicon', async () => {
    ada = await w.carbon('Ada');
    grace = await w.carbon('Grace');
    si = await w.silicon(ada, 'Analyst');
  });

  salesSheet(j, 'the Carbon', () => ada.api, 'Sales 2026', (o) => {
    sheet = o.sheet;
    tab = o.tab;
  });

  j.step('the Carbon invites the second Carbon and the Silicon as editors', async () => {
    since = await w.ting.seq();
    const sharing = await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [grace.id, si.id], role: 'editor', notify: true } });
    for (const p of [grace, si]) assert.equal(sharing.members.find((m: any) => m.account.uuid === p.uuid)?.role, 'editor', `${p.id} is an editor`);
  });

  j.check('the second Carbon, invited by c: id, gets calc.sheet.shared', async () => {
    const t = await w.ting.wait({ for: grace.uuid, type: 'calc.sheet.shared', after: since });
    assert.equal(t.data.sheet?.id, sheet.id);
  });

  j.check('the Silicon signs in to the calc command line', async () => {
    await si.cli.loginSilicon(si.id, si.stk);
    cliSignedIn = true;
  });

  viaCli(j, 'the Silicon makes a copy of the sheet', 'calc copies new', {
    cli: async () => {
      if (!cliSignedIn) throw new Error('the CLI is not signed in');
      return si.cli.json(['copies', 'new', sheet.id, copyName]);
    },
    http: async () => {
      const made = (await copies(si.api, sheet.id)).find((c) => c.name === copyName);
      return made ?? si.api.post<Copy>(`/v1/sheets/${sheet.id}/copies`, { json: { name: copyName } }, 201);
    },
    then: async () => {
      const list = await copies(ada.api, sheet.id);
      const c = list.find((x) => x.name === copyName);
      assert.ok(c, `the sheet's copies include "${copyName}" (${list.map((x) => x.name).join(', ') || 'none'})`);
      assert.equal(c.parent_id, null, 'made from the main sheet');
      assert.equal(c.status, 'open');
      assert.equal(c.created_by.uuid, si.uuid);
      assert.equal(c.forked_at, (await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`)).version, 'forked at the sheet as it was');
      copy = c;
    },
  });

  viaCli(j, 'the Silicon tries changes in the copy', 'calc write --copy', {
    cli: async () => {
      await si.cli.json(['write', sheet.id, 'E8', '9900', '--tab', tab.name, '--copy', copy.id]);
      await si.cli.json(['write', sheet.id, 'E11', '13000', '--tab', tab.name, '--copy', copy.id]);
      await si.cli.json(['write', sheet.id, 'F1', 'note', '--tab', tab.name, '--copy', copy.id]);
      return si.cli.json(['write', sheet.id, 'F8', 'EU March corrected from the invoices', '--tab', tab.name, '--copy', copy.id]);
    },
    http: () =>
      si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E8', input: '9900' }, { a: 'E11', input: '13000' }, { a: 'F1', input: 'note' }, { a: 'F8', input: 'EU March corrected from the invoices' }] }], { copy: copy.id }),
    then: async () => {
      assert.equal(await value(si.api, sheet.id, tab.id, 'E8', copy.id), 9900, 'the copy has the change');
      assert.equal(await value(si.api, sheet.id, tab.id, 'F8', copy.id), 'EU March corrected from the invoices');
    },
  });

  j.step('the main sheet is untouched by the copy', async () => {
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E8'), revenueOf('March', 'EU'));
    assert.equal(await value(ada.api, sheet.id, tab.id, 'F1'), null);
  });

  j.step('copies can have their own copies', async () => {
    nested = await si.api.post<Copy>(`${CalcApi.line(sheet.id, copy.id)}/copies`, { json: { name: 'Try weekly buckets' } }, 201);
    assert.equal(nested.parent_id, copy.id, 'a copy of the copy');
    await si.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'G1', input: 'weekly' }] }], { copy: nested.id });
    assert.equal(await value(si.api, sheet.id, tab.id, 'G1', nested.id), 'weekly');
    assert.equal(await value(si.api, sheet.id, tab.id, 'G1', copy.id), null, 'its parent copy is untouched');
    const list = await copies(ada.api, sheet.id);
    assert.ok(list.some((c) => c.id === nested.id && c.parent_id === copy.id), 'the tree of copies');
    assert.equal((await ada.api.get<Sheet>(`/v1/sheets/${sheet.id}`)).copies, 2, 'the sheet counts its open copies');
  });

  j.step('meanwhile the owner changes the sheet', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E8', input: '4100' }, { a: 'E20', input: '14300' }] }]);
  });

  j.step('the second Carbon compares the copy with the sheet', async () => {
    const cmp = await grace.api.get(`/v1/sheets/${sheet.id}/copies/${copy.id}/compare`);
    const changed = (cmp.diff?.cells ?? []).map((c: any) => c.address);
    for (const a of ['E8', 'E11', 'F1', 'F8']) assert.ok(changed.includes(a), `the comparison shows ${a} (it shows ${changed.join(', ')})`);
    assert.ok(!changed.includes('E20'), 'the comparison is the copy against the sheet at the fork: the owner\'s later E20 is not a change of the copy');
    assert.equal(cmp.parent_changed, true, 'the sheet changed since the copy was made');
    const conflict = (cmp.conflicts ?? []).find((c: any) => c.a === 'E8');
    assert.ok(conflict, 'E8 changed on both sides: a conflict');
    assert.equal(conflict.base?.value?.v, revenueOf('March', 'EU'), 'the conflict shows the value at the fork');
    assert.equal(conflict.sheet?.value?.v, 4100, 'the sheet\'s value');
    assert.equal(conflict.copy?.value?.v, 9900, 'the copy\'s value');
    const e8 = cmp.diff.cells.find((c: any) => c.address === 'E8');
    const by = typeof e8?.by === 'string' ? e8.by : e8?.by?.uuid;
    if (by !== undefined) assert.equal(by, si.uuid, 'the change in the copy is the Silicon\'s');
  });

  j.step('the second Carbon merges part of the copy: only column F', async () => {
    since = await w.ting.seq();
    const r = await grace.api.post(`/v1/sheets/${sheet.id}/copies/${copy.id}/merge`, { json: { range: { tab: tab.id, range: 'F1:F37' } } });
    assert.equal(r.cells_taken, 2, 'F1 and F8');
    assert.deepEqual(r.conflicts ?? [], [], 'nothing in column F conflicts');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'F8'), 'EU March corrected from the invoices', 'the sheet has the copy\'s note');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E8'), 4100, 'E8 is outside the merged range: the owner\'s value stays');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E11'), revenueOf('April', 'EU'), 'E11 is outside it too');
    const after = (await copies(ada.api, sheet.id)).find((c) => c.id === copy.id);
    assert.equal(after?.status, 'open', 'the copy stays open after a merge without close');
  });

  j.step('the owner gets calc.copy.merged', async () => {
    const t = await w.ting.wait({ for: ada.uuid, type: 'calc.copy.merged', after: since });
    assert.equal(t.data.sheet?.id, sheet.id);
    assert.equal(t.data.copy?.id, copy.id);
    assert.equal(t.data.copy?.name, copyName);
    j.note(`data: ${JSON.stringify(t.data).slice(0, 300)}`);
  });

  j.check('a dry run shows the conflicts, and each conflict is chosen', async () => {
    const dry = await grace.api.post(`/v1/sheets/${sheet.id}/copies/${copy.id}/merge`, { json: { dry_run: true } });
    const c = (dry.conflicts ?? []).find((x: any) => x.a === 'E8');
    assert.ok(c, 'E8 is a conflict');
    assert.equal(c.sheet?.value?.v, 4100);
    assert.equal(c.copy?.value?.v, 9900);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E8'), 4100, 'a dry run changes nothing');
    const r = await grace.api.post(`/v1/sheets/${sheet.id}/copies/${copy.id}/merge`, { json: { resolutions: [{ tab: tab.id, a: 'E8', take: 'copy' }] } });
    assert.ok(r.cells_taken >= 2, `${r.cells_taken} cells taken`);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E8'), 9900, 'the copy\'s E8 was chosen');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E11'), 13000, 'E11 changed only in the copy: taken');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'E20'), 14300, 'E20 changed only in the sheet: kept');
  });

  j.check('the owner is not told when the owner merges', async () => {
    since = await w.ting.seq();
    await ada.api.post(`/v1/sheets/${sheet.id}/copies/${nested.id}/merge`, { json: {} });
    assert.equal(await value(ada.api, sheet.id, tab.id, 'G1', copy.id), 'weekly', 'the nested copy merged into its parent copy');
    await w.ting.none({ for: ada.uuid, type: 'calc.copy.merged', after: since }, 2500);
  });

  j.step('a copy that isn\'t needed is deleted and stays readable', async () => {
    await si.api.del(`/v1/sheets/${sheet.id}/copies/${nested.id}`, {}, 204);
    const all = await copies(ada.api, sheet.id, 'all');
    assert.equal(all.find((c) => c.id === nested.id)?.status, 'deleted');
    const open = await copies(ada.api, sheet.id, 'open');
    assert.ok(!open.some((c) => c.id === nested.id), 'not among the open copies');
    assert.equal(await value(ada.api, sheet.id, tab.id, 'G1', nested.id), 'weekly', 'its cells are still in the history');
  });

  j.check('the Silicon lists and compares copies with the CLI (calc copies ls, calc copies compare)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const ls = await si.cli.json(['copies', 'ls', sheet.id]);
    assert.ok(JSON.stringify(ls).includes(copy.id), 'calc copies ls lists the copy');
    const all = await si.cli.json(['copies', 'ls', sheet.id, '--all']);
    assert.ok(JSON.stringify(all).includes(nested.id), 'calc copies ls --all lists the deleted copy too');
    const cmp = await si.cli.json(['copies', 'compare', sheet.id, copy.id]);
    const pending = (cmp.diff?.cells ?? []).map((c: any) => c.address);
    assert.ok(pending.includes('G1'), 'calc copies compare shows the newly merged nested-copy change G1');
    assert.ok(!pending.includes('E8'), 'already accepted E8 is no longer pending in the open copy');
  });

  j.check('the Silicon makes another copy with the CLI, changes it and merges it back (calc copies new, write, merge)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const made = await si.cli.json(['copies', 'new', sheet.id, 'Add a check column']);
    const id = made.id ?? made.copy?.id;
    assert.ok(id, `calc copies new answered ${JSON.stringify(made).slice(0, 200)}`);
    await si.cli.json(['write', sheet.id, 'H1', 'checked', '--tab', tab.name, '--copy', id]);
    const dry = await si.cli.json(['copies', 'merge', sheet.id, id, '--dry-run']);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'H1'), null, `a dry run changes nothing (${JSON.stringify(dry).slice(0, 200)})`);
    await si.cli.json(['copies', 'merge', sheet.id, id, '--close']);
    assert.equal(await value(ada.api, sheet.id, tab.id, 'H1'), 'checked', 'the merge brought H1 into the sheet');
    const after = (await copies(ada.api, sheet.id)).find((c) => c.id === id);
    assert.notEqual(after?.status, 'open', 'merged with --close, the copy is no longer open');
  });

  j.check('a viewer can\'t make copies', async () => {
    const viewer = await w.carbon('Viewer');
    await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [viewer.id], role: 'viewer', notify: false } });
    await viewer.api.refused('POST', `/v1/sheets/${sheet.id}/copies`, { json: { name: 'mine' } }, 403, 'not_allowed');
  });
});
