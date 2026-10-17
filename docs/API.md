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
 "code": null | {"language": "sql" | "python" | "javascript", "code": "…", "inputs": [2], "run_by": Account,
                 "run_at": "…", "keep_updated": true}}
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
| edit, import tabs, run code, make/merge/delete copies, restore versions, name versions | ✓ | ✓ | | |
| invite and change roles (never above their own), public link | ✓ | ✓ | | |
| protections, move to trash, transfer, delete forever | ✓ | | | |

Access through a folder is inherited; the higher role wins. A public sheet gives anyone with the link `view`, or
`comment`/`edit` to signed-in accounts when the owner chose that.

## Meta

- `GET /healthz` → `200 ok` (no auth, not under /v1).
- `GET /v1/meta` → `{"app_id": "calc", "version": "0.1.0", "accounts_url": "…", "limits": {"max_rows": 10485760,
  "max_cols": 16384, "max_upload_bytes": 524288000, "max_cells_per_edit": 200000, "code_timeout_seconds": 60}}`.
- `GET /v1/me` → `{"account": Account}`.
- `GET /v1/accounts/lookup?id=c:ada` → `Account` (resolves an id for sharing pickers; 404 when no such account).

## Sheets and folders

- `GET /v1/sheets?scope=mine|shared|trash&folder={id}&q={text}&cursor={c}&limit={n}` →
  `{"sheets": [Sheet], "folders": [Folder], "next_cursor": null}`. `mine` without `folder` is the top level.
- `POST /v1/sheets` `{"title": "…", "folder_id"?}` → 201 `Sheet` (one empty tab, "Tab 1"). Title defaults to
  "Untitled sheet".
- `POST /v1/sheets/import?filename={name}&title={t}&folder_id={f}` with the file as the raw body (any content
  type) → 201 `Sheet`. Formats: `.csv .tsv .xlsx .xls .ods .numbers .json .parquet`.
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

- `GET {line}/events?since={version}` → `text/event-stream`. Events (each `data:` is JSON):
  - `change`: `{"version", "by": Account, "at", "changes": [Change], "cells": [Cell…]}` — `cells` holds the new
    contents of up to 2,000 changed cells (same shape as the cells answer), otherwise it is omitted and viewers
    reload the ranges in `changes`. Events since `since` are replayed first (up to 1,000; beyond that a `reset`
    event tells the viewer to reload everything).
  - `presence`: `{"viewers": [{"account": Account, "tab": 1, "range": "B2", "client_id", "seen_at"}]}`.
  - `comment`: `{"thread": Thread}` when a thread is created, replied to, edited, resolved or reopened.
  - `copy`: `{"copy": Copy}` when a copy of this line is made, merged or deleted.
  - `ping` every 20 seconds.
- `POST {line}/presence` `{"tab": 1, "range": "B2", "client_id": "…"}` → 204 (expires after 45 seconds).

## Code

- `POST {line}/run` →

```jsonc
// request
{"language": "sql" | "python" | "javascript", "code": "select region, sum(amount) from Sales group by 1",
 "into": null                                   // just answer
       | {"new_tab": "By region"}               // a new tab made by this code
       | {"tab": 3, "at": "A1"},                // write the result into an existing tab
 "keep_updated": false}                         // with new_tab: rerun when an input tab changes
// answer
{"columns": ["region", "sum(amount)"], "rows": [["EU", 1200.5], …], "row_count": 4, "truncated": false,
 "stdout": "", "stderr": "", "duration_ms": 120, "version": 44 | null, "tab": Tab | null}
```
  `rows` holds at most 1,000 rows (`truncated` says if there were more; written results are always complete).
  - **SQL** (DuckDB): every tab is a table named as the tab is (quote names with spaces: `"By region"`); its first
    row gives the column names. Read only.
  - **Python**: `import calc` is ready: `calc.tab("Sales")` → pandas DataFrame (first row = columns),
    `calc.tabs()` → names, `calc.cell("Sales!B2")` → value, `calc.sql("select …")` → DataFrame. The result is
    `calc.result(df_or_rows)` or a variable named `result`; `print` goes to `stdout`. pandas and numpy are there.
  - **JavaScript**: `calc.tab("Sales")` → array of row objects, `calc.rows("Sales")` → arrays, `calc.cell(…)`,
    `calc.sql(…)`; the result is what the code `return`s (top-level `return` is allowed) or `calc.result(…)`.
  - Code runs with no network and no files outside its own scratch space, for at most 60 seconds and 2 GB.
    A failure answers 422 `code_failed` with `details: {"stderr", "line"?}`.
- `POST {line}/tabs/{tab}/rerun` → same answer as run: reruns a code-made tab as the caller.
- `PATCH {line}/tabs/{tab}/code` `{"keep_updated": bool}` → `Tab`.

## Files

- `POST {line}/tabs/import?filename={name}&mode=new_tab|replace&tab={tab}` raw body → `{"version", "tabs": [Tab]}`
  (every tab of the file becomes a tab; `replace` replaces `tab`'s contents with the file's first tab).
- `GET {line}/download?format=xlsx|xls|ods|numbers|csv|tsv|json|parquet|pdf&tab={tab}&version={v}` → the file
  (`Content-Disposition: attachment; filename="<title>.<ext>"`). Formats holding one tab use `tab` (default the
  first). `json` is `{"tabs": [{"name", "rows": [[…]]}]}` of shown values... with `formulas: true` cells are
  `{"v", "f"}`.

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
- `GET /v1/sheets/{sheet}/copies/{copy}/compare?limit=` → `{"diff": WorkbookDiff (parent at fork → copy now),
  "conflicts": [Conflict], "parent_changed": true}`.
- `POST /v1/sheets/{sheet}/copies/{copy}/merge` `{"tabs"?: [ids], "range"?: {"tab", "range"}, "resolutions"?:
  [{"tab", "a": "B2", "take": "copy"|"sheet"}], "default"?: "copy"|"sheet", "dry_run"?: false}` → `{"version",
  "cells_taken", "conflicts": [Conflict], "tabs_added", "tabs_removed", "rows_added", "rows_removed"}`. Merges into
  the copy's parent; the copy stays open unless `"close": true`.
  Conflict: `{"tab", "tab_name", "a", "base": Summary, "sheet": Summary, "copy": Summary, "taken": "copy"|"sheet"}`
  where Summary is `{"value"?: Value, "formula"?: "=…", "style"?: Style}`.

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
