// What the mock Silicon Ting received (e2e/mock-ting/server.ts), for the journeys: every ting Calc sent and Ting
// accepted, and every send it refused (with the reason).

export interface TingView {
  seq: number;
  id: string;
  created_at: string;
  type: string;
  for: string;
  key: string;
  data: any;
  metadata: any;
  silent: boolean;
  issuing_app: string;
  duplicates: number;
}

export interface TingQuery {
  for?: string;
  type?: string;
  key?: string;
  /** Only tings received after this sequence number (see `seq()`). */
  after?: number;
  /** Only tings whose JSON text contains this. */
  contains?: string;
}

function query(q: TingQuery): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== null && v !== '') p.set(k, String(v));
  return p.toString();
}

export class MockTing {
  readonly url: string;

  constructor(url: string) {
    this.url = url.replace(/\/+$/, '');
  }

  private async get(path: string): Promise<{ status: number; body: any }> {
    const res = await fetch(`${this.url}${path}`, { signal: AbortSignal.timeout(130_000) });
    const text = await res.text();
    let body: any = text;
    try {
      body = JSON.parse(text);
    } catch {
      // plain text
    }
    return { status: res.status, body };
  }

  async healthy(): Promise<boolean> {
    try {
      return (await this.get('/healthz')).status === 200;
    } catch {
      return false;
    }
  }

  /** The newest sequence number so far: wait for tings `after` it. */
  async seq(): Promise<number> {
    const r = await this.get('/tings?limit=1');
    return Number(r.body.last_seq ?? 0);
  }

  /** Every ting that matches, newest first. */
  async list(q: TingQuery = {}): Promise<TingView[]> {
    const r = await this.get(`/tings?${query({ ...q })}&limit=1000`);
    if (r.status !== 200) throw new Error(`the mock Ting answered ${r.status} to GET /tings: ${JSON.stringify(r.body).slice(0, 200)}`);
    return r.body.items as TingView[];
  }

  /** The first ting that matches (oldest first), waiting for it up to `timeoutMs`. */
  async wait(q: TingQuery, timeoutMs = 20_000): Promise<TingView> {
    const r = await this.get(`/tings/wait?${query({ ...q })}&timeout_ms=${timeoutMs}`);
    if (r.status === 200) return r.body as TingView;
    const refused = await this.recentRefusals();
    const why = refused.length
      ? ` The mock Ting's latest refusals: ${refused
          .slice(0, 3)
          .map((x) => `${x.code} (${x.message})`)
          .join('; ')}.`
      : '';
    throw new Error(`no ${q.type ?? 'ting'} for ${q.for ?? 'anyone'} reached the mock Ting within ${Math.round(timeoutMs / 1000)} s.${why}`);
  }

  /** Expects no matching ting to arrive within `ms`. */
  async none(q: TingQuery, ms = 3000): Promise<void> {
    const r = await this.get(`/tings/wait?${query({ ...q })}&timeout_ms=${ms}`);
    if (r.status === 200) throw new Error(`a ${r.body.type} for ${r.body.for} arrived, and none should have: ${JSON.stringify(r.body.data).slice(0, 300)}`);
  }

  /** The last sends the mock Ting refused, newest first (with Ting's error code and message). */
  async recentRefusals(): Promise<Array<{ code: string; message: string; type?: string; for?: string }>> {
    const r = await this.get('/_requests?outcome=refused&limit=20');
    return (r.body?.items ?? []) as Array<{ code: string; message: string }>;
  }
}
