# Silicon Calc service API

The contract between the service (`calc-api`), the Rust client (`silicon-calc-client`), the `calc` command line and
the web app. Every surface implements exactly this; change it here first.

- Base URL: the service's origin (locally `http://127.0.0.1:8730`; the web app reaches it as `/api/*` through its
  server). Every path below starts with `/v1` unless noted.
- Requests and answers are JSON (`Content-Type: application/json`) unless noted. Times are RFC 3339 UTC strings.
- **Auth**: `Authorization: Bearer <Silicon Accounts access token>` with `aud` = `calc`. Routes on a public sheet
  answer without a token for reading. A missing or bad token answers 401 `signed_out`.
- **Errors** are always `{"error": {"code": "snake_case", "message": "…", "hint": "…", "details": {…}?}}` with a
  fitting status: 400 `invalid_request`, 401 `signed_out`, 403 `not_allowed`, 404 `not_found`, 409 `conflict`,
  413 `too_large`, 422 `invalid_field` (`details.fields: {name: message}`), 429 `slow_down` (`Retry-After`), 500
  `internal`, 503 `unavailable`. `message` says exactly what happened and why; `hint` says what to do.
- **Words**: people are Carbons, agents are Silicons. Never "user", "human", "AI", "agent", "org", "team".

## Shapes

```jsonc
// Account: always shown by its current id, stored by uuid.
{"uuid": "6667d4b4-…", "id": "c:ada", "kind": "carbon", "display_name": "Ada", "pfp_url": "https://…"}

// Value (a cell's value). Errors are their Excel text.
{"t": "empty"} | {"t": "number", "v": 12.5} | {"t": "text", "v": "hi"} | {"t": "bool", "v": true} | {"t": "error", "v": "#DIV/0!"}

// Sheet
{
  "id": "k3j9xq2mpa7d",            // 12 characters [a-z0-9]
  "title": "Q3 revenue",
  "owner": Account,
  "folder_id": "f…" | null,
  "role": "owner" | "editor" | "commenter" | "viewer",   // the caller's
  "public": {"enabled": false, "access": "view" | "comment" | "edit"},
  "version": 42,                    // the main line's latest operation sequence number (0 = new)
  "created_at": "…", "created_by": Account,
  "updated_at": "…", "updated_by": Account,
  "trashed_at": null,
  "tabs": [Tab],                    // only on GET /sheets/{id}
  "copies": 3                       // open copies
}

// Tab
{"id": 1, "name": "Tab 1", "index": 0, "rows": 1000, "cols": 26, "used": "A1:F120" | null,
 "frozen_rows": 0, "frozen_cols": 0, "color": null,
 "code": null | {"language": "javascript", "code": "…", "inputs": [2], "run_by": Account,
                 "run_at": "…"}}
// rows/cols: the grid size to show = max(used extent + 1, 1000 rows / 26 columns).

// Folder
{"id": "f…", "name": "Finance", "parent_id": null, "owner": Account, "role": "…", "created_at": "…", "trashed_at": null}

// Copy (a line other than the main one)
{"id": "c…", "sheet_id": "k3j9…", "name": "Try weekly buckets", "parent_id": null | "c…",   // null = made from the main sheet
 "forked_at": 40,                 // the parent's version it was made at
 "version": 7, "created_by": Account, "created_at": "…",
 "status": "open" | "merged" | "deleted", "merged_at": null, "merged_by": null}
```

A **line** is the main sheet or one of its copies. Every route under `{line}` exists twice:
`/v1/sheets/{sheet}/…` (main) and `/v1/sheets/{sheet}/copies/{copy}/…` (a copy). Permissions are the sheet's.

Tabs in requests may be named by id (`1`) or by name (`"Sales"`). Ranges are A1 text: `B7`, `A1:C9`, `A:C`, `3:5`.

## Roles

| Can | owner | editor | commenter | viewer |
| --- | :-: | :-: | :-: | :-: |
| read cells, versions, comments, download, compare | ✓ | ✓ | ✓ | ✓ |
| comment, reply, react, resolve | ✓ | ✓ | ✓ | |
| edit, import tabs, save command results, make/merge/delete copies, restore versions, name versions | ✓ | ✓ | | |
| invite and change roles (never above their own), public link | ✓ | ✓ | | |
| protections, move to trash, transfer, delete forever | ✓ | | | |

Access through a folder is inherited; the higher role wins. A public sheet gives anyone with the link `view`, or
`comment`/`edit` to signed-in accounts when the owner chose that.

## Meta

- `GET /healthz` → `200 ok` (no auth, not under /v1).
- `GET /v1/meta` → `{"app_id": "calc", "version": "0.1.0", "accounts_url": "…", "limits": {"max_rows": 10485760,
  "max_cols": 16384, "max_upload_bytes": 524288000, "max_cells_per_edit": 200000, "browser_command_timeout_seconds": 60, "conversion_timeout_seconds": 60}}`.
  The browser enforces the command timeout; the service enforces the separate trusted conversion timeout.
- `GET /v1/me` → `{"account": Account}`.
- `GET /v1/accounts/lookup?id=c:ada` → `Account` (resolves an id for sharing pickers; 404 when no such account).

## Sheets and folders

- `GET /v1/sheets?scope=all|mine|shared|trash&folder={id}&q={text}&cursor={c}&limit={n}` →
  `{"sheets": [Sheet], "folders": [Folder], "next_cursor": null}`. `mine` (the API default) without `folder` is
  your top level. `all`, used by the Sheets home page, combines that with sheets and folders shared with you,
  preserving each item's role and sorting sheets together by last update. Search includes accessible items
  inside folders; results are deduplicated and exclude trash. `shared` remains the shared-only view.
- `POST /v1/sheets` `{"title": "…", "folder_id"?}` → 201 `Sheet` (one empty tab, "Tab 1"). Title defaults to
  "Untitled sheet".
- `POST /v1/sheets/import?filename={name}&title={t}&folder_id={f}` with the file as the raw body (any content
  type) → 201 `Sheet` with tabs and an import operation at version 1. Formats:
  `.csv .tsv .xlsx .xls .ods .numbers .json .parquet`. Optional `warnings` describes file content that could not be kept.
- `GET /v1/sheets/{sheet}` → `Sheet` with `tabs`.
- `PATCH /v1/sheets/{sheet}` `{"title"?, "folder_id"?}` → `Sheet`.
- `DELETE /v1/sheets/{sheet}` → 204: moves it to the trash (owner).
- `POST /v1/sheets/{sheet}/restore` → `Sheet`: out of the trash.
- `DELETE /v1/sheets/{sheet}/forever?confirm={title}` → 204: only from the trash, `confirm` must equal the title.
- `POST /v1/sheets/{sheet}/transfer` `{"to": "c:ada"}` → 201 `Transfer`. `GET /v1/transfers` → `{"incoming":
  [Transfer], "outgoing": [Transfer]}`; `POST /v1/transfers/{id}/accept|decline|cancel` → `Transfer`.
  Transfer: `{"id", "kind": "sheet"|"folder", "item_id", "title", "from": Account, "to": Account, "status":
  "pending"|"accepted"|"declined"|"cancelled", "created_at"}`.
- `POST /v1/folders` `{"name", "parent_id"?}` → 201 `Folder`; `GET /v1/folders/{id}` → `{"folder": Folder,
  "path": [Folder], "sheets": [Sheet], "folders": [Folder]}`; `PATCH /v1/folders/{id}` `{"name"?, "parent_id"?}`;
  `DELETE /v1/folders/{id}` (trash, with everything in it); `POST /v1/folders/{id}/restore`;
  `DELETE /v1/folders/{id}/forever?confirm={name}`; `POST /v1/folders/{id}/transfer`.
  A sheet or folder created inside someone else's shared folder belongs to that folder's owner (`created_by` stays).

## Sharing

Same routes for `/v1/sheets/{sheet}` and `/v1/folders/{folder}` (written `{item}`):

- `GET {item}/sharing` → `{"owner": Account, "members": [{"account": Account, "role": "editor"|"commenter"|"viewer",
  "via": "direct" | {"folder": Folder}}], "invites": [{"email", "role", "invited_by": Account, "created_at"}],
  "public": {"enabled", "access"}}`.
- `POST {item}/sharing` `{"invite": ["c:ada", "si:analyst", "x@example.com"], "role": "editor", "notify": true}` →
  the sharing answer. Unknown ids answer 422 naming them. An email with no account yet becomes an invite that turns
  into membership when an account with that verified email first signs in to Calc.
- `PATCH {item}/sharing/{account uuid}` `{"role"}`; `DELETE {item}/sharing/{account uuid}` (or the caller leaving);
  `DELETE {item}/invites/{email}`.
- `PUT {item}/public` `{"enabled": true, "access": "view"}` → `{"enabled", "access", "url"}` (sheets only; folders 400).

## Cells

- `GET {line}/tabs` → `{"version", "tabs": [Tab]}`.
- `GET {line}/tabs/{tab}/cells?range=A1:Z100&version={v}` → (range defaults to the used area)

```jsonc
{
  "version": 42, "tab": Tab, "range": "A1:Z100",
  "cells": [                                  // only non-empty cells, row by row
    {"a": "B2", "r": 1, "c": 1,
     "v": Value,                              // what it holds (a formula's result)
     "t": "1,200.00",                         // what it shows, formatted
     "f": "=SUM(B3:B9)",                      // formula, when it has one
     "s": 3,                                  // style id, when not default
     "spill": true,                           // value written by a spilling formula elsewhere
     "by": "6667d4b4-…", "at": "…"}           // who last changed it, when
  ],
  "styles": {"3": Style},                     // every style id the cells use (engine Style JSON)
  "accounts": {"6667d4b4-…": Account},        // every account in `by`
  "col_widths": {"B": 140}, "row_heights": {"7": 40},   // non-default sizes (column letters, 1-based rows)
  "hidden_rows": [4], "hidden_cols": ["C"], "merges": ["A1:B1"],
  "frozen_rows": 1, "frozen_cols": 0,
  "charts": [Chart], "protections": [Protection]
}
```
  `format=csv` (or `Accept: text/csv`) answers the range as CSV of shown values; `format=values` answers
  `{"version", "range", "values": [[number|string|bool|null]]}` (errors as their text, formulas as results).
- `GET {line}/tabs/{tab}/cells/{a1}` → one cell with everything above plus `"history": [{"version", "by", "at",
  "before": Value|formula, "after": …}]` (last 20 changes to that cell).

### Editing

`POST {line}/edits` `{"base_version": 42, "edits": [Edit], "client_id"?: "…"}` → `{"version": 43, "changes":
[Change], "overwrote": [Overwrite], "skipped": 0}`. Edits apply in order as **one** version (atomically: if one
fails, none apply, 422 with `details.index`). Edit types:

```jsonc
{"type": "set", "tab": 1, "cells": [{"a": "B2", "input": "=A1*2"}, {"a": "C2", "input": "12%"}]}
{"type": "set", "tab": 1, "at": "A1", "rows": [["name", "amount"], ["ada", "12"]]}   // a block of inputs
{"type": "values", "tab": 1, "at": "A1", "rows": [[Value|raw JSON…]]}               // raw values, no parsing
{"type": "style", "tab": 1, "range": "A1:C1", "patch": StylePatch}                   // engine StylePatch JSON
{"type": "clear", "tab": 1, "range": "A1:C9", "what": "contents" | "formats" | "all"}
{"type": "insert_rows" | "insert_cols", "tab": 1, "at": 4, "count": 2}               // 0-based positions
{"type": "delete_rows" | "delete_cols", "tab": 1, "at": 4, "count": 2}
{"type": "move_rows" | "move_cols", "tab": 1, "from": 4, "count": 1, "to": 0}
{"type": "sort", "tab": 1, "range": "A1:D100", "by": [{"col": "B", "asc": false}], "header": true}
{"type": "fill", "tab": 1, "from": "C2", "to": "C3:C1000"}
{"type": "add_tab", "name": "Notes", "index"?: 2}      // answer's changes include the new tab id
{"type": "rename_tab", "tab": 1, "name": "Sales"}
{"type": "move_tab", "tab": 1, "index": 0}
{"type": "delete_tab", "tab": 1}
{"type": "tab_props", "tab": 1, "frozen_rows"?: 1, "frozen_cols"?: 0, "color"?: "#1F5FB8" | null}
{"type": "col_width", "tab": 1, "cols": "A:C", "width": 140 | null}
{"type": "row_height", "tab": 1, "rows": "2:4", "height": 40 | null}
{"type": "hide_rows" | "show_rows" | "hide_cols" | "show_cols", "tab": 1, "range": "3:5"}
{"type": "merge" | "unmerge", "tab": 1, "range": "A1:C1"}
{"type": "names", "names": [{"name": "Revenue", "ref": "Sales!$B$2:$B$100"}]}       // replaces all names
{"type": "chart", "chart": {"id"?: "…", "tab": 1, "kind": "line"|"bar"|"column"|"area"|"pie"|"scatter",
   "title": "…", "range": "A1:C20", "anchor": "E2", "width": 480, "height": 300}}
{"type": "delete_chart", "id": "…"}
{"type": "protect", "protection": {"id"?: "…", "tab": 1, "range"?: "A1:D100", "editors": ["si:analyst"],
   "description": "Original data"}}                    // owner only; no range = whole tab
{"type": "unprotect", "id": "…"}
{"type": "undo"} | {"type": "redo"}                  // the caller's own last change on this line (alone in a request)
```

Change: `{"tab": 1, "range": "B2:B9"}` or `{"tab": 1, "reload": true}` or `{"tabs": true}` (the tab list changed)
or `{"meta": true}`. Overwrite (another account's change replaced by this one since `base_version`):
`{"tab": 1, "a": "B2", "by": Account, "at": "…", "before": Value, "version": 41}`.

A write to a protected range answers 403 `protected` naming the protection. A formula that can't be read answers
422 `bad_formula` with the cell and the reason.

### Live

- `GET {line}/events?since={version}` → `text/event-stream` (SSE). `Last-Event-ID` is used when `since` is absent.
  Change events carry the version as their SSE id; only workbook changes are replayed on reconnect.
  Events (each `data:` is JSON):
  - `change`: `{"version", "by": Account, "at", "changes": [Change], "cells": [Cell…]}` — `cells` holds the new
    contents of up to 2,000 changed cells (same shape as the cells answer), otherwise it is omitted and viewers
    reload the ranges in `changes`. Events since `since` are replayed first (up to 1,000; beyond that a `reset`
    event tells the viewer to reload everything).
  - `presence`: `{"viewers": [{"account": Account, "tab": 1, "range": "B2", "client_id", "seen_at"}]}`.
  - `comment`: `{"thread": Thread}` when a thread is created, replied to, edited, resolved or reopened.
  - `copy`: `{"copy": Copy}` when a copy of this line is made, merged or deleted.
  - `ping` every 20 seconds.
- `POST {line}/presence` `{"tab": 1, "range": "B2", "client_id": "…"}` → 204 (expires after 45 seconds).

## Browser commands

Commands use JavaScript only and execute in the browser. The service never evaluates submitted source.
The browser reads input cells through versioned routes, previews locally, then saves the materialized result:

- `POST {line}/run` →

```jsonc
// request: all top-level fields required
{"code": "return await calc.tab('Sales');", "base_version": 43,
 "inputs": [2], "columns": ["region", "amount"], "rows": [["EU", 1200.5]],
 "into": {"new_tab": "By region"}
       // or {"tab": 3, "at": "A1"}
       // or {"tab": 3, "at": "A1", "replace": true}
}
// answer
{"columns": ["region", "amount"], "rows": [["EU", 1200.5]], "row_count": 1, "truncated": false,
 "stdout": "", "stderr": "", "duration_ms": 0, "version": 44, "tab": Tab}
```

`base_version` must match the current line version exactly. The commit checks it atomically before writing;
a changed input snapshot returns 409 `conflict` and leaves the sheet unchanged. The caller needs editor or
owner access, and existing protected ranges remain enforced. `inputs` contains existing tab IDs read by the
browser; source text and inputs are metadata, not proof of how the result was computed.

Each row must match `columns`. Cells accept only null, booleans, finite numbers and strings; strings are
literal values, including strings beginning with `=`. Dates are ISO strings. A result has at least one column,
at most 200,000 cells including its header, at most 16 MiB of scalar/header data, and at most 1 MiB of source.
The complete JSON request also has the service-wide 16 MiB body limit.
The response includes at most 1,000 rows; the saved result is complete. `stdout`, `stderr` and `duration_ms`
are retained as empty/zero response fields because the server performs no command execution.

`new_tab` creates a tab with the header, results and JavaScript metadata. An existing `tab` with `at` writes
that rectangle. `replace: true` replaces the tab and its command metadata and must start at A1. A browser
rerun reads a fresh snapshot, executes locally and submits this replacement form. There are no server rerun,
automatic-update or source execution endpoints. Legacy request fields `language` and `keep_updated` and all
unknown request/destination fields are rejected. Old saved source metadata can remain in history, but it is
never executed by the service.

## Files

The web app uses durable imports. These are account-owned jobs; no authentication tokens are retained:

- `PUT /v1/imports/{uuid}` with `{filename,size,title?,folder_id?,sheet?,copy?}` creates an upload, or returns the existing job for the same id and request. Omit `sheet` to create a new workbook; otherwise the file adds tabs to that sheet/copy.
- `POST /v1/imports/{uuid}/chunks/{index}` uploads a raw chunk of the returned `chunk_size` (1 MiB), except the final shorter chunk. Identical retries succeed; changing an acknowledged chunk is rejected.
- `POST /v1/imports/{uuid}/start` verifies all bytes have arrived and queues processing. The request returns immediately; processing survives browser closure and service restarts.
- `GET /v1/imports` lists this account's undismissed imports. `GET /v1/imports/{uuid}` reads one job. Answers contain `id`, `request`, `state`, `updated_at` (Unix milliseconds), `chunks` (acknowledged indexes), `uploaded` (bytes), `chunk_size`, `result`, and `error`. The web panel shows active jobs and completed/failed results from the last two minutes; older receipts remain available through the API.
- States: `uploading`, `queued`, `processing`, `completed`, `failed`, `cancelled`. Completed results contain `{sheet,copy,tabs,version,warnings}`. A new workbook starts at version 0 with the imported data; adding tabs to an existing workbook records one undoable import version.
- `POST /v1/imports/{uuid}/retry` retries a failed conversion using the retained upload. `DELETE /v1/imports/{uuid}` cancels an uploading/queued job or dismisses a completed/failed job. Processing jobs finish in the background and cannot be cancelled mid-commit.

The browser retains source files in account-scoped IndexedDB until the service owns the complete upload. Refreshing or reopening Calc resumes missing chunks; a Web Lock coordinates browser tabs. If local storage is unavailable, the progress panel explains that Calc must stay open until upload completes. Already uploaded jobs are independent of browser storage. Deleting browser data can require selecting the original file again; acknowledged chunks are verified when reselecting. Original upload blobs are released after completion or cancellation. Failed sources remain until retry or dismissal.


- `POST {line}/tabs/import?filename={name}&mode=new_tab|replace&tab={tab}` raw body →
  `{"version", "tabs": [Tab], "warnings": [string]}`. `tabs` contains all resulting tabs in the line.
  Every tab of the file becomes a tab; `replace` replaces `tab`'s contents with the file's first tab.
- `GET {line}/download?format=xlsx|ods|pdf|html|csv|tsv|xls|numbers|json|parquet&tab={tab}&version={v}` → the file
  (`Content-Disposition: attachment; filename="<title>.<ext>"`). Formats holding one tab use `tab` (default the
  first). `json` is `{"tabs": [{"name", "rows": [[…]]}]}` of shown values... with `formulas: true` cells are
  `{"v", "f"}`.

The sheet's Download menu offers XLSX, ODS, PDF, HTML, CSV and TSV. XLSX/ODS/PDF/HTML export every tab;
CSV and TSV export the active tab's calculated values. The CLI and API can select one tab with `tab`.
XLSX and ODS keep ordinary supported spreadsheet formulas with cached results. JavaScript cells and
formulas calling custom functions export as calculated values, including every cell of an array result;
references to those spill ranges become ordinary ranges. Pending JavaScript calculations must finish before download.

XLSX and ODS also carry inert, versioned Calc metadata in `silicon-calc/programs.xml`: original JavaScript
formulas, active function definitions, and saved command sources with their input tabs. Direct reimport restores
those definitions and rerunnable analyses inside Calc's existing browser sandbox. Original tab identities and names
are restored for a new sheet; imports into existing sheets bind literal script tab references to the imported tabs.
Conflicting function definitions fail the import rather than replace existing code. Outputs edited outside Calc
remain edited values with an import warning instead of being overwritten with the old formula.
Excel and Google Sheets do not execute this metadata; importing or resaving through another app may discard it.
The metadata is source preservation for direct reimport, not a full backup of permissions, history or every format feature.
HTML is a self-contained, escaped table view of calculated values and basic cell formatting, with no executable scripts.

XLS and ODS imports and XLS/ODS/PDF downloads require LibreOffice on the service. Numbers defaults to
sandboxed `numbers-parser`, whose writer cannot preserve formulas and returns a conversion error when they
are present. The optional macOS native Numbers bridge preserves formulas through XLSX and requires an
explicit desktop opt-in; see [`runner/README.md`](../runner/README.md) for its security and resource boundary.
PDF and HTML are download-only. Conversion failures answer 422 `code_failed` with converter output in `details`.

## Versions

- `GET {line}/versions?cursor={c}&limit={n}` → `{"versions": [Version], "next_cursor"}`, newest first. A version
  groups consecutive operations by one account less than 5 minutes apart:
  `{"version": 42, "from_version": 37, "by": Account, "started_at", "ended_at", "summary": "si:analyst changed 340
  cells in Sales", "name": null | "Sent to the client", "kind": "edit"|"import"|"code"|"merge"|"restore"|"undo"}`.
- `POST {line}/versions/{v}/name` `{"name": "…"}` / `DELETE {line}/versions/{v}/name`.
- `GET {line}/versions/{v}/tabs` and `GET {line}/versions/{v}/tabs/{tab}/cells?range=` → the state at version `v`.
- `GET {line}/compare?from={v}&to={v|head}&limit=500&tab=&range=` → the engine's `WorkbookDiff` JSON with accounts:
  `{"tabs": [TabDiff], "cells": [CellDiff], "cells_changed", "truncated", "accounts": {uuid: Account}}`;
  CellDiff gains `"by"`: who changed it last between the versions.
- `POST {line}/versions/{v}/restore` `{"tab"?, "range"?}` → `{"version"}`: the whole sheet (or one tab, or one
  range) as it was at `v`, as a new version.
- `POST {line}/versions/{v}/undo-account` `{"account": "si:analyst"}` → `{"version", "cells_changed"}`: undoes
  everything that account changed after `v`, keeping everyone else's changes.
- `POST {line}/versions/{v}/new-sheet` `{"title"}` → 201 `Sheet`.

## Copies

- `GET /v1/sheets/{sheet}/copies?status=open|all` → `{"copies": [Copy]}` (the tree is given by `parent_id`).
- `POST {line}/copies` `{"name"}` → 201 `Copy`: a copy of that line as it is now (a copy of a copy nests).
- `PATCH /v1/sheets/{sheet}/copies/{copy}` `{"name"}`; `DELETE /v1/sheets/{sheet}/copies/{copy}` → 204 (status
  `deleted`; its history stays readable).
- `GET /v1/sheets/{sheet}/copies/{copy}/compare?limit=` → `{"diff": WorkbookDiff (accepted source baseline → copy now),
  "conflicts": [Conflict], "parent_changed": bool}`. The initial baseline is the parent at the fork; later
  comparisons use the accepted-source snapshot saved by merges. `parent_changed` compares the parent's current
  version with the version accepted by the last merge (or the fork version before the first merge).
- `POST /v1/sheets/{sheet}/copies/{copy}/merge` `{"tabs"?: [ids], "range"?: {"tab", "range"}, "resolutions"?:
  [{"tab", "a": "B2", "take": "copy"|"sheet"}], "default"?: "copy"|"sheet", "dry_run"?: false}` → `{"version",
  "cells_taken", "conflicts": [Conflict], "tabs_added", "tabs_removed", "rows_added", "rows_removed"}`. Merges into
  the copy's parent; the copy stays open unless `"close": true`.
  Conflict: `{"tab", "tab_name", "a", "base": Summary, "sheet": Summary, "copy": Summary, "taken": "copy"|"sheet"}`
  where Summary is `{"value"?: Value, "formula"?: "=…", "style"?: Style}`.
  An accepted merge records the selected source changes and their stable-id mappings into the parent. Partial
  merges advance only the selected portion of that baseline; unselected changes remain available. Resolving a
  conflict with `"sheet"` consumes that source change too, so the next compare does not offer it again. The
  original fork remains in history. The parent edit and receipt update commit together only if the parent
  version, copy head and receipt revision still match; otherwise the merge answers 409 `conflict` and should be
  compared and retried. A dry run does not advance the baseline.

## Comments

- `GET {line}/comments?status=open|resolved|all&tab={tab}` → `{"threads": [Thread]}`.
- `POST {line}/comments` `{"tab": 1, "range": "B2:B9" | "B:B" | "3:3" | null (the whole tab), "body": "…"}` → 201
  `Thread`. Mentions are `@c:ada` / `@si:analyst` in the body.
- `POST /v1/comments/{thread}/replies` `{"body"}` → 201 `Comment`.
- `PATCH /v1/comments/{comment}` `{"body"}` (author only); `DELETE /v1/comments/{comment}` (author or owner).
- `POST /v1/comments/{thread}/resolve` / `POST /v1/comments/{thread}/reopen` → `Thread`.
- `POST /v1/comments/{comment}/reactions` `{"emoji": "👍"}`; `DELETE /v1/comments/{comment}/reactions/{emoji}`.

```jsonc
// Thread
{"id": "…", "sheet_id", "copy_id": null, "tab": 1, "tab_name": "Sales",
 "range": "B2:B9" | null,            // where it is now (moves with the cells)
 "original_range": "B2:B9",          // where it was made
 "quote": "1,200 | 900 | …",        // the cells' shown text when it was made (kept if they're deleted)
 "detached": false,                  // its cells were deleted
 "status": "open" | "resolved", "resolved_by": Account|null, "resolved_at": null,
 "comments": [Comment], "created_at"}
// Comment
{"id", "author": Account, "body", "mentions": [Account], "reactions": [{"emoji", "accounts": [Account]}],
 "created_at", "edited_at": null}
```

## Notifications (Silicon Ting)

Notification delivery requires the recipient's explicit consent in Calc's **Notifications** account setting.
All three routes require a signed-in account and act only on that account:

- `GET /v1/me/notifications` → `{"enabled": boolean}` queries the current Calc grant in Ting. It does not enable delivery.
- `POST /v1/me/notifications` with `{}` → `{"enabled": true}` is the explicit Enable action. Calc exchanges the
  caller's Accounts access token for a User verification proof scoped to Ting's `subscriptions.register`, then registers that recipient.
- `DELETE /v1/me/notifications` → `{"enabled": false}` revokes the caller's Calc grant. It does not change other apps' grants.

Status queries and revocation use Calc App verification proofs with `subscriptions.read` and `subscriptions.revoke` respectively.
An unavailable or unconfigured service returns `503 unavailable`, not an assumed disabled state. No proof, token, or grant ID is
exposed to the browser. Enabling a grant does not change Ting's mute preferences or opt into required delivery. The Calc app author
must register the six types below in Ting before delivery can succeed.

Calc sends every notification through Silicon Ting and never its own. Each ting body:
`{"type", "for": "<recipient account uuid>", "key": "<unique>", "data": {…}, "metadata": {}}` with data carrying
`{"sheet": {"id", "title"}, "copy": {"id","name"}|null, "tab": {"id","name"}, "range", "quote", "thread_id",
"comment": {"id","body","author": Account}, "url": "<web link>", "cli": "calc comments show <thread>"}` as fits:

| Type | When | To |
| --- | --- | --- |
| `calc.mention.received` | someone mentions you in a comment | the one mentioned |
| `calc.comment.created` | someone comments on your sheet | the owner (not when the owner wrote it) |
| `calc.comment.replied` | someone replies in a thread you're in | everyone in the thread but the replier |
| `calc.thread.resolved` | a thread you started is resolved (by someone else) | who started it |
| `calc.copy.merged` | someone merges a copy into your sheet | the owner (not when the owner merged) |
| `calc.sheet.shared` | a sheet or folder is shared with you | the one invited |

## Search

- `GET /v1/search?q={text}&limit=` → `{"results": [{"sheet": Sheet, "matches": [{"tab", "tab_name", "a",
  "text"}]}]}`: titles and cell text of every sheet the caller can read.


### Optional email requirement for public sheets

`PUT /v1/sheets/{sheet}/public` accepts `require_email` (boolean, default false). Only the owner may change it. Its value is included in sheet and sharing responses. Private sheets keep their existing account permissions. The setting is remembered when a public link is switched off.

With a public link and this option enabled, every non-owner content request needs either the signed-in account's email or a guest viewing receipt. A missing receipt answers `403 email_required`. This covers copies, history, downloads and live streams; changing the policy rechecks existing readers. Search omits gated content until the requirement is met.

`POST /v1/sheets/{sheet}/visits` accepts `{ "email": "reader@example.com" }` without sign-in. The address is syntax-checked, not verified, and never grants account identity, membership, commenting or editing. If the caller has an account email, Calc uses that instead. Otherwise the response contains an opaque `view_grant`; API callers send it as `x-calc-view-grants`. The browser proxy keeps it in a per-file HttpOnly cookie and returns only `{ "ok": true }`. Receipts expire after 30 days and become invalid when public access or the requirement changes.

`GET /v1/sheets/{sheet}/viewers?offset=0` is owner-only. It returns `viewers`, `total`, and `next_offset`, in pages of 50 ordered by latest view. Each viewer has `email`, `email_source` (`account` or `entered`), `first_viewed_at`, and `last_viewed_at`. Repeated reads are grouped; last view is refreshed at most once a minute. The owner is excluded. History is collected while the requirement is enabled and remains available when it is turned off; prior anonymous views cannot be identified.
# JavaScript functions and formatting (0.2)

The complete browser guide is `/docs/javascript` on the Calc website. JavaScript is evaluated only in an isolated browser worker. The service never evaluates submitted source. Functions calculate while an editor has the sheet open; saved results remain available to readers. A formula awaiting browser calculation shows `#BUSY!`.

`GET /v1/me/functions` lists only the caller's account library as `{functions:[...]}`. `PUT /v1/me/functions/{name}` upserts `{name,parameters:["value"],code:"return value * 2;"}`. Names are canonical uppercase. The response adds `account`, the caller's UUID. `DELETE /v1/me/functions/{name}` removes that library entry; existing sheet copies remain intact. Maximum 256 functions per account, 32 parameters, 64 KB of source per definition.

`GET {line}/functions` requires sheet read access and returns `{version,epoch,functions,names,calls}`. `names` lists non-built-in function names referenced in formulas. A sheet definition has `account:null`; an imported account definition identifies its originating account. Local definitions take precedence. Definitions and completed calculations are included in Calc snapshots, versions and copies.

`PUT {line}/functions` requires edit access and an exact `base_version`. Its body is `{base_version,function:{name,parameters,code,account?},remove?:false}`. Only the caller's account can be named as an import source. Set `remove:true` to remove the matching name and source scope. The browser imports account functions when used and refreshes their definitions when that account opens the sheet. Source becomes readable with the sheet.

Each pending `calls` entry has `{id,name,tab,row,col,args,result:null}`. Row and column are zero-based. Inputs are JSON scalars or arrays of rows. A single-cell formula argument is a scalar. `JS(code,...inputs)` uses the first string as a function body and exposes the remaining inputs as `args`. Named functions take their named parameters. Bodies may use `await`, `calc.tabs()`, `calc.cell(tab,address)`, `calc.values(tab,range)`, `calc.range(tab,range)`, and `calc.tab(tab)`. Formula reads can omit the tab to use the calling cell's tab. Code has no network, credentials, DOM or filesystem access.

`POST {line}/javascript/results` commits `{base_version,epoch,results:[[call_id,{rows:[[{t:"number",v:10}]],error?:"message"}]]}`. It requires edit access, the exact current version, the current calculation epoch and unresolved call IDs. Conflicting or stale work is rejected. Rectangular outputs are limited to 200,000 cells per commit, 256 call results per commit, and 32 KB per text cell. Non-finite numbers are rejected. Results recalculate ordinary dependent formulas without executing code on the service. The browser gives formulas five seconds and commands sixty seconds.

Cell styles now include `clip:boolean`, `rotation:-90..90` (or `255` for stacked text) and `control`. Controls are `{kind:"checkbox",checked?:"Yes",unchecked?:"No"}`, `{kind:"dropdown",options:["Open","Done"]}`, `{kind:"person"}`, `{kind:"file"}`, and `{kind:"rating"}`. Default checkboxes store booleans; custom values store literal text. Dropdown selections always use typed `values` edits, even when their text begins with `=`. Ratings store numbers from zero through five. Empty new checkbox/rating cells initialize to false/zero. Clearing formats preserves controls; set `control:null` to remove them. Formatting with `num_fmt:"@"` makes subsequent entered content literal text.

`POST /v1/formats/preview` takes `{value:Value,code:"#,##0.00"}` and returns `{text,color}` from the same formatter used for saved cells. Formatting does not change underlying values. Cell JSON can include `tc`, a text colour chosen by a number format, and `cs`, the complete effective style after dynamic rules; `s` remains the stored base style ID.

The `format_rule` edit is `{type:"format_rule",tab,range,id?,rule}`. Omit `id` to create; reuse it to update in place. `delete_format_rule` takes `{tab,id}`. Tabs expose `format_rules:[{id,range,rule}]`. Rules use stable row/column boundaries, survive copies, snapshots, history and undo, and update after edits. Maximum 100 rules per tab and the ordinary per-edit cell budget per range. Rule forms:

- `{kind:"condition",condition,value,second,paint:{fill,color,bold,italic,underline,strike}}`. Conditions: `empty`, `not_empty`, `contains`, `not_contains`, `starts`, `ends`, `text_is`, `text_not`, `greater`, `greater_equal`, `less`, `less_equal`, `equal`, `not_equal`, `between`, `not_between`, `date_is`, `date_before`, `date_after`, `formula`. A custom formula is written for the top-left cell and relative references are evaluated for each cell. JavaScript cannot run as a conditional-formatting predicate.
- `{kind:"scale",low:"#FFFFFF",high:"#18703F",middle:null}`. Numeric range minimum/maximum determine the scale; an optional middle colour sits halfway between them.
- `{kind:"alternating",first,second,header:null,footer:null}`. Colours use `#RRGGBB`. Header/footer colours are optional. Alternation supplies the base; the first matching conditional rule takes precedence.

Excel exports include native checkbox controls, supported inline dropdown validation, rotation, native conditional rules and colour scales. Additional Calc metadata retains custom control values, chips, clipping and editable formatting rules on a direct XLSX reimport into Calc. Other spreadsheet software may discard that additional metadata. JavaScript source is retained in XLSX and ODS for direct reimport; external spreadsheet engines see calculated values and cannot execute it.
