// Writes e2e/stack.json for a running Silicon Accounts stack and checks that Calc's apps work on it:
// the calc app's sign-in setup (device flow, public client, redirect URIs), and Calc's App verification proof for
// Ting (receiving app ting, scope tings.send), issued with calc's credentials and verified with ting's.
//
//   node e2e/lib/write-stack.ts --out e2e/stack.json --apps e2e/stack/apps.json --public-url … --api-url … …
//
// scripts/accounts-stack.sh runs it after seeding. Exits 1 with the reason when a check fails.

import { readFileSync, renameSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { AccountsClient } from './testkit.ts';
import type { AppEntry, StackJson } from './stack.ts';

const { values: a } = parseArgs({
  options: {
    out: { type: 'string' },
    apps: { type: 'string' },
    'accounts-dir': { type: 'string' },
    'accounts-bin': { type: 'string' },
    'accounts-logs': { type: 'string' },
    'public-url': { type: 'string' },
    'api-url': { type: 'string' },
    'oidc-port': { type: 'string' },
    'messaging-port': { type: 'string' },
    'fake-apps-port': { type: 'string' },
    'iris-port': { type: 'string' },
    'apps-api-port': { type: 'string' },
    'database-url': { type: 'string' },
  },
});

function need(name: keyof typeof a): string {
  const v = a[name];
  if (typeof v !== 'string' || !v) {
    console.error(`error: --${name} is required`);
    process.exit(2);
  }
  return v;
}

interface SeedApp {
  app_id: string;
  secret: string;
  signin_defaults: { redirect_uris?: string[]; allowed_origins?: string[]; device_flow?: boolean; public_client?: boolean };
}

const seed = JSON.parse(readFileSync(need('apps'), 'utf8')) as { apps: SeedApp[] };
const entry = (id: string): AppEntry => {
  const app = seed.apps.find((x) => x.app_id === id);
  if (!app) throw new Error(`${need('apps')} has no app '${id}'`);
  return {
    app_id: app.app_id,
    app_secret: app.secret,
    redirect_uris: app.signin_defaults.redirect_uris ?? [],
    allowed_origins: app.signin_defaults.allowed_origins ?? [],
    device_flow: app.signin_defaults.device_flow ?? false,
    public_client: app.signin_defaults.public_client ?? false,
  };
};

const publicUrl = need('public-url').replace(/\/+$/, '');
const apiUrl = need('api-url').replace(/\/+$/, '');
const local = (port: string) => `http://127.0.0.1:${port}`;
const calcPort = process.env.CALC_PORT ?? '8730';
const webPort = process.env.CALC_WEB_PORT ?? '8731';
const tingPort = process.env.CALC_TING_PORT ?? '8735';

const stack: StackJson = {
  _comment:
    'The local stack for Silicon Calc (scripts/accounts-stack.sh writes it): Silicon Accounts with its mocks, Calc\'s apps on it (calc, ting) with their development-only secrets, and where the dev service, web app and mock Ting run. Local and fake.',
  written_at: new Date().toISOString(),
  accounts: {
    public_url: publicUrl,
    api_url: apiUrl,
    mock_oidc_url: local(need('oidc-port')),
    mock_messaging_url: local(need('messaging-port')),
    fake_apps_url: local(need('fake-apps-port')),
    mock_iris_url: local(need('iris-port')),
    apps_api_url: local(need('apps-api-port')),
    database_url: need('database-url'),
    checkout: need('accounts-dir'),
    cli: `${need('accounts-bin')}/silicon-accounts`,
    logs: need('accounts-logs'),
  },
  apps: { calc: entry('calc'), ting: entry('ting') },
  calc: {
    api_url: local(calcPort),
    web_url: local(webPort),
    database_url: `postgres://postgres@127.0.0.1:${process.env.CALC_PGPORT ?? '5482'}/silicon_calc`,
    blob_dir: '.dev/blobs',
  },
  ting: {
    url: local(tingPort),
    ws_url: `ws://127.0.0.1:${tingPort}/v1/ws?protocol=v1`,
    app_id: 'ting',
    issuing_app: 'calc',
    scope: 'tings.send',
  },
};

// ---- checks against the running stack
const accounts = new AccountsClient(apiUrl, { forwardedFor: null });
const problems: string[] = [];

const meta = (await accounts.meta()) as unknown as Record<string, unknown>;
if (String(meta.public_url ?? '').replace(/\/+$/, '') !== publicUrl) {
  problems.push(`accounts-api says its public URL is ${String(meta.public_url)}, not ${publicUrl}`);
}

const calc = accounts.app(stack.apps.calc.app_id, stack.apps.calc.app_secret);
const details = (await calc.details()) as { signin_config?: Record<string, unknown>; status?: string };
const cfg = details.signin_config ?? {};
if (details.status !== 'active') problems.push(`the calc app is ${String(details.status)}, not active`);
if (cfg.device_flow !== true) problems.push('the calc app has the device flow off (the calc CLI signs Carbons in with it)');
if (cfg.public_client !== true) problems.push('the calc app is not a public client (the calc CLI exchanges SLTs without a secret)');
const redirects = new Set((cfg.redirect_uris as string[] | undefined) ?? []);
for (const uri of stack.apps.calc.redirect_uris) if (!redirects.has(uri)) problems.push(`the calc app does not allow the redirect URI ${uri}`);

const issued = await calc.issueAppVerification({ receiving_app: stack.apps.ting.app_id, scopes: [stack.ting.scope] });
if (issued.status !== 201) {
  problems.push(`calc could not get an App verification proof for ting: HTTP ${issued.status} ${issued.text.slice(0, 300)}`);
} else {
  const token = (issued.body as { proof_token?: string }).proof_token ?? '';
  const ting = accounts.app(stack.apps.ting.app_id, stack.apps.ting.app_secret);
  const v = (await ting.verifyProof(token)) as unknown as Record<string, any>;
  if (v.valid !== true) problems.push(`ting does not accept calc's proof: ${JSON.stringify(v)}`);
  else {
    if (v.kind !== 'app_verification') problems.push(`calc's proof is a ${v.kind} proof, not app_verification`);
    if (v.issuing_app?.app_id !== 'calc') problems.push(`calc's proof names ${JSON.stringify(v.issuing_app)} as its issuing app`);
    if (!(v.scopes ?? []).includes(stack.ting.scope)) problems.push(`calc's proof has scopes ${JSON.stringify(v.scopes)}, without ${stack.ting.scope}`);
  }
}

if (problems.length) {
  for (const p of problems) console.error(`error: ${p}`);
  process.exit(1);
}

const out = need('out');
writeFileSync(`${out}.tmp`, `${JSON.stringify(stack, null, 2)}\n`);
renameSync(`${out}.tmp`, out);
console.log(`write-stack: calc and ting work on ${publicUrl}; wrote ${out}`);
