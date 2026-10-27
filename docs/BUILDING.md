# Building Silicon Calc

How the repository is built, run and tested, and the rules every change follows. The contracts are:

- `understanding/UNDERSTANDING.md`: what Calc is. Carbon-only: never edit it.
- `docs/ARCHITECTURE.md`: how it is built.
- `docs/API.md`: the HTTP contract between the service, the client, the CLI and the web app.

## Layout and who owns what

| Path | What |
| --- | --- |
| `crates/calc-engine` | The spreadsheet engine (no I/O). `formula/functions.rs` is the function library; `formats/` reads and writes files. |
| `crates/calc-runner` | Trusted Parquet, sandboxed Python file conversion, LibreOffice and optional native Numbers conversions. |
| `crates/calc-ting` | The Silicon Ting publisher (contract in its `lib.rs`). |
| `crates/calc-api` | The service. `lib.rs` wires modules; `accounts`, `auth`, `perms`, `error`, `config`, `state`, `blobs`, `ids`, `db` are shared foundations. |
| `crates/calc-api/src/{sheets,folders,sharing,transfers,search,comments,notify}.rs` | Platform: sheets, folders, sharing, transfers, search, comments, the Ting sender. |
| `crates/calc-api/src/lines/` | Lines: open lines (one task each), cells, edits, live events, presence, versions, compare, restore, copies, merge. |
| `crates/calc-api/src/{code,files}.rs` | Browser command result commits; imports and downloads. |
| `crates/calc-api/tests/support/` | The test harness: a stand-in Silicon Accounts signing real EdDSA tokens, a fresh database per test, the service in-process. |
| `crates/silicon-calc-client` | The Rust client for `docs/API.md`. |
| `crates/calc-cli` | The `calc` command line. |
| `runner/` | Fixed conversion programs and pinned Python dependencies. |
| `web/` | The web app (Next.js from the Silicon web kit, Silicon UI). |
| `e2e/`, `scripts/` | The local stack (Silicon Accounts, mock Ting), the end-to-end journeys, `dev.sh`, `e2e.sh`. |

Functions marked **Contract** in `lines/mod.rs`, `code.rs`, `notify.rs` and `calc-ting` are called across modules:
keep their signatures (add, never change or remove). Database changes go in new migration files
(`migrations/<timestamp>_<what>.sql`, a timestamp later than every existing one); never edit an applied migration.

## Local environment

| What | Where |
| --- | --- |
| Postgres 16 (Calc's own cluster, trust auth, user `postgres`) | `127.0.0.1:5482`, data `.dev/pg`. Start: `/opt/homebrew/opt/postgresql@16/bin/pg_ctl -D .dev/pg -o "-p 5482 -k /tmp" -l .dev/pg.log start` |
| calc-api (dev) | `127.0.0.1:8730` |
| web app (dev) | `127.0.0.1:8731` |
| mock Ting | `127.0.0.1:8735` |
| Silicon Accounts stack for Calc (public site / API / mock OIDC / mock email / fake apps / mock Iris / Apps stand-in) | `9990` / `9989` / `9991` / `9992` / `9993` / `9994` / `9996` |
| Ports for your own test runs | `8740`–`8799` (pick from the range your work item was given) |

Never touch anything else that runs on this Mac: the Carbon's own Silicon Accounts stack (8589, 8590, 8600, Postgres
5444), other stacks (8790, 9590, Postgres 5460), other repositories (read them, never write), or processes you did not
start. Don't kill by name; kill the pids you started.

Database tests: `CALC_TEST_DATABASE_URL` (default `postgres://postgres@127.0.0.1:5482/postgres`) is the admin
connection; every test creates its own database (`tests/support::fresh_database`).

## Building

```sh
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR=target/api                 # reuse the existing target; coordinate concurrent builds
cargo build -p <crate> && cargo test -p <crate>
```

DuckDB's bundled build is large: check `df -h /` before big builds and reuse an existing target where possible.
Coordinate access to a shared target; a separate target duplicates dependencies and needs additional disk
space. Never delete someone else's build artifacts. macOS has no `timeout`; use
`perl -e 'alarm shift; exec @ARGV' <seconds> <cmd…>` to bound a command.

## Local startup and conversion dependencies

Run `scripts/setup-runner.sh` to prepare the pinned Python conversion environment. JavaScript commands run
in the browser; no server Node interpreter is configured. Install LibreOffice for XLS/ODS import and XLS/ODS/PDF export; set
`CALC_RUNNER_LIBREOFFICE` when its executable is outside the standard locations. Linux also needs `bubblewrap`.
See [`runner/README.md`](../runner/README.md) for sandbox and conversion limits.

After the service and CLI have been built, start the usual local ports without duplicating build artifacts:

```sh
scripts/dev.sh --no-build --restart
```

`--no-build` selects the newest existing service and CLI binaries from the known targets; `CALC_API_BIN` and
`CALC_BIN` can select exact binaries. The script prints the selected binary paths and build times. It restarts
the service each time; `--restart` also restarts the web app and mock Ting. Logs are in `.dev/logs/`.

Native Numbers conversion is disabled by default. For local macOS testing with Apple Numbers installed and
Automation permission already granted, opt in explicitly:

```sh
CALC_RUNNER_NUMBERS_ENABLE=1 scripts/dev.sh --no-build --restart
```

The launch helper inherits exported environment variables, so this setting reaches the service unchanged.
A later invocation without it uses the default fallback unless it remains exported in the shell; setting it to
`0` explicitly disables the native bridge. Native Numbers runs with its own desktop app permissions and is
outside runner sandbox and app resource limits. Keep it disabled for exposed or unattended service runs.

## Rules

- **Words** (everything people read: pages, CLI output, errors, docs, code comments): people are **Carbons**, agents
  are **Silicons**, groups are never "teams" or "organizations". Never write "user", "human", "AI", "agent", "org",
  "frontend", "backend" or "Honeycomb". Accounts are shown by `c:`/`si:` id and display name, never by uuid.
- **Errors** say what happened and why, then what to do (`message` + `hint`).
- **No telemetry** anywhere.
- **Web**: minimal and calm, one light theme only (no dark mode, no theme switch), Silicon UI components
  (`https://ui.teamofsilicons.com/r/{name}.json` via the shadcn CLI), squircles, BDO Grotesk.
- **Silicons are first-class**: everything the web app does, the `calc` CLI does.
- **Don't commit or push**: the orchestrator commits after each phase. Leave the tree building.
- Before you finish: your crate builds without warnings, its tests pass, and you report exactly what works, what you
  verified and how, and what doesn't work yet.
