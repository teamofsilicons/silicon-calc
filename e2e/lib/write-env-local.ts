// Writes web/.env.local for the web app from e2e/stack.json (scripts/dev.sh and scripts/e2e.sh run it):
// the web kit's settings (web/.env.example) pointed at the local Silicon Accounts stack and calc-api.
// Keys the file already has that aren't set here are kept, so settings added by hand to a file this wrote survive.
//
//   node e2e/lib/write-env-local.ts --out web/.env.local --stack e2e/stack.json --session-secret-file .dev/web-session-secret \
//        --public-url http://127.0.0.1:8731 --api-url http://127.0.0.1:8730

import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

const { values: a } = parseArgs({
  options: {
    out: { type: 'string' },
    stack: { type: 'string' },
    'session-secret-file': { type: 'string' },
    'public-url': { type: 'string' },
    'api-url': { type: 'string' },
    'ting-url': { type: 'string' },
  },
});

for (const k of ['out', 'stack', 'session-secret-file', 'public-url', 'api-url'] as const) {
  if (!a[k]) {
    console.error(`error: --${k} is required`);
    process.exit(2);
  }
}

const stack = JSON.parse(readFileSync(a.stack!, 'utf8'));
const secret = readFileSync(a['session-secret-file']!, 'utf8').trim();

const values: Record<string, string> = {
  APP_ID: stack.apps.calc.app_id,
  APP_SECRET: stack.apps.calc.app_secret,
  ACCOUNTS_URL: stack.accounts.public_url,
  ACCOUNTS_API_URL: stack.accounts.api_url,
  APP_API_URL: a['api-url']!,
  SESSION_SECRET: secret,
  PUBLIC_URL: a['public-url']!,
  EXTRA_IMG_ORIGINS: stack.accounts.mock_iris_url,
  MOCK_MESSAGING_URL: stack.accounts.mock_messaging_url,
  MOCK_TING_URL: a['ting-url'] ?? stack.ting.url,
  TEST_STACK_JSON: a.stack!,
};

// Keep what an earlier file (written here) had beyond these keys.
const kept: string[] = [];
if (existsSync(a.out!)) {
  for (const line of readFileSync(a.out!, 'utf8').split('\n')) {
    const m = /^\s*([A-Z_][A-Z0-9_]*)\s*=/.exec(line);
    if (m && !(m[1]! in values)) kept.push(line);
  }
}

const lines = [
  '# Written by scripts/dev.sh from e2e/stack.json: the local Silicon Accounts stack, calc-api and the mock Ting.',
  '# Development only. Delete this line to keep your own copy (scripts/dev.sh then leaves the file alone).',
  ...Object.entries(values).map(([k, v]) => `${k}=${v}`),
  ...kept,
  '',
];
writeFileSync(`${a.out}.tmp`, lines.join('\n'), { mode: 0o600 });
renameSync(`${a.out}.tmp`, a.out!);
