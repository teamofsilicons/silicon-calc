//! Making an Excel file's formula text something the engine reads and keeps working: structured references to
//! tables (`Table1[Amount]`, `Table1[[#This Row],[Price]]`) become plain ranges, `_xlfn.ANCHORARRAY(A1)` becomes
//! `A1#`, a formula that is only an error literal becomes that error value, and formulas that reach into other
//! files are recognised (they keep their last values).

use std::sync::Arc;

use quick_xml::events::Event;

use crate::{
    addr::{Rect, col_name, quote_tab},
    formats::{
        IndexNames,
        xml::{attr, name_of, reader},
    },
    formula::{
        ast::Expr,
        parse::{parse_formula, to_a1},
    },
    value::{ErrorKind, Value},
};

/// A table (Excel "ListObject"): its area and column names.
#[derive(Clone, Debug)]
pub(crate) struct TableDef {
    pub name: String,
    pub sheet: usize,
    pub rect: Rect,
    pub header_rows: u32,
    pub totals_rows: u32,
    pub columns: Vec<String>,
}

pub(crate) fn read_table(xml: &[u8], sheet: usize) -> Option<TableDef> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut table: Option<TableDef> = None;
    loop {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match name_of(&e) {
                b"table" => {
                    let name = attr(&e, b"displayName").or_else(|| attr(&e, b"name"))?;
                    let rect = Rect::parse(&attr(&e, b"ref")?, crate::addr::MAX_ROWS, crate::addr::MAX_COLS)?;
                    let header_rows = attr(&e, b"headerRowCount").and_then(|v| v.parse().ok()).unwrap_or(1);
                    let totals_rows = attr(&e, b"totalsRowCount").and_then(|v| v.parse().ok()).unwrap_or(0);
                    table = Some(TableDef { name, sheet, rect, header_rows, totals_rows, columns: Vec::new() });
                }
                b"tableColumn" => {
                    if let (Some(t), Some(n)) = (table.as_mut(), attr(&e, b"name")) {
                        t.columns.push(super::decode_escapes(&n).into_owned());
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    table
}

/// What a formula from the file becomes.
pub(crate) enum Prepared {
    /// Formula text the engine reads (without `=`), and its tree when asked for.
    Formula { text: String, expr: Option<Arc<Expr>> },
    /// The formula was only a value (`=#N/A`), which is what an error constant becomes when written out.
    Constant(Value),
    /// The formula can't be kept: `"external"` (other files) or `"unreadable"`.
    Failed(&'static str),
}

/// Where a formula is, for converting it.
pub(crate) struct Place<'a> {
    pub row: u32,
    pub col: u32,
    pub sheet: usize,
    pub names: &'a IndexNames<'a>,
    pub tab_names: &'a [String],
    pub tables: &'a [TableDef],
}

pub(crate) fn prepare(text: &str, at: &Place<'_>, want_expr: bool) -> Prepared {
    let t = text.trim();
    let t = t.strip_prefix('=').unwrap_or(t).trim();
    if t.is_empty() {
        return Prepared::Failed("unreadable");
    }
    if let Some(e) = ErrorKind::parse(t) {
        return Prepared::Constant(Value::Error(e));
    }
    let mut changed = false;
    let mut s = t.to_owned();
    if s.contains('[') || mentions_table(&s, at.tables) {
        match convert_structured(&s, at) {
            Ok(Some(converted)) => {
                s = converted;
                changed = true;
            }
            Ok(None) => {}
            Err(Problem::External) => return Prepared::Failed("external"),
            Err(Problem::Unknown) => return Prepared::Failed("unreadable"),
        }
    }
    let expr = match parse_formula(&s, at.row, at.col, at.names) {
        Ok(e) => e,
        Err(_) => return Prepared::Failed("unreadable"),
    };
    let expr = if s.to_ascii_uppercase().contains("ANCHORARRAY") {
        changed = true;
        anchor_to_spill(&expr)
    } else {
        expr
    };
    let text = if changed { to_a1(&expr, at.row, at.col, at.names) } else { s };
    Prepared::Formula { text, expr: want_expr.then(|| Arc::new(expr)) }
}

/// Whether a table's name appears in the text (a bare `Table1` means the table's data).
fn mentions_table(text: &str, tables: &[TableDef]) -> bool {
    if tables.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    tables.iter().any(|t| lower.contains(&t.name.to_lowercase()))
}

/// `ANCHORARRAY(A1)` (how files spell `A1#`) back into a spill reference.
pub(crate) fn anchor_to_spill(e: &Expr) -> Expr {
    match e {
        Expr::Call(name, args) if name.eq_ignore_ascii_case("ANCHORARRAY") && args.len() == 1 => match &args[0] {
            Expr::Cell(c) => Expr::Spill(*c),
            other => Expr::Call(name.clone(), vec![anchor_to_spill(other)]),
        },
        Expr::Call(name, args) => Expr::Call(name.clone(), args.iter().map(anchor_to_spill).collect()),
        Expr::Unary(op, x) => Expr::Unary(*op, Box::new(anchor_to_spill(x))),
        Expr::Binary(op, a, b) => Expr::Binary(*op, Box::new(anchor_to_spill(a)), Box::new(anchor_to_spill(b))),
        Expr::Paren(x) => Expr::Paren(Box::new(anchor_to_spill(x))),
        Expr::Array(rows) => Expr::Array(rows.iter().map(|r| r.iter().map(anchor_to_spill).collect()).collect()),
        other => other.clone(),
    }
}

/// Whether formula text refers to another workbook (`[1]Sheet1!A1`, `'C:\x\[Book.xlsx]Sheet1'!A1`).
pub(crate) fn is_external(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        match b[i] {
            b'"' => in_str = !in_str,
            b'[' if !in_str => {
                let close = b[i + 1..].iter().position(|&c| c == b']').map(|p| i + 1 + p);
                if let Some(end) = close {
                    let inner = &text[i + 1..end];
                    let after = b.get(end + 1).copied();
                    if !inner.is_empty()
                        && (inner.bytes().all(|c| c.is_ascii_digit())
                            || inner.to_ascii_lowercase().contains(".xl")
                            || inner.to_ascii_lowercase().contains(".od"))
                        && after.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'!' || c == b'\'')
                    {
                        return true;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

enum Problem {
    External,
    Unknown,
}

/// The text of one bracket group starting at `chars[start] == '['`, with `'` escapes kept, and the index after it.
fn bracket_group(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut depth = 0usize;
    let mut i = start;
    let mut out = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && i + 1 < chars.len() {
            if depth > 0 {
                out.push(c);
                out.push(chars[i + 1]);
            }
            i += 2;
            continue;
        }
        if c == '[' {
            depth += 1;
            if depth > 1 {
                out.push(c);
            }
        } else if c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some((out, i + 1));
            }
            out.push(c);
        } else if depth > 0 {
            out.push(c);
        }
        i += 1;
    }
    None
}

fn unescape_name(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

/// One structured reference resolved to A1 text, from the spec inside the table's brackets.
fn resolve_spec(table: &TableDef, spec: &str, at: &Place<'_>) -> Result<String, Problem> {
    #[derive(Default)]
    struct Want {
        all: bool,
        headers: bool,
        data: bool,
        totals: bool,
        this_row: bool,
        cols: Vec<(usize, usize)>,
    }
    let mut want = Want::default();
    let col_index = |name: &str| -> Result<usize, Problem> {
        let n = unescape_name(name);
        table.columns.iter().position(|c| c.eq_ignore_ascii_case(&n)).ok_or(Problem::Unknown)
    };
    let item = |text: &str, want: &mut Want| -> Result<(), Problem> {
        let t = text.trim();
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "#all" => want.all = true,
            "#data" => want.data = true,
            "#headers" => want.headers = true,
            "#totals" => want.totals = true,
            "#this row" => want.this_row = true,
            "" => {}
            _ => {
                let c = col_index(t)?;
                want.cols.push((c, c));
            }
        }
        Ok(())
    };
    let chars: Vec<char> = spec.chars().collect();
    if !spec.contains('[') {
        // `Table1[Amount]`, `Table1[#All]`, `Table1[@Amount]`, `Table1[]`.
        let t = spec.trim();
        if let Some(rest) = t.strip_prefix('@') {
            want.this_row = true;
            item(rest, &mut want)?;
        } else {
            item(t, &mut want)?;
        }
    } else {
        // `[[#This Row],[Amount]]`, `[[Amount]:[Qty]]`, `[@[Unit Price]]`, `[@[A]:[B]]`.
        let mut i = 0;
        let mut pending_range: Option<usize> = None;
        while i < chars.len() {
            match chars[i] {
                '@' => {
                    want.this_row = true;
                    i += 1;
                }
                '[' => {
                    let (inner, next) = bracket_group(&chars, i).ok_or(Problem::Unknown)?;
                    i = next;
                    let is_range_end = pending_range.is_some();
                    if let Some(start) = pending_range.take() {
                        let end = col_index(&inner)?;
                        want.cols.push((start.min(end), start.max(end)));
                    } else if chars.get(i).is_some_and(|&c| c == ':') {
                        pending_range = Some(col_index(&inner)?);
                        i += 1;
                        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                            i += 1;
                        }
                    } else if !is_range_end {
                        item(&inner, &mut want)?;
                    }
                }
                c if c == ',' || c.is_whitespace() => i += 1,
                _ => {
                    // A bare column name inside `@…`.
                    let start = i;
                    while i < chars.len() && chars[i] != ',' && chars[i] != '[' {
                        i += 1;
                    }
                    let name: String = chars[start..i].iter().collect();
                    item(&name, &mut want)?;
                }
            }
        }
        if pending_range.is_some() {
            return Err(Problem::Unknown);
        }
    }
    let r = &table.rect;
    let first_data = r.r1 + table.header_rows;
    let last_data = r.r2.saturating_sub(table.totals_rows);
    fn add(rows: &mut Option<(u32, u32)>, a: u32, b: u32) {
        *rows = Some(match *rows {
            Some((x, y)) => (x.min(a), y.max(b)),
            None => (a, b),
        })
    }
    let mut rows: Option<(u32, u32)> = None;
    if want.all {
        add(&mut rows, r.r1, r.r2);
    }
    if want.headers {
        if table.header_rows == 0 {
            return Ok("#REF!".to_owned());
        }
        add(&mut rows, r.r1, r.r1 + table.header_rows - 1);
    }
    if want.totals {
        if table.totals_rows == 0 {
            return Ok("#REF!".to_owned());
        }
        add(&mut rows, r.r2 + 1 - table.totals_rows, r.r2);
    }
    if want.data || (rows.is_none() && !want.this_row) {
        add(&mut rows, first_data, last_data.max(first_data));
    }
    let (c1, c2) = if want.cols.is_empty() {
        (r.c1, r.c1 + table.columns.len().max(1) as u32 - 1)
    } else {
        let lo = want.cols.iter().map(|c| c.0).min().unwrap_or(0) as u32;
        let hi = want.cols.iter().map(|c| c.1).max().unwrap_or(0) as u32;
        (r.c1 + lo, r.c1 + hi)
    };
    let prefix = if table.sheet == at.sheet {
        String::new()
    } else {
        format!("{}!", quote_tab(at.tab_names.get(table.sheet).map(String::as_str).unwrap_or("")))
    };
    let (a, b) = (col_name(c1), col_name(c2));
    Ok(if want.this_row {
        let row = at.row + 1;
        if c1 == c2 { format!("{prefix}${a}{row}") } else { format!("{prefix}${a}{row}:${b}{row}") }
    } else {
        let (r1, r2) = rows.unwrap_or((first_data, last_data));
        if c1 == c2 && r1 == r2 {
            format!("{prefix}${a}${}", r1 + 1)
        } else {
            format!("{prefix}${a}${}:${b}${}", r1 + 1, r2 + 1)
        }
    })
}

/// Replaces every structured reference in `text` with the range it means. `Ok(None)` when there are none.
fn convert_structured(text: &str, at: &Place<'_>) -> Result<Option<String>, Problem> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let mut changed = false;
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\' || c == '?';
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == '"' {
                        if chars.get(i + 1) == Some(&'"') {
                            out.push('"');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '\'' => {
                // A quoted tab name; brackets inside mean another file.
                let start = i;
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\'' {
                        if chars.get(i + 1) == Some(&'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let quoted: String = chars[start..i].iter().collect();
                if quoted.contains('[') {
                    return Err(Problem::External);
                }
                out.push_str(&quoted);
            }
            c if ident(c) && (i == 0 || !ident(chars[i - 1])) => {
                let start = i;
                while i < chars.len() && ident(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let table = at.tables.iter().find(|t| t.name.eq_ignore_ascii_case(&word));
                match (table, chars.get(i)) {
                    (Some(t), Some('[')) => {
                        let (spec, next) = bracket_group(&chars, i).ok_or(Problem::Unknown)?;
                        out.push_str(&resolve_spec(t, &spec, at)?);
                        i = next;
                        changed = true;
                    }
                    (Some(t), _) if !matches!(chars.get(i), Some('(') | Some('!')) => {
                        // A table's name alone means its data.
                        out.push_str(&resolve_spec(t, "", at)?);
                        changed = true;
                    }
                    _ => out.push_str(&word),
                }
            }
            '[' => {
                let (spec, next) = bracket_group(&chars, i).ok_or(Problem::Unknown)?;
                let after = chars.get(next).copied();
                if spec.chars().all(|c| c.is_ascii_digit())
                    || after.is_some_and(|c| ident(c) || c == '!' || c == '\'')
                {
                    return Err(Problem::External);
                }
                // `[@Amount]` inside a table: the table holding this cell.
                let here = at.tables.iter().find(|t| {
                    t.sheet == at.sheet && t.rect.contains(crate::addr::CellPos::new(at.row, at.col))
                });
                match here {
                    Some(t) => {
                        out.push_str(&resolve_spec(t, &spec, at)?);
                        i = next;
                        changed = true;
                    }
                    None => return Err(Problem::Unknown),
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(changed.then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place<'a>(row: u32, col: u32, names: &'a IndexNames<'a>, tab_names: &'a [String], tables: &'a [TableDef]) -> Place<'a> {
        Place { row, col, sheet: 0, names, tab_names, tables }
    }

    #[test]
    fn structured_references() {
        let tab_names = vec!["Sales".to_owned(), "Other".to_owned()];
        let names = IndexNames(&tab_names);
        let tables = vec![TableDef {
            name: "Orders".into(),
            sheet: 0,
            rect: Rect::new(0, 0, 4, 2),
            header_rows: 1,
            totals_rows: 1,
            columns: vec!["Region".into(), "Unit Price".into(), "Qty".into()],
        }];
        let at = place(2, 4, &names, &tab_names, &tables);
        let conv = |t: &str| match prepare(t, &at, false) {
            Prepared::Formula { text, .. } => text,
            Prepared::Constant(v) => format!("const {v:?}"),
            Prepared::Failed(k) => format!("failed {k}"),
        };
        assert_eq!(conv("SUM(Orders[Qty])"), "SUM($C$2:$C$4)");
        assert_eq!(conv("SUM(Orders[[Unit Price]:[Qty]])"), "SUM($B$2:$C$4)");
        assert_eq!(conv("Orders[[#This Row],[Qty]]*2"), "$C3*2");
        assert_eq!(conv("Orders[@[Unit Price]]*Orders[@Qty]"), "$B3*$C3");
        assert_eq!(conv("ROWS(Orders[#All])"), "ROWS($A$1:$C$5)");
        assert_eq!(conv("Orders[[#Totals],[Qty]]"), "$C$5");
        assert_eq!(conv("COUNTA(Orders)"), "COUNTA($A$2:$C$4)");
        assert_eq!(conv("\"Orders[Qty]\"&1"), "\"Orders[Qty]\"&1");
        assert_eq!(conv("[1]Sheet1!A1"), "failed external");
        assert_eq!(conv("SUM(Orders[Nope])"), "failed unreadable");
        assert_eq!(conv("#N/A"), "const Error(NA)");
        assert_eq!(conv("SUM(_xlfn.ANCHORARRAY(A1))"), "SUM(A1#)");
        let other = Place { sheet: 1, ..place(0, 0, &names, &tab_names, &tables) };
        match prepare("SUM(Orders[Qty])", &other, false) {
            Prepared::Formula { text, .. } => assert_eq!(text, "SUM(Sales!$C$2:$C$4)"),
            _ => panic!("expected a formula"),
        }
        assert!(is_external("[2]Prices!B3*2"));
        assert!(is_external("'C:\\data\\[Book1.xlsx]Sheet1'!A1"));
        assert!(!is_external("SUM(A1:A3)"));
    }
}
