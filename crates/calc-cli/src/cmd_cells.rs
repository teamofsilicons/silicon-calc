//! Reading and changing cells: `read`, `cell`, `write`, `style`, `clear`, rows and columns, `sort`, `fill`,
//! `freeze`, sizes, hiding, merging, `undo` and `redo`.

use std::io::{IsTerminal, Read as _, Write as _};

use serde_json::{Value as Json, json};
use silicon_calc_client::a1::{self, CellRef, RangeKind, RangeRef, col_name};
use silicon_calc_client::{
    Border, BorderStyle, CellInput, Cells, ClearWhat, Edit, EditAnswer, EditRequest, HAlign, SortKey, StylePatch, Tab,
    ThreadFilter, VAlign, Value,
};

use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{GridCell, Table, grid, plural, when_ago};
use crate::resolve::{self, Target};

/// An id for this run, so its own changes can be told apart in live events.
pub fn client_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("calc-cli-{}-{nanos:x}", std::process::id())
}

/// Sends edits as one version, based on the version the command looked at, and says what it overwrote.
pub async fn send(ctx: &Ctx, target: &Target, edits: Vec<Edit>, base: Option<i64>) -> CliResult<EditAnswer> {
    let request = EditRequest { base_version: Some(base.unwrap_or(target.version)), edits, client_id: Some(client_id()) };
    let answer = ctx.client.edit(&target.line, &request).await?;
    report(ctx, target, &answer);
    Ok(answer)
}

fn tab_name(target: &Target, id: u32) -> String {
    target.tabs.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string())
}

/// Tells about overwritten and skipped cells on stderr.
pub fn report(ctx: &Ctx, target: &Target, answer: &EditAnswer) {
    if ctx.json {
        return;
    }
    for o in &answer.overwrote {
        let before = match &o.before {
            Value::Empty => "nothing".to_owned(),
            v => format!("\"{v}\""),
        };
        ctx.warn(format!(
            "{} had been changed by {} {} (version {}, it held {before}); your change replaced it, and the earlier value stays in the history.",
            a1::with_tab(&tab_name(target, o.tab), &o.a),
            o.by.label(),
            crate::output::ago(&o.at),
            o.version
        ));
    }
    if answer.skipped > 0 {
        ctx.warn(format!(
            "{} skipped: their rows or columns were deleted meanwhile.",
            plural(answer.skipped, "cell write was", "cell writes were")
        ));
    }
}

/// A range that may name its tab, or a tab's name alone.
pub fn range_or_tab(target: &Target, arg: Option<&str>, tab_flag: Option<&str>) -> CliResult<(Option<String>, Option<String>)> {
    if let Some(a) = arg.map(str::trim).filter(|a| !a.is_empty())
        && tab_flag.is_none()
        && !a.contains('!')
        && RangeRef::parse(a).is_none()
        && resolve::find_tab(&target.tabs, Some(a), "").is_ok()
    {
        return Ok((Some(a.to_owned()), None));
    }
    resolve::split_range(arg, tab_flag)
}

/// The used area of a tab, as a range.
fn used(tab: &Tab) -> Option<RangeRef> {
    tab.used.as_deref().and_then(RangeRef::parse)
}

/// A range cut to the tab's used area (whole columns and rows otherwise reach the limits).
fn bounded(range: RangeRef, tab: &Tab) -> RangeRef {
    let Some(u) = used(tab) else { return RangeRef::cells(range.r1, range.c1, range.r1, range.c1) };
    match range.kind {
        RangeKind::Cells => range,
        RangeKind::Cols => RangeRef::cells(0, range.c1, u.r2, range.c2),
        RangeKind::Rows => RangeRef::cells(range.r1, 0, range.r2, u.c2),
        RangeKind::OpenRows => RangeRef::cells(range.r1, range.c1, u.r2.max(range.r1), range.c2),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadFormat {
    Table,
    Csv,
    Tsv,
    Json,
    Values,
}

impl ReadFormat {
    pub fn parse(s: &str) -> CliResult<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "table" | "text" => Self::Table,
            "csv" => Self::Csv,
            "tsv" => Self::Tsv,
            "json" => Self::Json,
            "values" => Self::Values,
            other => {
                return Err(CliError::usage(format!("\"{other}\" is not a format read prints."), "Use table, csv, tsv, json or values."));
            }
        })
    }
}

pub struct ReadArgs {
    pub sheet: String,
    pub range: Option<String>,
    pub tab: Option<String>,
    pub format: Option<String>,
    pub formulas: bool,
    pub styles: bool,
    pub comments: bool,
    pub version: Option<String>,
    pub copy: Option<String>,
    pub max_rows: Option<u32>,
}

/// How many rows the table shows unless `--max-rows` says otherwise.
const TABLE_ROWS: u32 = 200;
const TABLE_COLS: u32 = 40;

pub async fn read(ctx: &Ctx, a: ReadArgs, at_version_route: bool) -> CliResult<()> {
    let format = match (&a.format, ctx.json) {
        (Some(f), json) => {
            let f = ReadFormat::parse(f)?;
            if json && f == ReadFormat::Table { ReadFormat::Json } else { f }
        }
        (None, true) => ReadFormat::Json,
        (None, false) => ReadFormat::Table,
    };
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let version = match &a.version {
        Some(v) => Some(resolve::version(ctx, &target.line, v, target.version).await?),
        None => None,
    };
    // The tabs as they were, when reading a past version.
    let tabs = match version {
        Some(v) if v != target.version => ctx.client.tabs_at(&target.line, v).await.map(|l| l.tabs).unwrap_or_else(|_| target.tabs.clone()),
        _ => target.tabs.clone(),
    };
    let target = Target { tabs, ..target };
    let (tab_arg, range) = range_or_tab(&target, a.range.as_deref(), a.tab.as_deref())?;
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let tab_ref = silicon_calc_client::TabRef::Id(tab.id);
    let fetch = |range: Option<String>| {
        let target = &target;
        let tab_ref = tab_ref.clone();
        async move {
            match (version, at_version_route) {
                (Some(v), true) => ctx.client.cells_at(&target.line, v, &tab_ref, range.as_deref()).await,
                _ => ctx.client.cells(&target.line, &tab_ref, range.as_deref(), version).await,
            }
        }
    };
    match format {
        ReadFormat::Csv if !a.formulas && !at_version_route => {
            let download = ctx.client.cells_csv(&target.line, &tab_ref, range.as_deref(), version).await?;
            let mut out = tokio::io::stdout();
            download.write_to(&mut out).await?;
            if a.comments {
                comments_to_stderr(ctx, &target, &tab, range.as_deref()).await?;
            }
            Ok(())
        }
        ReadFormat::Values => {
            let values = if at_version_route {
                let cells = fetch(range.clone()).await?;
                let (r, rows) = dense(&cells, false, None);
                json!({"version": cells.version, "range": r.map(|r| r.to_string()), "values": rows.into_iter().map(|row| row.into_iter().map(|c| c.1).collect::<Vec<_>>()).collect::<Vec<_>>()})
            } else {
                serde_json::to_value(ctx.client.cell_values(&target.line, &tab_ref, range.as_deref(), version).await?).unwrap_or_default()
            };
            println!("{}", serde_json::to_string_pretty(&values).unwrap_or_default());
            Ok(())
        }
        ReadFormat::Json => {
            let cells = fetch(range.clone()).await?;
            let mut value = serde_json::to_value(&cells).unwrap_or_default();
            if a.comments {
                let threads = threads_in(ctx, &target, &tab, cells.range.as_deref()).await?;
                value["threads"] = serde_json::to_value(threads).unwrap_or_default();
            }
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            Ok(())
        }
        ReadFormat::Csv | ReadFormat::Tsv => {
            let cells = fetch(range.clone()).await?;
            let (_, rows) = dense(&cells, a.formulas, None);
            let mut w = csv::WriterBuilder::new()
                .delimiter(if format == ReadFormat::Tsv { b'\t' } else { b',' })
                .from_writer(std::io::stdout().lock());
            for row in rows {
                w.write_record(row.iter().map(|c| c.0.as_str())).map_err(|e| CliError::file(format!("Can't write the output: {e}.")))?;
            }
            w.flush().map_err(|e| CliError::file(format!("Can't write the output: {e}.")))?;
            if a.comments {
                comments_to_stderr(ctx, &target, &tab, range.as_deref()).await?;
            }
            Ok(())
        }
        ReadFormat::Table => {
            let max_rows = a.max_rows.unwrap_or(TABLE_ROWS).max(1);
            let Some(area) = range.as_deref().and_then(RangeRef::parse).map(|r| bounded(r, &tab)).or_else(|| used(&tab)) else {
                println!("{} in {} is empty.", tab.name, target.place());
                return Ok(());
            };
            let shown = RangeRef::cells(
                area.r1,
                area.c1,
                area.r2.min(area.r1 + max_rows - 1),
                area.c2.min(area.c1 + TABLE_COLS - 1),
            );
            let cells = fetch(Some(shown.to_string())).await?;
            let gc: Vec<GridCell> = cells
                .cells
                .iter()
                .map(|c| GridCell {
                    row: c.r,
                    col: c.c,
                    text: if a.formulas { c.f.clone().unwrap_or_else(|| c.t.clone()) } else { c.t.clone() },
                    right: matches!(c.v, Value::Number(_)) && !(a.formulas && c.f.is_some()),
                })
                .collect();
            let mut out = format!(
                "{} · {} · {}{}\n",
                target.place(),
                a1::with_tab(&tab.name, &shown.to_string()),
                if version.is_some() { "version " } else { "version " },
                cells.version
            );
            out.push_str(&grid(&gc, shown.r1, shown.c1, shown.r2, shown.c2, 40));
            if a.styles {
                let styled: Vec<String> = cells
                    .cells
                    .iter()
                    .filter_map(|c| cells.style_of(c).filter(|s| !s.is_default()).map(|s| format!("  {:<8} {}", c.a, s.describe())))
                    .collect();
                if !styled.is_empty() {
                    out.push_str("\nStyles\n");
                    out.push_str(&styled.join("\n"));
                    out.push('\n');
                }
                if !cells.merges.is_empty() {
                    out.push_str(&format!("Merged: {}\n", cells.merges.join(", ")));
                }
            }
            if a.comments {
                let threads = threads_in(ctx, &target, &tab, Some(&shown.to_string())).await?;
                if !threads.is_empty() {
                    out.push_str("\nComments\n");
                    out.push_str(&crate::cmd_comments::render_threads(&threads, true));
                }
            }
            print!("{out}");
            let _ = std::io::stdout().flush();
            if shown.r2 < area.r2 || shown.c2 < area.c2 {
                ctx.note(format!(
                    "Showing {} of {}. For everything: calc read {} {} --format csv (or --max-rows N).",
                    shown,
                    area,
                    target.sheet.id,
                    a1::with_tab(&tab.name, &area.to_string())
                ));
            }
            Ok(())
        }
    }
}

/// A dense block from sparse cells: (shown text or formula, plain value) per cell.
fn dense(cells: &Cells, formulas: bool, area: Option<RangeRef>) -> (Option<RangeRef>, Vec<Vec<(String, Json)>>) {
    let area = area.or_else(|| cells.range.as_deref().and_then(RangeRef::parse)).map(|r| match r.kind {
        RangeKind::Cells => r,
        _ => {
            // Whole rows or columns: as far as the cells reach.
            let r2 = cells.cells.iter().map(|c| c.r).max().unwrap_or(r.r1).min(r.r2);
            let c2 = cells.cells.iter().map(|c| c.c).max().unwrap_or(r.c1).min(r.c2);
            RangeRef::cells(r.r1, r.c1, r2.max(r.r1), c2.max(r.c1))
        }
    });
    let Some(area) = area else { return (None, Vec::new()) };
    let mut rows = vec![vec![(String::new(), Json::Null); area.cols() as usize]; area.rows() as usize];
    for c in &cells.cells {
        if !area.contains(CellRef::new(c.r, c.c)) {
            continue;
        }
        let text = if formulas { c.f.clone().unwrap_or_else(|| c.t.clone()) } else { c.t.clone() };
        rows[(c.r - area.r1) as usize][(c.c - area.c1) as usize] = (text, c.v.to_json());
    }
    (Some(area), rows)
}

async fn threads_in(ctx: &Ctx, target: &Target, tab: &Tab, range: Option<&str>) -> CliResult<Vec<silicon_calc_client::Thread>> {
    let threads = ctx.client.threads(&target.line, ThreadFilter::Open, Some(&silicon_calc_client::TabRef::Id(tab.id))).await?;
    let area = range.and_then(RangeRef::parse);
    Ok(threads
        .into_iter()
        .filter(|t| t.tab == tab.id)
        .filter(|t| match (&area, t.range.as_deref().and_then(RangeRef::parse)) {
            (Some(a), Some(r)) => a.intersects(&r),
            _ => true,
        })
        .collect())
}

async fn comments_to_stderr(ctx: &Ctx, target: &Target, tab: &Tab, range: Option<&str>) -> CliResult<()> {
    let threads = threads_in(ctx, target, tab, range).await?;
    if !threads.is_empty() {
        eprintln!("Comments\n{}", crate::cmd_comments::render_threads(&threads, true));
    }
    Ok(())
}

// ── One cell ────────────────────────────────────────────────────────────────────────────────────────────

pub async fn cell(ctx: &Ctx, sheet: &str, address: &str, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, range) = resolve::split_range(Some(address), tab)?;
    let Some(a) = range.as_deref().and_then(RangeRef::parse).filter(RangeRef::is_cell) else {
        return Err(CliError::usage(format!("\"{address}\" is not one cell."), "Name one cell, like B7 or Sales!B7."));
    };
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let tab_ref = silicon_calc_client::TabRef::Id(tab.id);
    let a1_text = a.to_string();
    let detail = ctx.client.cell(&target.line, &tab_ref, &a1_text).await?;
    let threads: Vec<silicon_calc_client::Thread> = match ctx.client.threads(&target.line, ThreadFilter::All, Some(&tab_ref)).await {
        Ok(ts) => ts
            .into_iter()
            .filter(|t| t.tab == tab.id && t.range.as_deref().and_then(RangeRef::parse).is_none_or(|r| r.contains(a.start())))
            .collect(),
        Err(_) => Vec::new(),
    };
    let mut value = serde_json::to_value(&detail).unwrap_or_default();
    value["comments"] = serde_json::to_value(&threads).unwrap_or_default();
    ctx.emit(&value, || {
        let mut out = format!("{} in {}\n", a1::with_tab(&tab.name, &a1_text), target.place());
        let mut row = |k: &str, v: String| out.push_str(&format!("{k:<9}{v}\n"));
        row("value", if detail.v.is_empty() { "(empty)".into() } else { format!("{} ({})", detail.v, detail.v.kind()) });
        if !detail.t.is_empty() && detail.t != detail.v.to_string() {
            row("shows", detail.t.clone());
        }
        if let Some(f) = &detail.f {
            row("formula", f.clone());
        }
        if detail.spill {
            row("spill", "written by a formula spilling from another cell".into());
        }
        if let Some(s) = detail.style_resolved().filter(|s| !s.is_default()) {
            row("style", s.describe());
        }
        match (detail.author(), &detail.by, &detail.at) {
            (Some(acc), _, Some(at)) => row("changed", format!("by {} ({}), {}", acc.label(), acc.display_name, when_ago(at))),
            (None, Some(by), Some(at)) => row("changed", format!("by {}, {}", by.label(&detail.accounts), when_ago(at))),
            _ => {}
        }
        if !detail.history.is_empty() {
            out.push_str("\nHistory (newest first)\n");
            let mut t = Table::new(&["version", "who", "when", "before", "after"]).right(0);
            for h in &detail.history {
                t.row(vec![h.version.to_string(), h.by.label(&detail.accounts), crate::output::when(&h.at), h.before.to_string(), h.after.to_string()]);
            }
            out.push_str(&t.render());
        }
        if !threads.is_empty() {
            out.push_str("\nComments\n");
            out.push_str(&crate::cmd_comments::render_threads(&threads, true));
        }
        out
    });
    Ok(())
}

// ── Writing ─────────────────────────────────────────────────────────────────────────────────────────────

pub struct WriteArgs {
    pub sheet: String,
    pub address: Option<String>,
    pub inputs: Vec<String>,
    pub at: Option<String>,
    pub tab: Option<String>,
    pub csv: Option<String>,
    pub tsv: Option<String>,
    pub values: Option<String>,
    pub raw: bool,
    pub down: bool,
    pub base_version: Option<i64>,
    pub copy: Option<String>,
}

fn read_source(path: &str, what: &str) -> CliResult<String> {
    if path == "-" {
        if std::io::stdin().is_terminal() {
            return Err(CliError::usage(format!("Reading {what} from stdin, but stdin is a terminal."), format!("Pipe {what} in, or give a file.")));
        }
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| CliError::file(format!("Can't read stdin: {e}.")))?;
        return Ok(s);
    }
    std::fs::read_to_string(path).map_err(|e| CliError::file(format!("Can't read {path}: {e}.")))
}

/// Rows of a CSV or TSV text.
pub fn parse_delimited(text: &str, delimiter: u8) -> CliResult<Vec<Vec<String>>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record.map_err(|e| CliError::usage(format!("Line {} can't be read: {e}.", i + 1), "Check the quoting of that line."))?;
        rows.push(record.iter().map(str::to_owned).collect());
    }
    Ok(rows)
}

/// Rows of raw values from a JSON array of arrays.
pub fn parse_values(text: &str) -> CliResult<Vec<Vec<Json>>> {
    let v: Json = serde_json::from_str(text)
        .map_err(|e| CliError::usage(format!("The values aren't JSON: {e}."), "Give an array of rows, like [[\"name\", 12], [\"ada\", true]]."))?;
    let Json::Array(rows) = v else {
        return Err(CliError::usage("The values must be an array of rows.", "Give an array of rows, like [[\"name\", 12], [\"ada\", true]]."));
    };
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| match row {
            Json::Array(cells) => Ok(cells),
            _ => Err(CliError::usage(format!("Row {} of the values is not an array.", i + 1), "Every row is an array of values.")),
        })
        .collect()
}

/// The block `rows` would fill from `at`.
fn block(at: CellRef, rows: usize, cols: usize) -> RangeRef {
    RangeRef::cells(at.row, at.col, at.row + rows.saturating_sub(1) as u32, at.col + cols.saturating_sub(1) as u32)
}

pub async fn write(ctx: &Ctx, w: WriteArgs) -> CliResult<()> {
    let sources = [w.csv.is_some(), w.tsv.is_some(), w.values.is_some()].iter().filter(|b| **b).count();
    if sources > 1 {
        return Err(CliError::usage("--csv, --tsv and --values each give the data; pass only one.", "Pick the one your data is in."));
    }
    let anchor_arg = match (&w.at, &w.address) {
        (Some(a), Some(b)) if a != b => {
            return Err(CliError::usage(format!("Two places were given: {b} and --at {a}."), "Give the cell once."));
        }
        (Some(a), _) | (None, Some(a)) => a.clone(),
        (None, None) => {
            if sources == 1 {
                "A1".to_owned()
            } else {
                return Err(CliError::usage("Where to write is missing.", "Write calc write SHEET B2 value, or calc write SHEET --at A1 --csv data.csv."));
            }
        }
    };
    if sources == 1 && !w.inputs.is_empty() {
        return Err(CliError::usage("Inputs were given along with a file.", "Either type the inputs, or give --csv, --tsv or --values."));
    }
    let target = resolve::target(ctx, &w.sheet, w.copy.as_deref()).await?;
    let (tab_arg, range) = resolve::split_range(Some(&anchor_arg), w.tab.as_deref())?;
    let Some(anchor) = range.as_deref().and_then(RangeRef::parse).filter(|r| r.kind == RangeKind::Cells) else {
        return Err(CliError::usage(format!("\"{anchor_arg}\" is not a cell to write at."), "Name the top-left cell, like A1 or Sales!B2."));
    };
    let at = anchor.start();
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let tab_ref = silicon_calc_client::TabRef::Id(tab.id);

    enum Data {
        Inputs(Vec<Vec<String>>),
        Values(Vec<Vec<Json>>),
    }
    let data = if let Some(given) = w.values.as_deref() {
        // The JSON itself (it starts with `[`), a file holding it, or - for stdin.
        let text = if given.trim_start().starts_with('[') { given.to_owned() } else { read_source(given, "the values")? };
        Data::Values(parse_values(&text)?)
    } else if let Some((path, delim)) = w.csv.as_deref().map(|p| (p, b',')).or(w.tsv.as_deref().map(|p| (p, b'\t'))) {
        let rows = parse_delimited(&read_source(path, "the rows")?, delim)?;
        if w.raw {
            Data::Values(rows.into_iter().map(|r| r.into_iter().map(Json::String).collect()).collect())
        } else {
            Data::Inputs(rows)
        }
    } else {
        if w.inputs.is_empty() {
            return Err(CliError::usage(
                format!("Nothing to write at {anchor_arg}."),
                "Give the inputs after the cell (calc write SHEET B2 =A2*2), - to read one from stdin, or --csv/--tsv/--values.",
            ));
        }
        let mut inputs = Vec::with_capacity(w.inputs.len());
        for i in &w.inputs {
            if i == "-" {
                inputs.push(read_source("-", "the input")?.trim_end_matches(['\n', '\r']).to_owned());
            } else {
                inputs.push(i.clone());
            }
        }
        if w.raw {
            let cells: Vec<Json> = inputs.into_iter().map(Json::String).collect();
            Data::Values(if w.down { cells.into_iter().map(|c| vec![c]).collect() } else { vec![cells] })
        } else if w.down {
            Data::Inputs(inputs.into_iter().map(|i| vec![i]).collect())
        } else {
            Data::Inputs(vec![inputs])
        }
    };
    let (rows, cols) = match &data {
        Data::Inputs(r) => (r.len(), r.iter().map(Vec::len).max().unwrap_or(0)),
        Data::Values(r) => (r.len(), r.iter().map(Vec::len).max().unwrap_or(0)),
    };
    if rows == 0 || cols == 0 {
        return Err(CliError::usage("The data has no cells.", "Give at least one row with one value."));
    }
    let area = block(at, rows, cols);
    // Very large blocks go in pieces that fit one edit (each piece is its own version).
    let total = (rows * cols) as u64;
    let limit = if total > 100_000 { ctx.client.meta().await.map(|m| m.limits.max_cells_per_edit).unwrap_or(200_000).max(1) } else { 200_000 };
    let rows_per_piece = ((limit as usize) / cols.max(1)).max(1);
    let mut answers: Vec<EditAnswer> = Vec::new();
    let mut base = w.base_version;
    let mut start = 0usize;
    while start < rows {
        let end = (start + rows_per_piece).min(rows);
        let piece_at = CellRef::new(at.row + start as u32, at.col).to_string();
        let edit = match &data {
            Data::Inputs(r) if rows == 1 && cols == 1 => {
                Edit::set_cells(tab_ref.clone(), vec![CellInput { a: piece_at, input: r[0][0].clone() }])
            }
            Data::Inputs(r) => Edit::set_block(tab_ref.clone(), piece_at, r[start..end].to_vec()),
            Data::Values(r) => Edit::Values { tab: tab_ref.clone(), at: piece_at, rows: r[start..end].to_vec() },
        };
        let answer = send(ctx, &target, vec![edit], base).await?;
        base = Some(answer.version);
        answers.push(answer);
        start = end;
    }
    let merged = if answers.len() == 1 {
        answers.pop().unwrap_or_default()
    } else {
        let mut m = EditAnswer::default();
        for a in answers.iter() {
            m.version = a.version;
            m.changes.extend(a.changes.clone());
            m.overwrote.extend(a.overwrote.clone());
            m.skipped += a.skipped;
        }
        m
    };
    let pieces = answers.len().max(1);
    ctx.emit(&merged, || {
        let mut s = format!("Wrote {} to {} in {} (version {}).", plural(total, "cell", "cells"), a1::with_tab(&tab.name, &area.to_string()), target.place(), merged.version);
        if pieces > 1 {
            s.push_str(&format!(" It went in {pieces} pieces, each its own version."));
        }
        s
    });
    Ok(())
}

// ── Look ────────────────────────────────────────────────────────────────────────────────────────────────

/// `#RRGGBB` from `#rgb`, `rrggbb`, `#RRGGBB`; `None` for none/default.
pub fn parse_color(s: &str) -> CliResult<Option<String>> {
    let t = s.trim();
    if matches!(t.to_ascii_lowercase().as_str(), "none" | "default" | "clear" | "") {
        return Ok(None);
    }
    let hex = t.trim_start_matches('#');
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => hex.to_owned(),
        _ => String::new(),
    };
    if full.len() == 6 && full.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(Some(format!("#{}", full.to_ascii_uppercase())))
    } else {
        Err(CliError::usage(format!("\"{s}\" is not a colour."), "Write colours as hex, like #1F5FB8 (or none)."))
    }
}

pub struct StyleArgs {
    pub sheet: String,
    pub range: String,
    pub tab: Option<String>,
    pub copy: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub wrap: Option<bool>,
    pub color: Option<String>,
    pub fill: Option<String>,
    pub align: Option<String>,
    pub valign: Option<String>,
    pub format: Option<String>,
    pub font: Option<String>,
    pub size: Option<String>,
    pub border: Option<String>,
    pub border_color: Option<String>,
    pub clear: bool,
}

pub fn style_patch(s: &StyleArgs) -> CliResult<StylePatch> {
    let mut p = StylePatch { bold: s.bold, italic: s.italic, underline: s.underline, strike: s.strike, wrap: s.wrap, clear: s.clear, ..Default::default() };
    if let Some(c) = &s.color {
        p.color = Some(parse_color(c)?);
    }
    if let Some(c) = &s.fill {
        p.fill = Some(parse_color(c)?);
    }
    if let Some(a) = &s.align {
        p.h_align = Some(match a.to_ascii_lowercase().as_str() {
            "left" => HAlign::Left,
            "center" | "centre" | "middle" => HAlign::Center,
            "right" => HAlign::Right,
            "general" | "auto" | "default" => HAlign::General,
            _ => return Err(CliError::usage(format!("\"{a}\" is not an alignment."), "Use left, center, right or general.")),
        });
    }
    if let Some(a) = &s.valign {
        p.v_align = Some(match a.to_ascii_lowercase().as_str() {
            "top" => VAlign::Top,
            "middle" | "center" | "centre" => VAlign::Middle,
            "bottom" | "default" => VAlign::Bottom,
            _ => return Err(CliError::usage(format!("\"{a}\" is not a vertical alignment."), "Use top, middle or bottom.")),
        });
    }
    if let Some(f) = &s.format {
        let f = f.trim();
        p.num_fmt = Some(if f.is_empty() || f.eq_ignore_ascii_case("general") || f.eq_ignore_ascii_case("none") { None } else { Some(f.to_owned()) });
    }
    if let Some(f) = &s.font {
        p.font = Some(if f.eq_ignore_ascii_case("default") || f.trim().is_empty() { None } else { Some(f.trim().to_owned()) });
    }
    if let Some(size) = &s.size {
        p.font_size = Some(if size.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(size.trim().parse::<u16>().ok().filter(|n| (1..=409).contains(n)).ok_or_else(|| {
                CliError::usage(format!("\"{size}\" is not a font size."), "Give points from 1 to 409, or default.")
            })?)
        });
    }
    if let Some(b) = &s.border {
        let style = match b.to_ascii_lowercase().as_str() {
            "none" => BorderStyle::None,
            "thin" => BorderStyle::Thin,
            "medium" => BorderStyle::Medium,
            "thick" => BorderStyle::Thick,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            "double" => BorderStyle::Double,
            _ => return Err(CliError::usage(format!("\"{b}\" is not a border."), "Use thin, medium, thick, dashed, dotted, double or none.")),
        };
        let color = match &s.border_color {
            Some(c) => parse_color(c)?,
            None => None,
        };
        let border = (style != BorderStyle::None).then(|| Border { style, color });
        p.border_top = Some(border.clone());
        p.border_bottom = Some(border.clone());
        p.border_left = Some(border.clone());
        p.border_right = Some(border);
    } else if s.border_color.is_some() {
        return Err(CliError::usage("--border-color needs --border.", "Add --border thin (or another line)."));
    }
    if p.is_empty() {
        return Err(CliError::usage(
            "No change to the look was given.",
            "Add what to change, like --bold, --fill #FFF2CC, --format 0.00 or --align center (see calc style --help).",
        ));
    }
    Ok(p)
}

pub async fn style(ctx: &Ctx, s: StyleArgs) -> CliResult<()> {
    let patch = style_patch(&s)?;
    let target = resolve::target(ctx, &s.sheet, s.copy.as_deref()).await?;
    let (tab_arg, range) = resolve::split_range(Some(&s.range), s.tab.as_deref())?;
    let range = range.ok_or_else(|| CliError::usage("Which cells?", "Name a range, like A1:C1."))?;
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let answer = send(ctx, &target, vec![Edit::Style { tab: tab.id.into(), range: range.clone(), patch: patch.clone() }], None).await?;
    ctx.emit(&answer, || format!("Styled {} (version {}).", a1::with_tab(&tab.name, &range), answer.version));
    Ok(())
}

pub async fn clear(ctx: &Ctx, sheet: &str, range: &str, tab: Option<&str>, what: ClearWhat, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, r) = resolve::split_range(Some(range), tab)?;
    let r = r.ok_or_else(|| CliError::usage("Which cells?", "Name a range, like A1:C9."))?;
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let answer = send(ctx, &target, vec![Edit::Clear { tab: tab.id.into(), range: r.clone(), what }], None).await?;
    let words = match what {
        ClearWhat::Contents => "Cleared the contents of",
        ClearWhat::Formats => "Cleared the look of",
        ClearWhat::All => "Cleared",
    };
    ctx.emit(&answer, || format!("{words} {} (version {}).", a1::with_tab(&tab.name, &r), answer.version));
    Ok(())
}

// ── Rows and columns ────────────────────────────────────────────────────────────────────────────────────

/// Rows written `5` or `5:7` (1-based): zero-based first row and count.
pub fn parse_rows(s: &str) -> CliResult<(u32, u32)> {
    match RangeRef::parse(s) {
        Some(r) if r.kind == RangeKind::Rows => Ok((r.r1, r.rows())),
        _ => Err(CliError::usage(format!("\"{s}\" is not rows."), "Write rows by number, like 5 or 5:7.")),
    }
}

/// Columns written `C` or `C:E`: zero-based first column and count.
pub fn parse_cols(s: &str) -> CliResult<(u32, u32)> {
    match RangeRef::parse(s) {
        Some(r) if r.kind == RangeKind::Cols => Ok((r.c1, r.cols())),
        _ => Err(CliError::usage(format!("\"{s}\" is not columns."), "Write columns by letter, like C or C:E.")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Rows,
    Cols,
}

fn axis_words(axis: Axis, n: u32) -> String {
    match axis {
        Axis::Rows => plural(u64::from(n), "row", "rows"),
        Axis::Cols => plural(u64::from(n), "column", "columns"),
    }
}

fn axis_place(axis: Axis, first: u32, count: u32) -> String {
    match (axis, count) {
        (Axis::Rows, 1) => format!("row {}", first + 1),
        (Axis::Rows, _) => format!("rows {}:{}", first + 1, first + count),
        (Axis::Cols, 1) => format!("column {}", col_name(first)),
        (Axis::Cols, _) => format!("columns {}:{}", col_name(first), col_name(first + count - 1)),
    }
}

pub async fn insert(ctx: &Ctx, sheet: &str, axis: Axis, at: &str, count: u32, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    if count == 0 {
        return Err(CliError::usage("--count 0 inserts nothing.", "Give --count 1 or more."));
    }
    let (tab_arg, rest) = a1::split_tab(at);
    let tab_flag = tab.map(str::to_owned).or(tab_arg);
    let first = match axis {
        Axis::Rows => parse_rows(rest)?.0,
        Axis::Cols => parse_cols(rest)?.0,
    };
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab_flag.as_deref())?.clone();
    let edit = match axis {
        Axis::Rows => Edit::InsertRows { tab: t.id.into(), at: first, count },
        Axis::Cols => Edit::InsertCols { tab: t.id.into(), at: first, count },
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || {
        format!("Inserted {} before {} in {} (version {}).", axis_words(axis, count), axis_place(axis, first, 1), t.name, answer.version)
    });
    Ok(())
}

pub async fn delete(ctx: &Ctx, sheet: &str, axis: Axis, what: &str, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let (tab_arg, rest) = a1::split_tab(what);
    let tab_flag = tab.map(str::to_owned).or(tab_arg);
    let (first, count) = match axis {
        Axis::Rows => parse_rows(rest)?,
        Axis::Cols => parse_cols(rest)?,
    };
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab_flag.as_deref())?.clone();
    let edit = match axis {
        Axis::Rows => Edit::DeleteRows { tab: t.id.into(), at: first, count },
        Axis::Cols => Edit::DeleteCols { tab: t.id.into(), at: first, count },
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || format!("Deleted {} of {} (version {}).", axis_place(axis, first, count), t.name, answer.version));
    Ok(())
}

pub async fn move_axis(ctx: &Ctx, sheet: &str, axis: Axis, what: &str, to: &str, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let (tab_arg, rest) = a1::split_tab(what);
    let tab_flag = tab.map(str::to_owned).or(tab_arg);
    let (first, count) = match axis {
        Axis::Rows => parse_rows(rest)?,
        Axis::Cols => parse_cols(rest)?,
    };
    let dest = match axis {
        Axis::Rows => parse_rows(to)?.0,
        Axis::Cols => parse_cols(to)?.0,
    };
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab_flag.as_deref())?.clone();
    let edit = match axis {
        Axis::Rows => Edit::MoveRows { tab: t.id.into(), from: first, count, to: dest },
        Axis::Cols => Edit::MoveCols { tab: t.id.into(), from: first, count, to: dest },
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || {
        format!("Moved {} of {} to {} (version {}).", axis_place(axis, first, count), t.name, axis_place(axis, dest, 1), answer.version)
    });
    Ok(())
}

/// `B`, `B:desc`, `B:asc`.
pub fn parse_sort_key(s: &str) -> CliResult<SortKey> {
    let (col, dir) = match s.split_once(':') {
        Some((c, d)) => (c.trim(), d.trim().to_ascii_lowercase()),
        None => (s.trim(), "asc".to_owned()),
    };
    if a1::col_index(col).is_none() {
        return Err(CliError::usage(format!("\"{col}\" is not a column."), "Sort keys are columns with an optional direction: B, B:asc or B:desc."));
    }
    let asc = match dir.as_str() {
        "asc" | "a" | "up" | "a-z" => true,
        "desc" | "d" | "down" | "z-a" => false,
        other => return Err(CliError::usage(format!("\"{other}\" is not a direction."), "Use asc or desc, like B:desc.")),
    };
    Ok(SortKey { col: col.to_ascii_uppercase(), asc })
}

pub async fn sort(ctx: &Ctx, sheet: &str, range: Option<&str>, by: &[String], header: bool, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    if by.is_empty() {
        return Err(CliError::usage("Sort by which column?", "Add --by B (or --by B:desc; repeat it for more keys)."));
    }
    let keys = by.iter().map(|b| parse_sort_key(b)).collect::<CliResult<Vec<_>>>()?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, r) = range_or_tab(&target, range, tab)?;
    let t = target.tab(tab_arg.as_deref())?.clone();
    let r = match r {
        Some(r) => r,
        None => t.used.clone().ok_or_else(|| CliError::new("empty", format!("{} is empty; there is nothing to sort.", t.name), ""))?,
    };
    let answer = send(ctx, &target, vec![Edit::Sort { tab: t.id.into(), range: r.clone(), by: keys.clone(), header }], None).await?;
    let keys_text: Vec<String> = keys.iter().map(|k| format!("{} {}", k.col, if k.asc { "ascending" } else { "descending" })).collect();
    ctx.emit(&answer, || {
        format!(
            "Sorted {} by {}{} (version {}).",
            a1::with_tab(&t.name, &r),
            keys_text.join(", then "),
            if header { ", keeping the header row" } else { "" },
            answer.version
        )
    });
    Ok(())
}

pub async fn fill(ctx: &Ctx, sheet: &str, from: &str, to: &str, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_a, f) = resolve::split_range(Some(from), tab)?;
    let (tab_b, t2) = resolve::split_range(Some(to), tab_a.as_deref().or(tab))?;
    let (Some(f), Some(t2)) = (f, t2) else {
        return Err(CliError::usage("fill takes a source and a range to fill.", "For example: calc fill SHEET C2 C3:C100."));
    };
    let t = target.tab(tab_b.as_deref().or(tab_a.as_deref()))?.clone();
    let answer = send(ctx, &target, vec![Edit::Fill { tab: t.id.into(), from: f.clone(), to: t2.clone() }], None).await?;
    ctx.emit(&answer, || format!("Filled {} from {} (version {}).", a1::with_tab(&t.name, &t2), f, answer.version));
    Ok(())
}

pub async fn freeze(ctx: &Ctx, sheet: &str, rows: Option<u32>, cols: Option<u32>, off: bool, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let (rows, cols) = if off { (Some(0), Some(0)) } else { (rows, cols) };
    if rows.is_none() && cols.is_none() {
        return Err(CliError::usage("Freeze what?", "Add --rows N and/or --cols N (or --off to unfreeze)."));
    }
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab)?.clone();
    let answer = send(ctx, &target, vec![Edit::TabProps { tab: t.id.into(), frozen_rows: rows, frozen_cols: cols, color: None }], None).await?;
    ctx.emit(&answer, || {
        let mut parts = Vec::new();
        if let Some(r) = rows {
            parts.push(if r == 0 { "no rows".to_owned() } else { format!("the top {}", plural(u64::from(r), "row", "rows")) });
        }
        if let Some(c) = cols {
            parts.push(if c == 0 { "no columns".to_owned() } else { format!("the first {}", plural(u64::from(c), "column", "columns")) });
        }
        format!("{} now freezes {} (version {}).", t.name, parts.join(" and "), answer.version)
    });
    Ok(())
}

/// A size in pixels, or `auto`/`default` for the default.
pub fn parse_size(s: &str) -> CliResult<Option<f64>> {
    let t = s.trim().trim_end_matches("px");
    if matches!(t.to_ascii_lowercase().as_str(), "auto" | "default" | "reset") {
        return Ok(None);
    }
    match t.parse::<f64>() {
        Ok(n) if n > 0.0 && n <= 4000.0 => Ok(Some(n)),
        _ => Err(CliError::usage(format!("\"{s}\" is not a size."), "Give pixels from 1 to 4000, like 140, or auto for the default.")),
    }
}

pub async fn size(ctx: &Ctx, sheet: &str, axis: Axis, what: &str, px: &str, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let size = parse_size(px)?;
    let (tab_arg, rest) = a1::split_tab(what);
    let tab_flag = tab.map(str::to_owned).or(tab_arg);
    let (first, count) = match axis {
        Axis::Rows => parse_rows(rest)?,
        Axis::Cols => parse_cols(rest)?,
    };
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab_flag.as_deref())?.clone();
    let spec = match axis {
        Axis::Rows => format!("{}:{}", first + 1, first + count),
        Axis::Cols => format!("{}:{}", col_name(first), col_name(first + count - 1)),
    };
    let edit = match axis {
        Axis::Rows => Edit::RowHeight { tab: t.id.into(), rows: spec, height: size },
        Axis::Cols => Edit::ColWidth { tab: t.id.into(), cols: spec, width: size },
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || {
        let how = size.map(|s| format!("{s} pixels")).unwrap_or_else(|| "the default size".into());
        format!("Set {} of {} to {how} (version {}).", axis_place(axis, first, count), t.name, answer.version)
    });
    Ok(())
}

pub async fn hide(ctx: &Ctx, sheet: &str, what: &str, show: bool, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let (tab_arg, rest) = a1::split_tab(what);
    let tab_flag = tab.map(str::to_owned).or(tab_arg);
    let r = RangeRef::parse(rest).filter(|r| matches!(r.kind, RangeKind::Rows | RangeKind::Cols)).ok_or_else(|| {
        CliError::usage(format!("\"{rest}\" is neither rows nor columns."), "Name rows like 3:5 or columns like C:D.")
    })?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(tab_flag.as_deref())?.clone();
    let range = r.to_string();
    let edit = match (r.kind, show) {
        (RangeKind::Rows, false) => Edit::HideRows { tab: t.id.into(), range: range.clone() },
        (RangeKind::Rows, true) => Edit::ShowRows { tab: t.id.into(), range: range.clone() },
        (_, false) => Edit::HideCols { tab: t.id.into(), range: range.clone() },
        (_, true) => Edit::ShowCols { tab: t.id.into(), range: range.clone() },
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    let (axis, first, count) = if r.kind == RangeKind::Rows { (Axis::Rows, r.r1, r.rows()) } else { (Axis::Cols, r.c1, r.cols()) };
    ctx.emit(&answer, || {
        format!("{} {} of {} (version {}).", if show { "Showed" } else { "Hid" }, axis_place(axis, first, count), t.name, answer.version)
    });
    Ok(())
}

pub async fn merge(ctx: &Ctx, sheet: &str, range: &str, unmerge: bool, tab: Option<&str>, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, r) = resolve::split_range(Some(range), tab)?;
    let r = r.ok_or_else(|| CliError::usage("Which cells?", "Name a range, like A1:C1."))?;
    let t = target.tab(tab_arg.as_deref())?.clone();
    let edit = if unmerge { Edit::Unmerge { tab: t.id.into(), range: r.clone() } } else { Edit::Merge { tab: t.id.into(), range: r.clone() } };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || format!("{} {} (version {}).", if unmerge { "Unmerged" } else { "Merged" }, a1::with_tab(&t.name, &r), answer.version));
    Ok(())
}

pub async fn undo(ctx: &Ctx, sheet: &str, redo: bool, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let answer = send(ctx, &target, vec![if redo { Edit::Redo } else { Edit::Undo }], None).await?;
    ctx.emit(&answer, || {
        if answer.version == target.version {
            format!("Nothing to {} on {}.", if redo { "redo" } else { "undo" }, target.place())
        } else {
            format!("{} your last change on {} (version {}).", if redo { "Redid" } else { "Undid" }, target.place(), answer.version)
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_columns() {
        assert_eq!(parse_rows("5").unwrap(), (4, 1));
        assert_eq!(parse_rows("7:5").unwrap(), (4, 3));
        assert!(parse_rows("C").is_err());
        assert_eq!(parse_cols("C").unwrap(), (2, 1));
        assert_eq!(parse_cols("C:E").unwrap(), (2, 3));
        assert!(parse_cols("5").is_err());
        assert_eq!(axis_place(Axis::Rows, 4, 3), "rows 5:7");
        assert_eq!(axis_place(Axis::Cols, 2, 1), "column C");
    }

    #[test]
    fn sort_keys() {
        assert_eq!(parse_sort_key("b").unwrap(), SortKey { col: "B".into(), asc: true });
        assert_eq!(parse_sort_key("AA:desc").unwrap(), SortKey { col: "AA".into(), asc: false });
        assert!(parse_sort_key("B:sideways").is_err());
        assert!(parse_sort_key("12").is_err());
    }

    #[test]
    fn colours_and_sizes() {
        assert_eq!(parse_color("#1f5fb8").unwrap().as_deref(), Some("#1F5FB8"));
        assert_eq!(parse_color("f00").unwrap().as_deref(), Some("#FF0000"));
        assert_eq!(parse_color("none").unwrap(), None);
        assert!(parse_color("blue-ish").is_err());
        assert_eq!(parse_size("140").unwrap(), Some(140.0));
        assert_eq!(parse_size("140px").unwrap(), Some(140.0));
        assert_eq!(parse_size("auto").unwrap(), None);
        assert!(parse_size("-3").is_err());
    }

    #[test]
    fn delimited_and_values() {
        let rows = parse_delimited("\u{feff}name,amount\n\"Ada, Countess\",12\nbob\n", b',').unwrap();
        assert_eq!(rows, vec![vec!["name", "amount"], vec!["Ada, Countess", "12"], vec!["bob"]]);
        let rows = parse_delimited("a\tb\n1\t2\n", b'\t').unwrap();
        assert_eq!(rows[1], vec!["1", "2"]);
        let v = parse_values("[[1, \"a\", true, null]]").unwrap();
        assert_eq!(v[0].len(), 4);
        assert!(parse_values("{\"a\": 1}").is_err());
        assert!(parse_values("[1, 2]").is_err());
    }

    fn args() -> StyleArgs {
        StyleArgs {
            sheet: "s".into(),
            range: "A1".into(),
            tab: None,
            copy: None,
            bold: None,
            italic: None,
            underline: None,
            strike: None,
            wrap: None,
            color: None,
            fill: None,
            align: None,
            valign: None,
            format: None,
            font: None,
            size: None,
            border: None,
            border_color: None,
            clear: false,
        }
    }

    #[test]
    fn style_flags_become_a_patch() {
        let mut a = args();
        assert_eq!(style_patch(&a).unwrap_err().exit, 2);
        a.bold = Some(true);
        a.fill = Some("none".into());
        a.format = Some("0.00".into());
        a.align = Some("center".into());
        a.border = Some("thin".into());
        a.border_color = Some("#000".into());
        let p = style_patch(&a).unwrap();
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["bold"], true);
        assert_eq!(j["fill"], Json::Null);
        assert_eq!(j["num_fmt"], "0.00");
        assert_eq!(j["h_align"], "center");
        assert_eq!(j["border_top"]["style"], "thin");
        assert_eq!(j["border_top"]["color"], "#000000");
        let mut a = args();
        a.border_color = Some("#000".into());
        assert!(style_patch(&a).is_err());
    }

    #[test]
    fn blocks() {
        assert_eq!(block(CellRef::new(1, 1), 3, 2).to_string(), "B2:C4");
        assert_eq!(block(CellRef::new(0, 0), 1, 1).to_string(), "A1");
    }
}
