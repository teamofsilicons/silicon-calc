//! Excel's functions. `call` dispatches by name; each function takes its argument trees unevaluated so the
//! ones that short-circuit (IF, IFERROR, AND, CHOOSE, LET…) only evaluate what they need.

use std::{cmp::Ordering, sync::Arc};

use super::{
    ast::Expr,
    eval::{Area, Array, Ctx, Res},
};
use crate::{
    addr::{MAX_COLS, MAX_ROWS, Rect},
    dates,
    numfmt,
    value::{ErrorKind, Value, cmp_text_ci, compare},
};

type R = Res;

/// Calls the function `name` (upper case) with these arguments.
pub fn call(ctx: &mut Ctx<'_>, name: &str, args: &[Expr]) -> Res {
    match lookup(name) {
        Some(f) => f(ctx, args),
        None => Res::err(ErrorKind::Name),
    }
}

/// Whether Calc knows a function (used to flag unknown names while typing).
pub fn is_known(name: &str) -> bool {
    lookup(&name.to_ascii_uppercase()).is_some()
}

/// Every function name Calc knows, sorted.
pub fn names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = TABLE.iter().map(|(n, _)| *n).collect();
    v.sort_unstable();
    v
}

type Func = fn(&mut Ctx<'_>, &[Expr]) -> Res;

fn lookup(name: &str) -> Option<Func> {
    TABLE.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

static TABLE: &[(&str, Func)] = &[
    // Math and aggregates
    ("SUM", sum),
    ("PRODUCT", product),
    ("AVERAGE", average),
    ("AVERAGEA", averagea),
    ("COUNT", count),
    ("COUNTA", counta),
    ("COUNTBLANK", countblank),
    ("MIN", min),
    ("MAX", max),
    ("MINA", mina),
    ("MAXA", maxa),
    ("SUMPRODUCT", sumproduct),
    ("SUMSQ", sumsq),
    ("ROUND", round),
    ("ROUNDUP", roundup),
    ("ROUNDDOWN", rounddown),
    ("INT", int),
    ("TRUNC", trunc),
    ("ABS", abs),
    ("SIGN", sign),
    ("MOD", modulo),
    ("POWER", power),
    ("SQRT", sqrt),
    ("EXP", exp),
    ("LN", ln),
    ("LOG", log),
    ("LOG10", log10),
    ("PI", pi),
    ("CEILING", ceiling),
    ("CEILING.MATH", ceiling_math),
    ("FLOOR", floor),
    ("FLOOR.MATH", floor_math),
    ("MROUND", mround),
    ("EVEN", even),
    ("ODD", odd),
    ("QUOTIENT", quotient),
    ("RAND", rand),
    ("RANDBETWEEN", randbetween),
    ("MEDIAN", median),
    ("MODE", mode_sngl),
    ("MODE.SNGL", mode_sngl),
    ("LARGE", large),
    ("SMALL", small),
    ("STDEV", stdev_s),
    ("STDEV.S", stdev_s),
    ("STDEV.P", stdev_p),
    ("STDEVP", stdev_p),
    ("VAR", var_s),
    ("VAR.S", var_s),
    ("VAR.P", var_p),
    ("VARP", var_p),
    ("RANK", rank),
    ("RANK.EQ", rank),
    // Conditional aggregates
    ("SUMIF", sumif),
    ("SUMIFS", sumifs),
    ("COUNTIF", countif),
    ("COUNTIFS", countifs),
    ("AVERAGEIF", averageif),
    ("AVERAGEIFS", averageifs),
    ("MAXIFS", maxifs),
    ("MINIFS", minifs),
    // Logic
    ("IF", if_),
    ("IFS", ifs),
    ("IFERROR", iferror),
    ("IFNA", ifna),
    ("AND", and),
    ("OR", or),
    ("XOR", xor),
    ("NOT", not),
    ("TRUE", true_),
    ("FALSE", false_),
    ("SWITCH", switch),
    ("CHOOSE", choose),
    ("LET", let_),
    // Information
    ("ISBLANK", isblank),
    ("ISNUMBER", isnumber),
    ("ISTEXT", istext),
    ("ISNONTEXT", isnontext),
    ("ISLOGICAL", islogical),
    ("ISERROR", iserror),
    ("ISERR", iserr),
    ("ISNA", isna),
    ("ISEVEN", iseven),
    ("ISODD", isodd),
    ("ISFORMULA", isformula),
    ("NA", na),
    ("ERROR.TYPE", error_type),
    ("N", n),
    ("T", t),
    ("TYPE", type_),
    ("FORMULATEXT", formulatext),
    // Text
    ("CONCAT", concat),
    ("CONCATENATE", concat),
    ("TEXTJOIN", textjoin),
    ("LEN", len),
    ("LEFT", left),
    ("RIGHT", right),
    ("MID", mid),
    ("UPPER", upper),
    ("LOWER", lower),
    ("PROPER", proper),
    ("TRIM", trim),
    ("CLEAN", clean),
    ("TEXT", text),
    ("VALUE", value),
    ("FIND", find),
    ("SEARCH", search),
    ("SUBSTITUTE", substitute),
    ("REPLACE", replace),
    ("REPT", rept),
    ("EXACT", exact),
    ("CHAR", char_),
    ("CODE", code),
    ("UNICHAR", char_),
    ("UNICODE", code),
    ("FIXED", fixed),
    ("DOLLAR", dollar),
    ("NUMBERVALUE", value),
    ("TEXTBEFORE", textbefore),
    ("TEXTAFTER", textafter),
    ("TEXTSPLIT", textsplit),
    // Lookup and reference
    ("VLOOKUP", vlookup),
    ("HLOOKUP", hlookup),
    ("XLOOKUP", xlookup),
    ("LOOKUP", lookup_fn),
    ("MATCH", match_),
    ("XMATCH", xmatch),
    ("INDEX", index),
    ("ROW", row),
    ("ROWS", rows),
    ("COLUMN", column),
    ("COLUMNS", columns),
    ("INDIRECT", indirect),
    ("OFFSET", offset),
    ("ADDRESS", address),
    ("SHEET", sheet),
    ("SHEETS", sheets),
    // Dates
    ("DATE", date),
    ("TIME", time),
    ("TODAY", today),
    ("NOW", now),
    ("YEAR", year),
    ("MONTH", month),
    ("DAY", day),
    ("HOUR", hour),
    ("MINUTE", minute),
    ("SECOND", second),
    ("WEEKDAY", weekday),
    ("EDATE", edate),
    ("EOMONTH", eomonth),
    ("DATEDIF", datedif),
    ("DAYS", days),
    ("DATEVALUE", datevalue),
    ("TIMEVALUE", timevalue),
    ("NETWORKDAYS", networkdays),
    ("WORKDAY", workday),
    ("WEEKNUM", weeknum),
    ("ISOWEEKNUM", isoweeknum),
    ("YEARFRAC", yearfrac),
    // Dynamic arrays
    ("SEQUENCE", sequence),
    ("FILTER", filter),
    ("SORT", sort),
    ("SORTBY", sortby),
    ("UNIQUE", unique),
    ("TRANSPOSE", transpose),
    ("RANDARRAY", randarray),
    ("VSTACK", vstack),
    ("HSTACK", hstack),
    ("TAKE", take),
    ("DROP", drop),
    ("CHOOSECOLS", choosecols),
    ("CHOOSEROWS", chooserows),
    ("TOCOL", tocol),
    ("TOROW", torow),
];

// ---- argument helpers --------------------------------------------------------------------------------------

fn bad_args() -> Res {
    Res::err(ErrorKind::Value)
}

fn arity(args: &[Expr], min: usize, max: usize) -> bool {
    args.len() >= min && args.len() <= max
}

/// An optional argument, `None` when absent or left empty.
fn opt<'a>(args: &'a [Expr], i: usize) -> Option<&'a Expr> {
    args.get(i).filter(|e| !matches!(e, Expr::Missing))
}

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

/// A value-to-value function applied to every element when given a range or array (dynamic arrays lift).
fn lift(ctx: &mut Ctx<'_>, e: &Expr, f: impl Fn(&Value) -> Value) -> Res {
    let r = ctx.eval(e);
    ctx.map1(r, f)
}

/// Two-argument lifting with broadcasting.
fn lift2(ctx: &mut Ctx<'_>, a: &Expr, b: &Expr, f: impl Fn(&Value, &Value) -> Value) -> Res {
    let ra = ctx.eval(a);
    let rb = ctx.eval(b);
    ctx.map2(ra, rb, f)
}

fn num_result(n: f64) -> Value {
    if n.is_finite() { Value::Number(n) } else { Value::Error(ErrorKind::Num) }
}

fn as_num(v: &Value) -> Result<f64, ErrorKind> {
    v.coerce_number()
}

/// How an aggregate reads its arguments.
#[derive(Clone, Copy, PartialEq)]
enum Agg {
    /// SUM, AVERAGE, MIN…: numbers from ranges; numbers, numeric text and logicals typed directly.
    Numbers,
    /// AVERAGEA, MAXA: ranges also count text as 0 and logicals as 0/1.
    All,
}

/// Every number the arguments give, Excel style. An error anywhere is the answer.
fn numbers(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for a in args {
        if matches!(a, Expr::Missing) {
            continue;
        }
        let r = ctx.eval(a);
        match r {
            Res::V(v) => match v {
                Value::Empty => {}
                Value::Error(e) => return Err(e),
                other => out.push(other.coerce_number()?),
            },
            other => {
                let mut err = None;
                ctx.each(&other, |v| match v {
                    Value::Number(n) => out.push(*n),
                    Value::Error(e) if err.is_none() => err = Some(*e),
                    Value::Bool(b) if mode == Agg::All => out.push(if *b { 1.0 } else { 0.0 }),
                    Value::Text(_) if mode == Agg::All => out.push(0.0),
                    _ => {}
                });
                if let Some(e) = err {
                    return Err(e);
                }
            }
        }
    }
    Ok(out)
}

fn agg(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg, f: impl FnOnce(Vec<f64>) -> Value) -> Res {
    match numbers(ctx, args, mode) {
        Ok(v) => Res::V(f(v)),
        Err(e) => Res::err(e),
    }
}

// ---- math --------------------------------------------------------------------------------------------------

fn sum(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| num_result(kahan(&v)))
}

/// Compensated summation, so long columns of decimals add up the way people expect.
fn kahan(v: &[f64]) -> f64 {
    let mut s = 0.0;
    let mut c = 0.0;
    for &x in v {
        let t = s + x;
        if s.abs() >= x.abs() {
            c += (s - t) + x;
        } else {
            c += (x - t) + s;
        }
        s = t;
    }
    s + c
}

fn product(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| if v.is_empty() { Value::Number(0.0) } else { num_result(v.iter().product()) })
}

fn average(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| {
        if v.is_empty() { Value::Error(ErrorKind::Div0) } else { num_result(kahan(&v) / v.len() as f64) }
    })
}

fn averagea(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::All, |v| {
        if v.is_empty() { Value::Error(ErrorKind::Div0) } else { num_result(kahan(&v) / v.len() as f64) }
    })
}

fn count(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let mut n = 0u64;
    for a in args {
        let r = ctx.eval(a);
        match r {
            Res::V(v) => {
                if v.coerce_number().is_ok() && !matches!(v, Value::Empty) {
                    n += 1;
                }
            }
            other => {
                ctx.each(&other, |v| {
                    if matches!(v, Value::Number(_)) {
                        n += 1;
                    }
                });
            }
        }
    }
    Res::num(n as f64)
}

fn counta(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let mut n = 0u64;
    for a in args {
        if matches!(a, Expr::Missing) {
            continue;
        }
        let r = ctx.eval(a);
        match r {
            Res::V(v) => {
                if !v.is_empty() {
                    n += 1;
                }
            }
            other => {
                ctx.each(&other, |v| {
                    if !v.is_empty() {
                        n += 1;
                    }
                });
            }
        }
    }
    Res::num(n as f64)
}

fn countblank(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    let mut n = 0u64;
    let unvisited = ctx.each(&r, |v| {
        if v.is_empty() || matches!(v, Value::Text(s) if s.is_empty()) {
            n += 1;
        }
    });
    Res::num((n + unvisited) as f64)
}

fn min(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| Value::Number(v.into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x)))).unwrap_or(0.0)))
}

fn max(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| Value::Number(v.into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))).unwrap_or(0.0)))
}

fn mina(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::All, |v| Value::Number(v.into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x)))).unwrap_or(0.0)))
}

fn maxa(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::All, |v| Value::Number(v.into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))).unwrap_or(0.0)))
}

fn sumsq(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| num_result(v.iter().map(|x| x * x).sum()))
}

fn sumproduct(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.is_empty() {
        return bad_args();
    }
    let mut arrays = Vec::new();
    for a in args {
        let r = ctx.eval(a);
        if let Some(e) = r.as_error() {
            return Res::err(e);
        }
        arrays.push(ctx.to_array(&r));
    }
    let (rows, cols) = (arrays[0].rows, arrays[0].cols);
    if arrays.iter().any(|a| a.rows != rows || a.cols != cols) {
        return bad_args();
    }
    let mut total = 0.0;
    for i in 0..(rows * cols) as usize {
        let mut p = 1.0;
        for a in &arrays {
            match &a.data[i] {
                Value::Number(n) => p *= n,
                Value::Bool(b) => p *= if *b { 1.0 } else { 0.0 },
                Value::Error(e) => return Res::err(*e),
                _ => p *= 0.0,
            }
        }
        total += p;
    }
    Res::num(total)
}

fn round_with(n: f64, digits: f64, mode: i8) -> Value {
    let d = digits.trunc() as i32;
    let factor = 10f64.powi(d);
    let scaled = n * factor;
    // Nudge by a relative epsilon so 2.675 rounds to 2.68 as people expect (Excel does the same).
    let eps = scaled.abs() * 1e-15;
    let r = match mode {
        0 => (scaled.abs() + 0.5 + eps).floor() * scaled.signum(),
        1 => (scaled.abs() - eps).ceil() * scaled.signum(),
        _ => (scaled.abs() + eps).floor() * scaled.signum(),
    };
    num_result(r / factor)
}

fn round(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let digits = args.get(1).cloned().unwrap_or(Expr::Number(0.0));
    lift2(ctx, &args[0], &digits, |a, b| match (as_num(a), as_num(b)) {
        (Ok(x), Ok(d)) => round_with(x, d, 0),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn roundup(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let digits = args.get(1).cloned().unwrap_or(Expr::Number(0.0));
    lift2(ctx, &args[0], &digits, |a, b| match (as_num(a), as_num(b)) {
        (Ok(x), Ok(d)) => round_with(x, d, 1),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn rounddown(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let digits = args.get(1).cloned().unwrap_or(Expr::Number(0.0));
    lift2(ctx, &args[0], &digits, |a, b| match (as_num(a), as_num(b)) {
        (Ok(x), Ok(d)) => round_with(x, d, 2),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn trunc(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    rounddown(ctx, args)
}

fn unary_math(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(f64) -> Result<f64, ErrorKind>) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match as_num(v).and_then(f) {
        Ok(n) => num_result(n),
        Err(e) => Value::Error(e),
    })
}

fn int(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| Ok(x.floor()))
}

fn abs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| Ok(x.abs()))
}

fn sign(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| Ok(if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }))
}

fn sqrt(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| if x < 0.0 { Err(ErrorKind::Num) } else { Ok(x.sqrt()) })
}

fn exp(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| Ok(x.exp()))
}

fn ln(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| if x <= 0.0 { Err(ErrorKind::Num) } else { Ok(x.ln()) })
}

fn log10(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| if x <= 0.0 { Err(ErrorKind::Num) } else { Ok(x.log10()) })
}

fn even(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| {
        let r = (x.abs() / 2.0).ceil() * 2.0;
        Ok(r * if x < 0.0 { -1.0 } else { 1.0 })
    })
}

fn odd(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    unary_math(ctx, args, |x| {
        let a = x.abs();
        let mut r = a.ceil();
        if r % 2.0 == 0.0 {
            r += 1.0;
        }
        if a == 0.0 {
            r = 1.0;
        }
        Ok(r * if x < 0.0 { -1.0 } else { 1.0 })
    })
}

fn log(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let base = match opt(args, 1) {
        Some(e) => num!(ctx, e),
        None => 10.0,
    };
    if x <= 0.0 || base <= 0.0 || base == 1.0 {
        return Res::err(ErrorKind::Num);
    }
    Res::num(x.ln() / base.ln())
}

fn modulo(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    lift2(ctx, &args[0], &args[1], |a, b| match (as_num(a), as_num(b)) {
        (Ok(_), Ok(d)) if d == 0.0 => Value::Error(ErrorKind::Div0),
        (Ok(n), Ok(d)) => num_result(n - d * (n / d).floor()),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn quotient(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let (n, d) = (num!(ctx, &args[0]), num!(ctx, &args[1]));
    if d == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    Res::num((n / d).trunc())
}

fn power(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    lift2(ctx, &args[0], &args[1], |a, b| super::eval::binary(super::ast::BinaryOp::Pow, a, b))
}

fn pi(_: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !args.is_empty() {
        return bad_args();
    }
    Res::num(std::f64::consts::PI)
}

fn ceiling(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let sig = match opt(args, 1) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    if sig == 0.0 {
        return Res::num(0.0);
    }
    if x > 0.0 && sig < 0.0 {
        return Res::err(ErrorKind::Num);
    }
    Res::num((x / sig).ceil() * sig)
}

fn ceiling_math(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 3) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let sig = match opt(args, 1) {
        Some(e) => num!(ctx, e).abs(),
        None => 1.0,
    };
    let mode = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 0.0,
    };
    if sig == 0.0 {
        return Res::num(0.0);
    }
    let r = if x < 0.0 && mode != 0.0 { (x / sig).floor() * sig } else { (x / sig).ceil() * sig };
    Res::num(r)
}

fn floor(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let sig = match opt(args, 1) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    if sig == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    if x > 0.0 && sig < 0.0 {
        return Res::err(ErrorKind::Num);
    }
    Res::num((x / sig).floor() * sig)
}

fn floor_math(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 3) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let sig = match opt(args, 1) {
        Some(e) => num!(ctx, e).abs(),
        None => 1.0,
    };
    let mode = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 0.0,
    };
    if sig == 0.0 {
        return Res::num(0.0);
    }
    let r = if x < 0.0 && mode != 0.0 { (x / sig).ceil() * sig } else { (x / sig).floor() * sig };
    Res::num(r)
}

fn mround(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let (x, m) = (num!(ctx, &args[0]), num!(ctx, &args[1]));
    if m == 0.0 {
        return Res::num(0.0);
    }
    if x * m < 0.0 {
        return Res::err(ErrorKind::Num);
    }
    // Half away from zero, to the nearest multiple.
    let q = x / m;
    Res::num((q.abs() + 0.5 + q.abs() * 1e-15).floor() * q.signum() * m)
}

fn rand(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !args.is_empty() {
        return bad_args();
    }
    Res::num(ctx.grid.random())
}

fn randbetween(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let (lo, hi) = (num!(ctx, &args[0]).ceil(), num!(ctx, &args[1]).floor());
    if lo > hi {
        return Res::err(ErrorKind::Num);
    }
    Res::num((lo + (ctx.grid.random() * (hi - lo + 1.0)).floor()).min(hi))
}

fn sorted_numbers(ctx: &mut Ctx<'_>, args: &[Expr]) -> Result<Vec<f64>, ErrorKind> {
    let mut v = numbers(ctx, args, Agg::Numbers)?;
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Ok(v)
}

fn median(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match sorted_numbers(ctx, args) {
        Err(e) => Res::err(e),
        Ok(v) if v.is_empty() => Res::err(ErrorKind::Num),
        Ok(v) => {
            let n = v.len();
            Res::num(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
        }
    }
}

fn mode_sngl(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let v = match numbers(ctx, args, Agg::Numbers) {
        Ok(v) => v,
        Err(e) => return Res::err(e),
    };
    // The most frequent value; ties go to the one seen first.
    let mut best: Option<(f64, usize)> = None;
    for (i, x) in v.iter().enumerate() {
        let n = v.iter().filter(|y| *y == x).count();
        if n > 1 && best.is_none_or(|(_, bn)| n > bn) && !v[..i].contains(x) {
            best = Some((*x, n));
        }
    }
    match best {
        Some((x, _)) => Res::num(x),
        None => Res::err(ErrorKind::NA),
    }
}

fn kth(ctx: &mut Ctx<'_>, args: &[Expr], largest: bool) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let v = match sorted_numbers(ctx, &args[..1]) {
        Ok(v) => v,
        Err(e) => return Res::err(e),
    };
    let k = num!(ctx, &args[1]).ceil();
    if k < 1.0 || k as usize > v.len() {
        return Res::err(ErrorKind::Num);
    }
    let k = k as usize;
    Res::num(if largest { v[v.len() - k] } else { v[k - 1] })
}

fn large(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    kth(ctx, args, true)
}

fn small(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    kth(ctx, args, false)
}

fn variance(v: &[f64], sample: bool) -> Option<f64> {
    let n = v.len();
    if n == 0 || (sample && n < 2) {
        return None;
    }
    let mean = kahan(v) / n as f64;
    let ss: f64 = v.iter().map(|x| (x - mean) * (x - mean)).sum();
    Some(ss / if sample { (n - 1) as f64 } else { n as f64 })
}

fn var_s(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| variance(&v, true).map_or(Value::Error(ErrorKind::Div0), Value::Number))
}

fn var_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| variance(&v, false).map_or(Value::Error(ErrorKind::Div0), Value::Number))
}

fn stdev_s(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| variance(&v, true).map_or(Value::Error(ErrorKind::Div0), |x| Value::Number(x.sqrt())))
}

fn stdev_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    agg(ctx, args, Agg::Numbers, |v| variance(&v, false).map_or(Value::Error(ErrorKind::Div0), |x| Value::Number(x.sqrt())))
}

fn rank(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let v = match numbers(ctx, &args[1..2], Agg::Numbers) {
        Ok(v) => v,
        Err(e) => return Res::err(e),
    };
    let asc = match opt(args, 2) {
        Some(e) => num!(ctx, e) != 0.0,
        None => false,
    };
    if !v.contains(&x) {
        return Res::err(ErrorKind::NA);
    }
    let better = v.iter().filter(|&&y| if asc { y < x } else { y > x }).count();
    Res::num((better + 1) as f64)
}

// ---- criteria (COUNTIF and friends) ------------------------------------------------------------------------

/// A COUNTIF-style criterion: `5`, `">5"`, `"<>x"`, `"a*"`, `"=?b"`.
pub(crate) struct Criterion {
    op: &'static str,
    value: Value,
    pattern: Option<Vec<WildTok>>,
}

#[derive(Clone, Debug, PartialEq)]
enum WildTok {
    Any,
    One,
    Lit(char),
}

fn wildcard(s: &str) -> Option<Vec<WildTok>> {
    if !s.contains(['*', '?', '~']) {
        return None;
    }
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.next() {
                Some(d) => out.push(WildTok::Lit(d)),
                None => out.push(WildTok::Lit('~')),
            },
            '*' => out.push(WildTok::Any),
            '?' => out.push(WildTok::One),
            c => out.push(WildTok::Lit(c)),
        }
    }
    Some(out)
}

fn wild_match(p: &[WildTok], s: &[char]) -> bool {
    // Iterative matching with backtracking on the last *.
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() {
            match &p[pi] {
                WildTok::Any => {
                    star = Some((pi, si));
                    pi += 1;
                    continue;
                }
                WildTok::One => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                WildTok::Lit(c) if c.to_lowercase().eq(s[si].to_lowercase()) => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, ss)) => {
                pi = sp + 1;
                si = ss + 1;
                star = Some((sp, ss + 1));
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == WildTok::Any {
        pi += 1;
    }
    pi == p.len()
}

impl Criterion {
    pub(crate) fn new(v: &Value) -> Self {
        match v {
            Value::Text(s) => {
                let s: &str = s;
                let (op, rest) = ["<=", ">=", "<>", "<", ">", "="]
                    .iter()
                    .find_map(|op| s.strip_prefix(op).map(|r| (*op, r)))
                    .unwrap_or(("=", s));
                let value = match crate::input::parse_entry(rest) {
                    crate::input::Entry::Number(n, _) => Value::Number(n),
                    crate::input::Entry::Bool(b) => Value::Bool(b),
                    crate::input::Entry::Error(e) => Value::Error(e),
                    _ => Value::from(rest),
                };
                let pattern = match (&value, op) {
                    (Value::Text(t), "=" | "<>") => wildcard(t),
                    _ => None,
                };
                Self { op, value, pattern }
            }
            Value::Empty => Self { op: "=", value: Value::from(""), pattern: None },
            other => Self { op: "=", value: other.clone(), pattern: None },
        }
    }

    pub(crate) fn matches(&self, v: &Value) -> bool {
        if let Some(p) = &self.pattern {
            let s: Vec<char> = match v {
                Value::Text(t) => t.chars().collect(),
                Value::Empty => Vec::new(),
                Value::Number(_) | Value::Bool(_) => return self.op == "<>",
                Value::Error(_) => return self.op == "<>",
            };
            let m = wild_match(p, &s);
            return if self.op == "=" { m } else { !m };
        }
        match (&self.value, v) {
            // "=" with nothing after it matches blanks; "<>" with nothing matches non-blanks.
            (Value::Text(t), _) if t.is_empty() => {
                let blank = v.is_empty() || matches!(v, Value::Text(s) if s.is_empty());
                return if self.op == "<>" { !blank } else if self.op == "=" { blank } else { false };
            }
            (Value::Number(_), Value::Text(_)) | (Value::Number(_), Value::Empty) | (Value::Number(_), Value::Bool(_)) => {
                return self.op == "<>";
            }
            (Value::Text(_), Value::Number(_)) | (Value::Text(_), Value::Bool(_)) | (Value::Text(_), Value::Empty) => {
                return self.op == "<>";
            }
            (Value::Bool(_), Value::Number(_)) | (Value::Bool(_), Value::Text(_)) | (Value::Bool(_), Value::Empty) => {
                return self.op == "<>";
            }
            (_, Value::Error(e)) => {
                return match &self.value {
                    Value::Error(x) => (x == e) == (self.op == "="),
                    _ => false,
                };
            }
            _ => {}
        }
        let ord = compare(v, &self.value);
        match self.op {
            "=" => ord == Ordering::Equal,
            "<>" => ord != Ordering::Equal,
            "<" => ord == Ordering::Less,
            "<=" => ord != Ordering::Greater,
            ">" => ord == Ordering::Greater,
            _ => ord != Ordering::Less,
        }
    }
}

/// Pairs of (range, criterion) for the *IFS functions, and an optional range to aggregate.
fn ifs_matches(ctx: &mut Ctx<'_>, pairs: &[Expr]) -> Result<(Vec<bool>, u32, u32), ErrorKind> {
    if pairs.len() < 2 || pairs.len() % 2 != 0 {
        return Err(ErrorKind::Value);
    }
    let mut mask: Option<Vec<bool>> = None;
    let (mut rows, mut cols) = (0, 0);
    for pair in pairs.chunks(2) {
        let r = ctx.eval(&pair[0]);
        if let Some(e) = r.as_error() {
            return Err(e);
        }
        let arr = criteria_range(ctx, &r);
        let crit = ctx.scalar(&pair[1]);
        let c = Criterion::new(&crit);
        let m: Vec<bool> = arr.data.iter().map(|v| c.matches(v)).collect();
        match &mut mask {
            None => {
                rows = arr.rows;
                cols = arr.cols;
                mask = Some(m);
            }
            Some(existing) => {
                if arr.rows != rows || arr.cols != cols {
                    return Err(ErrorKind::Value);
                }
                for (a, b) in existing.iter_mut().zip(m) {
                    *a &= b;
                }
            }
        }
    }
    Ok((mask.unwrap_or_default(), rows, cols))
}

/// A criteria range as an array, keeping whole-column ranges to the rows that have ids.
fn criteria_range(ctx: &mut Ctx<'_>, r: &Res) -> Arc<Array> {
    ctx.to_array(r)
}

/// The values of `r` at the positions where `mask` is true, reading `r` with the same shape as the mask.
fn masked_values(ctx: &mut Ctx<'_>, r: &Res, mask: &[bool], rows: u32, cols: u32) -> Vec<Value> {
    let arr = match r {
        Res::R(a) => {
            // The sum range takes its size from the criteria range (Excel's rule), starting at its top left.
            let rect = Rect::new(a.rect.r1, a.rect.c1, a.rect.r1 + rows.max(1) - 1, a.rect.c1 + cols.max(1) - 1);
            ctx.to_array(&Res::R(Area { tab: a.tab, rect }))
        }
        other => ctx.to_array(other),
    };
    let mut out = Vec::new();
    for (i, &m) in mask.iter().enumerate() {
        if m {
            let (r, c) = (i as u32 / cols.max(1), i as u32 % cols.max(1));
            if r < arr.rows && c < arr.cols {
                out.push(arr.get(r, c).clone());
            }
        }
    }
    out
}

fn sumif(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let (mask, rows, cols) = match ifs_matches(ctx, &args[..2]) {
        Ok(m) => m,
        Err(e) => return Res::err(e),
    };
    let target = match opt(args, 2) {
        Some(e) => ctx.eval(e),
        None => ctx.eval(&args[0]),
    };
    let vals = masked_values(ctx, &target, &mask, rows, cols);
    sum_values(&vals)
}

fn sum_values(vals: &[Value]) -> Res {
    let mut nums = Vec::new();
    for v in vals {
        match v {
            Value::Number(n) => nums.push(*n),
            Value::Error(e) => return Res::err(*e),
            _ => {}
        }
    }
    Res::num(kahan(&nums))
}

fn sumifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 3 {
        return bad_args();
    }
    let (mask, rows, cols) = match ifs_matches(ctx, &args[1..]) {
        Ok(m) => m,
        Err(e) => return Res::err(e),
    };
    let target = ctx.eval(&args[0]);
    let vals = masked_values(ctx, &target, &mask, rows, cols);
    sum_values(&vals)
}

fn countif(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    countifs(ctx, args)
}

fn countifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match ifs_matches(ctx, args) {
        Ok((mask, _, _)) => Res::num(mask.iter().filter(|&&m| m).count() as f64),
        Err(e) => Res::err(e),
    }
}

fn average_values(vals: &[Value]) -> Res {
    let mut nums = Vec::new();
    for v in vals {
        match v {
            Value::Number(n) => nums.push(*n),
            Value::Error(e) => return Res::err(*e),
            _ => {}
        }
    }
    if nums.is_empty() { Res::err(ErrorKind::Div0) } else { Res::num(kahan(&nums) / nums.len() as f64) }
}

fn averageif(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let (mask, rows, cols) = match ifs_matches(ctx, &args[..2]) {
        Ok(m) => m,
        Err(e) => return Res::err(e),
    };
    let target = match opt(args, 2) {
        Some(e) => ctx.eval(e),
        None => ctx.eval(&args[0]),
    };
    let vals = masked_values(ctx, &target, &mask, rows, cols);
    average_values(&vals)
}

fn averageifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 3 {
        return bad_args();
    }
    let (mask, rows, cols) = match ifs_matches(ctx, &args[1..]) {
        Ok(m) => m,
        Err(e) => return Res::err(e),
    };
    let target = ctx.eval(&args[0]);
    let vals = masked_values(ctx, &target, &mask, rows, cols);
    average_values(&vals)
}

fn extreme_ifs(ctx: &mut Ctx<'_>, args: &[Expr], want_max: bool) -> R {
    if args.len() < 3 {
        return bad_args();
    }
    let (mask, rows, cols) = match ifs_matches(ctx, &args[1..]) {
        Ok(m) => m,
        Err(e) => return Res::err(e),
    };
    let target = ctx.eval(&args[0]);
    let vals = masked_values(ctx, &target, &mask, rows, cols);
    let mut best: Option<f64> = None;
    for v in vals {
        match v {
            Value::Number(n) => best = Some(best.map_or(n, |b| if want_max { b.max(n) } else { b.min(n) })),
            Value::Error(e) => return Res::err(e),
            _ => {}
        }
    }
    Res::num(best.unwrap_or(0.0))
}

fn maxifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    extreme_ifs(ctx, args, true)
}

fn minifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    extreme_ifs(ctx, args, false)
}

// ---- logic -------------------------------------------------------------------------------------------------

fn if_(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let cond = ctx.eval(&args[0]);
    // An array condition gives an array of answers.
    if !matches!(&cond, Res::V(_)) && cond.dims() != (1, 1) {
        let c = ctx.to_array(&cond);
        let t = ctx.eval(&args[1]);
        let f = match opt(args, 2) {
            Some(e) => ctx.eval(e),
            None => Res::V(Value::Bool(false)),
        };
        let ta = ctx.to_array(&t);
        let fa = ctx.to_array(&f);
        let data = (0..c.rows)
            .flat_map(|r| (0..c.cols).map(move |cc| (r, cc)))
            .map(|(r, cc)| match c.get(r, cc).coerce_bool() {
                Ok(true) => ta.get(r, cc).clone(),
                Ok(false) => fa.get(r, cc).clone(),
                Err(e) => Value::Error(e),
            })
            .collect();
        return Res::A(Arc::new(Array::new(c.rows, c.cols, data)));
    }
    let v = ctx.to_scalar(cond);
    match v.coerce_bool() {
        Ok(true) => {
            if matches!(args[1], Expr::Missing) { Res::num(0.0) } else { ctx.eval(&args[1]) }
        }
        Ok(false) => match args.get(2) {
            Some(Expr::Missing) => Res::num(0.0),
            Some(e) => ctx.eval(e),
            None => Res::V(Value::Bool(false)),
        },
        Err(e) => Res::err(e),
    }
}

fn ifs(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 2 || args.len() % 2 != 0 {
        return bad_args();
    }
    for pair in args.chunks(2) {
        if boolean!(ctx, &pair[0]) {
            return ctx.eval(&pair[1]);
        }
    }
    Res::err(ErrorKind::NA)
}

fn iferror(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    match &r {
        Res::V(Value::Error(_)) => ctx.eval(&args[1]),
        Res::V(_) => r,
        _ => {
            let fallback = ctx.scalar(&args[1]);
            ctx.map1(r, |v| if v.is_error() { fallback.clone() } else { v.clone() })
        }
    }
}

fn ifna(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    match &r {
        Res::V(Value::Error(ErrorKind::NA)) => ctx.eval(&args[1]),
        Res::V(_) => r,
        _ => {
            let fallback = ctx.scalar(&args[1]);
            ctx.map1(r, |v| if *v == Value::Error(ErrorKind::NA) { fallback.clone() } else { v.clone() })
        }
    }
}

/// Every logical the arguments hold (ranges: only logicals and numbers count).
fn logicals(ctx: &mut Ctx<'_>, args: &[Expr]) -> Result<Vec<bool>, ErrorKind> {
    let mut out = Vec::new();
    for a in args {
        let r = ctx.eval(a);
        match r {
            Res::V(v) => match v {
                Value::Empty => {}
                other => out.push(other.coerce_bool()?),
            },
            other => {
                let mut err = None;
                ctx.each(&other, |v| match v {
                    Value::Bool(b) => out.push(*b),
                    Value::Number(n) => out.push(*n != 0.0),
                    Value::Error(e) if err.is_none() => err = Some(*e),
                    _ => {}
                });
                if let Some(e) = err {
                    return Err(e);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ErrorKind::Value);
    }
    Ok(out)
}

fn and(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match logicals(ctx, args) {
        Ok(v) => Res::V(Value::Bool(v.iter().all(|&b| b))),
        Err(e) => Res::err(e),
    }
}

fn or(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match logicals(ctx, args) {
        Ok(v) => Res::V(Value::Bool(v.iter().any(|&b| b))),
        Err(e) => Res::err(e),
    }
}

fn xor(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match logicals(ctx, args) {
        Ok(v) => Res::V(Value::Bool(v.iter().filter(|&&b| b).count() % 2 == 1)),
        Err(e) => Res::err(e),
    }
}

fn not(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match v.coerce_bool() {
        Ok(b) => Value::Bool(!b),
        Err(e) => Value::Error(e),
    })
}

fn true_(_: &mut Ctx<'_>, _: &[Expr]) -> R {
    Res::V(Value::Bool(true))
}

fn false_(_: &mut Ctx<'_>, _: &[Expr]) -> R {
    Res::V(Value::Bool(false))
}

fn switch(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 3 {
        return bad_args();
    }
    let v = ctx.scalar(&args[0]);
    if let Value::Error(e) = v {
        return Res::err(e);
    }
    let rest = &args[1..];
    let pairs = rest.len() / 2;
    for i in 0..pairs {
        let candidate = ctx.scalar(&rest[i * 2]);
        if compare(&v, &candidate) == Ordering::Equal && std::mem::discriminant(&v) == std::mem::discriminant(&candidate) {
            return ctx.eval(&rest[i * 2 + 1]);
        }
    }
    if rest.len() % 2 == 1 { ctx.eval(&rest[rest.len() - 1]) } else { Res::err(ErrorKind::NA) }
}

fn choose(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 2 {
        return bad_args();
    }
    let i = num!(ctx, &args[0]).trunc();
    if i < 1.0 || i as usize >= args.len() {
        return bad_args();
    }
    ctx.eval(&args[i as usize])
}

fn let_(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 3 || args.len() % 2 == 0 {
        return bad_args();
    }
    let mut bound = 0;
    for pair in args[..args.len() - 1].chunks(2) {
        let Expr::Name(name) = &pair[0] else {
            ctx.unbind(bound);
            return bad_args();
        };
        let v = ctx.eval(&pair[1]);
        ctx.bind(name.clone(), v);
        bound += 1;
    }
    let out = ctx.eval(&args[args.len() - 1]);
    ctx.unbind(bound);
    out
}

// ---- information -------------------------------------------------------------------------------------------

fn is_fn(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(&Value) -> bool) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| Value::Bool(f(v)))
}

fn isblank(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| v.is_empty())
}

fn isnumber(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| matches!(v, Value::Number(_)))
}

fn istext(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| matches!(v, Value::Text(_)))
}

fn isnontext(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| !matches!(v, Value::Text(_)))
}

fn islogical(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| matches!(v, Value::Bool(_)))
}

fn iserror(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| v.is_error())
}

fn iserr(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| v.is_error() && *v != Value::Error(ErrorKind::NA))
}

fn isna(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    is_fn(ctx, args, |v| *v == Value::Error(ErrorKind::NA))
}

fn iseven(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match as_num(v) {
        Ok(n) => Value::Bool(n.trunc() % 2.0 == 0.0),
        Err(e) => Value::Error(e),
    })
}

fn isodd(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match as_num(v) {
        Ok(n) => Value::Bool(n.trunc() % 2.0 != 0.0),
        Err(e) => Value::Error(e),
    })
}

fn isformula(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    match ctx.eval(&args[0]) {
        Res::R(a) => Res::V(Value::Bool(ctx.grid.formula_text(a.tab, a.rect.r1, a.rect.c1).is_some())),
        Res::V(Value::Error(e)) => Res::err(e),
        _ => Res::err(ErrorKind::Value),
    }
}

fn formulatext(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    match ctx.eval(&args[0]) {
        Res::R(a) => match ctx.grid.formula_text(a.tab, a.rect.r1, a.rect.c1) {
            Some(t) => Res::V(Value::from(t)),
            None => Res::err(ErrorKind::NA),
        },
        Res::V(Value::Error(e)) => Res::err(e),
        _ => Res::err(ErrorKind::NA),
    }
}

fn na(_: &mut Ctx<'_>, _: &[Expr]) -> R {
    Res::err(ErrorKind::NA)
}

fn error_type(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    match ctx.scalar(&args[0]) {
        Value::Error(e) => Res::num(e.type_number()),
        _ => Res::err(ErrorKind::NA),
    }
}

fn n(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    Res::V(match ctx.scalar(&args[0]) {
        Value::Number(x) => Value::Number(x),
        Value::Bool(b) => Value::Number(if b { 1.0 } else { 0.0 }),
        Value::Error(e) => Value::Error(e),
        _ => Value::Number(0.0),
    })
}

fn t(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    Res::V(match ctx.scalar(&args[0]) {
        Value::Text(s) => Value::Text(s),
        Value::Error(e) => Value::Error(e),
        _ => Value::from(""),
    })
}

fn type_(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    if r.dims() != (1, 1) {
        return Res::num(64.0);
    }
    Res::num(match ctx.to_scalar(r) {
        Value::Number(_) | Value::Empty => 1.0,
        Value::Text(_) => 2.0,
        Value::Bool(_) => 4.0,
        Value::Error(_) => 16.0,
    })
}

// ---- text --------------------------------------------------------------------------------------------------

fn concat(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let mut out = String::new();
    for a in args {
        let r = ctx.eval(a);
        let mut err = None;
        ctx.each(&r, |v| match v.coerce_text() {
            Ok(s) => out.push_str(&s),
            Err(e) => {
                if err.is_none() {
                    err = Some(e)
                }
            }
        });
        if let Some(e) = err {
            return Res::err(e);
        }
    }
    if out.chars().count() > 32_767 {
        return Res::err(ErrorKind::Value);
    }
    Res::V(Value::from(out))
}

fn textjoin(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 3 {
        return bad_args();
    }
    let delim = txt!(ctx, &args[0]);
    let skip_empty = boolean!(ctx, &args[1]);
    let mut parts: Vec<String> = Vec::new();
    for a in &args[2..] {
        let r = ctx.eval(a);
        let mut err = None;
        ctx.each(&r, |v| match v.coerce_text() {
            Ok(s) => {
                if !(skip_empty && s.is_empty()) {
                    parts.push(s.to_string())
                }
            }
            Err(e) => {
                if err.is_none() {
                    err = Some(e)
                }
            }
        });
        if let Some(e) = err {
            return Res::err(e);
        }
    }
    Res::V(Value::from(parts.join(&delim)))
}

fn text1(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(&str) -> String) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match v.coerce_text() {
        Ok(s) => Value::from(f(&s)),
        Err(e) => Value::Error(e),
    })
}

fn len(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match v.coerce_text() {
        Ok(s) => Value::Number(s.chars().count() as f64),
        Err(e) => Value::Error(e),
    })
}

fn upper(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text1(ctx, args, |s| s.to_uppercase())
}

fn lower(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text1(ctx, args, |s| s.to_lowercase())
}

fn proper(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text1(ctx, args, |s| {
        let mut out = String::with_capacity(s.len());
        let mut prev_letter = false;
        for c in s.chars() {
            if prev_letter {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_letter = c.is_alphabetic();
        }
        out
    })
}

fn trim(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text1(ctx, args, |s| s.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
}

fn clean(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text1(ctx, args, |s| s.chars().filter(|c| (*c as u32) >= 32).collect())
}

fn left(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let count = args.get(1).cloned().filter(|e| !matches!(e, Expr::Missing)).unwrap_or(Expr::Number(1.0));
    lift2(ctx, &args[0], &count, |s, n| match (s.coerce_text(), as_num(n)) {
        (Ok(_), Ok(n)) if n < 0.0 => Value::Error(ErrorKind::Value),
        (Ok(s), Ok(n)) => Value::from(s.chars().take(n as usize).collect::<String>()),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn right(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let count = args.get(1).cloned().filter(|e| !matches!(e, Expr::Missing)).unwrap_or(Expr::Number(1.0));
    lift2(ctx, &args[0], &count, |s, n| match (s.coerce_text(), as_num(n)) {
        (Ok(_), Ok(n)) if n < 0.0 => Value::Error(ErrorKind::Value),
        (Ok(s), Ok(n)) => {
            let chars: Vec<char> = s.chars().collect();
            let k = (n as usize).min(chars.len());
            Value::from(chars[chars.len() - k..].iter().collect::<String>())
        }
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn mid(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 3 {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let start = num!(ctx, &args[1]).trunc();
    let n = num!(ctx, &args[2]).trunc();
    if start < 1.0 || n < 0.0 {
        return bad_args();
    }
    Res::V(Value::from(s.chars().skip(start as usize - 1).take(n as usize).collect::<String>()))
}

fn text(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    lift2(ctx, &args[0], &args[1], |v, f| {
        let code = match f.coerce_text() {
            Ok(c) => c,
            Err(e) => return Value::Error(e),
        };
        let v = match v {
            Value::Text(s) => match crate::value::parse_number_text(s) {
                Some(n) => Value::Number(n),
                None => v.clone(),
            },
            Value::Error(e) => return Value::Error(*e),
            other => other.clone(),
        };
        Value::from(numfmt::format_value(&v, Some(&code)).0)
    })
}

fn value(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.is_empty() {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match v {
        Value::Number(n) => Value::Number(*n),
        Value::Empty => Value::Number(0.0),
        Value::Text(s) => crate::value::parse_number_text(s).map(Value::Number).unwrap_or(Value::Error(ErrorKind::Value)),
        Value::Bool(_) => Value::Error(ErrorKind::Value),
        Value::Error(e) => Value::Error(*e),
    })
}

fn find_impl(ctx: &mut Ctx<'_>, args: &[Expr], case_insensitive: bool) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let needle = txt!(ctx, &args[0]);
    let hay = txt!(ctx, &args[1]);
    let start = match opt(args, 2) {
        Some(e) => num!(ctx, e).trunc(),
        None => 1.0,
    };
    let hay_chars: Vec<char> = hay.chars().collect();
    if start < 1.0 || start as usize > hay_chars.len() + 1 {
        return bad_args();
    }
    let s0 = start as usize - 1;
    if case_insensitive {
        let pattern = wildcard(&needle).unwrap_or_else(|| needle.chars().map(WildTok::Lit).collect());
        for i in s0..=hay_chars.len() {
            for j in i..=hay_chars.len() {
                if wild_match(&pattern, &hay_chars[i..j]) {
                    return Res::num((i + 1) as f64);
                }
            }
        }
        return bad_args();
    }
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() {
        return Res::num(start);
    }
    for i in s0..hay_chars.len() {
        if hay_chars[i..].starts_with(&needle_chars) {
            return Res::num((i + 1) as f64);
        }
    }
    bad_args()
}

fn find(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    find_impl(ctx, args, false)
}

fn search(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    find_impl(ctx, args, true)
}

fn substitute(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 3, 4) {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let old = txt!(ctx, &args[1]);
    let new = txt!(ctx, &args[2]);
    if old.is_empty() {
        return Res::V(Value::Text(s));
    }
    match opt(args, 3) {
        None => Res::V(Value::from(s.replace(&*old, &new))),
        Some(e) => {
            let k = num!(ctx, e).trunc();
            if k < 1.0 {
                return bad_args();
            }
            let mut count = 0;
            let mut out = String::new();
            let mut rest: &str = &s;
            while let Some(i) = rest.find(&*old) {
                count += 1;
                if count == k as usize {
                    out.push_str(&rest[..i]);
                    out.push_str(&new);
                    out.push_str(&rest[i + old.len()..]);
                    return Res::V(Value::from(out));
                }
                out.push_str(&rest[..i + old.len()]);
                rest = &rest[i + old.len()..];
            }
            Res::V(Value::Text(s))
        }
    }
}

fn replace(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 4 {
        return bad_args();
    }
    let s: Vec<char> = txt!(ctx, &args[0]).chars().collect();
    let start = num!(ctx, &args[1]).trunc();
    let n = num!(ctx, &args[2]).trunc();
    let new = txt!(ctx, &args[3]);
    if start < 1.0 || n < 0.0 {
        return bad_args();
    }
    let a = (start as usize - 1).min(s.len());
    let b = (a + n as usize).min(s.len());
    let mut out: String = s[..a].iter().collect();
    out.push_str(&new);
    out.extend(&s[b..]);
    Res::V(Value::from(out))
}

fn rept(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let n = num!(ctx, &args[1]).trunc();
    if n < 0.0 || s.chars().count() as f64 * n > 32_767.0 {
        return bad_args();
    }
    Res::V(Value::from(s.repeat(n as usize)))
}

fn exact(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    lift2(ctx, &args[0], &args[1], |a, b| match (a.coerce_text(), b.coerce_text()) {
        (Ok(x), Ok(y)) => Value::Bool(x == y),
        (Err(e), _) | (_, Err(e)) => Value::Error(e),
    })
}

fn char_(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match as_num(v) {
        Ok(n) if n >= 1.0 => char::from_u32(n as u32).map(|c| Value::from(c.to_string())).unwrap_or(Value::Error(ErrorKind::Value)),
        Ok(_) => Value::Error(ErrorKind::Value),
        Err(e) => Value::Error(e),
    })
}

fn code(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match v.coerce_text() {
        Ok(s) => s.chars().next().map(|c| Value::Number(c as u32 as f64)).unwrap_or(Value::Error(ErrorKind::Value)),
        Err(e) => Value::Error(e),
    })
}

fn fixed(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 3) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let d = match opt(args, 1) {
        Some(e) => num!(ctx, e).trunc() as i32,
        None => 2,
    };
    let no_commas = match opt(args, 2) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let rounded = match round_with(x, d as f64, 0) {
        Value::Number(n) => n,
        _ => return Res::err(ErrorKind::Num),
    };
    let decimals = d.max(0) as usize;
    let code = if no_commas {
        if decimals > 0 { format!("0.{}", "0".repeat(decimals)) } else { "0".into() }
    } else if decimals > 0 {
        format!("#,##0.{}", "0".repeat(decimals))
    } else {
        "#,##0".into()
    };
    Res::V(Value::from(numfmt::format_number(rounded, &code).0))
}

fn dollar(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let d = match opt(args, 1) {
        Some(e) => num!(ctx, e).trunc() as i32,
        None => 2,
    };
    let decimals = d.max(0) as usize;
    let code = if decimals > 0 {
        format!("$#,##0.{0};($#,##0.{0})", "0".repeat(decimals))
    } else {
        "$#,##0;($#,##0)".into()
    };
    Res::V(Value::from(numfmt::format_number(x, &code).0))
}

fn text_split_at(ctx: &mut Ctx<'_>, args: &[Expr], after: bool) -> R {
    if !arity(args, 2, 6) {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let delim = txt!(ctx, &args[1]);
    let instance = match opt(args, 2) {
        Some(e) => num!(ctx, e).trunc() as i64,
        None => 1,
    };
    let ci = match opt(args, 3) {
        Some(e) => num!(ctx, e) != 0.0,
        None => false,
    };
    if instance == 0 || delim.is_empty() {
        return bad_args();
    }
    let (hay, needle) = if ci { (s.to_lowercase(), delim.to_lowercase()) } else { (s.to_string(), delim.to_string()) };
    let positions: Vec<usize> = hay.match_indices(&needle).map(|(i, _)| i).collect();
    let idx = if instance > 0 {
        positions.get(instance as usize - 1)
    } else {
        positions.len().checked_sub((-instance) as usize).and_then(|i| positions.get(i))
    };
    match idx {
        Some(&i) => Res::V(Value::from(if after { s[i + delim.len()..].to_string() } else { s[..i].to_string() })),
        None => match opt(args, 5) {
            Some(e) => ctx.eval(e),
            None => Res::err(ErrorKind::NA),
        },
    }
}

fn textbefore(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text_split_at(ctx, args, false)
}

fn textafter(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    text_split_at(ctx, args, true)
}

fn textsplit(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 6) {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let col_delim = match opt(args, 1) {
        Some(e) => Some(txt!(ctx, e)),
        None => None,
    };
    let row_delim = match opt(args, 2) {
        Some(e) => Some(txt!(ctx, e)),
        None => None,
    };
    let ignore_empty = match opt(args, 3) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let rows: Vec<String> = match &row_delim {
        Some(d) if !d.is_empty() => s.split(&**d).map(str::to_owned).collect(),
        _ => vec![s.to_string()],
    };
    let mut grid: Vec<Vec<Value>> = Vec::new();
    for r in rows {
        let cells: Vec<Value> = match &col_delim {
            Some(d) if !d.is_empty() => r.split(&**d).filter(|p| !(ignore_empty && p.is_empty())).map(Value::from).collect(),
            _ => vec![Value::from(r)],
        };
        if !(ignore_empty && cells.is_empty()) {
            grid.push(cells);
        }
    }
    let width = grid.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let height = grid.len() as u32;
    let mut data = Vec::new();
    for row in grid {
        let n = row.len();
        data.extend(row);
        data.extend(std::iter::repeat_n(Value::Error(ErrorKind::NA), width as usize - n));
    }
    Res::A(Arc::new(Array::new(height, width, data)))
}

// ---- lookup ------------------------------------------------------------------------------------------------

/// Excel's "same value" for lookups: case-insensitive text, numbers by value, no type coercion.
fn lookup_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Text(x), Value::Text(y)) => cmp_text_ci(x, y) == Ordering::Equal,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Empty, Value::Empty) => true,
        _ => false,
    }
}

fn same_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_)) | (Value::Text(_), Value::Text(_)) | (Value::Bool(_), Value::Bool(_))
    )
}

/// Exact match position (with wildcards for text).
fn exact_index(values: &[Value], needle: &Value, wildcards: bool) -> Option<usize> {
    if wildcards
        && let Value::Text(t) = needle
        && let Some(p) = wildcard(t)
    {
        return values.iter().position(|v| match v {
            Value::Text(s) => wild_match(&p, &s.chars().collect::<Vec<_>>()),
            _ => false,
        });
    }
    values.iter().position(|v| lookup_eq(v, needle))
}

/// Approximate match in ascending data: the last position whose value is <= needle (binary search, like Excel).
fn approx_index(values: &[Value], needle: &Value) -> Option<usize> {
    let (mut lo, mut hi) = (0usize, values.len());
    let mut found = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let v = &values[mid];
        if v.is_empty() || !same_type(v, needle) {
            // Skip values of another type by scanning down for one of the same type.
            let mut j = mid;
            while j > lo && !same_type(&values[j], needle) {
                j -= 1;
            }
            if same_type(&values[j], needle) && compare(&values[j], needle) != Ordering::Greater {
                found = Some(j);
                lo = mid + 1;
            } else {
                hi = mid;
            }
            continue;
        }
        if compare(v, needle) != Ordering::Greater {
            found = Some(mid);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    found
}

/// A lookup vector from a range or array: one row or one column.
fn vector(ctx: &mut Ctx<'_>, r: &Res) -> Option<Vec<Value>> {
    let a = ctx.to_array(r);
    if a.rows == 1 || a.cols == 1 { Some(a.data.clone()) } else { None }
}

fn vlookup(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    hv_lookup(ctx, args, true)
}

fn hlookup(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    hv_lookup(ctx, args, false)
}

fn hv_lookup(ctx: &mut Ctx<'_>, args: &[Expr], vertical: bool) -> R {
    if !arity(args, 3, 4) {
        return bad_args();
    }
    let needle = ctx.scalar(&args[0]);
    if let Value::Error(e) = needle {
        return Res::err(e);
    }
    let table = ctx.eval(&args[1]);
    if let Some(e) = table.as_error() {
        return Res::err(e);
    }
    let idx = num!(ctx, &args[2]).trunc();
    let approximate = match opt(args, 3) {
        Some(e) => boolean!(ctx, e),
        None => true,
    };
    let arr = ctx.to_array(&table);
    let (n, width) = if vertical { (arr.rows, arr.cols) } else { (arr.cols, arr.rows) };
    if idx < 1.0 {
        return bad_args();
    }
    if idx as u32 > width {
        return Res::err(ErrorKind::Ref);
    }
    let keys: Vec<Value> = (0..n).map(|i| if vertical { arr.get(i, 0).clone() } else { arr.get(0, i).clone() }).collect();
    let pos = if approximate { approx_index(&keys, &needle) } else { exact_index(&keys, &needle, true) };
    match pos {
        Some(i) => {
            let k = idx as u32 - 1;
            Res::V(if vertical { arr.get(i as u32, k).clone() } else { arr.get(k, i as u32).clone() })
        }
        None => Res::err(ErrorKind::NA),
    }
}

fn match_(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let needle = ctx.scalar(&args[0]);
    if let Value::Error(e) = needle {
        return Res::err(e);
    }
    let r = ctx.eval(&args[1]);
    let Some(values) = vector(ctx, &r) else { return Res::err(ErrorKind::NA) };
    let mode = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    let pos = if mode == 0.0 {
        exact_index(&values, &needle, true)
    } else if mode > 0.0 {
        approx_index(&values, &needle)
    } else {
        // Descending: the smallest value >= needle.
        let mut found = None;
        for (i, v) in values.iter().enumerate() {
            if same_type(v, &needle) && compare(v, &needle) != Ordering::Less {
                found = Some(i);
            } else if same_type(v, &needle) {
                break;
            }
        }
        found
    };
    match pos {
        Some(i) => Res::num((i + 1) as f64),
        None => Res::err(ErrorKind::NA),
    }
}

/// XLOOKUP/XMATCH matching: `match_mode` 0 exact, -1 exact or next smaller, 1 exact or next larger, 2 wildcard;
/// `search_mode` 1 first to last, -1 last to first, 2/-2 binary (treated as linear, same answer on sorted data).
fn x_index(values: &[Value], needle: &Value, match_mode: i32, search_mode: i32) -> Option<usize> {
    let order: Vec<usize> = if search_mode < 0 { (0..values.len()).rev().collect() } else { (0..values.len()).collect() };
    if match_mode == 2 {
        if let Value::Text(t) = needle {
            let p = wildcard(t).unwrap_or_else(|| t.chars().map(WildTok::Lit).collect());
            return order.into_iter().find(|&i| match &values[i] {
                Value::Text(s) => wild_match(&p, &s.chars().collect::<Vec<_>>()),
                _ => false,
            });
        }
    }
    if let Some(i) = order.iter().copied().find(|&i| lookup_eq(&values[i], needle)) {
        return Some(i);
    }
    match match_mode {
        -1 => order
            .iter()
            .copied()
            .filter(|&i| same_type(&values[i], needle) && compare(&values[i], needle) == Ordering::Less)
            .max_by(|&a, &b| compare(&values[a], &values[b])),
        1 => order
            .iter()
            .copied()
            .filter(|&i| same_type(&values[i], needle) && compare(&values[i], needle) == Ordering::Greater)
            .min_by(|&a, &b| compare(&values[a], &values[b])),
        _ => None,
    }
}

fn xlookup(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 3, 6) {
        return bad_args();
    }
    let needle_res = ctx.eval(&args[0]);
    let lookup_res = ctx.eval(&args[1]);
    let Some(values) = vector(ctx, &lookup_res) else { return bad_args() };
    let ret = ctx.eval(&args[2]);
    let ret_arr = ctx.to_array(&ret);
    let match_mode = match opt(args, 4) {
        Some(e) => num!(ctx, e) as i32,
        None => 0,
    };
    let search_mode = match opt(args, 5) {
        Some(e) => num!(ctx, e) as i32,
        None => 1,
    };
    let vertical = {
        let a = ctx.to_array(&lookup_res);
        a.cols == 1
    };
    let one = |ctx: &mut Ctx<'_>, needle: &Value| -> Res {
        if let Value::Error(e) = needle {
            return Res::err(*e);
        }
        match x_index(&values, needle, match_mode, search_mode) {
            Some(i) => {
                if vertical {
                    if ret_arr.rows as usize != values.len() {
                        return bad_args();
                    }
                    if ret_arr.cols == 1 {
                        Res::V(ret_arr.get(i as u32, 0).clone())
                    } else {
                        let row: Vec<Value> = (0..ret_arr.cols).map(|c| ret_arr.get(i as u32, c).clone()).collect();
                        Res::A(Arc::new(Array::row(row)))
                    }
                } else {
                    if ret_arr.cols as usize != values.len() {
                        return bad_args();
                    }
                    if ret_arr.rows == 1 {
                        Res::V(ret_arr.get(0, i as u32).clone())
                    } else {
                        let col: Vec<Value> = (0..ret_arr.rows).map(|r| ret_arr.get(r, i as u32).clone()).collect();
                        Res::A(Arc::new(Array::column(col)))
                    }
                }
            }
            None => match opt(args, 3) {
                Some(e) => ctx.eval(e),
                None => Res::err(ErrorKind::NA),
            },
        }
    };
    if needle_res.dims() == (1, 1) {
        let needle = ctx.to_scalar(needle_res);
        return one(ctx, &needle);
    }
    // An array of lookups gives an array of answers.
    let needles = ctx.to_array(&needle_res);
    let mut data = Vec::with_capacity(needles.data.len());
    for nv in needles.data.iter() {
        let r = one(ctx, nv);
        data.push(ctx.to_scalar(r));
    }
    Res::A(Arc::new(Array::new(needles.rows, needles.cols, data)))
}

fn xmatch(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 4) {
        return bad_args();
    }
    let needle = ctx.scalar(&args[0]);
    let r = ctx.eval(&args[1]);
    let Some(values) = vector(ctx, &r) else { return bad_args() };
    let match_mode = match opt(args, 2) {
        Some(e) => num!(ctx, e) as i32,
        None => 0,
    };
    let search_mode = match opt(args, 3) {
        Some(e) => num!(ctx, e) as i32,
        None => 1,
    };
    match x_index(&values, &needle, match_mode, search_mode) {
        Some(i) => Res::num((i + 1) as f64),
        None => Res::err(ErrorKind::NA),
    }
}

fn lookup_fn(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let needle = ctx.scalar(&args[0]);
    let r = ctx.eval(&args[1]);
    let arr = ctx.to_array(&r);
    let (keys, results): (Vec<Value>, Vec<Value>) = match opt(args, 2) {
        Some(e) => {
            let rr = ctx.eval(e);
            let res = ctx.to_array(&rr);
            let keys = if arr.cols == 1 || arr.rows == 1 { arr.data.clone() } else { (0..arr.rows).map(|i| arr.get(i, 0).clone()).collect() };
            (keys, res.data.clone())
        }
        None => {
            if arr.rows >= arr.cols {
                ((0..arr.rows).map(|i| arr.get(i, 0).clone()).collect(), (0..arr.rows).map(|i| arr.get(i, arr.cols - 1).clone()).collect())
            } else {
                ((0..arr.cols).map(|i| arr.get(0, i).clone()).collect(), (0..arr.cols).map(|i| arr.get(arr.rows - 1, i).clone()).collect())
            }
        }
    };
    match approx_index(&keys, &needle) {
        Some(i) => Res::V(results.get(i).cloned().unwrap_or(Value::Error(ErrorKind::NA))),
        None => Res::err(ErrorKind::NA),
    }
}

fn index(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    if let Some(e) = r.as_error() {
        return Res::err(e);
    }
    let (rows, cols) = r.dims();
    let mut ri = num!(ctx, &args[1]).trunc() as i64;
    let mut ci = match opt(args, 2) {
        Some(e) => num!(ctx, e).trunc() as i64,
        None => 0,
    };
    // INDEX(row_vector, n) picks the nth item.
    if args.len() == 2 && rows == 1 && cols > 1 {
        ci = ri;
        ri = 0;
    }
    if ri < 0 || ci < 0 || ri as u32 > rows || ci as u32 > cols {
        return Res::err(ErrorKind::Ref);
    }
    match r {
        Res::R(a) => {
            let rect = a.rect;
            let (r1, r2) = if ri == 0 { (rect.r1, rect.r2) } else { (rect.r1 + ri as u32 - 1, rect.r1 + ri as u32 - 1) };
            let (c1, c2) = if ci == 0 { (rect.c1, rect.c2) } else { (rect.c1 + ci as u32 - 1, rect.c1 + ci as u32 - 1) };
            Res::R(Area { tab: a.tab, rect: Rect::new(r1, c1, r2, c2) })
        }
        other => {
            let arr = ctx.to_array(&other);
            match (ri, ci) {
                (0, 0) => Res::A(arr),
                (0, c) => Res::A(Arc::new(Array::column((0..arr.rows).map(|r| arr.get(r, c as u32 - 1).clone()).collect()))),
                (r, 0) if arr.cols == 1 => Res::V(arr.get(r as u32 - 1, 0).clone()),
                (r, 0) => Res::A(Arc::new(Array::row((0..arr.cols).map(|c| arr.get(r as u32 - 1, c).clone()).collect()))),
                (r, c) => Res::V(arr.get(r as u32 - 1, c as u32 - 1).clone()),
            }
        }
    }
}

fn row(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match opt(args, 0) {
        None => Res::num((ctx.row + 1) as f64),
        Some(e) => match ctx.eval(e) {
            Res::R(a) if a.rect.rows() == 1 || a.rect.r2 == MAX_ROWS - 1 => Res::num((a.rect.r1 + 1) as f64),
            Res::R(a) => Res::A(Arc::new(Array::column((a.rect.r1..=a.rect.r2).map(|r| Value::Number((r + 1) as f64)).collect()))),
            Res::V(Value::Error(e)) => Res::err(e),
            _ => bad_args(),
        },
    }
}

fn column(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    match opt(args, 0) {
        None => Res::num((ctx.col + 1) as f64),
        Some(e) => match ctx.eval(e) {
            Res::R(a) if a.rect.cols() == 1 || a.rect.c2 == MAX_COLS - 1 => Res::num((a.rect.c1 + 1) as f64),
            Res::R(a) => Res::A(Arc::new(Array::row((a.rect.c1..=a.rect.c2).map(|c| Value::Number((c + 1) as f64)).collect()))),
            Res::V(Value::Error(e)) => Res::err(e),
            _ => bad_args(),
        },
    }
}

fn rows(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    if let Some(e) = r.as_error() {
        return Res::err(e);
    }
    Res::num(r.dims().0 as f64)
}

fn columns(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    if let Some(e) = r.as_error() {
        return Res::err(e);
    }
    Res::num(r.dims().1 as f64)
}

fn indirect(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    let (tab_name, rest) = crate::addr::split_tab(&s);
    let tab = match tab_name {
        Some(n) => match ctx.grid.tab_id(&n) {
            Some(t) => t,
            None => return Res::err(ErrorKind::Ref),
        },
        None => ctx.tab,
    };
    match Rect::parse(rest, MAX_ROWS, MAX_COLS) {
        Some(rect) => Res::R(Area { tab, rect }),
        None => match ctx.grid.defined_name(rest, ctx.tab) {
            Some(e) => ctx.eval(&e),
            None => Res::err(ErrorKind::Ref),
        },
    }
}

fn offset(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 3, 5) {
        return bad_args();
    }
    let base = match ctx.eval(&args[0]) {
        Res::R(a) => a,
        Res::V(Value::Error(e)) => return Res::err(e),
        _ => return bad_args(),
    };
    let dr = num!(ctx, &args[1]).trunc() as i64;
    let dc = num!(ctx, &args[2]).trunc() as i64;
    let h = match opt(args, 3) {
        Some(e) => num!(ctx, e).trunc() as i64,
        None => base.rect.rows() as i64,
    };
    let w = match opt(args, 4) {
        Some(e) => num!(ctx, e).trunc() as i64,
        None => base.rect.cols() as i64,
    };
    let r1 = base.rect.r1 as i64 + dr;
    let c1 = base.rect.c1 as i64 + dc;
    if r1 < 0 || c1 < 0 || h < 1 || w < 1 || r1 + h > MAX_ROWS as i64 || c1 + w > MAX_COLS as i64 {
        return Res::err(ErrorKind::Ref);
    }
    Res::R(Area { tab: base.tab, rect: Rect::new(r1 as u32, c1 as u32, (r1 + h - 1) as u32, (c1 + w - 1) as u32) })
}

fn address(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 5) {
        return bad_args();
    }
    let r = num!(ctx, &args[0]).trunc();
    let c = num!(ctx, &args[1]).trunc();
    let abs = match opt(args, 2) {
        Some(e) => num!(ctx, e) as i32,
        None => 1,
    };
    let a1 = match opt(args, 3) {
        Some(e) => boolean!(ctx, e),
        None => true,
    };
    if r < 1.0 || c < 1.0 || r > MAX_ROWS as f64 || c > MAX_COLS as f64 {
        return bad_args();
    }
    let (ar, ac) = match abs {
        1 => (true, true),
        2 => (true, false),
        3 => (false, true),
        _ => (false, false),
    };
    let body = if a1 {
        format!(
            "{}{}{}{}",
            if ac { "$" } else { "" },
            crate::addr::col_name(c as u32 - 1),
            if ar { "$" } else { "" },
            r as u32
        )
    } else {
        format!(
            "R{}C{}",
            if ar { (r as u32).to_string() } else { format!("[{}]", r as u32) },
            if ac { (c as u32).to_string() } else { format!("[{}]", c as u32) }
        )
    };
    let full = match opt(args, 4) {
        Some(e) => format!("{}!{}", crate::addr::quote_tab(&txt!(ctx, e)), body),
        None => body,
    };
    Res::V(Value::from(full))
}

fn sheet(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let tab = match opt(args, 0) {
        None => ctx.tab,
        Some(e) => match ctx.eval(e) {
            Res::R(a) => a.tab,
            Res::V(Value::Text(name)) => match ctx.grid.tab_id(&name) {
                Some(t) => t,
                None => return Res::err(ErrorKind::NA),
            },
            _ => return bad_args(),
        },
    };
    match ctx.grid.tab_position(tab) {
        Some(p) => Res::num((p + 1) as f64),
        None => Res::err(ErrorKind::NA),
    }
}

fn sheets(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if opt(args, 0).is_some() {
        return Res::num(1.0);
    }
    Res::num(ctx.grid.tab_count() as f64)
}

// ---- dates -------------------------------------------------------------------------------------------------

fn date(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 3 {
        return bad_args();
    }
    let y = num!(ctx, &args[0]).trunc() as i64;
    let m = num!(ctx, &args[1]).trunc() as i64;
    let d = num!(ctx, &args[2]).trunc() as i64;
    match dates::serial_from_ymd(y, m, d) {
        Some(s) => Res::num(s),
        None => Res::err(ErrorKind::Num),
    }
}

fn time(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 3 {
        return bad_args();
    }
    let h = num!(ctx, &args[0]).trunc();
    let m = num!(ctx, &args[1]).trunc();
    let s = num!(ctx, &args[2]).trunc();
    let total = h * 3600.0 + m * 60.0 + s;
    if total < 0.0 {
        return Res::err(ErrorKind::Num);
    }
    Res::num((total / 86400.0).fract())
}

fn today(ctx: &mut Ctx<'_>, _: &[Expr]) -> R {
    Res::num(ctx.grid.now().floor())
}

fn now(ctx: &mut Ctx<'_>, _: &[Expr]) -> R {
    Res::num(ctx.grid.now())
}

fn date_part(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(f64) -> Option<f64>) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    lift(ctx, &args[0], |v| match as_num(v) {
        Ok(s) => f(s).map(Value::Number).unwrap_or(Value::Error(ErrorKind::Num)),
        Err(e) => Value::Error(e),
    })
}

fn year(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| dates::ymd_from_serial(s).map(|d| d.0 as f64))
}

fn month(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| dates::ymd_from_serial(s).map(|d| d.1 as f64))
}

fn day(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| dates::ymd_from_serial(s).map(|d| d.2 as f64))
}

fn hour(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| (s >= 0.0).then(|| dates::hms_from_serial(s).0 as f64))
}

fn minute(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| (s >= 0.0).then(|| dates::hms_from_serial(s).1 as f64))
}

fn second(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    date_part(ctx, args, |s| (s >= 0.0).then(|| dates::hms_from_serial(s).2 as f64))
}

fn weekday(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let s = num!(ctx, &args[0]);
    let kind = match opt(args, 1) {
        Some(e) => num!(ctx, e) as i64,
        None => 1,
    };
    if s < 0.0 {
        return Res::err(ErrorKind::Num);
    }
    let wd = dates::weekday_from_serial(s) as i64; // 0 = Sunday
    let r = match kind {
        1 | 17 => wd + 1,
        2 | 11 => (wd + 6) % 7 + 1,
        3 => (wd + 6) % 7,
        12 => (wd + 5) % 7 + 1,
        13 => (wd + 4) % 7 + 1,
        14 => (wd + 3) % 7 + 1,
        15 => (wd + 2) % 7 + 1,
        16 => (wd + 1) % 7 + 1,
        _ => return Res::err(ErrorKind::Num),
    };
    Res::num(r as f64)
}

fn add_months(serial: f64, months: i64, end_of_month: bool) -> Option<f64> {
    let (y, m, d) = dates::ymd_from_serial(serial)?;
    let total = y * 12 + (m as i64 - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let dim = dates::days_in_month(ny, nm);
    let nd = if end_of_month { dim } else { d.min(dim) };
    dates::serial_from_ymd(ny, nm as i64, nd as i64)
}

fn edate(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let s = num!(ctx, &args[0]).trunc();
    let m = num!(ctx, &args[1]).trunc() as i64;
    match add_months(s, m, false) {
        Some(v) => Res::num(v),
        None => Res::err(ErrorKind::Num),
    }
}

fn eomonth(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let s = num!(ctx, &args[0]).trunc();
    let m = num!(ctx, &args[1]).trunc() as i64;
    match add_months(s, m, true) {
        Some(v) => Res::num(v),
        None => Res::err(ErrorKind::Num),
    }
}

fn datedif(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 3 {
        return bad_args();
    }
    let a = num!(ctx, &args[0]).trunc();
    let b = num!(ctx, &args[1]).trunc();
    let unit = txt!(ctx, &args[2]).to_ascii_uppercase();
    if a > b {
        return Res::err(ErrorKind::Num);
    }
    let (Some((y1, m1, d1)), Some((y2, m2, d2))) = (dates::ymd_from_serial(a), dates::ymd_from_serial(b)) else {
        return Res::err(ErrorKind::Num);
    };
    let months = (y2 - y1) * 12 + m2 as i64 - m1 as i64 - if d2 < d1 { 1 } else { 0 };
    let r = match unit.as_str() {
        "D" => b - a,
        "M" => months as f64,
        "Y" => (months / 12) as f64,
        "YM" => (months % 12) as f64,
        "MD" => {
            if d2 >= d1 {
                (d2 - d1) as f64
            } else {
                let (py, pm) = if m2 == 1 { (y2 - 1, 12) } else { (y2, m2 - 1) };
                (dates::days_in_month(py, pm) as i64 - d1 as i64 + d2 as i64) as f64
            }
        }
        "YD" => {
            let anniversary = dates::serial_from_ymd(y2, m1 as i64, d1 as i64).unwrap_or(a);
            if anniversary <= b {
                b - anniversary
            } else {
                b - dates::serial_from_ymd(y2 - 1, m1 as i64, d1 as i64).unwrap_or(a)
            }
        }
        _ => return Res::err(ErrorKind::Num),
    };
    Res::num(r)
}

fn days(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 2 {
        return bad_args();
    }
    let end = num!(ctx, &args[0]).trunc();
    let start = num!(ctx, &args[1]).trunc();
    Res::num(end - start)
}

fn datevalue(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    match dates::parse_date_time(&s) {
        Some((v, _)) => Res::num(v.floor()),
        None => bad_args(),
    }
}

fn timevalue(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let s = txt!(ctx, &args[0]);
    match dates::parse_date_time(&s) {
        Some((v, _)) => Res::num(v.fract()),
        None => bad_args(),
    }
}

fn holiday_set(ctx: &mut Ctx<'_>, e: Option<&Expr>) -> Result<Vec<f64>, ErrorKind> {
    match e {
        None => Ok(Vec::new()),
        Some(e) => numbers(ctx, std::slice::from_ref(e), Agg::Numbers).map(|v| v.into_iter().map(f64::trunc).collect()),
    }
}

fn is_workday(serial: f64, holidays: &[f64]) -> bool {
    let wd = dates::weekday_from_serial(serial);
    wd != 0 && wd != 6 && !holidays.contains(&serial)
}

fn networkdays(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let a = num!(ctx, &args[0]).trunc();
    let b = num!(ctx, &args[1]).trunc();
    let hol = match holiday_set(ctx, opt(args, 2)) {
        Ok(h) => h,
        Err(e) => return Res::err(e),
    };
    let (lo, hi, sign) = if a <= b { (a, b, 1.0) } else { (b, a, -1.0) };
    if hi - lo > 3_000_000.0 {
        return Res::err(ErrorKind::Num);
    }
    let mut n = 0.0;
    let mut d = lo;
    while d <= hi {
        if is_workday(d, &hol) {
            n += 1.0;
        }
        d += 1.0;
    }
    Res::num(n * sign)
}

fn workday(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let mut d = num!(ctx, &args[0]).trunc();
    let mut n = num!(ctx, &args[1]).trunc() as i64;
    let hol = match holiday_set(ctx, opt(args, 2)) {
        Ok(h) => h,
        Err(e) => return Res::err(e),
    };
    let step = if n >= 0 { 1.0 } else { -1.0 };
    let mut guard = 0;
    while n != 0 {
        d += step;
        if is_workday(d, &hol) {
            n -= step as i64;
        }
        guard += 1;
        if guard > 10_000_000 || d < 0.0 {
            return Res::err(ErrorKind::Num);
        }
    }
    Res::num(d)
}

fn weeknum(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 2) {
        return bad_args();
    }
    let s = num!(ctx, &args[0]).trunc();
    let kind = match opt(args, 1) {
        Some(e) => num!(ctx, e) as i64,
        None => 1,
    };
    if kind == 21 {
        return isoweeknum(ctx, &args[..1]);
    }
    let Some((y, _, _)) = dates::ymd_from_serial(s) else { return Res::err(ErrorKind::Num) };
    let jan1 = dates::serial_from_ymd(y, 1, 1).unwrap_or(s);
    let start_dow = match kind {
        1 | 17 => 0,
        2 | 11 => 1,
        12 => 2,
        13 => 3,
        14 => 4,
        15 => 5,
        16 => 6,
        _ => return Res::err(ErrorKind::Num),
    };
    let jan1_dow = dates::weekday_from_serial(jan1) as i64;
    let offset = (jan1_dow - start_dow).rem_euclid(7);
    Res::num((((s - jan1) as i64 + offset) / 7 + 1) as f64)
}

fn isoweeknum(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let s = num!(ctx, &args[0]).trunc();
    // ISO weeks start on Monday; week 1 holds the year's first Thursday.
    let wd = (dates::weekday_from_serial(s) as i64 + 6) % 7; // 0 = Monday
    let thursday = s - wd as f64 + 3.0;
    let Some((ty, _, _)) = dates::ymd_from_serial(thursday) else { return Res::err(ErrorKind::Num) };
    let jan1 = dates::serial_from_ymd(ty, 1, 1).unwrap_or(thursday);
    Res::num(((thursday - jan1) / 7.0).floor() + 1.0)
}

fn yearfrac(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let mut a = num!(ctx, &args[0]).trunc();
    let mut b = num!(ctx, &args[1]).trunc();
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let basis = match opt(args, 2) {
        Some(e) => num!(ctx, e) as i64,
        None => 0,
    };
    let (Some((y1, m1, d1)), Some((y2, m2, d2))) = (dates::ymd_from_serial(a), dates::ymd_from_serial(b)) else {
        return Res::err(ErrorKind::Num);
    };
    let r = match basis {
        0 => {
            let (mut d1, mut d2) = (d1 as i64, d2 as i64);
            if d1 == 31 {
                d1 = 30;
            }
            if d2 == 31 && d1 >= 30 {
                d2 = 30;
            }
            ((y2 - y1) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2 - d1)) as f64 / 360.0
        }
        1 => {
            let years = (y2 - y1 + 1) as f64;
            let total: f64 = (y1..=y2)
                .map(|y| if dates::days_in_month(y, 2) == 29 { 366.0 } else { 365.0 })
                .sum();
            let avg = if y1 == y2 { total } else { total / years };
            (b - a) / avg
        }
        2 => (b - a) / 360.0,
        3 => (b - a) / 365.0,
        4 => {
            let (d1, d2) = (d1.min(30) as i64, d2.min(30) as i64);
            ((y2 - y1) * 360 + (m2 as i64 - m1 as i64) * 30 + (d2 - d1)) as f64 / 360.0
        }
        _ => return Res::err(ErrorKind::Num),
    };
    Res::num(r)
}

// ---- dynamic arrays ----------------------------------------------------------------------------------------

fn sequence(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 4) {
        return bad_args();
    }
    let rows = num!(ctx, &args[0]).trunc();
    let cols = match opt(args, 1) {
        Some(e) => num!(ctx, e).trunc(),
        None => 1.0,
    };
    let start = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    let step = match opt(args, 3) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    if rows < 1.0 || cols < 1.0 {
        return Res::err(ErrorKind::Calc);
    }
    if rows * cols > 10_000_000.0 {
        return Res::err(ErrorKind::Num);
    }
    let (r, c) = (rows as u32, cols as u32);
    let data = (0..r * c).map(|i| Value::Number(start + step * i as f64)).collect();
    Res::A(Arc::new(Array::new(r, c, data)))
}

fn randarray(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    let rows = match opt(args, 0) {
        Some(e) => num!(ctx, e).trunc(),
        None => 1.0,
    };
    let cols = match opt(args, 1) {
        Some(e) => num!(ctx, e).trunc(),
        None => 1.0,
    };
    let lo = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 0.0,
    };
    let hi = match opt(args, 3) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    let whole = match opt(args, 4) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    if rows < 1.0 || cols < 1.0 || lo > hi || rows * cols > 10_000_000.0 {
        return Res::err(ErrorKind::Value);
    }
    let n = (rows * cols) as usize;
    let data = (0..n)
        .map(|_| {
            let r = ctx.grid.random();
            Value::Number(if whole { (lo + (r * (hi - lo + 1.0)).floor()).min(hi) } else { lo + r * (hi - lo) })
        })
        .collect();
    Res::A(Arc::new(Array::new(rows as u32, cols as u32, data)))
}

fn filter(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    if let Some(e) = src.as_error() {
        return Res::err(e);
    }
    let arr = ctx.to_array(&src);
    let inc = ctx.eval(&args[1]);
    let mask = ctx.to_array(&inc);
    let keep = |v: &Value| -> Result<bool, ErrorKind> { v.coerce_bool() };
    let mut data = Vec::new();
    let (rows, cols);
    if mask.cols == 1 && mask.rows == arr.rows {
        let mut kept = 0;
        for r in 0..arr.rows {
            match keep(mask.get(r, 0)) {
                Ok(true) => {
                    kept += 1;
                    for c in 0..arr.cols {
                        data.push(arr.get(r, c).clone());
                    }
                }
                Ok(false) => {}
                Err(e) => return Res::err(e),
            }
        }
        rows = kept;
        cols = arr.cols;
    } else if mask.rows == 1 && mask.cols == arr.cols {
        let keep_cols: Vec<u32> = match (0..arr.cols).map(|c| keep(mask.get(0, c)).map(|k| (c, k))).collect::<Result<Vec<_>, _>>() {
            Ok(v) => v.into_iter().filter(|(_, k)| *k).map(|(c, _)| c).collect(),
            Err(e) => return Res::err(e),
        };
        for r in 0..arr.rows {
            for &c in &keep_cols {
                data.push(arr.get(r, c).clone());
            }
        }
        rows = arr.rows;
        cols = keep_cols.len() as u32;
    } else {
        return bad_args();
    }
    if rows == 0 || cols == 0 {
        return match opt(args, 2) {
            Some(e) => ctx.eval(e),
            None => Res::err(ErrorKind::Calc),
        };
    }
    Res::A(Arc::new(Array::new(rows, cols, data)))
}

fn sort_rows_by(arr: &Array, keys: &[(Arc<Array>, bool)], by_col: bool) -> Array {
    let n = if by_col { arr.cols } else { arr.rows };
    let mut idx: Vec<u32> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        for (k, asc) in keys {
            let (va, vb) = if by_col { (k.get(0, a), k.get(0, b)) } else { (k.get(a, 0), k.get(b, 0)) };
            let o = va.sort_cmp(vb);
            let o = if *asc { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    let mut data = Vec::with_capacity(arr.data.len());
    if by_col {
        for r in 0..arr.rows {
            for &c in &idx {
                data.push(arr.get(r, c).clone());
            }
        }
    } else {
        for &r in &idx {
            for c in 0..arr.cols {
                data.push(arr.get(r, c).clone());
            }
        }
    }
    Array::new(arr.rows, arr.cols, data)
}

fn sort(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 4) {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    if let Some(e) = src.as_error() {
        return Res::err(e);
    }
    let arr = ctx.to_array(&src);
    let by_col = match opt(args, 3) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let order = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => 1.0,
    };
    let index = match opt(args, 1) {
        Some(e) => num!(ctx, e).trunc() as u32,
        None => 1,
    };
    let limit = if by_col { arr.rows } else { arr.cols };
    if index < 1 || index > limit {
        return bad_args();
    }
    let key: Arc<Array> = if by_col {
        Arc::new(Array::row((0..arr.cols).map(|c| arr.get(index - 1, c).clone()).collect()))
    } else {
        Arc::new(Array::column((0..arr.rows).map(|r| arr.get(r, index - 1).clone()).collect()))
    };
    Res::A(Arc::new(sort_rows_by(&arr, &[(key, order >= 0.0)], by_col)))
}

fn sortby(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() < 2 {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    let arr = ctx.to_array(&src);
    let mut keys = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let k = ctx.eval(&args[i]);
        let ka = ctx.to_array(&k);
        let asc = match opt(args, i + 1) {
            Some(e) => num!(ctx, e) >= 0.0,
            None => true,
        };
        if ka.rows != arr.rows && ka.cols != arr.cols {
            return bad_args();
        }
        keys.push((ka, asc));
        i += 2;
    }
    let by_col = keys.first().is_some_and(|(k, _)| k.rows == 1 && arr.rows != 1 && k.cols == arr.cols);
    Res::A(Arc::new(sort_rows_by(&arr, &keys, by_col)))
}

fn unique(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if !arity(args, 1, 3) {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    if let Some(e) = src.as_error() {
        return Res::err(e);
    }
    let arr = ctx.to_array(&src);
    let by_col = match opt(args, 1) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let exactly_once = match opt(args, 2) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let n = if by_col { arr.cols } else { arr.rows };
    let line = |i: u32| -> Vec<Value> {
        if by_col { (0..arr.rows).map(|r| arr.get(r, i).clone()).collect() } else { (0..arr.cols).map(|c| arr.get(i, c).clone()).collect() }
    };
    let same = |a: &[Value], b: &[Value]| a.iter().zip(b).all(|(x, y)| lookup_eq(x, y) || (x.is_empty() && y.is_empty()));
    let lines: Vec<Vec<Value>> = (0..n).map(line).collect();
    let mut keep: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let count = lines.iter().filter(|m| same(l, m)).count();
        let first = lines[..i].iter().all(|m| !same(l, m));
        if first && (!exactly_once || count == 1) {
            keep.push(i);
        }
    }
    if keep.is_empty() {
        return Res::err(ErrorKind::Calc);
    }
    let mut data = Vec::new();
    if by_col {
        for r in 0..arr.rows {
            for &i in &keep {
                data.push(lines[i][r as usize].clone());
            }
        }
        Res::A(Arc::new(Array::new(arr.rows, keep.len() as u32, data)))
    } else {
        for &i in &keep {
            data.extend(lines[i].iter().cloned());
        }
        Res::A(Arc::new(Array::new(keep.len() as u32, arr.cols, data)))
    }
}

fn transpose(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    if args.len() != 1 {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    if let Some(e) = src.as_error() {
        return Res::err(e);
    }
    let a = ctx.to_array(&src);
    let mut data = Vec::with_capacity(a.data.len());
    for c in 0..a.cols {
        for r in 0..a.rows {
            data.push(a.get(r, c).clone());
        }
    }
    Res::A(Arc::new(Array::new(a.cols, a.rows, data)))
}

fn stack(ctx: &mut Ctx<'_>, args: &[Expr], vertical: bool) -> R {
    if args.is_empty() {
        return bad_args();
    }
    let arrays: Vec<Arc<Array>> = args
        .iter()
        .map(|a| {
            let r = ctx.eval(a);
            ctx.to_array(&r)
        })
        .collect();
    if vertical {
        let cols = arrays.iter().map(|a| a.cols).max().unwrap_or(0);
        let rows = arrays.iter().map(|a| a.rows).sum();
        let mut data = Vec::new();
        for a in &arrays {
            for r in 0..a.rows {
                for c in 0..cols {
                    data.push(if c < a.cols { a.get(r, c).clone() } else { Value::Error(ErrorKind::NA) });
                }
            }
        }
        Res::A(Arc::new(Array::new(rows, cols, data)))
    } else {
        let rows = arrays.iter().map(|a| a.rows).max().unwrap_or(0);
        let cols = arrays.iter().map(|a| a.cols).sum();
        let mut data = Vec::new();
        for r in 0..rows {
            for a in &arrays {
                for c in 0..a.cols {
                    data.push(if r < a.rows { a.get(r, c).clone() } else { Value::Error(ErrorKind::NA) });
                }
            }
        }
        Res::A(Arc::new(Array::new(rows, cols, data)))
    }
}

fn vstack(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    stack(ctx, args, true)
}

fn hstack(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    stack(ctx, args, false)
}

fn slice(a: &Array, rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> Res {
    if rows.is_empty() || cols.is_empty() {
        return Res::err(ErrorKind::Calc);
    }
    let mut data = Vec::new();
    for r in rows.clone() {
        for c in cols.clone() {
            data.push(a.get(r, c).clone());
        }
    }
    Res::A(Arc::new(Array::new(rows.len() as u32, cols.len() as u32, data)))
}

fn take_drop(ctx: &mut Ctx<'_>, args: &[Expr], take: bool) -> R {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    let a = ctx.to_array(&src);
    let rn = match opt(args, 1) {
        Some(e) => Some(num!(ctx, e).trunc() as i64),
        None => None,
    };
    let cn = match opt(args, 2) {
        Some(e) => Some(num!(ctx, e).trunc() as i64),
        None => None,
    };
    let range = |n: Option<i64>, len: u32| -> std::ops::Range<u32> {
        let len_i = len as i64;
        match n {
            None => 0..len,
            Some(n) if take => {
                if n >= 0 { 0..(n.min(len_i) as u32) } else { ((len_i + n).max(0) as u32)..len }
            }
            Some(n) => {
                if n >= 0 { (n.min(len_i) as u32)..len } else { 0..((len_i + n).max(0) as u32) }
            }
        }
    };
    slice(&a, range(rn, a.rows), range(cn, a.cols))
}

fn take(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    take_drop(ctx, args, true)
}

fn drop(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    take_drop(ctx, args, false)
}

fn choose_lines(ctx: &mut Ctx<'_>, args: &[Expr], cols: bool) -> R {
    if args.len() < 2 {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    let a = ctx.to_array(&src);
    let n = if cols { a.cols } else { a.rows } as i64;
    let mut picks = Vec::new();
    for e in &args[1..] {
        let r = ctx.eval(e);
        let mut err = None;
        ctx.each(&r, |v| match v.coerce_number() {
            Ok(i) => {
                let i = i.trunc() as i64;
                let idx = if i > 0 { i - 1 } else { n + i };
                if i == 0 || idx < 0 || idx >= n {
                    err = Some(ErrorKind::Value);
                } else {
                    picks.push(idx as u32);
                }
            }
            Err(e) => err = Some(e),
        });
        if let Some(e) = err {
            return Res::err(e);
        }
    }
    let mut data = Vec::new();
    if cols {
        for r in 0..a.rows {
            for &c in &picks {
                data.push(a.get(r, c).clone());
            }
        }
        Res::A(Arc::new(Array::new(a.rows, picks.len() as u32, data)))
    } else {
        for &r in &picks {
            for c in 0..a.cols {
                data.push(a.get(r, c).clone());
            }
        }
        Res::A(Arc::new(Array::new(picks.len() as u32, a.cols, data)))
    }
}

fn choosecols(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    choose_lines(ctx, args, true)
}

fn chooserows(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    choose_lines(ctx, args, false)
}

fn to_line(ctx: &mut Ctx<'_>, args: &[Expr], col: bool) -> R {
    if !arity(args, 1, 3) {
        return bad_args();
    }
    let src = ctx.eval(&args[0]);
    let a = ctx.to_array(&src);
    let ignore = match opt(args, 1) {
        Some(e) => num!(ctx, e) as i64,
        None => 0,
    };
    let by_column = match opt(args, 2) {
        Some(e) => boolean!(ctx, e),
        None => false,
    };
    let mut out = Vec::new();
    let mut push = |v: &Value| {
        let skip = match ignore {
            1 => v.is_empty(),
            2 => v.is_error(),
            3 => v.is_empty() || v.is_error(),
            _ => false,
        };
        if !skip {
            out.push(v.clone());
        }
    };
    if by_column {
        for c in 0..a.cols {
            for r in 0..a.rows {
                push(a.get(r, c));
            }
        }
    } else {
        for r in 0..a.rows {
            for c in 0..a.cols {
                push(a.get(r, c));
            }
        }
    }
    if out.is_empty() {
        return Res::err(ErrorKind::Calc);
    }
    Res::A(Arc::new(if col { Array::column(out) } else { Array::row(out) }))
}

fn tocol(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    to_line(ctx, args, true)
}

fn torow(ctx: &mut Ctx<'_>, args: &[Expr]) -> R {
    to_line(ctx, args, false)
}
