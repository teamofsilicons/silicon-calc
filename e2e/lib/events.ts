// A line's live events (`GET {line}/events?since=`, text/event-stream), as a viewer of the sheet sees them.
//
//   const live = await LiveEvents.open(ada.api, CalcApi.line(sheetId), version);
//   const change = await live.next((e) => e.event === 'change' && e.data.by?.uuid === silicon.uuid);
//   live.close();

import type { CalcApi } from './calc.ts';

export interface LiveEvent {
  event: string;
  data: any;
  at: number;
}

export class LiveEvents {
  readonly events: LiveEvent[] = [];
  private readonly controller: AbortController;
  private waiters: Array<{ match: (e: LiveEvent) => boolean; resolve: (e: LiveEvent) => void }> = [];
  private ended: string | null = null;
  readonly status: number;

  private constructor(controller: AbortController, status: number) {
    this.controller = controller;
    this.status = status;
  }

  /** Opens the stream; throws when the service refuses it. */
  static async open(api: CalcApi, line: string, since?: number): Promise<LiveEvents> {
    const controller = new AbortController();
    const headers: Record<string, string> = { Accept: 'text/event-stream' };
    if (api.token) headers.Authorization = `Bearer ${await api.token()}`;
    const url = api.url(`${line}/events`, { since });
    const res = await fetch(url, { headers, signal: controller.signal });
    if (res.status !== 200 || !res.body) {
      const text = await res.text().catch(() => '');
      controller.abort();
      throw new Error(`GET ${line}/events as ${api.who} answered HTTP ${res.status}, expected 200: ${text.slice(0, 300)}`);
    }
    const type = res.headers.get('content-type') ?? '';
    if (!type.includes('text/event-stream')) {
      controller.abort();
      throw new Error(`GET ${line}/events answered ${type || 'no content type'}, not text/event-stream`);
    }
    const live = new LiveEvents(controller, res.status);
    void live.pump(res.body);
    return live;
  }

  private async pump(body: ReadableStream<Uint8Array>): Promise<void> {
    const decoder = new TextDecoder();
    let buf = '';
    let event = 'message';
    let data: string[] = [];
    try {
      for await (const chunk of body as unknown as AsyncIterable<Uint8Array>) {
        buf += decoder.decode(chunk, { stream: true });
        let nl: number;
        while ((nl = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, nl).replace(/\r$/, '');
          buf = buf.slice(nl + 1);
          if (line === '') {
            if (data.length) this.push(event, data.join('\n'));
            event = 'message';
            data = [];
          } else if (line.startsWith(':')) {
            // a comment (keep-alive)
          } else if (line.startsWith('event:')) {
            event = line.slice(6).trim();
          } else if (line.startsWith('data:')) {
            data.push(line.slice(5).replace(/^ /, ''));
          }
        }
      }
      this.ended = 'the service ended the stream';
    } catch (error) {
      this.ended = this.controller.signal.aborted ? 'closed' : `the stream failed: ${(error as Error).message}`;
    }
  }

  private push(event: string, text: string): void {
    let data: any = text;
    try {
      data = JSON.parse(text);
    } catch {
      // not JSON
    }
    const e: LiveEvent = { event, data, at: Date.now() };
    this.events.push(e);
    for (const w of [...this.waiters]) {
      if (w.match(e)) {
        this.waiters = this.waiters.filter((x) => x !== w);
        w.resolve(e);
      }
    }
  }

  /** The first event (already received, or still to come) that matches, within `timeoutMs`. */
  next(match: (e: LiveEvent) => boolean, timeoutMs = 15_000, what = 'the event'): Promise<LiveEvent> {
    const seen = this.events.find(match);
    if (seen) return Promise.resolve(seen);
    return new Promise((resolve, reject) => {
      const w = { match, resolve: (e: LiveEvent) => (clearTimeout(timer), resolve(e)) };
      const timer = setTimeout(() => {
        this.waiters = this.waiters.filter((x) => x !== w);
        const got = this.events.map((e) => e.event).join(', ') || 'none';
        reject(new Error(`${what} did not arrive on the live stream within ${Math.round(timeoutMs / 1000)} s (events so far: ${got}${this.ended ? `; ${this.ended}` : ''})`));
      }, timeoutMs);
      this.waiters.push(w);
    });
  }

  close(): void {
    this.controller.abort();
  }
}
