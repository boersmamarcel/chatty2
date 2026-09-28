//! Benford's Law plugin — the two tools of a forensic Benford audit.
//!
//! A `chatty:plugin@0.3.0` plugin: it contributes tools, never a loop of its
//! own. The `benford-analyst` agent spec (a preset in chatty-core) loads it
//! and its model decides when to call them:
//!
//! | Tool | Input | Output |
//! |------|-------|--------|
//! | `compute_benford_distribution` | `numbers: [f64]` | observed & expected first-digit frequencies, deviation per digit |
//! | `chi_square_test` | `observed_counts: [u64]`, `total: u64` | χ² statistic, risk level (`LOW`/`MEDIUM`/`HIGH`), interpretation |
//!
//! Both tools are pure Rust with no host calls: they run deterministically
//! inside the WASM sandbox and request no capability.

use chatty_module_sdk::{
    export, Plugin, PluginMetadata, ToolCallRequest, ToolDefinition, ToolError, ToolResult,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Benford's Law expected first-digit frequencies (%) for digits 1–9.
/// Source: log₁₀(1 + 1/d)
const BENFORDS_EXPECTED: [f64; 9] = [
    30.103, // digit 1
    17.609, // digit 2
    12.494, // digit 3
    9.691,  // digit 4
    7.918,  // digit 5
    6.695,  // digit 6
    5.799,  // digit 7
    5.115,  // digit 8
    4.576,  // digit 9
];

// ---------------------------------------------------------------------------
// Plugin implementation
// ---------------------------------------------------------------------------

/// The Benford's Law plugin.
pub struct Benford;

impl Plugin for Benford {
    fn metadata() -> PluginMetadata {
        PluginMetadata {
            name: "benford".to_string(),
            version: "0.2.0".to_string(),
            description: "Benford's Law first-digit distribution and chi-square test".to_string(),
            requested_capabilities: vec![],
            config_keys: vec![],
        }
    }

    fn list_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "compute_benford_distribution".to_string(),
                description: concat!(
                    "Compute the first-digit frequency distribution of financial numbers ",
                    "and compare it to Benford's Law. Returns observed frequencies, ",
                    "expected frequencies, per-digit deviation, observed_counts array, ",
                    "and total count (total_analyzed) for use by chi_square_test."
                )
                .to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"numbers":{"type":"array","#,
                    r#""items":{"type":"number"},"description":"List of positive "#,
                    r#"financial amounts to analyse; zero and negatives are ignored"}},"#,
                    r#""required":["numbers"]}"#
                )
                .to_string(),
            },
            ToolDefinition {
                name: "chi_square_test".to_string(),
                description: concat!(
                    "Run a chi-square goodness-of-fit test on a first-digit distribution ",
                    "against Benford's Law. Returns the χ² statistic, degrees of freedom, ",
                    "risk level (LOW / MEDIUM / HIGH), most deviant digit, and interpretation. ",
                    "Use the observed_counts and total_analyzed that ",
                    "compute_benford_distribution returned."
                )
                .to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"observed_counts":{"type":"array","#,
                    r#""items":{"type":"integer"},"description":"Observed count per digit "#,
                    r#"1-9 (9 values)"},"total":{"type":"integer","description":"Total "#,
                    r#"number of observations"}},"required":["observed_counts","total"]}"#
                )
                .to_string(),
            },
        ]
    }

    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        chatty_module_sdk::log::info(&format!("benford: invoke_tool '{}'", call.name));
        let run = match call.name.as_str() {
            "compute_benford_distribution" => compute_benford_distribution,
            "chi_square_test" => chi_square_test,
            other => return Err(ToolError::unknown_tool(other)),
        };
        run(&call.arguments_json)
            .map(ToolResult::text)
            .map_err(ToolError::invalid_arguments)
    }
}

export!(Benford);

// ---------------------------------------------------------------------------
// Tool implementations — pure Rust, no network, deterministic
// ---------------------------------------------------------------------------

/// Compute the first-digit frequency distribution for a set of financial
/// numbers and compare it to Benford's Law.
///
/// Input JSON: `{"numbers": [f64, ...]}`
///
/// Output JSON: `{"total_analyzed": u64, "observed_counts": [u64; 9],
///               "distribution": [{digit, observed_count, observed_pct,
///               expected_pct, deviation}, ...]}`
fn compute_benford_distribution(args: &str) -> Result<String, String> {
    let numbers = parse_numbers_from_args(args)?;

    if numbers.is_empty() {
        return Err("numbers array is empty".to_string());
    }

    // Count first-digit occurrences (index 0 = digit 1, index 8 = digit 9).
    let mut counts = [0u64; 9];
    let mut valid: u64 = 0;

    for n in &numbers {
        if let Some(d) = first_significant_digit(*n) {
            counts[d - 1] += 1;
            valid += 1;
        }
    }

    if valid == 0 {
        return Err("No valid positive numbers found in the input".to_string());
    }

    // Build the per-digit rows.
    let rows: Vec<String> = counts
        .iter()
        .enumerate()
        .map(|(i, &cnt)| {
            let digit = i + 1;
            let observed_pct = 100.0 * cnt as f64 / valid as f64;
            let expected_pct = BENFORDS_EXPECTED[i];
            let deviation = observed_pct - expected_pct;
            format!(
                r#"{{"digit":{digit},"observed_count":{cnt},"observed_pct":{obs:.2},"expected_pct":{exp:.2},"deviation":{dev:.2}}}"#,
                obs = observed_pct,
                exp = expected_pct,
                dev = deviation,
            )
        })
        .collect();

    let counts_json: Vec<String> = counts.iter().map(|c| c.to_string()).collect();

    Ok(format!(
        r#"{{"total_analyzed":{valid},"observed_counts":[{counts}],"distribution":[{rows}]}}"#,
        counts = counts_json.join(","),
        rows = rows.join(","),
    ))
}

/// Run a chi-square goodness-of-fit test on a first-digit distribution.
///
/// Input JSON: `{"observed_counts": [u64; 9], "total": u64}`
///
/// Output JSON: `{"chi_square": f64, "degrees_of_freedom": 8,
///               "risk_level": "LOW"|"MEDIUM"|"HIGH",
///               "most_deviant_digit": u8, "interpretation": string}`
fn chi_square_test(args: &str) -> Result<String, String> {
    let (counts, total) = parse_chi_square_args(args)?;

    if counts.len() != 9 {
        return Err(format!(
            "observed_counts must have exactly 9 values (one per digit 1–9), got {}",
            counts.len()
        ));
    }
    if total == 0 {
        return Err("total must be greater than 0".to_string());
    }

    let total_f = total as f64;
    let mut chi_sq: f64 = 0.0;
    let mut max_abs_dev: f64 = 0.0;
    let mut most_deviant_digit: usize = 1;

    for (i, &observed) in counts.iter().enumerate() {
        let expected = BENFORDS_EXPECTED[i] / 100.0 * total_f;
        if expected > 0.0 {
            let diff = observed as f64 - expected;
            chi_sq += (diff * diff) / expected;
            if diff.abs() > max_abs_dev {
                max_abs_dev = diff.abs();
                most_deviant_digit = i + 1;
            }
        }
    }

    // Degrees of freedom = 8 (9 bins − 1 constraint).
    // Critical values for df = 8:
    //   p < 0.05  →  χ² > 15.507   (MEDIUM risk — statistically significant)
    //   p < 0.01  →  χ² > 20.090   (HIGH risk   — highly significant)
    let (risk_level, interpretation) = if chi_sq > 20.090 {
        (
            "HIGH",
            "Strong deviation from Benford's Law (p < 0.01). \
             Results are highly statistically significant — \
             recommend detailed forensic investigation.",
        )
    } else if chi_sq > 15.507 {
        (
            "MEDIUM",
            "Moderate deviation from Benford's Law (p < 0.05). \
             Results are statistically significant — \
             recommend selective transaction review.",
        )
    } else {
        (
            "LOW",
            "Distribution conforms to Benford's Law. \
             No statistically significant anomaly detected.",
        )
    };

    Ok(format!(
        r#"{{"chi_square":{chi_sq:.3},"degrees_of_freedom":8,"risk_level":"{risk_level}","most_deviant_digit":{most_deviant_digit},"interpretation":"{interpretation}"}}"#,
    ))
}

// ---------------------------------------------------------------------------
// JSON parsing helpers (no serde derive needed — only simple array/object)
// ---------------------------------------------------------------------------

/// Parse `{"numbers": [f64, f64, ...]}` from a JSON string.
fn parse_numbers_from_args(args: &str) -> Result<Vec<f64>, String> {
    let v: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("Invalid JSON args: {e}"))?;

    let arr = v["numbers"]
        .as_array()
        .ok_or_else(|| "Missing 'numbers' array in arguments".to_string())?;

    arr.iter()
        .map(|item| {
            item.as_f64()
                .ok_or_else(|| format!("Expected a number, got: {item}"))
        })
        .collect()
}

/// Parse `{"observed_counts": [u64, ...], "total": u64}` from a JSON string.
fn parse_chi_square_args(args: &str) -> Result<(Vec<u64>, u64), String> {
    let v: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("Invalid JSON args: {e}"))?;

    let counts_val = v["observed_counts"]
        .as_array()
        .ok_or_else(|| "Missing 'observed_counts' array in arguments".to_string())?;

    let counts: Vec<u64> = counts_val
        .iter()
        .map(|item| {
            item.as_u64()
                .ok_or_else(|| format!("Expected an integer count, got: {item}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let total = v["total"]
        .as_u64()
        .ok_or_else(|| "Missing 'total' field in arguments".to_string())?;

    Ok((counts, total))
}

/// Return the first significant digit (1–9) of a positive finite number.
/// Returns `None` for zero, negative, or non-finite values.
fn first_significant_digit(n: f64) -> Option<usize> {
    if n <= 0.0 || !n.is_finite() {
        return None;
    }
    // Bring n into the half-open interval [1, 10).
    let mut x = n;
    while x >= 10.0 {
        x /= 10.0;
    }
    while x < 1.0 {
        x *= 10.0;
    }
    Some(x as usize)
}

// ---------------------------------------------------------------------------
// Unit tests (run on host with `cargo test`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_significant_digit() {
        assert_eq!(first_significant_digit(1.0), Some(1));
        assert_eq!(first_significant_digit(9.99), Some(9));
        assert_eq!(first_significant_digit(1234.0), Some(1));
        assert_eq!(first_significant_digit(5678.0), Some(5));
        assert_eq!(first_significant_digit(0.00345), Some(3));
        assert_eq!(first_significant_digit(0.0), None);
        assert_eq!(first_significant_digit(-5.0), None);
    }

    #[test]
    fn test_compute_benford_distribution_basic() {
        // Numbers with first digits: 1,4,8,2,5,8,2,4,7,8 → 1×1, 2×2, 1×4, 1×4... 
        let args = r#"{"numbers": [1234, 4521, 891, 2340, 567, 8901, 234, 456, 789, 8123]}"#;
        let result = compute_benford_distribution(args).unwrap();
        assert!(result.contains("total_analyzed\":10"));
        assert!(result.contains("observed_counts"));
        assert!(result.contains("distribution"));
    }

    #[test]
    fn test_compute_benford_skips_non_positive() {
        let args = r#"{"numbers": [100, -50, 0, 200]}"#;
        let result = compute_benford_distribution(args).unwrap();
        // Only 100 and 200 are valid → total 2
        assert!(result.contains("total_analyzed\":2"));
    }

    #[test]
    fn test_chi_square_benford_conforming() {
        // Counts proportional to Benford's expected → LOW risk.
        // Use 1000 total with roughly expected proportions.
        let counts = [301u64, 176, 125, 97, 79, 67, 58, 51, 46];
        let total: u64 = counts.iter().sum();
        let counts_json: Vec<String> = counts.iter().map(|c| c.to_string()).collect();
        let args = format!(
            r#"{{"observed_counts":[{}],"total":{}}}"#,
            counts_json.join(","),
            total
        );
        let result = chi_square_test(&args).unwrap();
        assert!(result.contains("LOW"), "Expected LOW risk for Benford-conforming data, got: {result}");
    }

    #[test]
    fn test_chi_square_high_risk() {
        // Heavily skewed toward digit 1 → HIGH risk.
        let counts = [900u64, 10, 10, 10, 10, 10, 10, 10, 10];
        let total: u64 = counts.iter().sum();
        let counts_json: Vec<String> = counts.iter().map(|c| c.to_string()).collect();
        let args = format!(
            r#"{{"observed_counts":[{}],"total":{}}}"#,
            counts_json.join(","),
            total
        );
        let result = chi_square_test(&args).unwrap();
        assert!(result.contains("HIGH"), "Expected HIGH risk for skewed data, got: {result}");
    }

    #[test]
    fn test_chi_square_wrong_count_length() {
        let args = r#"{"observed_counts":[1,2,3],"total":6}"#;
        assert!(chi_square_test(&args).is_err());
    }

    #[test]
    fn test_compute_benford_empty() {
        let args = r#"{"numbers": []}"#;
        assert!(compute_benford_distribution(&args).is_err());
    }

    #[test]
    fn test_round_trip_tool_args() {
        // Simulate the LLM calling compute_benford → extracting counts → calling chi_square.
        let numbers_args =
            r#"{"numbers": [1234, 4521, 891, 2340, 567, 8901, 234, 456, 789]}"#;
        let dist_result = compute_benford_distribution(numbers_args).unwrap();

        // Parse out observed_counts and total from the result.
        let v: serde_json::Value = serde_json::from_str(&dist_result).unwrap();
        let counts: Vec<String> = v["observed_counts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.to_string())
            .collect();
        let total = v["total_analyzed"].as_u64().unwrap();

        let chi_args = format!(
            r#"{{"observed_counts":[{}],"total":{}}}"#,
            counts.join(","),
            total
        );
        let chi_result = chi_square_test(&chi_args).unwrap();
        assert!(chi_result.contains("chi_square"));
        assert!(chi_result.contains("risk_level"));
        assert!(chi_result.contains("interpretation"));
    }
}
