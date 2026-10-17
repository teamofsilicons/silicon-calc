//! Evaluating a formula tree against a grid.
//!
//! Reading a cell whose formula has not been recalculated yet records it in `missing` and reads it as empty; the
//! recalculation driver then evaluates those cells first and tries again. No recursion into other cells happens
//! here, so a chain of a million formulas cannot overflow the stack.

use std::sync::Arc;

use super::{
    ast::*,
    functions,
};
use crate::{
    addr::{MAX_COLS, MAX_ROWS, Rect},
    style::Style,
    value::{ErrorKind, Value, compare},
};

/// A cell by tab and position.
pub type Key = (TabId, u32, u32);

/// A cell's value without its text, for passes that only need numbers (SUM over a million rows).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Empty,
    Number(f64),
    Text,
    Bool(bool),
    Error(ErrorKind),
}

impl Kind {
    pub fn of(v: &Value) -> Self {
        match v {
            Value::Empty => Self::Empty,
            Value::Number(n) => Self::Number(*n),
            Value::Text(_) => Self::Text,
            Value::Bool(b) => Self::Bool(*b),
            Value::Error(e) => Self::Error(*e),
        }
    }
}

/// What evaluation reads from.
pub trait Grid {
    fn value(&self, tab: TabId, row: u32, col: u32) -> Value;
    /// How many rows and columns the tab has ids for (everything beyond is empty), or None when it doesn't exist.
    fn extent(&self, tab: TabId) -> Option<(u32, u32)>;
    /// Pushes every cell in `rect` whose formula still has to be recalculated.
    fn pending(&self, tab: TabId, rect: &Rect, out: &mut Vec<Key>);
    fn tab_id(&self, name: &str) -> Option<TabId>;
    fn tab_name(&self, tab: TabId) -> Option<String>;
    fn tab_position(&self, tab: TabId) -> Option<usize>;
    fn tab_count(&self) -> usize;
    /// A defined name's tree (as if written in A1 of `tab`).
    fn defined_name(&self, name: &str, tab: TabId) -> Option<Expr>;
    /// The size of the array a formula at this cell spilled, if it spilled.
    fn spill_extent(&self, tab: TabId, row: u32, col: u32) -> Option<(u32, u32)>;
    fn formula_text(&self, tab: TabId, row: u32, col: u32) -> Option<String>;
    fn number_format(&self, tab: TabId, row: u32, col: u32) -> Option<String>;
    /// Now, as a date serial (local time of the sheet).
    fn now(&self) -> f64;
    fn random(&self) -> f64;

    /// Visits the values of `rect`, row by row. The rectangle lies within the tab's extent. `f` returns false to
    /// stop early.
    fn visit(&self, tab: TabId, rect: &Rect, f: &mut dyn FnMut(u32, u32, &Value) -> bool) {
        for row in rect.r1..=rect.r2 {
            for col in rect.c1..=rect.c2 {
                if !f(row, col, &self.value(tab, row, col)) {
                    return;
                }
            }
        }
    }

    /// Like `visit`, without the text of text cells: the fast path for numeric passes.
    fn visit_kinds(&self, tab: TabId, rect: &Rect, f: &mut dyn FnMut(u32, u32, Kind) -> bool) {
        self.visit(tab, rect, &mut |r, c, v| f(r, c, Kind::of(v)));
    }

    /// Positions of the tab's hidden rows, sorted.
    fn hidden_rows(&self, _tab: TabId) -> Vec<u32> {
        Vec::new()
    }

    /// The formula tree a cell holds (relative to that cell), if it holds one.
    fn formula_tree(&self, _tab: TabId, _row: u32, _col: u32) -> Option<Arc<Expr>> {
        None
    }

    /// A column's width in characters of the default font, when it isn't the default.
    fn col_width(&self, _tab: TabId, _col: u32) -> Option<f64> {
        None
    }

    /// The style a cell shows.
    fn cell_style(&self, _tab: TabId, _row: u32, _col: u32) -> Option<Style> {
        None
    }

    /// Indexes of lookup ranges shared by every formula of one recalculation, when the grid keeps them.
    fn lookup_cache(&self) -> Option<&LookupCache> {
        None
    }
}

/// Indexes of the ranges lookups search (a VLOOKUP filled down a million rows reads its table once instead of a
/// million times). An entry is dropped as soon as a cell inside its range changes, and the grid drops everything
/// when a recalculation starts and ends. What an entry holds is up to the function that made it.
pub struct LookupCache {
    entries: std::sync::Mutex<Vec<CacheEntry>>,
    len: std::sync::atomic::AtomicUsize,
}

struct CacheEntry {
    area: Area,
    kind: u8,
    data: Arc<dyn std::any::Any + Send + Sync>,
}

/// How many ranges the cache keeps at once; the oldest goes first.
const CACHE_ENTRIES: usize = 32;

impl Default for LookupCache {
    fn default() -> Self {
        Self { entries: std::sync::Mutex::new(Vec::new()), len: std::sync::atomic::AtomicUsize::new(0) }
    }
}

impl std::fmt::Debug for LookupCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LookupCache({} entries)", self.len.load(std::sync::atomic::Ordering::Relaxed))
    }
}

impl LookupCache {
    /// What was kept for `area` under `kind`.
    pub fn get(&self, area: &Area, kind: u8) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        if self.len.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return None;
        }
        let entries = self.entries.lock().ok()?;
        entries.iter().find(|e| e.area == *area && e.kind == kind).map(|e| e.data.clone())
    }

    /// Keeps `data` for `area` under `kind`, replacing what was there.
    pub fn put(&self, area: Area, kind: u8, data: Arc<dyn std::any::Any + Send + Sync>) {
        let Ok(mut entries) = self.entries.lock() else { return };
        entries.retain(|e| !(e.area == area && e.kind == kind));
        if entries.len() >= CACHE_ENTRIES {
            entries.remove(0);
        }
        entries.push(CacheEntry { area, kind, data });
        self.len.store(entries.len(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Drops every entry whose range holds one of these cells of `tab`.
    pub fn invalidate(&self, tab: TabId, cells: &[(u32, u32)]) {
        if cells.is_empty() || self.len.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else { return };
        entries.retain(|e| {
            e.area.tab != tab
                || !cells.iter().any(|&(r, c)| r >= e.area.rect.r1 && r <= e.area.rect.r2 && c >= e.area.rect.c1 && c <= e.area.rect.c2)
        });
        self.len.store(entries.len(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
        self.len.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A rectangle of cells in a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub tab: TabId,
    pub rect: Rect,
}

impl Area {
    pub fn is_cell(&self) -> bool {
        self.rect.r1 == self.rect.r2 && self.rect.c1 == self.rect.c2
    }

    /// Whether it runs to the last row (a whole column, `A:A`) or the last column (a whole row, `3:3`).
    pub fn is_open(&self) -> bool {
        self.rect.r2 == MAX_ROWS - 1 || self.rect.c2 == MAX_COLS - 1
    }
}

/// An array of values, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Array {
    pub rows: u32,
    pub cols: u32,
    pub data: Vec<Value>,
}

impl Array {
    pub fn new(rows: u32, cols: u32, data: Vec<Value>) -> Self {
        debug_assert_eq!(data.len(), rows as usize * cols as usize);
        Self { rows, cols, data }
    }

    pub fn filled(rows: u32, cols: u32, v: Value) -> Self {
        Self { rows, cols, data: vec![v; rows as usize * cols as usize] }
    }

    pub fn column(values: Vec<Value>) -> Self {
        Self { rows: values.len() as u32, cols: 1, data: values }
    }

    pub fn row(values: Vec<Value>) -> Self {
        Self { rows: 1, cols: values.len() as u32, data: values }
    }

    pub fn get(&self, r: u32, c: u32) -> &Value {
        // Single rows and columns broadcast.
        let r = if self.rows == 1 { 0 } else { r };
        let c = if self.cols == 1 { 0 } else { c };
        self.data.get((r * self.cols + c) as usize).unwrap_or(&Value::Error(ErrorKind::NA))
    }
}

/// A name bound by LET or a LAMBDA's parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub name: Arc<str>,
    pub value: Res,
    /// A LAMBDA parameter the call left out (ISOMITTED is TRUE for it).
    pub omitted: bool,
}

/// A function value: what LAMBDA makes, or a built-in function named as a value (`BYROW(A1:C3, SUM)`).
#[derive(Clone, Debug, PartialEq)]
pub struct Lambda {
    pub params: Vec<Arc<str>>,
    pub body: Arc<Expr>,
    /// The LET names and parameters it could see where it was written.
    pub captured: Vec<Binding>,
    /// The cell its references are relative to.
    pub tab: TabId,
    pub row: u32,
    pub col: u32,
    /// For a built-in function used as a value: its name.
    pub builtin: Option<Arc<str>>,
}

/// What an expression evaluates to.
#[derive(Clone, Debug, PartialEq)]
pub enum Res {
    V(Value),
    R(Area),
    A(Arc<Array>),
    /// A LAMBDA (a cell can't show one: it shows #CALC!).
    L(Arc<Lambda>),
    /// A reference to several areas, made by the union operator: `(A1:B2,D1:E2)`.
    M(Arc<Vec<Area>>),
}

impl Res {
    pub fn err(e: ErrorKind) -> Self {
        Self::V(Value::Error(e))
    }

    pub fn num(n: f64) -> Self {
        if n.is_finite() { Self::V(Value::Number(n)) } else { Self::err(ErrorKind::Num) }
    }

    pub fn as_error(&self) -> Option<ErrorKind> {
        match self {
            Self::V(Value::Error(e)) => Some(*e),
            _ => None,
        }
    }

    /// Rows and columns of a range or array (1×1 for a value).
    pub fn dims(&self) -> (u32, u32) {
        match self {
            Self::V(_) | Self::L(_) | Self::M(_) => (1, 1),
            Self::R(a) => (a.rect.rows(), a.rect.cols()),
            Self::A(a) => (a.rows, a.cols),
        }
    }

    /// What a cell shows for a result that is neither a value, a range nor an array.
    pub fn shown_error(&self) -> Value {
        match self {
            Self::V(Value::Error(e)) => Value::Error(*e),
            // Several areas can't be shown in one cell.
            Self::M(_) => Value::Error(ErrorKind::Value),
            _ => Value::Error(ErrorKind::Calc),
        }
    }

    /// The areas a reference result covers (one for a range, several for a union).
    pub fn areas(&self) -> Option<Vec<Area>> {
        match self {
            Self::R(a) => Some(vec![*a]),
            Self::M(m) => Some(m.as_ref().clone()),
            _ => None,
        }
    }
}

/// Excel's text for a number used as text (`=1/3&""`): up to 15 significant digits, scientific when very large or
/// very small.
pub fn number_text(n: f64) -> String {
    if !n.is_finite() {
        return "#NUM!".to_owned();
    }
    if n == 0.0 {
        return "0".to_owned();
    }
    let abs = n.abs();
    if (1e-9..1e15).contains(&abs) {
        let digits_before = abs.log10().floor() as i32 + 1;
        let decimals = (15 - digits_before).clamp(0, 30) as usize;
        let s = format!("{:.*}", decimals, n);
        let s = trim_zeros(&s);
        // Rounding can carry into a sixteenth digit (999999999999999.9): fall through to scientific then.
        let digits = s.bytes().filter(u8::is_ascii_digit).count();
        if s != "-0" && digits <= 16 {
            return s;
        }
    }
    let s = format!("{:.14E}", n);
    let (mant, exp) = s.split_once('E').unwrap_or((&s, "0"));
    let mant = trim_zeros(mant);
    let exp: i32 = exp.parse().unwrap_or(0);
    format!("{mant}E{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s.to_owned() }
}

/// Excel's rule for a value used where text is expected (`=A1&"x"`), with numbers written to 15 digits.
pub fn text_of(v: &Value) -> Result<Arc<str>, ErrorKind> {
    match v {
        Value::Number(n) => Ok(Arc::from(number_text(*n).as_str())),
        other => other.coerce_text(),
    }
}

/// LET and LAMBDA names are case-insensitive, and Excel files write them with an `_xlpm.` prefix.
fn plain_name(n: &str) -> &str {
    if n.len() > 6 && n[..6].eq_ignore_ascii_case("_xlpm.") { &n[6..] } else { n }
}

pub struct Ctx<'g> {
    pub grid: &'g dyn Grid,
    pub tab: TabId,
    pub row: u32,
    pub col: u32,
    pub missing: Vec<Key>,
    scopes: Vec<Binding>,
    depth: u32,
}

/// How deep evaluation may nest (formulas inside formulas, LAMBDAs calling themselves) before giving #NUM!.
const MAX_DEPTH: u32 = 400;

impl<'g> Ctx<'g> {
    pub fn new(grid: &'g dyn Grid, tab: TabId, row: u32, col: u32) -> Self {
        Self { grid, tab, row, col, missing: Vec::new(), scopes: Vec::new(), depth: 0 }
    }

    /// Reads one cell's value, noting it when its formula is not recalculated yet.
    pub fn cell(&mut self, tab: TabId, row: u32, col: u32) -> Value {
        let before = self.missing.len();
        self.grid.pending(tab, &Rect::new(row, col, row, col), &mut self.missing);
        if self.missing.len() > before {
            return Value::Empty;
        }
        self.grid.value(tab, row, col)
    }

    /// The part of an area that can hold values (clipped to the tab's ids), noting cells still to recalculate.
    pub fn live_rect(&mut self, a: &Area) -> Option<Rect> {
        let (rows, cols) = self.grid.extent(a.tab)?;
        if rows == 0 || cols == 0 {
            return None;
        }
        let r = a.rect.intersection(&Rect::new(0, 0, rows - 1, cols - 1))?;
        self.grid.pending(a.tab, &r, &mut self.missing);
        Some(r)
    }

    /// Visits every value of a range or array (empty cells included, but only within the tab's ids for ranges).
    /// Returns how many cells were not visited because they lie beyond the tab's ids (all empty).
    pub fn each(&mut self, res: &Res, mut f: impl FnMut(&Value)) -> u64 {
        self.each_dyn(res, &mut f)
    }

    /// `each` without a generic closure, so visiting several areas doesn't nest closure types.
    fn each_dyn(&mut self, res: &Res, f: &mut dyn FnMut(&Value)) -> u64 {
        match res {
            Res::V(v) => {
                f(v);
                0
            }
            Res::A(a) => {
                a.data.iter().for_each(&mut *f);
                0
            }
            Res::L(_) => {
                f(&Value::Error(ErrorKind::Calc));
                0
            }
            Res::R(area) => {
                let total = area.rect.rows() as u64 * area.rect.cols() as u64;
                let Some(r) = self.live_rect(area) else { return total };
                let grid = self.grid;
                grid.visit(area.tab, &r, &mut |_, _, v| {
                    f(v);
                    true
                });
                total - r.rows() as u64 * r.cols() as u64
            }
            Res::M(areas) => {
                let mut unvisited = 0;
                for a in areas.iter() {
                    unvisited += self.each_dyn(&Res::R(*a), &mut *f);
                }
                unvisited
            }
        }
    }

    /// Like `each`, without text payloads (numbers, logicals, errors, and whether a cell holds text).
    pub fn each_kind(&mut self, res: &Res, mut f: impl FnMut(Kind)) -> u64 {
        self.each_kind_dyn(res, &mut f)
    }

    fn each_kind_dyn(&mut self, res: &Res, f: &mut dyn FnMut(Kind)) -> u64 {
        match res {
            Res::R(area) => {
                let total = area.rect.rows() as u64 * area.rect.cols() as u64;
                let Some(r) = self.live_rect(area) else { return total };
                let grid = self.grid;
                grid.visit_kinds(area.tab, &r, &mut |_, _, k| {
                    f(k);
                    true
                });
                total - r.rows() as u64 * r.cols() as u64
            }
            Res::M(areas) => {
                let mut unvisited = 0;
                for a in areas.iter() {
                    unvisited += self.each_kind_dyn(&Res::R(*a), &mut *f);
                }
                unvisited
            }
            other => self.each_dyn(other, &mut |v| f(Kind::of(v))),
        }
    }

    /// The rows and columns an area reads as an array: whole columns and rows stop at the last row or column
    /// with ids.
    pub fn array_dims(&mut self, area: &Area) -> (u32, u32) {
        let (rows, cols) = (area.rect.rows(), area.rect.cols());
        if !area.is_open() {
            return (rows, cols);
        }
        let (er, ec) = self.grid.extent(area.tab).unwrap_or((0, 0));
        let rr = if area.rect.r2 == MAX_ROWS - 1 { er.saturating_sub(area.rect.r1).min(rows) } else { rows };
        let cc = if area.rect.c2 == MAX_COLS - 1 { ec.saturating_sub(area.rect.c1).min(cols) } else { cols };
        (rr, cc)
    }

    /// Every value of a range or array in row-major order as an array (materialized).
    pub fn to_array(&mut self, res: &Res) -> Arc<Array> {
        match res {
            Res::V(v) => Arc::new(Array::new(1, 1, vec![v.clone()])),
            Res::A(a) => a.clone(),
            Res::L(_) => Arc::new(Array::new(1, 1, vec![Value::Error(ErrorKind::Calc)])),
            Res::M(_) => Arc::new(Array::new(1, 1, vec![Value::Error(ErrorKind::Value)])),
            Res::R(area) => {
                let (rows, cols) = self.array_dims(area);
                let cells = rows as u64 * cols as u64;
                if cells > 50_000_000 {
                    return Arc::new(Array::new(1, 1, vec![Value::Error(ErrorKind::Num)]));
                }
                let mut data = vec![Value::Empty; cells as usize];
                if cells > 0 {
                    let wanted = Area {
                        tab: area.tab,
                        rect: Rect::new(area.rect.r1, area.rect.c1, area.rect.r1 + rows - 1, area.rect.c1 + cols - 1),
                    };
                    if let Some(live) = self.live_rect(&wanted) {
                        let (r1, c1, w) = (area.rect.r1, area.rect.c1, cols as usize);
                        self.grid.visit(area.tab, &live, &mut |r, c, v| {
                            data[(r - r1) as usize * w + (c - c1) as usize] = v.clone();
                            true
                        });
                    }
                }
                Arc::new(Array::new(rows, cols, data))
            }
        }
    }

    /// Evaluates to a single value: a one-cell range is read, a bigger range is intersected implicitly with the
    /// formula's own row or column (Excel's @), an array gives its first value.
    pub fn scalar(&mut self, e: &Expr) -> Value {
        let r = self.eval(e);
        self.to_scalar(r)
    }

    pub fn to_scalar(&mut self, r: Res) -> Value {
        match r {
            Res::V(v) => v,
            Res::A(a) => a.data.first().cloned().unwrap_or(Value::Error(ErrorKind::Calc)),
            Res::L(_) => Value::Error(ErrorKind::Calc),
            Res::M(_) => Value::Error(ErrorKind::Value),
            Res::R(area) => {
                if area.is_cell() {
                    return self.cell(area.tab, area.rect.r1, area.rect.c1);
                }
                let rect = area.rect;
                if rect.c1 == rect.c2 && rect.r1 <= self.row && self.row <= rect.r2 {
                    return self.cell(area.tab, self.row, rect.c1);
                }
                if rect.r1 == rect.r2 && rect.c1 <= self.col && self.col <= rect.c2 {
                    return self.cell(area.tab, rect.r1, self.col);
                }
                Value::Error(ErrorKind::Value)
            }
        }
    }

    pub fn number(&mut self, e: &Expr) -> Result<f64, ErrorKind> {
        self.scalar(e).coerce_number()
    }

    pub fn text(&mut self, e: &Expr) -> Result<Arc<str>, ErrorKind> {
        text_of(&self.scalar(e))
    }

    pub fn boolean(&mut self, e: &Expr) -> Result<bool, ErrorKind> {
        self.scalar(e).coerce_bool()
    }

    fn resolve_cell(&self, c: &CellRef) -> Result<Area, ErrorKind> {
        let tab = c.tab.unwrap_or(self.tab);
        let row = c.row.resolve(self.row).filter(|&r| r < MAX_ROWS).ok_or(ErrorKind::Ref)?;
        let col = c.col.resolve(self.col).filter(|&c| c < MAX_COLS).ok_or(ErrorKind::Ref)?;
        if self.grid.extent(tab).is_none() {
            return Err(ErrorKind::Ref);
        }
        Ok(Area { tab, rect: Rect::new(row, col, row, col) })
    }

    pub fn resolve_range(&self, r: &RangeRef) -> Result<Area, ErrorKind> {
        let tab = r.tab.unwrap_or(self.tab);
        if self.grid.extent(tab).is_none() {
            return Err(ErrorKind::Ref);
        }
        let (r1, r2) = match r.rows {
            Some((a, b)) => (
                a.resolve(self.row).filter(|&x| x < MAX_ROWS).ok_or(ErrorKind::Ref)?,
                b.resolve(self.row).filter(|&x| x < MAX_ROWS).ok_or(ErrorKind::Ref)?,
            ),
            None => (0, MAX_ROWS - 1),
        };
        let (c1, c2) = match r.cols {
            Some((a, b)) => (
                a.resolve(self.col).filter(|&x| x < MAX_COLS).ok_or(ErrorKind::Ref)?,
                b.resolve(self.col).filter(|&x| x < MAX_COLS).ok_or(ErrorKind::Ref)?,
            ),
            None => (0, MAX_COLS - 1),
        };
        Ok(Area { tab, rect: Rect::new(r1, c1, r2, c2) })
    }

    pub fn bind(&mut self, name: Arc<str>, value: Res) {
        self.scopes.push(Binding { name, value, omitted: false });
    }

    pub fn unbind(&mut self, n: usize) {
        let len = self.scopes.len();
        self.scopes.truncate(len.saturating_sub(n));
    }

    /// Binds a LAMBDA parameter that the call left out.
    pub fn bind_omitted(&mut self, name: Arc<str>) {
        self.scopes.push(Binding { name, value: Res::V(Value::Empty), omitted: true });
    }

    /// The innermost LET name or LAMBDA parameter called `name`.
    pub fn lookup(&self, name: &str) -> Option<&Binding> {
        if self.scopes.is_empty() {
            return None;
        }
        let name = plain_name(name);
        self.scopes.iter().rev().find(|b| plain_name(&b.name).eq_ignore_ascii_case(name))
    }

    /// The names in scope (what a LAMBDA written here captures).
    pub fn scope(&self) -> &[Binding] {
        &self.scopes
    }

    /// How many evaluations are nested right now.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Evaluates an argument that must be a function: a LAMBDA, a name holding one, or a built-in function's name
    /// (`BYCOL(A1:C3, MAX)`).
    pub fn lambda_arg(&mut self, e: &Expr) -> Option<Arc<Lambda>> {
        if let Expr::Name(n) = e
            && self.lookup(n).is_none()
            && self.grid.defined_name(n, self.tab).is_none()
        {
            let mut upper = n.to_ascii_uppercase();
            for prefix in ["_XLFN._XLWS.", "_XLFN.", "_XLWS."] {
                if let Some(r) = upper.strip_prefix(prefix) {
                    upper = r.to_owned();
                }
            }
            if functions::is_known(&upper) {
                return Some(Arc::new(Lambda {
                    params: Vec::new(),
                    body: Arc::new(Expr::Missing),
                    captured: Vec::new(),
                    tab: self.tab,
                    row: self.row,
                    col: self.col,
                    builtin: Some(Arc::from(upper.as_str())),
                }));
            }
        }
        match self.eval(e) {
            Res::L(l) => Some(l),
            _ => None,
        }
    }

    /// Calls a function value. `None` arguments were left out.
    pub fn call_lambda(&mut self, lam: &Lambda, args: Vec<Option<Res>>) -> Res {
        if self.depth >= MAX_DEPTH {
            return Res::err(ErrorKind::Num);
        }
        if let Some(name) = &lam.builtin {
            // The arguments are already values: hand them over as hidden names the function reads back.
            let base = self.scopes.len();
            let mut exprs = Vec::with_capacity(args.len());
            for (i, a) in args.into_iter().enumerate() {
                match a {
                    Some(v) => {
                        let n = hidden_name(i);
                        self.scopes.push(Binding { name: n.clone(), value: v, omitted: false });
                        exprs.push(Expr::Name(n));
                    }
                    None => exprs.push(Expr::Missing),
                }
            }
            self.depth += 1;
            let out = functions::call(self, name, &exprs);
            self.depth -= 1;
            self.scopes.truncate(base);
            return out;
        }
        if args.len() > lam.params.len() {
            return Res::err(ErrorKind::Value);
        }
        let mut scope = lam.captured.clone();
        let mut args = args.into_iter();
        for p in &lam.params {
            match args.next().flatten() {
                Some(v) => scope.push(Binding { name: p.clone(), value: v, omitted: false }),
                None => scope.push(Binding { name: p.clone(), value: Res::V(Value::Empty), omitted: true }),
            }
        }
        let saved = std::mem::replace(&mut self.scopes, scope);
        let (tab, row, col) = (self.tab, self.row, self.col);
        self.tab = lam.tab;
        self.row = lam.row;
        self.col = lam.col;
        let out = self.eval(&lam.body);
        self.tab = tab;
        self.row = row;
        self.col = col;
        self.scopes = saved;
        out
    }

    pub fn eval(&mut self, e: &Expr) -> Res {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Res::err(ErrorKind::Num);
        }
        let out = self.eval_inner(e);
        self.depth -= 1;
        out
    }

    fn eval_inner(&mut self, e: &Expr) -> Res {
        match e {
            Expr::Number(n) => Res::V(Value::Number(*n)),
            Expr::Text(s) => Res::V(Value::Text(s.clone())),
            Expr::Bool(b) => Res::V(Value::Bool(*b)),
            Expr::Error(k) => Res::err(*k),
            Expr::Missing => Res::V(Value::Empty),
            Expr::Cell(c) => match self.resolve_cell(c) {
                Ok(a) => Res::R(a),
                Err(k) => Res::err(k),
            },
            Expr::Range(r) => match self.resolve_range(r) {
                Ok(a) => Res::R(a),
                Err(k) => Res::err(k),
            },
            Expr::Spill(c) => match self.resolve_cell(c) {
                Ok(a) => {
                    let (r, col) = (a.rect.r1, a.rect.c1);
                    // The anchor must be calculated before its extent is known.
                    let before = self.missing.len();
                    self.grid.pending(a.tab, &a.rect, &mut self.missing);
                    if self.missing.len() > before {
                        return Res::V(Value::Empty);
                    }
                    match self.grid.spill_extent(a.tab, r, col) {
                        Some((h, w)) => Res::R(Area { tab: a.tab, rect: Rect::new(r, col, r + h - 1, col + w - 1) }),
                        None => Res::err(ErrorKind::Ref),
                    }
                }
                Err(k) => Res::err(k),
            },
            Expr::Name(n) => {
                if let Some(b) = self.lookup(n) {
                    return b.value.clone();
                }
                match self.grid.defined_name(n, self.tab) {
                    Some(expr) => {
                        // Names are written as if in A1; evaluate them there.
                        let (r, c) = (self.row, self.col);
                        self.row = 0;
                        self.col = 0;
                        let out = self.eval(&expr);
                        self.row = r;
                        self.col = c;
                        out
                    }
                    None => Res::err(ErrorKind::Name),
                }
            }
            Expr::Paren(x) => self.eval(x),
            Expr::Unary(op, x) => {
                let v = self.eval(x);
                self.map1(v, |v| {
                    let n = match v.coerce_number() {
                        Ok(n) => n,
                        Err(e) => return Value::Error(e),
                    };
                    match op {
                        UnaryOp::Neg => Value::Number(-n),
                        UnaryOp::Plus => v.clone(),
                        UnaryOp::Percent => Value::Number(n / 100.0),
                    }
                })
            }
            Expr::Binary(BinaryOp::Range, a, b) => {
                let (ra, rb) = (self.eval(a), self.eval(b));
                match (ra, rb) {
                    (Res::R(x), Res::R(y)) if x.tab == y.tab => Res::R(Area {
                        tab: x.tab,
                        rect: Rect::new(
                            x.rect.r1.min(y.rect.r1),
                            x.rect.c1.min(y.rect.c1),
                            x.rect.r2.max(y.rect.r2),
                            x.rect.c2.max(y.rect.c2),
                        ),
                    }),
                    (Res::V(Value::Error(e)), _) | (_, Res::V(Value::Error(e))) => Res::err(e),
                    _ => Res::err(ErrorKind::Value),
                }
            }
            Expr::Binary(BinaryOp::Union, a, b) => {
                let (ra, rb) = (self.eval(a), self.eval(b));
                if let Some(e) = ra.as_error().or(rb.as_error()) {
                    return Res::err(e);
                }
                match (ra.areas(), rb.areas()) {
                    (Some(mut x), Some(y)) => {
                        x.extend(y);
                        Res::M(Arc::new(x))
                    }
                    _ => Res::err(ErrorKind::Value),
                }
            }
            Expr::Binary(BinaryOp::Intersect, a, b) => {
                let (ra, rb) = (self.eval(a), self.eval(b));
                match (ra, rb) {
                    (Res::R(x), Res::R(y)) if x.tab == y.tab => match x.rect.intersection(&y.rect) {
                        Some(r) => Res::R(Area { tab: x.tab, rect: r }),
                        None => Res::err(ErrorKind::Null),
                    },
                    _ => Res::err(ErrorKind::Value),
                }
            }
            Expr::Binary(op, a, b) => {
                let ra = self.eval(a);
                let rb = self.eval(b);
                let op = *op;
                self.map2(ra, rb, |x, y| binary(op, x, y))
            }
            Expr::Call(name, args) => functions::call(self, name, args),
            Expr::Invoke(callee, args) => match self.eval(callee) {
                Res::L(lam) => {
                    let vals = args.iter().map(|a| (!matches!(a, Expr::Missing)).then(|| self.eval(a))).collect();
                    self.call_lambda(&lam, vals)
                }
                Res::V(Value::Error(e)) => Res::err(e),
                _ => Res::err(ErrorKind::Value),
            },
            Expr::Array(rows) => {
                let r = rows.len() as u32;
                let c = rows.first().map_or(0, |x| x.len()) as u32;
                let mut data = Vec::with_capacity((r * c) as usize);
                for row in rows {
                    for item in row {
                        let v = self.scalar(item);
                        data.push(v);
                    }
                }
                Res::A(Arc::new(Array::new(r, c, data)))
            }
        }
    }

    /// Applies `f` to a value, or to every value of a range or array (giving an array).
    pub fn map1(&mut self, r: Res, f: impl Fn(&Value) -> Value) -> Res {
        match r {
            Res::V(v) => Res::V(f(&v)),
            Res::L(_) => Res::err(ErrorKind::Calc),
            Res::M(_) => Res::err(ErrorKind::Value),
            Res::R(a) if a.is_cell() => {
                let v = self.cell(a.tab, a.rect.r1, a.rect.c1);
                Res::V(f(&v))
            }
            other => {
                let arr = self.to_array(&other);
                Res::A(Arc::new(Array::new(arr.rows, arr.cols, arr.data.iter().map(f).collect())))
            }
        }
    }

    /// Applies `f` pairwise, broadcasting single rows, columns and values like Excel's dynamic arrays.
    pub fn map2(&mut self, a: Res, b: Res, f: impl Fn(&Value, &Value) -> Value) -> Res {
        let single = |r: &Res| match r {
            Res::V(_) | Res::L(_) | Res::M(_) => true,
            Res::R(a) => a.is_cell(),
            Res::A(a) => a.rows == 1 && a.cols == 1,
        };
        if single(&a) && single(&b) {
            let x = self.to_scalar(a);
            let y = self.to_scalar(b);
            return Res::V(f(&x, &y));
        }
        let x = self.to_array(&a);
        let y = self.to_array(&b);
        let rows = broadcast_dim(x.rows, y.rows);
        let cols = broadcast_dim(x.cols, y.cols);
        let mut data = Vec::with_capacity(rows as usize * cols as usize);
        for r in 0..rows {
            for c in 0..cols {
                let in_x = (x.rows == 1 || r < x.rows) && (x.cols == 1 || c < x.cols);
                let in_y = (y.rows == 1 || r < y.rows) && (y.cols == 1 || c < y.cols);
                if in_x && in_y {
                    data.push(f(x.get(r, c), y.get(r, c)));
                } else {
                    data.push(Value::Error(ErrorKind::NA));
                }
            }
        }
        Res::A(Arc::new(Array::new(rows, cols, data)))
    }
}

fn hidden_name(i: usize) -> Arc<str> {
    static NAMES: std::sync::OnceLock<Vec<Arc<str>>> = std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| (0..16).map(|i| Arc::from(format!("\u{1}{i}").as_str())).collect());
    names.get(i).cloned().unwrap_or_else(|| Arc::from(format!("\u{1}{i}").as_str()))
}

pub fn broadcast_dim(a: u32, b: u32) -> u32 {
    if a == 1 { b } else if b == 1 { a } else { a.max(b) }
}

pub fn binary(op: BinaryOp, x: &Value, y: &Value) -> Value {
    use BinaryOp::*;
    match op {
        Add | Sub | Mul | Div | Pow => {
            let a = match x.coerce_number() {
                Ok(n) => n,
                Err(e) => return Value::Error(e),
            };
            let b = match y.coerce_number() {
                Ok(n) => n,
                Err(e) => return Value::Error(e),
            };
            let r = match op {
                Add => a + b,
                Sub => a - b,
                Mul => a * b,
                Div => {
                    if b == 0.0 {
                        return Value::Error(ErrorKind::Div0);
                    }
                    a / b
                }
                _ => {
                    if a == 0.0 && b == 0.0 {
                        return Value::Error(ErrorKind::Num);
                    }
                    if a == 0.0 && b < 0.0 {
                        return Value::Error(ErrorKind::Div0);
                    }
                    a.powf(b)
                }
            };
            if r.is_finite() { Value::Number(r) } else { Value::Error(ErrorKind::Num) }
        }
        Concat => {
            let a = match text_of(x) {
                Ok(s) => s,
                Err(e) => return Value::Error(e),
            };
            let b = match text_of(y) {
                Ok(s) => s,
                Err(e) => return Value::Error(e),
            };
            let mut s = String::with_capacity(a.len() + b.len());
            s.push_str(&a);
            s.push_str(&b);
            Value::from(s)
        }
        Eq | Ne | Lt | Le | Gt | Ge => {
            if let Value::Error(e) = x {
                return Value::Error(*e);
            }
            if let Value::Error(e) = y {
                return Value::Error(*e);
            }
            let ord = compare(x, y);
            use std::cmp::Ordering::*;
            Value::Bool(match op {
                Eq => ord == Equal,
                Ne => ord != Equal,
                Lt => ord == Less,
                Le => ord != Greater,
                Gt => ord == Greater,
                _ => ord != Less,
            })
        }
        Range | Intersect | Union => Value::Error(ErrorKind::Value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_as_text() {
        assert_eq!(number_text(1.0 / 3.0), "0.333333333333333");
        assert_eq!(number_text(2.0 / 3.0), "0.666666666666667");
        assert_eq!(number_text(0.1 + 0.2), "0.3");
        assert_eq!(number_text(123456789012345.0), "123456789012345");
        assert_eq!(number_text(1e15), "1E+15");
        assert_eq!(number_text(1234567890123456.0), "1.23456789012346E+15");
        assert_eq!(number_text(-2.5), "-2.5");
        assert_eq!(number_text(1e-10), "1E-10");
        assert_eq!(number_text(0.0001), "0.0001");
        assert_eq!(number_text(46312.0), "46312");
    }
}
