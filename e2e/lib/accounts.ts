// Test accounts on the local Silicon Accounts stack, and their access tokens for Calc.
//
//   const env = new Env();
//   const ada = await newCarbon(env, { name: 'Ada' });              // signs up on the hosted pages (email code)
//   const t = await carbonCalcTokens(env, ada);                      // signs in to calc: hosted authorize flow + PKCE
//   const d = await carbonCalcTokensDevice(env, ada);                // or calc's device flow, approved by Ada's browser
//   const si = await newSilicon(env, ada, { name: 'Analyst' });      // Ada creates it as its custodian: an STK
//   const s = await siliconCalcTokens(env, si);                      // silicon_login → SLT for calc → exchange
//
// Every Carbon keeps its signed-in browser (cookie session at the account site), so it can approve device sign-ins,
// sign in to calc again ("continue as") and manage its Silicons. Codes come from the stack's mock email outbox.

import { randomBytes } from 'node:crypto';
import type { Env } from './stack.ts';
import {
  type AccountSession,
  type BrowserSession,
  type TokenResponse,
  createPkcePair,
  driveFlow,
  randomNonce,
  randomState,
  signUpCarbon,
} from './testkit.ts';

export interface Carbon {
  kind: 'carbon';
  uuid: string;
  /** `c:…` */
  id: string;
  display_name: string;
  email: string;
  /** The Carbon's browser at the account site (cookie session). */
  browser: BrowserSession;
  /** Account-site calls as the Carbon (its own account, its Silicons). */
  account: AccountSession;
}

export interface Silicon {
  kind: 'silicon';
  uuid: string;
  /** `si:…` */
  id: string;
  display_name: string;
  /** The Silicon's STK, shown once when its custodian created it. */
  stk: string;
  custodian: { uuid: string; id: string };
}

/** What Silicon Accounts answers when an account signs in to calc. */
export interface CalcTokens {
  access_token: string;
  refresh_token: string | null;
  expires_in: number;
  scope: string;
  account: Record<string, unknown> & { uuid: string; id: string; kind: string };
  /** The access token's claims (unverified: the service verifies them). */
  claims: Record<string, unknown> & { sub: string; aud: string | string[]; iss: string; exp: number };
}

/** A short random handle part: lowercase letters and digits. */
export function tag(n = 6): string {
  const alphabet = 'abcdefghijklmnopqrstuvwxyz0123456789';
  const bytes = randomBytes(n);
  let out = '';
  for (const b of bytes) out += alphabet[b % alphabet.length];
  return out;
}

export function decodeJwt(token: string): Record<string, any> {
  const part = token.split('.')[1];
  if (!part) throw new Error('not a JWT');
  return JSON.parse(Buffer.from(part, 'base64url').toString('utf8'));
}

function calcTokens(t: TokenResponse): CalcTokens {
  const raw = t as unknown as Record<string, any>;
  return {
    access_token: raw.access_token,
    refresh_token: raw.refresh_token ?? null,
    expires_in: raw.expires_in,
    scope: raw.scope ?? '',
    account: raw.account,
    claims: decodeJwt(raw.access_token) as CalcTokens['claims'],
  };
}

/**
 * A new Carbon: signs up on the account site's hosted pages with an email code (read from the mock outbox).
 * `handle` makes the id `c:<handle>` (3-30 of a-z 0-9 - _); the default is a readable name plus a random tag.
 */
export async function newCarbon(env: Env, opts: { name?: string; handle?: string; email?: string } = {}): Promise<Carbon> {
  const name = opts.name ?? 'Ada';
  const handle = opts.handle ?? `${name.toLowerCase().replace(/[^a-z0-9]/g, '').slice(0, 12) || 'carbon'}${tag()}`;
  const email = opts.email ?? `${handle}@calc-e2e.test`;
  const made = await signUpCarbon({
    accounts: env.accounts,
    messaging: env.messaging,
    email,
    signup: { id: `c:${handle}`, display_name: name, timezone: 'Asia/Kolkata', dob: '1990-01-01' },
  });
  return {
    kind: 'carbon',
    uuid: made.me.uuid,
    id: made.me.id,
    display_name: String(made.me.display_name ?? name),
    email,
    browser: made.browser,
    account: made.account,
  };
}

/** Signs an existing Carbon in on a fresh browser with an email code (when only its email is known). */
export async function signInCarbon(env: Env, email: string): Promise<Carbon> {
  const made = await signUpCarbon({ accounts: env.accounts, messaging: env.messaging, email });
  return {
    kind: 'carbon',
    uuid: made.me.uuid,
    id: made.me.id,
    display_name: String(made.me.display_name ?? ''),
    email,
    browser: made.browser,
    account: made.account,
  };
}

/**
 * Signs the Carbon in to calc through the hosted pages, the way Calc's web app does: /authorize with PKCE S256 to
 * one of calc's redirect URIs; the Carbon's browser continues as itself, shares its email when asked (`share`,
 * default ['email']) and approves; the code is exchanged with calc's credentials (`confidential`, the web app's
 * server) or as the public client with the PKCE verifier alone (a CLI with a loopback redirect).
 */
export async function carbonCalcTokens(
  env: Env,
  carbon: Carbon,
  opts: { redirectUri?: string; share?: string[]; confidential?: boolean } = {},
): Promise<CalcTokens> {
  const redirectUri = opts.redirectUri ?? env.calc.redirect_uris[0]!;
  const pkce = createPkcePair('S256');
  const state = randomState();
  let flow = await carbon.browser.createFlow({
    app_id: env.calc.app_id,
    redirect_uri: redirectUri,
    state,
    code_challenge: pkce.code_challenge,
    code_challenge_method: 'S256',
    nonce: randomNonce(),
    scope: 'email',
    timezone: 'Asia/Kolkata',
  });
  flow = await driveFlow(carbon.browser, flow, { messaging: env.messaging, share: opts.share ?? ['email'] });
  if (flow.step !== 'complete' || !flow.redirect_to) {
    throw new Error(`calc's sign-in for ${carbon.id} ended at ${flow.step}${flow.error ? `: ${flow.error.code} ${flow.error.message}` : ''}`);
  }
  const back = new URL(flow.redirect_to);
  if (back.searchParams.get('state') !== state) throw new Error(`calc's sign-in came back with another state (${back.searchParams.get('state')})`);
  const code = back.searchParams.get('code');
  if (!code) throw new Error(`calc's sign-in came back without a code: ${flow.redirect_to}`);
  if (opts.confidential === false) {
    const res = await env.accounts.http.post('/v1/oauth/token', {
      form: { grant_type: 'authorization_code', code, redirect_uri: redirectUri, code_verifier: pkce.code_verifier, client_id: env.calc.app_id },
    });
    if (res.status !== 200) throw new Error(`exchanging calc's code as the public client failed: HTTP ${res.status} ${res.text.slice(0, 300)}`);
    return calcTokens(res.body as TokenResponse);
  }
  return calcTokens(await env.calcApp().exchangeCode(code, redirectUri, pkce.code_verifier));
}

/** Starts calc's device flow the way the calc CLI does (`client_id=calc`, no secret). */
export async function startCalcDevice(env: Env, label = 'calc e2e'): Promise<{ device_code: string; user_code: string; verification_uri: string; interval: number }> {
  const res = await env.accounts.http.post('/v1/device/authorize', { json: { client_id: env.calc.app_id, scope: 'email', client_label: label } });
  if (res.status !== 200) throw new Error(`calc's device sign-in did not start: HTTP ${res.status} ${res.text.slice(0, 300)}`);
  return res.body as { device_code: string; user_code: string; verification_uri: string; interval: number };
}

/** The Carbon approves a device sign-in (what it does on the account site's /device page). */
export async function approveDevice(carbon: Carbon, userCode: string): Promise<void> {
  const shown = await carbon.browser.http.get(`/v1/device/${encodeURIComponent(userCode)}`);
  if (shown.status !== 200) throw new Error(`device code ${userCode} can't be looked at: HTTP ${shown.status} ${shown.text.slice(0, 300)}`);
  const res = await carbon.browser.http.post(`/v1/device/${encodeURIComponent(userCode)}/approve`);
  if (res.status !== 204) throw new Error(`approving device code ${userCode} failed: HTTP ${res.status} ${res.text.slice(0, 300)}`);
}

/** Polls calc's device sign-in until it has tokens (or fails). */
export async function pollCalcDevice(env: Env, deviceCode: string, timeoutMs = 60_000): Promise<CalcTokens> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const res = await env.accounts.http.post('/v1/oauth/token', {
      form: { grant_type: 'urn:ietf:params:oauth:grant-type:device_code', device_code: deviceCode, client_id: env.calc.app_id },
    });
    if (res.status === 200) return calcTokens(res.body as TokenResponse);
    const error = (res.body as { error?: string } | null)?.error;
    if (error !== 'authorization_pending' && error !== 'slow_down') {
      throw new Error(`calc's device sign-in failed: HTTP ${res.status} ${res.text.slice(0, 300)}`);
    }
    if (Date.now() > deadline) throw new Error(`calc's device sign-in was not approved within ${timeoutMs} ms`);
    await new Promise((r) => setTimeout(r, error === 'slow_down' ? 1500 : 300));
  }
}

/** Signs the Carbon in to calc with the device flow, approved by the Carbon's own browser session. */
export async function carbonCalcTokensDevice(env: Env, carbon: Carbon): Promise<CalcTokens> {
  const started = await startCalcDevice(env);
  await approveDevice(carbon, started.user_code);
  return pollCalcDevice(env, started.device_code);
}

/** A new Silicon, created by `custodian` (its custodian from the start), with its STK. */
export async function newSilicon(env: Env, custodian: Carbon, opts: { name?: string; handle?: string } = {}): Promise<Silicon> {
  const name = opts.name ?? 'Analyst';
  const handle = opts.handle ?? `${name.toLowerCase().replace(/[^a-z0-9]/g, '').slice(0, 12) || 'silicon'}${tag()}`;
  const made = await custodian.account.createSilicon({ id: `si:${handle}`, display_name: name, timezone: 'UTC' });
  if (!made.stk) throw new Error(`Silicon Accounts created si:${handle} without returning its STK`);
  return {
    kind: 'silicon',
    uuid: made.silicon.uuid,
    id: made.silicon.id,
    display_name: String(made.silicon.display_name ?? name),
    stk: made.stk,
    custodian: { uuid: custodian.uuid, id: custodian.id },
  };
}

/** A Silicon's short-lived token for an app (default calc): silicon_login with its STK, then mint the SLT. */
export async function siliconSlt(env: Env, silicon: Pick<Silicon, 'id' | 'stk'>, appId = env.calc.app_id): Promise<string> {
  const login = (await env.accounts.siliconLogin(silicon.id, silicon.stk, 'calc e2e')) as unknown as Record<string, any>;
  const session = env.accounts.withToken(login.access_token);
  const minted = await session.shortLivedToken(appId);
  return minted.slt;
}

/** Exchanges a Silicon's SLT at calc as the public client (`client_id=calc`, no secret), like the calc CLI. */
export async function exchangeSltPublic(env: Env, slt: string): Promise<CalcTokens> {
  const res = await env.accounts.http.post('/v1/oauth/token', {
    form: { grant_type: 'urn:silicon:params:oauth:grant-type:slt', slt, client_id: env.calc.app_id },
  });
  if (res.status !== 200) throw new Error(`exchanging the SLT at calc failed: HTTP ${res.status} ${res.text.slice(0, 300)}`);
  return calcTokens(res.body as TokenResponse);
}

/** A Silicon's access token for calc: silicon_login → SLT for calc → exchange (public client). */
export async function siliconCalcTokens(env: Env, silicon: Pick<Silicon, 'id' | 'stk'>): Promise<CalcTokens> {
  return exchangeSltPublic(env, await siliconSlt(env, silicon));
}

/** Rotates a refresh token of calc's public client (the CLI's way to stay signed in). */
export async function refreshCalcTokens(env: Env, refreshToken: string): Promise<CalcTokens> {
  const res = await env.accounts.http.post('/v1/oauth/token', {
    form: { grant_type: 'refresh_token', refresh_token: refreshToken, client_id: env.calc.app_id },
  });
  if (res.status !== 200) throw new Error(`refreshing calc's tokens failed: HTTP ${res.status} ${res.text.slice(0, 300)}`);
  return calcTokens(res.body as TokenResponse);
}
