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
    value::{ErrorKind, Value, compare},
};

/// A cell by tab and position.
pub type Key = (TabId, u32, u32);

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

/// What an expression evaluates to.
#[derive(Clone, Debug, PartialEq)]
pub enum Res {
    V(Value),
    R(Area),
    A(Arc<Array>),
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
            Self::V(_) => (1, 1),
            Self::R(a) => (a.rect.rows(), a.rect.cols()),
            Self::A(a) => (a.rows, a.cols),
        }
    }
}

pub struct Ctx<'g> {
    pub grid: &'g dyn Grid,
    pub tab: TabId,
    pub row: u32,
    pub col: u32,
    pub missing: Vec<Key>,
    scopes: Vec<(Arc<str>, Res)>,
    depth: u32,
}

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
        match res {
            Res::V(v) => {
                f(v);
                0
            }
            Res::A(a) => {
                a.data.iter().for_each(&mut f);
                0
            }
            Res::R(area) => {
                let total = area.rect.rows() as u64 * area.rect.cols() as u64;
                let Some(r) = self.live_rect(area) else { return total };
                let grid = self.grid;
                for row in r.r1..=r.r2 {
                    for col in r.c1..=r.c2 {
                        f(&grid.value(area.tab, row, col));
                    }
                }
                total - r.rows() as u64 * r.cols() as u64
            }
        }
    }

    /// Every value of a range or array in row-major order as an array (materialized).
    pub fn to_array(&mut self, res: &Res) -> Arc<Array> {
        match res {
            Res::V(v) => Arc::new(Array::new(1, 1, vec![v.clone()])),
            Res::A(a) => a.clone(),
            Res::R(area) => {
                let rows = area.rect.rows();
                let cols = area.rect.cols();
                let live = self.live_rect(area);
                let cells = rows as u64 * cols as u64;
                if cells > 50_000_000 {
                    return Arc::new(Array::new(1, 1, vec![Value::Error(ErrorKind::Num)]));
                }
                // Whole columns: stop at the last row with ids.
                let (rows, cols) = match live {
                    Some(l) if area.rect.r2 == MAX_ROWS - 1 || area.rect.c2 == MAX_COLS - 1 => {
                        let rr = if area.rect.r2 == MAX_ROWS - 1 { l.r2 - area.rect.r1 + 1 } else { rows };
                        let cc = if area.rect.c2 == MAX_COLS - 1 { l.c2 - area.rect.c1 + 1 } else { cols };
                        (rr, cc)
                    }
                    None if area.rect.r2 == MAX_ROWS - 1 || area.rect.c2 == MAX_COLS - 1 => (0, 0),
                    _ => (rows, cols),
                };
                let mut data = Vec::with_capacity(rows as usize * cols as usize);
                for r in 0..rows {
                    for c in 0..cols {
                        data.push(self.grid.value(area.tab, area.rect.r1 + r, area.rect.c1 + c));
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
        self.scalar(e).coerce_text()
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
        self.scopes.push((name, value));
    }

    pub fn unbind(&mut self, n: usize) {
        let len = self.scopes.len();
        self.scopes.truncate(len.saturating_sub(n));
    }

    pub fn eval(&mut self, e: &Expr) -> Res {
        self.depth += 1;
        if self.depth > 512 {
            self.depth -= 1;
            return Res::err(ErrorKind::Calc);
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
                if let Some((_, v)) = self.scopes.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                    return v.clone();
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
            Res::V(_) => true,
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

fn broadcast_dim(a: u32, b: u32) -> u32 {
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
                    a.powf(b)
                }
            };
            if r.is_finite() { Value::Number(r) } else { Value::Error(ErrorKind::Num) }
        }
        Concat => {
            let a = match x.coerce_text() {
                Ok(s) => s,
                Err(e) => return Value::Error(e),
            };
            let b = match y.coerce_text() {
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
        Range | Intersect => Value::Error(ErrorKind::Value),
    }
}
