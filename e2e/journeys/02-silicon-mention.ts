// A Carbon mentions a Silicon on a range; authenticated CLI/HTTP reads, supplied-result commits,
// live attribution, comments, mentions and protected persistence are verified here.
// Result tables below are fixtures supplied by this API client; JavaScript execution is proved by the browser suite.

import assert from 'node:assert/strict';
import { type Sheet, type Tab, type Thread, CalcApi, at, plain } from '../lib/calc.ts';
import { LiveEvents } from '../lib/events.ts';
import { revenueByMonth, revenueOf, salesGrid, totalRevenue } from '../lib/fixtures.ts';
import { journey, sleep } from '../lib/journey.ts';
import { salesSheet, tabsOf } from '../lib/sheets.ts';
import { needs, num, sameGrid, viaCli } from '../lib/steps.ts';
import type { TingView } from '../lib/ting.ts';
import { type CarbonPerson, type SiliconPerson, world } from '../lib/world.ts';

async function findTab(api: CalcApi, sheet: string, name: string): Promise<Tab | undefined> {
  return (await tabsOf(api, sheet)).find((t) => t.name === name);
}

async function thread(api: CalcApi, sheet: string, id: string): Promise<Thread> {
  const list = await api.get<{ threads: Thread[] }>(`/v1/sheets/${sheet}/comments`, { query: { status: 'all' } });
  const t = list.threads.find((x) => x.id === id);
  if (!t) throw new Error(`thread ${id} is not among the sheet's ${list.threads.length} threads`);
  return t;
}

const monthlyTab = 'By month';
const comparisonTab = 'March vs February';

export default journey('A Carbon mentions a Silicon on a range; the Silicon does the analysis', (j) => {
  const w = world();
  let ada: CarbonPerson;
  let si: SiliconPerson;
  let sheet: Sheet;
  let tab: { id: number; name: string };
  let live: LiveEvents | null = null;
  let watch: ReturnType<SiliconPerson['cli']['start']> | null = null;
  let thr: Thread;
  let mention: TingView;
  let replyId: string;
  let since = 0;
  let cliSignedIn = false;

  j.cleanup(async () => {
    live?.close();
    watch?.stop();
  });

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

  salesSheet(j, 'the Carbon', () => ada.api, 'Sales 2026', (o) => {
    sheet = o.sheet;
    tab = o.tab;
  });

  j.step('the Carbon finds the Silicon by its si: id', async () => {
    const found = await ada.api.get('/v1/accounts/lookup', { query: { id: si.id } });
    assert.equal(found.uuid, si.uuid);
    assert.equal(found.kind, 'silicon');
    assert.equal(found.id, si.id);
    await ada.api.refused('GET', '/v1/accounts/lookup', { query: { id: 'si:nobodyhere0000' } }, 404);
  });

  j.step('the Carbon invites the Silicon as an editor by its si: id', async () => {
    since = await w.ting.seq();
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
    j.note(`key ${t.key}; data: ${JSON.stringify(t.data).slice(0, 300)}`);
  });

  j.step('the Silicon sees the sheet among those shared with it', async () => {
    const list = await si.api.get('/v1/sheets', { query: { scope: 'shared' } });
    const s = list.sheets.find((x: Sheet) => x.id === sheet.id);
    assert.ok(s, 'the sheet is listed as shared with the Silicon');
    assert.equal(s.role, 'editor');
    assert.equal(s.owner.id, ada.id);
  });

  j.check('the Silicon lists it with the CLI (calc ls --shared)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const out = await si.cli.json(['ls', '--shared']);
    assert.ok(JSON.stringify(out).includes(sheet.id), 'calc ls --shared lists the sheet');
  });

  j.check('the Carbon watches the sheet live (GET /v1/sheets/{id}/events)', async () => {
    const v = (await ada.api.get(`/v1/sheets/${sheet.id}/tabs`)).version;
    live = await LiveEvents.open(ada.api, CalcApi.line(sheet.id), v);
  });

  j.check('the Silicon waits for mentions with the CLI and shows itself on the sheet (calc watch --mentions --here)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    watch = si.cli.start(['--json', 'watch', sheet.id, '--only', 'comment', '--mentions', '--count', '1', '--timeout', '90', '--here', `${tab.name}!A1`], { timeoutMs: 100_000 });
    if (live) {
      // The Carbon sees the Silicon on the sheet: then the watch is connected.
      const p = await live.next((e) => e.event === 'presence' && (e.data?.viewers ?? []).some((v: any) => v.account?.uuid === si.uuid), 20_000, 'a presence event naming the Silicon');
      j.note(`the Carbon sees ${p.data.viewers.map((v: any) => v.account?.id).join(', ')} on the sheet`);
    } else {
      await sleep(2000);
    }
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

  j.check('the Silicon\'s calc watch saw the mention as it happened', async () => {
    needs(watch, 'calc watch was not started (see that check)');
    const r = await watch!.done;
    assert.equal(r.code, 0, `calc watch exited ${r.code}: ${r.stderr.trim().slice(0, 300)}`);
    const text = watch!.lines.join('\n');
    assert.ok(text.includes(thr.id) || text.includes('why did March drop?'), `calc watch printed the comment (${text.slice(0, 300)})`);
  });

  j.check('the owner is not told about a comment the owner wrote (no calc.comment.created)', async () => {
    await w.ting.none({ for: ada.uuid, type: 'calc.comment.created', after: since }, 2500);
  });

  j.check('the Silicon reads the cells the ting names with the CLI (calc read … --format csv)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const d = mention.data;
    const csv = await si.cli.text(['read', d.sheet.id, d.range, '--tab', d.tab.name, '--format', 'csv']);
    const lines = csv.trim().split(/\r?\n/);
    assert.equal(lines.length, 36, `${lines.length} lines`);
    assert.equal(lines[6], String(revenueOf('March', 'EU')), 'E8 is EU in March');
  });

  j.check('the Silicon reads the table with the comments on it as JSON (calc --json read … --comments)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const out = await si.cli.json(['read', sheet.id, 'A1:E37', '--tab', tab.name, '--comments']);
    const text = JSON.stringify(out);
    assert.ok(text.includes('September'), 'the JSON has the cells');
    assert.ok(text.includes('why did March drop?'), 'and the comment on them');
  });

  j.step('the Silicon reads the table over HTTP as the same Silicon', async () => {
    const values = await si.api.values(sheet.id, tab.id, 'A1:E37');
    sameGrid(values, salesGrid(), 'what the Silicon reads');
  });

  j.step('the Silicon commits supplied monthly results into a new tab', async () => {
    const base_version = (await si.api.get(`${CalcApi.line(sheet.id)}/tabs`)).version;
    const rows = revenueByMonth().map((r) => [r.month, r.m, r.revenue]);
    const code = `const {rows} = await calc.tab(${JSON.stringify(tab.name)}); const months = new Map(); for (const [month,m,region,units,revenue] of rows) { const total = months.get(m) ?? {month,m,revenue:0}; total.revenue += revenue; months.set(m,total); } return [...months.values()].sort((a,b) => a.m-b.m);`;
    const r = await si.api.post(`${CalcApi.line(sheet.id)}/run`, { json: { code, base_version, inputs: [tab.id], columns: ['month', 'm', 'revenue'], rows, into: { new_tab: monthlyTab } } });
    assert.equal(r.row_count, 12);
    assert.ok(r.version > base_version, 'committing results makes one version');
    const t = await findTab(ada.api, sheet.id, monthlyTab);
    assert.ok(t);
    assert.equal(t.code?.language, 'javascript');
    assert.equal(t.code?.code, code, 'the supplied JavaScript source is saved');
    assert.equal(t.code?.run_by?.uuid, si.uuid, 'the server records the authenticated author');
    sameGrid(await ada.api.values(sheet.id, t.id, 'A1:C13'), [['month', 'm', 'revenue'], ...rows], 'supplied monthly results');
  });

  j.step('the Silicon commits supplied March and February comparison results', async () => {
    const base_version = (await si.api.get(`${CalcApi.line(sheet.id)}/tabs`)).version;
    const rows = [['EU', 12800, 3900, -8900], ['APAC', 7900, 8100, 200], ['US', 21500, 21900, 400]];
    const code = `const {rows} = await calc.tab(${JSON.stringify(tab.name)}); return ["EU","APAC","US"].map(region => { const February = rows.find(r => r[2] === region && r[0] === "February")[4]; const March = rows.find(r => r[2] === region && r[0] === "March")[4]; return {region, February, March, change: March-February}; }).sort((a,b) => a.change-b.change);`;
    const r = await si.api.post(`${CalcApi.line(sheet.id)}/run`, { json: { code, base_version, inputs: [tab.id], columns: ['region', 'February', 'March', 'change'], rows, into: { new_tab: comparisonTab } } });
    const t = await findTab(ada.api, sheet.id, comparisonTab);
    assert.ok(t);
    assert.equal(r.tab.id, t.id);
    assert.equal(t.code?.run_by?.uuid, si.uuid);
    sameGrid(await ada.api.values(sheet.id, t.id, 'A1:D4'), [['region', 'February', 'March', 'change'], ...rows], 'supplied comparison results');
  });

  j.check('the Carbon saw the Silicon\'s changes as they happened', async () => {
    needs(live, 'the live stream could not be opened (see that check)');
    const e = await live!.next((x) => x.event === 'change' && x.data?.by?.uuid === si.uuid, 15_000, 'a change event by the Silicon');
    assert.equal(e.data.by.id, si.id, 'the change says who made it, by si: id');
    assert.ok(Array.isArray(e.data.changes) && e.data.changes.length > 0);
  });

  j.step('every cell the Silicon wrote says it was the Silicon', async () => {
    const t = (await findTab(ada.api, sheet.id, monthlyTab))!;
    const cells = await ada.api.cells(sheet.id, t.id, 'A1:C13');
    assert.ok(cells.cells.length >= 39);
    for (const c of cells.cells) assert.equal(c.by, si.uuid, `${c.a} was last changed by ${c.by}`);
    assert.equal(cells.accounts[si.uuid]?.id, si.id);
    assert.equal(cells.accounts[si.uuid]?.kind, 'silicon');
  });

  const replyBody = 'March fell because EU revenue dropped from 12,800 to 3,900 (-8,900) while US and APAC grew. The numbers are in the tabs By month and March vs February.';

  viaCli(j, 'the Silicon replies in the thread', 'calc comments reply', {
    cli: async () => {
      needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
      return si.cli.json(['comments', 'reply', thr.id, replyBody]);
    },
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
    j.note(`data: ${JSON.stringify(t.data).slice(0, 300)}`);
  });

  j.check('the replier is not told about its own reply', async () => {
    await w.ting.none({ for: si.uuid, type: 'calc.comment.replied', after: since }, 2000);
  });

  j.check('the Silicon reads the thread with the CLI (calc comments show)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
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
    cli: async () => {
      needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
      return si.cli.json(['comments', 'resolve', thr.id]);
    },
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

  j.check('comments go on a row, a column and a whole tab too', async () => {
    const row = await ada.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: tab.id, range: '8:8', body: 'This row is the odd one.' } }, 201);
    assert.equal(row.range, '8:8');
    const col = await ada.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: tab.id, range: 'D:D', body: 'Units are pieces, not boxes.' } }, 201);
    assert.equal(col.range, 'D:D');
    const whole = await ada.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: tab.id, range: null, body: 'This tab is the export from October.' } }, 201);
    assert.equal(whole.range, null, 'a comment on the whole tab has no range');
    const list = await ada.api.get<{ threads: Thread[] }>(`/v1/sheets/${sheet.id}/comments`, { query: { status: 'open', tab: tab.id } });
    for (const t of [row, col, whole]) assert.ok(list.threads.some((x) => x.id === t.id), `the tab's threads include ${t.range ?? 'the whole tab'}`);
  });

  let siComment: Thread;
  let byMonth: Tab;
  viaCli(j, 'the Silicon comments on its own tab; the owner gets calc.comment.created', 'calc comments add', {
    cli: async () => {
      needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
      since = await w.ting.seq();
      return si.cli.json(['comments', 'add', sheet.id, `'${monthlyTab}'!C4`, 'March is the only month that went down.']);
    },
    http: async () => {
      const t = (await findTab(si.api, sheet.id, monthlyTab))!;
      const list = await si.api.get<{ threads: Thread[] }>(`/v1/sheets/${sheet.id}/comments`, { query: { status: 'all', tab: t.id } });
      const made = list.threads.find((x) => x.comments[0]?.author.uuid === si.uuid);
      if (made) return made;
      since = await w.ting.seq();
      return si.api.post<Thread>(`/v1/sheets/${sheet.id}/comments`, { json: { tab: t.id, range: 'C4', body: 'March is the only month that went down.' } }, 201);
    },
    then: async () => {
      byMonth = (await findTab(si.api, sheet.id, monthlyTab))!;
      const list = await ada.api.get<{ threads: Thread[] }>(`/v1/sheets/${sheet.id}/comments`, { query: { status: 'all', tab: byMonth.id } });
      siComment = list.threads.find((x) => x.comments[0]?.author.uuid === si.uuid)!;
      assert.ok(siComment, `the Silicon's comment is on ${monthlyTab}`);
      assert.equal(siComment.range, 'C4');
      const ting = await w.ting.wait({ for: ada.uuid, type: 'calc.comment.created', after: since });
      assert.equal(ting.data.thread_id, siComment.id);
      assert.equal(ting.data.comment?.author?.id, si.id);
      assert.equal(ting.data.range, 'C4');
    },
  });

  j.check('a comment is edited by its author only', async () => {
    const id = siComment.comments[0]!.id;
    const edited = await si.api.patch(`/v1/comments/${id}`, { json: { body: 'March is the only month that went down (EU).' } });
    assert.ok(edited.edited_at, 'the comment says it was edited');
    await ada.api.refused('PATCH', `/v1/comments/${id}`, { json: { body: 'not mine' } }, 403, 'not_allowed');
  });

  let totalTab: Tab;
  let totalVersion: number;
  j.step('the Silicon commits a supplied total with JavaScript provenance', async () => {
    const base_version = (await si.api.get(`${CalcApi.line(sheet.id)}/tabs`)).version;
    const r = await si.api.post(`${CalcApi.line(sheet.id)}/run`, { json: { code: `const {rows} = await calc.tab(${JSON.stringify(tab.name)}); return [{total: rows.reduce((n,r) => n+r[4],0)}];`, base_version, inputs: [tab.id], columns: ['total'], rows: [[totalRevenue()]], into: { new_tab: 'Total' } } });
    totalTab = r.tab;
    totalVersion = r.version;
    assert.equal(num((await si.api.values(sheet.id, totalTab.id, 'A2'))[0]![0]), totalRevenue());
  });

  j.step('a source edit leaves committed results unchanged until the browser runs again', async () => {
    await ada.api.edit(sheet.id, [{ type: 'set', tab: tab.id, cells: [{ a: 'E8', input: '4900' }] }]);
    assert.equal(num((await ada.api.values(sheet.id, totalTab.id, 'A2'))[0]![0]), totalRevenue());
    const t = byMonth ?? (await findTab(ada.api, sheet.id, monthlyTab))!;
    const march = revenueByMonth().find((r) => r.month === 'March')!;
    assert.equal(num((await ada.api.values(sheet.id, t.id, 'A4:C4'))[0]![2]), march.revenue);
  });

  j.check('a result commit from an older snapshot is rejected atomically', async () => {
    await si.api.refused('POST', `${CalcApi.line(sheet.id)}/run`, { json: { code: totalTab.code!.code, base_version: totalVersion, inputs: [tab.id], columns: ['total'], rows: [[0]], into: { tab: totalTab.id, at: 'A1', replace: true } } }, 409, 'conflict');
    assert.equal(num((await ada.api.values(sheet.id, totalTab.id, 'A2'))[0]![0]), totalRevenue(), 'the stale result did not overwrite the committed value');
  });

  j.check('committing supplied results does not execute their JavaScript source on the server', async () => {
    const base_version = (await si.api.get(`${CalcApi.line(sheet.id)}/tabs`)).version;
    const code = 'throw new Error("The API must not execute this source");';
    const r = await si.api.post(`${CalcApi.line(sheet.id)}/run`, { json: { code, base_version, inputs: [], columns: ['supplied'], rows: [[123]], into: { new_tab: 'Supplied result' } } });
    assert.equal(num((await ada.api.values(sheet.id, r.tab.id, 'A2'))[0]![0]), 123);
    assert.equal(r.tab.code.code, code);
  });

  j.check('the Silicon finds the sheet by a word in its cells with the CLI (calc search)', async () => {
    needs(cliSignedIn, 'the CLI is not signed in (see the login check)');
    const out = await si.cli.json(['search', 'September']);
    assert.ok(JSON.stringify(out).includes(sheet.id), 'calc search finds the sheet');
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
