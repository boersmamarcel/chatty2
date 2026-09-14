//! AGE-287: turn a `paired.csv` (written by harbor-chatty's `scripts/compare.py`)
//! into a McNemar report with a minimum detectable effect.
//!
//! Usage: `cargo run -p chatty-optimize --example paired_report -- paired.csv`
//!
//! Expects a header row with (at least) `a_correct` and `b_correct` columns;
//! other columns (task, attempt, tokens, cost) are ignored here — compare.py
//! reports those deltas itself.

use chatty_optimize::{
    BinaryOutcome, format_paired_binary_report, mcnemar, minimum_detectable_effect_binary,
};
use std::{env, fs};

fn parse_bool(field: &str) -> bool {
    matches!(field.trim().to_ascii_lowercase().as_str(), "true" | "1")
}

fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: paired_report <paired.csv>");
    let csv = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let mut lines = csv.lines();
    let header: Vec<&str> = lines.next().expect("empty csv").split(',').collect();
    let a_col = header
        .iter()
        .position(|c| *c == "a_correct")
        .expect("no a_correct column");
    let b_col = header
        .iter()
        .position(|c| *c == "b_correct")
        .expect("no b_correct column");

    let outcomes: Vec<BinaryOutcome> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            assert!(
                fields.len() > a_col.max(b_col),
                "row has {} column(s), need at least {} for a_correct/b_correct: {line:?}",
                fields.len(),
                a_col.max(b_col) + 1
            );
            BinaryOutcome {
                arm_a_correct: parse_bool(fields[a_col]),
                arm_b_correct: parse_bool(fields[b_col]),
            }
        })
        .collect();

    println!("{}", format_paired_binary_report(&outcomes));

    let res = mcnemar(&outcomes).expect("paired.csv has no rows");
    let discordant = res.b + res.c;
    println!(
        "discordant pairs: b(A>B)={} c(B>A)={} total={discordant}",
        res.b, res.c,
    );
    // minimum_detectable_effect_binary clamps discordant_rate to a floor of
    // 0.01, so at zero observed discordant pairs this would print an MDE
    // that is a clamp artifact, not a real power estimate (it can look
    // *better* than a larger, properly-powered run purely because of the
    // floor). Only report it when there's a real observed rate to base it
    // on; the default-rate MDE in the report line above is the honest
    // headline number for a zero-discordance run.
    if discordant == 0 {
        println!(
            "MDE (observed-rate): not meaningful at zero discordant pairs \
             (minimum_detectable_effect_binary's 0.01 floor would make this a \
             clamp artifact, not a power estimate) -- use the default-rate MDE above."
        );
    } else if let Some(mde) = minimum_detectable_effect_binary(
        outcomes.len(),
        0.05,
        0.8,
        discordant as f64 / outcomes.len() as f64,
    ) {
        println!(
            "MDE (n={}, alpha={}, power={}, discordant_rate={:.4}) ~= {:.4}",
            mde.n,
            mde.alpha,
            mde.power,
            discordant as f64 / outcomes.len() as f64,
            mde.mde
        );
    }
}
