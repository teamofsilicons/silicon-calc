//! The runner's side of a Python or JavaScript run: answering the child's requests (a tab, a cell, the tab names,
//! a query), noting what it gives back, and reading its result.
//!
//! Requests and answers are one line of JSON each:
//!
//! | request | answer |
//! | --- | --- |
//! | `{"op": "tabs"}` | `{"ok": true, "tabs": ["Sales", …]}` |
//! | `{"op": "tab", "name": "Sales"}` | `{"ok": true, "path", "columns", "types", "rows"}`: Parquet (Python) or JSON (JavaScript) |
//! | `{"op": "cell", "ref": "Sales!B2"}` | `{"ok": true, "value", "kind"}` |
//! | `{"op": "sql", "query": "…"}` (JavaScript) | `{"ok": true, "path", "columns", "rows"}`: JSON |
//! | `{"op": "result", "path", "format", "json_columns"?}` | `{"ok": true}` |
//! | `{"op": "failed", "type", "message", "line"?, "traceback"?}` | `{"ok": true}` |
//!
//! A failed request answers `{"ok": false, "error": "…", "hint": "…", "line"?}`.
//!
//! The JSON files have one array per column: `{"columns": […], "types": […], "rows": n, "data": [[…], …]}`, with
//! dates and times as milliseconds since 1970 (UTC) and times of day as `13:45:00` text.

use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{BufReader, BufWriter, Write as _},
    path::{Path, PathBuf},
};

use calc_engine::{TabId, Value};
use duckdb::arrow::{
    array::{Array, AsArray},
    datatypes::{DataType, Date32Type, Float64Type, Int64Type, Time64MicrosecondType, TimeUnit, TimestampMicrosecondType},
};
use serde::de::{self, DeserializeSeed, Deserializer as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Value as Json, json};

use crate::{
    Language, OutColumn, RunError, RunErrorKind, RunOutput, Runner, SheetSource,
    duck::{Duck, Settings, quote_literal},
    sandbox::{Finished, Stop},
    sql::Session,
    table::Table,
    values::{
        DATE_FORMAT, DATETIME_FORMAT, TIME_FORMAT, cell_json, group_digits, iso_time, unix_micros_from_serial,
        value_from_json,
    },
};

/// A tab written for the child.
struct TabFile {
    path: PathBuf,
    columns: Vec<String>,
    types: Vec<&'static str>,
    rows: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultFormat {
    Parquet,
    Json,
}

/// What the child said its result is.
struct ResultNote {
    path: PathBuf,
    format: ResultFormat,
    /// Parquet columns holding JSON text (columns that mixed kinds of values).
    json_columns: Vec<usize>,
}

/// What the child said went wrong.
struct FailureNote {
    kind: String,
    message: String,
    line: Option<u32>,
    traceback: String,
}

pub(crate) struct Host<'a> {
    runner: &'a Runner,
    sheet: &'a dyn SheetSource,
    scratch: &'a Path,
    language: Language,
    tabs: HashMap<String, TabFile>,
    inputs: Vec<TabId>,
    sql: Option<Session<'a>>,
    files: Option<Duck>,
    result: Option<ResultNote>,
    failure: Option<FailureNote>,
    next_file: u32,
}

fn fail(error: impl Into<String>, hint: impl Into<String>) -> Json {
    json!({"ok": false, "error": error.into(), "hint": hint.into()})
}

fn quoted_list(names: &[String]) -> String {
    names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
}

impl<'a> Host<'a> {
    pub fn new(runner: &'a Runner, sheet: &'a dyn SheetSource, scratch: &'a Path, language: Language) -> Self {
        Self {
            runner,
            sheet,
            scratch,
            language,
            tabs: HashMap::new(),
            inputs: Vec::new(),
            sql: None,
            files: None,
            result: None,
            failure: None,
            next_file: 0,
        }
    }

    /// Answers one request line.
    pub fn handle(&mut self, line: &[u8]) -> Vec<u8> {
        let answer = match serde_json::from_slice::<Json>(line) {
            Ok(req) => self.answer(&req),
            Err(e) => fail(format!("The runner couldn't read a request from the code's helper: {e}"), "This is a bug in Silicon Calc's helper."),
        };
        serde_json::to_vec(&answer).unwrap_or_else(|_| b"{\"ok\":false}".to_vec())
    }

    fn answer(&mut self, req: &Json) -> Json {
        let text = |k: &str| req.get(k).and_then(Json::as_str).unwrap_or("");
        match text("op") {
            "tabs" => json!({"ok": true, "tabs": self.sheet.tab_names()}),
            "tab" => self.tab(text("name")),
            "cell" => self.cell(text("ref")),
            "sql" => self.sql(text("query")),
            "result" => {
                let format = if text("format") == "parquet" { ResultFormat::Parquet } else { ResultFormat::Json };
                let json_columns =
                    req.get("json_columns").and_then(Json::as_array).map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect()).unwrap_or_default();
                match self.inside_scratch(text("path")) {
                    Some(path) => {
                        self.result = Some(ResultNote { path, format, json_columns });
                        json!({"ok": true})
                    }
                    None => fail("The result file must be in the run's own folder.", "Give the result with calc.result()."),
                }
            }
            "failed" => {
                self.failure = Some(FailureNote {
                    kind: text("type").to_owned(),
                    message: text("message").to_owned(),
                    line: req.get("line").and_then(Json::as_u64).map(|l| l as u32),
                    traceback: text("traceback").to_owned(),
                });
                json!({"ok": true})
            }
            other => fail(format!("The runner doesn't know the request \"{other}\"."), "This is a bug in Silicon Calc's helper."),
        }
    }

    /// A path the child gave, if it is inside the scratch folder.
    fn inside_scratch(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);
        let p = if p.is_absolute() { p.to_owned() } else { self.scratch.join(p) };
        let real = p.canonicalize().ok()?;
        real.starts_with(self.scratch).then_some(real)
    }

    fn file_name(&mut self, ext: &str) -> PathBuf {
        self.next_file += 1;
        self.scratch.join(format!("t{}.{ext}", self.next_file))
    }

    fn note_input(&mut self, tab: TabId) {
        if !self.inputs.contains(&tab) {
            self.inputs.push(tab);
        }
    }

    fn tab(&mut self, name: &str) -> Json {
        let key = name.trim().to_lowercase();
        if key.is_empty() {
            return fail("calc.tab() needs the name of a tab.", "Write calc.tab(\"Sales\"), or a range like calc.tab(\"Sales!A1:C9\").");
        }
        if !self.tabs.contains_key(&key) {
            let Some(table) = self.sheet.table(name) else {
                return fail(
                    format!("There is no tab named \"{}\" in this sheet.", name.trim()),
                    format!(
                        "The tabs are {}. A range of a tab works too, like \"Sales!A1:C9\".",
                        quoted_list(&self.sheet.tab_names())
                    ),
                );
            };
            self.note_input(table.tab);
            let written = match self.language {
                Language::JavaScript => self.write_json_tab(&table),
                _ => self.write_parquet_tab(&table),
            };
            match written {
                Ok(f) => {
                    self.tabs.insert(key.clone(), f);
                }
                Err(e) => return fail(e.message, e.hint),
            }
        }
        let f = &self.tabs[&key];
        json!({"ok": true, "path": f.path.to_string_lossy(), "columns": f.columns, "types": f.types, "rows": f.rows})
    }

    fn files_db(&mut self) -> Result<&Duck, RunError> {
        if self.files.is_none() {
            let l = &self.runner.config().limits;
            let spill = self.scratch.join("tmp");
            self.files = Some(Duck::open(&Settings { files: true, threads: l.threads, memory_bytes: l.memory_bytes, spill_dir: &spill })?);
        }
        Ok(self.files.as_ref().expect("just opened"))
    }

    fn write_parquet_tab(&mut self, table: &Table) -> Result<TabFile, RunError> {
        let path = self.file_name("parquet");
        let internal = format!("t{}", self.next_file);
        let db = {
            self.files_db()?;
            self.files.as_mut().expect("opened")
        };
        db.register_as(&internal, table)?;
        db.write_parquet(&internal, &path)?;
        db.execute(&format!("DROP TABLE {}", crate::duck::quote_ident(&internal)))?;
        // Registering the same name again later must load it again.
        db.forget(&internal);
        Ok(TabFile {
            path,
            columns: table.columns.clone(),
            types: table.types.iter().map(|t| t.name()).collect(),
            rows: table.rows(),
        })
    }

    fn write_json_tab(&mut self, table: &Table) -> Result<TabFile, RunError> {
        let path = self.file_name("json");
        let file = File::create(&path).map_err(RunError::internal)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        write_table_json(&mut w, table).map_err(RunError::internal)?;
        w.flush().map_err(RunError::internal)?;
        Ok(TabFile {
            path,
            columns: table.columns.clone(),
            types: table.types.iter().map(|t| t.name()).collect(),
            rows: table.rows(),
        })
    }

    fn cell(&mut self, reference: &str) -> Json {
        if reference.trim().is_empty() {
            return fail("calc.cell() needs a cell, like \"Sales!B2\".", "Write calc.cell(\"Sales!B2\"); without a tab it reads the first tab.");
        }
        match self.sheet.cell(reference) {
            Some(c) => {
                self.note_input(c.tab);
                let (value, kind) = cell_json(&c.value, c.format.as_deref());
                json!({"ok": true, "value": value, "kind": kind})
            }
            None => fail(
                format!("\"{}\" isn't a cell of this sheet.", reference.trim()),
                format!("Write a tab and a cell, like \"Sales!B2\" (quote names with spaces: \"'Q3 plan'!B2\"). The tabs are {}.", quoted_list(&self.sheet.tab_names())),
            ),
        }
    }

    fn sql(&mut self, query: &str) -> Json {
        match self.run_sql(query) {
            Ok(v) => v,
            Err(e) => {
                let mut v = fail(e.message, e.hint);
                if let Some(l) = e.line {
                    v["line"] = json!(l);
                }
                v
            }
        }
    }

    fn run_sql(&mut self, query: &str) -> Result<Json, RunError> {
        if self.sql.is_none() {
            self.sql = Some(Session::open(self.runner, self.sheet, &self.scratch.join("tmp"))?);
        }
        let session = self.sql.as_mut().expect("opened");
        session.prepare(query)?;
        let (columns, rows) = session.query(self.runner, query, None)?;
        let inputs = session.inputs.clone();
        for t in inputs {
            self.note_input(t);
        }
        let path = self.file_name("json");
        let file = File::create(&path).map_err(RunError::internal)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        write_columns_json(&mut w, &columns, rows).map_err(RunError::internal)?;
        w.flush().map_err(RunError::internal)?;
        let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        Ok(json!({"ok": true, "path": path.to_string_lossy(), "columns": names, "rows": rows}))
    }

    /// What the run gave, once the child has ended.
    pub fn finish(mut self, f: Finished, keep_rows: Option<usize>) -> Result<RunOutput, RunError> {
        let lang = self.language.label();
        let limits = &self.runner.config().limits;
        let with_output = |e: RunError, f: &Finished| e.with_output(f.stdout.clone(), f.stderr.clone());
        match f.stop {
            Some(Stop::Timeout) => {
                return Err(with_output(
                    RunError::new(
                        RunErrorKind::Timeout,
                        format!("The {lang} code ran for more than {} seconds and was stopped.", limits.timeout.as_secs().max(1)),
                        "Make it lighter: work on fewer rows, or let SQL do the heavy part (calc.sql).",
                    ),
                    &f,
                ));
            }
            Some(Stop::Memory) => {
                return Err(with_output(
                    RunError::new(
                        RunErrorKind::Memory,
                        format!("The {lang} code used more than {} of memory and was stopped.", bytes_text(limits.memory_bytes)),
                        "Read only the columns and rows you need, or let SQL aggregate first (calc.sql).",
                    ),
                    &f,
                ));
            }
            Some(Stop::Disk) => {
                return Err(with_output(
                    RunError::new(
                        RunErrorKind::TooLarge,
                        format!("The {lang} code wrote more than {} into its scratch space and was stopped.", bytes_text(limits.scratch_bytes)),
                        "Write less: give the result with calc.result() instead of writing files.",
                    ),
                    &f,
                ));
            }
            None => {}
        }
        if let Some(n) = self.failure.take() {
            return Err(with_output(failure_error(lang, &n), &f).with_stderr(join_text(&f.stderr, &n.traceback)).with_line(n.line));
        }
        if f.code != Some(0) {
            let how = match (f.code, f.signal) {
                (_, Some(9)) => "was killed".to_owned(),
                (_, Some(s)) => format!("ended on signal {s}"),
                (Some(c), _) => format!("stopped with exit code {c}"),
                _ => "stopped".to_owned(),
            };
            let sandboxed = f.stderr.contains("Operation not permitted") || f.stderr.contains("sandbox");
            return Err(with_output(
                RunError::failed(
                    format!("The {lang} process {how} before it finished."),
                    if sandboxed {
                        SANDBOX_HINT.to_owned()
                    } else {
                        "Look at stderr for what it said; code should end normally and give its result with calc.result().".to_owned()
                    },
                ),
                &f,
            ));
        }
        let mut out = RunOutput { stdout: f.stdout.clone(), stderr: f.stderr.clone(), inputs: std::mem::take(&mut self.inputs), ..Default::default() };
        let Some(note) = self.result.take() else { return Ok(out) };
        let (columns, rows) = match note.format {
            ResultFormat::Parquet => self.read_parquet_result(&note, keep_rows)?,
            ResultFormat::Json => read_json_result(&note.path, keep_rows, limits.result_cells)?,
        };
        out.columns = columns;
        out.row_count = rows;
        out.has_result = true;
        Ok(out)
    }

    fn read_parquet_result(&mut self, note: &ResultNote, keep: Option<usize>) -> Result<(Vec<OutColumn>, u64), RunError> {
        let limits = self.runner.config().limits.clone();
        let db = self.files_db()?;
        let sql = format!("SELECT * FROM read_parquet({})", quote_literal(&note.path.to_string_lossy()));
        let (mut columns, rows) = db.query(&sql, keep, limits.result_cells, limits.timeout).map_err(|e| {
            RunError::internal(format!("reading the result the code gave: {}", e.message))
        })?;
        for &i in &note.json_columns {
            if let Some(col) = columns.get_mut(i) {
                decode_json_column(col);
            }
        }
        Ok((columns, rows))
    }
}

pub(crate) const SANDBOX_HINT: &str = "Code in Silicon Calc runs with no network, no other programs and no files outside its own scratch space: read the sheet with calc.tab() and give the result with calc.result().";

fn join_text(a: &str, b: &str) -> String {
    match (a.trim().is_empty(), b.trim().is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{}\n{}", a.trim_end(), b),
    }
}

/// `2 GB`, `512 MB`.
pub(crate) fn bytes_text(n: u64) -> String {
    if n >= 1 << 30 && n % (1 << 30) == 0 {
        format!("{} GB", n >> 30)
    } else if n >= 1 << 30 {
        format!("{:.1} GB", n as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", (n >> 20).max(1))
    }
}

/// The error a failure report from the child stands for.
fn failure_error(lang: &str, n: &FailureNote) -> RunError {
    let at = n.line.map(|l| format!(" (line {l})")).unwrap_or_default();
    let what = if n.kind.is_empty() || n.kind == "CalcError" { n.message.clone() } else { format!("{}: {}", n.kind, n.message) };
    let what = what.trim().to_owned();
    if n.kind == "CalcError" {
        // The helper's own errors already say what to do; the hint is in the message's second sentence.
        let (message, hint) = split_hint(&what);
        return RunError::failed(format!("{message}{at}"), hint);
    }
    if n.kind == "MemoryError" || n.kind == "RangeError" && n.message.contains("memory") {
        return RunError::new(
            RunErrorKind::Memory,
            format!("The {lang} code ran out of memory{at}."),
            "Read only the columns and rows you need, or let SQL aggregate first (calc.sql).",
        );
    }
    let sandboxed = n.message.contains("Operation not permitted")
        || n.message.contains("EPERM")
        || n.kind == "PermissionError"
        || n.message.contains("ENOTFOUND")
        || n.message.contains("EACCES");
    let hint = if sandboxed {
        SANDBOX_HINT.to_owned()
    } else if n.kind == "SyntaxError" || n.kind == "IndentationError" || n.kind == "TabError" {
        format!("Fix the {lang} so it can be read, and run it again.")
    } else {
        "Fix the code and run it again; stderr has the whole trace.".to_owned()
    };
    RunError::failed(format!("The {lang} code failed{at}: {what}"), hint)
}

/// "What happened. What to do." → ("What happened.", "What to do.").
fn split_hint(text: &str) -> (String, String) {
    match text.split_once("\n") {
        Some((a, b)) => (a.trim().to_owned(), b.trim().to_owned()),
        None => (text.to_owned(), "Fix the code and run it again.".to_owned()),
    }
}

/// Decodes a column of JSON text (values of mixed kinds) into values, with a date format when it held dates.
fn decode_json_column(col: &mut OutColumn) {
    let (mut dates, mut with_time) = (0usize, false);
    for v in col.values.iter_mut() {
        if let Value::Text(s) = v {
            let parsed: Json = serde_json::from_str(s).unwrap_or(Json::String(s.to_string()));
            let (nv, is_date) = value_from_json(&parsed);
            if is_date {
                dates += 1;
                if let Value::Number(n) = nv
                    && n.fract() != 0.0
                {
                    with_time = true;
                }
            }
            *v = nv;
        }
    }
    if dates > 0 && col.format.is_none() {
        col.format = Some(if with_time { DATETIME_FORMAT } else { DATE_FORMAT }.to_owned());
    }
}

// ---- JSON files for JavaScript -----------------------------------------------------------------------------

/// Milliseconds since 1970 of a serial.
fn ms_from_serial(serial: f64) -> i64 {
    unix_micros_from_serial(serial).div_euclid(1000)
}

fn write_num(w: &mut impl std::io::Write, n: f64) -> std::io::Result<()> {
    if n.is_finite() {
        if n.fract() == 0.0 && n.abs() < 9.0e15 {
            write!(w, "{}", n as i64)
        } else {
            write!(w, "{n}")
        }
    } else {
        w.write_all(b"null")
    }
}

fn write_str(w: &mut impl std::io::Write, s: &str) -> std::io::Result<()> {
    serde_json::to_writer(&mut *w, s).map_err(std::io::Error::other)
}

/// A table as one array per column.
fn write_table_json(w: &mut impl std::io::Write, table: &Table) -> std::io::Result<()> {
    w.write_all(b"{\"columns\":")?;
    serde_json::to_writer(&mut *w, &table.columns).map_err(std::io::Error::other)?;
    w.write_all(b",\"types\":")?;
    let types: Vec<&str> = table.types.iter().map(|t| t.name()).collect();
    serde_json::to_writer(&mut *w, &types).map_err(std::io::Error::other)?;
    write!(w, ",\"rows\":{},\"data\":[", table.rows())?;
    for (ci, array) in table.batch.columns().iter().enumerate() {
        if ci > 0 {
            w.write_all(b",")?;
        }
        w.write_all(b"[")?;
        for i in 0..array.len() {
            if i > 0 {
                w.write_all(b",")?;
            }
            if array.is_null(i) {
                w.write_all(b"null")?;
                continue;
            }
            match array.data_type() {
                DataType::Int64 => write!(w, "{}", array.as_primitive::<Int64Type>().value(i))?,
                DataType::Float64 => write_num(w, array.as_primitive::<Float64Type>().value(i))?,
                DataType::Boolean => w.write_all(if array.as_boolean().value(i) { b"true" } else { b"false" })?,
                DataType::Utf8 => write_str(w, array.as_string::<i32>().value(i))?,
                DataType::Date32 => write!(w, "{}", array.as_primitive::<Date32Type>().value(i) as i64 * 86_400_000)?,
                DataType::Timestamp(TimeUnit::Microsecond, _) => {
                    write!(w, "{}", array.as_primitive::<TimestampMicrosecondType>().value(i).div_euclid(1000))?
                }
                DataType::Time64(TimeUnit::Microsecond) => {
                    let us = array.as_primitive::<Time64MicrosecondType>().value(i);
                    write_str(w, &iso_time(us as f64 / 86_400_000_000.0))?
                }
                _ => w.write_all(b"null")?,
            }
        }
        w.write_all(b"]")?;
    }
    w.write_all(b"]}")
}

/// A result (a query's, for JavaScript's `calc.sql()`) as one array per column, dates as milliseconds.
fn write_columns_json(w: &mut impl std::io::Write, columns: &[OutColumn], rows: u64) -> std::io::Result<()> {
    let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    w.write_all(b"{\"columns\":")?;
    serde_json::to_writer(&mut *w, &names).map_err(std::io::Error::other)?;
    let types: Vec<&str> = columns
        .iter()
        .map(|c| match c.format.as_deref() {
            Some(DATE_FORMAT) => "date",
            Some(DATETIME_FORMAT) => "timestamp",
            Some(TIME_FORMAT) => "time",
            _ => "value",
        })
        .collect();
    w.write_all(b",\"types\":")?;
    serde_json::to_writer(&mut *w, &types).map_err(std::io::Error::other)?;
    write!(w, ",\"rows\":{rows},\"data\":[")?;
    for (ci, col) in columns.iter().enumerate() {
        if ci > 0 {
            w.write_all(b",")?;
        }
        w.write_all(b"[")?;
        let kind = types[ci];
        for (i, v) in col.values.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            match v {
                Value::Empty => w.write_all(b"null")?,
                Value::Number(n) => match kind {
                    "date" | "timestamp" => write!(w, "{}", ms_from_serial(*n))?,
                    "time" => write_str(w, &iso_time(*n))?,
                    _ => write_num(w, *n)?,
                },
                Value::Text(s) => write_str(w, s)?,
                Value::Bool(b) => w.write_all(if *b { b"true" } else { b"false" })?,
                Value::Error(e) => write_str(w, e.as_str())?,
            }
        }
        w.write_all(b"]")?;
    }
    w.write_all(b"]}")
}

// ---- Reading a JSON result -----------------------------------------------------------------------------------

/// Reads `{"columns": [...], "rows": [[...], ...]}` straight into columns of values, keeping `keep` rows (all
/// when None, up to `max_cells`).
fn read_json_result(path: &Path, keep: Option<usize>, max_cells: u64) -> Result<(Vec<OutColumn>, u64), RunError> {
    let file = File::open(path).map_err(|e| RunError::internal(format!("opening the result: {e}")))?;
    let mut de = serde_json::Deserializer::from_reader(BufReader::with_capacity(1 << 20, file));
    let mut acc = Accumulator { names: Vec::new(), values: Vec::new(), dates: Vec::new(), times: Vec::new(), rows: 0, keep, max_cells, too_big: false };
    de.deserialize_map(ResultVisitor { acc: &mut acc }).map_err(|e| {
        if acc.too_big {
            crate::values::too_many_cells(max_cells)
        } else {
            RunError::internal(format!("reading the result the code gave: {e}"))
        }
    })?;
    let width = acc.names.len().max(acc.values.len());
    let mut columns = Vec::with_capacity(width);
    for i in 0..width {
        let name = acc.names.get(i).cloned().unwrap_or_else(|| format!("column{}", i + 1));
        let mut values = acc.values.get_mut(i).map(std::mem::take).unwrap_or_default();
        let kept = acc.kept();
        values.resize(kept, Value::Empty);
        let dates = acc.dates.get(i).copied().unwrap_or(0);
        let non_empty = values.iter().filter(|v| !v.is_empty()).count();
        let format = (dates > 0 && dates == non_empty).then(|| {
            if acc.times.get(i).copied().unwrap_or(false) { DATETIME_FORMAT } else { DATE_FORMAT }.to_owned()
        });
        columns.push(OutColumn { name, format, values });
    }
    let names = crate::table::column_names(&columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    for (c, n) in columns.iter_mut().zip(names) {
        c.name = n;
    }
    Ok((columns, acc.rows))
}

struct Accumulator {
    names: Vec<String>,
    values: Vec<Vec<Value>>,
    /// Per column: how many of its kept values were dates, and whether any had a time of day.
    dates: Vec<usize>,
    times: Vec<bool>,
    rows: u64,
    keep: Option<usize>,
    max_cells: u64,
    too_big: bool,
}

impl Accumulator {
    fn kept(&self) -> usize {
        match self.keep {
            Some(k) => (self.rows as usize).min(k),
            None => self.rows as usize,
        }
    }

    fn keeping(&self) -> bool {
        self.keep.is_none_or(|k| (self.rows as usize) < k)
    }

    fn put(&mut self, col: usize, v: Value, is_date: bool) {
        while self.values.len() <= col {
            let mut c = Vec::new();
            c.resize(self.rows as usize, Value::Empty);
            self.values.push(c);
            self.dates.push(0);
            self.times.push(false);
        }
        let column = &mut self.values[col];
        column.resize(self.rows as usize, Value::Empty);
        if is_date {
            self.dates[col] += 1;
            if let Value::Number(n) = v
                && n.fract() != 0.0
            {
                self.times[col] = true;
            }
        }
        column.push(v);
    }
}

struct ResultVisitor<'a> {
    acc: &'a mut Accumulator,
}

impl<'de> Visitor<'de> for ResultVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a result object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "columns" => {
                    let names: Vec<Json> = map.next_value()?;
                    self.acc.names = names.into_iter().map(|n| match n {
                        Json::String(s) => s,
                        Json::Null => String::new(),
                        other => other.to_string(),
                    }).collect();
                }
                "rows" => map.next_value_seed(RowsSeed { acc: &mut *self.acc })?,
                _ => {
                    map.next_value::<de::IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct RowsSeed<'a> {
    acc: &'a mut Accumulator,
}

impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for RowsSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        loop {
            let keep = self.acc.keeping();
            if keep && self.acc.keep.is_none() {
                let width = self.acc.names.len().max(self.acc.values.len()).max(1) as u64;
                if (self.acc.rows + 1).saturating_mul(width) > self.acc.max_cells {
                    self.acc.too_big = true;
                    return Err(de::Error::custom("too many cells"));
                }
            }
            let more = if keep { seq.next_element_seed(RowSeed { acc: &mut *self.acc })? } else { seq.next_element::<de::IgnoredAny>()?.map(|_| ()) };
            if more.is_none() {
                return Ok(());
            }
            self.acc.rows += 1;
        }
    }
}

struct RowSeed<'a> {
    acc: &'a mut Accumulator,
}

impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for RowSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a row")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut col = 0;
        while let Some(cell) = seq.next_element::<Json>()? {
            let (v, is_date) = value_from_json(&cell);
            self.acc.put(col, v, is_date);
            col += 1;
        }
        Ok(())
    }
}

/// A text for byte counts in messages (`1,048,576 bytes`).
#[allow(dead_code)]
pub(crate) fn bytes_exact(n: u64) -> String {
    format!("{} bytes", group_digits(n))
}
