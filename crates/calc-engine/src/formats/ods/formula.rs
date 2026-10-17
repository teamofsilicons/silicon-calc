//! Formulas between OpenDocument's syntax (OpenFormula) and Excel's, which is what the engine reads.
//!
//! OpenFormula writes references in brackets with a dot before the cell (`[.A1]`, `[.A1:.B9]`,
//! `[$Sheet2.$A$1]`, `['My tab'.A1:.B2]`), separates arguments with `;`, writes inline arrays as `{1;2|3;4}`
//! (`;` between columns, `|` between rows), `~` for a union and `!` for an intersection, and prefixes functions
//! Excel added after OpenFormula with `COM.MICROSOFT.`. Files saved by Excel may instead hold Excel's own syntax
//! behind `msoxl:`.

use crate::{
    addr::{col_name, quote_tab},
    formula::ast::{BinaryOp, Coord, Expr, UnaryOp},
    formula::parse::TabNames,
    value::ErrorKind,
};

/// Why a formula can't be kept.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// It reads another file.
    External,
    /// A reference across several tabs (`Sheet1.A1:Sheet3.B2`), which a sheet doesn't have.
    ThreeD,
    Unreadable,
}

/// Function name prefixes OpenFormula producers add, removed on the way in.
const PREFIXES: [&str; 6] = ["COM.MICROSOFT.", "ORG.OPENOFFICE.", "ORG.LIBREOFFICE.", "LEGACY.", "_XLFN._XLWS.", "_XLFN."];

/// OpenFormula text (`of:=SUM([.A1:.B2])`, with or without its namespace) as Excel text without `=`.
pub(crate) fn from_odf(formula: &str) -> Result<String, Problem> {
    let f = formula.trim();
    let (ns, body) = match f.split_once(":=") {
        Some((ns, rest)) if !ns.is_empty() && ns.chars().all(|c| c.is_ascii_alphanumeric()) => (ns.to_ascii_lowercase(), rest),
        _ => (String::from("of"), f.strip_prefix('=').unwrap_or(f)),
    };
    if ns == "msoxl" {
        return Ok(body.trim().to_owned());
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let mut braces = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push('"');
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
            '[' => {
                // A reference: up to the matching ], skipping quoted tab names.
                let start = i + 1;
                let mut j = start;
                let mut quoted = false;
                while j < chars.len() {
                    match chars[j] {
                        '\'' => quoted = !quoted,
                        ']' if !quoted => break,
                        _ => {}
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return Err(Problem::Unreadable);
                }
                let inner: String = chars[start..j].iter().collect();
                out.push_str(&reference(&inner)?);
                i = j + 1;
            }
            '{' => {
                braces += 1;
                out.push('{');
                i += 1;
            }
            '}' => {
                braces = braces.saturating_sub(1);
                out.push('}');
                i += 1;
            }
            '#' => {
                // Error literals (`#DIV/0!`, `#N/A`) are copied as they are: their `!` isn't an intersection.
                let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
                let literal = ["#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!", "#SPILL!", "#CALC!"]
                    .into_iter()
                    .find(|e| rest.len() >= e.len() && rest[..e.len()].eq_ignore_ascii_case(e));
                match literal {
                    Some(e) => {
                        out.push_str(e);
                        i += e.chars().count();
                    }
                    None => {
                        out.push('#');
                        i += 1;
                    }
                }
            }
            ';' => {
                out.push(',');
                i += 1;
            }
            '|' if braces > 0 => {
                out.push(';');
                i += 1;
            }
            '~' => {
                out.push(',');
                i += 1;
            }
            '!' => {
                out.push(' ');
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let mut k = i;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if chars.get(k) == Some(&'(') {
                    let upper = word.to_ascii_uppercase();
                    let mut name = upper.as_str();
                    for p in PREFIXES {
                        if let Some(rest) = name.strip_prefix(p) {
                            name = rest;
                        }
                    }
                    // TRUE() and FALSE() are functions in OpenFormula and constants in Excel.
                    let after_paren = chars[k + 1..].iter().position(|c| *c != ' ').map(|p| k + 1 + p);
                    if (name == "TRUE" || name == "FALSE") && after_paren.is_some_and(|p| chars[p] == ')') {
                        out.push_str(name);
                        i = after_paren.unwrap_or(k) + 1;
                        continue;
                    }
                    out.push_str(name);
                } else {
                    out.push_str(&word);
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out.trim().to_owned())
}

/// One side of a reference: an optional tab and the cell, column or row (`$Sheet1.$A$1`, `.A`, `.$3`).
struct Side {
    tab: Option<String>,
    cell: String,
}

fn side(s: &str) -> Result<Side, Problem> {
    let s = s.trim();
    if s.starts_with('.') {
        return Ok(Side { tab: None, cell: s[1..].to_owned() });
    }
    let s = s.strip_prefix('$').unwrap_or(s);
    if let Some(rest) = s.strip_prefix('\'') {
        let mut name = String::new();
        let mut it = rest.char_indices().peekable();
        while let Some((idx, c)) = it.next() {
            if c == '\'' {
                if matches!(it.peek(), Some((_, '\''))) {
                    name.push('\'');
                    it.next();
                    continue;
                }
                let after = &rest[idx + 1..];
                return match after.strip_prefix('.') {
                    Some(cell) => Ok(Side { tab: Some(name), cell: cell.to_owned() }),
                    None => Err(Problem::Unreadable),
                };
            }
            name.push(c);
        }
        return Err(Problem::Unreadable);
    }
    match s.rsplit_once('.') {
        Some((tab, cell)) if !tab.is_empty() => Ok(Side { tab: Some(tab.to_owned()), cell: cell.to_owned() }),
        _ => Ok(Side { tab: None, cell: s.to_owned() }),
    }
}

/// The inside of an OpenFormula reference (`.A1:.B2`, `$Sheet2.A1`) as Excel text (`A1:B2`, `Sheet2!A1`).
pub(crate) fn reference(inner: &str) -> Result<String, Problem> {
    let t = inner.trim();
    if t.starts_with("'file:") || t.contains("'#$") || t.contains("'#") && t.starts_with('\'') {
        return Err(Problem::External);
    }
    // Split at the `:` between the two sides (not inside a quoted tab name).
    let mut split = None;
    let mut quoted = false;
    for (i, c) in t.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            ':' if !quoted => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (a, b) = match split {
        Some(i) => (side(&t[..i])?, Some(side(&t[i + 1..])?)),
        None => (side(t)?, None),
    };
    if let Some(b) = &b
        && let (Some(ta), Some(tb)) = (&a.tab, &b.tab)
        && !ta.eq_ignore_ascii_case(tb)
    {
        return Err(Problem::ThreeD);
    }
    let tab = a.tab.clone().or_else(|| b.as_ref().and_then(|b| b.tab.clone()));
    let clean = |cell: &str| -> String { cell.trim().to_owned() };
    let body = match &b {
        Some(b) => format!("{}:{}", clean(&a.cell), clean(&b.cell)),
        None => clean(&a.cell),
    };
    if body.contains("#REF!") {
        return Ok("#REF!".to_owned());
    }
    Ok(match tab {
        Some(t) => format!("{}!{body}", quote_tab(&t)),
        None => body,
    })
}

/// A cell range address of a named range (`$Sheet1.$A$1:.$B$9`, no brackets) as Excel text.
pub(crate) fn range_address(address: &str) -> Result<String, Problem> {
    // Several areas are separated by spaces; a sheet holds one, so only the first is kept.
    let first = address.split_whitespace().next().unwrap_or("");
    reference(first)
}

// ---- writing ----------------------------------------------------------------------------------------------------

/// Functions OpenFormula doesn't define, which LibreOffice reads with Excel's `COM.MICROSOFT.` prefix.
fn needs_prefix(name: &str) -> bool {
    const PLAIN: [&str; 30] = [
        "CONCAT", "TEXTJOIN", "IFS", "SWITCH", "MAXIFS", "MINIFS", "XLOOKUP", "XMATCH", "FILTER", "SORT", "SORTBY", "UNIQUE",
        "SEQUENCE", "RANDARRAY", "LET", "LAMBDA", "ANCHORARRAY", "TEXTBEFORE", "TEXTAFTER", "TEXTSPLIT", "VSTACK", "HSTACK",
        "TOCOL", "TOROW", "WRAPROWS", "WRAPCOLS", "TAKE", "DROP", "CHOOSEROWS", "CHOOSECOLS",
    ];
    const DOTTED_ODF: [&str; 3] = ["ERROR.TYPE", "BINOM.DIST.RANGE", "FORECAST.LINEAR"];
    if PLAIN.contains(&name) || matches!(name, "EXPAND" | "ARRAYTOTEXT" | "VALUETOTEXT" | "AGGREGATE" | "MAP" | "REDUCE" | "SCAN" | "BYROW" | "BYCOL" | "MAKEARRAY" | "ISOMITTED") {
        return true;
    }
    name.contains('.') && !DOTTED_ODF.contains(&name)
}

fn odf_tab(name: &str) -> String {
    let plain = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.chars().next().is_some_and(|c| c.is_ascii_digit());
    if plain { name.to_owned() } else { format!("'{}'", name.replace('\'', "''")) }
}

fn coord(c: Coord, at: u32, is_col: bool) -> Option<String> {
    let p = c.resolve(at)?;
    let body = if is_col { col_name(p) } else { (p + 1).to_string() };
    Some(if c.is_abs() { format!("${body}") } else { body })
}

/// A reference as OpenFormula writes it, or `#REF!`.
fn write_ref(e: &Expr, row: u32, col: u32, names: &dyn TabNames) -> String {
    let render = || -> Option<String> {
        let (tab, a, b) = match e {
            Expr::Cell(c) => (c.tab, format!("{}{}", coord(c.col, col, true)?, coord(c.row, row, false)?), None),
            Expr::Range(r) => match (r.rows, r.cols) {
                (Some((r1, r2)), Some((c1, c2))) => (
                    r.tab,
                    format!("{}{}", coord(c1, col, true)?, coord(r1, row, false)?),
                    Some(format!("{}{}", coord(c2, col, true)?, coord(r2, row, false)?)),
                ),
                (None, Some((c1, c2))) => (r.tab, coord(c1, col, true)?, Some(coord(c2, col, true)?)),
                (Some((r1, r2)), None) => (r.tab, coord(r1, row, false)?, Some(coord(r2, row, false)?)),
                (None, None) => return None,
            },
            _ => return None,
        };
        let first = match tab {
            Some(id) => format!("${}.{a}", odf_tab(&names.tab_name(id)?)),
            None => format!(".{a}"),
        };
        Some(match b {
            Some(b) => format!("[{first}:.{b}]"),
            None => format!("[{first}]"),
        })
    };
    render().unwrap_or_else(|| "[.#REF!]".to_owned())
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}").to_uppercase() }
}

fn write(e: &Expr, row: u32, col: u32, names: &dyn TabNames, out: &mut String) {
    match e {
        Expr::Number(n) => out.push_str(&number(*n)),
        Expr::Text(s) => {
            out.push('"');
            out.push_str(&s.replace('"', "\"\""));
            out.push('"');
        }
        Expr::Bool(b) => out.push_str(if *b { "TRUE()" } else { "FALSE()" }),
        Expr::Error(k) => out.push_str(match k {
            ErrorKind::Spill | ErrorKind::Calc | ErrorKind::Circ => "#N/A",
            other => other.as_str(),
        }),
        Expr::Missing => {}
        Expr::Cell(_) | Expr::Range(_) => out.push_str(&write_ref(e, row, col, names)),
        Expr::Spill(c) => {
            out.push_str("COM.MICROSOFT.ANCHORARRAY(");
            out.push_str(&write_ref(&Expr::Cell(*c), row, col, names));
            out.push(')');
        }
        Expr::Name(n) => out.push_str(n),
        Expr::Unary(op, x) => match op {
            UnaryOp::Neg => {
                out.push('-');
                write(x, row, col, names, out);
            }
            UnaryOp::Plus => {
                out.push('+');
                write(x, row, col, names, out);
            }
            UnaryOp::Percent => {
                write(x, row, col, names, out);
                out.push('%');
            }
        },
        Expr::Binary(op, a, b) => {
            write(a, row, col, names, out);
            out.push_str(match op {
                BinaryOp::Intersect => "!",
                other => other.symbol(),
            });
            write(b, row, col, names, out);
        }
        Expr::Paren(x) => {
            out.push('(');
            write(x, row, col, names, out);
            out.push(')');
        }
        Expr::Call(name, args) => {
            if needs_prefix(name) {
                out.push_str("COM.MICROSOFT.");
            }
            out.push_str(name);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                write(a, row, col, names, out);
            }
            out.push(')');
        }
        Expr::Array(rows) => {
            out.push('{');
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                for (j, a) in r.iter().enumerate() {
                    if j > 0 {
                        out.push(';');
                    }
                    write(a, row, col, names, out);
                }
            }
            out.push('}');
        }
        #[allow(unreachable_patterns)]
        other => out.push_str(&crate::formula::parse::to_a1(other, row, col, names)),
    }
}

/// A formula tree as OpenFormula text, with its namespace: `of:=SUM([.A1:.B2])`.
pub(crate) fn to_odf(e: &Expr, row: u32, col: u32, names: &dyn TabNames) -> String {
    let mut out = String::from("of:=");
    write(e, row, col, names, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::parse::parse_formula;

    struct Tabs;
    impl TabNames for Tabs {
        fn tab_id(&self, name: &str) -> Option<u32> {
            match name.to_ascii_lowercase().as_str() {
                "sales" => Some(1),
                "my tab" => Some(2),
                _ => None,
            }
        }
        fn tab_name(&self, id: u32) -> Option<String> {
            match id {
                1 => Some("Sales".into()),
                2 => Some("My tab".into()),
                _ => None,
            }
        }
    }

    #[test]
    fn from_open_formula() {
        assert_eq!(from_odf("of:=SUM([.A1:.B2])").as_deref(), Ok("SUM(A1:B2)"));
        assert_eq!(from_odf("of:=[$Sales.$A$1]*2").as_deref(), Ok("Sales!$A$1*2"));
        assert_eq!(from_odf("of:=SUM(['My tab'.A1:.A9];[.C3])").as_deref(), Ok("SUM('My tab'!A1:A9,C3)"));
        assert_eq!(from_odf("of:=IF([.A1]>0;\"a;b\";TRUE())").as_deref(), Ok("IF(A1>0,\"a;b\",TRUE)"));
        assert_eq!(from_odf("of:={1;2|3;4}").as_deref(), Ok("{1,2;3,4}"));
        assert_eq!(from_odf("of:=COM.MICROSOFT.XLOOKUP([.A1];[.B:.B];[.C:.C])").as_deref(), Ok("XLOOKUP(A1,B:B,C:C)"));
        assert_eq!(from_odf("of:=SUM([.$1:.$3])").as_deref(), Ok("SUM($1:$3)"));
        assert_eq!(from_odf("msoxl:=SUM(A1:A3)").as_deref(), Ok("SUM(A1:A3)"));
        assert_eq!(from_odf("of:=[.#REF!]+1").as_deref(), Ok("#REF!+1"));
        assert_eq!(from_odf("of:=['file:///tmp/x.ods'#$Sheet1.A1]"), Err(Problem::External));
        assert_eq!(from_odf("of:=SUM([Sales.A1:Other.B2])"), Err(Problem::ThreeD));
        assert_eq!(range_address("$Sales.$B$2:.$B$9").as_deref(), Ok("Sales!$B$2:$B$9"));
        assert_eq!(range_address("$'My tab'.$A$1").as_deref(), Ok("'My tab'!$A$1"));
    }

    #[test]
    fn to_open_formula_and_back() {
        for (excel, odf) in [
            ("SUM(A1:B2)", "of:=SUM([.A1:.B2])"),
            ("Sales!$A$1*2", "of:=[$Sales.$A$1]*2"),
            ("SUM('My tab'!A1:A9,C3)", "of:=SUM([$'My tab'.A1:.A9];[.C3])"),
            ("IF(A1>0,\"a\",TRUE)", "of:=IF([.A1]>0;\"a\";TRUE())"),
            ("{1,2;3,4}", "of:={1;2|3;4}"),
            ("XLOOKUP(A1,B:B,C:C)", "of:=COM.MICROSOFT.XLOOKUP([.A1];[.B:.B];[.C:.C])"),
            ("STDEV.S(A1:A9)", "of:=COM.MICROSOFT.STDEV.S([.A1:.A9])"),
            ("SUM($1:$3)", "of:=SUM([.$1:.$3])"),
            ("-A1^2%", "of:=-[.A1]^2%"),
        ] {
            let e = parse_formula(excel, 4, 3, &Tabs).expect(excel);
            assert_eq!(to_odf(&e, 4, 3, &Tabs), odf, "{excel}");
            let back = from_odf(odf).expect(odf);
            let e2 = parse_formula(&back, 4, 3, &Tabs).expect(&back);
            assert_eq!(e, e2, "{excel} -> {odf} -> {back}");
        }
    }
}
