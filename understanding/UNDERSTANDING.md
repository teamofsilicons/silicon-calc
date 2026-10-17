# This file is only meant to be changed by carbons (humans), if you are an agent DONT EDIT THIS FILE.


# UNDERSTANDING.md - Calc

This understanding contains the understanding for the entire Silicon Calc, the service, the web app at `calc.teamofsilicons.com` and the `calc` CLI.

Silicon Calc is where Carbons and Silicons work on spreadsheets together. It is just a normal spreadsheet, with everything people expect from Google Sheets or Microsoft Excel, formulas included. The difference is that Silicons are native users: they open, edit, analyse and comment on the same sheets as Carbons, through the CLI, as easily as Carbons do in the web app.

Carbons will mostly bring in data, comment and make small changes. Silicons will mostly do the analysis. So loading data and commenting have to be effortless for Carbons, and Silicons get far more powerful tools than formulas: they can query and change a sheet with SQL, Python or JavaScript, run right inside Silicon Calc.


# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent account is referred to as a Silicon.
`Sheet` - A single spreadsheet in Silicon Calc. A sheet holds one or more tabs.
`Tab` - One grid of cells inside a sheet.
`Copy` - A duplicate of a sheet kept inside it, to try things without touching the main sheet. It can be merged back.
`Folder` - A place to keep sheets and other folders together.
`Owner` - The Carbon or Silicon a sheet or folder belongs to. Every sheet and folder has exactly one owner.
`Comment` - A note attached to a cell, range, row, column or tab. A comment and its replies are a `thread`.


# Sign in

Signing in and signing up are handled entirely by Silicon Accounts. Use the official and latest Silicon Accounts client everywhere. Carbons and Silicons are always stored by their Silicon Accounts uuid, and shown by their c:id or si:id.

Reading a public sheet never needs signing in.


# Sheets and folders

Sheets and folders work exactly as docs and folders do in Silicon Write. Sheets are personal, and every sheet belongs to the Carbon or Silicon who created it. There are no Teams or workspaces. Carbons and Silicons can both create sheets and invite whoever they need. Ownership, transfers, folders and the trash all work the same way as in Silicon Write.


# Size

A sheet can hold millions of rows and still feel instant: scrolling, sorting, filtering, formulas and queries stay fast, in the web app and in the CLI.


# Formats

Silicon Calc handles every common spreadsheet and data format. Any of these can be opened, which creates a new sheet from the file, or added to an existing sheet as a new tab. Any sheet or tab can be downloaded as any of them:

| Format          | Extension              |
| --------------- | ---------------------- |
| CSV             | `.csv`                 |
| TSV             | `.tsv`                 |
| Excel           | `.xlsx`, `.xls`        |
| OpenDocument    | `.ods`                 |
| Numbers         | `.numbers`             |
| JSON            | `.json`                |
| Parquet         | `.parquet`             |
| PDF             | `.pdf` (download only) |

Formulas, formatting, charts and anything else a format can hold are never dropped. They stay in the sheet and come back out in every format that supports them.


# Sharing

Sharing works exactly as in Silicon Write: invite by c:id, si:id or email, with the same three roles (`Editor`, `Commenter`, `Viewer`), shared folders and public links. Carbons and Silicons get exactly the same roles and the same rights. A Silicon invited as an editor edits directly, like any other editor.

## Protected ranges

The owner can protect tabs or ranges so that only chosen Carbons and Silicons can change them, for example to keep the original data a Carbon loaded untouched. This is off by default.


# Editing

Everyone edits the same sheet at the same time and sees every change as it happens. Every cell shows who last changed it and when.

## Formulas

Formulas work exactly as people expect from Google Sheets and Excel, and formulas in opened Excel files keep working.

## Code

Anyone who can edit can run SQL, Python or JavaScript on a sheet, inside Silicon Calc. Any tab or range can be used as a table, with its first row as the column names, so a Silicon can ask a question of a million rows as easily as of ten.

What code produces can be written into cells or into a new tab. A tab made by code remembers the code that made it and who ran it, so anyone can see how any number came to be. It can either update itself when the data it came from changes, or stay exactly as it was.

## Copies

Anyone who can edit can make a copy of a sheet inside it, to try something without touching the main sheet. Copies can have their own copies, so a Silicon can try many ideas at once. A copy can be compared with the sheet it came from, and merged back in whole or in part. Copies that aren't needed can be deleted, and they stay in the history like everything else.


# Comments

Anyone who can comment can attach a comment to a cell, a range, a row, a column or a tab. Comments can be replied to, reacted to, edited and resolved, and resolved threads can be reopened. A comment stays on its cells as rows and columns move around it.


# Mentions and notifications

Anyone can mention a Carbon or Silicon in a comment with their c:id or si:id. All notifications go through Silicon Ting; Silicon Calc never sends its own.

| Ting                      | When                                     | Sent to                 |
| ------------------------- | ---------------------------------------- | ----------------------- |
| `calc.mention.received`   | Someone mentions you in a comment        | The one mentioned       |
| `calc.comment.created`    | Someone comments on your sheet           | The owner               |
| `calc.comment.replied`    | Someone replies in a thread you're in    | Everyone in the thread  |
| `calc.thread.resolved`    | A thread you started is resolved         | Who started it          |
| `calc.copy.merged`        | Someone merges a copy into your sheet    | The owner               |
| `calc.sheet.shared`       | A sheet or folder is shared with you     | The one invited         |

A ting carries enough for a Silicon to act straight away: the sheet, the cells the comment is on, and the comment itself. Mentioning a Silicon is how a Carbon asks it for something, for example selecting a column and writing "@si:analyst why did March drop?".


# Versions

Everything in Silicon Calc is versioned, and nothing is ever lost, exactly as in Silicon Write. There is no save button. Every change is kept with who made it and when, versions can be named, opened, compared and restored, and everything one Carbon or Silicon changed can be undone while leaving everyone else's changes in place. Restoring is itself a new version. No change ever silently replaces another.


# Silicons

The `calc` CLI is how Silicons use Silicon Calc, and it can do everything the web app can.

- **Reading** - any tab or range can be read as CSV or JSON, with formulas and comments shown if asked.
- **Editing** - a Silicon changes cells or ranges by their address, so its change merges cleanly with whatever anyone else is doing at the same time.
- **Code** - a Silicon can run SQL, Python or JavaScript on any sheet it can edit, and save the results into cells or a new tab.
- **Copies** - a Silicon can make copies, work in them, compare them with the main sheet and merge them back.
- **Files** - a Silicon can open any supported file as a new sheet or tab, and download any sheet in any supported format.
- **Comments, versions and search** - exactly as a Carbon can.


# Look and feel

The web app is built entirely with Silicon UI (`https://ui.teamofsilicons.com`).


# What it is not

Silicon Calc is just normal spreadsheets. It is not a database, a dashboard tool, a docs editor or a code editor, and there are no Teams.
