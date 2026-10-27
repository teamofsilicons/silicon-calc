/** Read-only routing/header smoke test; no sign-in code is exchanged and no credentials are required. */
import assert from "node:assert/strict";

const base = new URL(process.env.CALC_WEB_CHECK_URL || "http://127.0.0.1:8731");
const publicUrl = new URL(process.env.CALC_WEB_PUBLIC_URL || "https://calc.teamofsilicons.com");
const accountsUrl = new URL(process.env.CALC_WEB_ACCOUNTS_URL || "https://accounts.teamofsilicons.com");
const headers = { Host: publicUrl.host, "X-Forwarded-Host": publicUrl.host, "X-Forwarded-Proto": publicUrl.protocol.slice(0, -1) };
const request = async (path, options = {}) => {
  const response = await fetch(new URL(path, base), { ...options, headers: { ...headers, ...options.headers }, redirect: "manual", signal: AbortSignal.timeout(10_000) });
  await response.body?.cancel();
  return response;
};

const landing = await request("/");
assert.equal(landing.status, 200, "signed-out landing should render");
assert.match(landing.headers.get("strict-transport-security"), /max-age/, "HTTPS pages should set HSTS");
assert.match(landing.headers.get("content-security-policy"), /frame-src 'self'/);
assert.ok(!landing.headers.get("content-security-policy").includes("unsafe-eval"));

const sheets = await request("/sheets");
assert.equal(sheets.status, 307);
const destination = new URL(sheets.headers.get("location"), publicUrl);
assert.equal(destination.origin, publicUrl.origin, "workspace redirect should keep the public origin");
assert.equal(destination.pathname, "/auth/sign-in");

const signin = await request("/auth/sign-in");
assert.equal(signin.status, 303);
const authorization = new URL(signin.headers.get("location"));
assert.equal(authorization.origin, accountsUrl.origin);
assert.equal(authorization.searchParams.get("app_id"), "calc");
assert.equal(authorization.searchParams.get("redirect_uri"), new URL("/auth/callback", publicUrl).href);
assert.equal(authorization.searchParams.get("code_challenge_method"), "S256");
const cookie = signin.headers.get("set-cookie");
assert.match(cookie, /__Host-calc_signin=/);
assert.match(cookie, /Secure/i);
assert.match(cookie, /HttpOnly/i);

const sandbox = await request("/code-sandbox");
assert.equal(sandbox.status, 200);
assert.equal(sandbox.headers.get("x-frame-options"), null, "sandbox must not inherit a blanket frame-denial header");
assert.match(sandbox.headers.get("content-security-policy"), /connect-src 'none'/);
assert.match(sandbox.headers.get("content-security-policy"), /worker-src blob:/);
assert.ok(!sandbox.headers.get("content-security-policy").includes("unsafe-eval"));
assert.equal(sandbox.headers.get("cache-control"), "no-store");

const rejected = await request("/api/v1/sheets", { method: "POST", headers: { Origin: "https://other.invalid" }, body: "{}" });
assert.equal(rejected.status, 403, "cross-origin writes must be rejected before they reach the API");
console.log("Production routing passed: canonical redirects, PKCE callback, Secure host cookies, HSTS, page and worker CSP, and CSRF rejection.");
