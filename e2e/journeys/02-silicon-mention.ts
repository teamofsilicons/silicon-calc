// The story Silicon Calc is for: a Carbon brings in data and asks a Silicon about it by mentioning it on a range;
// the Silicon gets everything it needs in a ting, reads the cells with the calc command line, answers the question
// with SQL and Python into new tabs, and replies in the thread; the Carbon is told, sees every change live, and
// every cell says who made it. Then the rest of comments: reactions, resolving (and the ting for it), reopening,
// comments that move with their cells, a Silicon's comment telling the owner, code tabs that keep themselves
// updated, and JavaScript.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { type Sheet, type Tab, type Thread, CalcApi, at, plain } from '../lib/calc.ts';
import { LiveEvents } from '../lib/events.ts';
import { SALES_FILE, revenueByMonth, revenueOf, salesGrid, totalRevenue } from '../lib/fixtures.ts';
import { eventually, journey } from '../lib/journey.ts';
import { num, sameGrid, viaCli } from '../lib/steps.ts';
import type { TingView } from '../lib/ting.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

async function tabs(api: CalcApi, sheet: string): Promise<Tab[]> {
  return (await api.get<Sheet>(`/v1/sheets/${sheet}`)).tabs ?? [];
}

async function findTab(api: CalcApi, sheet: string, name: string): Promise<Tab | undefined> {
  return (await tabs(api, sheet)).find((t) => t.name === name);
}

async function thread(api: CalcApi, sheet: string, id: string): Promise<Thread> {
  const list = await api.get<{ threads: Thread[] }>(`/v1/sheets/${sheet}/comments`, { query: { status: 'all' } });
  const t = list.threads.find((x) => x.id === id);
  if (!t) throw new Error(`thread ${id} is not among the sheet's ${list.threads.length} threads`);
  return t;
}

const sqlTab = 'By month';
const pyTab = 'March vs February';

export default journey('A Carbon mentions a Silicon on a range; the Silicon does the analysis', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: { id: number; name: string };
  let live: LiveEvents | null = null;
  let thr: Thread;
  let mention: TingView;
  let replyId: string;
  let since = 0;
  let cliSignedIn = false;

  j.cleanup(async () => live?.close());

  j.step('a Carbon, and a Silicon it made (its custodian)', async () => {
    ada = await w.carbon('Ada');
    si = await w.silicon(ada, 'Analyst');
    j.note(`${ada.id} and ${si.id}`);
    since = await w.ting.seq();
  });

  j.check('the Silicon signs in to the calc command line with its STK (calc login --silicon --stk-stdin)', async () => {
    const s = await si.cli.loginSilicon(si.id, si.stk);
    assert.equal(s.id, si.id, `signed in as ${s.id}`);
    assert.equal(s.kind, 'silicon');
    const who = await si.cli.json(['whoami']);
    assert.equal(who.id ?? who.account?.id, si.id, 'calc whoami names the Silicon');
    cliSignedIn = true;
  });

  j.step('the Carbon opens the sales CSV as a new sheet', async () => {
    sheet = await ada.api.post<Sheet>('/v1/sheets/import', { query: { filename: 'sales-2026.csv', title: 'Sales 2026' }, body: readFileSync(SALES_FILE), contentType: 'text/csv' }, 201);
    const t = (await tabs(ada.api, sheet.id))[0]!;
    tab = { id: t.id, name: t.name };
  });

  j.step('the Carbon finds the Silicon by its si: id', async () => {
    const found = await ada.api.get('/v1/accounts/lookup', { query: { id: si.id } });
    assert.equal(found.uuid, si.uuid);
    assert.equal(found.kind, 'silicon');
    assert.equal(found.id, si.id);
    await ada.api.refused('GET', '/v1/accounts/lookup', { query: { id: 'si:nobodyhere0000' } }, 404);
  });

  j.step('the Carbon invites the Silicon as an editor by its si: id', async () => {
    const sharing = await ada.api.post(`/v1/sheets/${sheet.id}/sharing`, { json: { invite: [si.id], role: 'editor', notify: true } });
    const m = sharing.members.find((x: any) => x.account.uuid === si.uuid);
    assert.ok(m, 'the Silicon is a member');
    assert.equal(m.role, 'editor');
    assert.equal(m.account.id, si.id, 'members are shown by their si: id');
    assert.equal(sharing.owner.uuid, ada.uuid);
  });

  j.step('the Silicon gets calc.sheet.shared', async () => {
    const t = await w.ting.wait({ for: si.uuid, type: 'calc.sheet.shared', after: since });
    assert.equal(t.issuing_app, 'calc');
    assert.equal(t.data.sheet?.id, sheet.id, 'the ting names the sheet');
    assert.equal(t.data.sheet?.title, 'Sales 2026');
    assert.ok(String(t.data.url ?? '').includes(sheet.id), `the ting links to the sheet (${t.data.url})`);
    j.note(`key ${t.key}`);
  });

  j.step('the Silicon sees the sheet among those shared with it', async () => {
    const list = await si.api.get('/v1/sheets', { query: { scope: 'shared' } });
    const s = list.sheets.find((x: Sheet) => x.id === sheet.id);
    assert.ok(s, 'the sheet is listed as shared with the Silicon');
    assert.equal(s.role, 'editor');
    assert.equal(s.owner.id, ada.id);
  });

  j.check('the Silicon lists it with the CLI (calc ls --shared)', async () => {
    if (!cliSignedIn) throw new Error('the CLI is not signed in (see the login check)');
    const out = await si.cli.json(['ls', '--shared']);
    assert.ok(JSON.stringify(out).includes(sheet.id), 'calc ls --shared lists the sheet');
  });

  j.check('the Carbon watches the sheet live (GET /v1/sheets/{id}/events)', async () => {
    const v = (await ada.api.get(`/v1/sheets/${sheet.id}/tabs`)).version;
    live = await LiveEvents.open(ada.api, CalcApi.line(sheet.id), v);
  });

  j.step('the Carbon selects the revenue column and asks the Silicon why March dropped', async () => {
    since = await w.ting.seq();
    thr = await ada.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: tab.id, range: 'E2:E37', body: `@${si.id} why did March drop?` } }, 201);
    assert.equal(thr.range, 'E2:E37');
    assert.equal(thr.original_range, 'E2:E37');
    assert.equal(thr.status, 'open');
    assert.equal(thr.tab, tab.id);
    assert.equal(thr.comments.length, 1);
    assert.equal(thr.comments[0]!.author.uuid, ada.uuid);
    assert.ok(thr.comments[0]!.mentions.some((m) => m.uuid === si.uuid), 'the comment knows it mentions the Silicon');
    assert.ok(typeof thr.quote === 'string' && thr.quote.length > 0, 'the thread keeps the cells\' text');
  });

  j.step('the Silicon gets calc.mention.received with enough to act on', async () => {
    mention = await w.ting.wait({ for: si.uuid, type: 'calc.mention.received', after: since });
    const d = mention.data;
    assert.equal(d.sheet?.id, sheet.id, 'the sheet');
    assert.equal(d.sheet?.title, 'Sales 2026');
    assert.equal(d.tab?.id, tab.id, 'the tab');
    assert.equal(d.tab?.name, tab.name);
    assert.equal(d.range, 'E2:E37', 'the cells the comment is on');
    assert.ok(typeof d.quote === 'string' && d.quote.length > 0, 'what the cells showed');
    assert.equal(d.thread_id, thr.id, 'the thread');
    assert.equal(d.comment?.id, thr.comments[0]!.id, 'the comment');
    assert.match(String(d.comment?.body), /why did March drop\?/);
    assert.equal(d.comment?.author?.id, ada.id, 'who asked, by c: id');
    assert.ok(String(d.url ?? '').includes(sheet.id), `a link to it (${d.url})`);
    assert.ok(String(d.cli ?? '').includes(thr.id), `the command to open it (${d.cli})`);
    assert.ok(d.copy === null || d.copy === undefined, 'not on a copy');
    j.note(`data: ${JSON.stringify(d).slice(0, 400)}`);
  });

  j.check('the owner is not told about a comment the owner wrote (no calc.comment.created)', async () => {
    await w.ting.none({ for: ada.uuid, type: 'calc.comment.created', after: since }, 2500);
  });

  j.check('the Silicon reads the commented range with the CLI (calc read … --format csv)', async () => {
    const csv = await si.cli.text(['read', mention.data.sheet.id, 'E1:E37', '--tab', mention.data.tab.name, '--format', 'csv']);
    const lines = csv.trim().split(/\r?\n/);
    assert.equal(lines.length, 37, `${lines.length} lines`);
    assert.equal(lines[0], 'revenue');
    assert.equal(lines[7], String(revenueOf('March', 'EU')), 'E8 is EU in March');
  });

  j.check('the Silicon reads the whole table with the CLI as JSON (calc --json read)', async () => {
    const out = await si.cli.json(['read', sheet.id, 'A1:E37', '--tab', tab.name]);
    assert.ok(JSON.stringify(out).includes('September'), 'the JSON has the cells');
  });

  j.step('the Silicon reads the table over HTTP as the same Silicon', async () => {
    const values = await si.api.values(sheet.id, tab.id, 'A1:E37');
    sameGrid(values, salesGrid(), 'what the Silicon reads');
  });

  const quoted = () => `"${tab.name.replace(/"/g, '""')}"`;
  const sql = () => `select month, m, sum(revenue) as revenue from ${quoted()} group by month, m order by m`;

  viaCli(j, 'the Silicon answers with SQL into a new tab "By month"', 'calc run sql … --new-tab', {
    cli: () => si.cli.json(['run', 'sql', sheet.id, '-c', sql(), '--new-tab', sqlTab]),
    http: async () => {
      if (await findTab(si.api, sheet.id, sqlTab)) return null;
      const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'sql', code: sql(), into: { new_tab: sqlTab }, keep_updated: false } });
      assert.deepEqual(r.columns, ['month', 'm', 'revenue']);
      assert.equal(r.row_count, 12);
      assert.equal(r.truncated, false);
      assert.ok(r.version !== null, 'writing a new tab makes a version');
      assert.equal(r.tab?.name, sqlTab);
      return r;
    },
    then: async () => {
      const t = await findTab(ada.api, sheet.id, sqlTab);
      assert.ok(t, `the sheet has a tab "${sqlTab}"`);
      assert.equal(t.code?.language, 'sql', 'the tab remembers the code that made it');
      assert.match(String(t.code?.code), /group by month/i);
      assert.equal(t.code?.run_by?.uuid, si.uuid, 'and who ran it');
      const values = await ada.api.values(sheet.id, t.id, 'A1:C13');
      sameGrid(values, [['month', 'm', 'revenue'], ...revenueByMonth().map((r) => [r.month, r.m, r.revenue])], 'revenue by month');
    },
  });

  const python = () =>
    [
      `df = calc.tab(${JSON.stringify(tab.name)})`,
      'two = df[df["month"].isin(["February", "March"])]',
      'p = two.pivot_table(index="region", columns="month", values="revenue", aggfunc="sum").reset_index()',
      'p["change"] = p["March"] - p["February"]',
      'result = p[["region", "February", "March", "change"]].sort_values("change")',
      'print("regions:", len(result))',
    ].join('\n');

  viaCli(j, 'the Silicon compares March with February in Python into a new tab', 'calc run python … --new-tab', {
    cli: () => si.cli.json(['run', 'python', sheet.id, '-c', python(), '--new-tab', pyTab]),
    http: async () => {
      if (await findTab(si.api, sheet.id, pyTab)) return null;
      const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'python', code: python(), into: { new_tab: pyTab } } });
      assert.match(r.stdout, /regions: 3/, 'print goes to stdout');
      return r;
    },
    then: async () => {
      const t = await findTab(ada.api, sheet.id, pyTab);
      assert.ok(t, `the sheet has a tab "${pyTab}"`);
      assert.equal(t.code?.language, 'python');
      assert.equal(t.code?.run_by?.uuid, si.uuid);
      const values = await ada.api.values(sheet.id, t.id, 'A1:D4');
      sameGrid(values.map((r) => r.map((v) => (typeof v === 'number' ? Math.round(v) : v))), [
        ['region', 'February', 'March', 'change'],
        ['EU', 12800, 3900, -8900],
        ['APAC', 7900, 8100, 200],
        ['US', 21500, 21900, 400],
      ], 'March against February');
    },
    timeoutMs: 180_000,
  });

  j.check('the Carbon saw the Silicon\'s changes as they happened', async () => {
    if (!live) throw new Error('the live stream could not be opened (see that check)');
    const e = await live.next((x) => x.event === 'change' && x.data?.by?.uuid === si.uuid, 15_000, "a change event by the Silicon");
    assert.equal(e.data.by.id, si.id, 'the change says who made it, by si: id');
    assert.ok(Array.isArray(e.data.changes) && e.data.changes.length > 0);
  });

  j.step('every cell the Silicon wrote says it was the Silicon', async () => {
    const t = (await findTab(ada.api, sheet.id, sqlTab))!;
    const cells = await ada.api.cells(sheet.id, t.id, 'A1:C13');
    assert.ok(cells.cells.length >= 39);
    for (const c of cells.cells) assert.equal(c.by, si.uuid, `${c.a} was last changed by ${c.by}`);
    assert.equal(cells.accounts[si.uuid]?.id, si.id);
    assert.equal(cells.accounts[si.uuid]?.kind, 'silicon');
  });

  const replyBody = 'March fell because EU revenue dropped from 12,800 to 3,900 (-8,900) while US and APAC grew. The numbers are in the tabs By month and March vs February.';

  viaCli(j, 'the Silicon replies in the thread', 'calc comments reply', {
    cli: () => si.cli.json(['comments', 'reply', thr.id, replyBody]),
    http: async () => {
      const t = await thread(si.api, sheet.id, thr.id);
      if (t.comments.some((c) => c.author.uuid === si.uuid)) return null;
      return si.api.post(`/v1/comments/${thr.id}/replies`, { json: { body: replyBody } }, 201);
    },
    then: async () => {
      const t = await thread(ada.api, sheet.id, thr.id);
      assert.equal(t.comments.length, 2, 'the question and the answer');
      const reply = t.comments[1]!;
      assert.equal(reply.author.uuid, si.uuid);
      assert.equal(reply.author.id, si.id);
      assert.equal(reply.body, replyBody);
      replyId = reply.id;
    },
  });

  j.step('the Carbon gets calc.comment.replied', async () => {
    const t = await w.ting.wait({ for: ada.uuid, type: 'calc.comment.replied', after: since });
    assert.equal(t.data.thread_id, thr.id);
    assert.equal(t.data.sheet?.id, sheet.id);
    assert.equal(t.data.comment?.author?.id, si.id, 'the reply is the Silicon\'s');
    assert.match(String(t.data.comment?.body), /EU revenue dropped/);
  });

  j.check('the replier is not told about its own reply', async () => {
    await w.ting.none({ for: si.uuid, type: 'calc.comment.replied', after: since }, 2000);
  });

  j.check('the Silicon reads the thread with the CLI (calc comments show)', async () => {
    const out = await si.cli.json(['comments', 'show', thr.id]);
    const text = JSON.stringify(out);
    assert.ok(text.includes('why did March drop?') && text.includes('EU revenue dropped'), 'both comments');
  });

  j.check('the Carbon reacts to the reply', async () => {
    await ada.api.post(`/v1/comments/${replyId}/reactions`, { json: { emoji: '👍' } });
    const t = await thread(ada.api, sheet.id, thr.id);
    const r = t.comments[1]!.reactions.find((x) => x.emoji === '👍');
    assert.ok(r && r.accounts.some((a) => a.uuid === ada.uuid), 'the reaction is on the reply, by the Carbon');
  });

  viaCli(j, 'the Silicon resolves the thread', 'calc comments resolve', {
    cli: () => si.cli.json(['comments', 'resolve', thr.id]),
    http: async () => {
      const t = await thread(si.api, sheet.id, thr.id);
      if (t.status === 'resolved') return t;
      return si.api.post(`/v1/comments/${thr.id}/resolve`, {});
    },
    then: async () => {
      const t = await thread(ada.api, sheet.id, thr.id);
      assert.equal(t.status, 'resolved');
      assert.equal(t.resolved_by?.uuid, si.uuid);
    },
  });

  j.step('the Carbon, who started the thread, gets calc.thread.resolved', async () => {
    const t = await w.ting.wait({ for: ada.uuid, type: 'calc.thread.resolved', after: since });
    assert.equal(t.data.thread_id, thr.id);
    assert.equal(t.data.sheet?.id, sheet.id);
  });

  j.check('a resolved thread can be reopened', async () => {
    const t = await ada.api.post<Thread>(`/v1/comments/${thr.id}/reopen`, {});
    assert.equal(t.status, 'open');
    const open = await ada.api.get(`/v1/sheets/${sheet.id}/comments`, { query: { status: 'open' } });
    assert.ok(open.threads.some((x: Thread) => x.id === thr.id));
  });

  let siComment: Thread;
  j.step('the Silicon comments on the sheet; the owner gets calc.comment.created', async () => {
    since = await w.ting.seq();
    const t = (await findTab(si.api, sheet.id, sqlTab))!;
    siComment = await si.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: t.id, range: 'C4', body: 'March is the only month that went down.' } }, 201);
    const ting = await w.ting.wait({ for: ada.uuid, type: 'calc.comment.created', after: since });
    assert.equal(ting.data.thread_id, siComment.id);
    assert.equal(ting.data.comment?.author?.id, si.id);
    assert.equal(ting.data.range, 'C4');
  });

  j.check('a comment is edited by its author only', async () => {
    const id = siComment.comments[0]!.id;
    const edited = await si.api.patch(`/v1/comments/${id}`, { json: { body: 'March is the only month that went down (EU).' } });
    assert.ok(edited.edited_at, 'the comment says it was edited');
    await ada.api.refused('PATCH', `/v1/comments/${id}`, { json: { body: 'not mine' } }, 403, 'not_allowed');
  });

  const totalSql = () => `select sum(revenue) as total from ${quoted()}`;
  j.step('the Silicon makes a total that keeps itself updated', async () => {
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'sql', code: totalSql(), into: { new_tab: 'Total' }, keep_updated: true } });
    assert.equal(r.tab?.code?.keep_updated, true);
    const t = (await findTab(si.api, sheet.id, 'Total'))!;
    assert.equal(num((await si.api.values(sheet.id, t.id, 'A2'))[0]![0]), totalRevenue());
  });

  j.step('when the Carbon corrects March, the total follows', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E8', input: '4900' }] }]);
    const t = (await findTab(ada.api, sheet.id, 'Total'))!;
    await eventually('the keep-updated total to rerun', async () => num((await ada.api.values(sheet.id, t.id, 'A2'))[0]![0]) === totalRevenue() + 1000, 45_000, 500);
    const after = (await findTab(ada.api, sheet.id, 'Total'))!;
    assert.equal(after.code?.run_by?.uuid, si.uuid, 'it reruns as the Silicon who set it up');
  });

  j.check('JavaScript runs on the sheet too', async () => {
    const code = `return calc.tab(${JSON.stringify(tab.name)}).filter((r) => r.region === "EU").map((r) => ({ month: r.month, revenue: r.revenue }));`;
    const r = await si.api.post(`/v1/sheets/${sheet.id}/run`, { json: { language: 'javascript', code, into: null } });
    assert.equal(r.row_count, 12);
    assert.deepEqual(r.columns, ['month', 'revenue']);
    const march = r.rows.find((x: unknown[]) => x[0] === 'March');
    assert.equal(num(march?.[1]), 4900);
  });

  j.check('code that fails says why (code_failed)', async () => {
    const e = await si.api.refused('POST', `/v1/sheets/${sheet.id}/run`, { json: { language: 'python', code: 'raise ValueError("no such column")' } }, 422, 'code_failed');
    assert.match(String(e.details?.stderr), /no such column/);
  });

  j.check('a comment stays on its cells as rows are inserted above them', async () => {
    await ada.api.edit(sheet.id, [{ type: 'insert_rows', tab: tab.id, at: 1, count: 2 }]);
    const t = await thread(ada.api, sheet.id, thr.id);
    assert.equal(t.range, 'E4:E39', 'the thread moved down two rows with its cells');
    assert.equal(t.original_range, 'E2:E37');
    assert.equal(t.quote, thr.quote, 'the quote is what the cells showed when it was made');
    const cells = await ada.api.cells(sheet.id, tab.id, 'E10');
    assert.equal(plain(at(cells, 'E10')), 4900, 'the cells moved too');
  });
});
