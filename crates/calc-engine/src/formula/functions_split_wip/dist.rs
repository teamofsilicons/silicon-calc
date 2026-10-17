//! Probability distributions, their inverses, and the tests built on them. The special functions underneath
//! (log-gamma, the regularized incomplete gamma and beta functions and their inverses, erf) are computed to close
//! to full double precision, so results match Excel's to at least ten significant digits.

use std::f64::consts::{PI, SQRT_2};

use super::*;

pub(super) const FUNCTIONS: &[(&str, Func)] = &[
    ("NORM.DIST", norm_dist),
    ("NORMDIST", norm_dist),
    ("NORM.INV", norm_inv),
    ("NORMINV", norm_inv),
    ("NORM.S.DIST", norm_s_dist),
    ("NORMSDIST", normsdist),
    ("NORM.S.INV", norm_s_inv),
    ("NORMSINV", norm_s_inv),
    ("LOGNORM.DIST", lognorm_dist),
    ("LOGNORMDIST", lognormdist),
    ("LOGNORM.INV", lognorm_inv),
    ("LOGINV", lognorm_inv),
    ("EXPON.DIST", expon_dist),
    ("EXPONDIST", expon_dist),
    ("POISSON.DIST", poisson_dist),
    ("POISSON", poisson_dist),
    ("BINOM.DIST", binom_dist),
    ("BINOMDIST", binom_dist),
    ("BINOM.DIST.RANGE", binom_dist_range),
    ("BINOM.INV", binom_inv),
    ("CRITBINOM", binom_inv),
    ("NEGBINOM.DIST", negbinom_dist),
    ("NEGBINOMDIST", negbinomdist),
    ("HYPGEOM.DIST", hypgeom_dist),
    ("HYPGEOMDIST", hypgeomdist),
    ("T.DIST", t_dist),
    ("T.DIST.2T", t_dist_2t),
    ("T.DIST.RT", t_dist_rt),
    ("TDIST", tdist),
    ("T.INV", t_inv),
    ("T.INV.2T", t_inv_2t),
    ("TINV", t_inv_2t),
    ("CHISQ.DIST", chisq_dist),
    ("CHISQ.DIST.RT", chisq_dist_rt),
    ("CHIDIST", chisq_dist_rt),
    ("CHISQ.INV", chisq_inv),
    ("CHISQ.INV.RT", chisq_inv_rt),
    ("CHIINV", chisq_inv_rt),
    ("F.DIST", f_dist),
    ("F.DIST.RT", f_dist_rt),
    ("FDIST", f_dist_rt),
    ("F.INV", f_inv),
    ("F.INV.RT", f_inv_rt),
    ("FINV", f_inv_rt),
    ("GAMMA.DIST", gamma_dist),
    ("GAMMADIST", gamma_dist),
    ("GAMMA.INV", gamma_inv),
    ("GAMMAINV", gamma_inv),
    ("GAMMA", gamma_fn),
    ("GAMMALN", gammaln),
    ("GAMMALN.PRECISE", gammaln),
    ("BETA.DIST", beta_dist),
    ("BETADIST", betadist),
    ("BETA.INV", beta_inv),
    ("BETAINV", beta_inv),
    ("WEIBULL.DIST", weibull_dist),
    ("WEIBULL", weibull_dist),
    ("FISHER", fisher),
    ("FISHERINV", fisherinv),
    ("PHI", phi),
    ("GAUSS", gauss),
    ("CONFIDENCE.NORM", confidence_norm),
    ("CONFIDENCE", confidence_norm),
    ("CONFIDENCE.T", confidence_t),
    ("T.TEST", t_test),
    ("TTEST", t_test),
    ("CHISQ.TEST", chisq_test),
    ("CHITEST", chisq_test),
    ("F.TEST", f_test),
    ("FTEST", f_test),
    ("Z.TEST", z_test),
    ("ZTEST", z_test),
];

// ---- special functions -----------------------------------------------------------------------------------

const LANCZOS_G: f64 = 7.0;
const LANCZOS: [f64; 9] = [
    0.999_999_999_999_809_93,
    676.520_368_121_885_1,
    -1_259.139_216_722_402_8,
    771.323_428_777_653_13,
    -176.615_029_162_140_59,
    12.507_343_278_686_905,
    -0.138_571_095_265_720_12,
    9.984_369_578_019_571_6e-6,
    1.505_632_735_149_311_6e-7,
];

/// ln Γ(x) for x > 0.
pub(super) fn ln_gamma(x: f64) -> f64 {
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1−x) = π / sin(πx).
        return (PI / (PI * x).sin().abs()).ln() - ln_gamma(1.0 - x);
    }
    if x == 1.0 || x == 2.0 {
        return 0.0;
    }
    let x = x - 1.0;
    let mut a = LANCZOS[0];
    let t = x + LANCZOS_G + 0.5;
    for (i, c) in LANCZOS.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Γ(x) for any x that isn't zero or a negative whole number.
pub(super) fn gamma(x: f64) -> f64 {
    if x == x.floor() && x > 0.0 && x <= 171.0 {
        let mut p = 1.0;
        let mut i = 2.0;
        while i < x {
            p *= i;
            i += 1.0;
        }
        return p;
    }
    if x < 0.5 {
        return PI / ((PI * x).sin() * gamma(1.0 - x));
    }
    let x1 = x - 1.0;
    let mut a = LANCZOS[0];
    let t = x1 + LANCZOS_G + 0.5;
    for (i, c) in LANCZOS.iter().enumerate().skip(1) {
        a += c / (x1 + i as f64);
    }
    if x > 140.0 {
        return (ln_gamma(x)).exp();
    }
    (2.0 * PI).sqrt() * t.powf(x1 + 0.5) * (-t).exp() * a
}

const FPMIN: f64 = 1e-300;
const EPS: f64 = 1e-16;

/// The regularized lower and upper incomplete gamma functions P(a, x) and Q(a, x).
pub(super) fn gamma_pq(a: f64, x: f64) -> (f64, f64) {
    if x <= 0.0 {
        return (0.0, 1.0);
    }
    let lead = (-x + a * x.ln() - ln_gamma(a)).exp();
    if x < a + 1.0 {
        let mut ap = a;
        let mut del = 1.0 / a;
        let mut sum = del;
        for _ in 0..1_000_000 {
            ap += 1.0;
            del *= x / ap;
            sum += del;
            if del.abs() < sum.abs() * EPS {
                break;
            }
        }
        let p = (sum * lead).min(1.0);
        (p, 1.0 - p)
    } else {
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / FPMIN;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..1_000_000 {
            let an = -(i as f64) * (i as f64 - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < FPMIN {
                d = FPMIN;
            }
            c = b + an / c;
            if c.abs() < FPMIN {
                c = FPMIN;
            }
            d = 1.0 / d;
            let del = d * c;
            h *= del;
            if (del - 1.0).abs() <= EPS {
                break;
            }
        }
        let q = (lead * h).min(1.0);
        (1.0 - q, q)
    }
}

/// The x with P(a, x) = p.
pub(super) fn gamma_p_inv(p: f64, a: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    let gln = ln_gamma(a);
    let a1 = a - 1.0;
    let (lna1, afac) = if a > 1.0 { (a1.ln(), (a1 * (a1.ln() - 1.0) - gln).exp()) } else { (0.0, 0.0) };
    let mut x = if a > 1.0 {
        let pp = if p < 0.5 { p } else { 1.0 - p };
        let t = (-2.0 * pp.ln()).sqrt();
        let mut z = (2.30753 + t * 0.27061) / (1.0 + t * (0.99229 + t * 0.04481)) - t;
        if p < 0.5 {
            z = -z;
        }
        (a * (1.0 - 1.0 / (9.0 * a) - z / (3.0 * a.sqrt())).powi(3)).max(1e-3)
    } else {
        let t = 1.0 - a * (0.253 + a * 0.12);
        if p < t { (p / t).powf(1.0 / a) } else { 1.0 - (1.0 - (p - t) / (1.0 - t)).ln() }
    };
    for _ in 0..100 {
        if x <= 0.0 {
            return 0.0;
        }
        let err = gamma_pq(a, x).0 - p;
        let t = if a > 1.0 { afac * (-(x - a1) + a1 * (x.ln() - lna1)).exp() } else { (-x + a1 * x.ln() - gln).exp() };
        if t == 0.0 || !t.is_finite() {
            break;
        }
        let u = err / t;
        let step = u / (1.0 - 0.5 * (u * ((a - 1.0) / x - 1.0)).min(1.0));
        x -= step;
        if x <= 0.0 {
            x = 0.5 * (x + step);
        }
        if step.abs() < 1e-15 * x {
            return x;
        }
    }
    // Fall back to bisection, which always gets there.
    bisect(|x| gamma_pq(a, x).0 - p, 0.0, x.max(1.0) * 4.0 + a * 10.0 + 100.0)
}

fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..100_000 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() <= EPS {
            break;
        }
    }
    h
}

/// The regularized incomplete beta function I_x(a, b) and its complement 1 − I_x(a, b), each computed directly.
pub(super) fn beta_reg(a: f64, b: f64, x: f64) -> (f64, f64) {
    if x <= 0.0 {
        return (0.0, 1.0);
    }
    if x >= 1.0 {
        return (1.0, 0.0);
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        let lo = (bt * beta_cf(a, b, x) / a).clamp(0.0, 1.0);
        (lo, 1.0 - lo)
    } else {
        let hi = (bt * beta_cf(b, a, 1.0 - x) / b).clamp(0.0, 1.0);
        (1.0 - hi, hi)
    }
}

/// The x with I_x(a, b) = p.
pub(super) fn beta_reg_inv(p: f64, a: f64, b: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    let (a1, b1) = (a - 1.0, b - 1.0);
    let mut x = if a >= 1.0 && b >= 1.0 {
        let pp = if p < 0.5 { p } else { 1.0 - p };
        let t = (-2.0 * pp.ln()).sqrt();
        let mut z = (2.30753 + t * 0.27061) / (1.0 + t * (0.99229 + t * 0.04481)) - t;
        if p < 0.5 {
            z = -z;
        }
        let al = (z * z - 3.0) / 6.0;
        let h = 2.0 / (1.0 / (2.0 * a - 1.0) + 1.0 / (2.0 * b - 1.0));
        let w = (z * (al + h).sqrt() / h) - (1.0 / (2.0 * b - 1.0) - 1.0 / (2.0 * a - 1.0)) * (al + 5.0 / 6.0 - 2.0 / (3.0 * h));
        a / (a + b * (2.0 * w).exp())
    } else {
        let (lna, lnb) = ((a / (a + b)).ln(), (b / (a + b)).ln());
        let t = (a * lna).exp() / a;
        let u = (b * lnb).exp() / b;
        let w = t + u;
        if p < t / w { (a * w * p).powf(1.0 / a) } else { 1.0 - (b * w * (1.0 - p)).powf(1.0 / b) }
    };
    let afac = -ln_gamma(a) - ln_gamma(b) + ln_gamma(a + b);
    for j in 0..100 {
        if x <= 0.0 || x >= 1.0 || !x.is_finite() {
            break;
        }
        let err = beta_reg(a, b, x).0 - p;
        let t = (a1 * x.ln() + b1 * (1.0 - x).ln() + afac).exp();
        if t == 0.0 || !t.is_finite() {
            break;
        }
        let u = err / t;
        let step = u / (1.0 - 0.5 * (u * (a1 / x - b1 / (1.0 - x))).min(1.0));
        x -= step;
        if x <= 0.0 {
            x = 0.5 * (x + step);
        }
        if x >= 1.0 {
            x = 0.5 * (x + step + 1.0);
        }
        if step.abs() < 1e-15 * x && j > 0 {
            return x;
        }
    }
    bisect(|x| beta_reg(a, b, x).0 - p, 0.0, 1.0)
}

/// A root of an increasing function on [lo, hi] by bisection.
fn bisect(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    for _ in 0..400 {
        let mid = 0.5 * (lo + hi);
        if mid == lo || mid == hi {
            break;
        }
        if f(mid) < 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// erf(x) and erfc(x), each accurate to about 1e-15 relative.
pub(super) fn erf_erfc(x: f64) -> (f64, f64) {
    if x.is_nan() {
        return (f64::NAN, f64::NAN);
    }
    let ax = x.abs();
    if ax < 2.0 {
        // erf(x) = 2/√π e^(−x²) Σ 2ⁿ x^(2n+1) / (1·3·…·(2n+1)): every term positive, no cancellation.
        let x2 = ax * ax;
        let mut term = ax;
        let mut sum = ax;
        let mut k = 1.0;
        loop {
            term *= 2.0 * x2 / (2.0 * k + 1.0);
            sum += term;
            if term < sum * 1e-17 {
                break;
            }
            k += 1.0;
        }
        let e = 2.0 / PI.sqrt() * (-x2).exp() * sum;
        let e = e.min(1.0);
        return if x < 0.0 { (-e, 1.0 + e) } else { (e, 1.0 - e) };
    }
    // erfc by its continued fraction, which converges quickly away from zero.
    let c = if ax > 27.3 {
        0.0
    } else {
        let mut f = ax;
        let mut cc = ax;
        let mut d = 0.0;
        for i in 1..5000 {
            let an = i as f64 * 0.5;
            d = ax + an * d;
            if d.abs() < FPMIN {
                d = FPMIN;
            }
            cc = ax + an / cc;
            if cc.abs() < FPMIN {
                cc = FPMIN;
            }
            d = 1.0 / d;
            let del = cc * d;
            f *= del;
            if (del - 1.0).abs() < 1e-16 {
                break;
            }
        }
        (-ax * ax).exp() / PI.sqrt() / f
    };
    if x < 0.0 { (c - 1.0, 2.0 - c) } else { (1.0 - c, c) }
}

/// The standard normal cumulative distribution Φ(z).
pub(super) fn norm_cdf(z: f64) -> f64 {
    0.5 * erf_erfc(-z / SQRT_2).1
}

pub(super) fn norm_pdf(z: f64) -> f64 {
    (-0.5 * z * z).exp() / (2.0 * PI).sqrt()
}

/// The z with Φ(z) = p (Acklam's approximation polished with Halley steps to full precision).
pub(super) fn norm_inv_std(p: f64) -> f64 {
    const A: [f64; 6] = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.383577518672690e2, -3.066479806614716e1, 2.506628277459239];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let p_low = 0.02425;
    let mut x = if p < p_low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - p_low {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    for _ in 0..3 {
        // In the upper tail work with the complement, which keeps its precision.
        let e = if p > 0.5 { (1.0 - p) - 0.5 * erf_erfc(x / SQRT_2).1 } else { norm_cdf(x) - p };
        let e = if p > 0.5 { -e } else { e };
        let u = e * (2.0 * PI).sqrt() * (x * x / 2.0).exp();
        x -= u / (1.0 + x * u / 2.0);
    }
    x
}

/// Student's t: the probability that T <= t.
pub(super) fn t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let (lo, _) = beta_reg(df / 2.0, 0.5, x);
    if t > 0.0 { 1.0 - 0.5 * lo } else { 0.5 * lo }
}

/// Student's t: the probability that T > t, for t >= 0, computed directly.
fn t_upper(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    0.5 * beta_reg(df / 2.0, 0.5, x).0
}

fn t_pdf(t: f64, df: f64) -> f64 {
    (ln_gamma((df + 1.0) / 2.0) - ln_gamma(df / 2.0) - 0.5 * (df * PI).ln() - (df + 1.0) / 2.0 * (1.0 + t * t / df).ln()).exp()
}

/// The t ≥ 0 with P(|T| > t) = p.
fn t_inv_two_tailed(p: f64, df: f64) -> f64 {
    if p >= 1.0 {
        return 0.0;
    }
    let x = beta_reg_inv(p, df / 2.0, 0.5);
    (df * (1.0 - x) / x).sqrt()
}

pub(super) fn chisq_cdf(x: f64, df: f64) -> (f64, f64) {
    gamma_pq(df / 2.0, x / 2.0)
}

fn chisq_pdf(x: f64, df: f64) -> f64 {
    if x <= 0.0 {
        return if df == 2.0 && x == 0.0 { 0.5 } else { 0.0 };
    }
    ((df / 2.0 - 1.0) * x.ln() - x / 2.0 - (df / 2.0) * 2f64.ln() - ln_gamma(df / 2.0)).exp()
}

fn f_cdf(x: f64, d1: f64, d2: f64) -> (f64, f64) {
    if x <= 0.0 {
        return (0.0, 1.0);
    }
    beta_reg(d1 / 2.0, d2 / 2.0, d1 * x / (d1 * x + d2))
}

fn f_pdf(x: f64, d1: f64, d2: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let ln_b = ln_gamma(d1 / 2.0) + ln_gamma(d2 / 2.0) - ln_gamma((d1 + d2) / 2.0);
    (0.5 * (d1 * (d1 * x).ln() + d2 * d2.ln() - (d1 + d2) * (d1 * x + d2).ln()) - x.ln() - ln_b).exp()
}

fn f_inv_lower(p: f64, d1: f64, d2: f64) -> f64 {
    let y = beta_reg_inv(p, d1 / 2.0, d2 / 2.0);
    if y >= 1.0 {
        return f64::INFINITY;
    }
    d2 * y / (d1 * (1.0 - y))
}

/// ln C(n, k).
fn ln_choose(n: f64, k: f64) -> f64 {
    ln_gamma(n + 1.0) - ln_gamma(k + 1.0) - ln_gamma(n - k + 1.0)
}

fn binom_pmf(k: f64, n: f64, p: f64) -> f64 {
    if p == 0.0 {
        return if k == 0.0 { 1.0 } else { 0.0 };
    }
    if p == 1.0 {
        return if k == n { 1.0 } else { 0.0 };
    }
    if n <= 1029.0 {
        return super::math::binomial(n, k) * p.powf(k) * (1.0 - p).powf(n - k);
    }
    (ln_choose(n, k) + k * p.ln() + (n - k) * (1.0 - p).ln()).exp()
}

fn binom_cdf(k: f64, n: f64, p: f64) -> f64 {
    if k >= n {
        return 1.0;
    }
    if n <= 1000.0 {
        let mut s = Sum::default();
        let mut i = 0.0;
        while i <= k {
            s.add(binom_pmf(i, n, p));
            i += 1.0;
        }
        return s.total().min(1.0);
    }
    beta_reg(n - k, k + 1.0, 1.0 - p).0
}

fn poisson_pmf(k: f64, lambda: f64) -> f64 {
    if lambda == 0.0 {
        return if k == 0.0 { 1.0 } else { 0.0 };
    }
    (-lambda + k * lambda.ln() - ln_gamma(k + 1.0)).exp()
}

// ---- normal and related --------------------------------------------------------------------------------

fn norm_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[x, m, sd, cum]| {
        let (x, m, sd) = (as_num(x)?, as_num(m)?, as_num(sd)?);
        let cum = cum.coerce_bool()?;
        if sd <= 0.0 {
            return Err(ErrorKind::Num);
        }
        let z = (x - m) / sd;
        Ok(num_result(if cum { norm_cdf(z) } else { norm_pdf(z) / sd }))
    })
}

fn norm_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[p, m, sd]| {
        if p <= 0.0 || p >= 1.0 || sd <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(m + sd * norm_inv_std(p))
    })
}

fn norm_s_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req], |[z, cum]| {
        let z = as_num(z)?;
        Ok(num_result(if cum.coerce_bool()? { norm_cdf(z) } else { norm_pdf(z) }))
    })
}

fn normsdist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[z]| Ok(norm_cdf(z)))
}

fn norm_s_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[p]| if p <= 0.0 || p >= 1.0 { Err(ErrorKind::Num) } else { Ok(norm_inv_std(p)) })
}

fn lognorm_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[x, m, sd, cum]| {
        let (x, m, sd) = (as_num(x)?, as_num(m)?, as_num(sd)?);
        let cum = cum.coerce_bool()?;
        if x <= 0.0 || sd <= 0.0 {
            return Err(ErrorKind::Num);
        }
        let z = (x.ln() - m) / sd;
        Ok(num_result(if cum { norm_cdf(z) } else { norm_pdf(z) / (x * sd) }))
    })
}

fn lognormdist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[x, m, sd]| {
        if x <= 0.0 || sd <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(norm_cdf((x.ln() - m) / sd))
    })
}

fn lognorm_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[p, m, sd]| {
        if p <= 0.0 || p >= 1.0 || sd <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok((m + sd * norm_inv_std(p)).exp())
    })
}

fn expon_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req], |[x, lambda, cum]| {
        let (x, l) = (as_num(x)?, as_num(lambda)?);
        let cum = cum.coerce_bool()?;
        if x < 0.0 || l <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(num_result(if cum { -(-l * x).exp_m1() } else { l * (-l * x).exp() }))
    })
}

fn weibull_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[x, alpha, beta, cum]| {
        let (x, a, b) = (as_num(x)?, as_num(alpha)?, as_num(beta)?);
        let cum = cum.coerce_bool()?;
        if x < 0.0 || a <= 0.0 || b <= 0.0 {
            return Err(ErrorKind::Num);
        }
        let t = (x / b).powf(a);
        Ok(num_result(if cum { -(-t).exp_m1() } else { a / b.powf(a) * x.powf(a - 1.0) * (-t).exp() }))
    })
}

fn fisher(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x <= -1.0 || x >= 1.0 { Err(ErrorKind::Num) } else { Ok(x.atanh()) })
}

fn fisherinv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[y]| Ok(y.tanh()))
}

fn phi(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(norm_pdf(x)))
}

fn gauss(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| Ok(if x >= 0.0 { 0.5 * erf_erfc(x / SQRT_2).0 } else { -0.5 * erf_erfc(-x / SQRT_2).0 }))
}

// ---- discrete ------------------------------------------------------------------------------------------

fn poisson_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req], |[x, mean, cum]| {
        let (k, l) = (as_num(x)?.trunc(), as_num(mean)?);
        let cum = cum.coerce_bool()?;
        if k < 0.0 || l < 0.0 {
            return Err(ErrorKind::Num);
        }
        if !cum {
            return Ok(num_result(poisson_pmf(k, l)));
        }
        if l == 0.0 {
            return Ok(Value::Number(1.0));
        }
        if k <= 1000.0 {
            let mut s = Sum::default();
            let mut i = 0.0;
            while i <= k {
                s.add(poisson_pmf(i, l));
                i += 1.0;
            }
            return Ok(num_result(s.total().min(1.0)));
        }
        Ok(num_result(gamma_pq(k + 1.0, l).1))
    })
}

fn binom_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[s, trials, p, cum]| {
        let (k, n, p) = (as_num(s)?.trunc(), as_num(trials)?.trunc(), as_num(p)?);
        let cum = cum.coerce_bool()?;
        if k < 0.0 || k > n || !(0.0..=1.0).contains(&p) {
            return Err(ErrorKind::Num);
        }
        Ok(num_result(if cum { binom_cdf(k, n, p) } else { binom_pmf(k, n, p) }))
    })
}

fn binom_dist_range(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !arity(args, 3, 4) {
        return bad_args();
    }
    lifted_num(ctx, args, [Req, Req, Req, Opt(Value::Number(f64::NAN))], |[trials, p, s, s2]| {
        let (n, k1) = (trials.trunc(), s.trunc());
        let k2 = if s2.is_nan() { k1 } else { s2.trunc() };
        if n < 0.0 || !(0.0..=1.0).contains(&p) || k1 < 0.0 || k1 > n || k2 < k1 || k2 > n {
            return Err(ErrorKind::Num);
        }
        let mut sum = Sum::default();
        let mut i = k1;
        while i <= k2 {
            sum.add(binom_pmf(i, n, p));
            i += 1.0;
        }
        Ok(sum.total())
    })
}

fn binom_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[trials, p, alpha]| {
        let n = trials.trunc();
        if n < 0.0 || !(0.0..=1.0).contains(&p) || !(0.0..=1.0).contains(&alpha) {
            return Err(ErrorKind::Num);
        }
        let mut s = Sum::default();
        let mut k = 0.0;
        while k <= n {
            s.add(binom_pmf(k, n, p));
            if s.total() >= alpha * (1.0 - 1e-14) {
                return Ok(k);
            }
            k += 1.0;
        }
        Ok(n)
    })
}

fn negbinom_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[f, s, p, cum]| {
        let (f, s, p) = (as_num(f)?.trunc(), as_num(s)?.trunc(), as_num(p)?);
        let cum = cum.coerce_bool()?;
        if f < 0.0 || s < 1.0 || !(0.0..=1.0).contains(&p) {
            return Err(ErrorKind::Num);
        }
        if cum {
            return Ok(num_result(beta_reg(s, f + 1.0, p).0));
        }
        Ok(num_result(super::math::binomial(f + s - 1.0, s - 1.0) * p.powf(s) * (1.0 - p).powf(f)))
    })
}

fn negbinomdist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[f, s, p]| {
        let (f, s) = (f.trunc(), s.trunc());
        if f < 0.0 || s < 1.0 || !(0.0..=1.0).contains(&p) {
            return Err(ErrorKind::Num);
        }
        Ok(super::math::binomial(f + s - 1.0, s - 1.0) * p.powf(s) * (1.0 - p).powf(f))
    })
}

fn hypgeom_pmf(k: f64, n: f64, big_k: f64, big_n: f64) -> f64 {
    (ln_choose(big_k, k) + ln_choose(big_n - big_k, n - k) - ln_choose(big_n, n)).exp()
}

fn hypgeom(k: f64, n: f64, big_k: f64, big_n: f64, cum: bool) -> Result<f64, ErrorKind> {
    let (k, n, big_k, big_n) = (k.trunc(), n.trunc(), big_k.trunc(), big_n.trunc());
    if k < 0.0 || k > n.min(big_k) || k < (n - big_n + big_k).max(0.0) || n <= 0.0 || n > big_n || big_k <= 0.0 || big_k > big_n || big_n <= 0.0 {
        return Err(ErrorKind::Num);
    }
    if !cum {
        return Ok(hypgeom_pmf(k, n, big_k, big_n));
    }
    let lo = (n - big_n + big_k).max(0.0);
    let mut s = Sum::default();
    let mut i = lo;
    while i <= k {
        s.add(hypgeom_pmf(i, n, big_k, big_n));
        i += 1.0;
    }
    Ok(s.total().min(1.0))
}

fn hypgeom_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req, Req], |[s, ns, ps, np, cum]| {
        let v = hypgeom(as_num(s)?, as_num(ns)?, as_num(ps)?, as_num(np)?, cum.coerce_bool()?)?;
        Ok(num_result(v))
    })
}

fn hypgeomdist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req, Req], |[s, ns, ps, np]| hypgeom(s, ns, ps, np, false))
}

// ---- t, chi-square, F ----------------------------------------------------------------------------------

fn t_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req], |[x, df, cum]| {
        let (x, df) = (as_num(x)?, as_num(df)?.trunc());
        let cum = cum.coerce_bool()?;
        if df < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(num_result(if cum { t_cdf(x, df) } else { t_pdf(x, df) }))
    })
}

fn t_dist_2t(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, df]| {
        let df = df.trunc();
        if x < 0.0 || df < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok((2.0 * t_upper(x, df)).min(1.0))
    })
}

fn t_dist_rt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, df]| {
        let df = df.trunc();
        if df < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(if x >= 0.0 { t_upper(x, df) } else { 1.0 - t_upper(-x, df) })
    })
}

fn tdist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[x, df, tails]| {
        let (df, tails) = (df.trunc(), tails.trunc());
        if x < 0.0 || df < 1.0 || !(tails == 1.0 || tails == 2.0) {
            return Err(ErrorKind::Num);
        }
        Ok((tails * t_upper(x, df)).min(1.0))
    })
}

fn t_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[p, df]| {
        let df = df.trunc();
        if p <= 0.0 || p >= 1.0 || df < 1.0 {
            return Err(ErrorKind::Num);
        }
        if p == 0.5 {
            return Ok(0.0);
        }
        Ok(if p < 0.5 { -t_inv_two_tailed(2.0 * p, df) } else { t_inv_two_tailed(2.0 * (1.0 - p), df) })
    })
}

fn t_inv_2t(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[p, df]| {
        let df = df.trunc();
        if p <= 0.0 || p > 1.0 || df < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(t_inv_two_tailed(p, df))
    })
}

fn chisq_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req], |[x, df, cum]| {
        let (x, df) = (as_num(x)?, as_num(df)?.trunc());
        let cum = cum.coerce_bool()?;
        if x < 0.0 || !(1.0..=1e10).contains(&df) {
            return Err(ErrorKind::Num);
        }
        Ok(num_result(if cum { chisq_cdf(x, df).0 } else { chisq_pdf(x, df) }))
    })
}

fn chisq_dist_rt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[x, df]| {
        let df = df.trunc();
        if x < 0.0 || !(1.0..=1e10).contains(&df) {
            return Err(ErrorKind::Num);
        }
        Ok(chisq_cdf(x, df).1)
    })
}

fn chisq_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[p, df]| {
        let df = df.trunc();
        if !(0.0..=1.0).contains(&p) || !(1.0..=1e10).contains(&df) {
            return Err(ErrorKind::Num);
        }
        Ok(2.0 * gamma_p_inv(p, df / 2.0))
    })
}

fn chisq_inv_rt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req], |[p, df]| {
        let df = df.trunc();
        if !(0.0..=1.0).contains(&p) || !(1.0..=1e10).contains(&df) {
            return Err(ErrorKind::Num);
        }
        Ok(2.0 * gamma_p_inv(1.0 - p, df / 2.0))
    })
}

fn f_args(x: f64, d1: f64, d2: f64) -> Result<(f64, f64), ErrorKind> {
    let (d1, d2) = (d1.trunc(), d2.trunc());
    if x < 0.0 || !(1.0..1e10).contains(&d1) || !(1.0..1e10).contains(&d2) {
        return Err(ErrorKind::Num);
    }
    Ok((d1, d2))
}

fn f_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[x, d1, d2, cum]| {
        let x = as_num(x)?;
        let (d1, d2) = f_args(x, as_num(d1)?, as_num(d2)?)?;
        Ok(num_result(if cum.coerce_bool()? { f_cdf(x, d1, d2).0 } else { f_pdf(x, d1, d2) }))
    })
}

fn f_dist_rt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[x, d1, d2]| {
        let (d1, d2) = f_args(x, d1, d2)?;
        Ok(f_cdf(x, d1, d2).1)
    })
}

fn f_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[p, d1, d2]| {
        let (d1, d2) = f_args(0.0, d1, d2)?;
        if !(0.0..=1.0).contains(&p) {
            return Err(ErrorKind::Num);
        }
        Ok(f_inv_lower(p, d1, d2))
    })
}

fn f_inv_rt(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[p, d1, d2]| {
        let (d1, d2) = f_args(0.0, d1, d2)?;
        if !(0.0..=1.0).contains(&p) {
            return Err(ErrorKind::Num);
        }
        // F with right tail p is 1/F(d2, d1) with left tail p.
        let y = beta_reg_inv(p, d2 / 2.0, d1 / 2.0);
        if y <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(d2 * (1.0 - y) / (d1 * y))
    })
}

// ---- gamma and beta ------------------------------------------------------------------------------------

fn gamma_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req], |[x, alpha, beta, cum]| {
        let (x, a, b) = (as_num(x)?, as_num(alpha)?, as_num(beta)?);
        let cum = cum.coerce_bool()?;
        if x < 0.0 || a <= 0.0 || b <= 0.0 {
            return Err(ErrorKind::Num);
        }
        if cum {
            return Ok(num_result(gamma_pq(a, x / b).0));
        }
        if x == 0.0 {
            return Ok(Value::Number(if a == 1.0 { 1.0 / b } else { 0.0 }));
        }
        Ok(num_result(((a - 1.0) * x.ln() - x / b - ln_gamma(a) - a * b.ln()).exp()))
    })
}

fn gamma_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[p, a, b]| {
        if !(0.0..1.0).contains(&p) || a <= 0.0 || b <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(b * gamma_p_inv(p, a))
    })
}

fn gamma_fn(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| {
        if x <= 0.0 && x == x.floor() {
            return Err(ErrorKind::Num);
        }
        Ok(clean15(gamma(x)))
    })
}

fn gammaln(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req], |[x]| if x <= 0.0 { Err(ErrorKind::Num) } else { Ok(ln_gamma(x)) })
}

fn beta_args(alpha: f64, beta: f64, a: f64, b: f64) -> Result<(), ErrorKind> {
    if alpha <= 0.0 || beta <= 0.0 || a >= b {
        return Err(ErrorKind::Num);
    }
    Ok(())
}

fn beta_dist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted(ctx, args, [Req, Req, Req, Req, Opt(n(0.0)), Opt(n(1.0))], |[x, alpha, beta, cum, a, b]| {
        let (x, al, be, a, b) = (as_num(x)?, as_num(alpha)?, as_num(beta)?, as_num(a)?, as_num(b)?);
        let cum = cum.coerce_bool()?;
        beta_args(al, be, a, b)?;
        if x < a || x > b {
            return Err(ErrorKind::Num);
        }
        let t = (x - a) / (b - a);
        if cum {
            return Ok(num_result(beta_reg(al, be, t).0));
        }
        let ln_b = ln_gamma(al) + ln_gamma(be) - ln_gamma(al + be);
        Ok(num_result(((al - 1.0) * t.ln() + (be - 1.0) * (1.0 - t).ln() - ln_b).exp() / (b - a)))
    })
}

fn betadist(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req, Opt(n(0.0)), Opt(n(1.0))], |[x, al, be, a, b]| {
        beta_args(al, be, a, b)?;
        if x < a || x > b {
            return Err(ErrorKind::Num);
        }
        Ok(beta_reg(al, be, (x - a) / (b - a)).0)
    })
}

fn beta_inv(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req, Opt(n(0.0)), Opt(n(1.0))], |[p, al, be, a, b]| {
        beta_args(al, be, a, b)?;
        if p <= 0.0 || p > 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(a + (b - a) * beta_reg_inv(p, al, be))
    })
}

// ---- confidence and tests ------------------------------------------------------------------------------

fn confidence_norm(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[alpha, sd, size]| {
        let size = size.trunc();
        if alpha <= 0.0 || alpha >= 1.0 || sd <= 0.0 || size < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(norm_inv_std(1.0 - alpha / 2.0) * sd / size.sqrt())
    })
}

fn confidence_t(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    lifted_num(ctx, args, [Req, Req, Req], |[alpha, sd, size]| {
        let size = size.trunc();
        if alpha <= 0.0 || alpha >= 1.0 || sd <= 0.0 || size < 1.0 {
            return Err(ErrorKind::Num);
        }
        if size == 1.0 {
            return Err(ErrorKind::Div0);
        }
        Ok(t_inv_two_tailed(alpha, size - 1.0) * sd / size.sqrt())
    })
}

fn mean_var(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = kahan(v) / n;
    let mut s = Sum::default();
    for x in v {
        s.add((x - m) * (x - m));
    }
    (m, s.total() / (n - 1.0))
}

fn t_test(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 4 {
        return bad_args();
    }
    let tails = num!(ctx, &args[2]).trunc();
    let kind = num!(ctx, &args[3]).trunc();
    if !(tails == 1.0 || tails == 2.0) || !(1.0..=3.0).contains(&kind) {
        return Res::err(ErrorKind::Num);
    }
    let (t, df) = if kind == 1.0 {
        let (ra, rb) = (ctx.eval(&args[0]), ctx.eval(&args[1]));
        let (a, b) = (ctx.to_array(&ra), ctx.to_array(&rb));
        if a.data.len() != b.data.len() {
            return Res::err(ErrorKind::NA);
        }
        let mut d = Vec::new();
        for (x, y) in a.data.iter().zip(&b.data) {
            match (x, y) {
                (Value::Error(e), _) | (_, Value::Error(e)) => return Res::err(*e),
                (Value::Number(x), Value::Number(y)) => d.push(x - y),
                _ => {}
            }
        }
        if d.len() < 2 {
            return Res::err(ErrorKind::Div0);
        }
        let (m, var) = mean_var(&d);
        if var == 0.0 {
            return Res::err(ErrorKind::Div0);
        }
        (m / (var / d.len() as f64).sqrt(), d.len() as f64 - 1.0)
    } else {
        let a = tri!(data_numbers(ctx, &args[0]));
        let b = tri!(data_numbers(ctx, &args[1]));
        if a.len() < 2 || b.len() < 2 {
            return Res::err(ErrorKind::Div0);
        }
        let (n1, n2) = (a.len() as f64, b.len() as f64);
        let ((m1, v1), (m2, v2)) = (mean_var(&a), mean_var(&b));
        if kind == 2.0 {
            let sp = ((n1 - 1.0) * v1 + (n2 - 1.0) * v2) / (n1 + n2 - 2.0);
            if sp == 0.0 {
                return Res::err(ErrorKind::Div0);
            }
            ((m1 - m2) / (sp * (1.0 / n1 + 1.0 / n2)).sqrt(), n1 + n2 - 2.0)
        } else {
            let (q1, q2) = (v1 / n1, v2 / n2);
            if q1 + q2 == 0.0 {
                return Res::err(ErrorKind::Div0);
            }
            let df = (q1 + q2).powi(2) / (q1 * q1 / (n1 - 1.0) + q2 * q2 / (n2 - 1.0));
            ((m1 - m2) / (q1 + q2).sqrt(), df)
        }
    };
    Res::num((tails * t_upper(t.abs(), df)).min(1.0))
}

fn chisq_test(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let (ra, re) = (ctx.eval(&args[0]), ctx.eval(&args[1]));
    let (a, e) = (ctx.to_array(&ra), ctx.to_array(&re));
    if a.rows != e.rows || a.cols != e.cols {
        return Res::err(ErrorKind::NA);
    }
    let mut chi = Sum::default();
    for (x, y) in a.data.iter().zip(&e.data) {
        match (x, y) {
            (Value::Error(err), _) | (_, Value::Error(err)) => return Res::err(*err),
            (Value::Number(x), Value::Number(y)) => {
                if *y == 0.0 {
                    return Res::err(ErrorKind::Div0);
                }
                chi.add((x - y) * (x - y) / y);
            }
            _ => {}
        }
    }
    let (r, c) = (a.rows as f64, a.cols as f64);
    let df = if r > 1.0 && c > 1.0 { (r - 1.0) * (c - 1.0) } else { r.max(c) - 1.0 };
    if df < 1.0 {
        return Res::err(ErrorKind::NA);
    }
    Res::num(chisq_cdf(chi.total(), df).1)
}

fn f_test(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if args.len() != 2 {
        return bad_args();
    }
    let a = tri!(data_numbers(ctx, &args[0]));
    let b = tri!(data_numbers(ctx, &args[1]));
    if a.len() < 2 || b.len() < 2 {
        return Res::err(ErrorKind::Div0);
    }
    let ((_, v1), (_, v2)) = (mean_var(&a), mean_var(&b));
    if v1 == 0.0 || v2 == 0.0 {
        return Res::err(ErrorKind::Div0);
    }
    let f = v1 / v2;
    let (lo, hi) = f_cdf(f, a.len() as f64 - 1.0, b.len() as f64 - 1.0);
    Res::num((2.0 * lo.min(hi)).min(1.0))
}

fn z_test(ctx: &mut Ctx<'_>, args: &[Expr]) -> Res {
    if !arity(args, 2, 3) {
        return bad_args();
    }
    let v = tri!(data_numbers(ctx, &args[0]));
    let x = num!(ctx, &args[1]);
    if v.is_empty() {
        return Res::err(ErrorKind::NA);
    }
    let n = v.len() as f64;
    let sigma = match opt(args, 2) {
        Some(e) => num!(ctx, e),
        None => {
            if v.len() < 2 {
                return Res::err(ErrorKind::Div0);
            }
            mean_var(&v).1.sqrt()
        }
    };
    if sigma <= 0.0 {
        return Res::err(if sigma == 0.0 { ErrorKind::Div0 } else { ErrorKind::Num });
    }
    let m = kahan(&v) / n;
    let z = (m - x) / (sigma / n.sqrt());
    Res::num(0.5 * erf_erfc(z / SQRT_2).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1e-300)
    }

    #[test]
    fn special_functions() {
        assert!(close(gamma(2.5), 1.329_340_388_179_137, 1e-14));
        assert!(close(gamma(-3.75), 0.267_866_128_861_417_5, 1e-12));
        assert!(close(ln_gamma(4.0), 6f64.ln(), 1e-14));
        assert!(close(erf_erfc(0.5).0, 0.520_499_877_813_046_5, 1e-15));
        assert!(close(erf_erfc(3.0).1, 2.209_049_699_858_544e-5, 1e-13));
        assert!(close(erf_erfc(1.5).1, 0.033_894_853_524_689_27, 1e-13));
        assert!(close(norm_cdf(1.333_333), 0.908_788_726, 1e-9));
        assert!(close(norm_inv_std(0.908_789), 1.333_334_673, 1e-8));
        assert!(close(norm_inv_std(1e-10), -6.361_340_902_404_056, 1e-12));
        let (p, q) = gamma_pq(3.0, 2.0);
        assert!(close(p, 0.323_323_583_816_936_5, 1e-14));
        assert!(close(q, 0.676_676_416_183_063_5, 1e-14));
        assert!(close(gamma_p_inv(p, 3.0), 2.0, 1e-12));
        let (lo, _) = beta_reg(2.0, 3.0, 0.4);
        assert!(close(lo, 0.5248, 1e-13));
        assert!(close(beta_reg_inv(0.5248, 2.0, 3.0), 0.4, 1e-12));
    }
}
