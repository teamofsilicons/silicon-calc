//! Statistics: counts and averages, order statistics, percentiles and ranks, spread and shape, two-variable
//! statistics and regression, frequencies, and SUBTOTAL / AGGREGATE.

use std::{cmp::Ordering, sync::Arc};

use ahash::{HashMap, HashMapExt as _};

use super::*;

pub(super) const FUNCTIONS: &[(&str, Func)] = &[
    ("COUNT", count),
    ("COUNTA", counta),
    ("COUNTBLANK", countblank),
    ("AVERAGE", average),
    ("AVERAGEA", averagea),
    ("MIN", min),
    ("MAX", max),
    ("MINA", mina),
    ("MAXA", maxa),
    ("MEDIAN", median),
    ("MODE", mode_sngl),
    ("MODE.SNGL", mode_sngl),
    ("MODE.MULT", mode_mult),
    ("LARGE", large),
    ("SMALL", small),
    ("RANK", rank_eq),
    ("RANK.EQ", rank_eq),
    ("RANK.AVG", rank_avg),
    ("PERCENTILE", percentile_inc),
    ("PERCENTILE.INC", percentile_inc),
    ("PERCENTILE.EXC", percentile_exc),
    ("QUARTILE", quartile_inc),
    ("QUARTILE.INC", quartile_inc),
    ("QUARTILE.EXC", quartile_exc),
    ("PERCENTRANK", percentrank_inc),
    ("PERCENTRANK.INC", percentrank_inc),
    ("PERCENTRANK.EXC", percentrank_exc),
    ("STDEV", stdev_s),
    ("STDEV.S", stdev_s),
    ("STDEV.P", stdev_p),
    ("STDEVP", stdev_p),
    ("STDEVA", stdeva),
    ("STDEVPA", stdevpa),
    ("VAR", var_s),
    ("VAR.S", var_s),
    ("VAR.P", var_p),
    ("VARP", var_p),
    ("VARA", vara),
    ("VARPA", varpa),
    ("AVEDEV", avedev),
    ("DEVSQ", devsq),
    ("KURT", kurt),
    ("SKEW", skew),
    ("SKEW.P", skew_p),
    ("GEOMEAN", geomean),
    ("HARMEAN", harmean),
    ("TRIMMEAN", trimmean),
    ("STANDARDIZE", standardize),
    ("CORREL", correl),
    ("PEARSON", correl),
    ("RSQ", rsq),
    ("COVAR", covariance_p),
    ("COVARIANCE.P", covariance_p),
    ("COVARIANCE.S", covariance_s),
    ("SLOPE", slope),
    ("INTERCEPT", intercept),
    ("STEYX", steyx),
    ("FORECAST", forecast),
    ("FORECAST.LINEAR", forecast),
    ("TREND", trend),
    ("GROWTH", growth),
    ("LINEST", linest),
    ("LOGEST", logest),
    ("FREQUENCY", frequency),
    ("PROB", prob),
    ("SUBTOTAL", subtotal),
    ("AGGREGATE", aggregate),
];

// ---- counting and averages -------------------------------------------------------------------------------

fn count(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let mut total = 0u64;
    for a in args {
        if matches!(a, Expr::Missing) {
            total += 1;
            continue;
        }
        let r = ctx.eval(a);
        match &r {
            // Typed in: numbers, logicals and text that reads as a number count.
            Res::V(v) => {
                if !v.is_error() && v.coerce_number().is_ok() {
                    total += 1;
                }
            }
            other => {
                ctx.each_kind(other, |k| {
                    if matches!(k, Kind::Number(_)) {
                        total += 1;
                    }
                });
            }
        }
    }
    Res::num(total as f64)
}

fn counta(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let mut total = 0u64;
    for a in args {
        if matches!(a, Expr::Missing) {
            total += 1;
            continue;
        }
        let r = ctx.eval(a);
        match &r {
            Res::V(v) => {
                if !v.is_empty() {
                    total += 1;
                }
            }
            other => {
                ctx.each_kind(other, |k| {
                    if k != Kind::Empty {
                        total += 1;
                    }
                });
            }
        }
    }
    Res::num(total as f64)
}

fn countblank(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 1 {
        return bad_args();
    }
    let r = ctx.eval(&args[0]);
    if let Res::V(Value::Error(e)) = r {
        return Res::err(e);
    }
    let mut total = 0u64;
    let unvisited = ctx.each(&r, |v| {
        if v.is_empty() || matches!(v, Value::Text(s) if s.is_empty()) {
            total += 1;
        }
    });
    Res::num((total + unvisited) as f64)
}

fn mean_of(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg) -> Res {
    let (mut s, mut n) = (Sum::default(), 0u64);
    tri!(for_each_number(ctx, args, mode, &mut |x| {
        s.add(x);
        n += 1;
    }));
    if n == 0 { Res::err(ErrorKind::Div0) } else { Res::num(s.total() / n as f64) }
}

fn average(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    mean_of(ctx, args, Agg::Numbers)
}

fn averagea(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    mean_of(ctx, args, Agg::All)
}

fn extreme(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg, largest: bool) -> Res {
    if args.is_empty() {
        return bad_args();
    }
    let mut best: Option<f64> = None;
    tri!(for_each_number(ctx, args, mode, &mut |x| {
        best = Some(match best {
            None => x,
            Some(b) if largest => b.max(x),
            Some(b) => b.min(x),
        })
    }));
    Res::num(best.unwrap_or(0.0))
}

fn min(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    extreme(ctx, args, Agg::Numbers, false)
}

fn max(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    extreme(ctx, args, Agg::Numbers, true)
}

fn mina(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    extreme(ctx, args, Agg::All, false)
}

fn maxa(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    extreme(ctx, args, Agg::All, true)
}

// ---- order statistics ------------------------------------------------------------------------------------

fn sort_numbers(v: &mut [f64]) {
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
}

fn median_of(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return Value::Error(ErrorKind::Num);
    }
    let n = v.len();
    let mid = n / 2;
    let (_, &mut hi, _) = v.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    if n % 2 == 1 {
        return num_result(hi);
    }
    let lo = v[..mid].iter().copied().fold(f64::NEG_INFINITY, f64::max);
    num_result(lo / 2.0 + hi / 2.0)
}

fn median(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    Res::V(median_of(v))
}

/// The values that occur most often (at least twice), in the order they first appear.
fn modes(v: &[f64]) -> Vec<f64> {
    let mut counts: HashMap<u64, (usize, usize)> = HashMap::new();
    for (i, x) in v.iter().enumerate() {
        let key = if *x == 0.0 { 0 } else { x.to_bits() };
        let e = counts.entry(key).or_insert((0, i));
        e.0 += 1;
    }
    let best = counts.values().map(|c| c.0).max().unwrap_or(0);
    if best < 2 {
        return Vec::new();
    }
    let mut firsts: Vec<usize> = counts.values().filter(|c| c.0 == best).map(|c| c.1).collect();
    firsts.sort_unstable();
    firsts.into_iter().map(|i| v[i]).collect()
}

fn mode_sngl(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    match modes(&v).first() {
        Some(&m) => Res::num(m),
        None => Res::err(ErrorKind::NA),
    }
}

fn mode_mult(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    let m = modes(&v);
    if m.is_empty() {
        return Res::err(ErrorKind::NA);
    }
    Res::A(Arc::new(Array::column(m.into_iter().map(Value::Number).collect())))
}

fn kth(ctx: &mut Ctx<'_>, args: &[Expr], largest: bool) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let mut v = tri!(data_numbers(ctx, &args[0]));
    sort_numbers(&mut v);
    let kr = ctx.eval(&args[1]);
    lift_res(ctx, vec![kr], |[k]| {
        let k = as_num(k)?.ceil();
        if k < 1.0 || k > v.len() as f64 {
            return Err(ErrorKind::Num);
        }
        let k = k as usize;
        Ok(Value::Number(if largest { v[v.len() - k] } else { v[k - 1] }))
    })
}

fn large(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    kth(ctx, args, true)
}

fn small(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    kth(ctx, args, false)
}

fn rank_fn(ctx: &mut Ctx<'_>, args: &[Expr], avg: bool) -> Res {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let xr = ctx.eval(&args[0]);
    let v = tri!(data_numbers(ctx, &args[1]));
    let ascending = tri!(legacy_num(ctx, args, 2, 0.0)) != 0.0;
    lift_res(ctx, vec![xr], |[x]| {
        let x = as_num(x)?;
        let (mut better, mut same) = (0usize, 0usize);
        for &y in &v {
            if y == x {
                same += 1;
            } else if (ascending && y < x) || (!ascending && y > x) {
                better += 1;
            }
        }
        if same == 0 {
            return Err(ErrorKind::NA);
        }
        Ok(Value::Number(if avg { better as f64 + (same as f64 + 1.0) / 2.0 } else { (better + 1) as f64 }))
    })
}

fn rank_eq(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    rank_fn(ctx, args, false)
}

fn rank_avg(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    rank_fn(ctx, args, true)
}

/// PERCENTILE.INC on sorted data.
pub(super) fn percentile_inc_of(v: &[f64], k: f64) -> Result<f64, ErrorKind> {
    if v.is_empty() || !(0.0..=1.0).contains(&k) {
        return Err(ErrorKind::Num);
    }
    let pos = k * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let frac = pos - lo as f64;
    Ok(if lo + 1 < v.len() { v[lo] + frac * (v[lo + 1] - v[lo]) } else { v[lo] })
}

/// PERCENTILE.EXC on sorted data.
pub(super) fn percentile_exc_of(v: &[f64], k: f64) -> Result<f64, ErrorKind> {
    let n = v.len() as f64;
    if v.is_empty() || k <= 0.0 || k >= 1.0 {
        return Err(ErrorKind::Num);
    }
    let pos = k * (n + 1.0);
    if pos < 1.0 || pos > n {
        return Err(ErrorKind::Num);
    }
    let lo = pos.floor() as usize;
    let frac = pos - lo as f64;
    Ok(if lo < v.len() { v[lo - 1] + frac * (v[lo] - v[lo - 1]) } else { v[lo - 1] })
}

fn percentile(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(&[f64], f64) -> Result<f64, ErrorKind>, scale: f64, quartile: bool) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let mut v = tri!(data_numbers(ctx, &args[0]));
    sort_numbers(&mut v);
    let kr = ctx.eval(&args[1]);
    lift_res(ctx, vec![kr], |[k]| {
        let mut k = as_num(k)?;
        if quartile {
            k = k.trunc();
        }
        Ok(num_result(f(&v, k / scale)?))
    })
}

fn percentile_inc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    percentile(ctx, args, percentile_inc_of, 1.0, false)
}

fn percentile_exc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    percentile(ctx, args, percentile_exc_of, 1.0, false)
}

fn quartile_inc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() == 2 {
        // Quarters outside 0..=4 are #NUM!.
        let q = num!(ctx, &args[1]).trunc();
        if !(0.0..=4.0).contains(&q) {
            return Res::err(ErrorKind::Num);
        }
    }
    percentile(ctx, args, percentile_inc_of, 4.0, true)
}

fn quartile_exc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() == 2 {
        let q = num!(ctx, &args[1]).trunc();
        if !(1.0..=3.0).contains(&q) {
            return Res::err(ErrorKind::Num);
        }
    }
    percentile(ctx, args, percentile_exc_of, 4.0, true)
}

fn percentrank(ctx: &mut Ctx<'_>, args: &[Expr], exclusive: bool) -> Res {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let mut v = tri!(data_numbers(ctx, &args[0]));
    sort_numbers(&mut v);
    let x = num!(ctx, &args[1]);
    let sig = tri!(opt_num(ctx, args, 2, 3.0)).trunc();
    if sig < 1.0 {
        return Res::err(ErrorKind::Num);
    }
    let n = v.len();
    if n == 0 || x < v[0] || x > v[n - 1] {
        return Res::err(ErrorKind::NA);
    }
    let rank_of = |value: f64| -> f64 {
        let smaller = v.partition_point(|&y| y < value) as f64;
        if exclusive { (smaller + 1.0) / (n as f64 + 1.0) } else if n == 1 { 1.0 } else { smaller / (n as f64 - 1.0) }
    };
    let r = if v.binary_search_by(|y| y.partial_cmp(&x).unwrap_or(Ordering::Equal)).is_ok() {
        rank_of(x)
    } else {
        let hi_i = v.partition_point(|&y| y < x);
        let (x0, x1) = (v[hi_i - 1], v[hi_i]);
        let (r0, r1) = (rank_of(x0), rank_of(x1));
        r0 + (x - x0) / (x1 - x0) * (r1 - r0)
    };
    // Excel cuts (does not round) to the significant digits asked for.
    let f = 10f64.powi(sig.min(15.0) as i32);
    Res::num((clean15(r * f)).floor() / f)
}

fn percentrank_inc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    percentrank(ctx, args, false)
}

fn percentrank_exc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    percentrank(ctx, args, true)
}

// ---- spread and shape ------------------------------------------------------------------------------------

fn mean(v: &[f64]) -> f64 {
    kahan(v) / v.len() as f64
}

/// The sum of squared deviations from the mean.
fn sum_sq_dev(v: &[f64]) -> f64 {
    let m = mean(v);
    let mut s = Sum::default();
    for x in v {
        s.add((x - m) * (x - m));
    }
    s.total()
}

pub(super) fn variance_of(v: &[f64], sample: bool) -> Result<f64, ErrorKind> {
    let n = v.len();
    if n == 0 || (sample && n < 2) {
        return Err(ErrorKind::Div0);
    }
    Ok(sum_sq_dev(v) / if sample { (n - 1) as f64 } else { n as f64 })
}

fn spread(ctx: &mut Ctx<'_>, args: &[Expr], mode: Agg, sample: bool, root: bool) -> Res {
    let v = tri!(numbers(ctx, args, mode));
    let var = tri!(variance_of(&v, sample));
    Res::num(if root { var.sqrt() } else { var })
}

fn stdev_s(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::Numbers, true, true)
}

fn stdev_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::Numbers, false, true)
}

fn stdeva(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::All, true, true)
}

fn stdevpa(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::All, false, true)
}

fn var_s(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::Numbers, true, false)
}

fn var_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::Numbers, false, false)
}

fn vara(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::All, true, false)
}

fn varpa(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    spread(ctx, args, Agg::All, false, false)
}

fn avedev(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    if v.is_empty() {
        return Res::err(ErrorKind::Num);
    }
    let m = mean(&v);
    let mut s = Sum::default();
    for x in &v {
        s.add((x - m).abs());
    }
    Res::num(s.total() / v.len() as f64)
}

fn devsq(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    if v.is_empty() {
        return Res::err(ErrorKind::Num);
    }
    Res::num(sum_sq_dev(&v))
}

fn kurt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    let n = v.len() as f64;
    if v.len() < 4 {
        return Res::err(ErrorKind::Div0);
    }
    let m = mean(&v);
    let s = (sum_sq_dev(&v) / (n - 1.0)).sqrt();
    if s == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    let mut q = Sum::default();
    for x in &v {
        q.add(((x - m) / s).powi(4));
    }
    let r = n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * q.total() - 3.0 * (n - 1.0).powi(2) / ((n - 2.0) * (n - 3.0));
    Res::num(r)
}

fn skew(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    let n = v.len() as f64;
    if v.len() < 3 {
        return Res::err(ErrorKind::Div0);
    }
    let m = mean(&v);
    let s = (sum_sq_dev(&v) / (n - 1.0)).sqrt();
    if s == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    let mut c = Sum::default();
    for x in &v {
        c.add(((x - m) / s).powi(3));
    }
    Res::num(n / ((n - 1.0) * (n - 2.0)) * c.total())
}

fn skew_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    let n = v.len() as f64;
    if v.is_empty() {
        return Res::err(ErrorKind::Div0);
    }
    let m = mean(&v);
    let s = (sum_sq_dev(&v) / n).sqrt();
    if s == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    let mut c = Sum::default();
    for x in &v {
        c.add(((x - m) / s).powi(3));
    }
    Res::num(c.total() / n)
}

fn geomean(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    if v.is_empty() || v.iter().any(|&x| x <= 0.0) {
        return Res::err(ErrorKind::Num);
    }
    let mut s = Sum::default();
    for x in &v {
        s.add(x.ln());
    }
    Res::num(clean15((s.total() / v.len() as f64).exp()))
}

fn harmean(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    if v.is_empty() || v.iter().any(|&x| x <= 0.0) {
        return Res::err(ErrorKind::Num);
    }
    let mut s = Sum::default();
    for x in &v {
        s.add(1.0 / x);
    }
    Res::num(v.len() as f64 / s.total())
}

fn trimmean(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let mut v = tri!(data_numbers(ctx, &args[0]));
    let p = num!(ctx, &args[1]);
    if !(0.0..1.0).contains(&p) || v.is_empty() {
        return Res::err(ErrorKind::Num);
    }
    sort_numbers(&mut v);
    // The number left out is rounded down to an even count, half from each end.
    let k = ((v.len() as f64 * p) / 2.0).floor() as usize;
    let kept = &v[k..v.len() - k];
    Res::num(mean(kept))
}

fn standardize(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[x, m, sd]| if sd <= 0.0 { Err(ErrorKind::Num) } else { Ok((x - m) / sd) })
}

// ---- two variables ---------------------------------------------------------------------------------------

/// Pairs (y, x) from two equally long arrays, skipping pairs where either side isn't a number.
fn xy_pairs(ctx: &mut Ctx<'_>, ys: &Expr, xs: &Expr) -> Result<(Vec<f64>, Vec<f64>), ErrorKind> {
    let (ry, rx) = (ctx.eval(ys), ctx.eval(xs));
    for r in [&ry, &rx] {
        if let Some(e) = r.as_error() {
            return Err(e);
        }
    }
    let (a, b) = (ctx.to_array(&ry), ctx.to_array(&rx));
    if a.data.len() != b.data.len() {
        return Err(ErrorKind::NA);
    }
    let (mut y, mut x) = (Vec::with_capacity(a.data.len()), Vec::with_capacity(a.data.len()));
    for (p, q) in a.data.iter().zip(&b.data) {
        match (p, q) {
            (Value::Error(e), _) | (_, Value::Error(e)) => return Err(*e),
            (Value::Number(p), Value::Number(q)) => {
                y.push(*p);
                x.push(*q);
            }
            _ => {}
        }
    }
    Ok((y, x))
}

/// Sums of squares and cross products around the means: (Sxx, Syy, Sxy, mean x, mean y).
fn moments(y: &[f64], x: &[f64]) -> (f64, f64, f64, f64, f64) {
    let (mx, my) = (mean(x), mean(y));
    let (mut sxx, mut syy, mut sxy) = (Sum::default(), Sum::default(), Sum::default());
    for (yi, xi) in y.iter().zip(x) {
        sxx.add((xi - mx) * (xi - mx));
        syy.add((yi - my) * (yi - my));
        sxy.add((xi - mx) * (yi - my));
    }
    (sxx.total(), syy.total(), sxy.total(), mx, my)
}

fn correl(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (y, x) = tri!(xy_pairs(ctx, &args[0], &args[1]));
    if y.is_empty() {
        return Res::err(ErrorKind::Div0);
    }
    let (sxx, syy, sxy, _, _) = moments(&y, &x);
    if sxx == 0.0 || syy == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    Res::num(sxy / (sxx * syy).sqrt())
}

fn rsq(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    match correl(ctx, args) {
        Res::V(Value::Number(r)) => Res::num(r * r),
        other => other,
    }
}

fn covariance(ctx: &mut Ctx<'_>, args: &[Expr], sample: bool) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (y, x) = tri!(xy_pairs(ctx, &args[0], &args[1]));
    let n = y.len();
    if n == 0 || (sample && n < 2) {
        return Res::err(ErrorKind::Div0);
    }
    let (_, _, sxy, _, _) = moments(&y, &x);
    Res::num(sxy / if sample { (n - 1) as f64 } else { n as f64 })
}

fn covariance_p(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    covariance(ctx, args, false)
}

fn covariance_s(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    covariance(ctx, args, true)
}

/// The least squares line through (x, y): (slope, intercept).
fn line(ctx: &mut Ctx<'_>, ys: &Expr, xs: &Expr) -> Result<(f64, f64), ErrorKind> {
    let (y, x) = xy_pairs(ctx, ys, xs)?;
    if y.is_empty() {
        return Err(ErrorKind::Div0);
    }
    let (sxx, _, sxy, mx, my) = moments(&y, &x);
    if sxx == 0.0 {
        return Err(ErrorKind::Div0);
    }
    let b = sxy / sxx;
    Ok((b, my - b * mx))
}

fn slope(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    Res::num(tri!(line(ctx, &args[0], &args[1])).0)
}

fn intercept(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    Res::num(tri!(line(ctx, &args[0], &args[1])).1)
}

fn steyx(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (y, x) = tri!(xy_pairs(ctx, &args[0], &args[1]));
    let n = y.len() as f64;
    if y.len() < 3 {
        return Res::err(ErrorKind::Div0);
    }
    let (sxx, syy, sxy, _, _) = moments(&y, &x);
    if sxx == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    Res::num(((syy - sxy * sxy / sxx) / (n - 2.0)).max(0.0).sqrt())
}

fn forecast(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 3 {
        return bad_args();
    }
    let xr = ctx.eval(&args[0]);
    let (b, a) = tri!(line(ctx, &args[1], &args[2]));
    lift_res(ctx, vec![xr], |[x]| Ok(num_result(a + b * as_num(x)?)))
}

// ---- regression (LINEST and friends) ---------------------------------------------------------------------

/// Observations of a regression: y values and the rows of x values (one per observation).
struct Observations {
    y: Vec<f64>,
    x: Vec<Vec<f64>>,
    /// The number of x variables.
    k: usize,
    /// Observations run along a row (each variable is a row of known_x).
    by_row: bool,
}

fn numeric(a: &Array) -> Result<Vec<f64>, ErrorKind> {
    a.data
        .iter()
        .map(|v| match v {
            Value::Number(n) => Ok(*n),
            Value::Error(e) => Err(*e),
            _ => Err(ErrorKind::Value),
        })
        .collect()
}

fn observations(ctx: &mut Ctx<'_>, args: &[Expr]) -> Result<Observations, ErrorKind> {
    let yr = ctx.eval(args.first().ok_or(ErrorKind::Value)?);
    if let Some(e) = yr.as_error() {
        return Err(e);
    }
    let ya = ctx.to_array(&yr);
    let y = numeric(&ya)?;
    let n = y.len();
    let xa = match opt(args, 1) {
        Some(e) => {
            let r = ctx.eval(e);
            if let Some(err) = r.as_error() {
                return Err(err);
            }
            ctx.to_array(&r)
        }
        None => Arc::new(Array::new(ya.rows, ya.cols, (1..=n).map(|i| Value::Number(i as f64)).collect())),
    };
    let xv = numeric(&xa)?;
    let (yr_, yc) = (ya.rows as usize, ya.cols as usize);
    let (xr, xc) = (xa.rows as usize, xa.cols as usize);
    // One variable laid out like y; or several, one per column (y a column) or per row (y a row).
    if xr == yr_ && xc == yc {
        return Ok(Observations { y, x: xv.into_iter().map(|v| vec![v]).collect(), k: 1, by_row: yr_ == 1 && yc > 1 });
    }
    if yc == 1 && xr == n {
        let x = (0..n).map(|i| xv[i * xc..(i + 1) * xc].to_vec()).collect();
        return Ok(Observations { y, x, k: xc, by_row: false });
    }
    if yr_ == 1 && xc == n {
        let x = (0..n).map(|i| (0..xr).map(|j| xv[j * xc + i]).collect()).collect();
        return Ok(Observations { y, x, k: xr, by_row: true });
    }
    Err(ErrorKind::Ref)
}

/// A least squares fit.
struct Fit {
    /// Coefficients for each x variable (0 for variables dropped as collinear), then the constant.
    coef: Vec<f64>,
    b: f64,
    se: Vec<f64>,
    se_b: f64,
    r2: f64,
    se_y: f64,
    f: f64,
    df: f64,
    ss_reg: f64,
    ss_resid: f64,
}

/// Fits y = x·m + b by Householder QR (columns that add nothing are dropped, as Excel does).
fn least_squares(y: &[f64], x: &[Vec<f64>], k: usize, constant: bool) -> Result<Fit, ErrorKind> {
    let n = y.len();
    let p = k + constant as usize;
    if n == 0 {
        return Err(ErrorKind::Value);
    }
    // Columns: the k variables, then the constant.
    let mut cols: Vec<Vec<f64>> = (0..k).map(|j| x.iter().map(|row| row[j]).collect()).collect();
    if constant {
        cols.push(vec![1.0; n]);
    }
    let mut keep: Vec<usize> = Vec::new();
    // Gram–Schmidt with re-orthogonalisation on the kept columns; detects collinear columns.
    let mut q: Vec<Vec<f64>> = Vec::new();
    let mut r = vec![vec![0.0; p]; p];
    let norm = |v: &[f64]| v.iter().map(|a| a * a).sum::<f64>().sqrt();
    for (j, col) in cols.iter().enumerate() {
        let mut v = col.clone();
        let original = norm(&v);
        for _ in 0..2 {
            for (qi, qv) in q.iter().enumerate() {
                let dot: f64 = qv.iter().zip(&v).map(|(a, b)| a * b).sum();
                r[qi][j] += dot;
                for (vi, qa) in v.iter_mut().zip(qv) {
                    *vi -= dot * qa;
                }
            }
        }
        let rest = norm(&v);
        if original == 0.0 || rest <= 1e-10 * original.max(1.0) {
            // Collinear with what came before: dropped. Its r column entries are unused.
            for row in r.iter_mut() {
                row[j] = 0.0;
            }
            continue;
        }
        for vi in v.iter_mut() {
            *vi /= rest;
        }
        r[q.len()][j] = rest;
        q.push(v);
        keep.push(j);
    }
    let m = keep.len();
    if m == 0 {
        return Err(ErrorKind::Num);
    }
    // R restricted to the kept columns (m × m upper triangular), and Qᵀy.
    let rk: Vec<Vec<f64>> = (0..m).map(|i| keep.iter().map(|&j| r[i][j]).collect()).collect();
    let qty: Vec<f64> = q.iter().map(|qv| qv.iter().zip(y).map(|(a, b)| a * b).sum()).collect();
    let mut beta = vec![0.0; m];
    for i in (0..m).rev() {
        let mut s = qty[i];
        for j in i + 1..m {
            s -= rk[i][j] * beta[j];
        }
        beta[i] = s / rk[i][i];
    }
    // R⁻¹ for the standard errors.
    let mut rinv = vec![vec![0.0; m]; m];
    for i in 0..m {
        rinv[i][i] = 1.0 / rk[i][i];
        for j in (0..i).rev() {
            let mut s = 0.0;
            for l in j + 1..=i {
                s += rk[j][l] * rinv[l][i];
            }
            rinv[j][i] = -s / rk[j][j];
        }
    }
    let mut full = vec![0.0; p];
    let mut var_diag = vec![0.0; p];
    for (idx, &j) in keep.iter().enumerate() {
        full[j] = beta[idx];
        var_diag[j] = (0..m).map(|l| rinv[idx][l] * rinv[idx][l]).sum();
    }
    let fitted: Vec<f64> = (0..n).map(|i| (0..p).map(|j| cols[j][i] * full[j]).sum()).collect();
    let mut ss_resid = Sum::default();
    for (yi, fi) in y.iter().zip(&fitted) {
        ss_resid.add((yi - fi) * (yi - fi));
    }
    let ss_resid = ss_resid.total();
    let ss_total = if constant {
        sum_sq_dev(y)
    } else {
        let mut s = Sum::default();
        for v in y {
            s.add(v * v);
        }
        s.total()
    };
    let ss_reg = ss_total - ss_resid;
    let df = n as f64 - m as f64;
    let se_y = if df > 0.0 { (ss_resid / df).sqrt() } else { 0.0 };
    let r2 = if ss_total == 0.0 { 1.0 } else { ss_reg / ss_total };
    let regressors = (m - (constant && keep.contains(&k)) as usize) as f64;
    let f = if df > 0.0 && ss_resid > 0.0 && regressors > 0.0 { (ss_reg / regressors) / (ss_resid / df) } else { f64::INFINITY };
    let se: Vec<f64> = (0..k).map(|j| se_y * var_diag[j].sqrt()).collect();
    let (b, se_b) = if constant { (full[k], se_y * var_diag[k].sqrt()) } else { (0.0, 0.0) };
    Ok(Fit { coef: full[..k].to_vec(), b, se, se_b, r2, se_y, f, df, ss_reg, ss_resid })
}

fn fit_args(ctx: &mut Ctx<'_>, args: &[Expr], log: bool) -> Result<(Observations, Fit, bool), ErrorKind> {
    let mut obs = observations(ctx, args)?;
    let constant = opt_bool(ctx, args, 2, true)?;
    let stats = opt_bool(ctx, args, 3, false)?;
    if log {
        for v in obs.y.iter_mut() {
            if *v <= 0.0 {
                return Err(ErrorKind::Num);
            }
            *v = v.ln();
        }
    }
    let fit = least_squares(&obs.y, &obs.x, obs.k, constant)?;
    Ok((obs, fit, stats))
}

fn regression_table(fit: &Fit, k: usize, stats: bool, log: bool) -> Res {
    let tr = |v: f64| if log { v.exp() } else { v };
    let width = k + 1;
    let mut rows: Vec<Vec<Value>> = Vec::new();
    // Coefficients come in reverse order of the x variables, then the constant.
    let mut first: Vec<Value> = fit.coef.iter().rev().map(|&c| num_result(tr(c))).collect();
    first.push(num_result(tr(fit.b)));
    rows.push(first);
    if stats {
        let na = Value::Error(ErrorKind::NA);
        let mut se: Vec<Value> = fit.se.iter().rev().map(|&s| num_result(s)).collect();
        se.push(if fit.se_b == 0.0 && fit.b == 0.0 { na.clone() } else { num_result(fit.se_b) });
        rows.push(se);
        let mut row3 = vec![num_result(fit.r2), num_result(fit.se_y)];
        let mut row4 = vec![if fit.f.is_finite() { Value::Number(fit.f) } else { Value::Error(ErrorKind::Num) }, num_result(fit.df)];
        let mut row5 = vec![num_result(fit.ss_reg), num_result(fit.ss_resid)];
        for r in [&mut row3, &mut row4, &mut row5] {
            r.resize(width, na.clone());
        }
        rows.push(row3);
        rows.push(row4);
        rows.push(row5);
    }
    // A one-column result (no x variables would be odd) still gets the shape Excel gives.
    for r in rows.iter_mut() {
        r.truncate(width.max(2));
    }
    grid_array(rows, Value::Error(ErrorKind::NA))
}

fn linest(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !arity(args, 1, 4) {
        return bad_args();
    }
    let (obs, fit, stats) = tri!(fit_args(ctx, args, false));
    regression_table(&fit, obs.k, stats, false)
}

fn logest(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !arity(args, 1, 4) {
        return bad_args();
    }
    let (obs, fit, stats) = tri!(fit_args(ctx, args, true));
    regression_table(&fit, obs.k, stats, true)
}

fn predict(ctx: &mut Ctx<'_>, args: &[Expr], log: bool) -> Res {
    if !arity(args, 1, 4) {
        return bad_args();
    }
    let mut fargs: Vec<Expr> = args.iter().take(2).cloned().collect();
    fargs.resize(2, Expr::Missing);
    if let Some(c) = args.get(3) {
        fargs.push(c.clone());
    }
    let (obs, fit, _) = tri!(fit_args(ctx, &fargs, log));
    let new_x = match opt(args, 2) {
        Some(e) => {
            let r = ctx.eval(e);
            if let Some(err) = r.as_error() {
                return Res::err(err);
            }
            ctx.to_array(&r)
        }
        None => {
            // The known x values, laid out as given.
            match opt(args, 1) {
                Some(e) => {
                    let r = ctx.eval(e);
                    ctx.to_array(&r)
                }
                None => {
                    let n = obs.y.len();
                    let data = (1..=n).map(|i| Value::Number(i as f64)).collect();
                    Arc::new(if obs.by_row { Array::new(1, n as u32, data) } else { Array::new(n as u32, 1, data) })
                }
            }
        }
    };
    let xs = tri!(numeric(&new_x));
    let value = |row: &[f64]| -> Value {
        let mut s = fit.b;
        for (j, x) in row.iter().enumerate() {
            s += fit.coef[j] * x;
        }
        num_result(if log { s.exp() } else { s })
    };
    if obs.k == 1 {
        let data = xs.iter().map(|&x| value(&[x])).collect();
        return Res::A(Arc::new(Array::new(new_x.rows, new_x.cols, data)));
    }
    let (r, c) = (new_x.rows as usize, new_x.cols as usize);
    if obs.by_row {
        if r != obs.k {
            return Res::err(ErrorKind::Ref);
        }
        let data = (0..c).map(|i| value(&(0..r).map(|j| xs[j * c + i]).collect::<Vec<_>>())).collect();
        Res::A(Arc::new(Array::row(data)))
    } else {
        if c != obs.k {
            return Res::err(ErrorKind::Ref);
        }
        let data = (0..r).map(|i| value(&xs[i * c..(i + 1) * c])).collect();
        Res::A(Arc::new(Array::column(data)))
    }
}

fn trend(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    predict(ctx, args, false)
}

fn growth(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    predict(ctx, args, true)
}

// ---- frequencies -----------------------------------------------------------------------------------------

fn frequency(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let data = tri!(data_numbers(ctx, &args[0]));
    let bins = tri!(data_numbers(ctx, &args[1]));
    let mut order: Vec<usize> = (0..bins.len()).collect();
    order.sort_by(|&a, &b| bins[a].partial_cmp(&bins[b]).unwrap_or(Ordering::Equal).then(a.cmp(&b)));
    let sorted: Vec<f64> = order.iter().map(|&i| bins[i]).collect();
    let mut counts = vec![0u64; bins.len() + 1];
    for x in data {
        let i = sorted.partition_point(|&b| b < x);
        if i < sorted.len() {
            counts[order[i]] += 1;
        } else {
            counts[bins.len()] += 1;
        }
    }
    Res::A(Arc::new(Array::column(counts.into_iter().map(|c| Value::Number(c as f64)).collect())))
}

fn prob(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !arity(args, 3, 4) {
        return bad_args();
    }
    let (rx, rp) = (ctx.eval(&args[0]), ctx.eval(&args[1]));
    let (xa, pa) = (ctx.to_array(&rx), ctx.to_array(&rp));
    if xa.data.len() != pa.data.len() {
        return Res::err(ErrorKind::NA);
    }
    let lo = num!(ctx, &args[2]);
    let hi = match opt(args, 3) {
        Some(e) => num!(ctx, e),
        None => lo,
    };
    let (mut total, mut hit) = (Sum::default(), Sum::default());
    for (x, p) in xa.data.iter().zip(&pa.data) {
        let (x, p) = match (x, p) {
            (Value::Error(e), _) | (_, Value::Error(e)) => return Res::err(*e),
            (Value::Number(x), Value::Number(p)) => (*x, *p),
            _ => continue,
        };
        if !(0.0..=1.0).contains(&p) {
            return Res::err(ErrorKind::Num);
        }
        total.add(p);
        if x >= lo && x <= hi {
            hit.add(p);
        }
    }
    if (total.total() - 1.0).abs() > 1e-9 {
        return Res::err(ErrorKind::Num);
    }
    Res::num(hit.total())
}

// ---- SUBTOTAL and AGGREGATE ------------------------------------------------------------------------------

/// What SUBTOTAL and AGGREGATE collect from their references.
#[derive(Default)]
struct Collected {
    nums: Vec<f64>,
    /// Non-empty values of any kind (COUNTA).
    nonempty: u64,
    error: Option<ErrorKind>,
}

#[derive(Clone, Copy)]
struct Skip {
    hidden: bool,
    nested: bool,
    errors: bool,
}

fn calls_subtotal(e: &Expr) -> bool {
    let mut found = false;
    e.walk(&mut |x| {
        if let Expr::Call(n, _) = x
            && (n.as_ref() == "SUBTOTAL" || n.as_ref() == "AGGREGATE")
        {
            found = true;
        }
    });
    found
}

fn collect(ctx: &mut Ctx<'_>, args: &[Expr], skip: Skip, out: &mut Collected) {
    for a in args {
        let r = ctx.eval(a);
        let mut take = |v: &Value, out: &mut Collected| match v {
            Value::Empty => {}
            Value::Number(n) => {
                out.nums.push(*n);
                out.nonempty += 1;
            }
            Value::Error(e) => {
                if !skip.errors {
                    out.nonempty += 1;
                    out.error.get_or_insert(*e);
                }
            }
            _ => out.nonempty += 1,
        };
        match &r {
            Res::R(area) => {
                let Some(rect) = ctx.live_rect(area) else { continue };
                let hidden = if skip.hidden { ctx.grid.hidden_rows(area.tab) } else { Vec::new() };
                let grid = ctx.grid;
                let mut nested: HashMap<usize, bool> = HashMap::new();
                grid.visit(area.tab, &rect, &mut |row, col, v| {
                    if !hidden.is_empty() && hidden.binary_search(&row).is_ok() {
                        return true;
                    }
                    if skip.nested
                        && let Some(tree) = grid.formula_tree(area.tab, row, col)
                    {
                        let key = Arc::as_ptr(&tree) as usize;
                        if *nested.entry(key).or_insert_with(|| calls_subtotal(&tree)) {
                            return true;
                        }
                    }
                    take(v, out);
                    true
                });
            }
            Res::V(v) => take(v, out),
            Res::A(arr) => arr.data.iter().for_each(|v| take(v, out)),
            Res::L(_) => take(&Value::Error(ErrorKind::Calc), out),
        }
    }
}

/// The statistic with this SUBTOTAL / AGGREGATE number over what was collected.
fn statistic(code: u32, c: &mut Collected, k: Option<f64>) -> Value {
    let counts_errors = matches!(code, 2 | 3);
    if let Some(e) = c.error
        && !counts_errors
    {
        return Value::Error(e);
    }
    let v = &mut c.nums;
    let nan = |r: Result<f64, ErrorKind>| r.map(num_result).unwrap_or_else(Value::Error);
    match code {
        1 => {
            if v.is_empty() {
                Value::Error(ErrorKind::Div0)
            } else {
                num_result(mean(v))
            }
        }
        2 => Value::Number(v.len() as f64),
        3 => Value::Number(c.nonempty as f64),
        4 => Value::Number(v.iter().copied().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))).unwrap_or(0.0)),
        5 => Value::Number(v.iter().copied().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x)))).unwrap_or(0.0)),
        6 => num_result(if v.is_empty() { 0.0 } else { v.iter().product() }),
        7 => nan(variance_of(v, true).map(f64::sqrt)),
        8 => nan(variance_of(v, false).map(f64::sqrt)),
        9 => num_result(kahan(v)),
        10 => nan(variance_of(v, true)),
        11 => nan(variance_of(v, false)),
        12 => median_of(std::mem::take(v)),
        13 => match modes(v).first() {
            Some(&m) => Value::Number(m),
            None => Value::Error(ErrorKind::NA),
        },
        14..=19 => {
            let Some(k) = k else { return Value::Error(ErrorKind::Value) };
            sort_numbers(v);
            match code {
                14 | 15 => {
                    let k = k.ceil();
                    if k < 1.0 || k > v.len() as f64 {
                        return Value::Error(ErrorKind::Num);
                    }
                    let k = k as usize;
                    Value::Number(if code == 14 { v[v.len() - k] } else { v[k - 1] })
                }
                16 => nan(percentile_inc_of(v, k)),
                17 => {
                    let q = k.trunc();
                    if !(0.0..=4.0).contains(&q) {
                        return Value::Error(ErrorKind::Num);
                    }
                    nan(percentile_inc_of(v, q / 4.0))
                }
                18 => nan(percentile_exc_of(v, k)),
                _ => {
                    let q = k.trunc();
                    if !(1.0..=3.0).contains(&q) {
                        return Value::Error(ErrorKind::Num);
                    }
                    nan(percentile_exc_of(v, q / 4.0))
                }
            }
        }
        _ => Value::Error(ErrorKind::Value),
    }
}

fn subtotal(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() < 2 {
        return bad_args();
    }
    let code = num!(ctx, &args[0]).trunc();
    let (base, hidden) = match code as i64 {
        1..=11 => (code as u32, false),
        101..=111 => (code as u32 - 100, true),
        _ => return bad_args(),
    };
    let mut c = Collected::default();
    collect(ctx, &args[1..], Skip { hidden, nested: true, errors: false }, &mut c);
    Res::V(statistic(base, &mut c, None))
}

fn aggregate(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() < 3 {
        return bad_args();
    }
    let code = num!(ctx, &args[0]).trunc();
    let options = tri!(legacy_num(ctx, args, 1, 0.0)).trunc();
    if !(1.0..=19.0).contains(&code) || !(0.0..=7.0).contains(&options) {
        return bad_args();
    }
    let code = code as u32;
    let o = options as u32;
    let skip = Skip { hidden: matches!(o, 1 | 3 | 5 | 7), nested: o <= 3, errors: matches!(o, 2 | 3 | 6 | 7) };
    let mut c = Collected::default();
    if code >= 14 {
        if args.len() != 4 {
            return bad_args();
        }
        let k = num!(ctx, &args[3]);
        collect(ctx, &args[2..3], skip, &mut c);
        return Res::V(statistic(code, &mut c, Some(k)));
    }
    collect(ctx, &args[2..], skip, &mut c);
    Res::V(statistic(code, &mut c, None))
}
