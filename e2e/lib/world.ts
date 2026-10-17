// Everything a journey works with: the local stack (e2e/stack.json), the Calc service under test, the mock Ting,
// the calc command line, and real accounts on Silicon Accounts with calc access tokens that renew themselves.
//
//   const w = world();
//   const ada = await w.carbon('Ada');                  // signs up with an email code, signs in to calc
//   const analyst = await w.silicon(ada, 'Analyst');    // Ada makes it (its custodian); it has an STK
//   await ada.api.post('/v1/sheets', { json: { title: 'Q3' } });
//   await analyst.cli.loginSilicon(analyst.id, analyst.stk);
//
// Settings: CALC_API_URL (default e2e/stack.json calc.api_url), MOCK_TING_URL (stack.json ting.url), CALC_CLI (the
// calc binary; default target/e2e/debug/calc), CALC_WEB_URL, STACK_JSON, E2E_DIR (where journeys keep files;
// default .dev/e2e).

import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { type Carbon, type Silicon, carbonCalcTokens, newCarbon, newSilicon, siliconCalcTokens, tag } from './accounts.ts';
import { CalcApi } from './calc.ts';
import { Cli } from './cli.ts';
import { Env, ROOT } from './stack.ts';
import { MockTing } from './ting.ts';

/** A calc access token that renews itself shortly before it expires. */
function renewing(get: () => Promise<{ access_token: string; expires_in: number }>): () => Promise<string> {
  let token: string | null = null;
  let until = 0;
  let pending: Promise<string> | null = null;
  return async () => {
    if (token && Date.now() < until) return token;
    pending ??= (async () => {
      const t = await get();
      token = t.access_token;
      until = Date.now() + Math.max(30, t.expires_in - 120) * 1000;
      return token;
    })().finally(() => {
      pending = null;
    });
    return pending;
  };
}

export interface Person {
  kind: 'carbon' | 'silicon';
  uuid: string;
  /** `c:…` or `si:…` */
  id: string;
  display_name: string;
  /** The service as this account. */
  api: CalcApi;
  /** The calc command line as this account (sign in first). */
  cli: Cli;
}

export interface CarbonPerson extends Person {
  kind: 'carbon';
  email: string;
  /** The Carbon's browser session at the account site (approves device sign-ins, signs in again). */
  carbon: Carbon;
}

export interface SiliconPerson extends Person {
  kind: 'silicon';
  stk: string;
  silicon: Silicon;
}

export class World {
  readonly env: Env;
  readonly apiUrl: string;
  readonly webUrl: string;
  readonly ting: MockTing;
  readonly cliBin: string;
  readonly dir: string;
  /** Every account made in this run, for the summary. */
  readonly people: Person[] = [];

  constructor() {
    this.env = new Env();
    const s = this.env.stack;
    this.apiUrl = (process.env.CALC_API_URL ?? s.calc.api_url).replace(/\/+$/, '');
    this.webUrl = (process.env.CALC_WEB_URL ?? s.calc.web_url).replace(/\/+$/, '');
    this.ting = new MockTing(process.env.MOCK_TING_URL ?? s.ting.url);
    this.cliBin = process.env.CALC_CLI ?? join(ROOT, 'target', 'e2e', 'debug', 'calc');
    this.dir = process.env.E2E_DIR ?? join(ROOT, '.dev', 'e2e');
    mkdirSync(this.dir, { recursive: true });
  }

  /** The service, signed out. */
  get anonymous(): CalcApi {
    return new CalcApi(this.apiUrl, null, 'nobody (signed out)');
  }

  /** A scratch directory for one journey's files. */
  scratch(name: string): string {
    const d = join(this.dir, 'scratch', `${name}-${tag(4)}`);
    mkdirSync(d, { recursive: true });
    return d;
  }

  private cliFor(id: string): Cli {
    return new Cli({
      bin: this.cliBin,
      home: join(this.dir, 'cli-homes', id.replace(/[^a-z0-9]+/gi, '-')),
      apiUrl: this.apiUrl,
      accountsUrl: this.env.stack.accounts.public_url,
      who: id,
    });
  }

  /** A new Carbon, signed up on the hosted pages with an email code and signed in to calc. */
  async carbon(name: string, o: { email?: string; handle?: string } = {}): Promise<CarbonPerson> {
    const carbon = await newCarbon(this.env, { name, ...o });
    const token = renewing(() => carbonCalcTokens(this.env, carbon));
    await token();
    const p: CarbonPerson = {
      kind: 'carbon',
      uuid: carbon.uuid,
      id: carbon.id,
      display_name: carbon.display_name,
      email: carbon.email,
      carbon,
      api: new CalcApi(this.apiUrl, token, carbon.id),
      cli: this.cliFor(carbon.id),
    };
    this.people.push(p);
    return p;
  }

  /** A new Silicon made by `custodian`, with its STK and a calc access token (silicon_login → SLT → exchange). */
  async silicon(custodian: CarbonPerson, name: string): Promise<SiliconPerson> {
    const silicon = await newSilicon(this.env, custodian.carbon, { name });
    const token = renewing(() => siliconCalcTokens(this.env, silicon));
    await token();
    const p: SiliconPerson = {
      kind: 'silicon',
      uuid: silicon.uuid,
      id: silicon.id,
      display_name: silicon.display_name,
      stk: silicon.stk,
      silicon,
      api: new CalcApi(this.apiUrl, token, silicon.id),
      cli: this.cliFor(silicon.id),
    };
    this.people.push(p);
    return p;
  }
}

let shared: World | null = null;

/** The world of this run (one per process). */
export function world(): World {
  shared ??= new World();
  return shared;
}
