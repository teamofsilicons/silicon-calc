// A stand-in for Silicon Ting on 127.0.0.1:8735, for Calc's local stack and its end-to-end journeys.
//
// It speaks Ting's v1 WebSocket protocol the way publishers use it (Silicon DM's ting.rs, Silicon Ting's ws.rs):
//
//   connect   GET /v1/ws?protocol=v1 (WebSocket)  →  {"op":"ready","receiver_id":"recv_…","protocol":"v1"}
//   send      {"op":"send","request_id":"…","proof_token":"sap_…","body":"<the ting, as the exact JSON text>"}
//   accepted  {"op":"accepted","request_id":"…","id":"ting_…","created_at":"…Z","status":"accepted","key":"…","silent":false}
//   refused   {"op":"error","request_id":"…","error":{"code":"…","message":"…","hint":"…"}}
//
// Every send is checked the way Ting checks it: the proof with Silicon Accounts (POST /v1/proofs/verify with the
// ting app's credentials: valid, an App verification proof issued by calc, scope tings.send), then the ting
// ({"type","for","key","data","metadata"}: a type of the issuing app, the recipient's account uuid, which Silicon
// Accounts must know, a 1-200 byte key, object data and metadata). A key is accepted once: the same ting again
// answers the first acceptance; other contents under the same key are refused (idempotency_conflict).
//
// What it received, for the journeys and for people:
//
//   GET    /tings?for=<uuid>&type=<type>&key=&after=<seq>&limit=   {"items":[ting…],"count","last_seq"} newest first
//   GET    /tings/wait?for=&type=&key=&after=<seq>&timeout_ms=     the first match with seq > after (408 on timeout)
//   GET    /tings/<id>                                              one ting
//   DELETE /tings                                                   forget every ting
//   GET    /_requests?outcome=accepted|refused&limit=               every send, refused ones with the reason
//   POST   /_faults {"kind":"error"|"drop"|"delay"|"no_reply"|"reject_upgrade"|"bad_ready","count"?,"code"?,"delay_ms"?}
//   GET|DELETE /_faults, POST /_disconnect (closes every socket), GET /_connections, POST /_reset, GET /healthz
//
//   node e2e/mock-ting/server.ts [--port 8735] [--host 127.0.0.1] [--stack e2e/stack.json] [--accounts-api URL]
//        [--app-id ting] [--app-secret S] [--issuer calc] [--scope tings.send] [--types a,b] [--ping-seconds 20]
//        [--ready-file path] [--log]
//
// Settings not given come from e2e/stack.json (STACK_JSON). Port 0 picks a free port (see --ready-file).

import { createHash, randomBytes } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import type { Duplex } from 'node:stream';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({
  options: {
    port: { type: 'string' },
    host: { type: 'string' },
    stack: { type: 'string' },
    'accounts-api': { type: 'string' },
    'app-id': { type: 'string' },
    'app-secret': { type: 'string' },
    issuer: { type: 'string' },
    scope: { type: 'string' },
    types: { type: 'string' },
    'ping-seconds': { type: 'string' },
    'ready-file': { type: 'string' },
    log: { type: 'boolean' },
  },
});

const here = dirname(fileURLToPath(import.meta.url));
const stackFile = args.stack ?? process.env.STACK_JSON ?? join(here, '..', 'stack.json');
const stack: any = existsSync(stackFile) ? JSON.parse(readFileSync(stackFile, 'utf8')) : {};

const PORT = Number(args.port ?? process.env.MOCK_TING_PORT ?? 8735);
const HOST = args.host ?? '127.0.0.1';
const ACCOUNTS_API = (args['accounts-api'] ?? process.env.ACCOUNTS_API_URL ?? stack.accounts?.api_url ?? 'http://127.0.0.1:9989').replace(/\/+$/, '');
const APP_ID = args['app-id'] ?? stack.apps?.ting?.app_id ?? 'ting';
const APP_SECRET = args['app-secret'] ?? process.env.MOCK_TING_APP_SECRET ?? stack.apps?.ting?.app_secret ?? '';
const ISSUERS = (args.issuer ?? process.env.MOCK_TING_ISSUERS ?? stack.ting?.issuing_app ?? 'calc').split(',').map((s: string) => s.trim()).filter(Boolean);
const SCOPE = args.scope ?? stack.ting?.scope ?? 'tings.send';
const CALC_TYPES = ['calc.mention.received', 'calc.comment.created', 'calc.comment.replied', 'calc.thread.resolved', 'calc.copy.merged', 'calc.sheet.shared'];
const TYPES = new Set((args.types ?? process.env.MOCK_TING_TYPES ?? CALC_TYPES.join(',')).split(',').map((s: string) => s.trim()).filter(Boolean));
const PING_SECONDS = Number(args['ping-seconds'] ?? process.env.MOCK_TING_PING_SECONDS ?? 20);
const LOG = args.log === true || process.env.MOCK_TING_LOG === '1';

if (!APP_SECRET) {
  console.error(`error: the mock Ting needs the ting app's secret to verify proofs\nhint: start the Accounts stack (scripts/accounts-stack.sh start writes ${stackFile}), or pass --app-secret`);
  process.exit(2);
}

const MAX_FRAME = 1024 * 1024;
const MAX_BODY = 256 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const TYPE = /^([a-z][a-z0-9_-]*)\.([a-z][a-z0-9_-]*)\.([a-z][a-z0-9_-]*)$/;

function log(...parts: unknown[]): void {
  if (LOG) console.error(new Date().toISOString(), ...parts);
}

// ------------------------------------------------------------------ state

interface Ting {
  seq: number;
  id: string;
  created_at: string;
  type: string;
  for: string;
  key: string;
  data: unknown;
  metadata: unknown;
  silent: boolean;
  read: boolean;
  issuing_app: string;
  proof_id: string;
  receiver_id: string;
  request_id: string;
  /** The body exactly as the publisher sent it. */
  body: string;
  /** How many times the same ting was sent again under its key (each answered with the first acceptance). */
  duplicates: number;
}

interface RequestRecord {
  seq: number;
  at: string;
  receiver_id: string;
  request_id: string | null;
  outcome: 'accepted' | 'duplicate' | 'refused';
  code?: string;
  message?: string;
  type?: string;
  for?: string;
  key?: string;
  ting_id?: string;
  ms: number;
}

interface Fault {
  kind: 'error' | 'drop' | 'delay' | 'no_reply' | 'reject_upgrade' | 'bad_ready';
  count: number;
  code?: string;
  message?: string;
  delay_ms?: number;
}

let seq = 0;
let requestSeq = 0;
const tings: Ting[] = [];
const byKey = new Map<string, Ting>();
const requests: RequestRecord[] = [];
let faults: Fault[] = [];
const waiters = new Set<{ match: (t: Ting) => boolean; resolve: (t: Ting) => void }>();
const connections = new Set<Conn>();
const recipients = new Map<string, number>(); // uuid → when Silicon Accounts last confirmed it (ms)

function newId(prefix: string): string {
  return `${prefix}_${randomBytes(12).toString('base64url').replace(/[-_]/g, 'x').toLowerCase()}`;
}

function takeFault(kinds: Fault['kind'][]): Fault | null {
  const f = faults.find((x) => kinds.includes(x.kind) && x.count > 0);
  if (!f) return null;
  f.count -= 1;
  faults = faults.filter((x) => x.count > 0);
  return f;
}

function record(r: Omit<RequestRecord, 'seq' | 'at'>): void {
  requests.unshift({ seq: ++requestSeq, at: new Date().toISOString(), ...r });
  if (requests.length > 1000) requests.length = 1000;
}

// ------------------------------------------------------------------ checks

class Refusal extends Error {
  code: string;
  hint: string;
  constructor(code: string, message: string, hint = '') {
    super(message);
    this.code = code;
    this.hint = hint;
  }
}

const basic = `Basic ${Buffer.from(`${APP_ID}:${APP_SECRET}`).toString('base64')}`;

async function accountsCall(method: string, path: string, body?: unknown): Promise<{ status: number; json: any }> {
  let res: Response;
  try {
    res = await fetch(`${ACCOUNTS_API}${path}`, {
      method,
      headers: { Authorization: basic, Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(10_000),
    });
  } catch (error) {
    throw new Refusal('unavailable', `Ting could not reach Silicon Accounts at ${ACCOUNTS_API} to check the proof: ${(error as Error).message}.`, 'Send the same ting again later.');
  }
  const text = await res.text();
  let json: any = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = null;
  }
  return { status: res.status, json };
}

interface VerifiedProof {
  proof_id: string;
  issuing_app: string;
}

async function verifyProof(token: string): Promise<VerifiedProof> {
  const { status, json } = await accountsCall('POST', '/v1/proofs/verify', { proof_token: token });
  if (status === 401) {
    throw new Refusal('unavailable', "Silicon Accounts refused the ting app's own credentials, so Ting can't check proofs.", 'Check the mock Ting\'s --app-secret (e2e/stack.json apps.ting.app_secret).');
  }
  if (status !== 200 || !json) throw new Refusal('unavailable', `Silicon Accounts answered HTTP ${status} when Ting checked the proof.`, 'Send the same ting again later.');
  if (json.valid !== true) {
    throw new Refusal('invalid_proof', 'The proof is not valid for Ting: it is unknown, expired or revoked, or it was issued for another app.', 'Get a new App verification proof for ting (scope tings.send) and send again.');
  }
  if (json.kind !== 'app_verification') {
    throw new Refusal('permission_denied', `Sending tings takes an App verification proof; this is a ${json.kind} proof.`, 'Issue an App verification proof with receiving app ting.');
  }
  const issuer = json.issuing_app?.app_id ?? '';
  if (!ISSUERS.includes(issuer)) {
    throw new Refusal('permission_denied', `The proof was issued by ${issuer || 'an unknown app'}, which may not send tings here (only ${ISSUERS.join(', ')}).`, '');
  }
  if (json.receiving_app?.app_id && json.receiving_app.app_id !== APP_ID) {
    throw new Refusal('invalid_proof', `The proof is for ${json.receiving_app.app_id}, not ${APP_ID}.`, 'Get an App verification proof whose receiving app is ting.');
  }
  if (!Array.isArray(json.scopes) || !json.scopes.includes(SCOPE)) {
    throw new Refusal('missing_scope', `The proof does not carry the ${SCOPE} scope (it has ${JSON.stringify(json.scopes ?? [])}).`, `Issue the proof with scopes ["${SCOPE}"].`);
  }
  return { proof_id: String(json.proof_id ?? ''), issuing_app: issuer };
}

async function checkRecipient(uuid: string): Promise<void> {
  const seen = recipients.get(uuid);
  if (seen && Date.now() - seen < 60_000) return;
  const { status } = await accountsCall('GET', `/v1/accounts/${encodeURIComponent(uuid)}`);
  if (status === 404) {
    throw new Refusal('unknown_recipient', `No Carbon or Silicon has the uuid ${uuid} at Silicon Accounts.`, 'Name the recipient by its Silicon Accounts uuid.');
  }
  if (status !== 200) throw new Refusal('unavailable', `Silicon Accounts answered HTTP ${status} when Ting looked the recipient up.`, 'Send the same ting again later.');
  recipients.set(uuid, Date.now());
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** Checks one ting body; returns it parsed. */
function checkBody(raw: string, issuer: string): { type: string; for: string; key: string; data: Record<string, unknown>; metadata: Record<string, unknown> } {
  if (Buffer.byteLength(raw) > MAX_BODY) throw new Refusal('payload_too_large', `The ting is ${Buffer.byteLength(raw)} bytes; Ting takes at most ${MAX_BODY}.`, 'Send less data in the ting.');
  let b: unknown;
  try {
    b = JSON.parse(raw);
  } catch {
    throw new Refusal('invalid_ting', 'The body is not valid JSON.', 'Send the ting as the exact JSON text of {"type","for","key","data","metadata"}.');
  }
  if (!isObject(b)) throw new Refusal('invalid_ting', 'A ting is a JSON object.', '');
  const allowed = new Set(['type', 'for', 'key', 'data', 'metadata', 'delivery']);
  for (const k of Object.keys(b)) {
    if (k === 'org_id') throw new Refusal('invalid_ting', 'org_id belongs to the organization-era Ting; a ting names its recipient by account uuid only.', 'Remove org_id.');
    if (!allowed.has(k)) throw new Refusal('invalid_ting', `A ting has no field "${k}" (fields: type, for, key, data, metadata).`, `Remove "${k}".`);
  }
  const type = b.type;
  if (typeof type !== 'string' || !type) throw new Refusal('invalid_ting', 'type is required: a string like calc.mention.received.', '');
  if (Buffer.byteLength(type) > 255) throw new Refusal('invalid_ting', 'type is longer than 255 bytes.', '');
  const m = TYPE.exec(type);
  if (!m) throw new Refusal('invalid_ting', `"${type}" is not a ting type: types are {app}.{service}.{event} in lowercase letters, digits, _ and -.`, '');
  if (m[1] !== issuer) throw new Refusal('invalid_ting', `"${type}" belongs to the app ${m[1]}, but the proof is ${issuer}'s.`, `Use one of ${issuer}'s types.`);
  if (TYPES.size && !TYPES.has(type)) throw new Refusal('invalid_ting', `"${type}" is not a registered type of ${issuer} (${[...TYPES].join(', ')}).`, 'Use a registered type.');
  const recipient = b.for;
  if (typeof recipient !== 'string' || !UUID.test(recipient.toLowerCase())) {
    throw new Refusal('invalid_ting', `for must be the recipient's Silicon Accounts uuid, like 6667d4b4-1f0b-4c8e-9a52-0d0d6f8c2a11 (got ${JSON.stringify(recipient)}).`, '');
  }
  const key = b.key;
  if (typeof key !== 'string' || !key || Buffer.byteLength(key) > 200 || /[\u0000-\u001f\u007f]/.test(key)) {
    throw new Refusal('invalid_ting', 'key is required: 1 to 200 bytes without control characters.', '');
  }
  if (!isObject(b.data)) throw new Refusal('invalid_ting', 'data is required and must be a JSON object.', '');
  if (b.metadata !== undefined && !isObject(b.metadata)) throw new Refusal('invalid_ting', 'metadata must be a JSON object.', '');
  if (b.delivery !== undefined && b.delivery !== 'required') throw new Refusal('invalid_ting', 'delivery may only be "required".', '');
  return { type, for: recipient.toLowerCase(), key, data: b.data, metadata: isObject(b.metadata) ? b.metadata : {} };
}

/** The normalized request (key order and whitespace don't matter; omitted metadata is {}). */
function fingerprint(t: { type: string; for: string; data: unknown; metadata: unknown }): string {
  const canon = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(canon);
    if (isObject(v)) return Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])]));
    return v;
  };
  return JSON.stringify(canon({ type: t.type, for: t.for, data: t.data, metadata: t.metadata }));
}

async function handleSend(conn: Conn, frame: Record<string, unknown>): Promise<Record<string, unknown> | null> {
  const started = performance.now();
  const requestId = typeof frame.request_id === 'string' ? frame.request_id : null;
  const refuse = (e: Refusal, extra: Partial<RequestRecord> = {}) => {
    record({ receiver_id: conn.id, request_id: requestId, outcome: 'refused', code: e.code, message: e.message, ms: Math.round(performance.now() - started), ...extra });
    log('refused', requestId, e.code, e.message);
    return { op: 'error', request_id: requestId, error: { code: e.code, message: e.message, ...(e.hint ? { hint: e.hint } : {}) } };
  };
  try {
    for (const k of Object.keys(frame)) {
      if (!['op', 'request_id', 'proof_token', 'body', 'headers'].includes(k)) throw new Refusal('invalid_input', `A send frame has no field "${k}".`, 'Send {"op","request_id","proof_token","body"}.');
    }
    if (!requestId || requestId.length > 100) throw new Refusal('invalid_input', 'request_id is required: 1 to 100 characters.', '');
    if (typeof frame.proof_token !== 'string' || !frame.proof_token) throw new Refusal('invalid_input', 'proof_token is required: the App verification proof (sap_…).', '');
    if (frame.proof_token.length > 32768) throw new Refusal('invalid_input', 'proof_token is too long.', '');
    if (typeof frame.body !== 'string') throw new Refusal('invalid_input', 'body must be the exact prepared JSON string of the ting.', 'Serialize the ting and send the text.');
    if (frame.headers !== undefined && !isObject(frame.headers)) throw new Refusal('invalid_input', 'headers must be an object.', '');

    const fault = takeFault(['error', 'delay', 'no_reply', 'drop']);
    if (fault?.kind === 'drop') {
      record({ receiver_id: conn.id, request_id: requestId, outcome: 'refused', code: 'fault_drop', message: 'dropped the connection (fault)', ms: 0 });
      conn.destroy();
      return null;
    }
    if (fault?.kind === 'no_reply') {
      record({ receiver_id: conn.id, request_id: requestId, outcome: 'refused', code: 'fault_no_reply', message: 'never answered (fault)', ms: 0 });
      return null;
    }
    if (fault?.kind === 'delay') await new Promise((r) => setTimeout(r, fault.delay_ms ?? 1000));
    if (fault?.kind === 'error') throw new Refusal(fault.code ?? 'unavailable', fault.message ?? `Ting refused this send on purpose (fault ${fault.code ?? 'unavailable'}).`, '');

    const proof = await verifyProof(frame.proof_token);
    const t = checkBody(frame.body, proof.issuing_app);
    await checkRecipient(t.for);
    const idKey = `${proof.issuing_app}\u0000${t.key}`;
    const existing = byKey.get(idKey);
    if (existing) {
      if (fingerprint(existing) !== fingerprint(t)) {
        throw new Refusal('idempotency_conflict', `The key ${t.key} was already used for a different ting (${existing.type} for ${existing.for}).`, 'Use a new key for a new ting.');
      }
      existing.duplicates += 1;
      record({ receiver_id: conn.id, request_id: requestId, outcome: 'duplicate', type: t.type, for: t.for, key: t.key, ting_id: existing.id, ms: Math.round(performance.now() - started) });
      return { op: 'accepted', request_id: requestId, id: existing.id, created_at: existing.created_at, status: 'accepted', key: existing.key, silent: existing.silent };
    }
    const ting: Ting = {
      seq: ++seq,
      id: newId('ting'),
      created_at: new Date().toISOString(),
      type: t.type,
      for: t.for,
      key: t.key,
      data: t.data,
      metadata: t.metadata,
      silent: false,
      read: false,
      issuing_app: proof.issuing_app,
      proof_id: proof.proof_id,
      receiver_id: conn.id,
      request_id: requestId,
      body: frame.body,
      duplicates: 0,
    };
    tings.push(ting);
    byKey.set(idKey, ting);
    record({ receiver_id: conn.id, request_id: requestId, outcome: 'accepted', type: t.type, for: t.for, key: t.key, ting_id: ting.id, ms: Math.round(performance.now() - started) });
    log('accepted', ting.id, ting.type, ting.for, ting.key);
    for (const w of [...waiters]) {
      if (w.match(ting)) {
        waiters.delete(w);
        w.resolve(ting);
      }
    }
    return { op: 'accepted', request_id: requestId, id: ting.id, created_at: ting.created_at, status: 'accepted', key: ting.key, silent: ting.silent };
  } catch (error) {
    if (error instanceof Refusal) return refuse(error);
    return refuse(new Refusal('internal', `The mock Ting failed: ${(error as Error).message}`, ''));
  }
}

// ------------------------------------------------------------------ WebSocket (RFC 6455, server side)

class Conn {
  id: string;
  socket: Duplex;
  connectedAt = new Date().toISOString();
  buf: Buffer = Buffer.alloc(0);
  fragments: Buffer[] = [];
  fragOpcode = 0;
  fragBytes = 0;
  closed = false;
  framesIn = 0;
  framesOut = 0;
  /** Messages are handled one after another, like Ting does, so answers come back in order. */
  chain: Promise<void> = Promise.resolve();
  pingTimer: ReturnType<typeof setInterval> | null = null;
  awaitingPongSince: number | null = null;

  constructor(socket: Duplex) {
    this.id = newId('recv');
    this.socket = socket;
  }

  start(head: Buffer): void {
    connections.add(this);
    this.socket.on('data', (chunk: Buffer) => this.onData(chunk));
    this.socket.on('close', () => this.cleanup());
    this.socket.on('error', () => this.cleanup());
    if (head.length) this.onData(head);
    const badReady = takeFault(['bad_ready']);
    this.sendJson(badReady ? { op: 'ready', protocol: 'v0' } : { op: 'ready', receiver_id: this.id, protocol: 'v1' });
    if (PING_SECONDS > 0) {
      this.pingTimer = setInterval(() => {
        if (this.awaitingPongSince && Date.now() - this.awaitingPongSince > 10_000) {
          log('no pong within 10 s; closing', this.id);
          this.close(1001, 'no pong');
          return;
        }
        if (!this.awaitingPongSince) {
          this.awaitingPongSince = Date.now();
          this.sendFrame(0x9, Buffer.from([1]));
        }
      }, PING_SECONDS * 1000);
    }
  }

  cleanup(): void {
    if (this.pingTimer) clearInterval(this.pingTimer);
    this.pingTimer = null;
    this.closed = true;
    connections.delete(this);
  }

  destroy(): void {
    this.cleanup();
    this.socket.destroy();
  }

  onData(chunk: Buffer): void {
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    for (;;) {
      const b = this.buf;
      if (b.length < 2) return;
      const fin = (b[0]! & 0x80) !== 0;
      const rsv = b[0]! & 0x70;
      const opcode = b[0]! & 0x0f;
      const masked = (b[1]! & 0x80) !== 0;
      let len = b[1]! & 0x7f;
      let off = 2;
      if (rsv) return this.close(1002, 'reserved bits are set (no extensions were agreed)');
      if (len === 126) {
        if (b.length < 4) return;
        len = b.readUInt16BE(2);
        off = 4;
      } else if (len === 127) {
        if (b.length < 10) return;
        const big = b.readBigUInt64BE(2);
        if (big > BigInt(MAX_FRAME)) return this.close(1009, 'frame larger than 1 MiB');
        len = Number(big);
        off = 10;
      }
      if (!masked) return this.close(1002, 'client frames must be masked');
      if (len > MAX_FRAME) return this.close(1009, 'frame larger than 1 MiB');
      if (b.length < off + 4 + len) return;
      const mask = b.subarray(off, off + 4);
      off += 4;
      const payload = Buffer.from(b.subarray(off, off + len));
      for (let i = 0; i < payload.length; i++) payload[i] = payload[i]! ^ mask[i & 3]!;
      this.buf = b.subarray(off + len);
      this.framesIn++;
      this.onFrame(fin, opcode, payload);
      if (this.closed) return;
    }
  }

  onFrame(fin: boolean, opcode: number, payload: Buffer): void {
    if (opcode >= 0x8) {
      if (!fin || payload.length > 125) return this.close(1002, 'control frames are short and unfragmented');
      if (opcode === 0x8) {
        // Answer the close with the same code, then end.
        this.sendFrame(0x8, payload.subarray(0, 2));
        this.cleanup();
        this.socket.end();
        return;
      }
      if (opcode === 0x9) return this.sendFrame(0xa, payload);
      if (opcode === 0xa) {
        this.awaitingPongSince = null;
        return;
      }
      return this.close(1002, `unknown control opcode ${opcode}`);
    }
    if (opcode === 0x0) {
      if (!this.fragOpcode) return this.close(1002, 'continuation without a message');
      this.fragments.push(payload);
      this.fragBytes += payload.length;
      if (this.fragBytes > MAX_FRAME) return this.close(1009, 'message larger than 1 MiB');
      if (fin) {
        const whole = Buffer.concat(this.fragments);
        const op = this.fragOpcode;
        this.fragments = [];
        this.fragOpcode = 0;
        this.fragBytes = 0;
        this.onMessage(op, whole);
      }
      return;
    }
    if (opcode === 0x1 || opcode === 0x2) {
      if (this.fragOpcode) return this.close(1002, 'a new message started inside a fragmented one');
      if (fin) return this.onMessage(opcode, payload);
      this.fragOpcode = opcode;
      this.fragments = [payload];
      this.fragBytes = payload.length;
      return;
    }
    this.close(1002, `unknown opcode ${opcode}`);
  }

  onMessage(opcode: number, payload: Buffer): void {
    if (opcode === 0x2) return this.close(1003, 'JSON text frames required');
    let text: string;
    try {
      text = new TextDecoder('utf-8', { fatal: true }).decode(payload);
    } catch {
      return this.close(1007, 'text frames must be UTF-8');
    }
    this.chain = this.chain.then(async () => {
      let frame: unknown;
      try {
        frame = JSON.parse(text);
      } catch {
        this.sendJson({ op: 'error', request_id: null, error: { code: 'invalid_input', message: 'The frame is not JSON.' } });
        return;
      }
      if (!isObject(frame)) {
        this.sendJson({ op: 'error', request_id: null, error: { code: 'invalid_input', message: 'A frame is a JSON object.' } });
        return;
      }
      const requestId = typeof frame.request_id === 'string' ? frame.request_id : null;
      if (frame.op !== 'send') {
        const known = ['subscribe', 'unsubscribe', 'ack', 'watch_inbox'].includes(String(frame.op));
        this.sendJson({
          op: 'error',
          request_id: requestId,
          error: known
            ? { code: 'invalid_input', message: `The mock Ting only takes send frames (it has no receivers): ${String(frame.op)}.` }
            : { code: 'invalid_input', message: 'Unknown WebSocket operation.' },
        });
        return;
      }
      const reply = await handleSend(this, frame);
      if (reply && !this.closed) this.sendJson(reply);
    });
  }

  sendJson(value: unknown): void {
    this.sendFrame(0x1, Buffer.from(JSON.stringify(value)));
  }

  sendFrame(opcode: number, payload: Buffer): void {
    if (this.closed || this.socket.destroyed) return;
    let header: Buffer;
    if (payload.length < 126) {
      header = Buffer.from([0x80 | opcode, payload.length]);
    } else if (payload.length < 65536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 126;
      header.writeUInt16BE(payload.length, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 127;
      header.writeBigUInt64BE(BigInt(payload.length), 2);
    }
    this.framesOut++;
    this.socket.write(Buffer.concat([header, payload]));
  }

  close(code: number, reason: string): void {
    if (this.closed) return;
    const r = Buffer.from(reason.slice(0, 120));
    const p = Buffer.alloc(2 + r.length);
    p.writeUInt16BE(code, 0);
    r.copy(p, 2);
    this.sendFrame(0x8, p);
    this.cleanup();
    this.socket.end();
    setTimeout(() => this.socket.destroy(), 1000).unref();
  }
}

function upgrade(req: IncomingMessage, socket: Duplex, head: Buffer): void {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const reject = (status: number, message: string) => {
    socket.end(`HTTP/1.1 ${status} ${status === 404 ? 'Not Found' : status === 503 ? 'Service Unavailable' : 'Bad Request'}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n${JSON.stringify({ error: { code: status === 503 ? 'unavailable' : 'invalid_input', message } })}`);
  };
  if (url.pathname !== '/v1/ws') return reject(404, `No WebSocket at ${url.pathname}; Ting's is /v1/ws?protocol=v1.`);
  if (url.searchParams.get('protocol') !== 'v1') return reject(400, 'Ask for protocol=v1.');
  if (takeFault(['reject_upgrade'])) return reject(503, 'Ting refused this connection on purpose (fault reject_upgrade).');
  const key = req.headers['sec-websocket-key'];
  if (typeof key !== 'string' || String(req.headers.upgrade ?? '').toLowerCase() !== 'websocket' || req.headers['sec-websocket-version'] !== '13') {
    return reject(400, 'Not a WebSocket upgrade (Upgrade: websocket, Sec-WebSocket-Version: 13, Sec-WebSocket-Key).');
  }
  const accept = createHash('sha1').update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  const conn = new Conn(socket);
  log('connected', conn.id);
  conn.start(head);
}

// ------------------------------------------------------------------ HTTP

function send(res: ServerResponse, status: number, body: unknown): void {
  const text = typeof body === 'string' ? body : JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': typeof body === 'string' ? 'text/plain; charset=utf-8' : 'application/json', 'Cache-Control': 'no-store' });
  res.end(text);
}

function view(t: Ting): Record<string, unknown> {
  return {
    seq: t.seq,
    id: t.id,
    created_at: t.created_at,
    type: t.type,
    for: t.for,
    key: t.key,
    data: t.data,
    metadata: t.metadata,
    silent: t.silent,
    read: t.read,
    issuing_app: t.issuing_app,
    duplicates: t.duplicates,
  };
}

function matcher(q: URLSearchParams): (t: Ting) => boolean {
  const recipient = q.get('for')?.toLowerCase();
  const type = q.get('type');
  const key = q.get('key');
  const after = Number(q.get('after') ?? 0);
  const contains = q.get('contains');
  return (t) =>
    (!recipient || t.for === recipient) &&
    (!type || t.type === type) &&
    (!key || t.key === key) &&
    t.seq > after &&
    (!contains || t.body.includes(contains));
}

async function readBody(req: IncomingMessage): Promise<any> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  const text = Buffer.concat(chunks).toString('utf8');
  if (!text) return {};
  return JSON.parse(text);
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const path = url.pathname;
  try {
    if (req.method === 'GET' && (path === '/healthz' || path === '/readyz')) return send(res, 200, 'ok');
    if (req.method === 'GET' && path === '/') {
      return send(res, 200, { app_id: APP_ID, mock: true, api_version: 'v1', websocket: '/v1/ws?protocol=v1', issuers: ISSUERS, scope: SCOPE, types: [...TYPES], accounts_api: ACCOUNTS_API, tings: tings.length });
    }
    if (path === '/v1/ws') return send(res, 426, { error: { code: 'upgrade_required', message: 'Connect with a WebSocket: GET /v1/ws?protocol=v1.' } });
    if (req.method === 'GET' && path === '/tings') {
      const match = matcher(url.searchParams);
      const limit = Math.min(Number(url.searchParams.get('limit') ?? 200) || 200, 1000);
      const items = tings.filter(match).reverse();
      return send(res, 200, { items: items.slice(0, limit).map(view), count: items.length, last_seq: seq });
    }
    if (req.method === 'GET' && path === '/tings/wait') {
      const match = matcher(url.searchParams);
      const timeout = Math.min(Number(url.searchParams.get('timeout_ms') ?? 10_000) || 10_000, 120_000);
      const found = tings.find(match);
      if (found) return send(res, 200, view(found));
      const ting = await new Promise<Ting | null>((resolve) => {
        const w = { match, resolve: (t: Ting) => resolve(t) };
        waiters.add(w);
        const timer = setTimeout(() => {
          waiters.delete(w);
          resolve(null);
        }, timeout);
        req.on('close', () => {
          clearTimeout(timer);
          waiters.delete(w);
        });
      });
      if (ting) return send(res, 200, view(ting));
      return send(res, 408, { error: { code: 'timeout', message: `No ting matching ${url.searchParams.toString()} arrived within ${timeout} ms.`, hint: 'GET /_requests shows every send, refused ones with the reason.' } });
    }
    if (req.method === 'GET' && path.startsWith('/tings/')) {
      const t = tings.find((x) => x.id === decodeURIComponent(path.slice('/tings/'.length)));
      return t ? send(res, 200, view(t)) : send(res, 404, { error: { code: 'not_found', message: 'No such ting.' } });
    }
    if (req.method === 'DELETE' && path === '/tings') {
      tings.length = 0;
      byKey.clear();
      return send(res, 200, { cleared: true });
    }
    if (req.method === 'GET' && path === '/_requests') {
      const outcome = url.searchParams.get('outcome');
      const limit = Math.min(Number(url.searchParams.get('limit') ?? 200) || 200, 1000);
      const items = requests.filter((r) => !outcome || r.outcome === outcome || (outcome === 'accepted' && r.outcome === 'duplicate'));
      return send(res, 200, { items: items.slice(0, limit), count: items.length });
    }
    if (req.method === 'DELETE' && path === '/_requests') {
      requests.length = 0;
      return send(res, 200, { cleared: true });
    }
    if (path === '/_faults') {
      if (req.method === 'GET') return send(res, 200, { items: faults });
      if (req.method === 'DELETE') {
        faults = [];
        return send(res, 200, { cleared: true });
      }
      if (req.method === 'POST') {
        const b = await readBody(req);
        const kinds = ['error', 'drop', 'delay', 'no_reply', 'reject_upgrade', 'bad_ready'];
        if (!kinds.includes(b.kind)) return send(res, 400, { error: { code: 'invalid_input', message: `kind must be one of ${kinds.join(', ')}` } });
        const f: Fault = { kind: b.kind, count: Number(b.count ?? 1), ...(b.code ? { code: String(b.code) } : {}), ...(b.message ? { message: String(b.message) } : {}), ...(b.delay_ms ? { delay_ms: Number(b.delay_ms) } : {}) };
        faults.push(f);
        return send(res, 201, f);
      }
    }
    if (req.method === 'POST' && path === '/_disconnect') {
      const n = connections.size;
      for (const c of [...connections]) c.close(1012, 'the mock Ting closed every connection (POST /_disconnect)');
      return send(res, 200, { closed: n });
    }
    if (req.method === 'GET' && path === '/_connections') {
      return send(res, 200, { count: connections.size, items: [...connections].map((c) => ({ receiver_id: c.id, connected_at: c.connectedAt, frames_in: c.framesIn, frames_out: c.framesOut })) });
    }
    if (req.method === 'POST' && path === '/_reset') {
      tings.length = 0;
      byKey.clear();
      requests.length = 0;
      faults = [];
      recipients.clear();
      return send(res, 200, { reset: true });
    }
    return send(res, 404, { error: { code: 'not_found', message: `No route ${req.method} ${path}.` } });
  } catch (error) {
    return send(res, 500, { error: { code: 'internal', message: (error as Error).message } });
  }
});

server.on('upgrade', upgrade);
server.on('clientError', (_err, socket) => socket.destroy());

server.listen(PORT, HOST, () => {
  const addr = server.address();
  const port = typeof addr === 'object' && addr ? addr.port : PORT;
  const info = { url: `http://${HOST}:${port}`, ws_url: `ws://${HOST}:${port}/v1/ws?protocol=v1`, port, accounts_api: ACCOUNTS_API, app_id: APP_ID, issuers: ISSUERS, scope: SCOPE };
  if (args['ready-file']) writeFileSync(args['ready-file'], `${JSON.stringify(info)}\n`);
  console.log(`mock-ting ready ${JSON.stringify(info)}`);
});

function shutdown(): void {
  for (const c of [...connections]) c.close(1001, 'the mock Ting is stopping');
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 2000).unref();
}
process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
