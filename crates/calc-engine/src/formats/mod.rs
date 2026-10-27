//! Reading and writing files. Every format a sheet can be opened from or downloaded as goes through here, except
//! Parquet and Apple Numbers, which the service converts with DuckDB and the code runner (they arrive here as
//! rows of values), and OpenDocument/PDF and writing `.xls`, converted through the runner.
//!
//! Reading gives an [`Imported`]: the file's tabs as [`TabContent`] (values, formulas as A1 text written at their
//! own cell, styles, sizes, merges, frozen panes, hidden rows and columns), its defined names, charts, tab
//! colours and cell notes, and warnings naming anything the file had that a sheet can't hold. [`Imported::plan`]
//! turns that into one operation that adds the tabs to a workbook.

pub mod csv;
mod colors;
mod cell_controls;
mod conditional;
mod html;
mod json;
pub mod programs;
mod numfmts;
#[cfg(any(test, feature = "formats-wip"))]
mod ods;
mod text;
mod xls;
mod xlsx;
mod xlsx_write;
mod xml;

use serde::{Deserialize, Serialize};

use crate::{
    formula::{
        ast::TabId,
        parse::{TabNames, parse_formula, to_a1},
    },
    model::{Chart, DefinedName},
    ops::{Op, TabContent, TabProps, valid_tab_name},
    workbook::Workbook,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Csv,
    Tsv,
    Xlsx,
    Xls,
    Ods,
    Numbers,
    Json,
    Parquet,
    Pdf,
    Html,
}

impl Format {
    pub const ALL: [Format; 10] =
        [Self::Csv, Self::Tsv, Self::Xlsx, Self::Xls, Self::Ods, Self::Numbers, Self::Json, Self::Parquet, Self::Pdf, Self::Html];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
            Self::Ods => "ods",
            Self::Numbers => "numbers",
            Self::Json => "json",
            Self::Parquet => "parquet",
            Self::Pdf => "pdf",
            Self::Html => "html",
        }
    }

    /// The format's name as people know it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Xlsx => "Excel",
            Self::Xls => "Excel 97-2003",
            Self::Ods => "OpenDocument",
            Self::Numbers => "Numbers",
            Self::Json => "JSON",
            Self::Parquet => "Parquet",
            Self::Pdf => "PDF",
            Self::Html => "Web page",
        }
    }

    pub fn media_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv; charset=utf-8",
            Self::Tsv => "text/tab-separated-values; charset=utf-8",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Numbers => "application/vnd.apple.numbers",
            Self::Json => "application/json",
            Self::Parquet => "application/vnd.apache.parquet",
            Self::Pdf => "application/pdf",
            Self::Html => "text/html; charset=utf-8",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "csv" | "txt" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            "xlsx" | "xlsm" | "xltx" | "xltm" => Self::Xlsx,
            "xls" => Self::Xls,
            "ods" | "ots" => Self::Ods,
            "numbers" => Self::Numbers,
            "json" | "jsonl" | "ndjson" => Self::Json,
            "parquet" => Self::Parquet,
            "pdf" => Self::Pdf,
            "html" | "htm" => Self::Html,
            _ => return None,
        })
    }

    /// From a file name, falling back to the first bytes (and, for zip packages, the parts inside).
    pub fn detect(filename: &str, bytes: &[u8]) -> Option<Self> {
        if let Some(f) = filename.rsplit_once('.').and_then(|(_, e)| Self::from_extension(e)) {
            return Some(f);
        }
        if bytes.starts_with(b"PAR1") {
            return Some(Self::Parquet);
        }
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            return Some(Self::Xls);
        }
        if bytes.starts_with(b"%PDF") {
            return Some(Self::Pdf);
        }
        if bytes.starts_with(b"PK") {
            return Some(zip_kind(bytes).unwrap_or(Self::Xlsx));
        }
        let head = text::decode_text(&bytes[..bytes.len().min(8192)]).0;
        let t = head.trim_start_matches('\u{feff}').trim_start();
        if t.starts_with('[') || t.starts_with('{') {
            return Some(Self::Json);
        }
        let first_line = t.lines().next().unwrap_or("");
        if first_line.contains('\t') && !first_line.contains(',') {
            return Some(Self::Tsv);
        }
        Some(Self::Csv)
    }

    /// Formats a whole sheet can be downloaded as (the rest download one tab).
    pub fn holds_many_tabs(self) -> bool {
        matches!(self, Self::Xlsx | Self::Xls | Self::Ods | Self::Numbers | Self::Json | Self::Pdf | Self::Html)
    }

    /// Whether a file in this format can be opened (every format but PDF and HTML).
    pub fn can_open(self) -> bool {
        !matches!(self, Self::Pdf | Self::Html)
    }
}

/// Which kind of zip package this is, from the names of its parts.
fn zip_kind(bytes: &[u8]) -> Option<Format> {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut kind = None;
    for name in archive.file_names() {
        let lower = name.to_ascii_lowercase();
        if lower == "mimetype" || lower == "content.xml" {
            kind = Some(Format::Ods);
        } else if lower.starts_with("xl/") || lower == "[content_types].xml" {
            return Some(Format::Xlsx);
        } else if lower.starts_with("index/") && lower.ends_with(".iwa") {
            return Some(Format::Numbers);
        }
    }
    kind
}

/// Extra things about a tab that its contents don't carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    /// The tab's colour, `#RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The file had the tab hidden (sheets can't hide tabs, so it is shown).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// A note (or comment thread) on a cell in the file, for the service to turn into a comment thread.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// Index into [`Imported::tabs`].
    pub tab: usize,
    pub row: u32,
    pub col: u32,
    /// The name the file gives its author (may be empty).
    pub author: String,
    pub text: String,
    /// Replies, in order: `(author, text)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<(String, String)>,
}

/// What reading a file gave.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    /// Tabs in order, with their names (as the file gives them; [`Imported::plan`] makes them fit a sheet).
    pub tabs: Vec<(String, TabContent)>,
    /// Defined names. `DefinedName::tab` here is an index into `tabs` (the tab a local name belongs to), not
    /// an id: [`Imported::plan`] maps it.
    pub names: Vec<DefinedName>,
    /// Things the file had that could not be kept, in words for the person who opened it.
    pub warnings: Vec<String>,
    /// Per tab (same order as `tabs`; missing entries mean nothing extra).
    pub tab_info: Vec<TabInfo>,
    /// Charts. `Chart::tab` here is an index into `tabs`; `Chart::id` is unique within the file.
    pub charts: Vec<Chart>,
    /// Notes and comments on cells.
    pub notes: Vec<Note>,
    /// JavaScript retained by a Calc workbook download, never evaluated during parsing.
    pub functions: Vec<crate::javascript::Function>,
    /// Original tab identities, when a Calc download carries executable definitions.
    pub source_tabs: Vec<(TabId, String)>,
}

/// Options for reading.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// CSV: the first row is a header row (always kept as text). Detected when None.
    pub header: Option<bool>,
    /// CSV: the delimiter; detected when None.
    pub delimiter: Option<u8>,
    /// Keep every value as typed text (no numbers or dates).
    pub raw_text: bool,
}

/// Options for writing.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// One tab only. Formats that hold one tab (CSV, TSV) use the first when None; the others write every tab.
    pub tab: Option<TabId>,
    /// Write values instead of formulas. JSON writes a formula cell as `{"v": value, "f": "=…"}` unless this is
    /// set, so the service sets it for a plain JSON download and clears it for `formulas: true`.
    pub values_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("This file can't be read as {format}: {message}")]
    Read { format: &'static str, message: String },
    #[error("The sheet can't be written as {format}: {message}")]
    Write { format: &'static str, message: String },
    #[error("{0} files are converted by the service, not the engine.")]
    External(&'static str),
    #[error("{0} is not supported.")]
    Unsupported(&'static str),
}

impl FormatError {
    /// What the person can do about it.
    pub fn hint(&self) -> String {
        match self {
            Self::Read { format, .. } => format!(
                "Check that the file really is {format} and opens in another spreadsheet app, or save it as .xlsx or .csv and open that."
            ),
            Self::Write { .. } => "Download one tab at a time, or choose CSV, which has no size limits.".to_owned(),
            Self::External(_) => "Send the file through the service's import, which converts it first.".to_owned(),
            Self::Unsupported(_) => "Choose another format: .xlsx, .ods, .csv, .tsv or .json open; PDF and HTML only download.".to_owned(),
        }
    }

    pub(crate) fn read(format: &'static str, message: impl Into<String>) -> Self {
        Self::Read { format, message: message.into() }
    }

    pub(crate) fn write(format: &'static str, message: impl Into<String>) -> Self {
        Self::Write { format, message: message.into() }
    }
}

/// Reads a file.
pub fn import(format: Format, bytes: &[u8], opts: &ImportOptions) -> Result<Imported, FormatError> {
    match format {
        Format::Csv => csv::import(bytes, opts.delimiter.or(Some(b',')), opts),
        Format::Tsv => csv::import(bytes, Some(b'\t'), opts),
        Format::Xlsx => xlsx::import(bytes, opts),
        Format::Xls => xls::import(bytes, opts),
        Format::Ods => Err(FormatError::External("OpenDocument")),
        Format::Json => json::import(bytes, opts),
        Format::Parquet => Err(FormatError::External("Parquet")),
        Format::Numbers => Err(FormatError::External("Numbers")),
        Format::Pdf => Err(FormatError::Unsupported("Opening PDF")),
        Format::Html => Err(FormatError::Unsupported("Opening HTML")),
    }
}

/// Writes a sheet (or one tab of it).
pub fn export(wb: &Workbook, format: Format, opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    programs::ensure_calculated(wb, format, opts)?;
    match format {
        Format::Csv => csv::export(wb, opts, b','),
        Format::Tsv => csv::export(wb, opts, b'\t'),
        Format::Xlsx => xlsx_write::export(wb, opts),
        Format::Ods => Err(FormatError::External("OpenDocument")),
        Format::Json => json::export(wb, opts),
        Format::Pdf => Err(FormatError::External("PDF")),
        Format::Xls => Err(FormatError::External("Excel 97-2003")),
        Format::Parquet => Err(FormatError::External("Parquet")),
        Format::Numbers => Err(FormatError::External("Numbers")),
        Format::Html => html::export(wb, opts),
    }
}

/// The tab an export of one tab uses.
pub(crate) fn export_tab(wb: &Workbook, opts: &ExportOptions) -> Option<TabId> {
    opts.tab.or_else(|| wb.tabs.first().map(|t| t.id))
}

/// The tabs an export of a whole sheet writes: the one asked for, or all of them.
pub(crate) fn export_tabs(wb: &Workbook, opts: &ExportOptions, format: &'static str) -> Result<Vec<TabId>, FormatError> {
    match opts.tab {
        Some(t) if wb.tab(t).is_some() => Ok(vec![t]),
        Some(_) => Err(FormatError::write(format, "that tab doesn't exist")),
        None if wb.tabs.is_empty() => Err(FormatError::write(format, "it has no tabs")),
        None => Ok(wb.tabs.iter().map(|t| t.id).collect()),
    }
}

// ---- turning an import into an operation ----------------------------------------------------------------------

/// How an import becomes part of a sheet.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    /// One operation adding every tab of the file with its contents, colours, defined names and charts.
    pub op: Op,
    /// The ids the new tabs get, in the file's order.
    pub tabs: Vec<TabId>,
    /// The names they get: a name the sheet already uses, or one a tab can't have, is changed (and every
    /// formula, name and chart of the file follows).
    pub names: Vec<String>,
    /// Defined names of the file left out because the sheet already defines them.
    pub skipped_names: Vec<String>,
}

/// Tab names by provisional id (index + 1), for reading and rewriting an import's formulas.
pub(crate) struct IndexNames<'a>(pub &'a [String]);

impl TabNames for IndexNames<'_> {
    fn tab_id(&self, name: &str) -> Option<TabId> {
        self.0.iter().position(|n| n.eq_ignore_ascii_case(name)).map(|i| i as TabId + 1)
    }

    fn tab_name(&self, id: TabId) -> Option<String> {
        self.0.get((id as usize).checked_sub(1)?).cloned()
    }
}

/// A tab name made valid: forbidden characters replaced, trimmed, at most 100 characters, never empty.
pub(crate) fn clean_tab_name(name: &str, fallback: &str) -> String {
    let mut n: String = name
        .chars()
        .map(|c| if matches!(c, ':' | '\\' | '/' | '?' | '*' | '[' | ']') || c.is_control() { ' ' } else { c })
        .collect();
    n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = n.trim_matches('\'').trim();
    let n: String = n.chars().take(100).collect();
    let n = n.trim().to_owned();
    if n.is_empty() { fallback.to_owned() } else { n }
}

/// `base`, or `base 2`, `base 3`, … whichever no name in `taken` has (case-insensitive).
fn unique_name(base: &str, taken: &[String]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if free(base) {
        return base.to_owned();
    }
    let stem: String = base.chars().take(94).collect();
    (2..).map(|i| format!("{stem} {i}")).find(|n| free(n)).unwrap_or_default()
}

/// Rewrites a formula written for the file's tab names to the tab names it gets (`text` without `=`).
fn rename_in_formula(text: &str, row: u32, col: u32, old: &IndexNames<'_>, new: &IndexNames<'_>) -> String {
    match parse_formula(text, row, col, old) {
        Ok(expr) => to_a1(&expr, row, col, new),
        Err(_) => text.to_owned(),
    }
}

/// Rewrites a range text like `Sales!A1:C9` (charts) to the new tab names.
fn rename_in_range(range: &str, old: &[String], new: &[String]) -> String {
    let (tab, rest) = crate::addr::split_tab(range);
    match tab.and_then(|t| old.iter().position(|o| o.eq_ignore_ascii_case(&t))) {
        Some(i) => format!("{}!{rest}", crate::addr::quote_tab(&new[i])),
        None => range.to_owned(),
    }
}

impl Imported {
    /// Whether the file gave nothing at all.
    pub fn is_empty(&self) -> bool {
        self.tabs.iter().all(|(_, c)| c.columns.iter().all(|col| col.values.is_empty() && col.styles.is_empty()))
    }

    /// One operation adding every tab of the file to `wb` (at `index`, or after the last tab), with the ids,
    /// names and defined names the tabs end up with. Tabs whose name `wb` already uses are renamed ("Sales 2"),
    /// and the file's formulas, names and charts are rewritten to match.
    pub fn plan(&self, wb: &Workbook, index: Option<usize>) -> ImportPlan {
        let file_names: Vec<String> = self.tabs.iter().map(|(n, _)| n.clone()).collect();
        let mut taken: Vec<String> = wb.tabs.iter().map(|t| t.name.clone()).collect();
        let mut names = Vec::with_capacity(self.tabs.len());
        for (i, (name, _)) in self.tabs.iter().enumerate() {
            let clean = clean_tab_name(name, &format!("Tab {}", i + 1));
            let clean = if valid_tab_name(&clean) { clean } else { format!("Tab {}", i + 1) };
            let n = unique_name(&clean, &taken);
            taken.push(n.clone());
            names.push(n);
        }
        let renamed = file_names.iter().zip(&names).any(|(a, b)| a != b);
        let ids: Vec<TabId> = if wb.tabs.is_empty() && self.source_tabs.len() == self.tabs.len() {
            self.source_tabs.iter().map(|t| t.0).collect()
        } else { (0..self.tabs.len() as TabId).map(|i| wb.next_tab_id + i).collect() };
        let (old_n, new_n) = (IndexNames(&file_names), IndexNames(&names));

        let mut ops = Vec::new();
        if let Some(functions) = self.import_functions(wb, &ids, &names) { ops.push(Op::SetFunctions { functions }); }
        let at = index.map(|i| i.min(wb.tabs.len()));
        for (i, id) in ids.iter().enumerate() {
            ops.push(Op::AddTab { tab: *id, name: names[i].clone(), index: at.map(|a| a + i) });
        }
        for (i, (_, content)) in self.tabs.iter().enumerate() {
            let mut content = if renamed { rename_content(content, &old_n, &new_n) } else { content.clone() };
            self.bind_programs(&mut content, &ids, &names);
            ops.push(Op::ReplaceTab { tab: ids[i], content: Box::new(content) });
            if let Some(color) = self.tab_info.get(i).and_then(|t| t.color.clone()) {
                ops.push(Op::TabProps { tab: ids[i], props: TabProps { color: Some(Some(color)), ..Default::default() } });
            }
        }
        // Defined names: the sheet's own stay; the file's are added unless the sheet already defines them.
        let mut skipped_names = Vec::new();
        if !self.names.is_empty() {
            let mut all = wb.names.clone();
            for n in &self.names {
                let tab = n.tab.and_then(|i| ids.get(i as usize).copied());
                if n.tab.is_some() && tab.is_none() {
                    continue;
                }
                if all.iter().any(|e| e.name.eq_ignore_ascii_case(&n.name) && e.tab == tab) {
                    skipped_names.push(n.name.clone());
                    continue;
                }
                let text = if renamed { rename_in_formula(&n.text, 0, 0, &old_n, &new_n) } else { n.text.clone() };
                let text = if self.source_tabs.is_empty() { text } else { programs::bind_formula(&text,0,0,&self.source_tabs,&ids,&names) };
                all.push(DefinedName { name: n.name.clone(), text, tab });
            }
            if all.len() != wb.names.len() {
                ops.push(Op::SetNames { names: all });
            }
        }
        let used_ids: Vec<&str> = wb.charts.iter().map(|c| c.id.as_str()).collect();
        for (k, chart) in self.charts.iter().enumerate() {
            let Some(&tab) = ids.get(chart.tab as usize) else { continue };
            let mut c = chart.clone();
            c.tab = tab;
            if renamed {
                c.range = rename_in_range(&c.range, &file_names, &names);
            }
            let mut id = format!("chart-{tab}-{}", k + 1);
            while used_ids.contains(&id.as_str()) {
                id.push('x');
            }
            c.id = id;
            ops.push(Op::PutChart { chart: c });
        }
        ImportPlan { op: Op::Batch { ops }, tabs: ids, names, skipped_names }
    }

    /// One operation replacing `tab`'s contents with the file's first tab (formulas of that tab that name it
    /// are rewritten to `tab`'s name; references to the file's other tabs become `#REF!`).
    pub fn plan_replace(&self, wb: &Workbook, tab: TabId) -> Option<Op> {
        let (file_name, content) = self.tabs.first()?;
        let target = wb.tab(tab)?.name.clone();
        let file_names: Vec<String> = self.tabs.iter().map(|(n, _)| n.clone()).collect();
        let mut content = if !target.eq_ignore_ascii_case(file_name) || self.tabs.len() > 1 {
            let mut new_names = vec![String::new(); file_names.len()];
            new_names[0] = target.clone();
            rename_content(content, &IndexNames(&file_names), &IndexNames(&new_names))
        } else {
            content.clone()
        };
        self.bind_programs(&mut content, &[tab], std::slice::from_ref(&target));
        let replace=Op::ReplaceTab { tab, content: Box::new(content) };
        Some(match self.import_functions(wb, &[tab], &[target]) {
            Some(functions) => Op::Batch {ops:vec![Op::SetFunctions {functions},replace]},
            None => replace,
        })
    }

    fn import_functions(&self, wb: &Workbook, ids: &[TabId], names: &[String]) -> Option<Vec<crate::javascript::Function>> {
        if self.functions.is_empty() {return None;}
        let mut all=wb.functions.clone();
        for function in &self.functions {
            let mut f=function.clone();
            f.code=programs::bind_source(&f.code,&self.source_tabs,ids,names);
            if !all.contains(&f) {all.push(f);}
        }
        Some(all)
    }

    fn bind_programs(&self, content: &mut TabContent, ids: &[TabId], names: &[String]) {
        if self.source_tabs.is_empty() {return;}
        if let Some(code)=&mut content.code {
            code.code=programs::bind_source(&code.code,&self.source_tabs,ids,names);
            code.inputs=code.inputs.iter().filter_map(|id|self.source_tabs.iter().position(|t|t.0==*id).and_then(|i|ids.get(i).copied())).collect();
        }
        for (c,col) in content.columns.iter_mut().enumerate() { for (r,text) in &mut col.formulas {
            *text=programs::bind_formula(text,*r,c as u32,&self.source_tabs,ids,names);
        } }
    }

    /// The defined names with their tabs mapped to ids (`ids[i]` is the id tab `i` got).
    pub fn names_for(&self, ids: &[TabId]) -> Vec<DefinedName> {
        self.names
            .iter()
            .filter_map(|n| {
                let tab = match n.tab {
                    Some(i) => Some(*ids.get(i as usize)?),
                    None => None,
                };
                Some(DefinedName { name: n.name.clone(), text: n.text.clone(), tab })
            })
            .collect()
    }
}

/// A tab's content with its formulas rewritten from the file's tab names to new ones (an empty new name turns
/// references to that tab into `#REF!`).
fn rename_content(content: &TabContent, old: &IndexNames<'_>, new: &IndexNames<'_>) -> TabContent {
    let mut c = content.clone();
    for (ci, col) in c.columns.iter_mut().enumerate() {
        for (r, f) in col.formulas.iter_mut() {
            let text = f.strip_prefix('=').unwrap_or(f);
            *f = rename_in_formula(text, *r, ci as u32, old, new);
        }
    }
    c
}

// ---- shared helpers -------------------------------------------------------------------------------------------

/// A number as text with Excel's 15 significant digits: `0.1+0.2` is `0.3`, `1E+15` and `1.5E-12` beyond.
pub(crate) fn number_text(n: f64) -> String {
    if !n.is_finite() {
        return "#NUM!".to_owned();
    }
    if n == 0.0 {
        return "0".to_owned();
    }
    let abs = n.abs();
    if !(1e-9..1e15).contains(&abs) {
        let s = format!("{n:.14E}");
        let (m, e) = s.split_once('E').unwrap_or((&s, "0"));
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{}E{}{:02}", trim_fraction(m), if e < 0 { '-' } else { '+' }, e.abs());
    }
    let digits_before = abs.log10().floor() as i32 + 1;
    let decimals = (15 - digits_before).clamp(0, 30) as usize;
    let s = trim_fraction(&format!("{n:.decimals$}"));
    if s == "-0" { "0".to_owned() } else { s }
}

fn trim_fraction(s: &str) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s.to_owned() }
}

/// A number rounded to 15 significant digits (what `number_text` shows), for formats that hold numbers.
#[cfg(test)]
pub(crate) fn round15(n: f64) -> f64 {
    if !n.is_finite() || n == 0.0 {
        return n;
    }
    number_text(n).parse().unwrap_or(n)
}

/// Collects warnings, counting repeats of one kind and keeping the first few examples.
#[derive(Default)]
pub(crate) struct Warnings {
    kinds: Vec<(&'static str, usize, Vec<String>)>,
    plain: Vec<String>,
}

impl Warnings {
    /// Counts one occurrence of a kind of problem, with an example (`B7 on Sales: =Table1[@Price]`).
    pub fn count(&mut self, kind: &'static str, example: impl FnOnce() -> String) {
        match self.kinds.iter_mut().find(|k| k.0 == kind) {
            Some(k) => {
                k.1 += 1;
                if k.2.len() < 3 {
                    k.2.push(example());
                }
            }
            None => self.kinds.push((kind, 1, vec![example()])),
        }
    }

    /// A one-off warning, kept once.
    pub fn say(&mut self, text: impl Into<String>) {
        let t = text.into();
        if !self.plain.contains(&t) {
            self.plain.push(t);
        }
    }

    /// The warnings as sentences. `describe(kind, count)` words each kind.
    pub fn finish(self, describe: impl Fn(&str, usize) -> String) -> Vec<String> {
        let mut out = self.plain;
        for (kind, n, examples) in self.kinds {
            let mut s = describe(kind, n);
            let examples: Vec<String> = examples.into_iter().filter(|e| !e.is_empty()).collect();
            if !examples.is_empty() {
                s.push_str(&format!(" ({}{})", examples.join("; "), if n > examples.len() { "; …" } else { "" }));
            }
            out.push(s);
        }
        out
    }
}

/// "1 cell" / "3 cells".
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_as_text() {
        assert_eq!(number_text(0.1 + 0.2), "0.3");
        assert_eq!(number_text(1200.0), "1200");
        assert_eq!(number_text(-3.5), "-3.5");
        assert_eq!(number_text(std::f64::consts::PI), "3.14159265358979");
        assert_eq!(number_text(123456789.123456789), "123456789.123457");
        assert_eq!(number_text(0.001), "0.001");
        assert_eq!(number_text(1e15), "1E+15");
        assert_eq!(number_text(1.5e-12), "1.5E-12");
        assert_eq!(number_text(-0.0), "0");
        assert_eq!(round15(0.1 + 0.2), 0.3);
    }

    #[test]
    fn tab_names_are_made_valid() {
        assert_eq!(clean_tab_name("Q3: [draft]", "Tab 1"), "Q3 draft");
        assert_eq!(clean_tab_name("   ", "Tab 4"), "Tab 4");
        assert_eq!(unique_name("Sales", &["sales".into(), "Sales 2".into()]), "Sales 3");
    }

    #[test]
    fn detects_formats() {
        assert_eq!(Format::detect("data.CSV", b""), Some(Format::Csv));
        assert_eq!(Format::detect("x", b"[{\"a\":1}]"), Some(Format::Json));
        assert_eq!(Format::detect("x", b"a\tb\n1\t2\n"), Some(Format::Tsv));
        assert_eq!(Format::detect("x", b"%PDF-1.4"), Some(Format::Pdf));
        assert_eq!(Format::detect("x", b"a,b\n"), Some(Format::Csv));
    }
}
