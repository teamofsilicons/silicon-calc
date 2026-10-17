// Test accounts on the local Silicon Accounts stack, from the command line (scripts/dev.sh uses it; so can people,
// the web app's browser tests and other scripts). Everything prints JSON on stdout; messages go to stderr.
//
//   node e2e/bin/accounts.ts carbon [--name Ada] [--handle ada7] [--token]
//        a new Carbon (signed up on the hosted pages with an email code); --token adds a calc access token
//   node e2e/bin/accounts.ts silicon [--custodian <carbon email>] [--name Analyst] [--handle analyst7] [--token]
//        a new Silicon with its STK, created by the custodian (a new Carbon when none is named)
//   node e2e/bin/accounts.ts token --silicon <si:id> [--stk <stk>]      a calc access token for a Silicon
//   node e2e/bin/accounts.ts token --carbon <email> [--device]          a calc access token for a Carbon (hosted
//        sign-in with an email code; --device: calc's device flow, approved by the Carbon)
//   node e2e/bin/accounts.ts slt --silicon <si:id> [--stk <stk>]        a fresh short-lived token for calc (2 minutes,
//        single use), what `calc` exchanges to sign a Silicon in; with --raw only the token is printed
//   node e2e/bin/accounts.ts approve --carbon <email> --code <user code> the Carbon approves a device sign-in (the
//        code the calc CLI shows)
//   node e2e/bin/accounts.ts dev-accounts [--file .dev/accounts.json]   the local test accounts: made once (a Carbon
//        with a Silicon, and a second Carbon), then reused; prints them with fresh calc access tokens
//
// STKs not given are looked up in the dev accounts file. Settings come from e2e/stack.json (STACK_JSON).

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { parseArgs } from 'node:util';
import {
  approveDevice,
  carbonCalcTokens,
  carbonCalcTokensDevice,
  newCarbon,
  newSilicon,
  pollCalcDevice,
  siliconCalcTokens,
  siliconSlt,
  signInCarbon,
  startCalcDevice,
  tag,
  type CalcTokens,
  type Carbon,
} from '../lib/accounts.ts';
import { Env, ROOT } from '../lib/stack.ts';

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    name: { type: 'string' },
    handle: { type: 'string' },
    token: { type: 'boolean' },
    custodian: { type: 'string' },
    silicon: { type: 'string' },
    carbon: { type: 'string' },
    stk: { type: 'string' },
    code: { type: 'string' },
    device: { type: 'boolean' },
    raw: { type: 'boolean' },
    file: { type: 'string' },
  },
});

const DEV_FILE = o.file ?? join(ROOT, '.dev', 'accounts.json');

interface DevAccounts {
  _comment: string;
  carbon: { uuid: string; id: string; display_name: string; email: string };
  second_carbon: { uuid: string; id: string; display_name: string; email: string };
  silicon: { uuid: string; id: string; display_name: string; stk: string; custodian: string };
}

function readDev(): DevAccounts | null {
  if (!existsSync(DEV_FILE)) return null;
  try {
    return JSON.parse(readFileSync(DEV_FILE, 'utf8')) as DevAccounts;
  } catch {
    return null;
  }
}

function out(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

function fail(message: string, hint?: string): never {
  process.stderr.write(`error: ${message}\n${hint ? `hint: ${hint}\n` : ''}`);
  process.exit(1);
}

function tokenView(t: CalcTokens) {
  return { access_token: t.access_token, refresh_token: t.refresh_token, expires_in: t.expires_in, scope: t.scope, aud: t.claims.aud, sub: t.claims.sub };
}

function carbonView(c: Carbon) {
  return { kind: 'carbon', uuid: c.uuid, id: c.id, display_name: c.display_name, email: c.email };
}

function stkFor(id: string): string {
  if (o.stk) return o.stk;
  const dev = readDev();
  if (dev?.silicon.id === id) return dev.silicon.stk;
  fail(`no STK for ${id}`, 'pass --stk (it was printed once when the Silicon was made)');
}

const env = new Env();
const command = positionals[0];

switch (command) {
  case 'carbon': {
    const c = await newCarbon(env, { ...(o.name ? { name: o.name } : {}), ...(o.handle ? { handle: o.handle } : {}) });
    out({ ...carbonView(c), ...(o.token ? { calc: tokenView(await carbonCalcTokens(env, c)) } : {}) });
    break;
  }
  case 'silicon': {
    const custodian = o.custodian ? await signInCarbon(env, o.custodian) : await newCarbon(env, { name: 'Custodian' });
    const s = await newSilicon(env, custodian, { ...(o.name ? { name: o.name } : {}), ...(o.handle ? { handle: o.handle } : {}) });
    out({ ...s, custodian: carbonView(custodian), ...(o.token ? { calc: tokenView(await siliconCalcTokens(env, s)) } : {}) });
    break;
  }
  case 'token': {
    if (o.silicon) {
      out(tokenView(await siliconCalcTokens(env, { id: o.silicon, stk: stkFor(o.silicon) })));
    } else if (o.carbon) {
      const c = await signInCarbon(env, o.carbon);
      out(tokenView(o.device ? await carbonCalcTokensDevice(env, c) : await carbonCalcTokens(env, c)));
    } else {
      fail('token needs --silicon <si:id> or --carbon <email>');
    }
    break;
  }
  case 'slt': {
    if (!o.silicon) fail('slt needs --silicon <si:id>');
    const slt = await siliconSlt(env, { id: o.silicon, stk: stkFor(o.silicon) });
    if (o.raw) process.stdout.write(`${slt}\n`);
    else out({ slt, app_id: env.calc.app_id, expires_in: 120 });
    break;
  }
  case 'approve': {
    if (!o.carbon || !o.code) fail('approve needs --carbon <email> --code <user code>');
    const c = await signInCarbon(env, o.carbon);
    await approveDevice(c, o.code);
    out({ approved: o.code, by: c.id });
    break;
  }
  case 'device': {
    // Starts calc's device flow and waits for its approval (for trying the flow by hand).
    const started = await startCalcDevice(env, 'accounts.ts');
    process.stderr.write(`approve ${started.user_code} at ${started.verification_uri} (or: node e2e/bin/accounts.ts approve --carbon <email> --code ${started.user_code})\n`);
    out(tokenView(await pollCalcDevice(env, started.device_code, 600_000)));
    break;
  }
  case 'dev-accounts': {
    let dev = readDev();
    if (dev) {
      // The stack's database may have been reset since: check that the accounts still exist.
      const lookup = await env.calcApp().request('GET', `/v1/accounts/${encodeURIComponent(dev.carbon.uuid)}`);
      if (lookup.status !== 200) {
        process.stderr.write(`accounts: ${dev.carbon.id} is gone from the stack; making new test accounts\n`);
        dev = null;
      }
    }
    if (!dev) {
      const t = tag(4);
      const ada = await newCarbon(env, { name: 'Ada Lovelace', handle: `ada${t}` });
      const grace = await newCarbon(env, { name: 'Grace Hopper', handle: `grace${t}` });
      const analyst = await newSilicon(env, ada, { name: 'Analyst', handle: `analyst${t}` });
      dev = {
        _comment: 'Local test accounts on the Silicon Accounts stack for Calc (scripts/dev.sh). Development only. Carbons sign in with their email; the code is in the mock outbox.',
        carbon: carbonView(ada),
        second_carbon: carbonView(grace),
        silicon: { uuid: analyst.uuid, id: analyst.id, display_name: analyst.display_name, stk: analyst.stk, custodian: ada.id },
      };
      mkdirSync(dirname(DEV_FILE), { recursive: true });
      writeFileSync(DEV_FILE, `${JSON.stringify(dev, null, 2)}\n`, { mode: 0o600 });
    }
    const siliconToken = await siliconCalcTokens(env, dev.silicon);
    out({ ...dev, file: DEV_FILE, silicon_calc_access_token: siliconToken.access_token, silicon_calc_refresh_token: siliconToken.refresh_token });
    break;
  }
  default:
    fail(`unknown command '${command ?? ''}'`, 'use carbon, silicon, token, slt, approve, device or dev-accounts (see the top of e2e/bin/accounts.ts)');
}
