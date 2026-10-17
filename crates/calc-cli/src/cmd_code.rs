//! Code: `run` SQL, Python or JavaScript on a sheet (answer only, into cells, or into a new tab that remembers
//! its code and can keep itself updated), and `rerun` a tab made by code.

use std::io::{IsTerminal, Read as _, Write as _};

use serde_json::Value as Json;
use silicon_calc_client::a1::{self, RangeKind, RangeRef};
use silicon_calc_client::{Language, RunAnswer, RunInto, RunRequest, TabRef};

use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{Table, json_is_number, json_text, plural, thousands};
use crate::resolve::{self, Target};

pub struct RunArgs {
    pub language: String,
    pub sheet: String,
    pub file: Option<String>,
    pub code: Option<String>,
    pub into_new_tab: Option<String>,
    pub into: Option<String>,
    pub keep_updated: bool,
    pub copy: Option<String>,
    pub max_rows: Option<usize>,
    pub format: Option<String>,
}

pub fn parse_language(s: &str) -> CliResult<Language> {
    Language::parse(s).ok_or_else(|| {
        CliError::usage(format!("\"{s}\" is not a language Silicon Calc runs."), "Use sql, python (py) or javascript (js).")
    })
}

/// The code: `-c`, a file, or stdin (`-`, or piped in with neither).
pub fn code_source(code: Option<&str>, file: Option<&str>) -> CliResult<String> {
    let text = match (code, file) {
        (Some(_), Some(f)) => {
            return Err(CliError::usage(format!("Both -c and the file {f} give the code."), "Give the code one way: -c CODE, a file, or - for stdin."));
        }
        (Some(c), None) => c.to_owned(),
        (None, Some("-")) => read_stdin()?,
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| CliError::file(format!("Can't read the code in {path}: {e}.")))?,
        (None, None) => {
            if std::io::stdin().is_terminal() {
                return Err(CliError::usage(
                    "No code was given.",
                    "Pass it with -c 'select …', as a file (calc run python SHEET analysis.py), or on stdin.",
                ));
            }
            read_stdin()?
        }
    };
    if text.trim().is_empty() {
        return Err(CliError::usage("The code is empty.", "Give the code to run."));
    }
    Ok(text)
}

fn read_stdin() -> CliResult<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).map_err(|e| CliError::file(format!("Can't read the code from stdin: {e}.")))?;
    Ok(s)
}

/// Where the result goes: `--into-new-tab NAME` or `--into 'Tab!A1'`.
fn destination(target: &Target, new_tab: Option<&str>, into: Option<&str>) -> CliResult<Option<RunInto>> {
    match (new_tab, into) {
        (Some(_), Some(_)) => Err(CliError::usage("--into-new-tab and --into both say where the result goes.", "Pick one.")),
        (Some(name), None) => {
            let name = name.trim();
            if name.is_empty() {
                return Err(CliError::usage("The new tab's name is empty.", "Name it, like --into-new-tab \"By region\"."));
            }
            if target.tabs.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
                return Err(CliError::usage_code(
                    "conflict",
                    format!("{} already has a tab named \"{name}\".", target.place()),
                    format!("Pick another name, write into it with --into '{}!A1', or rerun it with calc rerun.", a1::quote_tab(name)),
                ));
            }
            Ok(Some(RunInto::NewTab { new_tab: name.to_owned() }))
        }
        (None, Some(place)) => {
            let (tab, rest) = a1::split_tab(place);
            let cell = RangeRef::parse(rest).filter(|r| r.kind == RangeKind::Cells).ok_or_else(|| {
                CliError::usage(format!("\"{place}\" is not a cell to write the result at."), "Name the top-left cell, like --into 'Sales!E1' or --into E1.")
            })?;
            let t = target.tab(tab.as_deref())?;
            Ok(Some(RunInto::Cells { tab: TabRef::Id(t.id), at: cell.start().to_string() }))
        }
        (None, None) => Ok(None),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Table,
    Csv,
    Tsv,
    Json,
}

fn shape(s: Option<&str>, json: bool) -> CliResult<Shape> {
    if json {
        return Ok(Shape::Json);
    }
    Ok(match s.map(|s| s.trim().to_ascii_lowercase()) {
        None => Shape::Table,
        Some(f) => match f.as_str() {
            "table" | "text" => Shape::Table,
            "csv" => Shape::Csv,
            "tsv" => Shape::Tsv,
            "json" => Shape::Json,
            other => return Err(CliError::usage(format!("\"{other}\" is not a format run prints."), "Use table, csv, tsv or json.")),
        },
    })
}

/// The result as an aligned table, at most `max_rows` rows.
pub fn render_result(answer: &RunAnswer, max_rows: usize) -> String {
    if answer.columns.is_empty() {
        return String::new();
    }
    let mut t = Table::raw(&answer.columns);
    let numeric: Vec<bool> = (0..answer.columns.len())
        .map(|i| {
            let mut any = false;
            for row in &answer.rows {
                match row.get(i) {
                    Some(Json::Null) | None => {}
                    Some(v) if json_is_number(v) => any = true,
                    Some(_) => return false,
                }
            }
            any
        })
        .collect();
    for (i, n) in numeric.iter().enumerate() {
        if *n {
            t = t.right(i);
        }
    }
    for row in answer.rows.iter().take(max_rows) {
        t.row(row.iter().map(json_text).collect());
    }
    t.render()
}

fn write_delimited(answer: &RunAnswer, delimiter: u8) -> CliResult<()> {
    let mut w = csv::WriterBuilder::new().delimiter(delimiter).from_writer(std::io::stdout().lock());
    let err = |e: csv::Error| CliError::file(format!("Can't write the output: {e}."));
    w.write_record(&answer.columns).map_err(err)?;
    for row in &answer.rows {
        w.write_record(row.iter().map(json_text)).map_err(err)?;
    }
    w.flush().map_err(|e| CliError::file(format!("Can't write the output: {e}.")))
}

/// Prints what a run gave: what the code printed, the result, and where it was written.
fn print_answer(ctx: &Ctx, target: &Target, answer: &RunAnswer, shape: Shape, max_rows: usize) -> CliResult<()> {
    if shape == Shape::Json {
        ctx.emit(answer, String::new);
        return Ok(());
    }
    if !answer.stdout.is_empty() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(answer.stdout.as_bytes());
        if !answer.stdout.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
    match shape {
        Shape::Csv => write_delimited(answer, b',')?,
        Shape::Tsv => write_delimited(answer, b'\t')?,
        _ => {
            let table = render_result(answer, max_rows);
            if !table.is_empty() {
                print!("{table}");
                let _ = std::io::stdout().flush();
            }
        }
    }
    if !answer.stderr.trim().is_empty() {
        eprintln!("{}", answer.stderr.trim_end());
    }
    let shown = answer.rows.len().min(if shape == Shape::Table { max_rows } else { usize::MAX });
    let mut note = if answer.columns.is_empty() {
        format!("Ran in {} ms; no result table.", thousands(answer.duration_ms))
    } else {
        format!("{} in {} ms.", plural(answer.row_count, "row", "rows"), thousands(answer.duration_ms))
    };
    if (shown as u64) < answer.row_count && !answer.columns.is_empty() {
        note.push_str(&format!(
            " Showing {}; write them all into a tab with --into-new-tab NAME, or print them with --format csv{}.",
            thousands(shown as u64),
            if answer.truncated { " (the answer itself holds the first 1,000)" } else { "" }
        ));
    }
    if let (Some(v), Some(tab)) = (answer.version, &answer.tab) {
        let code = if tab.code.is_some() { ", remembering the code that made it" } else { "" };
        note.push_str(&format!(" Wrote the result into \"{}\"{code} in {} (version {v}).", tab.name, target.place()));
        if let Some(c) = &tab.code
            && c.keep_updated
        {
            note.push_str(" It reruns whenever a tab it reads changes.");
        }
    } else if let Some(v) = answer.version {
        note.push_str(&format!(" Wrote the result (version {v})."));
    }
    ctx.note(note);
    Ok(())
}

/// A failed run says what the code printed and where it stopped.
fn explain_failure(ctx: &Ctx, err: CliError) -> CliError {
    if err.code == "code_failed" && !ctx.json {
        if let Some(stderr) = err.details.as_ref().and_then(|d| d.get("stderr")).and_then(Json::as_str).filter(|s| !s.trim().is_empty()) {
            let lines: Vec<&str> = stderr.trim_end().lines().collect();
            let start = lines.len().saturating_sub(40);
            if start > 0 {
                eprintln!("… ({} earlier lines)", start);
            }
            eprintln!("{}", lines[start..].join("\n"));
        }
        if let Some(line) = err.details.as_ref().and_then(|d| d.get("line")).and_then(Json::as_u64) {
            eprintln!("(line {line} of the code)");
        }
    }
    err
}

pub async fn run(ctx: &Ctx, a: RunArgs) -> CliResult<()> {
    let language = parse_language(&a.language)?;
    let shape = shape(a.format.as_deref(), ctx.json)?;
    let code = code_source(a.code.as_deref(), a.file.as_deref())?;
    if a.keep_updated && a.into_new_tab.is_none() {
        return Err(CliError::usage(
            "--keep-updated works with --into-new-tab: only a tab made by code can rerun itself.",
            "Add --into-new-tab NAME.",
        ));
    }
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let into = destination(&target, a.into_new_tab.as_deref(), a.into.as_deref())?;
    let request = RunRequest { language, code, into, keep_updated: a.keep_updated };
    let answer = ctx.client.run(&target.line, &request).await.map_err(|e| explain_failure(ctx, e.into()))?;
    print_answer(ctx, &target, &answer, shape, a.max_rows.unwrap_or(100).max(1))
}

pub async fn rerun(ctx: &Ctx, sheet: &str, copy: Option<&str>, tab: &str, format: Option<&str>, max_rows: Option<usize>) -> CliResult<()> {
    let shape = shape(format, ctx.json)?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    if t.code.is_none() {
        return Err(CliError::new(
            "not_code",
            format!("The tab \"{}\" wasn't made by code, so there is nothing to rerun.", t.name),
            "calc tabs ls SHEET shows which tabs were made by code.",
        ));
    }
    let answer = ctx.client.rerun(&target.line, &TabRef::Id(t.id)).await.map_err(|e| explain_failure(ctx, e.into()))?;
    print_answer(ctx, &target, &answer, shape, max_rows.unwrap_or(100).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn languages() {
        assert_eq!(parse_language("SQL").unwrap(), Language::Sql);
        assert_eq!(parse_language("py").unwrap(), Language::Python);
        assert_eq!(parse_language("js").unwrap(), Language::Javascript);
        assert_eq!(parse_language("ruby").unwrap_err().exit, 2);
    }

    #[test]
    fn code_comes_one_way() {
        assert_eq!(code_source(Some("select 1"), None).unwrap(), "select 1");
        assert_eq!(code_source(Some("select 1"), Some("x.sql")).unwrap_err().exit, 2);
        assert_eq!(code_source(Some("  "), None).unwrap_err().exit, 2);
        assert_eq!(code_source(None, Some("/no/such/file.py")).unwrap_err().exit, 2);
    }

    #[test]
    fn results_print_as_tables() {
        let answer = RunAnswer {
            columns: vec!["region".into(), "sum(amount)".into()],
            rows: vec![vec![json!("EU"), json!(1200.5)], vec![json!("US"), json!(900)], vec![json!(null), json!(null)]],
            row_count: 3,
            ..Default::default()
        };
        let out = render_result(&answer, 10);
        assert_eq!(out, "region  sum(amount)\nEU           1200.5\nUS              900\n\n");
        assert_eq!(render_result(&answer, 1).lines().count(), 2);
        assert_eq!(render_result(&RunAnswer::default(), 10), "");
    }
}
