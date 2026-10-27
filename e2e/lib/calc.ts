// The Silicon Calc service over HTTP (docs/API.md), as one account (or signed out), for the journeys.
//
//   const api = new CalcApi('http://127.0.0.1:8780', () => tokens(), 'c:ada');
//   const sheet = await api.post('/v1/sheets', { json: { title: 'Q3' } }, 201);
//   const answer = await api.raw('GET', `/v1/sheets/${id}/download`, { query: { format: 'xlsx' } });
//
// `get`/`post`/… answer the parsed JSON and throw a CalcError (with the service's error code, message and hint)
// when the status isn't the expected one; `raw` answers everything, bytes included.

export type TokenSource = () => Promise<string>;

export interface Answer {
  method: string;
  path: string;
  status: number;
  headers: Headers;
  bytes: Buffer;
  ms: number;
  text(): string;
  json<T = any>(): T;
}

export interface CalcErrorBody {
  code: string;
  message: string;
  hint?: string;
  details?: any;
}

export class CalcError extends Error {
  readonly status: number;
  readonly code: string;
  readonly body: CalcErrorBody | null;
  readonly detail: string;

  constructor(who: string, a: Answer, expected: string) {
    let body: CalcErrorBody | null = null;
    try {
      const parsed = a.json();
      if (parsed && typeof parsed === 'object' && parsed.error && typeof parsed.error === 'object') body = parsed.error as CalcErrorBody;
    } catch {
      body = null;
    }
    const why = body ? `${body.code}: ${body.message}${body.hint ? ` (hint: ${body.hint})` : ''}` : a.text().slice(0, 300) || '(no body)';
    super(`${a.method} ${a.path} as ${who} answered HTTP ${a.status}, expected ${expected}: ${why}`);
    this.name = 'CalcError';
    this.status = a.status;
    this.code = body?.code ?? '';
    this.body = body;
    this.detail = body?.details ? `details: ${JSON.stringify(body.details).slice(0, 600)}` : '';
  }
}

export interface CallOptions {
  query?: Record<string, string | number | boolean | null | undefined>;
  json?: unknown;
  body?: Buffer | Uint8Array | string;
  contentType?: string;
  headers?: Record<string, string>;
  accept?: string;
  timeoutMs?: number;
  /** Send no Authorization header even when the account has a token. */
  anonymous?: boolean;
}

export class CalcApi {
  readonly baseUrl: string;
  readonly token: TokenSource | null;
  /** Who calls, for messages (`c:ada`, `si:analyst`, `nobody`). */
  readonly who: string;

  constructor(baseUrl: string, token: TokenSource | null, who: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.token = token;
    this.who = who;
  }

  /** The same service, signed out. */
  anonymous(): CalcApi {
    return new CalcApi(this.baseUrl, null, 'nobody (signed out)');
  }

  url(path: string, query?: CallOptions['query']): string {
    const u = new URL(`${this.baseUrl}${path.startsWith('/') ? '' : '/'}${path}`);
    for (const [k, v] of Object.entries(query ?? {})) if (v !== undefined && v !== null) u.searchParams.set(k, String(v));
    return u.toString();
  }

  async raw(method: string, path: string, o: CallOptions = {}): Promise<Answer> {
    const headers: Record<string, string> = { Accept: o.accept ?? 'application/json', ...o.headers };
    let body: BodyInit | undefined;
    if (o.json !== undefined) {
      headers['Content-Type'] = 'application/json';
      body = JSON.stringify(o.json);
    } else if (o.body !== undefined) {
      body = typeof o.body === 'string' ? o.body : new Uint8Array(o.body);
      headers['Content-Type'] = o.contentType ?? 'application/octet-stream';
    }
    if (this.token && !o.anonymous) headers.Authorization = `Bearer ${await this.token()}`;
    const url = this.url(path, o.query);
    const t0 = performance.now();
    let res: Response;
    try {
      res = await fetch(url, { method, headers, body, signal: AbortSignal.timeout(o.timeoutMs ?? 60_000) });
    } catch (error) {
      throw new Error(`${method} ${path} as ${this.who} could not reach the service at ${this.baseUrl}: ${(error as Error).message}`);
    }
    const bytes = Buffer.from(await res.arrayBuffer());
    const ms = performance.now() - t0;
    const shown = `${path}${o.query ? `?${new URL(url).searchParams.toString()}` : ''}`;
    let parsed: { done: boolean; value: any } = { done: false, value: undefined };
    return {
      method,
      path: shown,
      status: res.status,
      headers: res.headers,
      bytes,
      ms,
      text: () => bytes.toString('utf8'),
      json: () => {
        if (!parsed.done) parsed = { done: true, value: bytes.length ? JSON.parse(bytes.toString('utf8')) : null };
        return parsed.value;
      },
    };
  }

  /** The request, its JSON answer, or a CalcError when the status isn't `expect` (default 200, 201 or 204). */
  async call<T = any>(method: string, path: string, o: CallOptions = {}, expect?: number | number[]): Promise<T> {
    const a = await this.raw(method, path, o);
    const ok = expect === undefined ? [200, 201, 204] : Array.isArray(expect) ? expect : [expect];
    if (!ok.includes(a.status)) throw new CalcError(this.who, a, ok.join(' or '));
    if (a.status === 204 || a.bytes.length === 0) return null as T;
    const type = a.headers.get('content-type') ?? '';
    if (!type.includes('json')) return a.text() as T;
    try {
      return a.json<T>();
    } catch {
      throw new Error(`${method} ${a.path} as ${this.who} answered ${a.status} with JSON that doesn't parse: ${a.text().slice(0, 200)}`);
    }
  }

  get<T = any>(path: string, o?: CallOptions, expect?: number | number[]): Promise<T> {
    return this.call<T>('GET', path, o, expect);
  }

  post<T = any>(path: string, o?: CallOptions, expect?: number | number[]): Promise<T> {
    return this.call<T>('POST', path, o, expect);
  }

  patch<T = any>(path: string, o?: CallOptions, expect?: number | number[]): Promise<T> {
    return this.call<T>('PATCH', path, o, expect);
  }

  put<T = any>(path: string, o?: CallOptions, expect?: number | number[]): Promise<T> {
    return this.call<T>('PUT', path, o, expect);
  }

  del<T = any>(path: string, o?: CallOptions, expect?: number | number[]): Promise<T> {
    return this.call<T>('DELETE', path, o, expect);
  }

  /** Expects the call to be refused with `status` (and the error `code` when given); answers the error. */
  async refused(method: string, path: string, o: CallOptions, status: number | number[], code?: string | string[]): Promise<CalcErrorBody> {
    const a = await this.raw(method, path, o);
    const statuses = Array.isArray(status) ? status : [status];
    let body: CalcErrorBody | null = null;
    try {
      body = (a.json() as { error?: CalcErrorBody } | null)?.error ?? null;
    } catch {
      body = null;
    }
    if (!statuses.includes(a.status)) {
      throw new Error(`${method} ${a.path} as ${this.who} answered HTTP ${a.status}, expected it to be refused with ${statuses.join(' or ')}${body ? ` (${body.code}: ${body.message})` : `: ${a.text().slice(0, 200)}`}`);
    }
    if (!body || typeof body.code !== 'string' || typeof body.message !== 'string') {
      throw new Error(`${method} ${a.path} answered ${a.status} without the error shape {"error": {"code", "message", "hint"}}: ${a.text().slice(0, 200)}`);
    }
    const codes = code === undefined ? null : Array.isArray(code) ? code : [code];
    if (codes && !codes.includes(body.code)) {
      throw new Error(`${method} ${a.path} as ${this.who} was refused with ${body.code} (${body.message}), expected ${codes.join(' or ')}`);
    }
    return body;
  }

  // ---------------------------------------------------------------------------------------- the API's shapes

  /** `/v1/sheets/{sheet}` or `/v1/sheets/{sheet}/copies/{copy}`: where a line's routes live. */
  static line(sheet: string, copy?: string | null): string {
    return copy ? `/v1/sheets/${sheet}/copies/${copy}` : `/v1/sheets/${sheet}`;
  }

  /** One edit request on a line, at the line's current version unless `base` is given. */
  async edit(sheet: string, edits: unknown[], o: { copy?: string | null; base?: number } = {}): Promise<EditAnswer> {
    const line = CalcApi.line(sheet, o.copy);
    const base = o.base ?? (await this.get<{ version: number }>(`${line}/tabs`)).version;
    return this.post<EditAnswer>(`${line}/edits`, { json: { base_version: base, edits } });
  }

  /** The cells of a range (`range` omitted: the used area). */
  cells(sheet: string, tab: string | number, range?: string, o: { copy?: string | null; version?: number } = {}): Promise<CellsAnswer> {
    const line = CalcApi.line(sheet, o.copy);
    return this.get<CellsAnswer>(`${line}/tabs/${encodeURIComponent(String(tab))}/cells`, { query: { range, version: o.version } });
  }

  /** A range as rows of plain values (`format=values`): numbers, strings, booleans, null. */
  async values(sheet: string, tab: string | number, range: string, o: { copy?: string | null; version?: number } = {}): Promise<Array<Array<number | string | boolean | null>>> {
    const line = CalcApi.line(sheet, o.copy);
    const a = await this.get<{ values: Array<Array<number | string | boolean | null>> }>(`${line}/tabs/${encodeURIComponent(String(tab))}/cells`, {
      query: { range, format: 'values', version: o.version },
    });
    return a.values;
  }
}

export interface Account {
  uuid: string;
  id: string;
  kind: 'carbon' | 'silicon';
  display_name: string;
  pfp_url?: string | null;
}

export interface Tab {
  id: number;
  name: string;
  index: number;
  rows: number;
  cols: number;
  used: string | null;
  code: null | { language: string; code: string; inputs: number[]; run_by: Account; run_at: string };
  [k: string]: unknown;
}

export interface Sheet {
  id: string;
  title: string;
  owner: Account;
  folder_id: string | null;
  role: 'owner' | 'editor' | 'commenter' | 'viewer';
  public: { enabled: boolean; access: string };
  version: number;
  created_by: Account;
  updated_by: Account;
  trashed_at: string | null;
  tabs?: Tab[];
  copies: number;
  [k: string]: unknown;
}

export interface Cell {
  a: string;
  r: number;
  c: number;
  v: { t: 'empty' | 'number' | 'text' | 'bool' | 'error'; v?: number | string | boolean };
  t: string;
  f?: string;
  s?: number;
  spill?: boolean;
  by: string;
  at: string;
}

export interface CellsAnswer {
  version: number;
  tab: Tab;
  range: string;
  cells: Cell[];
  accounts: Record<string, Account>;
  protections?: Array<{ id: string; tab: number; range?: string | null; editors: unknown[]; description?: string }>;
  [k: string]: unknown;
}

export interface EditAnswer {
  version: number;
  changes: Array<Record<string, unknown>>;
  overwrote: Array<{ tab: number; a: string; by: Account; at: string; before: unknown; version: number }>;
  skipped: number;
}

export interface Thread {
  id: string;
  sheet_id: string;
  copy_id: string | null;
  tab: number;
  tab_name: string;
  range: string | null;
  original_range: string | null;
  quote: string;
  detached: boolean;
  status: 'open' | 'resolved';
  resolved_by: Account | null;
  comments: Comment[];
  [k: string]: unknown;
}

export interface Comment {
  id: string;
  author: Account;
  body: string;
  mentions: Account[];
  reactions: Array<{ emoji: string; accounts: Account[] }>;
  created_at: string;
  edited_at: string | null;
}

export interface Copy {
  id: string;
  sheet_id: string;
  name: string;
  parent_id: string | null;
  forked_at: number;
  version: number;
  created_by: Account;
  status: 'open' | 'merged' | 'deleted';
  [k: string]: unknown;
}

export interface Version {
  version: number;
  from_version: number;
  by: Account;
  started_at: string;
  ended_at: string;
  summary: string;
  name: string | null;
  kind: string;
}

export interface RunAnswer {
  columns: string[];
  rows: unknown[][];
  row_count: number;
  truncated: boolean;
  stdout: string;
  stderr: string;
  duration_ms: number;
  version: number | null;
  tab: Tab | null;
}

/** A cell's plain value from the cells answer (`null` for empty). */
export function plain(cell: Cell | undefined): number | string | boolean | null {
  if (!cell || cell.v.t === 'empty') return null;
  return (cell.v.v as number | string | boolean) ?? null;
}

/** The cell at an address in a cells answer. */
export function at(answer: CellsAnswer, a: string): Cell | undefined {
  return answer.cells.find((c) => c.a === a.toUpperCase());
}
