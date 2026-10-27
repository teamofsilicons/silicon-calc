# Silicon Calc architecture

`understanding/UNDERSTANDING.md` is the contract. This file records how it is built.

## Layout

| Path | What |
| --- | --- |
| `crates/calc-engine` | The spreadsheet itself, no I/O: workbook model, cell storage, formulas (parser, evaluator, functions, recalculation), number formats, operations and their inverses, diff and three-way merge, snapshots, CSV/TSV/JSON/XLSX codecs and the legacy XLS reader. |
| `crates/calc-api` | The service (`calc-api` binary): Postgres, blob storage, Silicon Accounts sign-in, sharing, comments, versions, copies, live editing over SSE, command result and conversion routes, Ting. |
| `crates/calc-runner` | Trusted Parquet conversions through DuckDB, sandboxed file converters, LibreOffice and optional native Numbers conversion. |
| `crates/silicon-calc-client` | The Rust client for the service's HTTP API (used by the CLI and tests). |
| `crates/calc-cli` | The `calc` command line. |
| `runner/` | Fixed file conversion programs and their Python requirements. |
| `web/` | The web app (Next.js, from the Silicon web kit, Silicon UI components, light only). |
| `e2e/` | End-to-end: a local Silicon Accounts stack, a stand-in Ting, journeys for Carbons (browser) and Silicons (CLI). |
| `scripts/` | `dev.sh`, `stop.sh`, `e2e.sh`. |

## The model

- A **sheet** is a workbook. It has one main **line** and any number of **copies**; a copy is a line forked from
  another line (main or a copy) at a sequence number. Every line has its own linear history of operations.
- A line's state is a `Workbook`: ordered tabs, each a grid of cells, plus styles, column widths, row heights,
  frozen panes, merges, charts and protected ranges.
- **Rows and columns have stable ids** (`u32`, allocated per tab, never reused). Operations name cells by
  `(row_id, col_id)`, so concurrent edits never need transforming: an insert in front of a cell does not change
  the cell's id. Positions are looked up through two arrays per axis (position → id, id → position).
- Cells are stored by column in chunks of 1,024 rows. A chunk is either empty, all numbers (dense `f64` plus a
  presence bitmap) or mixed (`Cell` per row). Text is interned per tab. A plain value costs 8–16 bytes; formulas
  share one parsed tree per distinct R1C1 form, so a formula filled down a million rows is one tree.
- Every cell carries its last author (index into the tab's author table) and the time of the change, so any cell
  can say who last changed it and when without reading history.

## Formulas

Excel syntax and semantics: A1 and R1C1 references, `Tab!A1`, `'My tab'!A1:B9`, whole columns and rows, absolute
and relative parts, names, arrays `{1,2;3,4}`, dynamic arrays that spill, errors (`#DIV/0!`, `#N/A`, `#NAME?`,
`#REF!`, `#VALUE!`, `#NUM!`, `#NULL!`, `#SPILL!`, `#CALC!`, `#CIRC!` shown as Excel's circular warning), dates as
serial numbers (1900 system with Excel's leap-year bug).

Dependencies are found by formula group (all cells with the same R1C1 tree): for a changed cell, each group asks
which of its members reference it, from the tree's relative and absolute offsets, without a per-cell dependency
map. Recalculation marks dependents dirty transitively and evaluates them with an explicit work stack (no
recursion, so a chain a million cells long is fine), detecting cycles.

## Operations, history and copies

- An operation is one change by one account: set cells (values, formulas, styles), insert/delete/move rows or
  columns, sort, add/rename/move/delete tabs, import a file into a tab, write code results, merge a copy,
  restore, and so on. Each stores enough to be inverted (the cells' previous contents).
- Operations are appended to the line's log in Postgres with a sequence number. Small ones are stored inline
  (JSON); bulk ones (imports, code results, large pastes, sorts) keep their payload in blob storage.
- A snapshot of the whole workbook (binary, zstd) is written to blob storage every so often; the state at any
  sequence is the nearest snapshot before it plus the operations after it.
- Versions shown to people are runs of operations by the same account close together in time, plus named
  versions. "Undo everything X changed since version V" rebuilds the state at V, replays every later operation
  except X's, and applies the difference to the current state as one new operation, so it is itself undoable.
- Comparing versions is a cell-level diff. A copy's first compare and three-way merge use its parent's state
  at the fork as the baseline. Each accepted merge saves a receipt: the accepted source snapshot, stable-id
  maps from the copy's tabs/rows/columns to the parent's, and the accepted parent version. Later compares and
  merges use that receipt, so already accepted changes are not offered again; the original fork stays in history.
- A partial merge advances only the selected tabs or range in the accepted-source baseline. Choosing the
  parent's value for a conflict also consumes that source change, even though the parent cell stays unchanged.
  Unselected source changes remain available for a later merge. The parent operation and receipt update commit
  together, checking the expected parent version, source head and receipt revision; a concurrent edit or merge
  returns a conflict for the caller to compare and retry.

## Live editing

Each open line is owned by one in-memory actor in the service (a tokio task) that applies operations in order,
persists them, recalculates and broadcasts changes over Server-Sent Events (SSE) at `GET {line}/events`.
Clients send edits and presence through HTTP requests. A reconnect uses `since` or `Last-Event-ID` to replay up to
1,000 saved changes; a `reset` asks the client to reload when it is further behind or misses buffered events.
Comment, copy and presence events are live; only workbook changes are replayed. Clients send operations with
the version they were looking at. If an operation overwrites a cell someone else changed after
that version, it is applied and the overwrite is reported to both sides with the earlier value (kept in
history), so no change silently replaces another. Idle lines are evicted after a snapshot.

## Browser commands

JavaScript commands execute in a browser worker. Input metadata and cells come from one captured sheet
version; previews remain in the browser. Saving sends the source metadata, input tab IDs, scalar result rows
and that version to `POST {line}/run`. The service never evaluates source. It validates dimensions and size,
checks edit permission and protections, and atomically rejects a commit when the line version has changed.
Strings remain literal cell values, including leading `=` text.

A new command tab or replacement retains JavaScript source, inputs and the account that saved it. Rerunning
uses the browser worker again with a fresh snapshot. There are no server language runtimes or background
reruns. Historical code metadata remains readable; it cannot start execution. DuckDB and Python are internal
file conversion dependencies only.

## Files and conversion boundaries

The service spools uploads to private files before parsing. Parsing and conversion run on blocking threads;
imports currently read the uploaded bytes into memory and exports build the output bytes before the HTTP
response. The synchronous import endpoint records an import operation at version 1. Durable imports create a new workbook at version 0, or record one import operation when adding tabs to an existing line. Imported cell notes become anchored
comment threads, retaining the file author's name in the comment text.

CSV, TSV, JSON and XLSX use engine codecs. Parquet uses a fresh in-process DuckDB database per conversion,
with internally generated queries and paths; no database is shared between conversions. XLS and ODS imports convert to XLSX through
LibreOffice; XLS, ODS and PDF exports convert from engine-produced XLSX. LibreOffice uses a fresh profile,
disabled macros and link updates, OS sandboxing and runner resource limits. Its trusted converter profile
allows local UNIX IPC; IP network access remains denied.

Numbers defaults to sandboxed `numbers-parser`. Its writer supports values and some styles, and refuses
formula exports instead of silently flattening them. On a local macOS desktop,
`CALC_RUNNER_NUMBERS_ENABLE=1` opts into a fixed AppleScript bridge through installed Apple Numbers and XLSX.
That native app may activate and uses its own permissions: it is outside the runner's filesystem sandbox,
process group and memory limits, and an app operation may continue after the launcher times out. This mode is
unsuitable for an exposed or unattended service. See [`runner/README.md`](../runner/README.md) for setup.

## Accounts, sharing, Ting

- Every request carries a Silicon Accounts access token (`aud` = `calc`), verified locally against the JWKS.
  Accounts are stored by uuid and shown by their current `c:`/`si:` id, looked up with the app's credentials.
- The CLI signs Carbons in with the device flow and Silicons with an SLT exchanged as a public client, and keeps
  its session in `~/.calc/session.json`.
- Notifications are written to an outbox table in the same transaction as their cause, and a worker sends them
  to Silicon Ting over its v1 WebSocket with Calc's App verification proof (receiving app `ting`, scope
  `tings.send`). Without `CALC_TING_URL` they are kept and marked as not sent.
- The signed-in account's Notifications setting reads its current Ting grant. Only an explicit Enable action
  issues a User verification proof from that caller's token and registers the recipient; Disable revokes only
  that caller's Calc grant. These actions never enable required delivery or change mute preferences. Failed
  upstream checks remain errors rather than an assumed opt-in state. Calc's author registers its six types in Ting.


### Persistent imports

`import_jobs` and `import_chunks` own the browser import lifecycle. The browser saves source Blobs in account-scoped IndexedDB and sends immutable, idempotent 1 MiB chunks. A root-layout panel survives workspace/editor navigation and never navigates on completion. Server processing starts only when every chunk has arrived; it needs no browser or retained bearer token. Permissions are checked at submission and again before applying results.

A PostgreSQL advisory lock serializes import workers. Its dedicated connection closes when the task/process exits, allowing another worker to recover a `processing` job. New-sheet creation, notes, and completion receipt share a transaction. Existing-line imports use the line actor's transaction hook to commit data, notes, and receipt together. Replaying requests or restarting between completion and source cleanup cannot apply an import twice. Network/server failures retry; conversion errors remain retryable. The global progress panel shows active jobs and completed/failed results for two minutes from the server's saved update time, then hides without deleting the receipts. Source chunks use normal content-addressed blob storage and are covered by the existing database/blob backups and reference-aware garbage collector.
