//! Benford's Law audit agent — agentic chatty WASM module.
//!
//! Demonstrates a **full agentic tool-calling loop** running entirely inside
//! WASM. Given a list of financial numbers, the agent:
//!
//! 1. Calls the host LLM with two tool definitions
//! 2. The LLM asks to call `compute_benford_distribution` → executed locally
//! 3. The LLM asks to call `chi_square_test` → executed locally
//! 4. Tool results are fed back; the LLM writes the final audit report
//!
//! ## Usage
//!
//! Via the chatty `/agent` command (local module runtime):
//!
//! ```text
//! /agent benford-agent Analyze these invoice amounts: 1234 4521 891 2340 567 8901
//! ```
//!
//! Via A2A HTTP (protocol gateway exposes `/a2a/benford-agent`):
//!
//! ```sh
//! curl -X POST http://localhost:8420/a2a/benford-agent \
//!   -H "Content-Type: application/json" \
//!   -d '{"jsonrpc":"2.0","id":1,"method":"message/send",
//!        "params":{"message":{"parts":[{"type":"text",
//!          "text":"Analyze these invoice amounts: 1234 4521 891 2340 567 8901"}]}}}'
//! ```
//!
//! ## Tools
//!
//! | Tool | Input | Output |
//! |------|-------|--------|
//! | `compute_benford_distribution` | `numbers: [f64]` | observed & expected first-digit frequencies, deviation per digit |
//! | `chi_square_test` | `observed_counts: [u64]`, `total: u64` | χ² statistic, risk level (`LOW`/`MEDIUM`/`HIGH`), interpretation |
//!
//! Both tools are implemented in pure Rust with no external network calls —
//! they run deterministically inside the WASM sandbox.

use chatty_module_sdk::{
    export_module, AgentCard, ChatRequest, ChatResponse, Message, ModuleExports, Role, Skill,
    ToolDefinition,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum LLM ↔ tool-result turns before forcing a summary.
const MAX_TURNS: usize = 6;

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

/// JSON tool schema passed to `llm::complete`.
/// Using the OpenAI function-calling format which the host translates to
/// provider-specific formats (Anthropic tool_use, Gemini functionDeclarations, etc.)
const TOOLS_JSON: &str = r#"[
  {
    "name": "compute_benford_distribution",
    "description": "Compute the first-digit frequency distribution of a list of financial numbers and compare it to Benford's Law. Returns observed frequencies, expected frequencies, and the signed deviation (observed − expected) for each digit 1–9, plus observed_counts and total for use by chi_square_test.",
    "parameters": {
      "type": "object",
      "properties": {
        "numbers": {
          "type": "array",
          "items": { "type": "number" },
          "description": "List of positive financial amounts to analyse (e.g. invoice totals, transaction values). Negative values and zero are ignored."
        }
      },
      "required": ["numbers"]
    }
  },
  {
    "name": "chi_square_test",
    "description": "Run a chi-square goodness-of-fit test comparing observed first-digit counts against Benford's Law expected distribution. Returns the χ² statistic, degrees of freedom (8), risk level (LOW / MEDIUM / HIGH), the most deviant digit, and a plain-English interpretation. Use the observed_counts and total values returned by compute_benford_distribution.",
    "parameters": {
      "type": "object",
      "properties": {
        "observed_counts": {
          "type": "array",
          "items": { "type": "integer" },
          "description": "Observed count for each first digit 1–9, exactly 9 integers (index 0 = digit 1)."
        },
        "total": {
          "type": "integer",
          "description": "Total number of valid observations (sum of observed_counts)."
        }
      },
      "required": ["observed_counts", "total"]
    }
  }
]"#;

const SYSTEM_PROMPT: &str = "\
You are a forensic financial auditor specialising in Benford's Law analysis. \
Your task is to detect anomalies in financial datasets that may indicate fraud, \
errors, or data manipulation.\n\
\n\
When given a list of financial numbers:\n\
1. Call compute_benford_distribution to obtain the first-digit distribution and \
   the observed_counts + total values.\n\
2. Call chi_square_test using those observed_counts and total values.\n\
3. Write a concise, professional audit report that includes:\n\
   - The risk level and χ² statistic\n\
   - Which digits deviate most from Benford's Law and by how much\n\
   - A clear conclusion and recommended next steps\n\
\n\
Always call both tools before writing your report. Be specific and quantitative.";

// ---------------------------------------------------------------------------
// Agent implementation
// ---------------------------------------------------------------------------

/// Benford's Law audit agent.
#[derive(Default)]
pub struct BenfordAgent;

impl ModuleExports for BenfordAgent {
    // -----------------------------------------------------------------------
    // chat — full agentic tool-calling loop
    // -----------------------------------------------------------------------

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        chatty_module_sdk::log::info("benford-agent: starting Benford's Law audit");

        let user_content = req
            .messages
            .iter()
            .rfind(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("")
            .to_string();

        // Build the initial message history for the LLM.
        let mut messages: Vec<Message> = vec![
            Message {
                role: Role::System,
                content: SYSTEM_PROMPT.to_string(),
            },
            Message {
                role: Role::User,
                content: user_content,
            },
        ];

        // ── Agentic loop ────────────────────────────────────────────────────
        for turn in 0..MAX_TURNS {
            chatty_module_sdk::log::debug(&format!(
                "benford-agent: agentic turn {}/{}",
                turn + 1,
                MAX_TURNS
            ));

            let resp =
                chatty_module_sdk::llm::complete("", &messages, Some(TOOLS_JSON))?;

            // No tool calls → LLM produced the final audit report.
            if resp.tool_calls.is_empty() {
                chatty_module_sdk::log::info(
                    "benford-agent: audit report generated — no more tool calls",
                );
                return Ok(ChatResponse {
                    content: resp.content,
                    tool_calls: vec![],
                    usage: resp.usage,
                });
            }

            // Log what the LLM wants to do.
            for tc in &resp.tool_calls {
                chatty_module_sdk::log::info(&format!(
                    "benford-agent: LLM requested tool '{}' with args: {}",
                    tc.name, tc.arguments
                ));
            }

            // Record the assistant turn in history (include tool-call intent
            // in the content so the LLM has context on the next turn).
            let tool_call_summary = resp
                .tool_calls
                .iter()
                .map(|tc| format!("{}({})", tc.name, tc.arguments))
                .collect::<Vec<_>>()
                .join("; ");

            let assistant_content = if resp.content.is_empty() {
                format!("[Calling tools: {}]", tool_call_summary)
            } else {
                format!("{}\n[Calling tools: {}]", resp.content, tool_call_summary)
            };

            messages.push(Message {
                role: Role::Assistant,
                content: assistant_content,
            });

            // Execute each requested tool locally and collect results.
            let mut tool_result_lines: Vec<String> = Vec::new();
            for tc in &resp.tool_calls {
                let result = self
                    .invoke_tool(tc.name.clone(), tc.arguments.clone())
                    .unwrap_or_else(|e| {
                        chatty_module_sdk::log::warn(&format!(
                            "benford-agent: tool '{}' error: {}",
                            tc.name, e
                        ));
                        format!(r#"{{"error": "{e}"}}"#)
                    });

                chatty_module_sdk::log::debug(&format!(
                    "benford-agent: tool '{}' result: {}",
                    tc.name, result
                ));

                tool_result_lines.push(format!("[{}] → {}", tc.name, result));
            }

            // Feed tool results back as a user message so the LLM can
            // reference them on the next turn.
            messages.push(Message {
                role: Role::User,
                content: format!("Tool results:\n{}", tool_result_lines.join("\n\n")),
            });
        }

        // ── Fallback: max turns reached ─────────────────────────────────────
        chatty_module_sdk::log::warn(
            "benford-agent: max turns reached — requesting summary from LLM",
        );
        messages.push(Message {
            role: Role::User,
            content:
                "Please now provide your complete audit findings and recommendations \
                 based on all tool results above."
                    .to_string(),
        });

        let final_resp = chatty_module_sdk::llm::complete("", &messages, None)?;
        Ok(ChatResponse {
            content: final_resp.content,
            tool_calls: vec![],
            usage: final_resp.usage,
        })
    }

    // -----------------------------------------------------------------------
    // invoke_tool — called by the host for MCP / direct tool invocation
    //               (also called internally from the agentic loop above)
    // -----------------------------------------------------------------------

    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> {
        chatty_module_sdk::log::info(&format!("benford-agent: invoke_tool '{}'", name));

        match name.as_str() {
            "compute_benford_distribution" => compute_benford_distribution(&args),
            "chi_square_test" => chi_square_test(&args),
            _ => {
                chatty_module_sdk::log::error(&format!("benford-agent: unknown tool '{}'", name));
                Err(format!("unknown tool: {name}"))
            }
        }
    }

    // -----------------------------------------------------------------------
    // list_tools — advertised via MCP tools/list and A2A agent card
    // -----------------------------------------------------------------------

    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "compute_benford_distribution".to_string(),
                description: concat!(
                    "Compute the first-digit frequency distribution of financial numbers ",
                    "and compare it to Benford's Law. Returns observed frequencies, ",
                    "expected frequencies, per-digit deviation, observed_counts array, ",
                    "and total count."
                )
                .to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"numbers":{"type":"array","#,
                    r#""items":{"type":"number"},"description":"List of positive "#,
                    r#"financial amounts to analyse"}},"required":["numbers"]}"#
                )
                .to_string(),
            },
            ToolDefinition {
                name: "chi_square_test".to_string(),
                description: concat!(
                    "Run a chi-square goodness-of-fit test on a first-digit distribution ",
                    "against Benford's Law. Returns the χ² statistic, degrees of freedom, ",
                    "risk level (LOW / MEDIUM / HIGH), most deviant digit, and interpretation."
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

    // -----------------------------------------------------------------------
    // get_agent_card — returned by A2A GET /.well-known/agent.json
    // -----------------------------------------------------------------------

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "benford-agent".to_string(),
            display_name: "Benford's Law Audit Agent".to_string(),
            description: concat!(
                "Forensic financial auditor that applies Benford's Law to detect ",
                "anomalies in transaction datasets. Runs a full agentic tool-calling ",
                "loop: computes first-digit distributions, performs a chi-square test, ",
                "then synthesises a professional audit report via the host LLM. ",
                "Exposed via MCP (tools) and A2A (message/send) protocols."
            )
            .to_string(),
            version: "0.1.0".to_string(),
            skills: vec![Skill {
                name: "benford-analysis".to_string(),
                description: concat!(
                    "Analyse a list of financial amounts for Benford's Law conformance ",
                    "and produce a risk-rated audit report with recommendations."
                )
                .to_string(),
                examples: vec![
                    "Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789"
                        .to_string(),
                    "Run a Benford audit on Q4 transactions: 10234 5621 8901 3412 7654"
                        .to_string(),
                    "Check for fraud in these GL entries: 9823 1045 3278 6541 2109 7832"
                        .to_string(),
                ],
            }],
            tools: vec![],
        }
    }
}

// Wire the trait implementation to the WIT guest exports.
export_module!(BenfordAgent);

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
