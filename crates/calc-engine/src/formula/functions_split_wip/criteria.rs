//! Criteria, the way COUNTIF reads them (`5`, `">5"`, `"<>x"`, `"a*"`, `"=?b"`, `">=2026-01-01"`), and the
//! conditional aggregates built on them. The aggregates stream over their ranges once per criterion, building a
//! match mask, and never copy a range; cells beyond the last row with ids are empty and handled in one step, so
//! `COUNTIFS(A:A,"x",B:B,">5")` over a million rows reads each cell once.

use std::cmp::Ordering;

use super::*;
use crate::{
    input::{Entry, parse_entry},
    value::{cmp_text_ci, parse_number_text},
};

pub(super) const FUNCTIONS: &[(&str, Func)] = &[
    ("COUNTIF", countif),
    ("COUNTIFS", countifs),
    ("SUMIF", sumif),
    ("SUMIFS", sumifs),
    ("AVERAGEIF", averageif),
    ("AVERAGEIFS", averageifs),
    ("MAXIFS", maxifs),
    ("MINIFS", minifs),
];

// ---- wildcards ---------------------------------------------------------------------------------------------

/// One piece of a wildcard pattern. Literal characters are kept case-folded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Wild {
    /// `*`: any run of characters.
    Any,
    /// `?`: any one character.
    One,
    Lit(char),
}

/// Excel compares text without case: characters are compared by their lower-case form.
pub(super) fn fold(c: char) -> char {
    if c.is_ascii() { c.to_ascii_lowercase() } else { c.to_lowercase().next().unwrap_or(c) }
}

/// The pattern of a text with `*`, `?` or `~` in it (`~*`, `~?`, `~~` are the characters themselves), or None
/// when the text has no wildcards.
pub(super) fn wildcard(s: &str) -> Option<Vec<Wild>> {
    if !s.contains(['*', '?', '~']) {
        return None;
    }
    Some(pattern(s))
}

/// The pattern of a text, wildcards or not.
pub(super) fn pattern(s: &str) -> Vec<Wild> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.next() {
                Some(d) => out.push(Wild::Lit(fold(d))),
                None => out.push(Wild::Lit('~')),
            },
            '*' => {
                if out.last() != Some(&Wild::Any) {
                    out.push(Wild::Any);
                }
            }
            '?' => out.push(Wild::One),
            c => out.push(Wild::Lit(fold(c))),
        }
    }
    out
}

/// Whether the whole of `s` matches the pattern, ignoring case.
pub(super) fn wild_match(p: &[Wild], s: &str) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    // The last `*` seen: the pattern position after it and where in the text it started absorbing.
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        let c = s[si..].chars().next().unwrap_or('\0');
        if pi < p.len() {
            match p[pi] {
                Wild::Any => {
                    star = Some((pi + 1, si));
                    pi += 1;
                    continue;
                }
                Wild::One => {
                    pi += 1;
                    si += c.len_utf8();
                    continue;
                }
                Wild::Lit(l) if l == fold(c) => {
                    pi += 1;
                    si += c.len_utf8();
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((after, from)) => {
                let skipped = s[from..].chars().next().map_or(1, char::len_utf8);
                star = Some((after, from + skipped));
                pi = after;
                si = from + skipped;
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|w| *w == Wild::Any)
}

/// Text equality ignoring case, without allocating.
pub(super) fn eq_ci(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars().map(fold).eq(b.chars().map(fold))
}

// ---- criteria ----------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn test(self, o: Ordering) -> bool {
        match self {
            Op::Eq => o == Ordering::Equal,
            Op::Ne => o != Ordering::Equal,
            Op::Lt => o == Ordering::Less,
            Op::Le => o != Ordering::Greater,
            Op::Gt => o == Ordering::Greater,
            Op::Ge => o != Ordering::Less,
        }
    }
}

#[derive(Clone, Debug)]
enum Target {
    Number(f64),
    Text { text: Arc<str>, pattern: Option<Vec<Wild>> },
    Bool(bool),
    Error(ErrorKind),
    /// Nothing after the operator: `""` (blank cells and empty text), `"="` (blank cells only), `"<>"`.
    Blank { strict: bool },
}

/// A COUNTIF-style criterion.
#[derive(Clone, Debug)]
pub(super) struct Criterion {
    op: Op,
    target: Target,
}

impl Criterion {
    /// The criterion a value stands for, as COUNTIF, SUMIFS and the others read it.
    pub(super) fn new(v: &Value) -> Self {
        Self::build(v, false)
    }

    /// A criterion from a criteria range of DSUM and the other database functions: plain text there matches
    /// text that begins with it, as Excel's advanced filter does.
    pub(super) fn database(v: &Value) -> Self {
        Self::build(v, true)
    }

    fn build(v: &Value, prefix: bool) -> Self {
        let s: &str = match v {
            Value::Number(n) => return Self { op: Op::Eq, target: Target::Number(*n) },
            Value::Bool(b) => return Self { op: Op::Eq, target: Target::Bool(*b) },
            Value::Error(e) => return Self { op: Op::Eq, target: Target::Error(*e) },
            // An empty criteria cell is read as 0 (Excel's rule for COUNTIFS).
            Value::Empty => return Self { op: Op::Eq, target: Target::Number(0.0) },
            Value::Text(s) => s,
        };
        let (op, rest, explicit) = [("<=", Op::Le), (">=", Op::Ge), ("<>", Op::Ne), ("<", Op::Lt), (">", Op::Gt), ("=", Op::Eq)]
            .iter()
            .find_map(|(sym, op)| s.strip_prefix(sym).map(|r| (*op, r, true)))
            .unwrap_or((Op::Eq, s, false));
        if rest.is_empty() {
            return Self { op, target: Target::Blank { strict: explicit } };
        }
        let target = match parse_entry(rest) {
            Entry::Number(n, _) => Target::Number(n),
            Entry::Bool(b) => Target::Bool(b),
            Entry::Error(e) => Target::Error(e),
            _ => {
                let pattern = match op {
                    Op::Eq | Op::Ne if prefix && !explicit => {
                        let mut p = pattern(rest);
                        if p.last() != Some(&Wild::Any) {
                            p.push(Wild::Any);
                        }
                        Some(p)
                    }
                    Op::Eq | Op::Ne => wildcard(rest),
                    _ => None,
                };
                Target::Text { text: Arc::from(rest), pattern }
            }
        };
        Self { op, target }
    }

    /// Whether a cell's value meets the criterion.
    pub(super) fn matches(&self, v: &Value) -> bool {
        let op = self.op;
        match &self.target {
            Target::Blank { strict } => {
                let blank = match v {
                    Value::Empty => true,
                    Value::Text(s) => s.is_empty() && !strict,
                    _ => false,
                };
                match op {
                    Op::Eq => blank,
                    Op::Ne => !blank,
                    _ => match v {
                        Value::Text(s) => op.test(cmp_text_ci(s, "")),
                        _ => false,
                    },
                }
            }
            Target::Number(n) => {
                let x = match v {
                    Value::Number(x) => Some(*x),
                    // Text that reads as a number equals that number (COUNTIF's rule for = and <>).
                    Value::Text(s) if matches!(op, Op::Eq | Op::Ne) => parse_number_text(s),
                    _ => None,
                };
                match x {
                    Some(x) => op.test(x.partial_cmp(n).unwrap_or(Ordering::Less)),
                    None => op == Op::Ne,
                }
            }
            Target::Text { text, pattern } => match v {
                Value::Text(s) => match pattern {
                    Some(p) => (op == Op::Eq) == wild_match(p, s),
                    None => match op {
                        Op::Eq => eq_ci(s, text),
                        Op::Ne => !eq_ci(s, text),
                        _ => op.test(cmp_text_ci(s, text)),
                    },
                },
                _ => op == Op::Ne,
            },
            Target::Bool(b) => match v {
                Value::Bool(x) => op.test(x.cmp(b)),
                _ => op == Op::Ne,
            },
            Target::Error(e) => match v {
                Value::Error(x) => match op {
                    Op::Eq => x == e,
                    Op::Ne => x != e,
                    _ => false,
                },
                _ => op == Op::Ne,
            },
        }
    }

    /// The value an equality criterion looks for, when it is a plain number or text (for grouping lookups).
    fn plain_key(&self) -> Option<Key> {
        match (&self.target, self.op) {
            (Target::Number(n), Op::Eq) => Some(Key::Num(norm_bits(*n))),
            (Target::Text { text, pattern: None }, Op::Eq) => Some(Key::Text(text.chars().map(fold).collect())),
            _ => None,
        }
    }
}

/// A number's bits with -0 read as 0, for hashing numbers by value.
fn norm_bits(n: f64) -> u64 {
    if n == 0.0 { 0 } else { n.to_bits() }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Num(u64),
    Text(String),
}

// ---- ranges read lazily ------------------------------------------------------------------------------------

/// A criteria or value range: an area read from the grid, or an array.
#[derive(Clone)]
pub(super) enum Range2 {
    Area(Area),
    Arr(Arc<Array>),
}

impl Range2 {
    pub(super) fn of(ctx: &mut Ctx<'_>, r: Res) -> Result<Self, ErrorKind> {
        match r {
            Res::R(a) => {
                // Note cells still to recalculate, so the formula runs again after them.
                ctx.live_rect(&a);
                Ok(Range2::Area(a))
            }
            Res::A(a) => Ok(Range2::Arr(a)),
            Res::V(Value::Error(e)) => Err(e),
            Res::V(v) => Ok(Range2::Arr(Arc::new(Array::new(1, 1, vec![v])))),
            Res::L(_) => Err(ErrorKind::Calc),
            Res::M(_) => Err(ErrorKind::Value),
        }
    }

    /// Rows and columns, whole columns counted in full.
    pub(super) fn dims(&self) -> (u32, u32) {
        match self {
            Range2::Area(a) => (a.rect.rows(), a.rect.cols()),
            Range2::Arr(a) => (a.rows, a.cols),
        }
    }

    /// The rows and columns from the top left that can hold values; everything beyond is empty.
    pub(super) fn live_dims(&self, ctx: &Ctx<'_>) -> (u32, u32) {
        match self {
            Range2::Area(a) => {
                let (er, ec) = ctx.grid.extent(a.tab).unwrap_or((0, 0));
                (er.saturating_sub(a.rect.r1).min(a.rect.rows()), ec.saturating_sub(a.rect.c1).min(a.rect.cols()))
            }
            Range2::Arr(a) => (a.rows, a.cols),
        }
    }

    /// Visits the values of rows `0..rows` and columns `0..cols` (relative), row by row.
    pub(super) fn visit(&self, ctx: &Ctx<'_>, rows: u32, cols: u32, f: &mut dyn FnMut(u32, u32, &Value)) {
        if rows == 0 || cols == 0 {
            return;
        }
        match self {
            Range2::Area(a) => {
                let (r1, c1) = (a.rect.r1, a.rect.c1);
                let rect = Rect::new(r1, c1, r1 + rows - 1, c1 + cols - 1);
                ctx.grid.visit(a.tab, &rect, &mut |r, c, v| {
                    f(r - r1, c - c1, v);
                    true
                });
            }
            Range2::Arr(a) => {
                for r in 0..rows.min(a.rows) {
                    for c in 0..cols.min(a.cols) {
                        f(r, c, &a.data[(r * a.cols + c) as usize]);
                    }
                }
            }
        }
    }

    /// Like `visit`, without the text of text cells.
    fn visit_kinds(&self, ctx: &Ctx<'_>, rows: u32, cols: u32, f: &mut dyn FnMut(u32, u32, Kind)) {
        if rows == 0 || cols == 0 {
            return;
        }
        match self {
            Range2::Area(a) => {
                let (r1, c1) = (a.rect.r1, a.rect.c1);
                let rect = Rect::new(r1, c1, r1 + rows - 1, c1 + cols - 1);
                ctx.grid.visit_kinds(a.tab, &rect, &mut |r, c, k| {
                    f(r - r1, c - c1, k);
                    true
                });
            }
            Range2::Arr(_) => self.visit(ctx, rows, cols, &mut |r, c, v| f(r, c, Kind::of(v))),
        }
    }

    /// The same area resized to `rows` × `cols` from its top left (SUMIF's rule for its sum range).
    fn resized(&self, rows: u32, cols: u32) -> Self {
        match self {
            Range2::Area(a) => {
                let r2 = (a.rect.r1 as u64 + rows.max(1) as u64 - 1).min(crate::addr::MAX_ROWS as u64 - 1) as u32;
                let c2 = (a.rect.c1 as u64 + cols.max(1) as u64 - 1).min(crate::addr::MAX_COLS as u64 - 1) as u32;
                Range2::Area(Area { tab: a.tab, rect: Rect::new(a.rect.r1, a.rect.c1, r2, c2) })
            }
            other => other.clone(),
        }
    }
}

use crate::addr::Rect;

// ---- match masks -------------------------------------------------------------------------------------------

/// Which cells of a criteria shape meet every criterion. Only the window holding values is kept cell by cell;
/// the rest of the shape is empty everywhere and either all matches or all doesn't.
struct Mask {
    bits: Vec<bool>,
    /// The window's columns (its rows are `bits.len() / cols`).
    cols: u32,
    rest_cells: u64,
    rest_match: bool,
}

impl Mask {
    fn count(&self) -> u64 {
        self.bits.iter().filter(|&&b| b).count() as u64 + if self.rest_match { self.rest_cells } else { 0 }
    }

    fn get(&self, r: u32, c: u32) -> bool {
        self.bits.get((r * self.cols + c) as usize).copied().unwrap_or(self.rest_match)
    }
}

/// The window (from the top left) outside which every range is empty.
fn window(ctx: &Ctx<'_>, ranges: &[&Range2], shape: (u32, u32)) -> (u32, u32) {
    let (mut wr, mut wc) = (0, 0);
    for r in ranges {
        let (lr, lc) = r.live_dims(ctx);
        wr = wr.max(lr);
        wc = wc.max(lc);
    }
    (wr.min(shape.0), wc.min(shape.1))
}

fn mask(ctx: &Ctx<'_>, conds: &[(&Range2, &Criterion)], shape: (u32, u32), win: (u32, u32)) -> Mask {
    let (wr, wc) = win;
    let mut bits = vec![true; wr as usize * wc as usize];
    for (rng, crit) in conds {
        let (lr, lc) = rng.live_dims(ctx);
        let (lr, lc) = (lr.min(wr), lc.min(wc));
        if !crit.matches(&Value::Empty) {
            // Cells of the window beyond this range's values are empty, and fail.
            for r in 0..wr {
                let from = if r < lr { lc } else { 0 };
                for c in from..wc {
                    bits[(r * wc + c) as usize] = false;
                }
            }
        }
        rng.visit(ctx, lr, lc, &mut |r, c, v| {
            let i = (r * wc + c) as usize;
            if bits[i] && !crit.matches(v) {
                bits[i] = false;
            }
        });
    }
    let total = shape.0 as u64 * shape.1 as u64;
    Mask {
        bits,
        cols: wc,
        rest_cells: total - wr as u64 * wc as u64,
        rest_match: conds.iter().all(|(_, c)| c.matches(&Value::Empty)),
    }
}

// ---- the functions -----------------------------------------------------------------------------------------

/// What a conditional aggregate computes over the matching cells of its value range.
#[derive(Clone, Copy, PartialEq)]
enum Agg2 {
    Count,
    Sum,
    Average,
    Max,
    Min,
}

/// Criteria ranges with their (evaluated) criteria, all of one shape.
struct Conditions {
    ranges: Vec<Range2>,
    criteria: Vec<Res>,
    shape: (u32, u32),
}

fn conditions(ctx: &mut Ctx<'_>, pairs: &[Expr], strict_shape: bool) -> Result<Conditions, ErrorKind> {
    if pairs.is_empty() || pairs.len() % 2 != 0 {
        return Err(ErrorKind::Value);
    }
    let mut ranges = Vec::new();
    let mut criteria = Vec::new();
    let mut shape = None;
    for pair in pairs.chunks(2) {
        let r = ctx.eval(&pair[0]);
        let rng = Range2::of(ctx, r)?;
        let d = rng.dims();
        match shape {
            None => shape = Some(d),
            Some(s) if s != d && strict_shape => return Err(ErrorKind::Value),
            _ => {}
        }
        ranges.push(rng);
        let c = match &pair[1] {
            Expr::Missing => Res::V(Value::Empty),
            e => ctx.eval(e),
        };
        criteria.push(c);
    }
    Ok(Conditions { ranges, criteria, shape: shape.unwrap_or((1, 1)) })
}

/// Evaluates a conditional aggregate, element by element when any criterion is an array.
fn conditional(ctx: &mut Ctx<'_>, conds: Conditions, target: Option<Range2>, agg: Agg2) -> Res {
    let single = conds.criteria.iter().all(is_single);
    if single {
        let crits: Vec<Criterion> = conds.criteria.into_iter().map(|c| Criterion::new(&ctx.to_scalar(c))).collect();
        return Res::V(aggregate(ctx, &conds.ranges, &crits, conds.shape, target.as_ref(), agg));
    }
    let arrays: Vec<Arc<Array>> = conds.criteria.iter().map(|c| ctx.to_array(c)).collect();
    let rows = arrays.iter().fold(1, |acc, a| broadcast_dim(acc, a.rows));
    let cols = arrays.iter().fold(1, |acc, a| broadcast_dim(acc, a.cols));
    // Many equality criteria over one range: count (or sum) every value once and look each criterion up.
    if conds.ranges.len() == 1 && rows as u64 * cols as u64 > 8 && matches!(agg, Agg2::Count | Agg2::Sum) {
        let crits: Vec<Criterion> = arrays[0].data.iter().map(Criterion::new).collect();
        if crits.iter().all(|c| c.plain_key().is_some()) {
            return grouped(ctx, &conds.ranges[0], conds.shape, target.as_ref(), agg, &arrays[0]);
        }
    }
    let mut data = Vec::with_capacity(rows as usize * cols as usize);
    for r in 0..rows {
        for c in 0..cols {
            let covered = arrays.iter().all(|a| (a.rows == 1 || r < a.rows) && (a.cols == 1 || c < a.cols));
            if !covered {
                data.push(Value::Error(ErrorKind::NA));
                continue;
            }
            let crits: Vec<Criterion> = arrays.iter().map(|a| Criterion::new(a.get(r, c))).collect();
            data.push(aggregate(ctx, &conds.ranges, &crits, conds.shape, target.as_ref(), agg));
        }
    }
    Res::A(Arc::new(Array::new(rows, cols, data)))
}

fn aggregate(ctx: &Ctx<'_>, ranges: &[Range2], crits: &[Criterion], shape: (u32, u32), target: Option<&Range2>, agg: Agg2) -> Value {
    let mut all: Vec<&Range2> = ranges.iter().collect();
    if let Some(t) = target {
        all.push(t);
    }
    let win = window(ctx, &all, shape);
    let conds: Vec<(&Range2, &Criterion)> = ranges.iter().zip(crits).collect();
    let m = mask(ctx, &conds, shape, win);
    if agg == Agg2::Count {
        return Value::Number(m.count() as f64);
    }
    let Some(t) = target else { return Value::Error(ErrorKind::Value) };
    let (lr, lc) = t.live_dims(ctx);
    let (lr, lc) = (lr.min(win.0), lc.min(win.1));
    let mut sum = Sum::default();
    let mut n = 0u64;
    let mut best: Option<f64> = None;
    let mut err = None;
    t.visit_kinds(ctx, lr, lc, &mut |r, c, k| {
        if err.is_some() || !m.get(r, c) {
            return;
        }
        match k {
            Kind::Number(x) => {
                sum.add(x);
                n += 1;
                best = Some(match (best, agg) {
                    (None, _) => x,
                    (Some(b), Agg2::Min) => b.min(x),
                    (Some(b), _) => b.max(x),
                });
            }
            Kind::Error(e) => err = Some(e),
            _ => {}
        }
    });
    if let Some(e) = err {
        return Value::Error(e);
    }
    match agg {
        Agg2::Sum => num_result(sum.total()),
        Agg2::Average if n == 0 => Value::Error(ErrorKind::Div0),
        Agg2::Average => num_result(sum.total() / n as f64),
        Agg2::Max | Agg2::Min => Value::Number(best.unwrap_or(0.0)),
        Agg2::Count => unreachable!("handled above"),
    }
}

/// COUNTIF / SUMIF with an array of plain equality criteria: one pass over the range, then a lookup each.
fn grouped(ctx: &Ctx<'_>, range: &Range2, shape: (u32, u32), target: Option<&Range2>, agg: Agg2, criteria: &Array) -> Res {
    let mut totals: ahash::HashMap<Key, (u64, Sum, Option<ErrorKind>)> = ahash::HashMap::default();
    let target_values: Option<Arc<Array>> = target.map(|t| {
        let (lr, lc) = t.live_dims(ctx);
        let (lr, lc) = (lr.min(shape.0), lc.min(shape.1));
        let mut data = vec![Value::Empty; lr as usize * lc as usize];
        t.visit(ctx, lr, lc, &mut |r, c, v| {
            if matches!(v, Value::Number(_) | Value::Error(_)) {
                data[(r * lc + c) as usize] = v.clone();
            }
        });
        Arc::new(Array::new(lr, lc, data))
    });
    let (lr, lc) = range.live_dims(ctx);
    range.visit(ctx, lr.min(shape.0), lc.min(shape.1), &mut |r, c, v| {
        let mut keys: Vec<Key> = Vec::with_capacity(2);
        match v {
            Value::Number(x) => keys.push(Key::Num(norm_bits(*x))),
            Value::Text(s) => {
                if let Some(x) = parse_number_text(s) {
                    keys.push(Key::Num(norm_bits(x)));
                }
                keys.push(Key::Text(s.chars().map(fold).collect()));
            }
            _ => {}
        }
        if keys.is_empty() {
            return;
        }
        let add = target_values.as_ref().and_then(|t| (r < t.rows && c < t.cols).then(|| t.data[(r * t.cols + c) as usize].clone()));
        for k in keys {
            let e = totals.entry(k).or_insert((0, Sum::default(), None));
            e.0 += 1;
            match &add {
                Some(Value::Number(x)) => e.1.add(*x),
                Some(Value::Error(err)) => {
                    e.2.get_or_insert(*err);
                }
                _ => {}
            }
        }
    });
    let data = criteria
        .data
        .iter()
        .map(|v| {
            let key = Criterion::new(v).plain_key().expect("checked by the caller");
            match totals.get(&key) {
                None => Value::Number(0.0),
                Some((n, s, err)) => match agg {
                    Agg2::Count => Value::Number(*n as f64),
                    _ => match err {
                        Some(e) => Value::Error(*e),
                        None => num_result(s.total()),
                    },
                },
            }
        })
        .collect();
    Res::A(Arc::new(Array::new(criteria.rows, criteria.cols, data)))
}

fn countif(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let conds = tri!(conditions(ctx, args, true));
    conditional(ctx, conds, None, Agg2::Count)
}

fn countifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let conds = tri!(conditions(ctx, args, true));
    conditional(ctx, conds, None, Agg2::Count)
}

/// SUMIF and AVERAGEIF: the value range takes the criteria range's size from its own top left.
fn one_condition(ctx: &mut Ctx<'_>, args: &[Expr], agg: Agg2) -> Res {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let conds = tri!(conditions(ctx, &args[..2], true));
    let target = match opt(args, 2) {
        Some(e) => {
            let r = ctx.eval(e);
            let t = tri!(Range2::of(ctx, r));
            let t = t.resized(conds.shape.0, conds.shape.1);
            if let Range2::Area(a) = &t {
                ctx.live_rect(a);
            }
            t
        }
        None => conds.ranges[0].clone(),
    };
    conditional(ctx, conds, Some(target), agg)
}

fn sumif(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    one_condition(ctx, args, Agg2::Sum)
}

fn averageif(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    one_condition(ctx, args, Agg2::Average)
}

/// SUMIFS, AVERAGEIFS, MAXIFS, MINIFS: the value range first, then pairs; every range has the same shape.
fn many_conditions(ctx: &mut Ctx<'_>, args: &[Expr], agg: Agg2) -> Res {
    if args.len() < 3 || args.len() % 2 == 0 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    let target = tri!(Range2::of(ctx, r));
    let conds = tri!(conditions(ctx, &args[1..], true));
    if target.dims() != conds.shape {
        return bad_args();
    }
    conditional(ctx, conds, Some(target), agg)
}

fn sumifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    many_conditions(ctx, args, Agg2::Sum)
}

fn averageifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    many_conditions(ctx, args, Agg2::Average)
}

fn maxifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    many_conditions(ctx, args, Agg2::Max)
}

fn minifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    many_conditions(ctx, args, Agg2::Min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Value {
        Value::from(s)
    }

    #[test]
    fn wildcards() {
        let p = pattern("a*c");
        assert!(wild_match(&p, "abc"));
        assert!(wild_match(&p, "AXYZC"));
        assert!(wild_match(&p, "ac"));
        assert!(!wild_match(&p, "abd"));
        assert!(wild_match(&pattern("?b*"), "ab"));
        assert!(!wild_match(&pattern("?b*"), "b"));
        assert!(wild_match(&pattern("~*x"), "*x"));
        assert!(!wild_match(&pattern("~*x"), "ax"));
        assert!(wild_match(&pattern("*"), ""));
        assert!(wild_match(&pattern("*ab*ab"), "xxabyyabab"));
        assert!(wild_match(&pattern("é*"), "Étoile"));
    }

    #[test]
    fn criteria() {
        assert!(Criterion::new(&t(">5")).matches(&Value::Number(6.0)));
        assert!(!Criterion::new(&t(">5")).matches(&t("6")));
        assert!(Criterion::new(&Value::Number(5.0)).matches(&t("5")));
        assert!(Criterion::new(&t("5")).matches(&Value::Number(5.0)));
        assert!(Criterion::new(&t("<>5")).matches(&Value::Empty));
        assert!(Criterion::new(&t("")).matches(&Value::Empty));
        assert!(Criterion::new(&t("")).matches(&t("")));
        assert!(Criterion::new(&t("=")).matches(&Value::Empty));
        assert!(!Criterion::new(&t("=")).matches(&t("")));
        assert!(Criterion::new(&t("<>")).matches(&t("")));
        assert!(!Criterion::new(&t("<>")).matches(&Value::Empty));
        assert!(Criterion::new(&t("app*")).matches(&t("Apples")));
        assert!(!Criterion::new(&t("app*")).matches(&Value::Number(1.0)));
        assert!(Criterion::new(&t("<>app*")).matches(&Value::Number(1.0)));
        assert!(Criterion::new(&t("#N/A")).matches(&Value::Error(ErrorKind::NA)));
        assert!(Criterion::new(&t("TRUE")).matches(&Value::Bool(true)));
        assert!(!Criterion::new(&t("TRUE")).matches(&Value::Number(1.0)));
        assert!(Criterion::new(&t(">=2026-01-01")).matches(&Value::Number(46100.0)));
        assert!(Criterion::new(&t("<b")).matches(&t("Apple")));
        assert!(!Criterion::new(&t("<b")).matches(&Value::Number(1.0)));
        assert!(Criterion::new(&Value::Empty).matches(&Value::Number(0.0)));
        assert!(!Criterion::new(&Value::Empty).matches(&Value::Empty));
        assert!(Criterion::database(&t("Ap")).matches(&t("Apple")));
        assert!(!Criterion::database(&t("=Ap")).matches(&t("Apple")));
        assert!(Criterion::database(&t("=Apple")).matches(&t("apple")));
    }
}
