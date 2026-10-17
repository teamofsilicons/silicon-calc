/**
 * One open line (the main sheet or a copy) in this browser: its tabs and what is loaded of them (store.ts), the shared
 * styles and accounts, the live event stream (changes, presence, comments, copies) with reconnection from the last
 * version seen, the queue of edits this browser sends (one at a time, each with the version it was looking at), the
 * inputs shown before the service answers, and the overwrite notices ("your change replaced someone else's").
 *
 * Framework-free: React reads it through `subscribe` and the version counter `revision` (lib/sheet/react.ts).
 */
import { ApiError } from "../errors";
import * as calc from "../calc/api";
import type { Account, CellsAnswer, Change, ChangeEvent, Copy, Edit, EditAnswer, Overwrite, Sheet, Style, Tab, Thread, Viewer } from "../calc/types";
import { a1, cellKey, MAX_COLS, MAX_ROWS, parseRange, type Rect, rectsIntersect } from "./address";
import { blockFromId, blockId, blockRect, blocksFor, changeRects, TabData } from "./store";

export interface SessionOptions {
  line: calc.LineRef;
  sheet: Sheet;
  tabs: Tab[];
  version: number;
  /** The signed-in account (null: reading a public sheet signed out). */
  me: Account | null;
  /** Open a past version read-only (no live stream, no edits). */
  atVersion?: number | null;
  /** True when this line cannot be edited (a viewer or commenter, a deleted or merged copy). */
  readOnly?: boolean;
}

export type LiveState = "connecting" | "live" | "reconnecting" | "offline";

export interface PendingSet {
  tab: number;
  row: number;
  col: number;
  input: string;
}

export interface EditOutcome {
  answer: EditAnswer | null;
  error: ApiError | null;
}

type Listener = () => void;

const MAX_INFLIGHT = 6;
/** Recent change events kept to bring an older block answer up to date. */
const EVENT_LOG = 300;

interface LoggedChange {
  version: number;
  tab: number;
  rects: Rect[];
  reload: boolean;
}

export class SheetSession {
  readonly line: calc.LineRef;
  sheet: Sheet;
  tabs: Tab[];
  version: number;
  readonly me: Account | null;
  readonly atVersion: number | null;
  readOnly: boolean;
  readonly clientId: string;
  readonly styles = new Map<number, Style>();
  readonly accounts = new Map<string, Account>();
  viewers: Viewer[] = [];
  live: LiveState = "connecting";
  /** The last failure reading cells (shown quietly by the editor; blocks are tried again). */
  lastError: ApiError | null = null;
  /** Bumped on every change anyone can see; React keys snapshots on it. */
  revision = 0;

  /** Raised when a comment thread changes (created, replied, resolved…). */
  onThread: ((thread: Thread) => void) | null = null;
  /** Raised when a copy of this line is made, merged or deleted. */
  onCopy: ((copy: Copy) => void) | null = null;
  /** Raised when one of this browser's edits replaced someone else's change. */
  onOverwrite: ((overwrites: Overwrite[]) => void) | null = null;
  /** Raised when an edit failed (the service's words), with the inputs it carried. */
  onEditError: ((error: ApiError, pending: PendingSet[]) => void) | null = null;
  /** Raised when the sheet itself changed (title, sharing, trash) or the tab list did. */
  onMeta: (() => void) | null = null;
  /** Raised for every change event (the history panel refreshes on it). */
  onChange: ((event: ChangeEvent) => void) | null = null;

  private readonly data = new Map<number, TabData>();
  private readonly listeners = new Set<Listener>();
  private readonly inflight = new Map<string, AbortController>();
  private readonly queue: Array<{ tab: number; id: number }> = [];
  private wanted = new Map<number, Set<number>>();
  /** Blocks whose last read failed, and when they may be tried again (ms, performance.now()). */
  private failed = new Map<string, number>();
  private lastView: { tab: number; visible: Rect[]; margin: Rect[] } | null = null;
  private retryView: ReturnType<typeof setTimeout> | null = null;
  private log: LoggedChange[] = [];
  private source: EventSource | null = null;
  private retry = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  private editChain: Promise<unknown> = Promise.resolve();
  private editSeq = 0;
  private versionWaiters: Array<{ version: number; resolve: () => void }> = [];
  private presenceTimer: ReturnType<typeof setInterval> | null = null;
  private presenceDebounce: ReturnType<typeof setTimeout> | null = null;
  private lastPresence: { tab: number; range: string } | null = null;
  private notifyScheduled = false;

  constructor(options: SessionOptions) {
    this.line = options.line;
    this.sheet = options.sheet;
    this.tabs = [...options.tabs].sort((a, b) => a.index - b.index);
    this.version = options.version;
    this.me = options.me;
    this.atVersion = options.atVersion ?? null;
    this.readOnly = options.readOnly ?? false;
    this.clientId = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `c${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;
    if (options.me) this.accounts.set(options.me.uuid, options.me);
    for (const tab of this.tabs) this.data.set(tab.id, new TabData(tab));
  }

  /* ---- subscribing ----------------------------------------------------------------------------------------------- */

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Tells every listener, at most once per animation frame. */
  notify(): void {
    this.revision++;
    if (this.notifyScheduled) return;
    this.notifyScheduled = true;
    const run = () => {
      this.notifyScheduled = false;
      for (const listener of [...this.listeners]) listener();
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
    else queueMicrotask(run);
  }

  /* ---- tabs ------------------------------------------------------------------------------------------------------ */

  tab(id: number): TabData | null {
    return this.data.get(id) ?? null;
  }

  firstTab(): Tab | null {
    return this.tabs[0] ?? null;
  }

  private setTabs(tabs: Tab[]): void {
    this.tabs = [...tabs].sort((a, b) => a.index - b.index);
    for (const tab of this.tabs) {
      const data = this.data.get(tab.id);
      if (data) data.setTab(tab);
      else this.data.set(tab.id, new TabData(tab));
    }
    for (const id of [...this.data.keys()]) if (!this.tabs.some(tab => tab.id === id)) this.data.delete(id);
  }

  /** Reads the tab list again (after a tab was added, renamed, moved or deleted). */
  async refreshTabs(): Promise<void> {
    const answer = this.atVersion !== null ? await calc.versionTabs(this.line, this.atVersion) : await calc.getTabs(this.line);
    this.setTabs(answer.tabs);
    this.notify();
  }

  /* ---- loading cells --------------------------------------------------------------------------------------------- */

  /**
   * The grid says what it shows: the blocks under `visible` load first, then the ones within `margin` around it (both
   * may be several rectangles: frozen rows and columns are separate panes). Blocks no longer wanted stop loading; the
   * oldest far-away ones are dropped.
   */
  view(tab: number, visible: Rect | Rect[], margin: Rect | Rect[]): void {
    const data = this.data.get(tab);
    if (!data) return;
    const visibleRects = Array.isArray(visible) ? visible : [visible];
    const marginRects = Array.isArray(margin) ? margin : [margin];
    this.lastView = { tab, visible: visibleRects, margin: marginRects };
    const now = performance.now();
    const first: number[] = [];
    const seen = new Set<number>();
    for (const r of visibleRects) for (const b of blocksFor(r)) {
      const id = blockId(b.br, b.bc);
      if (!seen.has(id)) {
        seen.add(id);
        first.push(id);
      }
    }
    const around: number[] = [];
    for (const r of marginRects) for (const b of blocksFor(r)) {
      const id = blockId(b.br, b.bc);
      if (!seen.has(id)) {
        seen.add(id);
        around.push(id);
      }
    }
    const wanted = new Set([...first, ...around]);
    this.wanted.set(tab, wanted);
    for (const id of first) data.touch(id, now);
    // Stop what is no longer wanted (scrolled far past before it arrived).
    for (const [key, controller] of this.inflight) {
      const [t, id] = key.split(":").map(Number) as [number, number];
      if (t === tab && !wanted.has(id)) {
        controller.abort();
        this.inflight.delete(key);
      }
    }
    this.queue.length = 0;
    for (const id of [...first, ...around]) {
      const key = `${tab}:${id}`;
      if ((this.failed.get(key) ?? 0) > now) continue;
      if ((!data.loaded.has(id) || data.stale.has(id)) && !this.inflight.has(key)) this.queue.push({ tab, id });
    }
    data.evict(wanted);
    this.pump();
  }

  /** True while blocks the grid shows are still being read. */
  loading(tab: number): boolean {
    const data = this.data.get(tab);
    const wanted = this.wanted.get(tab);
    if (!data || !wanted) return false;
    for (const id of wanted) if (!data.loaded.has(id) || data.stale.has(id)) return true;
    return false;
  }

  private pump(): void {
    while (this.inflight.size < MAX_INFLIGHT && this.queue.length) {
      const next = this.queue.shift()!;
      void this.loadBlock(next.tab, next.id);
    }
  }

  private async loadBlock(tab: number, id: number): Promise<void> {
    const data = this.data.get(tab);
    if (!data) return;
    const key = `${tab}:${id}`;
    const controller = new AbortController();
    this.inflight.set(key, controller);
    const area = blockRect(blockFromId(id));
    try {
      const answer = await calc.getCells(this.line, tab, a1(area), { version: this.atVersion, signal: controller.signal });
      if (controller.signal.aborted) return;
      this.failed.delete(key);
      this.takeAnswer(data, answer, area, id);
    } catch (error) {
      if (controller.signal.aborted) return;
      const failure = ApiError.from(error);
      // A tab that went away (deleted elsewhere): the tab list catches up.
      if (failure.status === 404) void this.refreshTabs().catch(() => undefined);
      // Anything else is tried again in a few seconds, when the block is still in view; until then it shows as loading.
      this.failed.set(key, performance.now() + 3_000);
      this.lastError = failure;
      if (!this.retryView && !this.closed) {
        this.retryView = setTimeout(() => {
          this.retryView = null;
          const last = this.lastView;
          if (last && !this.closed) this.view(last.tab, last.visible, last.margin);
        }, 3_100);
      }
      this.notify();
    } finally {
      if (this.inflight.get(key) === controller) this.inflight.delete(key);
      this.pump();
    }
  }

  /**
   * Takes in a cells answer read elsewhere (the server reads the first block of the first tab, so the grid paints at
   * once): every block it covers whole counts as loaded.
   */
  seed(answer: CellsAnswer): void {
    const data = this.data.get(answer.tab?.id ?? -1) ?? (this.tabs[0] ? this.data.get(this.tabs[0].id) : undefined);
    if (!data || !answer.range) return;
    const area = parseRange(answer.range, MAX_ROWS, MAX_COLS);
    if (!area) return;
    data.applyAnswer(answer, area, this.styles, this.accounts);
    const now = performance.now();
    for (const b of blocksFor(area)) {
      const r = blockRect(b);
      if (r.r1 >= area.r1 && r.r2 <= area.r2 && r.c1 >= area.c1 && r.c2 <= area.c2) data.markLoaded(blockId(b.br, b.bc), now);
    }
    this.notify();
  }

  private takeAnswer(data: TabData, answer: CellsAnswer, area: Rect, id: number): void {
    data.applyAnswer(answer, area, this.styles, this.accounts);
    data.markLoaded(id, performance.now());
    // Changes newer than this answer that touched the block: read it again (it is older than what the stream said).
    if (this.atVersion === null && answer.version < this.version) {
      const newer = this.log.some(entry => entry.version > answer.version && entry.tab === data.tab.id && (entry.reload || entry.rects.some(r => rectsIntersect(r, area))));
      if (newer) {
        data.stale.add(id);
        if (this.wanted.get(data.tab.id)?.has(id)) this.queue.push({ tab: data.tab.id, id });
      }
    }
    // `version` stays what the stream has brought: it is where a reconnect starts and what edits say they saw.
    this.notify();
  }

  /** Reads a range now (used after an edit when the stream is slow, and for a reload). */
  async reload(tab: number, range: Rect | null = null): Promise<void> {
    const data = this.data.get(tab);
    if (!data) return;
    if (!range) {
      // Everything held is out of date: it stays on screen until the new answers replace it (no flash of empty cells).
      for (const id of data.loaded.keys()) data.stale.add(id);
      const wanted = this.wanted.get(tab);
      if (wanted) for (const id of wanted) if (!this.inflight.has(`${tab}:${id}`)) this.queue.push({ tab, id });
      this.pump();
      this.notify();
      return;
    }
    const answer = await calc.getCells(this.line, tab, a1(range), { version: this.atVersion });
    data.applyAnswer(answer, range, this.styles, this.accounts);
    this.notify();
  }

  /** Every cell's values in a range, loaded or not (for copying a large selection, filling, jumping). */
  async values(tab: number, range: Rect): Promise<Array<Array<number | string | boolean | null>>> {
    const answer = await calc.getValues(this.line, tab, a1(range));
    return answer.values;
  }

  /* ---- live ------------------------------------------------------------------------------------------------------ */

  /** Opens the event stream (from the version this page was rendered at). */
  connect(): void {
    if (this.atVersion !== null || this.closed || typeof EventSource === "undefined") {
      this.live = "offline";
      return;
    }
    this.source?.close();
    this.live = this.retry ? "reconnecting" : "connecting";
    const source = new EventSource(calc.eventsUrl(this.line, this.version), { withCredentials: true });
    this.source = source;
    source.onopen = () => {
      this.retry = 0;
      this.live = "live";
      this.notify();
    };
    source.onerror = () => {
      if (this.source !== source) return;
      source.close();
      this.source = null;
      if (this.closed) return;
      this.live = "reconnecting";
      this.notify();
      // 1 s, 2 s, 4 s … at most 15 s; the next stream starts from the last version seen.
      const wait = Math.min(15_000, 1000 * 2 ** Math.min(this.retry, 4));
      this.retry++;
      if (this.retryTimer) clearTimeout(this.retryTimer);
      this.retryTimer = setTimeout(() => this.connect(), wait);
    };
    source.addEventListener("change", event => this.onChangeEvent(parse<ChangeEvent>(event)));
    source.addEventListener("presence", event => {
      const body = parse<{ viewers: Viewer[] }>(event);
      if (!body) return;
      this.viewers = body.viewers ?? [];
      for (const viewer of this.viewers) this.accounts.set(viewer.account.uuid, viewer.account);
      this.notify();
    });
    source.addEventListener("comment", event => {
      const body = parse<{ thread: Thread }>(event);
      if (body?.thread) this.onThread?.(body.thread);
    });
    source.addEventListener("copy", event => {
      const body = parse<{ copy: Copy }>(event);
      if (body?.copy) this.onCopy?.(body.copy);
    });
    source.addEventListener("reset", () => {
      void this.resetAll();
    });
  }

  /** Reloads everything (the stream said too much changed to describe). */
  private async resetAll(): Promise<void> {
    try {
      const [sheet, tabs] = await Promise.all([calc.getSheet(this.line.sheet), calc.getTabs(this.line)]);
      this.sheet = sheet;
      this.version = Math.max(this.version, tabs.version);
      this.setTabs(tabs.tabs);
    } catch {
      // The next event or reconnect tries again.
    }
    for (const tab of this.tabs) void this.reload(tab.id);
    this.onMeta?.();
  }

  private onChangeEvent(event: ChangeEvent | null): void {
    if (!event || typeof event.version !== "number") return;
    if (event.version <= this.version && this.log.some(entry => entry.version === event.version)) return;
    if (event.by) this.accounts.set(event.by.uuid, event.by);
    const changes = event.changes ?? [];
    let tabsChanged = false;
    let metaChanged = false;
    for (const change of changes) {
      if ("tabs" in change && change.tabs) tabsChanged = true;
      if ("meta" in change && change.meta) metaChanged = true;
    }
    const byTab = new Map<number, { rects: Rect[]; reload: boolean }>();
    for (const change of changes) {
      if (!("tab" in change)) continue;
      const entry = byTab.get(change.tab) ?? { rects: [], reload: false };
      if ("reload" in change && change.reload) entry.reload = true;
      byTab.set(change.tab, entry);
    }
    for (const [tab, entry] of byTab) {
      const data = this.data.get(tab);
      entry.rects = changeRects(changes as Change[], tab, data?.rows.count, data?.cols.count);
      this.log.push({ version: event.version, tab, rects: entry.rects, reload: entry.reload });
      if (!data) continue;
      if (entry.reload) {
        void this.reload(tab);
        continue;
      }
      const cells = (event.cells ?? []).filter(cell => (cell as { tab?: number }).tab === undefined || (cell as { tab?: number }).tab === tab);
      if (event.cells) {
        data.applyCells(cells, entry.rects);
        // A style this browser has not seen yet: read the range for its definition.
        if (cells.some(cell => cell.s && !this.styles.has(cell.s))) for (const r of entry.rects) void this.reload(tab, r).catch(() => undefined);
      } else {
        data.invalidate(entry.rects);
        const wanted = this.wanted.get(tab);
        if (wanted) for (const id of wanted) if (data.stale.has(id)) this.queue.push({ tab, id });
        this.pump();
      }
    }
    if (this.log.length > EVENT_LOG) this.log = this.log.slice(-EVENT_LOG);
    if (event.version > this.version) this.version = event.version;
    if (tabsChanged) void this.refreshTabs().catch(() => undefined);
    if (metaChanged || tabsChanged) this.onMeta?.();
    this.onChange?.(event);
    this.resolveWaiters();
    this.notify();
  }

  private resolveWaiters(): void {
    const ready = this.versionWaiters.filter(waiter => waiter.version <= this.version);
    this.versionWaiters = this.versionWaiters.filter(waiter => waiter.version > this.version);
    for (const waiter of ready) waiter.resolve();
  }

  /** Resolves once the stream has brought this version (true), or after `timeout` ms (false). */
  private waitForVersion(version: number, timeout: number): Promise<boolean> {
    if (this.version >= version) return Promise.resolve(true);
    return new Promise(resolve => {
      const timer = setTimeout(() => {
        this.versionWaiters = this.versionWaiters.filter(waiter => waiter.resolve !== done);
        resolve(false);
      }, timeout);
      const done = () => {
        clearTimeout(timer);
        resolve(true);
      };
      this.versionWaiters.push({ version, resolve: done });
    });
  }

  /* ---- editing --------------------------------------------------------------------------------------------------- */

  /**
   * Sends edits as one version. `pending` inputs show at once and stay until the new contents arrive. Edits go one
   * at a time, each with the newest version this browser has seen, so the service can say what it overwrote.
   */
  edit(edits: Edit[], pending: PendingSet[] = []): Promise<EditOutcome> {
    if (this.readOnly || this.atVersion !== null) return Promise.resolve({ answer: null, error: new ApiError({ status: 403, code: "not_allowed", message: "This sheet is open read-only.", hint: "Ask its owner for edit access, or open the current version." }) });
    const id = ++this.editSeq;
    for (const p of pending) this.data.get(p.tab)?.pending.set(cellKey(p.row, p.col), { input: p.input, edit: id });
    if (pending.length) this.notify();
    const run = async (): Promise<EditOutcome> => {
      try {
        const answer = await calc.postEdits(this.line, { base_version: this.version, edits, client_id: this.clientId });
        if (answer.overwrote?.length) {
          for (const overwrite of answer.overwrote) if (overwrite.by) this.accounts.set(overwrite.by.uuid, overwrite.by);
          this.onOverwrite?.(answer.overwrote);
        }
        // The stream usually brings the new contents first; when it is slow, read the changed ranges directly.
        if (answer.version > this.version) {
          const arrived = await this.waitForVersion(answer.version, 700);
          if (!arrived) await this.catchUp(answer);
        }
        this.clearPending(id);
        return { answer, error: null };
      } catch (error) {
        const failure = ApiError.from(error);
        this.clearPending(id);
        this.onEditError?.(failure, pending);
        return { answer: null, error: failure };
      }
    };
    const next = this.editChain.then(run, run);
    this.editChain = next.catch(() => undefined);
    return next;
  }

  private async catchUp(answer: EditAnswer): Promise<void> {
    let tabsChanged = false;
    for (const change of answer.changes ?? []) {
      if ("tabs" in change && change.tabs) tabsChanged = true;
      if (!("tab" in change)) continue;
      const data = this.data.get(change.tab);
      if (!data) continue;
      if ("reload" in change && change.reload) {
        await this.reload(change.tab).catch(() => undefined);
        continue;
      }
      if ("range" in change && typeof change.range === "string") {
        const [r] = changeRects([change], change.tab, data.rows.count, data.cols.count);
        if (r) {
          if ((r.r2 - r.r1 + 1) * (r.c2 - r.c1 + 1) <= 20_000) await this.reload(change.tab, r).catch(() => undefined);
          else {
            data.invalidate([r]);
            const wanted = this.wanted.get(change.tab);
            if (wanted) for (const blockIdValue of wanted) if (data.stale.has(blockIdValue)) this.queue.push({ tab: change.tab, id: blockIdValue });
            this.pump();
          }
        }
      }
    }
    if (tabsChanged) await this.refreshTabs().catch(() => undefined);
    // `version` is left to the stream: when it reconnects it replays from there, so nobody else's change is skipped.
    this.notify();
  }

  private clearPending(id: number): void {
    let changed = false;
    for (const data of this.data.values()) {
      for (const [key, value] of data.pending) {
        if (value.edit === id) {
          data.pending.delete(key);
          changed = true;
        }
      }
    }
    if (changed) this.notify();
  }

  undo(): Promise<EditOutcome> {
    return this.edit([{ type: "undo" }]);
  }

  redo(): Promise<EditOutcome> {
    return this.edit([{ type: "redo" }]);
  }

  /* ---- presence -------------------------------------------------------------------------------------------------- */

  /** Tells the others where this browser's selection is (debounced; repeated every 20 s while open). */
  setPresence(tab: number, range: string): void {
    if (!this.me || this.atVersion !== null) return;
    this.lastPresence = { tab, range };
    if (this.presenceDebounce) clearTimeout(this.presenceDebounce);
    this.presenceDebounce = setTimeout(() => this.sendPresence(), 350);
    if (!this.presenceTimer) this.presenceTimer = setInterval(() => this.sendPresence(), 20_000);
  }

  private sendPresence(): void {
    if (!this.lastPresence || this.closed) return;
    void calc.postPresence(this.line, { ...this.lastPresence, client_id: this.clientId }).catch(() => undefined);
  }

  /** Other viewers (not this browser), by tab. */
  othersOn(tab: number): Viewer[] {
    return this.viewers.filter(viewer => viewer.client_id !== this.clientId && viewer.tab === tab);
  }

  /* ---- closing --------------------------------------------------------------------------------------------------- */

  close(): void {
    this.closed = true;
    this.source?.close();
    this.source = null;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    if (this.presenceTimer) clearInterval(this.presenceTimer);
    if (this.presenceDebounce) clearTimeout(this.presenceDebounce);
    if (this.retryView) clearTimeout(this.retryView);
    for (const controller of this.inflight.values()) controller.abort();
    this.inflight.clear();
    this.listeners.clear();
  }
}

function parse<T>(event: Event): T | null {
  try {
    return JSON.parse((event as MessageEvent<string>).data) as T;
  } catch {
    return null;
  }
}
