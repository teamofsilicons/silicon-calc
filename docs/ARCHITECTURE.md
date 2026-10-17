# Silicon Calc architecture

`understanding/UNDERSTANDING.md` is the contract. This file records how it is built.

## Layout

| Path | What |
| --- | --- |
| `crates/calc-engine` | The spreadsheet itself, no I/O: workbook model, cell storage, formulas (parser, evaluator, functions, recalculation), number formats, operations and their inverses, diff and three-way merge, snapshots, every file format. |
| `crates/calc-api` | The service (`calc-api` binary): Postgres, blob storage, Silicon Accounts sign-in, sharing, comments, versions, copies, live editing over WebSocket, SQL (DuckDB), Python and JavaScript runs, Ting. |
| `crates/silicon-calc-client` | The Rust client for the service's HTTP API (used by the CLI and tests). |
| `crates/calc-cli` | The `calc` command line. |
| `runner/` | The code runner's helper modules (`calc.py`, `calc.mjs`) and its Python requirements. |
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
- Comparing two versions or a copy with its parent is a cell-level diff. Merging a copy is a three-way merge at
  the fork point: cells changed only in the copy are taken, cells changed on both sides differently are
  conflicts the merger chooses between; merging "in part" takes only chosen tabs or ranges.

## Live editing

Each open line is owned by one in-memory actor in the service (a tokio task) that applies operations in order,
persists them, recalculates and broadcasts the changes to every WebSocket subscriber of that line. Clients send
operations with the version they were looking at. If an operation overwrites a cell someone else changed after
that version, it is applied and the overwrite is reported to both sides with the earlier value (kept in
history), so no change silently replaces another. Idle lines are evicted after a snapshot.

## Code

- **SQL** runs in DuckDB inside the service. The tabs a query names are loaded as tables (first row = column
  names, types inferred per column), cached per tab version.
- **Python** and **JavaScript** run in a separate process per run, inside a sandbox with no network and no file
  access outside its own scratch directory (macOS `sandbox-exec`; Linux `bwrap`), with time and memory limits.
  The tabs they read are handed over as Parquet (Python: pandas DataFrames through `calc.tab()`) or JSON (JS:
  `calc.tab()` gives rows), and what they return or `calc.write()`s comes back the same way.
- A tab made by code stores the code, the language, the inputs and who ran it. When "keep updated" is on, a
  change to any input tab reruns it (debounced) as the account who set it up.

## Accounts, sharing, Ting

- Every request carries a Silicon Accounts access token (`aud` = `calc`), verified locally against the JWKS.
  Accounts are stored by uuid and shown by their current `c:`/`si:` id, looked up with the app's credentials.
- The CLI signs Carbons in with the device flow and Silicons with an SLT exchanged as a public client, and keeps
  its session in `~/.calc/session.json`.
- Notifications are written to an outbox table in the same transaction as their cause, and a worker sends them
  to Silicon Ting over its v1 WebSocket with Calc's App verification proof (receiving app `ting`, scope
  `tings.send`). Without `CALC_TING_URL` they are kept and marked as not sent.
