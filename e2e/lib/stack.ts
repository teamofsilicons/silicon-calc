// e2e/stack.json: where everything of the local stack is, and Calc's two apps on it (scripts/accounts-stack.sh
// writes it). Local and fake: the secrets only work on this stack.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { AccountsClient, MockMessagingClient, MockOidcClient } from './testkit.ts';

export const E2E_DIR = join(dirname(fileURLToPath(import.meta.url)), '..');
export const ROOT = join(E2E_DIR, '..');

export interface AppEntry {
  app_id: string;
  app_secret: string;
  redirect_uris: string[];
  allowed_origins: string[];
  device_flow: boolean;
  public_client: boolean;
}

export interface StackJson {
  _comment?: string;
  written_at: string;
  accounts: {
    public_url: string;
    api_url: string;
    mock_oidc_url: string;
    mock_messaging_url: string;
    fake_apps_url: string;
    mock_iris_url: string;
    apps_api_url: string;
    database_url: string;
    checkout: string;
    cli: string;
    logs: string;
  };
  apps: { calc: AppEntry; ting: AppEntry } & Record<string, AppEntry>;
  calc: {
    api_url: string;
    web_url: string;
    database_url: string;
    blob_dir: string;
  };
  ting: {
    url: string;
    ws_url: string;
    app_id: string;
    issuing_app: string;
    scope: string;
  };
}

export function stackPath(): string {
  return process.env.STACK_JSON ?? join(E2E_DIR, 'stack.json');
}

export function loadStack(path: string = stackPath()): StackJson {
  if (!existsSync(path)) {
    throw new Error(`${path} does not exist: the Silicon Accounts stack for Calc has not been set up. Run scripts/accounts-stack.sh start (or scripts/dev.sh).`);
  }
  return JSON.parse(readFileSync(path, 'utf8')) as StackJson;
}

/**
 * Clients for the stack. `forwardedFor: 'random'` gives each client its own X-Forwarded-For, so Silicon Accounts
 * counts its per-network code limits per client instead of for all of 127.0.0.1 (the stack trusts the header).
 */
export class Env {
  readonly stack: StackJson;
  readonly accounts: AccountsClient;
  readonly messaging: MockMessagingClient;
  readonly oidc: MockOidcClient;

  constructor(stack: StackJson = loadStack()) {
    this.stack = stack;
    this.accounts = new AccountsClient(stack.accounts.api_url, { forwardedFor: 'random' });
    this.messaging = new MockMessagingClient(stack.accounts.mock_messaging_url);
    this.oidc = new MockOidcClient(stack.accounts.mock_oidc_url);
  }

  get calc(): AppEntry {
    return this.stack.apps.calc;
  }

  get ting(): AppEntry {
    return this.stack.apps.ting;
  }

  /** Calc's own credentials (HTTP Basic calc:secret), as the web app's server and the service use them. */
  calcApp() {
    return this.accounts.app(this.calc.app_id, this.calc.app_secret);
  }

  /** Ting's credentials: the mock Ting verifies Calc's proofs with them. */
  tingApp() {
    return this.accounts.app(this.ting.app_id, this.ting.app_secret);
  }
}
