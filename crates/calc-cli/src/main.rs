//! `calc`: the Silicon Calc command line, for Carbons and Silicons.

mod auth;
mod cmd_cells;
mod cmd_code;
mod cmd_comments;
mod cmd_copies;
mod cmd_files;
mod cmd_library;
mod cmd_tabs;
mod cmd_versions;
mod cmd_watch;
mod ctx;
mod error;
mod output;
mod resolve;
mod session;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use silicon_calc_client::{ClearWhat, Item, ThreadFilter, TransferAction};

use crate::cmd_cells::Axis;
use crate::ctx::{Ctx, Options};
use crate::error::{CliError, CliResult};

/// The whole guide (docs/CLI.md), shipped inside the command so it always matches it.
const GUIDE: &str = include_str!("../../../docs/CLI.md");

const ABOUT: &str = "Silicon Calc: spreadsheets Carbons and Silicons work on together.";

const LONG_ABOUT: &str = "\
Silicon Calc: spreadsheets Carbons and Silicons work on together.

The calc command line does everything the web app does: make and share sheets, read and change cells by
their address, run SQL, Python or JavaScript on any tab, work in copies and merge them back, comment with
@mentions, look back through every version, and open or download every common file format.

SHEET is a sheet's id, its link, or a title that matches exactly one sheet you can see. --copy COPY acts on
one of the sheet's copies (its id or name) instead of the main sheet. Ranges are A1 text: B7, A1:C9, A:C
(whole columns), 3:5 (whole rows), with the tab in front when needed: Sales!A1:C9, 'By region'!B2.";

const AFTER_HELP: &str = "\
QUICK START
  calc login                                    Carbon: approve a code in the browser
  printf '%s' \"$STK\" | calc login --silicon si:you --stk-stdin
                                                Silicon: sign in with your si:id and STK
  calc new \"Q3 revenue\"                         a new sheet
  calc import sales.csv                         a file as a new sheet
  calc read \"Q3 revenue\" A1:D20                 cells as a grid (--format csv or json for machines)
  calc write \"Q3 revenue\" B2 '=SUM(C2:C9)'      change cells by their address
  calc run sql \"Q3 revenue\" -c 'select region, sum(amount) from Sales group by 1'
  calc copies new \"Q3 revenue\" \"Try weekly buckets\"
  calc comments add \"Q3 revenue\" B2:B9 '@c:ada March looks low'
  calc watch \"Q3 revenue\" --only comment --mentions --count 1

Every command has --help with examples, and --json for machine output (errors too:
{\"error\":{\"code\",\"message\",\"hint\"}} on stdout). calc docs prints the whole guide; calc docs TOPIC prints
one part of it (calc docs copies).

The sign-in lives in $CALC_HOME/session.json (default ~/.calc/session.json, mode 0600) and refreshes itself.

Exit codes: 0 done; 1 failed (Silicon Calc or Silicon Accounts refused, something isn't there, no network);
2 the command line can't be used as written (a bad flag, value, range or file, an ambiguous title, a missing
confirmation); 130 stopped with Ctrl-C.

No telemetry: calc records and sends nothing about how it is used.";

#[derive(Parser)]
#[command(name = "calc", version, about = ABOUT, long_about = LONG_ABOUT, after_help = AFTER_HELP)]
#[command(propagate_version = true, max_term_width = 110)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args)]
#[command(next_help_heading = "Global options")]
struct Global {
    /// Machine output: one JSON document on stdout (errors too: {"error":{"code","message","hint"}})
    #[arg(long, global = true)]
    json: bool,
    /// Results and errors only: no notices or suggested next steps on stderr
    #[arg(short, long, global = true)]
    quiet: bool,
    /// The Silicon Calc service [default: the one you signed in with, then https://calc-api.teamofsilicons.com]
    #[arg(long, global = true, env = "CALC_API_URL", value_name = "URL")]
    api_url: Option<String>,
    /// Silicon Accounts [default: the one you signed in with, then https://accounts.teamofsilicons.com]
    #[arg(long, global = true, env = "CALC_ACCOUNTS_URL", value_name = "URL")]
    accounts_url: Option<String>,
}

/// A sheet, and the line to act on: the main sheet, or one of its copies.
#[derive(Args, Clone)]
struct On {
    /// The sheet: its id, its link, or its exact title
    sheet: String,
    /// Act on this copy of the sheet (its id or name) instead of the main sheet
    #[arg(long, value_name = "COPY")]
    copy: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    // ── Signing in ──────────────────────────────────────────────────────────────────────────────────────
    /// Sign in: Carbons approve a code in the browser; Silicons use their si:id and STK, or a short-lived token
    #[command(after_help = "\
Examples:
  calc login                                         Carbon: shows a code and a link to approve it
  printf '%s' \"$STK\" | calc login --silicon si:analyst --stk-stdin
                                                     Silicon: signs in with its si:id and STK
  ACCOUNTS_SILICON=si:analyst ACCOUNTS_STK=stk-… calc login
  calc login --slt slt_…                             Silicon: a short-lived token for calc
  silicon-accounts login --app calc -q | calc login --slt-stdin
  calc login status --json                           {\"authenticated\": true, \"id\": \"si:analyst\", …}

A short-lived token works once, within 2 minutes, for the app calc only. The sign-in is kept in
$CALC_HOME/session.json (default ~/.calc), refreshes itself, and lasts until calc logout.")]
    Login(LoginArgs),
    /// Sign out: end the sign-in at Silicon Accounts and delete it from this machine
    #[command(after_help = "Example:\n  calc logout")]
    Logout,
    /// Show who you are signed in as
    #[command(after_help = "Example:\n  calc whoami --json")]
    Whoami,
    /// Show how Silicon Calc signs in: app id, URLs, version (works offline, never fails)
    #[command(after_help = "Example:\n  calc accounts --json")]
    Accounts,
    /// Print the guide to the calc command line, or one part of it
    #[command(after_help = "\
Examples:
  calc docs | less
  calc docs copies          only the part about copies
  calc docs \"sign in\"")]
    Docs {
        /// A part of the guide: sign in, sheets, cells, code, copies, versions, comments, watch, files…
        topic: Vec<String>,
    },
    /// Show the Silicon Calc service: its version, its Silicon Accounts and its limits
    #[command(after_help = "Example:\n  calc meta --json")]
    Meta,
    /// Look up a Carbon or Silicon by its c:id or si:id
    #[command(after_help = "Example:\n  calc lookup si:analyst")]
    Lookup {
        /// c:id or si:id
        id: String,
    },

    // ── Sheets and folders ──────────────────────────────────────────────────────────────────────────────
    /// List your sheets and folders, what is shared with you, or your trash
    #[command(after_help = "\
Examples:
  calc ls                         your sheets and folders (the top level)
  calc ls --folder Finance/Q3     one folder (an id, a path or a name)
  calc ls --shared                shared with you
  calc ls --trash                 your trash
  calc ls --q revenue             titles containing \"revenue\"
  calc ls --all --json            every page, for machines")]
    Ls {
        /// Sheets and folders shared with you
        #[arg(long, conflicts_with = "trash")]
        shared: bool,
        /// Your trash
        #[arg(long)]
        trash: bool,
        /// One folder's contents (an id, a path like Finance/Q3, or a name)
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Only titles containing this text
        #[arg(long = "q", alias = "query", value_name = "TEXT")]
        q: Option<String>,
        /// At most this many sheets per page
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// Every page, not only the first
        #[arg(long)]
        all: bool,
    },
    /// Make a new sheet (one empty tab, \"Tab 1\")
    #[command(after_help = "\
Examples:
  calc new \"Q3 revenue\"
  calc new \"Q3 revenue\" --folder Finance
  calc new --json | jq -r .id")]
    New {
        /// Its title [default: \"Untitled sheet\"]
        title: Option<String>,
        /// The folder to make it in
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
    },
    /// Open a file as a new sheet: .csv .tsv .xlsx .xls .ods .numbers .json .parquet
    #[command(after_help = "\
Examples:
  calc import sales.csv                          title from the file name
  calc import budget.xlsx --title \"Budget 2027\" --folder Finance
  curl -s https://example.test/data.csv | calc import - --format csv --title \"Data\"

Every tab of the file becomes a tab of the sheet; formulas, formatting and charts come along.
To add a file to an existing sheet, use calc tabs import.")]
    Import {
        /// The file (- reads stdin, with --format)
        file: String,
        /// The new sheet's title [default: from the file name]
        #[arg(long)]
        title: Option<String>,
        /// The folder to make it in
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// The file's format when its name doesn't say: csv, tsv, xlsx, xls, ods, numbers, json, parquet
        #[arg(long, short = 'f')]
        format: Option<String>,
    },
    /// Show a sheet: owner, your role, folder, public link, version, copies and tabs
    #[command(after_help = "\
Examples:
  calc info \"Q3 revenue\"
  calc info k3j9xq2mpa7d --copy \"Try weekly buckets\" --json")]
    Info {
        #[command(flatten)]
        on: On,
    },
    /// Rename a sheet
    #[command(after_help = "Example:\n  calc rename \"Q3 revenue\" \"Q3 revenue (final)\"")]
    Rename {
        /// The sheet
        sheet: String,
        /// Its new title
        title: String,
    },
    /// Move a sheet into a folder (/ for the top level)
    #[command(after_help = "\
Examples:
  calc mv \"Q3 revenue\" Finance/Q3
  calc mv k3j9xq2mpa7d /")]
    Mv {
        /// The sheet
        sheet: String,
        /// The folder (an id, a path like Finance/Q3, a name, or / for the top level)
        folder: String,
    },
    /// Move a sheet to the trash (calc restore brings it back)
    #[command(after_help = "Example:\n  calc rm \"Old draft\"")]
    Rm {
        /// The sheet
        sheet: String,
    },
    /// Bring a sheet back from the trash
    #[command(after_help = "Example:\n  calc restore \"Old draft\"")]
    Restore {
        /// The sheet in your trash (its id or exact title)
        sheet: String,
    },
    /// List your trash
    #[command(after_help = "Example:\n  calc trash")]
    Trash {
        /// Every page, not only the first
        #[arg(long)]
        all: bool,
    },
    /// Delete a sheet in the trash forever, with its history (asks first; --confirm for scripts)
    #[command(after_help = "\
Examples:
  calc delete-forever \"Old draft\"                asks you to type the title
  calc delete-forever k3j9xq2mpa7d --confirm      for scripts

Only sheets in the trash can be deleted forever (calc rm moves one there). It can't be undone.")]
    DeleteForever {
        /// The sheet in your trash
        sheet: String,
        /// Delete without asking
        #[arg(long)]
        confirm: bool,
    },
    /// Offer a sheet to another Carbon or Silicon; it becomes theirs when they accept
    #[command(after_help = "\
Examples:
  calc transfer \"Q3 revenue\" c:ada
  calc transfers                                  to see where it stands")]
    Transfer {
        /// The sheet
        sheet: String,
        /// Who gets it: c:id or si:id
        to: String,
    },
    /// List transfers to and from you, or accept, decline or cancel one
    #[command(after_help = "\
Examples:
  calc transfers
  calc transfers accept t3k9xq2mpa7d
  calc transfers cancel t3k9xq2mpa7d")]
    Transfers {
        #[command(subcommand)]
        action: Option<TransfersCmd>,
    },
    /// Folders: make, list, rename, move, delete, restore, delete forever, transfer
    #[command(subcommand)]
    Folders(FoldersCmd),
    /// Sharing: who has a sheet or folder, invite with a role, change or remove roles, the public link
    #[command(subcommand)]
    Share(ShareCmd),
    /// Find sheets by title and by the text in their cells
    #[command(after_help = "\
Examples:
  calc search \"March\"
  calc search revenue --json")]
    Search {
        /// The words to find
        text: String,
        /// At most this many sheets
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },

    // ── Tabs ────────────────────────────────────────────────────────────────────────────────────────────
    /// Tabs: list, show, add, rename, move, delete, colour, open a file into, keep a code tab updated
    #[command(subcommand)]
    Tabs(TabsCmd),

    // ── Cells ───────────────────────────────────────────────────────────────────────────────────────────
    /// Read cells as a grid, CSV, TSV or JSON (with formulas, looks, comments, or as at a past version)
    #[command(after_help = "\
Examples:
  calc read \"Q3 revenue\"                          the first tab's used area as a grid
  calc read \"Q3 revenue\" Sales!A1:D20
  calc read \"Q3 revenue\" Sales --format csv > sales.csv
  calc read \"Q3 revenue\" B:B --format values      {\"values\": [[…], …]} for machines
  calc read \"Q3 revenue\" A1:D9 --formulas --comments
  calc read \"Q3 revenue\" --format json            every cell: value, shown text, formula, style, who, when
  calc read \"Q3 revenue\" --version 40             as it was at version 40 (or a version's name)

Formats: table (default), csv and tsv (shown values; --formulas for formulas), json (the full cells answer),
values (plain values: numbers, text, true/false, null). With --json the default format is json.")]
    Read(ReadCmd),
    /// One cell: its value, formula, look, who changed it last and when, its history and comments
    #[command(after_help = "\
Examples:
  calc cell \"Q3 revenue\" B7
  calc cell \"Q3 revenue\" Sales!B7 --json")]
    Cell {
        #[command(flatten)]
        on: On,
        /// The cell: B7 or Sales!B7
        address: String,
        /// The tab (or name it in the address)
        #[arg(long)]
        tab: Option<String>,
    },
    /// Change cells by their address: type inputs, or write rows from CSV, TSV or JSON
    #[command(after_help = "\
Examples:
  calc write \"Q3 revenue\" B2 1200                   one cell
  calc write \"Q3 revenue\" C2 '=B2*1.2'              a formula
  calc write \"Q3 revenue\" A1 region amount date     across a row
  calc write \"Q3 revenue\" A2 EU US APAC --down       down a column
  calc write \"Q3 revenue\" --at Sales!A1 --csv sales.csv
  some-command | calc write \"Q3 revenue\" --at A1 --tsv -
  calc write \"Q3 revenue\" --at A1 --values '[[\"name\",12],[\"ada\",true]]'
  calc write \"Q3 revenue\" B2 -- -5                  -- before inputs that start with -

Inputs are read as a person typing them would be: =formulas, 12%, 2026-10-17, TRUE, numbers. --raw keeps
text exactly as given. Your change touches only the cells you name, so it merges cleanly with what anyone
else is doing; when it replaces someone else's newer change, calc tells you (the earlier value stays in
the history).")]
    Write(WriteCmd),
    /// Change how cells look: bold, italic, underline, strike, colours, alignment, wrap, number format, borders
    #[command(after_help = "\
Examples:
  calc style \"Q3 revenue\" A1:D1 --bold --fill '#FFF2CC'
  calc style \"Q3 revenue\" B2:B100 --format '#,##0.00' --align right
  calc style \"Q3 revenue\" Sales!C:C --format 0%
  calc style \"Q3 revenue\" A1 --no-bold --color none
  calc style \"Q3 revenue\" A1:D20 --border thin --border-color '#999999'
  calc style \"Q3 revenue\" A1:D20 --reset              back to the default look")]
    Style(StyleCmd),
    /// Clear cells' contents (default), their look (--formats), or both (--all)
    #[command(after_help = "\
Examples:
  calc clear \"Q3 revenue\" B2:D9
  calc clear \"Q3 revenue\" Sales!A:A --all")]
    Clear {
        #[command(flatten)]
        on: On,
        /// The cells
        range: String,
        /// The tab (or name it in the range)
        #[arg(long)]
        tab: Option<String>,
        /// Clear only the look, keeping the values
        #[arg(long)]
        formats: bool,
        /// Clear values and look
        #[arg(long, conflicts_with = "formats")]
        all: bool,
    },
    /// Insert empty rows before a row
    #[command(after_help = "\
Examples:
  calc insert-rows \"Q3 revenue\" 5                  one empty row at row 5 (row 5 moves down)
  calc insert-rows \"Q3 revenue\" Sales!2 --count 3")]
    InsertRows {
        #[command(flatten)]
        on: On,
        /// The row number the new rows start at
        at: String,
        /// How many
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// The tab (or name it: Sales!5)
        #[arg(long)]
        tab: Option<String>,
    },
    /// Insert empty columns before a column
    #[command(after_help = "\
Examples:
  calc insert-cols \"Q3 revenue\" C                  one empty column at C (C moves right)
  calc insert-cols \"Q3 revenue\" Sales!B --count 2")]
    InsertCols {
        #[command(flatten)]
        on: On,
        /// The column the new columns start at
        at: String,
        /// How many
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// The tab (or name it: Sales!C)
        #[arg(long)]
        tab: Option<String>,
    },
    /// Delete rows (formulas pointing at them show #REF!; undo brings them back)
    #[command(after_help = "\
Examples:
  calc delete-rows \"Q3 revenue\" 7
  calc delete-rows \"Q3 revenue\" Sales!5:9")]
    DeleteRows {
        #[command(flatten)]
        on: On,
        /// The rows: 7 or 5:9
        rows: String,
        /// The tab (or name it: Sales!5:9)
        #[arg(long)]
        tab: Option<String>,
    },
    /// Delete columns
    #[command(after_help = "\
Examples:
  calc delete-cols \"Q3 revenue\" D
  calc delete-cols \"Q3 revenue\" Sales!D:F")]
    DeleteCols {
        #[command(flatten)]
        on: On,
        /// The columns: D or D:F
        cols: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Move rows to another place (comments and formulas move with their cells)
    #[command(after_help = "\
Examples:
  calc move-rows \"Q3 revenue\" 9 --to 2            row 9 becomes row 2
  calc move-rows \"Q3 revenue\" Sales!5:7 --to 20")]
    MoveRows {
        #[command(flatten)]
        on: On,
        /// The rows: 9 or 5:7
        rows: String,
        /// The row they move to
        #[arg(long, value_name = "ROW")]
        to: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Move columns to another place
    #[command(after_help = "\
Examples:
  calc move-cols \"Q3 revenue\" E --to B
  calc move-cols \"Q3 revenue\" Sales!C:D --to A")]
    MoveCols {
        #[command(flatten)]
        on: On,
        /// The columns: E or C:D
        cols: String,
        /// The column they move to
        #[arg(long, value_name = "COLUMN")]
        to: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Sort a range's rows by one or more columns
    #[command(after_help = "\
Examples:
  calc sort \"Q3 revenue\" --by B:desc --header        the used area, keeping row 1 on top
  calc sort \"Q3 revenue\" Sales!A2:D500 --by C --by B:desc")]
    Sort {
        #[command(flatten)]
        on: On,
        /// The cells to sort [default: the tab's used area]
        range: Option<String>,
        /// A column to sort by, with :asc (default) or :desc; repeat for more keys
        #[arg(long, value_name = "COLUMN[:asc|desc]", required = true)]
        by: Vec<String>,
        /// The first row is a header: it stays where it is
        #[arg(long)]
        header: bool,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Fill a range from a cell, as dragging its corner would (formulas adjust, series continue)
    #[command(after_help = "\
Examples:
  calc fill \"Q3 revenue\" C2 C3:C1000
  calc fill \"Q3 revenue\" Sales!A2:A3 A4:A100      continues the series")]
    Fill {
        #[command(flatten)]
        on: On,
        /// The cell or cells to fill from
        from: String,
        /// The range to fill
        to: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Freeze the top rows and left columns of a tab (or --off)
    #[command(after_help = "\
Examples:
  calc freeze \"Q3 revenue\" --rows 1
  calc freeze \"Q3 revenue\" --rows 1 --cols 2 --tab Sales
  calc freeze \"Q3 revenue\" --off")]
    Freeze {
        #[command(flatten)]
        on: On,
        /// How many rows stay at the top
        #[arg(long, value_name = "N")]
        rows: Option<u32>,
        /// How many columns stay at the left
        #[arg(long, value_name = "N")]
        cols: Option<u32>,
        /// Freeze nothing
        #[arg(long, conflicts_with_all = ["rows", "cols"])]
        off: bool,
        /// The tab [default: the first]
        #[arg(long)]
        tab: Option<String>,
    },
    /// Set the width of columns in pixels (auto for the default)
    #[command(after_help = "\
Examples:
  calc width \"Q3 revenue\" A:C 140
  calc width \"Q3 revenue\" Sales!B auto")]
    Width {
        #[command(flatten)]
        on: On,
        /// The columns: B or A:C
        cols: String,
        /// Pixels, or auto
        size: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Set the height of rows in pixels (auto for the default)
    #[command(after_help = "\
Examples:
  calc height \"Q3 revenue\" 1 40
  calc height \"Q3 revenue\" 2:9 auto")]
    Height {
        #[command(flatten)]
        on: On,
        /// The rows: 1 or 2:9
        rows: String,
        /// Pixels, or auto
        size: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Hide rows or columns
    #[command(after_help = "\
Examples:
  calc hide \"Q3 revenue\" 3:5
  calc hide \"Q3 revenue\" Sales!C:D")]
    Hide {
        #[command(flatten)]
        on: On,
        /// Rows (3:5) or columns (C:D)
        what: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Show hidden rows or columns again
    #[command(visible_alias = "unhide", after_help = "Example:\n  calc show \"Q3 revenue\" 3:5")]
    Show {
        #[command(flatten)]
        on: On,
        /// Rows (3:5) or columns (C:D)
        what: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Merge a range into one cell
    #[command(after_help = "Example:\n  calc merge \"Q3 revenue\" A1:D1")]
    Merge {
        #[command(flatten)]
        on: On,
        /// The cells
        range: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Split merged cells again
    #[command(after_help = "Example:\n  calc unmerge \"Q3 revenue\" A1:D1")]
    Unmerge {
        #[command(flatten)]
        on: On,
        /// The cells
        range: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Defined names (Revenue = Sales!$B$2:$B$100): list, set, add, remove
    #[command(subcommand)]
    Names(NamesCmd),
    /// Charts: list, add, delete
    #[command(subcommand)]
    Chart(ChartCmd),
    /// Protect a tab or range so only the owner and chosen Carbons and Silicons can change it (owner only)
    #[command(after_help = "\
Examples:
  calc protect \"Q3 revenue\" Sales!A1:D500 --description \"Original data\"
  calc protect \"Q3 revenue\" Sales --editor si:analyst --editor c:ada     the whole tab
  calc protections \"Q3 revenue\"                                          what is protected")]
    Protect {
        #[command(flatten)]
        on: On,
        /// The range, or a tab's name for the whole tab [default: the whole first tab]
        range: Option<String>,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
        /// Who else may change it: c:id or si:id (repeat, or separate with commas)
        #[arg(long = "editor", value_name = "ID")]
        editors: Vec<String>,
        /// What it is for
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,
    },
    /// Remove a protection (owner only)
    #[command(after_help = "Example:\n  calc unprotect \"Q3 revenue\" p1")]
    Unprotect {
        #[command(flatten)]
        on: On,
        /// The protection's id (calc protections lists them)
        id: String,
    },
    /// List what is protected, and who may change it
    #[command(after_help = "Example:\n  calc protections \"Q3 revenue\"")]
    Protections {
        #[command(flatten)]
        on: On,
        /// Only this tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Undo your own last change on the sheet (everyone else's stay)
    #[command(after_help = "\
Examples:
  calc undo \"Q3 revenue\"
  calc undo \"Q3 revenue\" --copy \"Try weekly buckets\"")]
    Undo {
        #[command(flatten)]
        on: On,
    },
    /// Redo the change you last undid
    #[command(after_help = "Example:\n  calc redo \"Q3 revenue\"")]
    Redo {
        #[command(flatten)]
        on: On,
    },

    // ── Code ────────────────────────────────────────────────────────────────────────────────────────────
    /// Run SQL, Python or JavaScript on a sheet; print the result, or write it into cells or a new tab
    #[command(after_help = "\
Examples:
  calc run sql \"Q3 revenue\" -c 'select region, sum(amount) as total from Sales group by 1 order by 2 desc'
  calc run sql \"Q3 revenue\" -c 'select * from \"By region\" limit 5' --format csv
  calc run python \"Q3 revenue\" analysis.py --into-new-tab Forecast --keep-updated
  calc run js \"Q3 revenue\" -c 'return calc.tab(\"Sales\").filter(r => r.amount > 1000)' --into 'Big!A1'
  cat query.sql | calc run sql \"Q3 revenue\"

Every tab is a table named as the tab is (first row = column names). SQL is DuckDB, read only. Python has
pandas and numpy: calc.tab(\"Sales\") is a DataFrame, calc.sql(\"select …\") too; the result is
calc.result(df) or a variable named result. JavaScript: calc.tab(\"Sales\") gives row objects; return the
result. A tab made with --into-new-tab remembers its code and who ran it; --keep-updated reruns it whenever
a tab it reads changes.")]
    Run(RunCmd),
    /// Rerun the code that made a tab, as you
    #[command(after_help = "Example:\n  calc rerun \"Q3 revenue\" Forecast")]
    Rerun {
        #[command(flatten)]
        on: On,
        /// The tab made by code
        tab: String,
        /// table (default), csv, tsv or json
        #[arg(long, short = 'f')]
        format: Option<String>,
        /// The table shows at most this many rows [default: 100]
        #[arg(long, value_name = "N")]
        max_rows: Option<usize>,
    },

    // ── Copies, versions, comments ──────────────────────────────────────────────────────────────────────
    /// Copies of a sheet kept inside it: list, make, rename, delete, compare, merge back
    #[command(subcommand)]
    Copies(CopiesCmd),
    /// Versions: list, show, read, name, compare, restore, undo one account's changes, new sheet from one
    #[command(subcommand)]
    Versions(VersionsCmd),
    /// Comments on cells, ranges, rows, columns and tabs: list, show, add, reply, edit, delete, resolve, react
    #[command(subcommand)]
    Comments(CommentsCmd),

    // ── Live ────────────────────────────────────────────────────────────────────────────────────────────
    /// Stream a sheet's events as they happen: changes, comments, copies, who is here
    #[command(after_help = "\
Examples:
  calc watch \"Q3 revenue\"                                          readable lines until Ctrl-C
  calc watch \"Q3 revenue\" --json                                   one JSON object per line
  calc watch \"Q3 revenue\" --only comment --mentions --count 1 --timeout 3600
                                         wait (up to an hour) for a comment that mentions you
  calc watch \"Q3 revenue\" --since 40                               what happened after version 40, then live
  calc watch \"Q3 revenue\" --here Sales!B2                          others see you on Sales!B2

With --count and --timeout, not reaching the count in time exits 1 (code timed_out). The stream
reconnects by itself from the last version seen, so nothing is missed.")]
    Watch(WatchCmd),
    /// Show yourself on a tab or range to everyone viewing the sheet (for 45 seconds)
    #[command(after_help = "Example:\n  calc presence \"Q3 revenue\" Sales!B2:B9")]
    Presence {
        #[command(flatten)]
        on: On,
        /// The tab or range: Sales, Sales!B2:B9, or B2 on the first tab [default: the first tab]
        place: Option<String>,
    },

    // ── Files ───────────────────────────────────────────────────────────────────────────────────────────
    /// Download a sheet (or one tab, at any version) as xlsx, xls, ods, numbers, csv, tsv, json, parquet or pdf
    #[command(after_help = "\
Examples:
  calc download \"Q3 revenue\"                                saves \"Q3 revenue.xlsx\" here
  calc download \"Q3 revenue\" --format csv --tab Sales -o sales.csv
  calc download \"Q3 revenue\" -o report.pdf                  the format from the name
  calc download \"Q3 revenue\" --format json --formulas -o - | jq .
  calc download \"Q3 revenue\" --version \"Sent to the client\" -o sent.xlsx

csv, tsv and parquet hold one tab (--tab, default the first); the others hold every tab, with formulas,
formatting and charts wherever the format can hold them.")]
    Download(DownloadCmd),
}

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
struct LoginArgs {
    #[command(subcommand)]
    command: Option<LoginCmd>,
    /// Silicon: a short-lived token (slt_…) from `silicon-accounts login --app calc -q`
    #[arg(long, value_name = "SLT", conflicts_with_all = ["slt_stdin", "silicon"])]
    slt: Option<String>,
    /// Silicon: read the short-lived token from stdin
    #[arg(long, conflicts_with = "silicon")]
    slt_stdin: bool,
    /// Silicon: sign in as this si:id, with its STK [env: ACCOUNTS_SILICON]
    #[arg(long, value_name = "SI_ID")]
    silicon: Option<String>,
    /// Silicon: the STK as an argument (every process can see it; prefer --stk-stdin or ACCOUNTS_STK)
    #[arg(long, value_name = "STK", conflicts_with = "stk_stdin")]
    stk: Option<String>,
    /// Silicon: read the STK from stdin
    #[arg(long)]
    stk_stdin: bool,
    /// Sign in even when already signed in (the sign-in it replaces is ended)
    #[arg(long)]
    force: bool,
    /// How this sign-in is named among your Silicon Accounts sign-ins [default: \"calc CLI on <host>\"]
    #[arg(long, value_name = "TEXT")]
    label: Option<String>,
    /// Carbon: only print the link; don't open the browser
    #[arg(long)]
    no_browser: bool,
}

#[derive(Subcommand)]
enum LoginCmd {
    /// Whether you are signed in, and as whom (with --json it always exits 0: {"authenticated": false} when not)
    #[command(after_help = "\
Examples:
  calc login status
  calc login status --json          {\"authenticated\": true, \"id\": \"si:analyst\", \"kind\": \"silicon\", …}
  calc login status --offline       reads only the stored sign-in, no network")]
    Status {
        /// Only read the stored sign-in: no network
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Subcommand)]
enum TransfersCmd {
    /// Take a sheet or folder offered to you
    Accept {
        /// The transfer's id (calc transfers lists them)
        id: String,
    },
    /// Refuse a sheet or folder offered to you
    Decline {
        /// The transfer's id
        id: String,
    },
    /// Withdraw a transfer you offered
    Cancel {
        /// The transfer's id
        id: String,
    },
}

#[derive(Subcommand)]
enum FoldersCmd {
    /// Make a folder
    #[command(after_help = "\
Examples:
  calc folders new Finance
  calc folders new Q3 --in Finance")]
    New {
        /// Its name
        name: String,
        /// The folder to make it in [default: the top level]
        #[arg(long = "in", value_name = "FOLDER")]
        parent: Option<String>,
    },
    /// List your folders, or one folder's contents
    #[command(visible_alias = "list", after_help = "\
Examples:
  calc folders ls
  calc folders ls Finance/Q3")]
    Ls {
        /// The folder (an id, a path like Finance/Q3, or a name)
        folder: Option<String>,
        /// Every page, not only the first
        #[arg(long)]
        all: bool,
    },
    /// Rename a folder
    #[command(after_help = "Example:\n  calc folders rename Finance \"Finance 2027\"")]
    Rename {
        /// The folder
        folder: String,
        /// Its new name
        name: String,
    },
    /// Move a folder into another (/ for the top level)
    #[command(after_help = "\
Examples:
  calc folders mv Finance/Q3 Archive
  calc folders mv Archive/Q3 /")]
    Mv {
        /// The folder
        folder: String,
        /// Where to: a folder, or / for the top level
        to: String,
    },
    /// Move a folder and everything in it to the trash
    #[command(after_help = "Example:\n  calc folders rm Archive")]
    Rm {
        /// The folder
        folder: String,
    },
    /// Bring a folder back from the trash
    #[command(after_help = "Example:\n  calc folders restore Archive")]
    Restore {
        /// The folder in your trash (its id or name)
        folder: String,
    },
    /// Delete a folder in the trash forever, with everything in it (asks first; --confirm for scripts)
    #[command(after_help = "Example:\n  calc folders delete-forever f3k9xq2mpa7d --confirm")]
    DeleteForever {
        /// The folder in your trash
        folder: String,
        /// Delete without asking
        #[arg(long)]
        confirm: bool,
    },
    /// Offer a folder to another Carbon or Silicon
    #[command(after_help = "Example:\n  calc folders transfer Finance c:ada")]
    Transfer {
        /// The folder
        folder: String,
        /// Who gets it: c:id or si:id
        to: String,
    },
}

#[derive(Subcommand)]
enum ShareCmd {
    /// Who has a sheet or folder, with what role, and the public link
    #[command(visible_alias = "list", after_help = "\
Examples:
  calc share ls \"Q3 revenue\"
  calc share ls Finance --folder")]
    Ls {
        /// The sheet (or folder)
        item: String,
        /// The item is a folder (when a sheet has the same name)
        #[arg(long)]
        folder: bool,
    },
    /// Invite Carbons and Silicons (c:id, si:id or email) with a role
    #[command(after_help = "\
Examples:
  calc share add \"Q3 revenue\" si:analyst --role editor
  calc share add \"Q3 revenue\" c:ada friend@example.com --role commenter
  calc share add Finance c:ada --role viewer --folder

Roles: editor (edits, runs code, makes and merges copies, invites), commenter, viewer. An email with no
account yet becomes an invite that turns into a role when that address first signs in to Silicon Calc.")]
    Add {
        /// The sheet (or folder)
        item: String,
        /// c:id, si:id or email addresses
        #[arg(required = true)]
        who: Vec<String>,
        /// editor, commenter or viewer
        #[arg(long, default_value = "editor")]
        role: String,
        /// Don't tell them (no ting is sent)
        #[arg(long)]
        no_notify: bool,
        /// The item is a folder
        #[arg(long)]
        folder: bool,
    },
    /// Change someone's role
    #[command(after_help = "Example:\n  calc share role \"Q3 revenue\" si:analyst viewer")]
    Role {
        /// The sheet (or folder)
        item: String,
        /// c:id or si:id
        who: String,
        /// editor, commenter or viewer
        role: String,
        /// The item is a folder
        #[arg(long)]
        folder: bool,
    },
    /// Remove someone (or an email invite)
    #[command(after_help = "\
Examples:
  calc share rm \"Q3 revenue\" si:analyst
  calc share rm \"Q3 revenue\" friend@example.com")]
    Rm {
        /// The sheet (or folder)
        item: String,
        /// c:id, si:id, or the invited email address
        who: String,
        /// The item is a folder
        #[arg(long)]
        folder: bool,
    },
    /// Leave a sheet or folder shared with you
    #[command(after_help = "Example:\n  calc share leave \"Q3 revenue\"")]
    Leave {
        /// The sheet (or folder)
        item: String,
        /// The item is a folder
        #[arg(long)]
        folder: bool,
    },
    /// Turn a sheet's public link on (view, comment or edit) or off
    #[command(after_help = "\
Examples:
  calc share public \"Q3 revenue\" on                   anyone with the link can view
  calc share public \"Q3 revenue\" on --access comment  signed-in accounts with the link can comment
  calc share public \"Q3 revenue\" off")]
    Public {
        /// The sheet
        sheet: String,
        /// on or off
        state: String,
        /// view (default), comment or edit
        #[arg(long)]
        access: Option<String>,
    },
}

#[derive(Subcommand)]
enum TabsCmd {
    /// List a sheet's tabs
    #[command(visible_alias = "list", after_help = "Example:\n  calc tabs ls \"Q3 revenue\"")]
    Ls {
        #[command(flatten)]
        on: On,
    },
    /// Show one tab: its size, frozen panes, colour, and the code that made it
    #[command(after_help = "Example:\n  calc tabs show \"Q3 revenue\" \"By region\"")]
    Show {
        #[command(flatten)]
        on: On,
        /// The tab (its name or id)
        tab: String,
    },
    /// Add an empty tab
    #[command(after_help = "\
Examples:
  calc tabs add \"Q3 revenue\" Notes
  calc tabs add \"Q3 revenue\" Summary --at 1         as the first tab")]
    Add {
        #[command(flatten)]
        on: On,
        /// Its name
        name: String,
        /// Its position, from 1 [default: last]
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// Rename a tab (formulas pointing at it follow)
    #[command(after_help = "Example:\n  calc tabs rename \"Q3 revenue\" \"Tab 1\" Sales")]
    Rename {
        #[command(flatten)]
        on: On,
        /// The tab
        tab: String,
        /// Its new name
        name: String,
    },
    /// Move a tab to another position (from 1)
    #[command(name = "move", visible_alias = "mv", after_help = "Example:\n  calc tabs move \"Q3 revenue\" Summary 1")]
    Move {
        #[command(flatten)]
        on: On,
        /// The tab
        tab: String,
        /// Its new position, from 1
        to: usize,
    },
    /// Delete a tab (undo brings it back)
    #[command(after_help = "Example:\n  calc tabs rm \"Q3 revenue\" Scratch")]
    Rm {
        #[command(flatten)]
        on: On,
        /// The tab
        tab: String,
    },
    /// Colour a tab (#RRGGBB, or none)
    #[command(after_help = "\
Examples:
  calc tabs color \"Q3 revenue\" Sales '#1F5FB8'
  calc tabs color \"Q3 revenue\" Sales none")]
    Color {
        #[command(flatten)]
        on: On,
        /// The tab
        tab: String,
        /// #RRGGBB, #RGB or none
        color: String,
    },
    /// Open a file into a sheet: every tab of the file as a new tab, or --replace one tab's contents
    #[command(after_help = "\
Examples:
  calc tabs import \"Q3 revenue\" october.csv              a new tab named after the file
  calc tabs import \"Q3 revenue\" budget.xlsx              every tab of the workbook
  calc tabs import \"Q3 revenue\" sales-v2.csv --replace Sales")]
    Import {
        #[command(flatten)]
        on: On,
        /// The file: .csv .tsv .xlsx .xls .ods .numbers .json .parquet (- reads stdin, with --format)
        file: String,
        /// Replace this tab's contents with the file's first tab
        #[arg(long, value_name = "TAB")]
        replace: Option<String>,
        /// The file's format when its name doesn't say
        #[arg(long, short = 'f')]
        format: Option<String>,
    },
    /// Turn a code tab's rerunning on or off
    #[command(after_help = "\
Examples:
  calc tabs keep-updated \"Q3 revenue\" Forecast on
  calc tabs keep-updated \"Q3 revenue\" Forecast off      it stays exactly as it is")]
    KeepUpdated {
        #[command(flatten)]
        on: On,
        /// The tab made by code
        tab: String,
        /// on or off
        state: String,
    },
}

#[derive(Args)]
struct ReadCmd {
    #[command(flatten)]
    on: On,
    /// The range: B7, A1:C9, A:C, 3:5, Sales!A1:C9, or a tab's name [default: the tab's used area]
    range: Option<String>,
    /// The tab [default: the first; or name it in the range]
    #[arg(long)]
    tab: Option<String>,
    /// table, csv, tsv, json or values
    #[arg(long, short = 'f')]
    format: Option<String>,
    /// Formulas instead of their results (table, csv, tsv)
    #[arg(long)]
    formulas: bool,
    /// List each styled cell's look under the table
    #[arg(long)]
    styles: bool,
    /// Show the open comments on these cells
    #[arg(long)]
    comments: bool,
    /// The sheet as it was at this version (a number or a version's name)
    #[arg(long, value_name = "VERSION")]
    version: Option<String>,
    /// The table shows at most this many rows [default: 200]
    #[arg(long, value_name = "N")]
    max_rows: Option<u32>,
}

#[derive(Args)]
struct WriteCmd {
    #[command(flatten)]
    on: On,
    /// The cell to write, or the top-left one of several: B2, Sales!B2
    address: Option<String>,
    /// What to type, one cell each (across; --down for down); - reads one from stdin
    #[arg(value_name = "INPUT", allow_negative_numbers = true)]
    inputs: Vec<String>,
    /// The top-left cell (instead of the address)
    #[arg(long, value_name = "CELL")]
    at: Option<String>,
    /// The tab [default: the first; or name it in the address]
    #[arg(long)]
    tab: Option<String>,
    /// Rows from a CSV file (- reads stdin), typed in as a person would
    #[arg(long, value_name = "FILE|-")]
    csv: Option<String>,
    /// Rows from a TSV file (- reads stdin)
    #[arg(long, value_name = "FILE|-")]
    tsv: Option<String>,
    /// Raw values from a JSON array of rows (a file, - for stdin, or the JSON itself): [["name", 12], ["ada", true]]
    #[arg(long, value_name = "JSON|FILE|-")]
    values: Option<String>,
    /// Keep text exactly as given: no formulas, numbers or dates are read from it
    #[arg(long)]
    raw: bool,
    /// Write the inputs down a column instead of across a row
    #[arg(long)]
    down: bool,
    /// The version you were looking at: overwriting someone's later change is reported
    #[arg(long, value_name = "VERSION")]
    base_version: Option<i64>,
}

#[derive(Args)]
struct StyleCmd {
    #[command(flatten)]
    on: On,
    /// The cells: A1:C1, Sales!B2:B9, B:B, 1:1
    range: String,
    /// The tab
    #[arg(long)]
    tab: Option<String>,
    /// Bold
    #[arg(long)]
    bold: bool,
    /// Not bold
    #[arg(long, conflicts_with = "bold")]
    no_bold: bool,
    /// Italic
    #[arg(long)]
    italic: bool,
    /// Not italic
    #[arg(long, conflicts_with = "italic")]
    no_italic: bool,
    /// Underlined
    #[arg(long)]
    underline: bool,
    /// Not underlined
    #[arg(long, conflicts_with = "underline")]
    no_underline: bool,
    /// Struck through
    #[arg(long)]
    strike: bool,
    /// Not struck through
    #[arg(long, conflicts_with = "strike")]
    no_strike: bool,
    /// Wrap text
    #[arg(long)]
    wrap: bool,
    /// Don't wrap text
    #[arg(long, conflicts_with = "wrap")]
    no_wrap: bool,
    /// Text colour: #RRGGBB, #RGB, or none
    #[arg(long, value_name = "COLOUR")]
    color: Option<String>,
    /// Fill colour: #RRGGBB, #RGB, or none
    #[arg(long, value_name = "COLOUR")]
    fill: Option<String>,
    /// left, center, right or general
    #[arg(long)]
    align: Option<String>,
    /// top, middle or bottom
    #[arg(long)]
    valign: Option<String>,
    /// A number format: 0.00, #,##0, 0%, yyyy-mm-dd, $#,##0.00, or general
    #[arg(long, value_name = "FORMAT", allow_hyphen_values = true)]
    format: Option<String>,
    /// The font family, or default
    #[arg(long)]
    font: Option<String>,
    /// The font size in points, or default
    #[arg(long, value_name = "POINTS")]
    size: Option<String>,
    /// A border around each cell: thin, medium, thick, dashed, dotted, double or none
    #[arg(long, value_name = "LINE")]
    border: Option<String>,
    /// The border's colour
    #[arg(long, value_name = "COLOUR")]
    border_color: Option<String>,
    /// Reset the look to the default first, then apply the rest
    #[arg(long)]
    reset: bool,
}

#[derive(Subcommand)]
enum NamesCmd {
    /// List the sheet's defined names
    #[command(visible_alias = "list", after_help = "Example:\n  calc names ls \"Q3 revenue\"")]
    Ls {
        #[command(flatten)]
        on: On,
    },
    /// Set exactly these names, replacing all others (--clear removes every name)
    #[command(after_help = "\
Examples:
  calc names set \"Q3 revenue\" 'Revenue=Sales!$B$2:$B$100' 'Rate=Notes!$B$1'
  calc names set \"Q3 revenue\" --clear")]
    Set {
        #[command(flatten)]
        on: On,
        /// NAME=REFERENCE pairs
        defs: Vec<String>,
        /// Remove every name
        #[arg(long, conflicts_with = "defs")]
        clear: bool,
    },
    /// Add a name, or point an existing one somewhere else
    #[command(after_help = "Example:\n  calc names add \"Q3 revenue\" Revenue 'Sales!$B$2:$B$100'")]
    Add {
        #[command(flatten)]
        on: On,
        /// The name
        name: String,
        /// What it refers to: a range, a cell, or a formula
        #[arg(allow_hyphen_values = true)]
        reference: String,
    },
    /// Remove a name (formulas using it show #NAME?)
    #[command(after_help = "Example:\n  calc names rm \"Q3 revenue\" Revenue")]
    Rm {
        #[command(flatten)]
        on: On,
        /// The name
        name: String,
    },
}

#[derive(Subcommand)]
enum ChartCmd {
    /// List the charts (of one tab, or of every tab)
    #[command(visible_alias = "list", after_help = "Example:\n  calc chart ls \"Q3 revenue\"")]
    Ls {
        #[command(flatten)]
        on: On,
        /// Only this tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Add a chart of a range (or replace one with --replace ID)
    #[command(after_help = "\
Examples:
  calc chart add \"Q3 revenue\" Sales!A1:C13 --kind line --title \"Revenue by month\"
  calc chart add \"Q3 revenue\" A1:B5 --kind pie --at E2 --width 400 --height 300

The first row and column may be labels. Kinds: line, bar, column, area, pie, scatter.")]
    Add {
        #[command(flatten)]
        on: On,
        /// The data
        range: String,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
        /// line, bar, column, area, pie or scatter
        #[arg(long, default_value = "line")]
        kind: String,
        /// Its title
        #[arg(long)]
        title: Option<String>,
        /// The cell its top-left corner sits on [default: right of the data]
        #[arg(long, value_name = "CELL")]
        at: Option<String>,
        /// Width in pixels
        #[arg(long, default_value_t = 480)]
        width: u32,
        /// Height in pixels
        #[arg(long, default_value_t = 300)]
        height: u32,
        /// Replace this chart instead of adding one
        #[arg(long, value_name = "ID")]
        replace: Option<String>,
    },
    /// Delete a chart
    #[command(after_help = "Example:\n  calc chart rm \"Q3 revenue\" ch1")]
    Rm {
        #[command(flatten)]
        on: On,
        /// The chart's id (calc chart ls lists them)
        id: String,
    },
}

#[derive(Args)]
struct RunCmd {
    /// sql, python (py) or javascript (js)
    language: String,
    #[command(flatten)]
    on: On,
    /// A file holding the code (- reads stdin)
    file: Option<String>,
    /// The code itself
    #[arg(short = 'c', long = "code", value_name = "CODE", allow_hyphen_values = true)]
    code: Option<String>,
    /// Write the result into a new tab made by this code (it remembers the code and who ran it)
    #[arg(long, value_name = "NAME")]
    into_new_tab: Option<String>,
    /// Write the result into cells, from this top-left cell: 'Sales!E1' or E1
    #[arg(long, value_name = "CELL")]
    into: Option<String>,
    /// With --into-new-tab: rerun whenever a tab the code reads changes
    #[arg(long)]
    keep_updated: bool,
    /// How the result prints: table (default), csv, tsv or json
    #[arg(long, short = 'f')]
    format: Option<String>,
    /// The table shows at most this many rows [default: 100]
    #[arg(long, value_name = "N")]
    max_rows: Option<usize>,
}

#[derive(Subcommand)]
enum CopiesCmd {
    /// A sheet's copies, as a tree (copies of copies under their parents)
    #[command(visible_alias = "list", after_help = "\
Examples:
  calc copies ls \"Q3 revenue\"
  calc copies ls \"Q3 revenue\" --all        merged and deleted ones too")]
    Ls {
        /// The sheet
        sheet: String,
        /// Merged and deleted copies too
        #[arg(long)]
        all: bool,
    },
    /// Make a copy of the sheet (or, with --copy, of one of its copies) as it is now
    #[command(after_help = "\
Examples:
  calc copies new \"Q3 revenue\" \"Try weekly buckets\"
  calc copies new \"Q3 revenue\" \"Weekly, EU only\" --copy \"Try weekly buckets\"     a copy of a copy

Then work in it with --copy: calc write \"Q3 revenue\" B2 0 --copy \"Try weekly buckets\".")]
    New {
        #[command(flatten)]
        on: On,
        /// What the copy is for
        name: String,
    },
    /// Rename a copy
    #[command(after_help = "Example:\n  calc copies rename \"Q3 revenue\" \"Try weekly buckets\" \"Weekly buckets\"")]
    Rename {
        /// The sheet
        sheet: String,
        /// The copy (its id or name)
        copy: String,
        /// Its new name
        name: String,
    },
    /// Delete a copy (its history stays readable)
    #[command(after_help = "Example:\n  calc copies rm \"Q3 revenue\" \"Try weekly buckets\"")]
    Rm {
        /// The sheet
        sheet: String,
        /// The copy
        copy: String,
    },
    /// Compare a copy with where it came from: what changed in it, and what conflicts
    #[command(after_help = "Example:\n  calc copies compare \"Q3 revenue\" \"Try weekly buckets\"")]
    Compare {
        /// The sheet
        sheet: String,
        /// The copy
        copy: String,
        /// List at most this many cells [default: 500]
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// Merge a copy back into where it came from, in whole or in part
    #[command(after_help = "\
Examples:
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\" --dry-run           what would happen
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\"                     everything; conflicts take the copy's
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\" --take sheet        conflicts keep the sheet's
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\" --tab Forecast       only one tab
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\" --range 'Sales!B2:B90' --resolve B7=sheet
  calc copies merge \"Q3 revenue\" \"Try weekly buckets\" --close             and close the copy

Cells changed only in the copy are taken. Cells changed in both, differently, are conflicts: each
takes the copy's value unless --resolve or --take says otherwise.")]
    Merge {
        /// The sheet
        sheet: String,
        /// The copy
        copy: String,
        /// The side every conflict takes: copy (default) or sheet
        #[arg(long, value_name = "SIDE")]
        take: Option<String>,
        /// Only these tabs (repeat for more)
        #[arg(long)]
        tab: Vec<String>,
        /// Only this range: 'Sales!B2:B90'
        #[arg(long)]
        range: Option<String>,
        /// The side one conflicting cell takes: B7=sheet, 'Sales'!C3=copy (repeat for more)
        #[arg(long, value_name = "CELL=SIDE")]
        resolve: Vec<String>,
        /// Only say what would happen
        #[arg(long)]
        dry_run: bool,
        /// Close the copy after merging it
        #[arg(long)]
        close: bool,
    },
}

#[derive(Subcommand)]
enum VersionsCmd {
    /// List versions, newest first
    #[command(visible_alias = "list", after_help = "\
Examples:
  calc versions ls \"Q3 revenue\"
  calc versions ls \"Q3 revenue\" --all --json")]
    Ls {
        #[command(flatten)]
        on: On,
        /// Versions per page [default: 50]
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// Every page
        #[arg(long)]
        all: bool,
    },
    /// Show one version: who, when, and every cell it changed
    #[command(after_help = "\
Examples:
  calc versions show \"Q3 revenue\" 42
  calc versions show \"Q3 revenue\" \"Sent to the client\"")]
    Show {
        #[command(flatten)]
        on: On,
        /// The version (a number or a name)
        version: String,
        /// List at most this many cells [default: 500]
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// Read cells as they were at a version (like calc read --version)
    #[command(after_help = "\
Examples:
  calc versions read \"Q3 revenue\" 40
  calc versions read \"Q3 revenue\" 40 Sales!A1:D9 --format csv")]
    Read {
        #[command(flatten)]
        on: On,
        /// The version (a number or a name)
        version: String,
        /// The range [default: the tab's used area]
        range: Option<String>,
        /// The tab
        #[arg(long)]
        tab: Option<String>,
        /// table, csv, tsv, json or values
        #[arg(long, short = 'f')]
        format: Option<String>,
        /// Formulas instead of their results
        #[arg(long)]
        formulas: bool,
        /// List each styled cell's look
        #[arg(long)]
        styles: bool,
        /// The table shows at most this many rows [default: 200]
        #[arg(long, value_name = "N")]
        max_rows: Option<u32>,
    },
    /// Name a version so it's easy to find again
    #[command(after_help = "Example:\n  calc versions name \"Q3 revenue\" 42 \"Sent to the client\"")]
    Name {
        #[command(flatten)]
        on: On,
        /// The version number
        version: String,
        /// The name
        name: String,
    },
    /// Remove a version's name
    #[command(after_help = "Example:\n  calc versions unname \"Q3 revenue\" 42")]
    Unname {
        #[command(flatten)]
        on: On,
        /// The version (a number or its name)
        version: String,
    },
    /// Compare two versions cell by cell (TO defaults to now)
    #[command(after_help = "\
Examples:
  calc versions compare \"Q3 revenue\" 40              version 40 against now
  calc versions compare \"Q3 revenue\" 40 42 --tab Sales
  calc versions compare \"Q3 revenue\" \"Sent to the client\" --range Sales!B:B")]
    Compare {
        #[command(flatten)]
        on: On,
        /// The older version
        from: String,
        /// The newer version [default: now]
        to: Option<String>,
        /// Only this tab
        #[arg(long)]
        tab: Option<String>,
        /// Only this range
        #[arg(long)]
        range: Option<String>,
        /// List at most this many cells [default: 500]
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// Restore the sheet (or one tab, or one range) as it was at a version; that is a new version itself
    #[command(after_help = "\
Examples:
  calc versions restore \"Q3 revenue\" 40
  calc versions restore \"Q3 revenue\" 40 --tab Sales
  calc versions restore \"Q3 revenue\" 40 --range Sales!B2:B90")]
    Restore {
        #[command(flatten)]
        on: On,
        /// The version (a number or a name)
        version: String,
        /// Only this tab (named as it was then)
        #[arg(long)]
        tab: Option<String>,
        /// Only this range
        #[arg(long)]
        range: Option<String>,
    },
    /// Undo everything one Carbon or Silicon changed after a version, keeping everyone else's changes
    #[command(after_help = "Example:\n  calc versions undo-account \"Q3 revenue\" si:analyst --since 40")]
    UndoAccount {
        #[command(flatten)]
        on: On,
        /// c:id or si:id
        account: String,
        /// Undo their changes after this version
        #[arg(long, value_name = "VERSION")]
        since: String,
    },
    /// Make a new sheet holding the sheet as it was at a version
    #[command(after_help = "Example:\n  calc versions new-sheet \"Q3 revenue\" 40 --title \"Q3 revenue as sent\"")]
    NewSheet {
        #[command(flatten)]
        on: On,
        /// The version (a number or a name)
        version: String,
        /// The new sheet's title
        #[arg(long)]
        title: Option<String>,
    },
}

#[derive(Subcommand)]
enum CommentsCmd {
    /// List a sheet's comment threads (open ones by default)
    #[command(visible_alias = "list", after_help = "\
Examples:
  calc comments ls \"Q3 revenue\"
  calc comments ls \"Q3 revenue\" --all --tab Sales --json")]
    Ls {
        #[command(flatten)]
        on: On,
        /// Only resolved threads
        #[arg(long, conflicts_with = "all")]
        resolved: bool,
        /// Open and resolved threads
        #[arg(long)]
        all: bool,
        /// Only this tab
        #[arg(long)]
        tab: Option<String>,
    },
    /// Show one thread with every comment and reaction
    #[command(after_help = "Example:\n  calc comments show 019a…")]
    Show {
        /// The thread's id
        thread: String,
    },
    /// Comment on a cell, range, rows, columns or a whole tab; @c:id and @si:id mention
    #[command(after_help = "\
Examples:
  calc comments add \"Q3 revenue\" B7 \"Is this the final number?\"
  calc comments add \"Q3 revenue\" Sales!B2:B9 \"@si:analyst why did March drop?\"
  calc comments add \"Q3 revenue\" C:C \"These are in euros\"
  calc comments add \"Q3 revenue\" Sales \"This whole tab comes from the October export\"
  echo \"Long note…\" | calc comments add \"Q3 revenue\" A1 -

A comment stays on its cells as rows and columns move around it. Mentioned Carbons and Silicons hear about
it through Silicon Ting.")]
    Add {
        #[command(flatten)]
        on: On,
        /// Where: B7, A1:C9, B:B, 3:3, Sales!B2:B9, or a tab's name for the whole tab
        place: String,
        /// The text (- reads stdin)
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, value_name = "TEXT")]
        body: Vec<String>,
        /// The tab (or name it in the place)
        #[arg(long)]
        tab: Option<String>,
    },
    /// Reply in a thread
    #[command(after_help = "Example:\n  calc comments reply 019a… \"Three fewer working days; see Notes!A1.\"")]
    Reply {
        /// The thread's id
        thread: String,
        /// The text (- reads stdin)
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, value_name = "TEXT")]
        body: Vec<String>,
    },
    /// Edit your comment
    #[command(after_help = "Example:\n  calc comments edit 019b… \"Better wording\"")]
    Edit {
        /// The comment's id
        comment: String,
        /// The new text (- reads stdin)
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, value_name = "TEXT")]
        body: Vec<String>,
    },
    /// Delete a comment (yours, or any on your sheet)
    #[command(after_help = "Example:\n  calc comments rm 019b…")]
    Rm {
        /// The comment's id
        comment: String,
    },
    /// Resolve a thread
    #[command(after_help = "Example:\n  calc comments resolve 019a…")]
    Resolve {
        /// The thread's id
        thread: String,
    },
    /// Reopen a resolved thread
    #[command(after_help = "Example:\n  calc comments reopen 019a…")]
    Reopen {
        /// The thread's id
        thread: String,
    },
    /// React to a comment with an emoji (or --remove your reaction)
    #[command(after_help = "\
Examples:
  calc comments react 019b… 👍
  calc comments react 019b… 👍 --remove")]
    React {
        /// The comment's id
        comment: String,
        /// The emoji
        emoji: String,
        /// Take your reaction back
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Args)]
struct WatchCmd {
    #[command(flatten)]
    on: On,
    /// First replay what happened after this version (a number or a name) [default: only what happens from now]
    #[arg(long, value_name = "VERSION")]
    since: Option<String>,
    /// Only these kinds: change, comment, copy, presence, reset (comma-separated or repeated)
    #[arg(long, value_name = "KINDS")]
    only: Vec<String>,
    /// Only comments that mention you
    #[arg(long)]
    mentions: bool,
    /// Stop after this many events
    #[arg(long, value_name = "N")]
    count: Option<u64>,
    /// Stop after this many seconds (with --count, not reaching it is exit 1, timed_out)
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    /// Show yourself to everyone viewing the sheet on this tab or range while watching
    #[arg(long, value_name = "PLACE")]
    here: Option<String>,
    /// Stop when the connection drops instead of reconnecting
    #[arg(long)]
    no_reconnect: bool,
    /// Also print the service's keep-alive pings
    #[arg(long, hide = true)]
    pings: bool,
}

#[derive(Args)]
struct DownloadCmd {
    #[command(flatten)]
    on: On,
    /// xlsx (default), xls, ods, numbers, csv, tsv, json, parquet or pdf [default: from -o's name, else xlsx]
    #[arg(long, short = 'f')]
    format: Option<String>,
    /// Where to save: a file, a folder, or - for stdout [default: the sheet's title, here]
    #[arg(short = 'o', long = "output", value_name = "PATH|-")]
    output: Option<String>,
    /// The tab, for formats holding one (csv, tsv, parquet) [default: the first]
    #[arg(long)]
    tab: Option<String>,
    /// The sheet as it was at this version (a number or a name)
    #[arg(long, value_name = "VERSION")]
    version: Option<String>,
    /// json only: cells as {"v", "f"}, with their formulas
    #[arg(long)]
    formulas: bool,
    /// Replace the file if it exists
    #[arg(long)]
    force: bool,
}

fn on_off(s: &str, what: &str) -> CliResult<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        other => Err(CliError::usage(format!("\"{other}\" is neither on nor off."), format!("Write {what} on, or {what} off."))),
    }
}

/// The guide, or the parts of it whose heading names `topic`.
fn docs_part(topic: &str) -> CliResult<String> {
    let topic = topic.trim().to_lowercase();
    if topic.is_empty() {
        return Ok(GUIDE.to_owned());
    }
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in GUIDE.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            sections.push((h.trim().to_owned(), format!("{line}\n")));
        } else if let Some(last) = sections.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    let stem = |w: &str| -> String {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        let w = w.strip_suffix("ies").or_else(|| w.strip_suffix('s')).or_else(|| w.strip_suffix('y')).unwrap_or(w);
        w.to_owned()
    };
    let wanted: Vec<String> = topic.split_whitespace().map(stem).filter(|w| !w.is_empty()).collect();
    let found: Vec<&(String, String)> = sections
        .iter()
        .filter(|(h, _)| {
            let words: Vec<String> = h.to_lowercase().split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_owned()).collect();
            wanted.iter().all(|want| words.iter().any(|w| w.starts_with(want.as_str())))
        })
        .collect();
    if found.is_empty() {
        let names: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
        return Err(CliError::usage(
            format!("The guide has no part about \"{topic}\"."),
            format!("Its parts: {}. calc docs prints all of it.", names.join(", ")),
        ));
    }
    Ok(found.iter().map(|(_, t)| t.trim_end()).collect::<Vec<_>>().join("\n\n"))
}

async fn run(ctx: &Ctx, command: Command) -> CliResult<()> {
    use cmd_cells as cells;
    use cmd_library as lib;
    match command {
        Command::Login(args) => match args.command {
            Some(LoginCmd::Status { offline }) => auth::status(ctx, offline).await,
            None => {
                auth::login(
                    ctx,
                    auth::LoginArgs {
                        slt: args.slt,
                        slt_stdin: args.slt_stdin,
                        silicon: args.silicon,
                        stk: args.stk,
                        stk_stdin: args.stk_stdin,
                        force: args.force,
                        label: args.label,
                        open: !args.no_browser && std::io::stderr().is_terminal() && !ctx.json,
                    },
                )
                .await
            }
        },
        Command::Logout => auth::logout(ctx).await,
        Command::Whoami => auth::whoami(ctx).await,
        Command::Accounts => {
            let value = auth::accounts(Some(ctx.api_url.clone()), Some(ctx.accounts_url.clone()));
            ctx.emit(&value, || auth::accounts_text(&value));
            Ok(())
        }
        Command::Docs { topic } => {
            let text = docs_part(&topic.join(" "))?;
            ctx.emit(&serde_json::json!({"markdown": text}), || text.clone());
            Ok(())
        }
        Command::Meta => lib::meta(ctx).await,
        Command::Lookup { id } => lib::lookup(ctx, &id).await,
        Command::Ls { shared, trash, folder, q, limit, all } => lib::ls(ctx, lib::LsArgs { shared, trash, folder, q, limit, all }).await,
        Command::New { title, folder } => lib::new(ctx, title, folder).await,
        Command::Import { file, title, folder, format } => lib::import(ctx, &file, title, folder, format.as_deref()).await,
        Command::Info { on } => lib::info(ctx, &on.sheet, on.copy.as_deref()).await,
        Command::Rename { sheet, title } => lib::rename(ctx, &sheet, &title).await,
        Command::Mv { sheet, folder } => lib::mv(ctx, &sheet, &folder).await,
        Command::Rm { sheet } => lib::rm(ctx, &sheet).await,
        Command::Restore { sheet } => lib::restore(ctx, &sheet).await,
        Command::Trash { all } => lib::trash(ctx, all).await,
        Command::DeleteForever { sheet, confirm } => lib::delete_forever(ctx, &sheet, confirm).await,
        Command::Transfer { sheet, to } => {
            let s = resolve::sheet(ctx, &sheet, resolve::Look::Visible).await?;
            lib::transfer(ctx, Item::Sheet(s.id.clone()), &format!("\"{}\"", s.title), &to).await
        }
        Command::Transfers { action } => match action {
            None => lib::transfers(ctx).await,
            Some(TransfersCmd::Accept { id }) => lib::transfer_action(ctx, &id, TransferAction::Accept).await,
            Some(TransfersCmd::Decline { id }) => lib::transfer_action(ctx, &id, TransferAction::Decline).await,
            Some(TransfersCmd::Cancel { id }) => lib::transfer_action(ctx, &id, TransferAction::Cancel).await,
        },
        Command::Folders(cmd) => match cmd {
            FoldersCmd::New { name, parent } => lib::folder_new(ctx, &name, parent).await,
            FoldersCmd::Ls { folder, all } => lib::folder_ls(ctx, folder, all).await,
            FoldersCmd::Rename { folder, name } => lib::folder_rename(ctx, &folder, &name).await,
            FoldersCmd::Mv { folder, to } => lib::folder_mv(ctx, &folder, &to).await,
            FoldersCmd::Rm { folder } => lib::folder_rm(ctx, &folder).await,
            FoldersCmd::Restore { folder } => lib::folder_restore(ctx, &folder).await,
            FoldersCmd::DeleteForever { folder, confirm } => lib::folder_delete_forever(ctx, &folder, confirm).await,
            FoldersCmd::Transfer { folder, to } => {
                let f = resolve::folder(ctx, &folder).await?;
                lib::transfer(ctx, Item::Folder(f.id.clone()), &format!("the folder \"{}\"", f.name), &to).await
            }
        },
        Command::Share(cmd) => match cmd {
            ShareCmd::Ls { item, folder } => lib::share_ls(ctx, &item, folder).await,
            ShareCmd::Add { item, who, role, no_notify, folder } => lib::share_add(ctx, &item, &who, &role, !no_notify, folder).await,
            ShareCmd::Role { item, who, role, folder } => lib::share_role(ctx, &item, &who, &role, folder).await,
            ShareCmd::Rm { item, who, folder } => lib::share_rm(ctx, &item, &who, folder).await,
            ShareCmd::Leave { item, folder } => lib::share_leave(ctx, &item, folder).await,
            ShareCmd::Public { sheet, state, access } => lib::share_public(ctx, &sheet, &state, access.as_deref()).await,
        },
        Command::Search { text, limit } => lib::search(ctx, &text, limit).await,
        Command::Tabs(cmd) => match cmd {
            TabsCmd::Ls { on } => cmd_tabs::ls(ctx, &on.sheet, on.copy.as_deref()).await,
            TabsCmd::Show { on, tab } => cmd_tabs::show(ctx, &on.sheet, on.copy.as_deref(), &tab).await,
            TabsCmd::Add { on, name, at } => cmd_tabs::add(ctx, &on.sheet, on.copy.as_deref(), &name, at).await,
            TabsCmd::Rename { on, tab, name } => cmd_tabs::rename(ctx, &on.sheet, on.copy.as_deref(), &tab, &name).await,
            TabsCmd::Move { on, tab, to } => cmd_tabs::move_tab(ctx, &on.sheet, on.copy.as_deref(), &tab, to).await,
            TabsCmd::Rm { on, tab } => cmd_tabs::rm(ctx, &on.sheet, on.copy.as_deref(), &tab).await,
            TabsCmd::Color { on, tab, color } => cmd_tabs::color(ctx, &on.sheet, on.copy.as_deref(), &tab, &color).await,
            TabsCmd::Import { on, file, replace, format } => {
                cmd_tabs::import(ctx, &on.sheet, on.copy.as_deref(), &file, replace.as_deref(), format.as_deref()).await
            }
            TabsCmd::KeepUpdated { on, tab, state } => {
                let on_ = on_off(&state, "keep-updated")?;
                cmd_tabs::keep_updated(ctx, &on.sheet, on.copy.as_deref(), &tab, on_).await
            }
        },
        Command::Read(r) => {
            let args = cells::ReadArgs {
                sheet: r.on.sheet,
                range: r.range,
                tab: r.tab,
                format: r.format,
                formulas: r.formulas,
                styles: r.styles,
                comments: r.comments,
                version: r.version,
                copy: r.on.copy,
                max_rows: r.max_rows,
            };
            cells::read(ctx, args, false).await
        }
        Command::Cell { on, address, tab } => cells::cell(ctx, &on.sheet, &address, tab.as_deref(), on.copy.as_deref()).await,
        Command::Write(w) => {
            let args = cells::WriteArgs {
                sheet: w.on.sheet,
                address: w.address,
                inputs: w.inputs,
                at: w.at,
                tab: w.tab,
                csv: w.csv,
                tsv: w.tsv,
                values: w.values,
                raw: w.raw,
                down: w.down,
                base_version: w.base_version,
                copy: w.on.copy,
            };
            cells::write(ctx, args).await
        }
        Command::Style(s) => {
            let tri = |yes: bool, no: bool| if yes { Some(true) } else if no { Some(false) } else { None };
            let args = cells::StyleArgs {
                sheet: s.on.sheet,
                range: s.range,
                tab: s.tab,
                copy: s.on.copy,
                bold: tri(s.bold, s.no_bold),
                italic: tri(s.italic, s.no_italic),
                underline: tri(s.underline, s.no_underline),
                strike: tri(s.strike, s.no_strike),
                wrap: tri(s.wrap, s.no_wrap),
                color: s.color,
                fill: s.fill,
                align: s.align,
                valign: s.valign,
                format: s.format,
                font: s.font,
                size: s.size,
                border: s.border,
                border_color: s.border_color,
                clear: s.reset,
            };
            cells::style(ctx, args).await
        }
        Command::Clear { on, range, tab, formats, all } => {
            let what = if all {
                ClearWhat::All
            } else if formats {
                ClearWhat::Formats
            } else {
                ClearWhat::Contents
            };
            cells::clear(ctx, &on.sheet, &range, tab.as_deref(), what, on.copy.as_deref()).await
        }
        Command::InsertRows { on, at, count, tab } => cells::insert(ctx, &on.sheet, Axis::Rows, &at, count, tab.as_deref(), on.copy.as_deref()).await,
        Command::InsertCols { on, at, count, tab } => cells::insert(ctx, &on.sheet, Axis::Cols, &at, count, tab.as_deref(), on.copy.as_deref()).await,
        Command::DeleteRows { on, rows, tab } => cells::delete(ctx, &on.sheet, Axis::Rows, &rows, tab.as_deref(), on.copy.as_deref()).await,
        Command::DeleteCols { on, cols, tab } => cells::delete(ctx, &on.sheet, Axis::Cols, &cols, tab.as_deref(), on.copy.as_deref()).await,
        Command::MoveRows { on, rows, to, tab } => cells::move_axis(ctx, &on.sheet, Axis::Rows, &rows, &to, tab.as_deref(), on.copy.as_deref()).await,
        Command::MoveCols { on, cols, to, tab } => cells::move_axis(ctx, &on.sheet, Axis::Cols, &cols, &to, tab.as_deref(), on.copy.as_deref()).await,
        Command::Sort { on, range, by, header, tab } => cells::sort(ctx, &on.sheet, range.as_deref(), &by, header, tab.as_deref(), on.copy.as_deref()).await,
        Command::Fill { on, from, to, tab } => cells::fill(ctx, &on.sheet, &from, &to, tab.as_deref(), on.copy.as_deref()).await,
        Command::Freeze { on, rows, cols, off, tab } => cells::freeze(ctx, &on.sheet, rows, cols, off, tab.as_deref(), on.copy.as_deref()).await,
        Command::Width { on, cols, size, tab } => cells::size(ctx, &on.sheet, Axis::Cols, &cols, &size, tab.as_deref(), on.copy.as_deref()).await,
        Command::Height { on, rows, size, tab } => cells::size(ctx, &on.sheet, Axis::Rows, &rows, &size, tab.as_deref(), on.copy.as_deref()).await,
        Command::Hide { on, what, tab } => cells::hide(ctx, &on.sheet, &what, false, tab.as_deref(), on.copy.as_deref()).await,
        Command::Show { on, what, tab } => cells::hide(ctx, &on.sheet, &what, true, tab.as_deref(), on.copy.as_deref()).await,
        Command::Merge { on, range, tab } => cells::merge(ctx, &on.sheet, &range, false, tab.as_deref(), on.copy.as_deref()).await,
        Command::Unmerge { on, range, tab } => cells::merge(ctx, &on.sheet, &range, true, tab.as_deref(), on.copy.as_deref()).await,
        Command::Names(cmd) => match cmd {
            NamesCmd::Ls { on } => cmd_tabs::names_ls(ctx, &on.sheet, on.copy.as_deref()).await,
            NamesCmd::Set { on, defs, clear } => cmd_tabs::names_set(ctx, &on.sheet, on.copy.as_deref(), &defs, clear).await,
            NamesCmd::Add { on, name, reference } => cmd_tabs::names_add(ctx, &on.sheet, on.copy.as_deref(), &name, &reference).await,
            NamesCmd::Rm { on, name } => cmd_tabs::names_rm(ctx, &on.sheet, on.copy.as_deref(), &name).await,
        },
        Command::Chart(cmd) => match cmd {
            ChartCmd::Ls { on, tab } => cmd_tabs::chart_ls(ctx, &on.sheet, on.copy.as_deref(), tab.as_deref()).await,
            ChartCmd::Add { on, range, tab, kind, title, at, width, height, replace } => {
                let args = cmd_tabs::ChartArgs { sheet: on.sheet, copy: on.copy, range, tab, kind, title, at, width, height, id: replace };
                cmd_tabs::chart_add(ctx, args).await
            }
            ChartCmd::Rm { on, id } => cmd_tabs::chart_rm(ctx, &on.sheet, on.copy.as_deref(), &id).await,
        },
        Command::Protect { on, range, tab, editors, description } => {
            cmd_tabs::protect(ctx, &on.sheet, on.copy.as_deref(), range.as_deref(), tab.as_deref(), &editors, description).await
        }
        Command::Unprotect { on, id } => cmd_tabs::unprotect(ctx, &on.sheet, on.copy.as_deref(), &id).await,
        Command::Protections { on, tab } => cmd_tabs::protections_ls(ctx, &on.sheet, on.copy.as_deref(), tab.as_deref()).await,
        Command::Undo { on } => cells::undo(ctx, &on.sheet, false, on.copy.as_deref()).await,
        Command::Redo { on } => cells::undo(ctx, &on.sheet, true, on.copy.as_deref()).await,
        Command::Run(r) => {
            let args = cmd_code::RunArgs {
                language: r.language,
                sheet: r.on.sheet,
                file: r.file,
                code: r.code,
                into_new_tab: r.into_new_tab,
                into: r.into,
                keep_updated: r.keep_updated,
                copy: r.on.copy,
                max_rows: r.max_rows,
                format: r.format,
            };
            cmd_code::run(ctx, args).await
        }
        Command::Rerun { on, tab, format, max_rows } => {
            cmd_code::rerun(ctx, &on.sheet, on.copy.as_deref(), &tab, format.as_deref(), max_rows).await
        }
        Command::Copies(cmd) => match cmd {
            CopiesCmd::Ls { sheet, all } => cmd_copies::ls(ctx, &sheet, all).await,
            CopiesCmd::New { on, name } => cmd_copies::new(ctx, &on.sheet, on.copy.as_deref(), &name).await,
            CopiesCmd::Rename { sheet, copy, name } => cmd_copies::rename(ctx, &sheet, &copy, &name).await,
            CopiesCmd::Rm { sheet, copy } => cmd_copies::rm(ctx, &sheet, &copy).await,
            CopiesCmd::Compare { sheet, copy, limit } => cmd_copies::compare(ctx, &sheet, &copy, limit).await,
            CopiesCmd::Merge { sheet, copy, take, tab, range, resolve, dry_run, close } => {
                cmd_copies::merge(ctx, cmd_copies::MergeArgs { sheet, copy, take, tabs: tab, range, resolve, dry_run, close }).await
            }
        },
        Command::Versions(cmd) => match cmd {
            VersionsCmd::Ls { on, limit, all } => cmd_versions::ls(ctx, &on.sheet, on.copy.as_deref(), limit, all).await,
            VersionsCmd::Show { on, version, limit } => cmd_versions::show(ctx, &on.sheet, on.copy.as_deref(), &version, limit).await,
            VersionsCmd::Read { on, version, range, tab, format, formulas, styles, max_rows } => {
                let args = cells::ReadArgs {
                    sheet: on.sheet,
                    range,
                    tab,
                    format,
                    formulas,
                    styles,
                    comments: false,
                    version: Some(version),
                    copy: on.copy,
                    max_rows,
                };
                cells::read(ctx, args, true).await
            }
            VersionsCmd::Name { on, version, name } => cmd_versions::name(ctx, &on.sheet, on.copy.as_deref(), &version, &name).await,
            VersionsCmd::Unname { on, version } => cmd_versions::unname(ctx, &on.sheet, on.copy.as_deref(), &version).await,
            VersionsCmd::Compare { on, from, to, tab, range, limit } => {
                cmd_versions::compare(ctx, cmd_versions::CompareArgs { sheet: on.sheet, copy: on.copy, from, to, tab, range, limit }).await
            }
            VersionsCmd::Restore { on, version, tab, range } => {
                cmd_versions::restore(ctx, &on.sheet, on.copy.as_deref(), &version, tab.as_deref(), range.as_deref()).await
            }
            VersionsCmd::UndoAccount { on, account, since } => {
                cmd_versions::undo_account(ctx, &on.sheet, on.copy.as_deref(), &account, &since).await
            }
            VersionsCmd::NewSheet { on, version, title } => cmd_versions::new_sheet(ctx, &on.sheet, on.copy.as_deref(), &version, title).await,
        },
        Command::Comments(cmd) => match cmd {
            CommentsCmd::Ls { on, resolved, all, tab } => {
                let filter = if all {
                    ThreadFilter::All
                } else if resolved {
                    ThreadFilter::Resolved
                } else {
                    ThreadFilter::Open
                };
                cmd_comments::ls(ctx, &on.sheet, on.copy.as_deref(), filter, tab.as_deref()).await
            }
            CommentsCmd::Show { thread } => cmd_comments::show(ctx, &thread).await,
            CommentsCmd::Add { on, place, body, tab } => {
                cmd_comments::add(ctx, &on.sheet, on.copy.as_deref(), Some(&place), tab.as_deref(), &body).await
            }
            CommentsCmd::Reply { thread, body } => cmd_comments::reply(ctx, &thread, &body).await,
            CommentsCmd::Edit { comment, body } => cmd_comments::edit(ctx, &comment, &body).await,
            CommentsCmd::Rm { comment } => cmd_comments::rm(ctx, &comment).await,
            CommentsCmd::Resolve { thread } => cmd_comments::resolve_thread(ctx, &thread, false).await,
            CommentsCmd::Reopen { thread } => cmd_comments::resolve_thread(ctx, &thread, true).await,
            CommentsCmd::React { comment, emoji, remove } => cmd_comments::react(ctx, &comment, &emoji, remove).await,
        },
        Command::Watch(w) => {
            let args = cmd_watch::WatchArgs {
                sheet: w.on.sheet,
                copy: w.on.copy,
                since: w.since,
                only: w.only,
                mentions: w.mentions,
                count: w.count,
                timeout: w.timeout,
                no_reconnect: w.no_reconnect,
                here: w.here,
                pings: w.pings,
            };
            cmd_watch::watch(ctx, args).await
        }
        Command::Presence { on, place } => cmd_watch::presence(ctx, &on.sheet, on.copy.as_deref(), place.as_deref()).await,
        Command::Download(d) => {
            let args = cmd_files::DownloadArgs {
                sheet: d.on.sheet,
                copy: d.on.copy,
                format: d.format,
                output: d.output,
                tab: d.tab,
                version: d.version,
                formulas: d.formulas,
                force: d.force,
            };
            cmd_files::download(ctx, args).await
        }
    }
}

fn report(json: bool, err: &CliError) -> ExitCode {
    if json {
        println!("{}", serde_json::to_string_pretty(&err.to_json()).unwrap_or_default());
    } else {
        eprintln!("error: {}", err.message);
        if let Some(hint) = &err.hint {
            eprintln!("hint: {hint}");
        }
    }
    ExitCode::from(err.exit)
}

/// A clap error as calc's own: the first lines of clap's message, exit 2.
fn usage_error(e: &clap::Error) -> CliError {
    let rendered = e.render().to_string();
    let message = rendered
        .lines()
        .take_while(|l| !l.starts_with("Usage:") && !l.starts_with("For more information"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let message = message.trim_start_matches("error: ").to_owned();
    CliError::usage(message, "Run the command with --help to see its arguments and examples.")
}

fn main() -> ExitCode {
    let wants_json = std::env::args().any(|a| a == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let informational = matches!(
                e.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            if informational {
                let _ = e.print();
                return ExitCode::from(if e.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand { error::exit::USAGE } else { 0 });
            }
            if wants_json {
                return report(true, &usage_error(&e));
            }
            let _ = e.print();
            return ExitCode::from(error::exit::USAGE);
        }
    };
    let Some(command) = cli.command else {
        let _ = Cli::command().print_long_help();
        return ExitCode::SUCCESS;
    };
    let json = cli.global.json;
    // `accounts` works anywhere: no home, no network, nothing written.
    if matches!(command, Command::Accounts) && crate::session::home().is_err() {
        let value = auth::accounts(cli.global.api_url.clone(), cli.global.accounts_url.clone());
        if json {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        } else {
            print!("{}", auth::accounts_text(&value));
        }
        return ExitCode::SUCCESS;
    }
    let login_status = matches!(command, Command::Login(LoginArgs { command: Some(LoginCmd::Status { .. }), .. }));
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return report(json, &CliError::new("internal", format!("calc couldn't start: {e}."), "Retry the command.")),
    };
    let options = Options { json, quiet: cli.global.quiet, api_url: cli.global.api_url, accounts_url: cli.global.accounts_url };
    runtime.block_on(async move {
        let ctx = match Ctx::new(options) {
            Ok(ctx) => ctx,
            Err(e) => {
                // `login status --json` always answers: with no usable home, nobody is signed in here.
                if login_status && json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({"authenticated": false, "app_id": session::APP_ID, "reason": e.code, "message": e.message}))
                            .unwrap_or_default()
                    );
                    return ExitCode::SUCCESS;
                }
                return report(json, &e);
            }
        };
        match run(&ctx, command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                // Without a sign-in, a 401 means "sign in", not "your token is bad".
                let e = if !ctx.signed_in && e.status == Some(401) {
                    let mut out = ctx.signed_out_error();
                    out.status = Some(401);
                    out
                } else {
                    e
                };
                report(json, &e)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("calc").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_tree_is_sound() {
        Cli::command().debug_assert();
    }

    #[test]
    fn write_takes_negative_numbers_and_stdin() {
        let cli = parse(&["write", "Q3", "B2", "-5", "-", "=A1*2"]).unwrap();
        let Some(Command::Write(w)) = cli.command else { panic!("not write") };
        assert_eq!(w.on.sheet, "Q3");
        assert_eq!(w.address.as_deref(), Some("B2"));
        assert_eq!(w.inputs, vec!["-5", "-", "=A1*2"]);
        let cli = parse(&["write", "Q3", "--at", "A1", "--csv", "-", "--copy", "Try"]).unwrap();
        let Some(Command::Write(w)) = cli.command else { panic!("not write") };
        assert_eq!((w.at.as_deref(), w.csv.as_deref(), w.on.copy.as_deref()), (Some("A1"), Some("-"), Some("Try")));
        let cli = parse(&["write", "Q3", "A1", "--", "-text"]).unwrap();
        let Some(Command::Write(w)) = cli.command else { panic!("not write") };
        assert_eq!(w.inputs, vec!["-text"]);
    }

    #[test]
    fn run_reads_language_sheet_and_file() {
        let cli = parse(&["run", "sql", "Q3 revenue", "-c", "select 1", "--into-new-tab", "Out", "--keep-updated"]).unwrap();
        let Some(Command::Run(r)) = cli.command else { panic!("not run") };
        assert_eq!((r.language.as_str(), r.on.sheet.as_str(), r.code.as_deref()), ("sql", "Q3 revenue", Some("select 1")));
        assert!(r.keep_updated);
        let cli = parse(&["run", "py", "k3j9xq2mpa7d", "analysis.py", "--copy", "c1"]).unwrap();
        let Some(Command::Run(r)) = cli.command else { panic!("not run") };
        assert_eq!((r.file.as_deref(), r.on.copy.as_deref()), (Some("analysis.py"), Some("c1")));
    }

    #[test]
    fn global_flags_go_anywhere() {
        let cli = parse(&["read", "Q3", "A1:B2", "--json", "--api-url", "http://127.0.0.1:8770"]).unwrap();
        assert!(cli.global.json);
        assert_eq!(cli.global.api_url.as_deref(), Some("http://127.0.0.1:8770"));
        let cli = parse(&["--json", "login", "status"]).unwrap();
        assert!(cli.global.json);
        assert!(matches!(cli.command, Some(Command::Login(LoginArgs { command: Some(LoginCmd::Status { offline: false }), .. }))));
    }

    #[test]
    fn usage_mistakes_are_caught() {
        assert!(parse(&["sort", "Q3"]).is_err(), "sort needs --by");
        assert!(parse(&["login", "--slt", "x", "--silicon", "si:a"]).is_err());
        assert!(parse(&["comments", "add", "Q3", "B2"]).is_err(), "a comment needs text");
        assert!(parse(&["clear", "Q3", "A1", "--formats", "--all"]).is_err());
        assert!(parse(&["freeze", "Q3", "--rows", "1", "--off"]).is_err());
        let e = parse(&["nonsense"]).unwrap_err();
        let c = usage_error(&e);
        assert_eq!(c.exit, 2);
        assert!(c.message.contains("nonsense"), "{}", c.message);
    }

    #[test]
    fn comments_take_text_with_dashes() {
        let cli = parse(&["comments", "add", "Q3", "Sales!B2:B9", "@si:analyst", "-", "why?"]).unwrap();
        let Some(Command::Comments(CommentsCmd::Add { place, body, .. })) = cli.command else { panic!("not add") };
        assert_eq!(place, "Sales!B2:B9");
        assert_eq!(body, vec!["@si:analyst", "-", "why?"]);
    }

    #[test]
    fn the_guide_has_parts() {
        assert!(docs_part("").unwrap().len() > 1000);
        let copies = docs_part("copies").unwrap();
        assert!(copies.starts_with("## Copies"), "{}", &copies[..copies.len().min(80)]);
        assert!(docs_part("copy").unwrap().starts_with("## Copies"));
        assert!(docs_part("sign in").unwrap().starts_with("## Sign in"));
        assert_eq!(docs_part("zebras").unwrap_err().exit, 2);
    }

    #[test]
    fn switches() {
        assert!(on_off("ON", "x").unwrap());
        assert!(!on_off("off", "x").unwrap());
        assert_eq!(on_off("maybe", "x").unwrap_err().exit, 2);
    }
}
