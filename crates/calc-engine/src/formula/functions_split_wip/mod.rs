//! Excel's functions. `call` dispatches by name; each function takes its argument trees unevaluated so the
//! ones that short-circuit (IF, IFERROR, AND, CHOOSE, LET…) only evaluate what they need.
//!
//! The library is split by topic; this module holds the dispatch table and what every topic shares: reading
//! arguments, Excel's array lifting (a function of single values given a range or array answers with an array),
//! and the passes aggregates make over ranges without copying them.

use std::sync::{Arc, OnceLock};

use ahash::{HashMap, HashMapExt as _};

use super::{
    ast::Expr,
    eval::{Area, Array, Ctx, Kind, Res, broadcast_dim, text_of},
};
use crate::value::{ErrorKind, Value};

macro_rules! num {
    ($ctx:expr, $e:expr) => {
        match $ctx.number($e) {
            Ok(n) => n,
            Err(err) => return Res::err(err),
        }
    };
}

macro_rules! txt {
    ($ctx:expr, $e:expr) => {
        match $ctx.text($e) {
            Ok(s) => s,
            Err(err) => return Res::err(err),
        }
    };
}

macro_rules! boolean {
    ($ctx:expr, $e:expr) => {
        match $ctx.boolean($e) {
            Ok(b) => b,
            Err(err) => return Res::err(err),
        }
    };
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return Res::err(err),
        }
    };
}

mod arrays;
mod criteria;
mod database;
mod datetime;
mod dist;
mod engineering;
mod financial;
mod info;
mod logic;
mod lookup;
mod math;
mod regex;
mod stats;
mod text;

type Func = fn(&mut Ctx<'_>, &[Expr]) -> Res;

fn table() -> &'static HashMap<&'static str, Func> {
    static TABLE: OnceLock<HashMap<&'static str, Func>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = HashMap::new();
        for list in [
            math::FUNCTIONS,
            stats::FUNCTIONS,
            dist::FUNCTIONS,
            criteria::FUNCTIONS,
            logic::FUNCTIONS,
            info::FUNCTIONS,
            text::FUNCTIONS,
            regex::FUNCTIONS,
            lookup::FUNCTIONS,
            datetime::FUNCTIONS,
            financial::FUNCTIONS,
            engineering::FUNCTIONS,
            database::FUNCTIONS,
            arrays::FUNCTIONS,
        ] {
            for &(name, f) in list {
                let previous = t.insert(name, f);
                debug_assert!(previous.is_none(), "{name} is defined twice");
            }
        }
        t
    })
}

/// Calls the function `name` (upper case) with these arguments.
pub fn call(ctx: &mut Ctx<'_>, name: &str, args: &[Expr]) -> Res {
    if let Some(f) = table().get(name) {
        return f(ctx, args);
    }
    logic::call_by_name(ctx, name, args)
}

/// Whether Calc knows a function (used to flag unknown names while typing).
pub fn is_known(name: &str) -> bool {
    table().contains_key(name.to_ascii_uppercase().as_str())
}

/// Every function name Calc knows, sorted.
pub fn names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = table().keys().copied().collect();
    v.sort_unstable();
    v
}

// ---- argument helpers --------------------------------------------------------------------------------------

fn bad_args() -> Res {
    Res::err(ErrorKind::Value)
}

fn arity(args: &[Expr], min: usize, max: usize) -> bool {
    args.len() >= min && args.len() <= max
}

/// An optional argument, `None` when absent or left empty.
fn opt(args: &[Expr], i: usize) -> Option<&Expr> {
    args.get(i).filter(|e| !matches!(e, Expr::Missing))
}

/// An optional number: `default` when absent or left empty.
fn opt_num(ctx: &mut Ctx<'_>, args: &[Expr], i: usize, default: f64) -> Result<f64, ErrorKind> {
    match opt(args, i) {
        Some(e) => ctx.number(e),
        None => Ok(default),
    }
}

/// An optional logical: `default` when absent or left empty.
fn opt_bool(ctx: &mut Ctx<'_>, args: &[Expr], i: usize, default: bool) -> Result<bool, ErrorKind> {
    match opt(args, i) {
        Some(e) => ctx.boolean(e),
        None => Ok(default),
    }
}

/// A legacy optional argument: `default` when absent, but 0 (or FALSE, or "") when left empty, as Excel's older
/// functions read `VLOOKUP(x, t, 2,)`.
fn legacy_num(ctx: &mut Ctx<'_>, args: &[Expr], i: usize, default: f64) -> Result<f64, ErrorKind> {
    match args.get(i) {
        None => Ok(default),
        Some(Expr::Missing) => Ok(0.0),
        Some(e) => ctx.number(e),
    }
}

fn legacy_bool(ctx: &mut Ctx<'_>, args: &[Expr], i: usize, default: bool) -> Result<bool, ErrorKind> {
    match args.get(i) {
        None => Ok(default),
        Some(Expr::Missing) => Ok(false),
        Some(e) => ctx.boolean(e),
    }
}

fn num_result(n: f64) -> Value {
    if n.is_finite() { Value::Number(n) } else { Value::Error(ErrorKind::Num) }
}

fn as_num(v: &Value) -> Result<f64, ErrorKind> {
    v.coerce_number()
}

fn as_text(v: &Value) -> Result<Arc<str>, ErrorKind> {
    text_of(v)
}

/// A number argument that must be an integer, truncated toward zero, within `i64`.
fn int_of(n: f64) -> Result<i64, ErrorKind> {
    if !n.is_finite() || n.abs() >= 9.2e18 {
        return Err(ErrorKind::Num);
    }
    Ok(n.trunc() as i64)
}

/// Rounds away float noise: to 15 significant digits, as Excel keeps numbers.
fn clean15(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let digits = 15 - (x.abs().log10().floor() as i32 + 1);
    if !(-300..=300).contains(&digits) {
        return x;
    }
    let f = 10f64.powi(digits.abs());
    let r = if digits >= 0 { (x * f).round() / f } else { (x / f).round() * f };
    if r.is_finite() { r } else { x }
}

/// Whether a result is a single value (a value, a one-cell range, a 1×1 array).
fn is_single(r: &Res) -> bool {
    match r {
        Res::V(_) | Res::L(_) | Res::M(_) => true,
        Res::R(a) => a.is_cell(),
        Res::A(a) => a.rows == 1 && a.cols == 1,
    }
}

/// An argument evaluated, with an error value turned into `Err`.
fn eval_ok(ctx: &mut Ctx<'_>, e: &Expr) -> Result<Res, ErrorKind> {
    let r = ctx.eval(e);
    match r.as_error() {
        Some(err) => Err(err),
        None => Ok(r),
    }
}

/// An argument read as an array (a range is read whole; whole columns stop at the last row with ids).
fn array_arg(ctx: &mut Ctx<'_>, e: &Expr) -> Result<Arc<Array>, ErrorKind> {
    let r = eval_ok(ctx, e)?;
    if matches!(r, Res::M(_)) {
        return Err(ErrorKind::Value);
    }
    Ok(ctx.to_array(&r))
}

/// An argument read as a single value, an error value as `Err`.
fn scalar_ok(ctx: &mut Ctx<'_>, e: &Expr) -> Result<Value, ErrorKind> {
    match ctx.scalar(e) {
        Value::Error(err) => Err(err),
        v => Ok(v),
    }
}

/// An optional integer argument (truncated): `default` when absent or left empty.
fn opt_int(ctx: &mut Ctx<'_>, args: &[Expr], i: usize, default: i64) -> Result<i64, ErrorKind> {
    match opt(args, i) {
        Some(e) => int_of(ctx.number(e)?),
        None => Ok(default),
    }
}

/// A one-row array.
fn row_array(values: Vec<Value>) -> Res {
    Res::A(Arc::new(Array::row(values)))
}

/// A one-column array.
fn col_array(values: Vec<Value>) -> Res {
    Res::A(Arc::new(Array::column(values)))
}

// ---- lifting -----------------------------------------------------------------------------------------------

/// How a lifted function's argument is read when it is absent or left empty.
#[derive(Clone)]
enum Need {
    Required,
    /// Absent: this value. Left empty (`f(x,)`): an empty value (0, FALSE, "").
    Optional(Value),
    /// Absent or left empty: this value.
    Default(Value),
}

use Need::{Default as Def, Optional as Opt, Required as Req};

fn n(v: f64) -> Value {
    Value::Number(v)
}

/// Evaluates a lifted function's arguments.
fn lift_args(ctx: &mut Ctx<'_>, args: &[Expr], spec: &[Need]) -> Option<Vec<Res>> {
    let required = spec.iter().filter(|s| matches!(s, Need::Required)).count();
    if args.len() < required || args.len() > spec.len() {
        return None;
    }
    let mut out = Vec::with_capacity(spec.len());
    for (i, need) in spec.iter().enumerate() {
        let r = match (args.get(i), need) {
            (None, Need::Optional(v) | Need::Default(v)) => Res::V(v.clone()),
            (None, Need::Required) => return None,
            (Some(Expr::Missing), Need::Default(v)) => Res::V(v.clone()),
            (Some(e), _) => ctx.eval(e),
        };
        out.push(r);
    }
    Some(out)
}

/// Applies `f` to single values, or element by element when any argument is a range or array (Excel's lifting),
/// broadcasting single rows and columns. Elements outside a smaller array are #N/A.
fn lift_res<const N: usize>(
    ctx: &mut Ctx<'_>,
    vals: Vec<Res>,
    f: impl Fn([&Value; N]) -> Result<Value, ErrorKind>,
) -> Res {
    debug_assert_eq!(vals.len(), N);
    let run = |vs: [&Value; N]| f(vs).unwrap_or_else(Value::Error);
    if vals.iter().all(is_single) {
        let scalars: Vec<Value> = vals.into_iter().map(|r| ctx.to_scalar(r)).collect();
        return Res::V(run(std::array::from_fn(|i| &scalars[i])));
    }
    let arrays: Vec<Arc<Array>> = vals.iter().map(|r| ctx.to_array(r)).collect();
    let rows = arrays.iter().fold(1, |acc, a| broadcast_dim(acc, a.rows));
    let cols = arrays.iter().fold(1, |acc, a| broadcast_dim(acc, a.cols));
    let mut data = Vec::with_capacity(rows as usize * cols as usize);
    let na = Value::Error(ErrorKind::NA);
    for r in 0..rows {
        for c in 0..cols {
            let covered = arrays.iter().all(|a| (a.rows == 1 || r < a.rows) && (a.cols == 1 || c < a.cols));
            if covered {
                data.push(run(std::array::from_fn(|i| arrays[i].get(r, c))));
            } else {
                data.push(na.clone());
            }
        }
    }
    Res::A(Arc::new(Array::new(rows, cols, data)))
}

/// A lifted function of `N` arguments read by `spec`.
fn lifted<const N: usize>(
    ctx: &mut Ctx<'_>,
    args: &[Expr],
    spec: [Need; N],
    f: impl Fn([&Value; N]) -> Result<Value, ErrorKind>,
) -> Res {
    match lift_args(ctx, args, &spec) {
        Some(vals) => lift_res(ctx, vals, f),
        None => bad_args(),
    }
}

/// A lifted function of numbers: every argument is read as a number first (errors pass through, in order), and a
/// result that isn't finite is #NUM!.
fn lifted_num<const N: usize>(
    ctx: &mut Ctx<'_>,
    args: &[Expr],
    spec: [Need; N],
    f: impl Fn([f64; N]) -> Result<f64, ErrorKind>,
) -> Res {
    lifted(ctx, args, spec, |vs| {
        let mut ns = [0.0; N];
        for (i, v) in vs.iter().enumerate() {
            ns[i] = v.coerce_number()?;
        }
        Ok(num_result(f(ns)?))
    })
}

// ---- reading many values -----------------------------------------------------------------------------------

/// How an aggregate reads its arguments: SUM-like functions take only numbers from ranges and arrays (numbers,
/// numeric text and logicals typed directly); the A functions (AVERAGEA, MAXA…) also count text as 0 and
/// logicals as 0 or 1 from ranges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Agg {
    Numbers,
    All,
}

/// Feeds every number the arguments give to `f`, Excel style. The first error anywhere is the answer.
fn for_each_number(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg, f: &mut dyn FnMut(f64)) -> Result<(), ErrorKind> {
    for a in args {
        if matches!(a, Expr::Missing) {
            // An argument left empty is a 0 typed in.
            f(0.0);
            continue;
        }
        let r = ctx.eval(a);
        feed_numbers(ctx, &r, mode, true, f)?;
    }
    Ok(())
}

/// Feeds the numbers of one evaluated argument. `direct`: a value typed into the argument list (where text and
/// logicals are converted) rather than read from a range or array.
fn feed_numbers(ctx: &mut Ctx<'_>, r: &Res, mode: Agg, direct: bool, f: &mut dyn FnMut(f64)) -> Result<(), ErrorKind> {
    match r {
        Res::V(v) if direct => match v {
            Value::Empty => f(0.0),
            Value::Error(e) => return Err(*e),
            other => f(other.coerce_number()?),
        },
        Res::V(v) => feed_one(v, mode, f)?,
        Res::L(_) => return Err(ErrorKind::Calc),
        Res::M(areas) => {
            for a in areas.iter() {
                feed_numbers(ctx, &Res::R(*a), mode, false, f)?;
            }
        }
        Res::A(a) => {
            for v in &a.data {
                feed_one(v, mode, f)?;
            }
        }
        Res::R(area) => {
            let Some(rect) = ctx.live_rect(area) else { return Ok(()) };
            let mut err = None;
            ctx.grid.visit_kinds(area.tab, &rect, &mut |_, _, k| {
                match k {
                    Kind::Number(n) => f(n),
                    Kind::Error(e) => {
                        err = Some(e);
                        return false;
                    }
                    Kind::Bool(b) if mode == Agg::All => f(if b { 1.0 } else { 0.0 }),
                    Kind::Text if mode == Agg::All => f(0.0),
                    _ => {}
                }
                true
            });
            if let Some(e) = err {
                return Err(e);
            }
        }
    }
    Ok(())
}

fn feed_one(v: &Value, mode: Agg, f: &mut dyn FnMut(f64)) -> Result<(), ErrorKind> {
    match v {
        Value::Number(n) => f(*n),
        Value::Error(e) => return Err(*e),
        Value::Bool(b) if mode == Agg::All => f(if *b { 1.0 } else { 0.0 }),
        Value::Text(_) if mode == Agg::All => f(0.0),
        _ => {}
    }
    Ok(())
}

/// Every number the arguments give, in order.
fn numbers(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for_each_number(ctx, args, mode, &mut |n| out.push(n))?;
    Ok(out)
}

/// The numbers of one argument read as a data set (a range or array: only its numbers; a value: itself).
fn data_numbers(ctx: &mut Ctx<'_>, e: &Expr) -> Result<Vec<f64>, ErrorKind> {
    let r = ctx.eval(e);
    let mut out = Vec::new();
    feed_numbers(ctx, &r, Agg::Numbers, true, &mut |n| out.push(n))?;
    Ok(out)
}

/// Neumaier's compensated sum, so long columns of decimals add up the way people expect.
#[derive(Clone, Copy, Default)]
struct Sum {
    s: f64,
    c: f64,
}

impl Sum {
    fn add(&mut self, x: f64) {
        let t = self.s + x;
        if self.s.abs() >= x.abs() {
            self.c += (self.s - t) + x;
        } else {
            self.c += (x - t) + self.s;
        }
        self.s = t;
    }

    fn total(&self) -> f64 {
        self.s + self.c
    }
}

fn kahan(v: &[f64]) -> f64 {
    let mut s = Sum::default();
    for &x in v {
        s.add(x);
    }
    s.total()
}

// ---- reading ranges lazily ---------------------------------------------------------------------------------

/// A range or array that is read element by element: a range is read from the grid as needed, never copied.
#[derive(Clone)]
enum Src {
    Arr(Arc<Array>),
    Rng { area: Area, rows: u32, cols: u32 },
}

impl Src {
    /// Reads a result as a grid of values. Whole columns and rows stop at the last row or column with ids.
    fn of(ctx: &mut Ctx<'_>, r: &Res) -> Result<Src, ErrorKind> {
        match r {
            Res::V(Value::Error(e)) => Err(*e),
            Res::V(v) => Ok(Src::Arr(Arc::new(Array::new(1, 1, vec![v.clone()])))),
            Res::L(_) => Err(ErrorKind::Calc),
            Res::M(_) => Err(ErrorKind::Value),
            Res::A(a) => Ok(Src::Arr(a.clone())),
            Res::R(area) => {
                let (rows, cols) = ctx.array_dims(area);
                // Note cells still to recalculate, so the formula runs again after them.
                ctx.live_rect(area);
                Ok(Src::Rng { area: *area, rows, cols })
            }
        }
    }

    fn rows(&self) -> u32 {
        match self {
            Src::Arr(a) => a.rows,
            Src::Rng { rows, .. } => *rows,
        }
    }

    fn cols(&self) -> u32 {
        match self {
            Src::Arr(a) => a.cols,
            Src::Rng { cols, .. } => *cols,
        }
    }

    fn get(&self, ctx: &Ctx<'_>, r: u32, c: u32) -> Value {
        match self {
            Src::Arr(a) => a.data.get((r * a.cols + c) as usize).cloned().unwrap_or(Value::Error(ErrorKind::NA)),
            Src::Rng { area, .. } => ctx.grid.value(area.tab, area.rect.r1 + r, area.rect.c1 + c),
        }
    }

    /// The rows and columns (from the top left) that can hold values; everything beyond is empty.
    fn live_dims(&self, ctx: &Ctx<'_>) -> (u32, u32) {
        match self {
            Src::Arr(a) => (a.rows, a.cols),
            Src::Rng { area, rows, cols } => {
                let (er, ec) = ctx.grid.extent(area.tab).unwrap_or((0, 0));
                (er.saturating_sub(area.rect.r1).min(*rows), ec.saturating_sub(area.rect.c1).min(*cols))
            }
        }
    }

    /// Visits rows `rows` and columns `cols` (relative, exclusive ends) row by row; `f` returns false to stop.
    fn visit(&self, ctx: &Ctx<'_>, rows: std::ops::Range<u32>, cols: std::ops::Range<u32>, f: &mut dyn FnMut(u32, u32, &Value) -> bool) {
        if rows.is_empty() || cols.is_empty() {
            return;
        }
        match self {
            Src::Arr(a) => {
                for r in rows {
                    for c in cols.clone() {
                        if !f(r, c, &a.data[(r * a.cols + c) as usize]) {
                            return;
                        }
                    }
                }
            }
            Src::Rng { area, .. } => {
                let (r1, c1) = (area.rect.r1, area.rect.c1);
                let (lr, lc) = self.live_dims(ctx);
                // Beyond the tab's ids every cell is empty: those are visited without reading.
                let live_r_end = lr.clamp(rows.start, rows.end);
                let live_c_end = lc.clamp(cols.start, cols.end);
                let empty = Value::Empty;
                if live_r_end > rows.start && live_c_end == cols.end {
                    let rect = crate::addr::Rect::new(r1 + rows.start, c1 + cols.start, r1 + live_r_end - 1, c1 + cols.end - 1);
                    let mut go = true;
                    ctx.grid.visit(area.tab, &rect, &mut |gr, gc, v| {
                        go = f(gr - r1, gc - c1, v);
                        go
                    });
                    if !go {
                        return;
                    }
                } else {
                    for r in rows.start..live_r_end {
                        if live_c_end > cols.start {
                            let rect = crate::addr::Rect::new(r1 + r, c1 + cols.start, r1 + r, c1 + live_c_end - 1);
                            let mut go = true;
                            ctx.grid.visit(area.tab, &rect, &mut |gr, gc, v| {
                                go = f(gr - r1, gc - c1, v);
                                go
                            });
                            if !go {
                                return;
                            }
                        }
                        for c in live_c_end..cols.end {
                            if !f(r, c, &empty) {
                                return;
                            }
                        }
                    }
                }
                for r in live_r_end..rows.end {
                    for c in cols.clone() {
                        if !f(r, c, &empty) {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// The whole thing as an array.
    fn array(&self, ctx: &mut Ctx<'_>) -> Arc<Array> {
        match self {
            Src::Arr(a) => a.clone(),
            Src::Rng { area, .. } => ctx.to_array(&Res::R(*area)),
        }
    }

    /// A sub-block as a result: a range stays a reference.
    fn block(&self, r: u32, c: u32, rows: u32, cols: u32) -> Res {
        match self {
            Src::Rng { area, .. } => Res::R(Area {
                tab: area.tab,
                rect: crate::addr::Rect::new(
                    area.rect.r1 + r,
                    area.rect.c1 + c,
                    area.rect.r1 + r + rows - 1,
                    area.rect.c1 + c + cols - 1,
                ),
            }),
            Src::Arr(a) => {
                if rows == 1 && cols == 1 {
                    return Res::V(a.data[(r * a.cols + c) as usize].clone());
                }
                let mut data = Vec::with_capacity(rows as usize * cols as usize);
                for rr in r..r + rows {
                    for cc in c..c + cols {
                        data.push(a.data[(rr * a.cols + cc) as usize].clone());
                    }
                }
                Res::A(Arc::new(Array::new(rows, cols, data)))
            }
        }
    }
}

/// Builds an array from rows of values; rows shorter than the longest are padded with `pad`.
fn grid_array(rows: Vec<Vec<Value>>, pad: Value) -> Res {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if rows.is_empty() || width == 0 {
        return Res::err(ErrorKind::Calc);
    }
    let height = rows.len();
    let mut data = Vec::with_capacity(width * height);
    for row in rows {
        let n = row.len();
        data.extend(row);
        data.extend(std::iter::repeat_n(pad.clone(), width - n));
    }
    Res::A(Arc::new(Array::new(height as u32, width as u32, data)))
}

/// The value a cell shows for an element of a result array (empty becomes 0, as when spilled).
fn shown(v: &Value) -> Value {
    match v {
        Value::Empty => Value::Number(0.0),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_function_is_registered_once() {
        let names = names();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names.len(), dedup.len());
        assert!(names.len() > 400, "{} functions", names.len());
        for n in ["SUM", "XLOOKUP", "LAMBDA", "REGEXEXTRACT", "NORM.S.INV", "XIRR", "DSUM", "GROUPBY"] {
            assert!(is_known(n), "{n}");
        }
        assert!(is_known("sum"));
    }

    #[test]
    fn cleaning() {
        assert_eq!(clean15(0.1 + 0.2), 0.3);
        assert_eq!(clean15(1.4000000000000001), 1.4);
        assert_eq!(clean15(123456.0), 123456.0);
    }
}
