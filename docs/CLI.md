# calc: the Silicon Calc command line

Silicon Calc is where Carbons and Silicons work on spreadsheets together. `calc` makes and shares sheets,
reads and changes cells by address, works in copies and merges them back, comments with mentions, reads
versions, watches live changes, and imports or downloads files. JavaScript commands run in the browser;
the CLI can inspect their saved code and read or edit their results.

This guide is built into the command: `calc docs` prints all of it, and `calc docs TOPIC` prints one part
(`calc docs copies`, `calc docs sign in`, `calc docs code`). Every command also has `--help` with examples.

Parts: Install · Sign in · Naming things · Output and errors · Sheets and folders · Sharing · Reading cells ·
Changing cells · Tabs · Names, charts and protections · Code · Copies · Versions · Comments · Live: watch and
presence · Files · Search and the service · Recipes for Silicons · Every command.

## Install

```sh
silicon-apps install calc
calc --version
calc accounts --json           # how calc signs in; works offline
```

From the repository instead: `cargo install --path crates/calc-cli` builds the same `calc`.

calc records and sends nothing about how it is used: no telemetry of any kind.

## Sign in

Sign-in is handled entirely by Silicon Accounts. A sign-in belongs to one account, one Silicon Calc service and
one Silicon Accounts.

**Carbons** approve a code in the browser:

```sh
calc login
```

```text
To sign in to Silicon Calc as a Carbon, open
  https://accounts.teamofsilicons.com/device?code=WDJB-MJHT
and confirm the code WDJB-MJHT (it expires in 10 minutes).
(Silicons don't approve codes: calc login --silicon si:you --stk-stdin.)
Waiting for approval…
Signed in to Silicon Calc as c:ada (Ada, a Carbon).
```

calc opens the link in the browser when it can (`--no-browser` only prints it). With `--json`, the code arrives
as one JSON line on stderr (`{"event": "device_code", "user_code", "verification_uri", …}`) and the sign-in as
JSON on stdout.

**Silicons** sign in with their si:id and STK. Pass the STK on stdin, so it never shows in a process list or a
shell history:

```sh
printf '%s' "$STK" | calc login --silicon si:analyst --stk-stdin
```

```text
Signing si:analyst in to Silicon Accounts…
Signed in to Silicon Calc as si:analyst (Analyst, a Silicon).
```

Other ways to give the same:

```sh
ACCOUNTS_SILICON=si:analyst ACCOUNTS_STK=stk-… calc login      # secrets injected as variables
calc login --silicon si:analyst                                # in a terminal: asks for the STK, hidden
calc login --slt slt_…                                         # a short-lived token for calc you already have
silicon-accounts login --app calc -q | calc login --slt-stdin  # the same, straight from silicon-accounts
```

`--stk VALUE` works too, but calc warns: arguments are visible to every process on the machine.

What `calc login --silicon` does, in one go: it signs in to Silicon Accounts with the si:id and STK, asks for a
short-lived token for the app `calc`, exchanges that token as Silicon Calc's public client for Silicon Calc's own
tokens, and ends the first sign-in it only needed for minting the token. Silicon Calc never sees the STK. A
short-lived token works once, within 2 minutes, for the app `calc` only; a used or expired one can't be retried,
so get a fresh one.

**The session** is kept in `$CALC_HOME/session.json` (default `~/.calc/session.json`; with `SILICON_HOME` set,
`$SILICON_HOME/.calc/session.json`), mode 0600 in a 0700 folder. The access token lasts 30 minutes; calc
refreshes it by itself when less than a minute is left, or when Silicon Calc refuses it. Refresh tokens rotate on
every use, and presenting a used one ends the whole sign-in, so calc refreshes under a lock: many `calc`
processes can share one home at once, and only one of them refreshes. A sign-in lasts until `calc logout`, or at
most 900 days.

```sh
calc login status              # who, how, until when; exit 1 when signed out
calc login status --json       # always exit 0: read "authenticated"
calc login status --offline    # only the stored sign-in, no network
calc whoami                    # asks Silicon Calc who you are
calc logout                    # ends the sign-in at Silicon Accounts and deletes it here
```

```json
{
  "authenticated": true,
  "app_id": "calc",
  "uuid": "4143123f-b494-481c-adbf-c14b14cfccc0",
  "id": "si:analyst",
  "kind": "silicon",
  "display_name": "Analyst",
  "method": "silicon",
  "expires_at": "2026-10-17T09:01:30Z",
  "refresh_expires_at": "2029-04-05T08:31:29Z",
  "api_url": "https://calc-api.teamofsilicons.com",
  "accounts_url": "https://accounts.teamofsilicons.com",
  "verified": true
}
```

Signed out, `calc login status --json` prints `{"authenticated": false, "app_id": "calc"}` (with a `reason` when
there is one) and exits 0. `verified` says whether Silicon Calc confirmed the sign-in just now.

A home holds one sign-in. `calc login` while signed in refuses (`conflict`), unless it is the same Silicon again;
`--force` signs in anyway and ends the sign-in it replaces. `--label TEXT` names this sign-in among your Silicon
Accounts sign-ins (default `calc CLI on <host>`).

**Several Silicons on one machine:** give each its own home, so their sign-ins never replace each other.

```sh
printf '%s' "$SCOUT_STK"  | CALC_HOME=/srv/silicons/scout/.calc  calc login --silicon si:scout  --stk-stdin
printf '%s' "$LEDGER_STK" | CALC_HOME=/srv/silicons/ledger/.calc calc login --silicon si:ledger --stk-stdin
CALC_HOME=/srv/silicons/scout/.calc calc ls
```

**Another service:** `--api-url URL` (or `CALC_API_URL`) names the Silicon Calc service and `--accounts-url URL`
(or `CALC_ACCOUNTS_URL`) its Silicon Accounts. Without them, calc uses the ones you signed in with, then
production (`https://calc-api.teamofsilicons.com`, `https://accounts.teamofsilicons.com`). A sign-in is only ever
sent to the service it was made with: pointed elsewhere, calc says you are signed in somewhere else and sends no
token.

Reading a public sheet never needs signing in: `calc read <its link>` works signed out.

## Naming things

**A sheet** is named by its id (12 letters and digits, like `k3j9xq2mpa7d`), its link
(`https://calc.teamofsilicons.com/s/k3j9xq2mpa7d`), or its exact title when exactly one sheet you can see has it.
When several do, calc lists them with their ids and exits 2: pass the id. Quote titles with spaces.

**A copy** is named with `--copy COPY`, its id or its name, on any command that reads or changes a sheet:
`calc read "Q3 revenue" --copy "Try weekly buckets"`.

**A tab** is named by its name or its id: with `--tab Sales`, or in front of a range: `Sales!A1:C9`,
`'By region'!B2` (quote names with spaces or punctuation; `''` inside quotes is one `'`). Without one, a command
uses the first tab. Where a command takes a range, a tab's name alone means that whole tab.

**A range** is A1 text: `B7` (one cell), `A1:C9` (a block), `A:C` (whole columns), `3:5` (whole rows), `A2:B`
(from a cell down to the end of its columns).

**A version** is its number (`40`), its name (`"Sent to the client"`), or `head` for the latest.

**An account** is its id: `c:ada` for a Carbon, `si:analyst` for a Silicon. Accounts are always shown by id, never
by uuid.

## Output and errors

Every command prints something readable by default: aligned tables, a grid with column letters and row numbers,
one line saying what changed and the version it made. Notices and suggested next steps go to stderr, so stdout
stays clean for pipes; `-q` (`--quiet`) leaves them out.

`--json` prints one JSON document on stdout instead: the service's own answer for that route (a `Sheet`, the
`Cells` answer, an edit's `{"version", "changes", "overwrote", "skipped"}`, …), sometimes with a few fields calc
adds. `calc watch --json` prints one JSON object per line. `--json` goes anywhere on the line.

Errors say what happened and why, then what to do:

```text
error: You can't edit "Q3 revenue": you are a viewer on it.
hint: Ask its owner (c:ada) for the editor role.
```

With `--json` the error is the JSON document on stdout:

```json
{"error": {"code": "not_allowed", "message": "You can't edit \"Q3 revenue\": you are a viewer on it.", "hint": "Ask its owner (c:ada) for the editor role.", "status": 403, "exit_code": 1}}
```

`code` is stable (`signed_out`, `not_allowed`, `not_found`, `conflict`, `protected`, `bad_formula`, `code_failed`,
`invalid_field`, `slow_down`, `ambiguous`, `usage`, …), `status` is the service's HTTP status when it answered,
and `details` carries specifics (`fields` for `invalid_field`, `index` for a failed edit, `stderr` and `line` for
code, `retry_after_seconds` for `slow_down`).

Exit codes:

| Exit | When |
| --- | --- |
| 0 | Done. |
| 1 | It failed: Silicon Calc or Silicon Accounts refused, something isn't there, the network failed, a `watch --count` timed out. |
| 2 | The command line can't be used as written: a bad flag, value, range or file, an ambiguous title, a missing `--confirm`. |
| 130 | Stopped with Ctrl-C. |

When Silicon Calc asks to slow down (`slow_down`), calc waits as long as it was told and tries again, twice.

## Sheets and folders

Sheets are personal: every sheet and folder belongs to the Carbon or Silicon who made it, its owner. There are no
workspaces; share with whoever you need.

```sh
calc ls                                # your sheets and folders at the top level
calc ls --folder Finance/Q3            # one folder (an id, a path or a name)
calc ls --shared                       # shared with you
calc ls --q revenue                    # titles containing "revenue"
calc ls --all --json                   # every page
calc new "Q3 revenue"                  # one empty tab, "Tab 1"
calc new "Q3 revenue" --folder Finance
calc new --json | jq -r .id            # "Untitled sheet"
calc import sales.csv                  # a file as a new sheet (see Files)
calc info "Q3 revenue"                 # owner, your role, folder, public link, version, copies, tabs
calc rename "Q3 revenue" "Q3 revenue (final)"
calc mv "Q3 revenue" Finance/Q3        # into a folder; / for the top level
calc rm "Old draft"                    # to the trash
calc trash                             # what is in your trash
calc restore "Old draft"               # out of the trash
calc delete-forever "Old draft"        # asks you to type the title; --confirm for scripts
```

Only sheets in the trash can be deleted forever, with their whole history; it can't be undone. Without a terminal
to ask in, `delete-forever` needs `--confirm` (exit 2 `confirm_needed` otherwise).

**Folders** keep sheets and other folders together. A sheet or folder made inside someone else's shared folder
belongs to that folder's owner (who made it is still recorded).

```sh
calc folders new Finance
calc folders new Q3 --in Finance
calc folders ls                        # your folders
calc folders ls Finance/Q3             # one folder's path and contents
calc folders rename Finance "Finance 2027"
calc folders mv Finance/Q3 Archive     # / for the top level
calc folders rm Archive                # to the trash, with everything in it
calc folders restore Archive
calc folders delete-forever Archive --confirm
```

**Transfers** give a sheet or folder to another Carbon or Silicon. It moves when they accept.

```sh
calc transfer "Q3 revenue" c:ada
calc folders transfer Finance c:ada
calc transfers                         # incoming and outgoing, with their ids
calc transfers accept t3k9xq2mpa7d
calc transfers decline t3k9xq2mpa7d
calc transfers cancel t3k9xq2mpa7d     # withdraw one you offered
```

## Sharing

Carbons and Silicons get exactly the same roles and rights. A Silicon invited as an editor edits directly, like
any other editor.

| Can | owner | editor | commenter | viewer |
| --- | :-: | :-: | :-: | :-: |
| read cells, versions, comments; download; compare | yes | yes | yes | yes |
| comment, reply, react, resolve | yes | yes | yes | |
| edit, open files into tabs, run code, make and merge copies, restore and name versions | yes | yes | | |
| invite and change roles (never above their own), the public link | yes | yes | | |
| protections, the trash, transfers, deleting forever | yes | | | |

```sh
calc share ls "Q3 revenue"                                 # owner, members, invites, public link
calc share add "Q3 revenue" si:analyst --role editor
calc share add "Q3 revenue" c:ada friend@example.com --role commenter
calc share add Finance c:ada --role viewer --folder        # a folder: everything in it
calc share role "Q3 revenue" si:analyst viewer
calc share rm "Q3 revenue" si:analyst
calc share rm "Q3 revenue" friend@example.com              # an email invite
calc share leave "Q3 revenue"                              # something shared with you
calc share public "Q3 revenue" on                          # anyone with the link can view
calc share public "Q3 revenue" on --access comment         # signed-in accounts with it can comment
calc share public "Q3 revenue" off
calc lookup si:analyst                                     # who an id is
```

Whoever is invited hears about it through Silicon Ting (`calc.sheet.shared`); `--no-notify` sends no ting. An
email with no account yet becomes an invite that turns into a role when that address first signs in to Silicon
Calc. Access through a folder is inherited, and the higher role wins; someone who has a sheet through a folder is
changed on that folder. `--folder` says the name is a folder's when a sheet has the same name.

## Reading cells

```sh
calc read "Q3 revenue"                         # the first tab's used area, as a grid
calc read "Q3 revenue" Sales!A1:D20
calc read "Q3 revenue" Sales                   # a tab's name alone: all of it
calc read "Q3 revenue" A1:D20 --tab Sales      # the same, with --tab
calc read "Q3 revenue" Sales --format csv > sales.csv
calc read "Q3 revenue" B:B --format values     # {"values": [[…], …]} for machines
calc read "Q3 revenue" A1:D9 --formulas --comments
calc read "Q3 revenue" --format json           # every cell: value, shown text, formula, style, who, when
calc read "Q3 revenue" --version 40            # as it was at version 40 (or a version's name)
```

```text
"Q3 revenue" · Sales!A1:C4 · version 42
   A       B         C
1  region  amount    share
2  EU      1,200.50   0.48
3  US        900.00   0.36
4  APAC      400.00   0.16
```

Formats:

| `--format` | What |
| --- | --- |
| `table` | The grid above (the default). At most 200 rows (`--max-rows N`) and 40 columns; calc says when it cut. |
| `csv`, `tsv` | The shown values (as formatted), every row of the range. `--formulas` puts formulas in place of their results. |
| `json` | The service's full cells answer: each non-empty cell's address, value (`{"t": "number", "v": 12.5}`), shown text, formula, style, who last changed it (`by`, a uuid described in `accounts`) and when; plus styles, sizes, merges, frozen panes, charts and protections. The default with `--json`. |
| `values` | `{"version", "range", "values": [[…]]}`: plain numbers, text, true/false and null (errors as their text, formulas as results). |

`--styles` lists each styled cell's look under the table; `--comments` shows the open comments on those cells
(with `--format json` they come as `"threads"`). A sheet with millions of rows reads as fast as a small one: ask
for the range you need.

**One cell**, with everything about it:

```sh
calc cell "Q3 revenue" B7
calc cell "Q3 revenue" Sales!B7 --json
```

```text
Sales!B7 in "Q3 revenue"
value    1200.5 (number)
shows    1,200.50
formula  =SUM(B2:B6)
style    bold, format #,##0.00
changed  by si:analyst (Analyst), 2026-10-17 08:12 UTC (5 min ago)

History (newest first)
VERSION  WHO         WHEN                  BEFORE  AFTER
     42  si:analyst  2026-10-17 08:12 UTC  1100    =SUM(B2:B6)
```

## Changing cells

A change names cells by their address and touches only those cells, so it merges cleanly with whatever anyone
else is doing at the same time. Every change is a new version, kept with who made it and when; there is no save.

```sh
calc write "Q3 revenue" B2 1200                     # one cell
calc write "Q3 revenue" C2 '=B2*1.2'                # a formula
calc write "Q3 revenue" A1 region amount date       # across a row, from A1
calc write "Q3 revenue" A2 EU US APAC --down        # down a column
calc write "Q3 revenue" Sales!B2 12%                # on the tab Sales
calc write "Q3 revenue" B2 -- -5                    # -- before inputs that start with -
echo 'a long note' | calc write "Q3 revenue" D2 -   # - reads one input from stdin
calc write "Q3 revenue" --at Sales!A1 --csv sales.csv
some-command | calc write "Q3 revenue" --at A1 --tsv -
calc write "Q3 revenue" --at A1 --values '[["name", 12], ["ada", true]]'
calc write "Q3 revenue" --at A1 --values rows.json  # or a file, or - for stdin
```

Inputs are read the way a person typing them would be: `=` starts a formula, and `12%`, `2026-10-17`, `TRUE` and
`1,200.50` become a percentage, a date, a truth value and a number. `--raw` keeps text exactly as given.
`--values` writes raw JSON values with no reading at all. A block larger than one edit can hold (200,000 cells)
goes in pieces, each its own version.

When your change replaces a change someone else made after the version you looked at, calc tells you on stderr;
the earlier value stays in the history, so no change silently replaces another. `--base-version V` says which
version you looked at (by default, the one calc read just before writing). A formula that can't be read is
refused (`bad_formula`, with the cell and why), and so is a write to a protected range (`protected`).

```sh
calc clear "Q3 revenue" B2:D9                       # contents; --formats the look; --all both
calc style "Q3 revenue" A1:D1 --bold --fill '#FFF2CC'
calc style "Q3 revenue" B2:B100 --format '#,##0.00' --align right
calc style "Q3 revenue" Sales!C:C --format 0%
calc style "Q3 revenue" A1 --no-bold --color none
calc style "Q3 revenue" A1:D20 --border thin --border-color '#999999'
calc style "Q3 revenue" A1:D20 --reset              # back to the default look
```

`calc style` takes `--bold`, `--italic`, `--underline`, `--strike`, `--wrap` (each with a `--no-…`), `--color` and
`--fill` (`#RRGGBB`, `#RGB` or `none`), `--align left|center|right|general`, `--valign top|middle|bottom`,
`--format` (a number format such as `0.00`, `#,##0`, `0%`, `yyyy-mm-dd`, `$#,##0.00`, or `general`), `--font`,
`--size` (points), `--border thin|medium|thick|dashed|dotted|double|none` with `--border-color`, and `--reset`.

**Rows and columns** (rows by number, columns by letter, the tab in front or with `--tab`):

```sh
calc insert-rows "Q3 revenue" 5                     # an empty row 5; row 5 moves down
calc insert-rows "Q3 revenue" Sales!2 --count 3
calc insert-cols "Q3 revenue" C --count 2
calc delete-rows "Q3 revenue" Sales!5:9             # formulas pointing at them show #REF!
calc delete-cols "Q3 revenue" D:F
calc move-rows "Q3 revenue" 9 --to 2                # row 9 becomes row 2
calc move-cols "Q3 revenue" E --to B
calc sort "Q3 revenue" --by B:desc --header         # the used area, row 1 stays on top
calc sort "Q3 revenue" Sales!A2:D500 --by C --by B:desc
calc fill "Q3 revenue" C2 C3:C1000                  # as dragging C2's corner: formulas adjust
calc fill "Q3 revenue" A2:A3 A4:A100                # series continue
calc freeze "Q3 revenue" --rows 1 --cols 2 --tab Sales
calc freeze "Q3 revenue" --off
calc width "Q3 revenue" A:C 140                     # pixels, or auto
calc height "Q3 revenue" 1 40
calc hide "Q3 revenue" 3:5                          # rows; C:D hides columns
calc show "Q3 revenue" 3:5
calc merge "Q3 revenue" A1:D1
calc unmerge "Q3 revenue" A1:D1
```

Comments and formulas move with their cells when rows and columns move.

**Undo** takes back your own last change on a sheet (or a copy) and leaves everyone else's in place; `redo` puts
it back. To take back everything one account changed since a version, see Versions.

```sh
calc undo "Q3 revenue"
calc redo "Q3 revenue"
calc undo "Q3 revenue" --copy "Try weekly buckets"
```

## Tabs

```sh
calc tabs ls "Q3 revenue"                           # number, name, id, used area, frozen panes, code
calc tabs show "Q3 revenue" "By region"             # one tab, with the code that made it
calc tabs add "Q3 revenue" Notes
calc tabs add "Q3 revenue" Summary --at 1           # as the first tab
calc tabs rename "Q3 revenue" "Tab 1" Sales         # formulas pointing at it follow
calc tabs move "Q3 revenue" Summary 3               # to position 3 (from 1)
calc tabs rm "Q3 revenue" Scratch                   # undo brings it back
calc tabs color "Q3 revenue" Sales '#1F5FB8'        # or none
calc tabs import "Q3 revenue" october.csv           # a file's tabs as new tabs (see Files)
calc tabs import "Q3 revenue" sales-v2.csv --replace Sales
```

## Names, charts and protections

**Defined names** let formulas say `=SUM(Revenue)`:

```sh
calc names ls "Q3 revenue"
calc names add "Q3 revenue" Revenue 'Sales!$B$2:$B$100'
calc names rm "Q3 revenue" Revenue                  # formulas using it show #NAME?
calc names set "Q3 revenue" 'Revenue=Sales!$B$2:$B$100' 'Rate=Notes!$B$1'   # exactly these
calc names set "Q3 revenue" --clear
```

**Charts** (`line`, `bar`, `column`, `area`, `pie`, `scatter`); the first row and column may be labels:

```sh
calc chart ls "Q3 revenue"
calc chart add "Q3 revenue" Sales!A1:C13 --kind line --title "Revenue by month"
calc chart add "Q3 revenue" A1:B5 --kind pie --at E2 --width 400 --height 300
calc chart add "Q3 revenue" A1:B9 --kind bar --replace ch1     # change a chart
calc chart rm "Q3 revenue" ch1
```

**Protections**: the owner can protect a tab or a range so that only chosen Carbons and Silicons can change it,
for example to keep the data a Carbon loaded untouched. It is off by default.

```sh
calc protect "Q3 revenue" Sales!A1:D500 --description "Original data"
calc protect "Q3 revenue" Sales --editor si:analyst --editor c:ada   # the whole tab
calc protections "Q3 revenue"                                        # what is protected, who may change it
calc unprotect "Q3 revenue" p1
```

A write to a protected range by anyone else fails with `protected`, naming the protection.

## Code

Reusable JavaScript functions can be saved for one sheet or your account. Formulas such as `=NET_PRICE(A1,B1)` use saved functions; `=JS("return args[0] * 2;", A1)` runs a body directly in a cell. An editor must have the sheet open in the browser to calculate JavaScript; the CLI reads the saved result. Define and manage functions in the browser’s JavaScript panel. See the full [JavaScript in Calc guide](https://calc.teamofsilicons.com/docs/javascript) for cell access, arrays, scopes, commands and limits.

Commands use JavaScript and run in the browser. Open a sheet's command panel to preview a result, save it
into cells or a new tab, or rerun saved JavaScript. There is no language selector or server execution API.

```javascript
const table = await calc.tab("Sales");
const amount = table.columns.indexOf("amount");
return { columns: table.columns, rows: table.rows.filter(row => row[amount] > 1000) };
```

The browser runs code in an isolated worker with a time limit. It reads a fixed sheet version and submits
plain result values for the service to validate and save. If the sheet changed while the command was running,
run it again before saving. Closing the browser stops execution; commands do not run automatically on the server.

A command tab retains its JavaScript, input tabs and author. `calc tabs show` displays that metadata, and the
normal CLI read, write, import and export commands work with its results. The CLI does not execute code.

## Copies

A copy is a duplicate of a sheet kept inside it, to try something without touching the main sheet. Anyone who can
edit can make one. Copies can have their own copies, so a Silicon can try many ideas at once. A copy can be
compared with where it came from and merged back, in whole or in part.

```sh
calc copies new "Q3 revenue" "Try weekly buckets"
calc copies new "Q3 revenue" "Weekly, EU only" --copy "Try weekly buckets"   # a copy of a copy
calc copies ls "Q3 revenue"                          # the copies, as a tree; --all with merged and deleted
calc write "Q3 revenue" B2 0 --copy "Try weekly buckets"      # work in it: --copy on any command
calc read "Q3 revenue" --copy "Try weekly buckets"
calc copies compare "Q3 revenue" "Try weekly buckets"         # what changed in it, and what conflicts
calc copies merge "Q3 revenue" "Try weekly buckets" --dry-run # what merging would do
calc copies merge "Q3 revenue" "Try weekly buckets"           # all of it
calc copies merge "Q3 revenue" "Try weekly buckets" --tab Forecast           # one tab
calc copies merge "Q3 revenue" "Try weekly buckets" --range 'Sales!B2:B90'   # one range
calc copies merge "Q3 revenue" "Try weekly buckets" --take sheet             # conflicts keep the sheet's
calc copies merge "Q3 revenue" "Try weekly buckets" --resolve B7=sheet --resolve Sales!C3=copy
calc copies merge "Q3 revenue" "Try weekly buckets" --close   # and close the copy
calc copies rename "Q3 revenue" "Try weekly buckets" "Weekly buckets"
calc copies rm "Q3 revenue" "Weekly buckets"                  # its history stays readable
```

Merging goes into the copy's parent (the main sheet, or the copy it was made from). Cells changed only in the
copy are taken. Cells changed in both, differently, are conflicts: each takes the copy's side unless `--take
sheet` or a `--resolve CELL=SIDE` says otherwise. The copy stays open after a merge unless `--close`. When someone
else merges into your sheet, you hear about it through Silicon Ting (`calc.copy.merged`).

## Versions

Everything in Silicon Calc is versioned and nothing is ever lost. Every change is kept with who made it and when;
a version groups one account's changes made close together. Versions can be named, read, compared and restored,
and everything one Carbon or Silicon changed can be undone while everyone else's changes stay. Restoring is
itself a new version.

```sh
calc versions ls "Q3 revenue"                        # newest first: number, when, who, kind, name, summary
calc versions ls "Q3 revenue" --all --json
calc versions show "Q3 revenue" 42                   # who, when, and every cell it changed
calc versions read "Q3 revenue" 40 Sales!A1:D9       # cells as they were (like calc read --version)
calc versions name "Q3 revenue" 42 "Sent to the client"
calc versions unname "Q3 revenue" 42
calc versions compare "Q3 revenue" 40                # version 40 against now
calc versions compare "Q3 revenue" 40 42 --tab Sales
calc versions compare "Q3 revenue" "Sent to the client" --range Sales!B:B
calc versions restore "Q3 revenue" 40                # the whole sheet as it was
calc versions restore "Q3 revenue" 40 --tab Sales    # one tab (named as it was then)
calc versions restore "Q3 revenue" 40 --range Sales!B2:B90
calc versions undo-account "Q3 revenue" si:analyst --since 40   # everyone else's changes stay
calc versions new-sheet "Q3 revenue" 40 --title "Q3 revenue as sent"
calc download "Q3 revenue" --version "Sent to the client" -o sent.xlsx
```

Every command above takes `--copy COPY` to work on a copy's own history.

## Comments

Anyone who can comment can attach a comment to a cell, a range, whole rows or columns, or a whole tab. Comments
can be replied to, reacted to, edited and resolved, and resolved threads can be reopened. A comment stays on its
cells as rows and columns move around it.

```sh
calc comments add "Q3 revenue" B7 "Is this the final number?"
calc comments add "Q3 revenue" Sales!B2:B9 "@si:analyst why did March drop?"
calc comments add "Q3 revenue" C:C "These are in euros"
calc comments add "Q3 revenue" Sales "This whole tab comes from the October export"
echo "A long note…" | calc comments add "Q3 revenue" A1 -
calc comments ls "Q3 revenue"                        # open threads; --resolved, --all, --tab Sales
calc comments show 019a…                             # one thread, every comment and reaction
calc comments reply 019a… "Three fewer working days; see Notes!A1."
calc comments edit 019b… "Better wording"            # your own comment
calc comments rm 019b…                               # yours, or any on your sheet
calc comments react 019b… 👍                          # --remove takes it back
calc comments resolve 019a…
calc comments reopen 019a…
calc read "Q3 revenue" A1:D9 --comments              # cells with their comments
```

`comments ls` and `comments show` print each comment's id in brackets, for `edit`, `rm` and `react`.

**Mentions** are `@c:id` and `@si:id` in the text. All notifications go through Silicon Ting; Silicon Calc never
sends its own:

| Ting | When | Sent to |
| --- | --- | --- |
| `calc.mention.received` | Someone mentions you in a comment | The one mentioned |
| `calc.comment.created` | Someone comments on your sheet | The owner |
| `calc.comment.replied` | Someone replies in a thread you're in | Everyone in the thread |
| `calc.thread.resolved` | A thread you started is resolved | Who started it |
| `calc.copy.merged` | Someone merges a copy into your sheet | The owner |
| `calc.sheet.shared` | A sheet or folder is shared with you | The one invited |

A ting carries enough to act straight away: the sheet, the tab and cells the comment is on, their text, the
comment, a web link, and the command that shows the thread (`calc comments show <thread>`). Mentioning a Silicon
is how a Carbon asks it for something: selecting a column and writing "@si:analyst why did March drop?".

## Live: watch and presence

Everyone edits the same sheet at the same time and sees every change as it happens. `calc watch` streams a sheet's
events: changes (with the new contents of the cells), comments, copies, and who is viewing.

```sh
calc watch "Q3 revenue"                              # readable lines until Ctrl-C
calc watch "Q3 revenue" --json                       # one JSON object per line
calc watch "Q3 revenue" --since 40                   # what happened after version 40, then live
calc watch "Q3 revenue" --only change,comment        # only some kinds: change, comment, copy, presence, reset
calc watch "Q3 revenue" --only comment --mentions --count 1 --timeout 3600
                                                     # wait up to an hour for a comment that mentions you
calc watch "Q3 revenue" --here Sales!B2              # others see you on Sales!B2 while you watch
calc watch "Q3 revenue" --copy "Try weekly buckets"
calc presence "Q3 revenue" Sales!B2:B9               # show yourself there for 45 seconds
```

```text
08:12:01  v43  si:analyst changed Sales!B2:B9  (B2 = 1300; B3 = 980; B4 = 410; 5 more)
08:12:30  comment  c:ada on Sales!B7: "Is this the final number?"  (thread 019a…)
08:13:02  copy     si:analyst made the copy "Try weekly buckets" (c7d2…) at version 43
08:13:40  here     c:ada on Sales!B7, si:analyst on 'By region'
```

With `--json`, each line is the event as the service sends it, with `"event"` (`change`, `comment`, `copy`,
`presence`, `reset`) and `"sheet"` added; comment events also say `"what"` (`created`, `replied`, `edited`,
`deleted`, `resolved`, `reopened`). `reset` means too much changed at once to describe: read the sheet again.

`--count N` stops after N events and `--timeout SECONDS` after that long; with both, not reaching the count in time
exits 1 with `timed_out`. The stream reconnects by itself from the last version it saw, so nothing is missed
(`--no-reconnect` stops instead).

## Files

These spreadsheet and data formats can be opened as a new sheet, added as tabs or downloaded. PDF and HTML are
download-only; formats requiring external conversion depend on the service's installed converters:

| Format | Extension |
| --- | --- |
| CSV | `.csv` |
| TSV | `.tsv` |
| Excel | `.xlsx`, `.xls` |
| OpenDocument | `.ods` |
| Numbers | `.numbers` |
| JSON | `.json` |
| Parquet | `.parquet` |
| PDF | `.pdf` (download only) |
| Web page | `.html` (download only) |

Choosing a download format leaves the sheet in Calc unchanged. File fidelity depends on the format and
converter; read import warnings for content the reader could not keep and the conversion limits below.

XLSX and ODS keep supported spreadsheet formulas. JavaScript calculations export as values, with their original
functions, cell formulas and saved command sources retained for direct reimport into Calc. Other spreadsheet apps
cannot run that code and may discard it when saving. CSV, TSV, PDF and HTML keep calculated output. Wait for pending
JavaScript calculations to finish before downloading.

```sh
calc import sales.csv                                # a new sheet, titled from the file name
calc import budget.xlsx --title "Budget 2027" --folder Finance
curl -s https://example.test/data.csv | calc import - --format csv --title "Data"
calc tabs import "Q3 revenue" october.csv            # every tab of the file as a new tab
calc tabs import "Q3 revenue" sales-v2.csv --replace Sales     # one tab's contents replaced
calc download "Q3 revenue"                           # "Q3 revenue.xlsx" in this folder
calc download "Q3 revenue" --format csv --tab Sales -o sales.csv
calc download "Q3 revenue" -o report.pdf             # the format from the name
calc download "Q3 revenue" -o exports/               # into a folder, named after the sheet
calc download "Q3 revenue" --format json --formulas -o - | jq .
calc download "Q3 revenue" --version 40 -o then.xlsx
calc download "Q3 revenue" --copy "Try weekly buckets" -o trial.ods
```

csv, tsv and parquet hold one tab (`--tab`, default the first); the others hold every tab. `json` is
`{"tabs": [{"name", "rows": [[…]]}]}` of shown values; with `--formulas` each cell is `{"v", "f"}`. A download
never replaces an existing file unless `--force`, and is written beside it first, so a failed download never
leaves half a file. The CLI streams uploads and downloads; the service currently materializes file bytes during
parsing and export.

XLS/ODS import and XLS/ODS/PDF downloads require LibreOffice on the service. The default Numbers writer rejects
formula exports; full native Numbers conversion is a local macOS opt-in with separate desktop app permissions.
See [File conversion runtime](../runner/README.md) for setup and limits.

## Search and the service

```sh
calc search "March"                  # titles and cell text of every sheet you can read
calc search revenue --limit 20 --json
calc meta                            # the service: version, its Silicon Accounts, its limits
calc accounts --json                 # how calc signs in (no network)
```

## Recipes for Silicons

**Answer a mention.** Wait for it, read the cells it is on, work it out, answer in the thread.

```sh
calc --json watch "$SHEET" --only comment --mentions --count 1 --timeout 3600 > mention.json
THREAD=$(jq -r .thread.id mention.json)
TAB=$(jq -r .thread.tab_name mention.json)
RANGE=$(jq -r .thread.range mention.json)
calc read "$SHEET" "$RANGE" --tab "$TAB" --format csv
# Inspect the data, then use the browser command panel for JavaScript analysis.
calc comments reply "$THREAD" "March fell because EU revenue dropped; the numbers are in the tab By month."
calc comments resolve "$THREAD"
```

A ting (`calc.mention.received`) carries the same: `data.sheet.id`, `data.tab.name`, `data.range`,
`data.thread_id` and `data.cli`.

**Try an idea without touching the sheet.**

```sh
COPY=$(calc --json copies new "$SHEET" "Weekly buckets" | jq -r .id)
calc tabs add "$SHEET" Weekly --copy "$COPY"
calc write "$SHEET" Weekly!A1 "Week" "Revenue" --copy "$COPY"
calc copies compare "$SHEET" "$COPY"
calc copies merge "$SHEET" "$COPY" --dry-run
calc copies merge "$SHEET" "$COPY" --tab Weekly --close
```

**Load data and ask it questions.**

```sh
SHEET=$(calc --json import orders.parquet --title "Orders" | jq -r .id)
TAB=$(calc --json tabs ls "$SHEET" | jq -r '.tabs[0].name')
calc read "$SHEET" "$TAB" --format csv
calc share add "$SHEET" c:ada --role commenter
```

**Keep totals current with spreadsheet formulas.**

```sh
calc write "$SHEET" Sales!D1 '=SUM(B2:B1000)'
calc write "$SHEET" Sales!B2 1300
```

## Every command

| Command | What it does |
| --- | --- |
| `calc login` | Sign in: Carbons approve a code; Silicons `--silicon si:id --stk-stdin`, `--slt`, `--slt-stdin`. |
| `calc login status` | Whether you are signed in (`--json` always exits 0; `--offline`). |
| `calc logout` | End the sign-in at Silicon Accounts and delete it here. |
| `calc whoami` | Who you are signed in as. |
| `calc accounts` | How calc signs in: app id, URLs, version (offline). |
| `calc docs [TOPIC]` | This guide, or one part of it. |
| `calc meta` | The service's version, Silicon Accounts and limits. |
| `calc lookup ID` | A Carbon or Silicon by c:id or si:id. |
| `calc ls` | Your sheets and folders (`--shared`, `--trash`, `--folder`, `--q`, `--all`). |
| `calc new [TITLE]` | A new sheet. |
| `calc import FILE` | A file as a new sheet. |
| `calc info SHEET` | A sheet: owner, role, folder, public link, version, copies, tabs. |
| `calc rename SHEET TITLE` | Rename a sheet. |
| `calc mv SHEET FOLDER` | Move a sheet into a folder (`/` for the top level). |
| `calc rm SHEET` / `calc restore SHEET` | To the trash and back. |
| `calc trash` | Your trash. |
| `calc delete-forever SHEET` | Delete a sheet in the trash forever (`--confirm`). |
| `calc transfer SHEET ID` | Offer a sheet to another Carbon or Silicon. |
| `calc transfers` | Transfers to and from you; `accept`, `decline` or `cancel` one by id. |
| `calc folders` | Folders: `new`, `ls`, `rename`, `mv`, `rm`, `restore`, `delete-forever`, `transfer`. |
| `calc share` | Sharing: `ls`, `add`, `role`, `rm`, `leave`, `public`. |
| `calc search TEXT` | Titles and cell text of every sheet you can read. |
| `calc tabs` | Tabs: `ls`, `show`, `add`, `rename`, `move`, `rm`, `color`, `import`. |
| `calc read SHEET [RANGE]` | Cells as a grid, CSV, TSV, JSON or values. |
| `calc cell SHEET CELL` | One cell with its formula, look, author, history and comments. |
| `calc write SHEET CELL INPUT…` | Change cells (`--down`, `--csv`, `--tsv`, `--values`, `--raw`). |
| `calc style SHEET RANGE` | Change how cells look. |
| `calc clear SHEET RANGE` | Clear contents (`--formats`, `--all`). |
| `calc insert-rows`, `insert-cols`, `delete-rows`, `delete-cols`, `move-rows`, `move-cols` | Rows and columns. |
| `calc sort SHEET [RANGE] --by COL[:desc]` | Sort rows (`--header`). |
| `calc fill SHEET FROM TO` | Fill a range from a cell. |
| `calc freeze SHEET --rows N --cols N` | Freeze panes (`--off`). |
| `calc width SHEET COLS SIZE`, `calc height SHEET ROWS SIZE` | Column widths and row heights (`auto`). |
| `calc hide SHEET WHAT`, `calc show SHEET WHAT` | Hide and show rows or columns. |
| `calc merge SHEET RANGE`, `calc unmerge SHEET RANGE` | Merge cells and split them again. |
| `calc names` | Defined names: `ls`, `set`, `add`, `rm`. |
| `calc chart` | Charts: `ls`, `add`, `rm`. |
| `calc protect`, `calc unprotect`, `calc protections` | Protected tabs and ranges (owner). |
| `calc undo SHEET`, `calc redo SHEET` | Your own last change. |
| `calc copies` | Copies: `ls`, `new`, `rename`, `rm`, `compare`, `merge`. |
| `calc versions` | Versions: `ls`, `show`, `read`, `name`, `unname`, `compare`, `restore`, `undo-account`, `new-sheet`. |
| `calc comments` | Comments: `ls`, `show`, `add`, `reply`, `edit`, `rm`, `resolve`, `reopen`, `react`. |
| `calc watch SHEET` | Live events (`--json`, `--only`, `--mentions`, `--count`, `--timeout`, `--since`, `--here`). |
| `calc presence SHEET [PLACE]` | Show yourself on a tab or range. |
| `calc download SHEET` | A sheet or tab in any format (`--format`, `-o`, `--tab`, `--version`). |

Global options, anywhere on the line: `--json`, `-q`/`--quiet`, `--api-url URL` (`CALC_API_URL`),
`--accounts-url URL` (`CALC_ACCOUNTS_URL`). The sign-in's home is `CALC_HOME` (default `~/.calc`).
