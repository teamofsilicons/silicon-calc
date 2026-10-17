//! Parsing formula text (A1 style, as people type it) into a tree, and writing a tree back as text.

use std::sync::Arc;

use super::ast::*;
use crate::{
    addr::{MAX_COLS, MAX_ROWS, col_index, col_name, quote_tab},
    value::ErrorKind,
};

/// Turns tab names into ids and back.
pub trait TabNames {
    fn tab_id(&self, name: &str) -> Option<TabId>;
    fn tab_name(&self, id: TabId) -> Option<String>;
}

/// No tabs at all (tests, or text with no tab references).
pub struct NoTabs;

impl TabNames for NoTabs {
    fn tab_id(&self, _: &str) -> Option<TabId> {
        None
    }
    fn tab_name(&self, _: TabId) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Character offset in the formula text (after the `=`).
    pub at: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at character {})", self.message, self.at + 1)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ErrorKind),
    /// A reference, already parsed. `tab` is the name as written.
    Ref { tab: Option<String>, r: RefTok },
    /// A tab prefix whose reference failed to parse, or a reference to a tab that does not exist.
    BadRef,
    Func(String),
    Name(String),
    Op(&'static str),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Hash,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RefTok {
    Cell { row: (bool, u32), col: (bool, u32) },
    Area { r1: (bool, u32), c1: (bool, u32), r2: (bool, u32), c2: (bool, u32) },
    Cols { c1: (bool, u32), c2: (bool, u32) },
    Rows { r1: (bool, u32), r2: (bool, u32) },
}

struct Lexer<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

fn parse_col_part(s: &str) -> Option<(bool, u32)> {
    let (abs, rest) = match s.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, s),
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some((abs, col_index(rest)?))
}

fn parse_row_part(s: &str) -> Option<(bool, u32)> {
    let (abs, rest) = match s.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, s),
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) || rest.len() > 8 {
        return None;
    }
    let n: u32 = rest.parse().ok()?;
    (n >= 1 && n <= MAX_ROWS).then_some((abs, n - 1))
}

fn parse_cell_part(s: &str) -> Option<((bool, u32), (bool, u32))> {
    // $A$1, A1, $A1, A$1
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }
    let letters_start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == letters_start {
        return None;
    }
    let col = parse_col_part(&s[..i])?;
    let row = parse_row_part(&s[i..])?;
    Some((row, col))
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, b: s.as_bytes(), i: 0 }
    }

    fn err(&self, message: impl Into<String>) -> ParseError {
        ParseError { message: message.into(), at: self.s[..self.i].chars().count() }
    }

    fn peek_char(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    /// Reads a run of characters that can make up a reference, name or function name.
    fn word(&mut self) -> &'a str {
        let start = self.i;
        while let Some(c) = self.peek_char() {
            if c.is_alphanumeric() || c == '_' || c == '.' || c == '$' || c == '\\' || c == '?' {
                self.i += c.len_utf8();
            } else {
                break;
            }
        }
        &self.s[start..self.i]
    }

    /// After a reference's first part, an optional `:second` part forming an area.
    fn try_ref(&mut self, first: &str) -> Option<RefTok> {
        let save = self.i;
        let second = if self.b.get(self.i) == Some(&b':') {
            self.i += 1;
            let w = self.word();
            if w.is_empty() {
                self.i = save;
                None
            } else {
                Some(w)
            }
        } else {
            None
        };
        if let Some((row, col)) = parse_cell_part(first) {
            if let Some(sec) = second {
                if let Some((r2, c2)) = parse_cell_part(sec) {
                    return Some(RefTok::Area { r1: row, c1: col, r2, c2 });
                }
                self.i = save;
            }
            return Some(RefTok::Cell { row, col });
        }
        if let Some(sec) = second {
            if let (Some(c1), Some(c2)) = (parse_col_part(first), parse_col_part(sec)) {
                return Some(RefTok::Cols { c1, c2 });
            }
            if let (Some(r1), Some(r2)) = (parse_row_part(first), parse_row_part(sec)) {
                return Some(RefTok::Rows { r1, r2 });
            }
        }
        self.i = save;
        None
    }

    fn next(&mut self) -> Result<Option<(Tok, usize)>, ParseError> {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let start = self.i;
        let c = self.peek_char().unwrap_or(' ');
        let tok = match c {
            '"' => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    match self.peek_char() {
                        None => return Err(self.err("A text value is missing its closing quote (\").")),
                        Some('"') => {
                            self.i += 1;
                            if self.peek_char() == Some('"') {
                                out.push('"');
                                self.i += 1;
                            } else {
                                break;
                            }
                        }
                        Some(ch) => {
                            out.push(ch);
                            self.i += ch.len_utf8();
                        }
                    }
                }
                Tok::Str(out)
            }
            '#' => {
                let rest = &self.s[self.i..];
                let found = ["#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!", "#SPILL!", "#CALC!", "#CIRC!"]
                    .iter()
                    .find(|e| rest.len() >= e.len() && rest[..e.len()].eq_ignore_ascii_case(e));
                match found {
                    Some(e) => {
                        self.i += e.len();
                        Tok::Err(ErrorKind::parse(e).unwrap_or(ErrorKind::NA))
                    }
                    None => {
                        self.i += 1;
                        Tok::Hash
                    }
                }
            }
            '\'' => {
                // 'Tab name'!A1
                self.i += 1;
                let mut name = String::new();
                loop {
                    match self.peek_char() {
                        None => return Err(self.err("A tab name is missing its closing quote (').")),
                        Some('\'') => {
                            self.i += 1;
                            if self.peek_char() == Some('\'') {
                                name.push('\'');
                                self.i += 1;
                            } else {
                                break;
                            }
                        }
                        Some(ch) => {
                            name.push(ch);
                            self.i += ch.len_utf8();
                        }
                    }
                }
                if self.peek_char() != Some('!') {
                    return Err(self.err(format!("'{name}' must be followed by ! and a reference, like '{name}'!A1.")));
                }
                self.i += 1;
                self.ref_after_tab(name)?
            }
            '0'..='9' | '.' => {
                // A row range (3:5) or a number.
                let save = self.i;
                let w = self.word();
                if !w.contains('.')
                    && let Some(r) = self.try_ref(w)
                    && matches!(r, RefTok::Rows { .. })
                {
                    Tok::Ref { tab: None, r }
                } else {
                    self.i = save;
                    let mut end = self.i;
                    while end < self.b.len() && (self.b[end].is_ascii_digit() || self.b[end] == b'.') {
                        end += 1;
                    }
                    if end < self.b.len() && (self.b[end] == b'e' || self.b[end] == b'E') {
                        let mut j = end + 1;
                        if j < self.b.len() && (self.b[j] == b'+' || self.b[j] == b'-') {
                            j += 1;
                        }
                        if j < self.b.len() && self.b[j].is_ascii_digit() {
                            while j < self.b.len() && self.b[j].is_ascii_digit() {
                                j += 1;
                            }
                            end = j;
                        }
                    }
                    let text = &self.s[self.i..end];
                    self.i = end;
                    Tok::Num(text.parse().map_err(|_| self.err(format!("{text} is not a number.")))?)
                }
            }
            c if c.is_alphabetic() || c == '_' || c == '$' || c == '\\' => {
                let w = self.word();
                if self.peek_char() == Some('!') {
                    self.i += 1;
                    return Ok(Some((self.ref_after_tab(w.to_owned())?, start)));
                }
                if self.peek_char() == Some('(') && !w.contains('$') {
                    let mut name = w.to_ascii_uppercase();
                    for prefix in ["_XLFN._XLWS.", "_XLFN.", "_XLWS.", "_XLUDF."] {
                        if let Some(r) = name.strip_prefix(prefix) {
                            name = r.to_owned();
                        }
                    }
                    Tok::Func(name)
                } else if let Some(r) = self.try_ref(w) {
                    Tok::Ref { tab: None, r }
                } else if w.eq_ignore_ascii_case("TRUE") {
                    Tok::Bool(true)
                } else if w.eq_ignore_ascii_case("FALSE") {
                    Tok::Bool(false)
                } else if w.contains('$') {
                    return Err(self.err(format!("{w} is not a reference.")));
                } else {
                    Tok::Name(w.to_owned())
                }
            }
            _ => {
                let two = self.s.get(self.i..self.i + 2).unwrap_or("");
                let (t, len) = match two {
                    "<>" => (Tok::Op("<>"), 2),
                    "<=" => (Tok::Op("<="), 2),
                    ">=" => (Tok::Op(">="), 2),
                    _ => match c {
                        '+' => (Tok::Op("+"), 1),
                        '-' => (Tok::Op("-"), 1),
                        '*' => (Tok::Op("*"), 1),
                        '/' => (Tok::Op("/"), 1),
                        '^' => (Tok::Op("^"), 1),
                        '&' => (Tok::Op("&"), 1),
                        '=' => (Tok::Op("="), 1),
                        '<' => (Tok::Op("<"), 1),
                        '>' => (Tok::Op(">"), 1),
                        '%' => (Tok::Op("%"), 1),
                        ':' => (Tok::Op(":"), 1),
                        '(' => (Tok::LParen, 1),
                        ')' => (Tok::RParen, 1),
                        '{' => (Tok::LBrace, 1),
                        '}' => (Tok::RBrace, 1),
                        ',' => (Tok::Comma, 1),
                        ';' => (Tok::Semi, 1),
                        _ => return Err(self.err(format!("{c} can't be used in a formula."))),
                    },
                };
                self.i += len;
                t
            }
        };
        Ok(Some((tok, start)))
    }

    fn ref_after_tab(&mut self, tab: String) -> Result<Tok, ParseError> {
        if self.s[self.i..].starts_with("#REF!") || self.s[self.i..].starts_with("#ref!") {
            self.i += 5;
            return Ok(Tok::Err(ErrorKind::Ref));
        }
        let w = self.word();
        match self.try_ref(w) {
            Some(r) => Ok(Tok::Ref { tab: Some(tab), r }),
            None => Err(self.err(format!("{tab}!{w} is not a reference."))),
        }
    }
}

struct Parser<'a, 'n> {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    at_row: u32,
    at_col: u32,
    names: &'n dyn TabNames,
    src: &'a str,
}

fn coord(part: (bool, u32), at: u32) -> Coord {
    if part.0 { Coord::Abs(part.1) } else { Coord::Rel(part.1 as i32 - at as i32) }
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.0.clone());
        self.pos += 1;
        t
    }

    fn err(&self, message: impl Into<String>) -> ParseError {
        let at = self.toks.get(self.pos).map(|t| self.src[..t.1].chars().count()).unwrap_or(self.src.chars().count());
        ParseError { message: message.into(), at }
    }

    fn expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) => match *o {
                    "+" => BinaryOp::Add,
                    "-" => BinaryOp::Sub,
                    "*" => BinaryOp::Mul,
                    "/" => BinaryOp::Div,
                    "^" => BinaryOp::Pow,
                    "&" => BinaryOp::Concat,
                    "=" => BinaryOp::Eq,
                    "<>" => BinaryOp::Ne,
                    "<" => BinaryOp::Lt,
                    "<=" => BinaryOp::Le,
                    ">" => BinaryOp::Gt,
                    ">=" => BinaryOp::Ge,
                    ":" => BinaryOp::Range,
                    _ => break,
                },
                _ => break,
            };
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            self.bump();
            // All Excel binary operators are left-associative, including ^.
            let rhs = self.expr(prec + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Some(Tok::Op("-")) => {
                self.bump();
                let e = self.unary()?;
                Ok(match e {
                    Expr::Number(n) => Expr::Number(-n),
                    other => Expr::Unary(UnaryOp::Neg, Box::new(other)),
                })
            }
            Some(Tok::Op("+")) => {
                self.bump();
                let e = self.unary()?;
                Ok(Expr::Unary(UnaryOp::Plus, Box::new(e)))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Op("%")) => {
                    self.bump();
                    e = Expr::Unary(UnaryOp::Percent, Box::new(e));
                }
                Some(Tok::Hash) => {
                    self.bump();
                    e = match e {
                        Expr::Cell(c) => Expr::Spill(c),
                        _ => return Err(self.err("# can only follow a cell, like A1#.")),
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn reference(&self, tab: Option<String>, r: RefTok) -> Result<Expr, ParseError> {
        let tab = match tab {
            None => None,
            Some(name) => match self.names.tab_id(&name) {
                Some(id) => Some(id),
                None => return Ok(Expr::Error(ErrorKind::Ref)),
            },
        };
        let (ar, ac) = (self.at_row, self.at_col);
        Ok(match r {
            RefTok::Cell { row, col } => Expr::Cell(CellRef { tab, row: coord(row, ar), col: coord(col, ac) }),
            RefTok::Area { r1, c1, r2, c2 } => Expr::Range(RangeRef {
                tab,
                rows: Some((coord(r1, ar), coord(r2, ar))),
                cols: Some((coord(c1, ac), coord(c2, ac))),
            }),
            RefTok::Cols { c1, c2 } => Expr::Range(RangeRef { tab, rows: None, cols: Some((coord(c1, ac), coord(c2, ac))) }),
            RefTok::Rows { r1, r2 } => Expr::Range(RangeRef { tab, rows: Some((coord(r1, ar), coord(r2, ar))), cols: None }),
        })
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let Some(tok) = self.bump() else {
            return Err(self.err("The formula ends where a value or reference was expected."));
        };
        Ok(match tok {
            Tok::Num(n) => Expr::Number(n),
            Tok::Str(s) => Expr::Text(Arc::from(s.as_str())),
            Tok::Bool(b) => Expr::Bool(b),
            Tok::Err(e) => Expr::Error(e),
            Tok::BadRef => Expr::Error(ErrorKind::Ref),
            Tok::Ref { tab, r } => self.reference(tab, r)?,
            Tok::Name(n) => Expr::Name(Arc::from(n.as_str())),
            Tok::LParen => {
                let e = self.expr(0)?;
                match self.bump() {
                    Some(Tok::RParen) => Expr::Paren(Box::new(e)),
                    _ => {
                        self.pos = self.pos.saturating_sub(1);
                        return Err(self.err("A ( is missing its closing )."));
                    }
                }
            }
            Tok::Func(name) => {
                self.bump(); // (
                let mut args = Vec::new();
                if self.peek() == Some(&Tok::RParen) {
                    self.bump();
                } else {
                    loop {
                        if matches!(self.peek(), Some(Tok::Comma) | Some(Tok::RParen)) {
                            args.push(Expr::Missing);
                        } else {
                            args.push(self.expr(0)?);
                        }
                        match self.bump() {
                            Some(Tok::Comma) => continue,
                            Some(Tok::RParen) => break,
                            _ => {
                                self.pos = self.pos.saturating_sub(1);
                                return Err(self.err(format!("{name}( is missing a , between arguments or its closing ).")));
                            }
                        }
                    }
                }
                Expr::Call(Arc::from(name.as_str()), args)
            }
            Tok::LBrace => {
                let mut rows = vec![Vec::new()];
                loop {
                    let neg = if self.peek() == Some(&Tok::Op("-")) {
                        self.bump();
                        true
                    } else {
                        false
                    };
                    let item = match self.bump() {
                        Some(Tok::Num(n)) => Expr::Number(if neg { -n } else { n }),
                        Some(Tok::Str(s)) if !neg => Expr::Text(Arc::from(s.as_str())),
                        Some(Tok::Bool(b)) if !neg => Expr::Bool(b),
                        Some(Tok::Err(e)) if !neg => Expr::Error(e),
                        _ => {
                            self.pos = self.pos.saturating_sub(1);
                            return Err(self.err("An array like {1,2;3,4} may only hold numbers, text, TRUE/FALSE or errors."));
                        }
                    };
                    rows.last_mut().expect("a row").push(item);
                    match self.bump() {
                        Some(Tok::Comma) => {}
                        Some(Tok::Semi) => rows.push(Vec::new()),
                        Some(Tok::RBrace) => break,
                        _ => {
                            self.pos = self.pos.saturating_sub(1);
                            return Err(self.err("An array is missing its closing }."));
                        }
                    }
                }
                let width = rows[0].len();
                if rows.iter().any(|r| r.len() != width) {
                    return Err(self.err("Every row of an array needs the same number of values."));
                }
                Expr::Array(rows)
            }
            Tok::Op(o) => return Err(self.err(format!("{o} needs a value before it."))),
            Tok::RParen => {
                self.pos -= 1;
                return Err(self.err("There is a ) without a matching (."));
            }
            Tok::Comma | Tok::Semi => {
                self.pos -= 1;
                return Err(self.err("A , is only allowed between a function's arguments."));
            }
            Tok::RBrace | Tok::Hash => {
                self.pos -= 1;
                return Err(self.err("This character is not expected here."));
            }
        })
    }
}

/// Parses formula text (without the leading `=`) written in the cell at (`row`, `col`).
pub fn parse_formula(src: &str, row: u32, col: u32, names: &dyn TabNames) -> Result<Expr, ParseError> {
    let mut lexer = Lexer::new(src);
    let mut toks = Vec::new();
    while let Some(t) = lexer.next()? {
        toks.push(t);
    }
    if toks.is_empty() {
        return Err(ParseError { message: "The formula is empty.".into(), at: 0 });
    }
    let mut p = Parser { toks, pos: 0, at_row: row, at_col: col, names, src };
    let e = p.expr(0)?;
    if p.pos < p.toks.len() {
        return Err(p.err("There is more after the end of the formula; is an operator or ( missing?"));
    }
    Ok(e)
}

// ---- writing ---------------------------------------------------------------------------------------------------

fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n}");
        if s.contains('e') { s.to_uppercase() } else { s }
    }
}

fn write_coord_a1(c: Coord, at: u32, is_col: bool, max: u32) -> Option<String> {
    let p = c.resolve(at)?;
    if p >= max {
        return None;
    }
    let body = if is_col { col_name(p) } else { (p + 1).to_string() };
    Some(if c.is_abs() { format!("${body}") } else { body })
}

fn tab_prefix(tab: Option<TabId>, names: &dyn TabNames) -> Option<String> {
    match tab {
        None => Some(String::new()),
        Some(id) => names.tab_name(id).map(|n| format!("{}!", quote_tab(&n))),
    }
}

fn write_ref(e: &Expr, row: u32, col: u32, names: &dyn TabNames) -> String {
    let render = || -> Option<String> {
        match e {
            Expr::Cell(c) | Expr::Spill(c) => {
                let pre = tab_prefix(c.tab, names)?;
                let s = format!(
                    "{pre}{}{}",
                    write_coord_a1(c.col, col, true, MAX_COLS)?,
                    write_coord_a1(c.row, row, false, MAX_ROWS)?
                );
                Some(if matches!(e, Expr::Spill(_)) { format!("{s}#") } else { s })
            }
            Expr::Range(r) => {
                let pre = tab_prefix(r.tab, names)?;
                Some(match (r.rows, r.cols) {
                    (Some((r1, r2)), Some((c1, c2))) => format!(
                        "{pre}{}{}:{}{}",
                        write_coord_a1(c1, col, true, MAX_COLS)?,
                        write_coord_a1(r1, row, false, MAX_ROWS)?,
                        write_coord_a1(c2, col, true, MAX_COLS)?,
                        write_coord_a1(r2, row, false, MAX_ROWS)?
                    ),
                    (None, Some((c1, c2))) => format!(
                        "{pre}{}:{}",
                        write_coord_a1(c1, col, true, MAX_COLS)?,
                        write_coord_a1(c2, col, true, MAX_COLS)?
                    ),
                    (Some((r1, r2)), None) => format!(
                        "{pre}{}:{}",
                        write_coord_a1(r1, row, false, MAX_ROWS)?,
                        write_coord_a1(r2, row, false, MAX_ROWS)?
                    ),
                    (None, None) => return None,
                })
            }
            _ => None,
        }
    };
    render().unwrap_or_else(|| "#REF!".to_owned())
}

/// Writes a tree as A1 text (without `=`) for the cell at (`row`, `col`).
pub fn to_a1(e: &Expr, row: u32, col: u32, names: &dyn TabNames) -> String {
    let mut out = String::new();
    write_expr(e, &mut out, &mut |e, out| out.push_str(&write_ref(e, row, col, names)));
    out
}

fn write_coord_r1c1(c: Coord, axis: char) -> String {
    match c {
        Coord::Abs(p) => format!("{axis}{}", p + 1),
        Coord::Rel(0) => axis.to_string(),
        Coord::Rel(d) => format!("{axis}[{d}]"),
    }
}

/// The tree's R1C1 form, the key formulas are grouped by: two cells with the same key hold the same formula.
pub fn r1c1_key(e: &Expr) -> String {
    let mut out = String::new();
    write_expr(e, &mut out, &mut |e, out| {
        let tab = |t: Option<TabId>| t.map(|t| format!("#{t}!")).unwrap_or_default();
        match e {
            Expr::Cell(c) => out.push_str(&format!("{}{}{}", tab(c.tab), write_coord_r1c1(c.row, 'R'), write_coord_r1c1(c.col, 'C'))),
            Expr::Spill(c) => {
                out.push_str(&format!("{}{}{}#", tab(c.tab), write_coord_r1c1(c.row, 'R'), write_coord_r1c1(c.col, 'C')))
            }
            Expr::Range(r) => {
                out.push_str(&tab(r.tab));
                match r.rows {
                    Some((a, b)) => out.push_str(&format!("{}:{}", write_coord_r1c1(a, 'R'), write_coord_r1c1(b, 'R'))),
                    None => out.push_str("R*"),
                }
                match r.cols {
                    Some((a, b)) => out.push_str(&format!("{}:{}", write_coord_r1c1(a, 'C'), write_coord_r1c1(b, 'C'))),
                    None => out.push_str("C*"),
                }
            }
            _ => {}
        }
    });
    out
}

fn write_expr(e: &Expr, out: &mut String, refs: &mut dyn FnMut(&Expr, &mut String)) {
    match e {
        Expr::Number(n) => out.push_str(&fmt_number(*n)),
        Expr::Text(s) => {
            out.push('"');
            out.push_str(&s.replace('"', "\"\""));
            out.push('"');
        }
        Expr::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
        Expr::Error(k) => out.push_str(k.as_str()),
        Expr::Missing => {}
        Expr::Cell(_) | Expr::Range(_) | Expr::Spill(_) => refs(e, out),
        Expr::Name(n) => out.push_str(n),
        Expr::Unary(UnaryOp::Neg, x) => {
            out.push('-');
            write_expr(x, out, refs);
        }
        Expr::Unary(UnaryOp::Plus, x) => {
            out.push('+');
            write_expr(x, out, refs);
        }
        Expr::Unary(UnaryOp::Percent, x) => {
            write_expr(x, out, refs);
            out.push('%');
        }
        Expr::Binary(op, a, b) => {
            write_expr(a, out, refs);
            out.push_str(op.symbol());
            write_expr(b, out, refs);
        }
        Expr::Paren(x) => {
            out.push('(');
            write_expr(x, out, refs);
            out.push(')');
        }
        Expr::Call(name, args) => {
            out.push_str(name);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_expr(a, out, refs);
            }
            out.push(')');
        }
        Expr::Array(rows) => {
            out.push('{');
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                for (j, a) in r.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    write_expr(a, out, refs);
                }
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tabs;
    impl TabNames for Tabs {
        fn tab_id(&self, name: &str) -> Option<TabId> {
            match name.to_ascii_lowercase().as_str() {
                "sales" => Some(1),
                "my tab" => Some(2),
                _ => None,
            }
        }
        fn tab_name(&self, id: TabId) -> Option<String> {
            match id {
                1 => Some("Sales".into()),
                2 => Some("My tab".into()),
                _ => None,
            }
        }
    }

    fn roundtrip(src: &str) -> String {
        let e = parse_formula(src, 4, 3, &Tabs).unwrap_or_else(|err| panic!("{src}: {err}"));
        to_a1(&e, 4, 3, &Tabs)
    }

    #[test]
    fn round_trips() {
        for f in [
            "A1+B2*3",
            "SUM(A1:B10)",
            "$A$1",
            "A$1:$B2",
            "Sales!A1",
            "'My tab'!A1:C3",
            "SUM(A:A)",
            "SUM(3:5)",
            "IF(A1>0,\"yes\",\"no\")",
            "-A1^2",
            "{1,2;3,4}",
            "A1#",
            "IF(A1,,2)",
            "50%",
            "\"a\"\"b\"&C1",
            "XLOOKUP(A2,Sales!A:A,Sales!B:B,\"none\")",
            "TRUE",
            "#N/A",
            "SUM(A1:INDEX(B:B,3))",
            "STDEV.S(A1:A9)",
        ] {
            assert_eq!(roundtrip(f), f);
        }
    }

    #[test]
    fn relative_keys_are_shared() {
        let a = parse_formula("A1*B1", 0, 2, &Tabs).expect("parse");
        let b = parse_formula("A2*B2", 1, 2, &Tabs).expect("parse");
        assert_eq!(r1c1_key(&a), r1c1_key(&b));
        assert_eq!(to_a1(&a, 9, 2, &Tabs), "A10*B10");
        let c = parse_formula("$A$1*B1", 0, 2, &Tabs).expect("parse");
        let d = parse_formula("$A$1*B2", 1, 2, &Tabs).expect("parse");
        assert_eq!(r1c1_key(&c), r1c1_key(&d));
    }

    #[test]
    fn precedence() {
        let e = parse_formula("1+2*3^2", 0, 0, &NoTabs).expect("parse");
        assert_eq!(to_a1(&e, 0, 0, &NoTabs), "1+2*3^2");
        match e {
            Expr::Binary(BinaryOp::Add, _, b) => assert!(matches!(*b, Expr::Binary(BinaryOp::Mul, _, _))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn errors() {
        assert!(parse_formula("SUM(1,2", 0, 0, &NoTabs).is_err());
        assert!(parse_formula("1+", 0, 0, &NoTabs).is_err());
        assert!(parse_formula("\"abc", 0, 0, &NoTabs).is_err());
        assert_eq!(parse_formula("Nope!A1", 0, 0, &Tabs), Ok(Expr::Error(ErrorKind::Ref)));
    }
}
