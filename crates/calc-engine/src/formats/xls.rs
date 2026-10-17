//! Excel 97-2003 workbooks (`.xls`), read with calamine: every worksheet in order with its values (dates and
//! times as dates), the formulas calamine can decode (with the values they last had), merged cells, hidden tabs
//! and defined names. calamine doesn't give styles, sizes or charts of `.xls` files, so those are named in the
//! warnings. Writing `.xls` is done by the code runner, which converts the `.xlsx` the engine writes.

use std::io::Cursor;

use calamine::{Data, Reader, SheetType, SheetVisible, Xls, XlsOptions};

use crate::{
    addr::{MAX_COLS, MAX_ROWS, Rect, quote_tab},
    formats::{FormatError, ImportOptions, Imported, IndexNames, TabInfo, Warnings, plural},
    formula::parse::{parse_formula, to_a1},
    model::DefinedName,
    ops::{ColumnContent, TabContent},
    style::Style,
    value::{ErrorKind, Value},
};

const FORMAT: &str = "Excel 97-2003";
/// Days between the 1904 and 1900 date systems.
const DAYS_1904: f64 = 1462.0;

fn error_of(e: &calamine::CellErrorType) -> ErrorKind {
    use calamine::CellErrorType as E;
    match e {
        E::Div0 => ErrorKind::Div0,
        E::NA => ErrorKind::NA,
        E::Name => ErrorKind::Name,
        E::Null => ErrorKind::Null,
        E::Num => ErrorKind::Num,
        E::Ref => ErrorKind::Ref,
        E::Value => ErrorKind::Value,
        E::GettingData => ErrorKind::NA,
    }
}

/// calamine writes tab names in references without quotes (`My tab!A1`); quotes them where they need it,
/// outside text literals, longest names first so `Sales 2025` wins over `Sales`.
pub(crate) fn quote_tab_names(formula: &str, tabs: &[String]) -> String {
    let mut names: Vec<&String> = tabs.iter().filter(|t| quote_tab(t) != **t).collect();
    if names.is_empty() {
        return formula.to_owned();
    }
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut out = String::with_capacity(formula.len() + 8);
    let mut in_str = false;
    let mut i = 0;
    let b = formula.as_bytes();
    'outer: while i < b.len() {
        let c = b[i];
        if c == b'"' {
            in_str = !in_str;
        }
        if !in_str && c != b'\'' {
            let prev_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.' || b[i - 1] == b'\'');
            if prev_ok {
                for n in &names {
                    let end = i + n.len();
                    if formula.get(i..end).is_some_and(|s| s.eq_ignore_ascii_case(n)) && b.get(end) == Some(&b'!') {
                        out.push_str(&quote_tab(n));
                        i = end;
                        continue 'outer;
                    }
                }
            }
        }
        let ch = formula[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub(crate) fn import(bytes: &[u8], _opts: &ImportOptions) -> Result<Imported, FormatError> {
    let mut book = Xls::new_with_options(Cursor::new(bytes), XlsOptions::default())
        .map_err(|e| FormatError::read(FORMAT, format!("it can't be opened ({e})")))?;
    let date1904 = book.has_1904_epoch();
    let sheets = book.sheets_metadata().to_vec();
    let mut warnings = Warnings::default();
    let mut kept: Vec<(String, bool)> = Vec::new();
    for s in &sheets {
        match s.typ {
            SheetType::WorkSheet => kept.push((s.name.clone(), !matches!(s.visible, SheetVisible::Visible))),
            SheetType::ChartSheet => warnings.say(format!("\"{}\" is a chart sheet, which a sheet can't hold, so it was left out.", s.name)),
            _ => warnings.say(format!("\"{}\" is a macro or dialog sheet, so it was left out.", s.name)),
        }
    }
    if kept.is_empty() {
        return Err(FormatError::read(FORMAT, "it has no worksheets"));
    }
    let tab_names: Vec<String> = kept.iter().map(|k| k.0.clone()).collect();
    let names = IndexNames(&tab_names);

    let mut tabs = Vec::with_capacity(kept.len());
    let mut tab_info = Vec::with_capacity(kept.len());
    for (name, hidden) in &kept {
        let range = book.worksheet_range(name).map_err(|e| FormatError::read(FORMAT, format!("the worksheet \"{name}\" can't be read ({e})")))?;
        let formulas = book.worksheet_formula(name).unwrap_or_default();
        let merges = book.merge_cells_by_sheet_name(name).unwrap_or_default();

        let mut columns: Vec<ColumnContent> = Vec::new();
        let mut styles: Vec<Style> = Vec::new();
        fn style_ix(code: &str, styles: &mut Vec<Style>) -> u32 {
            match styles.iter().position(|s| s.num_fmt.as_deref() == Some(code)) {
                Some(i) => i as u32,
                None => {
                    styles.push(Style { num_fmt: Some(code.to_owned()), ..Default::default() });
                    styles.len() as u32 - 1
                }
            }
        }
        let (mut max_r, mut max_c): (Option<u32>, Option<u32>) = (None, None);
        let mut touch = |r: u32, c: u32| {
            max_r = Some(max_r.map_or(r, |m| m.max(r)));
            max_c = Some(max_c.map_or(c, |m| m.max(c)));
        };
        for (r, c, v) in range.used_cells() {
            let (r, c) = match range.start() {
                Some((r0, c0)) => (r as u32 + r0, c as u32 + c0),
                None => (r as u32, c as u32),
            };
            if r >= MAX_ROWS || c >= MAX_COLS {
                continue;
            }
            let (value, fmt): (Value, Option<&str>) = match v {
                Data::Empty => continue,
                Data::Int(i) => (Value::Number(*i as f64), None),
                Data::Float(f) => (Value::Number(*f), None),
                Data::String(s) => (Value::from(s.as_str()), None),
                Data::Bool(b) => (Value::Bool(*b), None),
                Data::Error(e) => (Value::Error(error_of(e)), None),
                Data::DateTime(dt) => {
                    let mut n = dt.as_f64();
                    if dt.is_duration() {
                        (Value::Number(n), Some("[h]:mm:ss"))
                    } else {
                        if date1904 {
                            n += DAYS_1904;
                        }
                        let fmt = if n.fract() == 0.0 {
                            "yyyy-mm-dd"
                        } else if n < 1.0 {
                            "h:mm:ss"
                        } else {
                            "yyyy-mm-dd h:mm:ss"
                        };
                        (Value::Number(n), Some(fmt))
                    }
                }
                Data::DateTimeIso(s) => match crate::formats::json::parse_iso(s) {
                    Some((n, f)) => (Value::Number(n), Some(f)),
                    None => (Value::from(s.as_str()), None),
                },
                Data::DurationIso(s) => (Value::from(s.as_str()), None),
            };
            touch(r, c);
            if columns.len() <= c as usize {
                columns.resize_with(c as usize + 1, ColumnContent::default);
            }
            let col = &mut columns[c as usize];
            if col.values.len() < r as usize {
                col.values.resize(r as usize, Value::Empty);
            }
            if col.values.len() == r as usize {
                col.values.push(value);
            } else {
                col.values[r as usize] = value;
            }
            if let Some(f) = fmt {
                let ix = style_ix(f, &mut styles);
                col.styles.push((r, ix));
            }
        }
        for (r, c, text) in formulas.used_cells() {
            let (r, c) = match formulas.start() {
                Some((r0, c0)) => (r as u32 + r0, c as u32 + c0),
                None => (r as u32, c as u32),
            };
            if r >= MAX_ROWS || c >= MAX_COLS || text.trim().is_empty() {
                continue;
            }
            if text.starts_with("Unrecognised formula") {
                warnings.count("unreadable", || format!("{} on {name}", crate::addr::CellPos::new(r, c)));
                continue;
            }
            let text = quote_tab_names(text.trim().trim_start_matches('='), &tab_names);
            match parse_formula(&text, r, c, &names) {
                Ok(expr) => {
                    touch(r, c);
                    if columns.len() <= c as usize {
                        columns.resize_with(c as usize + 1, ColumnContent::default);
                    }
                    let col = &mut columns[c as usize];
                    if col.values.len() <= r as usize {
                        // A formula whose last value was empty still needs a slot.
                        col.values.resize(r as usize + 1, Value::Empty);
                    }
                    col.formulas.push((r, to_a1(&expr, r, c, &names)));
                }
                Err(_) => warnings.count("unreadable", || format!("{} on {name}: ={text}", crate::addr::CellPos::new(r, c))),
            }
        }
        let mut content = TabContent::default();
        for m in &merges {
            let rect = Rect::new(m.start.0, m.start.1, m.end.0, m.end.1);
            if (rect.rows() > 1 || rect.cols() > 1) && rect.r2 < MAX_ROWS && rect.c2 < MAX_COLS {
                content.merges.push(rect);
                touch(rect.r2, rect.c2);
            }
        }
        for col in columns.iter_mut() {
            col.styles.sort_by_key(|s| s.0);
            col.formulas.sort_by_key(|f| f.0);
            while col.values.last().is_some_and(Value::is_empty) {
                let last = col.values.len() as u32 - 1;
                if col.formulas.iter().any(|f| f.0 == last) {
                    break;
                }
                col.values.pop();
            }
        }
        content.rows = max_r.map_or(0, |r| r + 1);
        content.cols = max_c.map_or(0, |c| c + 1);
        if columns.len() < content.cols as usize {
            columns.resize_with(content.cols as usize, ColumnContent::default);
        }
        content.columns = columns;
        content.styles = styles;
        if *hidden {
            warnings.say(format!("\"{name}\" was hidden in the file; sheets show every tab."));
        }
        tab_info.push(TabInfo { color: None, hidden: *hidden });
        tabs.push((name.clone(), content));
    }

    let mut defined = Vec::new();
    for (name, text) in book.defined_names() {
        let n = name.trim();
        if n.is_empty() || n.starts_with("_xlnm.") || n.starts_with('\u{1}') || n.starts_with('\u{6}') || text.trim().is_empty() {
            continue;
        }
        let text = quote_tab_names(text.trim().trim_start_matches('='), &tab_names);
        match parse_formula(&text, 0, 0, &names) {
            Ok(expr) => defined.push(DefinedName { name: n.to_owned(), text: to_a1(&expr, 0, 0, &names), tab: None }),
            Err(_) => warnings.count("name", || n.to_owned()),
        }
    }
    warnings.say("Excel 97-2003 files open with their values and formulas; their cell styles, column widths and charts aren't read. Save the file as .xlsx in Excel to keep those.");
    let warnings = warnings.finish(|kind, n| match kind {
        "unreadable" => format!("{} couldn't be read, so they keep the values they last had", plural(n, "formula", "formulas")),
        "name" => format!("{} couldn't be read and were left out", plural(n, "defined name", "defined names")),
        other => format!("{n} × {other}"),
    });
    Ok(Imported { tabs, names: defined, warnings, tab_info, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_tab_names_calamine_leaves_bare() {
        let tabs = vec!["Sales".to_owned(), "My data".to_owned(), "My data 2".to_owned()];
        assert_eq!(quote_tab_names("SUM(My data!A1:A3)+Sales!B2", &tabs), "SUM('My data'!A1:A3)+Sales!B2");
        assert_eq!(quote_tab_names("My data 2!A1&\"My data!x\"", &tabs), "'My data 2'!A1&\"My data!x\"");
        assert_eq!(quote_tab_names("'My data'!A1", &tabs), "'My data'!A1");
    }

    #[test]
    fn not_an_xls_file() {
        assert!(matches!(import(b"not a workbook", &ImportOptions::default()), Err(FormatError::Read { .. })));
    }
}
