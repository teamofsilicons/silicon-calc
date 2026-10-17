//! Math and trigonometry: sums and products, rounding, powers and logarithms, trigonometry, combinatorics,
//! number bases and Roman numerals, matrices, random numbers.

use std::sync::Arc;

use super::*;

pub(super) const FUNCTIONS: &[(&str, Func)] = &[
    ("SUM", sum),
    ("PRODUCT", product),
    ("SUMSQ", sumsq),
    ("SUMPRODUCT", sumproduct),
    ("SUMX2MY2", sumx2my2),
    ("SUMX2PY2", sumx2py2),
    ("SUMXMY2", sumxmy2),
    ("SERIESSUM", seriessum),
    ("ROUND", round),
    ("ROUNDUP", roundup),
    ("ROUNDDOWN", rounddown),
    ("TRUNC", trunc),
    ("INT", int),
    ("MROUND", mround),
    ("CEILING", ceiling),
    ("CEILING.MATH", ceiling_math),
    ("CEILING.PRECISE", ceiling_precise),
    ("ISO.CEILING", ceiling_precise),
    ("FLOOR", floor),
    ("FLOOR.MATH", floor_math),
    ("FLOOR.PRECISE", floor_precise),
    ("EVEN", even),
    ("ODD", odd),
    ("ABS", abs),
    ("SIGN", sign),
    ("MOD", modulo),
    ("QUOTIENT", quotient),
    ("POWER", power),
    ("SQRT", sqrt),
    ("SQRTPI", sqrtpi),
    ("EXP", exp),
    ("LN", ln),
    ("LOG", log),
    ("LOG10", log10),
    ("PI", pi),
    ("SIN", sin),
    ("COS", cos),
    ("TAN", tan),
    ("ASIN", asin),
    ("ACOS", acos),
    ("ATAN", atan),
    ("ATAN2", atan2),
    ("SINH", sinh),
    ("COSH", cosh),
    ("TANH", tanh),
    ("ASINH", asinh),
    ("ACOSH", acosh),
    ("ATANH", atanh),
    ("COT", cot),
    ("COTH", coth),
    ("CSC", csc),
    ("CSCH", csch),
    ("SEC", sec),
    ("SECH", sech),
    ("ACOT", acot),
    ("ACOTH", acoth),
    ("DEGREES", degrees),
    ("RADIANS", radians),
    ("FACT", fact),
    ("FACTDOUBLE", factdouble),
    ("COMBIN", combin),
    ("COMBINA", combina),
    ("PERMUT", permut),
    ("PERMUTATIONA", permutationa),
    ("MULTINOMIAL", multinomial),
    ("GCD", gcd),
    ("LCM", lcm),
    ("BASE", base),
    ("DECIMAL", decimal),
    ("ROMAN", roman),
    ("ARABIC", arabic),
    ("RAND", rand),
    ("RANDBETWEEN", randbetween),
    ("MMULT", mmult),
    ("MDETERM", mdeterm),
    ("MINVERSE", minverse),
    ("MUNIT", munit),
];

// ---- sums and products ---------------------------------------------------------------------------------

fn sum(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.is_empty() {
        return bad_args();
    }
    let mut s = Sum::default();
    tri!(for_each_number(ctx, args, Agg::Numbers, &mut |x| s.add(x)));
    Res::num(s.total())
}

fn product(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.is_empty() {
        return bad_args();
    }
    let (mut p, mut any) = (1.0, false);
    tri!(for_each_number(ctx, args, Agg::Numbers, &mut |x| {
        p *= x;
        any = true;
    }));
    Res::num(if any { p } else { 0.0 })
}

fn sumsq(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.is_empty() {
        return bad_args();
    }
    let mut s = Sum::default();
    tri!(for_each_number(ctx, args, Agg::Numbers, &mut |x| s.add(x * x)));
    Res::num(s.total())
}

fn sumproduct(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.is_empty() {
        return bad_args();
    }
    let mut arrays = Vec::with_capacity(args.len());
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
    let mut total = Sum::default();
    for i in 0..(rows as usize * cols as usize) {
        let mut p = 1.0;
        for a in &arrays {
            match &a.data[i] {
                Value::Number(n) => p *= n,
                Value::Error(e) => return Res::err(*e),
                // Text, logicals and blanks count as zero (which is why people write --(A1:A9>0)).
                _ => p = 0.0,
            }
        }
        total.add(p);
    }
    Res::num(total.total())
}

/// Pairs of numbers from two equally long arrays (pairs where either side isn't a number are skipped).
fn pairs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Result<Vec<(f64, f64)>, ErrorKind> {
    if args.len() != 2 {
        return Err(ErrorKind::Value);
    }
    let (ra, rb) = (ctx.eval(&args[0]), ctx.eval(&args[1]));
    for r in [&ra, &rb] {
        if let Some(e) = r.as_error() {
            return Err(e);
        }
    }
    let (a, b) = (ctx.to_array(&ra), ctx.to_array(&rb));
    if a.data.len() != b.data.len() {
        return Err(ErrorKind::NA);
    }
    let mut out = Vec::with_capacity(a.data.len());
    for (x, y) in a.data.iter().zip(&b.data) {
        match (x, y) {
            (Value::Error(e), _) | (_, Value::Error(e)) => return Err(*e),
            (Value::Number(x), Value::Number(y)) => out.push((*x, *y)),
            _ => {}
        }
    }
    Ok(out)
}

fn sum_pairs(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(f64, f64) -> f64) -> Res {
    let p = tri!(pairs(ctx, args));
    let mut s = Sum::default();
    for (x, y) in p {
        s.add(f(x, y));
    }
    Res::num(s.total())
}

fn sumx2my2(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    sum_pairs(ctx, args, |x, y| x * x - y * y)
}

fn sumx2py2(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    sum_pairs(ctx, args, |x, y| x * x + y * y)
}

fn sumxmy2(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    sum_pairs(ctx, args, |x, y| (x - y) * (x - y))
}

fn seriessum(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 4 {
        return bad_args();
    }
    let x = num!(ctx, &args[0]);
    let n = num!(ctx, &args[1]);
    let m = num!(ctx, &args[2]);
    let r = ctx.eval(&args[3]);
    let coeffs = ctx.to_array(&r);
    let mut s = Sum::default();
    for (i, c) in coeffs.data.iter().enumerate() {
        let a = match c {
            Value::Number(a) => *a,
            Value::Error(e) => return Res::err(*e),
            _ => return bad_args(),
        };
        s.add(a * x.powf(n + i as f64 * m));
    }
    Res::num(s.total())
}

// ---- rounding --------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Rounding {
    HalfAway,
    Away,
    Toward,
}

/// Rounds to `digits` decimal places the way Excel does: on the number's 15-digit decimal value, so 2.675
/// rounds to 2.68 and 1.005 to 1.01.
fn round_digits(x: f64, digits: f64, mode: Rounding) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let d = digits.trunc().clamp(-400.0, 400.0) as i32;
    if d > 308 {
        return x;
    }
    if d < -308 {
        return 0.0;
    }
    let f = 10f64.powi(d.abs());
    let scaled = if d >= 0 { x * f } else { x / f };
    if !scaled.is_finite() || scaled.abs() >= 1e17 {
        // Nothing to round at that position.
        return x;
    }
    let s = clean15(scaled);
    let r = match mode {
        Rounding::HalfAway => s.abs().add_half_floor() * s.signum(),
        Rounding::Away => s.abs().ceil() * s.signum(),
        Rounding::Toward => s.abs().floor() * s.signum(),
    };
    let out = if d >= 0 { r / f } else { r * f };
    if out == 0.0 { 0.0 } else { out }
}

trait HalfFloor {
    fn add_half_floor(self) -> f64;
}

impl HalfFloor for f64 {
    fn add_half_floor(self) -> f64 {
        (self + 0.5).floor()
    }
}

fn round_fn(ctx: &mut Ctx<'_>, args: &[Expr], mode: Rounding) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(0.0))], move |[x, d]| Ok(round_digits(x, d, mode)))
}

fn round(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    round_fn(ctx, args, Rounding::HalfAway)
}

fn roundup(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    round_fn(ctx, args, Rounding::Away)
}

fn rounddown(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    round_fn(ctx, args, Rounding::Toward)
}

fn trunc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    round_fn(ctx, args, Rounding::Toward)
}

fn int(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(x.floor()))
}

/// `x / step` snapped to a whole number when it is one but for float noise (2.1 / 0.7 is 3.0000000000000004).
fn quotient_snapped(x: f64, step: f64) -> f64 {
    let q = x / step;
    let r = q.round();
    if (q - r).abs() <= 1e-12 * q.abs().max(1.0) { r } else { q }
}

fn mround(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, m]| {
        if m == 0.0 || x == 0.0 {
            return Ok(0.0);
        }
        if x.signum() != m.signum() {
            return Err(ErrorKind::Num);
        }
        let q = quotient_snapped(x, m);
        let q = clean15(q);
        Ok(clean15((q.abs() + 0.5).floor() * q.signum() * m))
    })
}

fn ceiling(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0))], |[x, sig]| {
        if sig == 0.0 || x == 0.0 {
            return Ok(0.0);
        }
        if x > 0.0 && sig < 0.0 {
            return Err(ErrorKind::Num);
        }
        // A negative number with a negative significance rounds away from zero; otherwise up.
        let q = quotient_snapped(x, sig);
        Ok(clean15(q.ceil() * sig))
    })
}

fn ceiling_math(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0)), Opt(n(0.0))], |[x, sig, mode]| {
        let sig = if sig == 0.0 { return Ok(0.0) } else { sig.abs() };
        let q = quotient_snapped(x, sig);
        let r = if x < 0.0 && mode != 0.0 { q.floor() } else { q.ceil() };
        Ok(clean15(r * sig))
    })
}

fn ceiling_precise(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0))], |[x, sig]| {
        if sig == 0.0 {
            return Ok(0.0);
        }
        let sig = sig.abs();
        Ok(clean15(quotient_snapped(x, sig).ceil() * sig))
    })
}

fn floor(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0))], |[x, sig]| {
        if x == 0.0 {
            return Ok(0.0);
        }
        if sig == 0.0 {
            return Err(ErrorKind::Div0);
        }
        if x > 0.0 && sig < 0.0 {
            return Err(ErrorKind::Num);
        }
        let q = quotient_snapped(x, sig);
        Ok(clean15(q.floor() * sig))
    })
}

fn floor_math(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0)), Opt(n(0.0))], |[x, sig, mode]| {
        let sig = if sig == 0.0 { return Ok(0.0) } else { sig.abs() };
        let q = quotient_snapped(x, sig);
        let r = if x < 0.0 && mode != 0.0 { q.ceil() } else { q.floor() };
        Ok(clean15(r * sig))
    })
}

fn floor_precise(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(1.0))], |[x, sig]| {
        if sig == 0.0 {
            return Ok(0.0);
        }
        let sig = sig.abs();
        Ok(clean15(quotient_snapped(x, sig).floor() * sig))
    })
}

fn even(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| {
        let r = (clean15(x.abs()) / 2.0).ceil() * 2.0;
        Ok(r * x.signum())
    })
}

fn odd(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| {
        let a = clean15(x.abs());
        let mut r = a.ceil();
        if r % 2.0 == 0.0 {
            r += 1.0;
        }
        Ok(if x < 0.0 { -r } else { r })
    })
}

// ---- arithmetic ------------------------------------------------------------------------------------------

fn abs(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(x.abs()))
}

fn sign(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }))
}

fn modulo(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, d]| {
        if d == 0.0 {
            return Err(ErrorKind::Div0);
        }
        let q = quotient_snapped(x, d).floor();
        let r = x - d * q;
        // A remainder that is the divisor itself but for float noise is zero.
        if r.abs() <= 1e-14 * x.abs().max(d.abs()) || (r - d).abs() <= 1e-14 * d.abs() {
            return Ok(0.0);
        }
        Ok(clean15(r))
    })
}

fn quotient(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, d]| {
        if d == 0.0 {
            return Err(ErrorKind::Div0);
        }
        Ok(quotient_snapped(x, d).trunc())
    })
}

fn power(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req], |[a, b]| Ok(super::super::eval::binary(super::super::ast::BinaryOp::Pow, a, b)))
}

fn sqrt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x < 0.0 { Err(ErrorKind::Num) } else { Ok(x.sqrt()) })
}

fn sqrtpi(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x < 0.0 { Err(ErrorKind::Num) } else { Ok((x * std::f64::consts::PI).sqrt()) })
}

fn exp(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(x.exp()))
}

fn ln(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x <= 0.0 { Err(ErrorKind::Num) } else { Ok(x.ln()) })
}

fn log10(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x <= 0.0 { Err(ErrorKind::Num) } else { Ok(x.log10()) })
}

fn log(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Opt(n(10.0))], |[x, base]| {
        if x <= 0.0 || base <= 0.0 {
            return Err(ErrorKind::Num);
        }
        if base == 1.0 {
            return Err(ErrorKind::Div0);
        }
        if base == 10.0 {
            return Ok(x.log10());
        }
        if base == 2.0 {
            return Ok(x.log2());
        }
        Ok(clean15(x.ln() / base.ln()))
    })
}

fn pi(_: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !args.is_empty() {
        return bad_args();
    }
    Res::num(std::f64::consts::PI)
}

// ---- trigonometry ----------------------------------------------------------------------------------------

/// Excel refuses trigonometry of numbers 2^27 or larger.
const TRIG_LIMIT: f64 = 134_217_728.0;

fn trig(ctx: &mut Ctx<'_>, args: &[Expr], f: fn(f64) -> Result<f64, ErrorKind>) -> Res {
    lifted_num(ctx, args, [Req], move |[x]| f(x))
}

fn sin(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= TRIG_LIMIT { Err(ErrorKind::Num) } else { Ok(x.sin()) })
}

fn cos(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= TRIG_LIMIT { Err(ErrorKind::Num) } else { Ok(x.cos()) })
}

fn tan(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= TRIG_LIMIT { Err(ErrorKind::Num) } else { Ok(x.tan()) })
}

fn asin(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() > 1.0 { Err(ErrorKind::Num) } else { Ok(x.asin()) })
}

fn acos(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() > 1.0 { Err(ErrorKind::Num) } else { Ok(x.acos()) })
}

fn atan(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.atan()))
}

fn atan2(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    // Excel's order is (x, y).
    lifted_num(ctx, args, [Req, Req], |[x, y]| if x == 0.0 && y == 0.0 { Err(ErrorKind::Div0) } else { Ok(y.atan2(x)) })
}

fn sinh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.sinh()))
}

fn cosh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.cosh()))
}

fn tanh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.tanh()))
}

fn asinh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.asinh()))
}

fn acosh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x < 1.0 { Err(ErrorKind::Num) } else { Ok(x.acosh()) })
}

fn atanh(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= 1.0 { Err(ErrorKind::Num) } else { Ok(x.atanh()) })
}

fn cot(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| {
        if x.abs() >= TRIG_LIMIT {
            return Err(ErrorKind::Num);
        }
        if x == 0.0 { Err(ErrorKind::Div0) } else { Ok(1.0 / x.tan()) }
    })
}

fn coth(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| {
        if x.abs() >= TRIG_LIMIT {
            return Err(ErrorKind::Num);
        }
        if x == 0.0 { Err(ErrorKind::Div0) } else { Ok(1.0 / x.tanh()) }
    })
}

fn csc(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| {
        if x.abs() >= TRIG_LIMIT {
            return Err(ErrorKind::Num);
        }
        if x == 0.0 { Err(ErrorKind::Div0) } else { Ok(1.0 / x.sin()) }
    })
}

fn csch(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| {
        if x.abs() >= TRIG_LIMIT {
            return Err(ErrorKind::Num);
        }
        if x == 0.0 { Err(ErrorKind::Div0) } else { Ok(1.0 / x.sinh()) }
    })
}

fn sec(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= TRIG_LIMIT { Err(ErrorKind::Num) } else { Ok(1.0 / x.cos()) })
}

fn sech(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() >= TRIG_LIMIT { Err(ErrorKind::Num) } else { Ok(1.0 / x.cosh()) })
}

fn acot(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(std::f64::consts::FRAC_PI_2 - x.atan()))
}

fn acoth(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| if x.abs() <= 1.0 { Err(ErrorKind::Num) } else { Ok(0.5 * ((x + 1.0) / (x - 1.0)).ln()) })
}

fn degrees(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.to_degrees()))
}

fn radians(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    trig(ctx, args, |x| Ok(x.to_radians()))
}

// ---- combinatorics ---------------------------------------------------------------------------------------

/// n! for 0 <= n <= 170.
pub(super) fn factorial(n: f64) -> Result<f64, ErrorKind> {
    let n = n.trunc();
    if n < 0.0 {
        return Err(ErrorKind::Num);
    }
    if n > 170.0 {
        return Err(ErrorKind::Num);
    }
    let mut p = 1.0;
    let mut i = 2.0;
    while i <= n {
        p *= i;
        i += 1.0;
    }
    Ok(p)
}

/// The binomial coefficient C(n, k) for whole numbers 0 <= k <= n.
pub(super) fn binomial(n: f64, k: f64) -> f64 {
    let k = k.min(n - k);
    if k < 0.0 {
        return 0.0;
    }
    if k == 0.0 {
        return 1.0;
    }
    if n <= 1029.0 {
        let mut c = 1.0;
        let mut i = 1.0;
        while i <= k {
            c = c * (n - k + i) / i;
            i += 1.0;
        }
        return if c < 9.007_199_254_740_992e15 { c.round() } else { c };
    }
    (super::dist::ln_gamma(n + 1.0) - super::dist::ln_gamma(k + 1.0) - super::dist::ln_gamma(n - k + 1.0)).exp()
}

fn fact(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| factorial(x))
}

fn factdouble(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| {
        let n = x.trunc();
        if n < -1.0 {
            return Err(ErrorKind::Num);
        }
        let mut p = 1.0;
        let mut i = n;
        while i > 1.0 {
            p *= i;
            if !p.is_finite() {
                return Err(ErrorKind::Num);
            }
            i -= 2.0;
        }
        Ok(p)
    })
}

fn combin(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[n, k]| {
        let (n, k) = (n.trunc(), k.trunc());
        if n < 0.0 || k < 0.0 || n < k {
            return Err(ErrorKind::Num);
        }
        Ok(binomial(n, k))
    })
}

fn combina(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[n, k]| {
        let (n, k) = (n.trunc(), k.trunc());
        if n < 0.0 || k < 0.0 || (n == 0.0 && k > 0.0) {
            return Err(ErrorKind::Num);
        }
        Ok(binomial(n + k - 1.0, k))
    })
}

fn permut(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[n, k]| {
        let (n, k) = (n.trunc(), k.trunc());
        if n < 0.0 || k < 0.0 || n < k {
            return Err(ErrorKind::Num);
        }
        let mut p = 1.0;
        let mut i = n - k + 1.0;
        while i <= n {
            p *= i;
            i += 1.0;
        }
        Ok(p)
    })
}

fn permutationa(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[n, k]| {
        let (n, k) = (n.trunc(), k.trunc());
        if n < 0.0 || k < 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(n.powf(k))
    })
}

fn multinomial(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(numbers(ctx, args, Agg::Numbers));
    let mut total = 0.0;
    let mut result = 1.0;
    for x in v {
        let x = x.trunc();
        if x < 0.0 {
            return Res::err(ErrorKind::Num);
        }
        total += x;
        result *= binomial(total, x);
    }
    Res::num(result)
}

fn whole_numbers(ctx: &mut Ctx<'_>, args: &[Expr]) -> Result<Vec<u64>, ErrorKind> {
    let v = numbers(ctx, args, Agg::Numbers)?;
    v.into_iter()
        .map(|x| {
            let x = x.trunc();
            if !(0.0..9.007_199_254_740_992e15).contains(&x) { Err(ErrorKind::Num) } else { Ok(x as u64) }
        })
        .collect()
}

fn gcd2(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn gcd(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(whole_numbers(ctx, args));
    Res::num(v.into_iter().fold(0, gcd2) as f64)
}

fn lcm(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    let v = tri!(whole_numbers(ctx, args));
    let mut l: f64 = 1.0;
    for x in v {
        if x == 0 {
            return Res::num(0.0);
        }
        let g = gcd2(l as u64, x) as f64;
        l = l / g * x as f64;
        if l >= 9.007_199_254_740_992e15 {
            return Res::err(ErrorKind::Num);
        }
    }
    Res::num(l)
}

// ---- number bases and numerals ---------------------------------------------------------------------------

fn base(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Opt(n(0.0))], |[x, radix, min_len]| {
        let x = as_num(x)?.trunc();
        let radix = as_num(radix)?.trunc();
        let min_len = as_num(min_len)?.trunc();
        if !(0.0..9.007_199_254_740_992e15).contains(&x) || !(2.0..=36.0).contains(&radix) || !(0.0..=255.0).contains(&min_len) {
            return Err(ErrorKind::Num);
        }
        let (mut v, r) = (x as u64, radix as u64);
        let mut digits = Vec::new();
        while v > 0 {
            digits.push(std::char::from_digit((v % r) as u32, r as u32).unwrap_or('0').to_ascii_uppercase());
            v /= r;
        }
        while digits.len() < (min_len as usize).max(1) {
            digits.push('0');
        }
        Ok(Value::from(digits.iter().rev().collect::<String>()))
    })
}

fn decimal(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req], |[t, radix]| {
        let t = as_text(t)?;
        let radix = as_num(radix)?.trunc();
        if !(2.0..=36.0).contains(&radix) || t.chars().count() > 255 {
            return Err(ErrorKind::Num);
        }
        let mut v: f64 = 0.0;
        for c in t.trim().chars() {
            let d = c.to_digit(radix as u32).ok_or(ErrorKind::Num)?;
            v = v * radix + d as f64;
        }
        if v >= 9.007_199_254_740_992e15 {
            return Err(ErrorKind::Num);
        }
        Ok(Value::Number(v))
    })
}

/// Roman numerals in Excel's five forms, from classic (0) to most concise (4).
fn roman_text(value: u32, form: u32) -> String {
    const CHARS: [char; 7] = ['M', 'D', 'C', 'L', 'X', 'V', 'I'];
    const VALUES: [u32; 7] = [1000, 500, 100, 50, 10, 5, 1];
    let max_index = VALUES.len() - 1;
    let mut out = String::new();
    let mut val = value;
    for i in 0..=max_index / 2 {
        let mut index = 2 * i;
        let digit = val / VALUES[index];
        if digit % 5 == 4 {
            let index2 = if digit == 4 { index - 1 } else { index - 2 };
            let mut steps = 0;
            while steps < form && index < max_index {
                steps += 1;
                if VALUES[index2] - VALUES[index + 1] <= val {
                    index += 1;
                } else {
                    steps = form;
                }
            }
            out.push(CHARS[index]);
            out.push(CHARS[index2]);
            val = val + VALUES[index] - VALUES[index2];
        } else {
            if digit > 4 {
                out.push(CHARS[index - 1]);
            }
            for _ in 0..digit % 5 {
                out.push(CHARS[index]);
            }
            val %= VALUES[index];
        }
    }
    out
}

fn roman(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Opt(n(0.0))], |[x, form]| {
        let x = as_num(x)?.trunc();
        let form = match form {
            Value::Bool(true) => 0.0,
            Value::Bool(false) => 4.0,
            other => as_num(other)?.trunc(),
        };
        if !(0.0..=3999.0).contains(&x) || !(0.0..=4.0).contains(&form) {
            return Err(ErrorKind::Value);
        }
        Ok(Value::from(roman_text(x as u32, form as u32)))
    })
}

fn arabic(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req], |[t]| {
        let t = as_text(t)?;
        let s = t.trim();
        if s.chars().count() > 255 {
            return Err(ErrorKind::Value);
        }
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let value = |c: char| -> Option<i64> {
            Some(match c.to_ascii_uppercase() {
                'I' => 1,
                'V' => 5,
                'X' => 10,
                'L' => 50,
                'C' => 100,
                'D' => 500,
                'M' => 1000,
                _ => return None,
            })
        };
        let vals: Vec<i64> = s.chars().map(value).collect::<Option<_>>().ok_or(ErrorKind::Value)?;
        let mut total = 0i64;
        for (i, &v) in vals.iter().enumerate() {
            if vals.get(i + 1).is_some_and(|&next| next > v) {
                total -= v;
            } else {
                total += v;
            }
        }
        Ok(Value::Number(if neg { -total } else { total } as f64))
    })
}

// ---- random numbers --------------------------------------------------------------------------------------

fn rand(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !args.is_empty() {
        return bad_args();
    }
    Res::num(ctx.grid.random())
}

fn randbetween(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (lo, hi) = (num!(ctx, &args[0]).ceil(), num!(ctx, &args[1]).floor());
    if lo > hi {
        return Res::err(ErrorKind::Num);
    }
    Res::num((lo + (ctx.grid.random() * (hi - lo + 1.0)).floor()).min(hi))
}

// ---- matrices --------------------------------------------------------------------------------------------

/// A numeric matrix from a range or array; anything that isn't a number is #VALUE!.
fn matrix(ctx: &mut Ctx<'_>, e: &Expr) -> Result<(usize, usize, Vec<f64>), ErrorKind> {
    let r = ctx.eval(e);
    if let Some(err) = r.as_error() {
        return Err(err);
    }
    let a = ctx.to_array(&r);
    let mut out = Vec::with_capacity(a.data.len());
    for v in &a.data {
        match v {
            Value::Number(n) => out.push(*n),
            Value::Error(e) => return Err(*e),
            _ => return Err(ErrorKind::Value),
        }
    }
    Ok((a.rows as usize, a.cols as usize, out))
}

fn number_array(rows: usize, cols: usize, data: Vec<f64>) -> Res {
    Res::A(Arc::new(Array::new(rows as u32, cols as u32, data.into_iter().map(num_result).collect())))
}

fn mmult(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (ar, ac, a) = tri!(matrix(ctx, &args[0]));
    let (br, bc, b) = tri!(matrix(ctx, &args[1]));
    if ac != br {
        return bad_args();
    }
    let mut out = vec![0.0; ar * bc];
    for i in 0..ar {
        for j in 0..bc {
            let mut s = Sum::default();
            for k in 0..ac {
                s.add(a[i * ac + k] * b[k * bc + j]);
            }
            out[i * bc + j] = s.total();
        }
    }
    number_array(ar, bc, out)
}

/// LU decomposition with partial pivoting: the determinant, and the factors for solving.
fn lu(n: usize, mut m: Vec<f64>) -> (f64, Vec<f64>, Vec<usize>) {
    let mut perm: Vec<usize> = (0..n).collect();
    let mut det = 1.0;
    for k in 0..n {
        let mut p = k;
        for i in k + 1..n {
            if m[i * n + k].abs() > m[p * n + k].abs() {
                p = i;
            }
        }
        if m[p * n + k] == 0.0 {
            return (0.0, m, perm);
        }
        if p != k {
            for j in 0..n {
                m.swap(k * n + j, p * n + j);
            }
            perm.swap(k, p);
            det = -det;
        }
        det *= m[k * n + k];
        for i in k + 1..n {
            let f = m[i * n + k] / m[k * n + k];
            m[i * n + k] = f;
            for j in k + 1..n {
                m[i * n + j] -= f * m[k * n + j];
            }
        }
    }
    (det, m, perm)
}

fn mdeterm(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 1 {
        return bad_args();
    }
    let (r, c, m) = tri!(matrix(ctx, &args[0]));
    if r != c {
        return bad_args();
    }
    let (det, _, _) = lu(r, m);
    Res::num(clean15(det))
}

fn minverse(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 1 {
        return bad_args();
    }
    let (r, c, m) = tri!(matrix(ctx, &args[0]));
    if r != c {
        return bad_args();
    }
    let n = r;
    let (det, f, perm) = lu(n, m);
    if det == 0.0 || !det.is_finite() {
        return Res::err(ErrorKind::Num);
    }
    let mut inv = vec![0.0; n * n];
    for col in 0..n {
        // Solve L U x = P e_col.
        let mut x: Vec<f64> = (0..n).map(|i| if perm[i] == col { 1.0 } else { 0.0 }).collect();
        for i in 0..n {
            for k in 0..i {
                x[i] -= f[i * n + k] * x[k];
            }
        }
        for i in (0..n).rev() {
            for k in i + 1..n {
                x[i] -= f[i * n + k] * x[k];
            }
            x[i] /= f[i * n + i];
        }
        for i in 0..n {
            inv[i * n + col] = clean15(x[i]);
        }
    }
    number_array(n, n, inv)
}

fn munit(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 1 {
        return bad_args();
    }
    let k = num!(ctx, &args[0]).trunc();
    if k < 1.0 {
        return bad_args();
    }
    if k > 4096.0 {
        return Res::err(ErrorKind::Num);
    }
    let k = k as usize;
    let mut data = vec![0.0; k * k];
    for i in 0..k {
        data[i * k + i] = 1.0;
    }
    number_array(k, k, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roman_forms() {
        assert_eq!(roman_text(499, 0), "CDXCIX");
        assert_eq!(roman_text(499, 1), "LDVLIV");
        assert_eq!(roman_text(499, 2), "XDIX");
        assert_eq!(roman_text(499, 3), "VDIV");
        assert_eq!(roman_text(499, 4), "ID");
        assert_eq!(roman_text(2013, 0), "MMXIII");
        assert_eq!(roman_text(1999, 0), "MCMXCIX");
    }

    #[test]
    fn rounding() {
        assert_eq!(round_digits(2.675, 2.0, Rounding::HalfAway), 2.68);
        assert_eq!(round_digits(1.005, 2.0, Rounding::HalfAway), 1.01);
        assert_eq!(round_digits(-2.5, 0.0, Rounding::HalfAway), -3.0);
        assert_eq!(round_digits(1234.567, -2.0, Rounding::HalfAway), 1200.0);
        assert_eq!(round_digits(0.1 + 0.2, 1.0, Rounding::Away), 0.3);
        assert_eq!(round_digits(3.14159, 3.0, Rounding::Toward), 3.141);
        assert_eq!(round_digits(-3.14159, 1.0, Rounding::Toward), -3.1);
        assert_eq!(round_digits(31415.92654, -2.0, Rounding::Toward), 31400.0);
    }
}
