//! Paired evaluation statistics (AGE-24).
//!
//! Always report a paired statistic + confidence interval, never a bare delta.
//! Print the minimum detectable effect (MDE) for the chosen n so underpowered
//! nulls are distinguishable from true nulls.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use statrs::distribution::{ChiSquared, ContinuousCDF};

/// Per-item binary outcome for two arms on the same item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryOutcome {
    pub arm_a_correct: bool,
    pub arm_b_correct: bool,
}

/// Per-item continuous scores for two arms on the same item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinuousOutcome {
    pub arm_a: f64,
    pub arm_b: f64,
}

/// McNemar test on discordant pairs (exact mid-p approximation via χ² with continuity).
#[derive(Debug, Clone, PartialEq)]
pub struct McNemarResult {
    /// Items where A correct and B wrong.
    pub b: usize,
    /// Items where B correct and A wrong.
    pub c: usize,
    /// χ² statistic with continuity correction.
    pub chi2: f64,
    /// Two-sided p-value.
    pub p_value: f64,
    /// Arm A accuracy.
    pub accuracy_a: f64,
    /// Arm B accuracy.
    pub accuracy_b: f64,
    /// Accuracy(B) − Accuracy(A).
    pub delta: f64,
}

/// Run McNemar on paired binary outcomes. Panics not used — returns None if empty.
pub fn mcnemar(outcomes: &[BinaryOutcome]) -> Option<McNemarResult> {
    if outcomes.is_empty() {
        return None;
    }
    let n = outcomes.len() as f64;
    let mut b = 0usize;
    let mut c = 0usize;
    let mut correct_a = 0usize;
    let mut correct_b = 0usize;
    for o in outcomes {
        if o.arm_a_correct {
            correct_a += 1;
        }
        if o.arm_b_correct {
            correct_b += 1;
        }
        match (o.arm_a_correct, o.arm_b_correct) {
            (true, false) => b += 1,
            (false, true) => c += 1,
            _ => {}
        }
    }
    let discordant = (b + c) as f64;
    let (chi2, p_value) = if discordant == 0.0 {
        (0.0, 1.0)
    } else {
        let diff = (b as f64 - c as f64).abs() - 1.0;
        let chi2 = if diff < 0.0 {
            0.0
        } else {
            (diff * diff) / discordant
        };
        let dist = ChiSquared::new(1.0).ok()?;
        let p = 1.0 - dist.cdf(chi2);
        (chi2, p)
    };
    Some(McNemarResult {
        b,
        c,
        chi2,
        p_value,
        accuracy_a: correct_a as f64 / n,
        accuracy_b: correct_b as f64 / n,
        delta: (correct_b as f64 - correct_a as f64) / n,
    })
}

/// Bootstrap result for the mean paired difference (B − A).
#[derive(Debug, Clone, PartialEq)]
pub struct PairedBootstrapResult {
    pub mean_delta: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n_bootstrap: usize,
    pub seed: u64,
}

/// Paired bootstrap of mean(B − A) with a percentile CI.
pub fn paired_bootstrap_mean_diff(
    outcomes: &[ContinuousOutcome],
    n_bootstrap: usize,
    seed: u64,
    ci_level: f64,
) -> Option<PairedBootstrapResult> {
    if outcomes.is_empty() || n_bootstrap == 0 || !(0.0..1.0).contains(&ci_level) {
        return None;
    }
    let n = outcomes.len();
    let diffs: Vec<f64> = outcomes.iter().map(|o| o.arm_b - o.arm_a).collect();
    let mean_delta = diffs.iter().sum::<f64>() / n as f64;

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut samples = Vec::with_capacity(n_bootstrap);
    for _ in 0..n_bootstrap {
        let mut sum = 0.0;
        for _ in 0..n {
            let idx = rng.gen_range(0..n);
            sum += diffs[idx];
        }
        samples.push(sum / n as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let alpha = 1.0 - ci_level;
    let lo_idx = ((alpha / 2.0) * (n_bootstrap as f64 - 1.0)).round() as usize;
    let hi_idx = ((1.0 - alpha / 2.0) * (n_bootstrap as f64 - 1.0)).round() as usize;
    Some(PairedBootstrapResult {
        mean_delta,
        ci_low: samples[lo_idx.min(n_bootstrap - 1)],
        ci_high: samples[hi_idx.min(n_bootstrap - 1)],
        n_bootstrap,
        seed,
    })
}

/// Minimum detectable effect for a two-sided paired binary comparison (approx).
#[derive(Debug, Clone, PartialEq)]
pub struct MinimumDetectableEffect {
    pub n: usize,
    /// Approximate MDE in absolute accuracy points (e.g. 0.05 = 5 points).
    pub mde: f64,
    pub alpha: f64,
    pub power: f64,
}

/// Approximate MDE for McNemar / paired proportion under equal discordant rates.
///
/// Uses a normal approximation: `mde ≈ z_(1-α/2) * sqrt(2 p_disc / n) / (1 − β factor)`,
/// with a fixed power z. Conservative default: assume 20% discordant pairs.
pub fn minimum_detectable_effect_binary(
    n: usize,
    alpha: f64,
    power: f64,
    discordant_rate: f64,
) -> Option<MinimumDetectableEffect> {
    if n == 0 || !(0.0..1.0).contains(&alpha) || !(0.0..1.0).contains(&power) {
        return None;
    }
    let z_alpha = standard_normal_quantile(1.0 - alpha / 2.0)?;
    let z_beta = standard_normal_quantile(power)?;
    let p_disc = discordant_rate.clamp(0.01, 1.0);
    let se = (2.0 * p_disc / n as f64).sqrt();
    let mde = (z_alpha + z_beta) * se;
    Some(MinimumDetectableEffect {
        n,
        mde,
        alpha,
        power,
    })
}

fn standard_normal_quantile(p: f64) -> Option<f64> {
    // Beasley-Springer-Moro approximation for Φ^{-1}.
    if !(0.0..=1.0).contains(&p) || p == 0.0 || p == 1.0 {
        return None;
    }
    let a = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    let b = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_407e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_367_919e1,
        -1.328_068_155_288_572e1,
    ];
    let c = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_039e-1,
        -2.400_758_277_161_838,
        -2.549_732_839_338_367,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    let d = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let plow = 0.02425;
    let phigh = 1.0 - plow;
    let q = if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= phigh {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    };
    Some(q)
}

/// Pretty-print a McNemar result including MDE for the sample size.
pub fn format_paired_binary_report(outcomes: &[BinaryOutcome]) -> String {
    let Some(res) = mcnemar(outcomes) else {
        return "no outcomes".into();
    };
    let mde = minimum_detectable_effect_binary(outcomes.len(), 0.05, 0.8, 0.2);
    let mde_s = mde
        .map(|m| format!("{:.4}", m.mde))
        .unwrap_or_else(|| "n/a".into());
    format!(
        "paired binary n={}  acc_a={:.4} acc_b={:.4} delta={:.4}  \
         McNemar χ²={:.4} p={:.4}  MDE(α=0.05,power=0.8)≈{}",
        outcomes.len(),
        res.accuracy_a,
        res.accuracy_b,
        res.delta,
        res.chi2,
        res.p_value,
        mde_s
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcnemar_detects_improvement() {
        let mut outcomes = Vec::new();
        for _ in 0..40 {
            outcomes.push(BinaryOutcome {
                arm_a_correct: false,
                arm_b_correct: true,
            });
        }
        for _ in 0..10 {
            outcomes.push(BinaryOutcome {
                arm_a_correct: true,
                arm_b_correct: true,
            });
        }
        let r = mcnemar(&outcomes).unwrap();
        assert!(r.delta > 0.0);
        assert!(r.p_value < 0.01);
    }

    #[test]
    fn bootstrap_is_seed_stable() {
        let outcomes: Vec<_> = (0..30)
            .map(|i| ContinuousOutcome {
                arm_a: i as f64 * 0.1,
                arm_b: i as f64 * 0.1 + 0.5,
            })
            .collect();
        let a = paired_bootstrap_mean_diff(&outcomes, 500, 7, 0.95).unwrap();
        let b = paired_bootstrap_mean_diff(&outcomes, 500, 7, 0.95).unwrap();
        assert_eq!(a, b);
        assert!((a.mean_delta - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mde_decreases_with_n() {
        let small = minimum_detectable_effect_binary(40, 0.05, 0.8, 0.2).unwrap();
        let large = minimum_detectable_effect_binary(400, 0.05, 0.8, 0.2).unwrap();
        assert!(large.mde < small.mde);
    }

    #[test]
    fn report_includes_mde() {
        let outcomes = vec![
            BinaryOutcome {
                arm_a_correct: true,
                arm_b_correct: true,
            };
            20
        ];
        let s = format_paired_binary_report(&outcomes);
        assert!(s.contains("MDE"));
    }
}
