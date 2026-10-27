# Calc web production

The web app is a Next.js backend for the browser. It stores Accounts tokens in a sealed, HTTP-only cookie and calls
the Calc API itself. Browser requests, file transfers and live event streams all use the web origin through `/api/*`.
JavaScript commands run inside an isolated browser worker; the API only accepts their validated table results.

## Build

Build on Linux for the architecture that will run the image. The existing macOS `.next` directory is local test output.
From the Calc repository root:

```sh
docker build -f web/Dockerfile -t silicon-calc-web:<release> web
```

The image pins Node 24.21.0 and pnpm 10.33.0, installs the lockfile, runs TypeScript/lint/unit checks, and creates a
Next standalone build. It runs as the unprivileged `node` account and listens on port 8731. `.dockerignore` excludes
local environment files, dependencies, test sessions and build output. No Accounts or session secret is needed at
build time. Supply runtime values through the deployment's secret environment file.

For the native Linux deployment, run from the secret-free source checkout with Node and pnpm on `PATH`:

```sh
PATH=/opt/node/bin:$PATH sh deploy/package-web.sh /opt/silicon-calc/releases/<release>/web
```

The script installs the lockfile, runs TypeScript/lint/unit checks and a standalone build, verifies matching build
ids and all symlinks, then atomically stages a new destination. `--prebuilt` skips those checks/build when packaging
an already verified Linux build. It rejects local `.env` files, existing destinations, broken/absolute/escaping
symlinks and environment files in the release. `deploy/calc-web.service` runs the result as `calc-web` using
`/opt/node/bin/node` and `/etc/silicon-calc/web.env`. The installer must create that service account, keep the
environment file root-readable, and ensure the account can read the immutable release files.
The unit permits `/var/cache/silicon-calc-web` for runtime cache writes; the installer may link the installed
release's `.next/cache` there after validating the packaged release. Packaged symlinks must remain inside the release.

For manual packaging, `NEXT_OUTPUT=standalone pnpm build` creates `.next/standalone/server.js`. Copy the complete
standalone tree with symlinks preserved, `.next/static` into its `.next/static`, and `public` into its `public`.
Verify `.next/BUILD_ID` matches `.next/standalone/.next/BUILD_ID`, and reject any `.env*` file in the packaged tree.
Run `node server.js` with Node 24 or newer, `HOSTNAME=127.0.0.1`, and `PORT=8731` behind the HTTPS proxy. Build from a
clean source tree without local `.env` files, since Next can copy those into standalone output.

## Runtime environment and Accounts registration

| Variable | Production value |
| --- | --- |
| `NODE_ENV` | `production` (set in the image) |
| `NEXT_TELEMETRY_DISABLED` | `1` (set in the image) |
| `APP_ID` | `calc` |
| `APP_SECRET` | Calc's production Accounts app secret, supplied only to the server |
| `SESSION_SECRET` | At least 32 random bytes, persisted across restarts; rotating it signs browsers out |
| `PUBLIC_URL` | `https://calc.teamofsilicons.com` |
| `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` |
| `ACCOUNTS_API_URL` | Omit to use `ACCOUNTS_URL`, or set the production Accounts API's HTTPS address; loopback HTTP is accepted when on the same host |
| `APP_API_URL` | The private Calc API URL reachable from this process, for example `http://calc-api:8730` in a container network or `http://127.0.0.1:8730` for native services |
| `EXTRA_IMG_ORIGINS` | Usually empty; Accounts and `https://iris.teamofsilicons.com` are already allowed |
| `EXTRA_ORIGINS` | Empty for one canonical domain |
| `PORT` | `8731` |
| `HOSTNAME` | `0.0.0.0` in a private container network; `127.0.0.1` for a native service |

Register the exact redirect URI **`https://calc.teamofsilicons.com/auth/callback`** on the production `calc` app.
The hosted authorize request uses PKCE S256, `response_type=code`, and the `email` scope. Server-side token exchange,
refresh and revocation use the app's HTTP Basic credentials. HTTPS enables `__Host-calc_session` and
`__Host-calc_signin` cookies with Secure, HTTP-only and SameSite=Lax attributes. Keep `PUBLIC_URL` canonical: sign-in
requests arriving on a different host redirect there before setting their pending cookie.

No `NEXT_PUBLIC_*`, separate browser API origin, CORS exception, or browser Accounts credential is required. Do not
copy the local `.env.local` into production. A container's loopback address refers to that container, so it cannot
reach a separate API container through `127.0.0.1`.

## HTTPS proxy requirements

- Send browser `/api/*` requests to the Next server, preserving the path. Next removes `/api/` and authenticates the
  request before forwarding it. Direct CLI/API routes such as `/v1/*` may be routed separately by infrastructure.
- Preserve `Host` and set trusted `X-Forwarded-Host`, `X-Forwarded-Proto` and `X-Forwarded-For`. The public app uses
  HTTPS while its private Next listener can use HTTP. `skipProxyUrlNormalize` is enabled for this arrangement.
- Stream event responses immediately; disable response buffering and do not impose a short idle timeout. The Next
  upstream call has a five-minute limit and the browser reconnects the event stream. File import/export requests
  may run for up to five minutes. Preserve the API's upload/body limits instead of adding a smaller web limit.
- Honor `private, no-store` on API/auth/pages. Hashed `/_next/static/*` and versioned fonts may be cached. Do not cache
  cookies, authenticated pages, event streams or `/code-sandbox` responses in a shared cache.
- Preserve the web application's route-specific security headers. Ordinary pages have a nonce CSP and
  `X-Frame-Options: DENY`; **`/code-sandbox` must keep its own CSP and must not receive a blanket frame-denial header**.
  The parent allows this same-origin URL as a frame. Its `sandbox="allow-scripts"` creates an opaque origin, and its
  CSP permits only the inline bridge and Blob worker while `connect-src 'none'` blocks network access. Neither the
  app nor the command sandbox needs `unsafe-eval`. Do not add `allow-same-origin` or loosen the sandbox CSP.

One web process is the initial deployment topology. Session refresh deduplication is currently in process memory;
multiple replicas require a coordinated refresh mechanism or a verified affinity strategy before scaling.

## Release verification

`/robots.txt` is the container liveness check; it does not prove that the API or Accounts can be reached. Before
switching traffic, request the candidate with the production `Host` and `X-Forwarded-Proto: https`, then verify `/`,
`/sign-in`, `/sheets` redirecting to sign-in, `/code-sandbox`'s CSP, static assets and `/api/v1/meta` through the BFF.
Run `CALC_WEB_CHECK_URL=http://127.0.0.1:8731 node scripts/check-production.mjs` against the candidate. The script
sends the production proxy headers and checks public redirects, cookie attributes, PKCE, HSTS, both CSPs and CSRF
rejection without exchanging a sign-in code. Set `CALC_WEB_CHECK_URL=https://calc.teamofsilicons.com` after the switch
to check the public proxy too; this script is available in the source checkout, not the minimal runtime image.

After the switch, complete hosted Accounts sign-in on the public domain, create/edit a sheet, observe a second-tab
update, run and save a JavaScript command, and verify stop/timeout and blocked network access through the public proxy.
Keep the prior image or standalone directory and the same session secret for a reversible rollout.

Local evidence from the current implementation: TypeScript/lint/build and 51 unit tests passed; the real local API
and Accounts browser suite passed 26 checks including five accessibility journeys, followed by the command suite
after presentation-only polish. This evidence does not establish a deployed Linux image or a public-domain sign-in.
