//! Statistical special functions and the Excel distribution family.
//!
//! The building blocks are the log-gamma function, the regularized incomplete
//! gamma `P(a,x)` and incomplete beta `I_x(a,b)`, and the inverse normal CDF —
//! everything Excel's NORM/GAMMA/CHISQ/BETA/F/T/BINOM/… functions reduce to.
//! Distribution wrappers return `None` on a domain error (the caller maps that to
//! `#NUM!`). Continuous inverses use bisection on the CDF, which is robust and
//! matches Excel to well past display precision.

use std::f64::consts::PI;

/// Lanczos log-gamma (g = 7), valid for x > 0; reflection handles x < 0.5.
pub fn lgamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_1,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311e-7,
    ];
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1-x) = π / sin(πx).
        (PI / (PI * x).sin()).ln() - lgamma(1.0 - x)
    } else {
        let x = x - 1.0;
        let mut a = C[0];
        let t = x + G + 0.5;
        for (i, &c) in C.iter().enumerate().skip(1) {
            a += c / (x + i as f64);
        }
        0.5 * (2.0 * PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
    }
}

/// Γ(x). Errors (returns `None`) at zero and negative integers.
pub fn gamma(x: f64) -> Option<f64> {
    if x > 0.0 {
        Some(lgamma(x).exp())
    } else if x.fract() == 0.0 {
        None // pole at 0 and negative integers
    } else {
        // Reflection keeps the sign for negative non-integers.
        let g = PI / ((PI * x).sin() * lgamma(1.0 - x).exp());
        Some(g)
    }
}

/// Regularized lower incomplete gamma `P(a, x)` (= `GAMMADIST` cumulative core).
pub fn gammp(a: f64, x: f64) -> f64 {
    if x < 0.0 || a <= 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gser(a, x)
    } else {
        1.0 - gcf(a, x)
    }
}

/// Regularized upper incomplete gamma `Q(a, x) = 1 - P(a, x)`.
pub fn gammq(a: f64, x: f64) -> f64 {
    1.0 - gammp(a, x)
}

/// Series expansion for `P(a, x)`, good for x < a+1.
fn gser(a: f64, x: f64) -> f64 {
    let gln = lgamma(a);
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..1000 {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * 1e-15 {
            break;
        }
    }
    sum * (-x + a * x.ln() - gln).exp()
}

/// Continued fraction for `Q(a, x)`, good for x ≥ a+1.
fn gcf(a: f64, x: f64) -> f64 {
    let gln = lgamma(a);
    let tiny = 1e-300;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / tiny;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..1000 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < tiny {
            d = tiny;
        }
        c = b + an / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    (-x + a * x.ln() - gln).exp() * h
}

/// Regularized incomplete beta `I_x(a, b)`.
pub fn betai(a: f64, b: f64, x: f64) -> f64 {
    if !(0.0..=1.0).contains(&x) || a <= 0.0 || b <= 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x == 1.0 {
        return x;
    }
    let bt = (lgamma(a + b) - lgamma(a) - lgamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(a, b, x) / a
    } else {
        1.0 - bt * betacf(b, a, 1.0 - x) / b
    }
}

/// Continued fraction for the incomplete beta function (Lentz's method).
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    let tiny = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < tiny {
        d = tiny;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..1000 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = 1.0 + aa / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// The error function, built from the incomplete gamma.
pub fn erf(x: f64) -> f64 {
    if x < 0.0 {
        -gammp(0.5, x * x)
    } else {
        gammp(0.5, x * x)
    }
}

pub fn erfc(x: f64) -> f64 {
    1.0 - erf(x)
}

/// Standard normal PDF.
pub fn norm_pdf(z: f64) -> f64 {
    (-0.5 * z * z).exp() / (2.0 * PI).sqrt()
}

/// Standard normal CDF.
pub fn norm_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / std::f64::consts::SQRT_2)
}

/// Inverse standard normal CDF (Acklam's rational approximation + one Halley
/// step), accurate to ~1e-15 across (0, 1).
pub fn norm_inv(p: f64) -> Option<f64> {
    if p <= 0.0 || p >= 1.0 {
        return None;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let plow = 0.02425;
    let phigh = 1.0 - plow;
    let mut x = if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= phigh {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // One Halley refinement.
    let e = norm_cdf(x) - p;
    let u = e * (2.0 * PI).sqrt() * (x * x / 2.0).exp();
    x -= u / (1.0 + x * u / 2.0);
    Some(x)
}

/// Bisection inverse of a monotonically-increasing CDF on `[lo, hi]`.
pub fn invert_cdf(target: f64, lo: f64, hi: f64, cdf: impl Fn(f64) -> f64) -> Option<f64> {
    if !(0.0..=1.0).contains(&target) {
        return None;
    }
    let (mut lo, mut hi) = (lo, hi);
    // Expand the upper bound until it brackets the target.
    let mut guard = 0;
    while cdf(hi) < target && guard < 200 {
        hi *= 2.0;
        guard += 1;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if cdf(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
        if (hi - lo).abs() < 1e-12 * (1.0 + mid_abs(lo, hi)) {
            break;
        }
    }
    Some(0.5 * (lo + hi))
}

fn mid_abs(lo: f64, hi: f64) -> f64 {
    (0.5 * (lo + hi)).abs()
}

// --- distribution CDFs/PDFs shared by several Excel functions ----------------

/// Gamma distribution: PDF or CDF at `x` with shape `a`, scale `b`.
pub fn gamma_dist(x: f64, a: f64, b: f64, cumulative: bool) -> Option<f64> {
    if x < 0.0 || a <= 0.0 || b <= 0.0 {
        return None;
    }
    if cumulative {
        Some(gammp(a, x / b))
    } else if x == 0.0 {
        Some(if a < 1.0 {
            f64::INFINITY
        } else if a == 1.0 {
            1.0 / b
        } else {
            0.0
        })
    } else {
        Some(((a - 1.0) * x.ln() - x / b - a * b.ln() - lgamma(a)).exp())
    }
}

/// Beta distribution CDF/PDF on a general interval `[lo, hi]`.
pub fn beta_dist(x: f64, a: f64, b: f64, cumulative: bool, lo: f64, hi: f64) -> Option<f64> {
    if a <= 0.0 || b <= 0.0 || hi <= lo || x < lo || x > hi {
        return None;
    }
    let z = (x - lo) / (hi - lo);
    if cumulative {
        Some(betai(a, b, z))
    } else {
        let d = hi - lo;
        Some(
            ((a - 1.0) * z.ln() + (b - 1.0) * (1.0 - z).ln() + lgamma(a + b)
                - lgamma(a)
                - lgamma(b))
            .exp()
                / d,
        )
    }
}

/// Student-t CDF at `t` with `df` degrees of freedom.
pub fn t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let ib = 0.5 * betai(df / 2.0, 0.5, x);
    if t >= 0.0 { 1.0 - ib } else { ib }
}

/// F CDF at `x` with `d1`, `d2` degrees of freedom.
pub fn f_cdf(x: f64, d1: f64, d2: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    betai(d1 / 2.0, d2 / 2.0, d1 * x / (d1 * x + d2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * (1.0 + b.abs())
    }

    #[test]
    fn special_functions_match_known_values() {
        assert!(close(lgamma(5.0), (24.0f64).ln(), 1e-12)); // Γ(5)=4!=24
        assert!(close(gamma(0.5).unwrap(), PI.sqrt(), 1e-12)); // Γ(½)=√π
        assert!(close(erf(1.0), 0.842_700_792_949_715, 1e-10));
        assert!(close(norm_cdf(0.0), 0.5, 1e-12));
        assert!(close(norm_cdf(1.96), 0.975_002_104_851_780, 1e-9));
        assert!(close(norm_inv(0.975).unwrap(), 1.959_963_984_540_054, 1e-9));
    }

    #[test]
    fn incomplete_functions() {
        // P(1, x) = 1 - e^-x (exponential CDF).
        assert!(close(gammp(1.0, 2.0), 1.0 - (-2.0f64).exp(), 1e-12));
        // I_x(1,1) = x.
        assert!(close(betai(1.0, 1.0, 0.3), 0.3, 1e-12));
        // Symmetry: t_cdf(0) = 0.5.
        assert!(close(t_cdf(0.0, 10.0), 0.5, 1e-12));
    }

    #[test]
    fn distributions_and_inverses_round_trip() {
        // CHISQ.DIST(3.84, 1, TRUE) ≈ 0.95 (the classic 1-df 5% critical value).
        let p = gamma_dist(3.841_458_820_694_124, 0.5, 2.0, true).unwrap();
        assert!(close(p, 0.95, 1e-6));
        // Invert it back.
        let x = invert_cdf(0.95, 0.0, 10.0, |x| gamma_dist(x, 0.5, 2.0, true).unwrap()).unwrap();
        assert!(close(x, 3.841_458_820_694_124, 1e-6));
    }

    #[test]
    fn lgamma_and_gamma_edge_cases() {
        // Γ(1)=Γ(2)=1 → lgamma = 0 at both integers.
        assert!(close(lgamma(1.0), 0.0, 1e-12));
        assert!(close(lgamma(2.0), 0.0, 1e-12));
        // Reflection branch (x < 0.5): Γ(¼) = 3.625_609_908_221_908.
        assert!(close(lgamma(0.25), 3.625_609_908_221_908_f64.ln(), 1e-11));
        // Factorials: Γ(n) = (n-1)!.
        assert!(close(gamma(5.0).unwrap(), 24.0, 1e-9)); // 4!
        assert!(close(gamma(6.0).unwrap(), 120.0, 1e-9)); // 5!
        assert!(close(gamma(1.0).unwrap(), 1.0, 1e-12));
        // Negative non-integer via reflection: Γ(-½) = -2√π.
        assert!(close(gamma(-0.5).unwrap(), -2.0 * PI.sqrt(), 1e-10));
        // Poles at 0 and negative integers → None.
        assert!(gamma(0.0).is_none());
        assert!(gamma(-1.0).is_none());
        assert!(gamma(-3.0).is_none());
    }

    #[test]
    fn incomplete_gamma_complementary_and_domain() {
        // P(a,x) + Q(a,x) = 1 for valid arguments.
        assert!(close(gammp(2.5, 3.0) + gammq(2.5, 3.0), 1.0, 1e-12));
        // Both series (x < a+1) and continued-fraction (x ≥ a+1) branches.
        assert!(close(gammp(2.0, 1.0) + gammq(2.0, 1.0), 1.0, 1e-12));
        assert!(close(gammp(2.0, 8.0) + gammq(2.0, 8.0), 1.0, 1e-12));
        // P(2, x) = 1 - (1+x)e^-x (Erlang-2 CDF).
        let x = 3.0;
        assert!(close(gammp(2.0, x), 1.0 - (1.0 + x) * (-x).exp(), 1e-12));
        // x == 0 → 0; domain errors → NaN.
        assert!(close(gammp(3.0, 0.0), 0.0, 1e-15));
        assert!(gammp(3.0, -1.0).is_nan());
        assert!(gammp(0.0, 1.0).is_nan());
    }

    #[test]
    fn incomplete_beta_symmetry_and_bounds() {
        // Symmetry: I_x(a,b) = 1 - I_{1-x}(b,a).
        assert!(close(
            betai(2.0, 3.0, 0.4),
            1.0 - betai(3.0, 2.0, 0.6),
            1e-12
        ));
        // I_x(2,2) = 3x² - 2x³ closed form.
        let x = 0.35;
        assert!(close(
            betai(2.0, 2.0, x),
            3.0 * x * x - 2.0 * x * x * x,
            1e-12
        ));
        // Boundaries return x exactly; out-of-domain → NaN.
        assert!(close(betai(2.0, 5.0, 0.0), 0.0, 1e-15));
        assert!(close(betai(2.0, 5.0, 1.0), 1.0, 1e-15));
        assert!(betai(2.0, 5.0, 1.5).is_nan());
        assert!(betai(0.0, 5.0, 0.5).is_nan());
    }

    #[test]
    fn erf_erfc_and_normal_symmetry() {
        // erf is odd, erf(0)=0, erfc(0)=1.
        assert!(close(erf(0.0), 0.0, 1e-15));
        assert!(close(erf(-1.0), -erf(1.0), 1e-12));
        assert!(close(erfc(0.0), 1.0, 1e-12));
        assert!(close(erf(1.0) + erfc(1.0), 1.0, 1e-12));
        // Normal PDF peak and symmetry.
        assert!(close(norm_pdf(0.0), 1.0 / (2.0 * PI).sqrt(), 1e-15));
        assert!(close(norm_pdf(1.0), 0.241_970_724_519_143, 1e-12));
        assert!(close(norm_pdf(-2.3), norm_pdf(2.3), 1e-15));
        // CDF reflection: Φ(z) + Φ(-z) = 1.
        assert!(close(norm_cdf(1.3) + norm_cdf(-1.3), 1.0, 1e-12));
    }

    #[test]
    fn norm_inv_edges_and_round_trip() {
        // Out of the open interval → None.
        assert!(norm_inv(0.0).is_none());
        assert!(norm_inv(1.0).is_none());
        assert!(norm_inv(-0.1).is_none());
        // Median is 0.
        assert!(close(norm_inv(0.5).unwrap(), 0.0, 1e-12));
        // Round-trip through the CDF across all three approximation branches.
        for &p in &[0.001_f64, 0.02, 0.25, 0.5, 0.8, 0.98, 0.999] {
            let z = norm_inv(p).unwrap();
            assert!(close(norm_cdf(z), p, 1e-9));
        }
    }

    #[test]
    fn invert_cdf_bounds_and_bracket_expansion() {
        // Target outside [0,1] → None.
        assert!(invert_cdf(-0.1, 0.0, 1.0, |_| 0.5).is_none());
        assert!(invert_cdf(1.1, 0.0, 1.0, |_| 0.5).is_none());
        // Exponential CDF 1 - e^-x: invert 0.5 → ln 2. The upper bound (1.0)
        // starts below the target's x, forcing the doubling expansion.
        let x = invert_cdf(0.5, 0.0, 1.0, |x: f64| 1.0 - (-x).exp()).unwrap();
        assert!(close(x, 2.0_f64.ln(), 1e-9));
    }

    #[test]
    fn gamma_dist_pdf_cdf_and_domain() {
        // a=1 is the exponential: CDF = 1 - e^{-x/b}, PDF = e^{-x/b}/b.
        assert!(close(
            gamma_dist(2.0, 1.0, 1.0, true).unwrap(),
            1.0 - (-2.0_f64).exp(),
            1e-12
        ));
        assert!(close(
            gamma_dist(2.0, 1.0, 3.0, false).unwrap(),
            (-2.0_f64 / 3.0).exp() / 3.0,
            1e-12
        ));
        // PDF at x=0 depends on the shape.
        assert!(gamma_dist(0.0, 0.5, 2.0, false).unwrap().is_infinite()); // a<1
        assert!(close(
            gamma_dist(0.0, 1.0, 4.0, false).unwrap(),
            0.25,
            1e-15
        )); // a=1 → 1/b
        assert!(close(gamma_dist(0.0, 2.0, 4.0, false).unwrap(), 0.0, 1e-15)); // a>1
        // Domain errors → None.
        assert!(gamma_dist(-1.0, 2.0, 2.0, true).is_none());
        assert!(gamma_dist(1.0, 0.0, 2.0, true).is_none());
        assert!(gamma_dist(1.0, 2.0, 0.0, true).is_none());
    }

    #[test]
    fn beta_dist_uniform_and_general_interval() {
        // a=b=1 on [0,1] is uniform: CDF = z, PDF = 1.
        assert!(close(
            beta_dist(0.3, 1.0, 1.0, true, 0.0, 1.0).unwrap(),
            0.3,
            1e-12
        ));
        assert!(close(
            beta_dist(0.5, 1.0, 1.0, false, 0.0, 1.0).unwrap(),
            1.0,
            1e-12
        ));
        // General interval rescales: uniform on [2,4], CDF at x=3 is 0.5.
        assert!(close(
            beta_dist(3.0, 1.0, 1.0, true, 2.0, 4.0).unwrap(),
            0.5,
            1e-12
        ));
        // On [2,4] the uniform PDF is 1/(4-2) = 0.5.
        assert!(close(
            beta_dist(3.0, 1.0, 1.0, false, 2.0, 4.0).unwrap(),
            0.5,
            1e-12
        ));
        // Matches the plain incomplete beta on the unit interval.
        assert!(close(
            beta_dist(0.4, 2.0, 3.0, true, 0.0, 1.0).unwrap(),
            betai(2.0, 3.0, 0.4),
            1e-12
        ));
        // Domain errors → None.
        assert!(beta_dist(0.5, 0.0, 3.0, true, 0.0, 1.0).is_none());
        assert!(beta_dist(0.5, 2.0, 3.0, true, 1.0, 1.0).is_none()); // hi <= lo
        assert!(beta_dist(5.0, 2.0, 3.0, true, 0.0, 1.0).is_none()); // x > hi
    }

    #[test]
    fn t_and_f_cdf_symmetry_and_known_values() {
        // Student-t is symmetric: T(t) + T(-t) = 1.
        assert!(close(t_cdf(1.3, 7.0) + t_cdf(-1.3, 7.0), 1.0, 1e-12));
        // df=1 is the standard Cauchy: CDF(1) = 0.5 + arctan(1)/π = 0.75.
        assert!(close(t_cdf(1.0, 1.0), 0.75, 1e-12));
        // F(d,d) has median 1 by symmetry.
        assert!(close(f_cdf(1.0, 5.0, 5.0), 0.5, 1e-12));
        assert!(close(f_cdf(1.0, 3.0, 3.0), 0.5, 1e-12));
        // Non-positive x → 0.
        assert!(close(f_cdf(0.0, 4.0, 6.0), 0.0, 1e-15));
        assert!(close(f_cdf(-2.0, 4.0, 6.0), 0.0, 1e-15));
    }
}
