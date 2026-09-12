//! Manual debug entry for human-reserved [`select_candidate`](chatty_optimize::gepa::select::select_candidate).
//!
//! ```bash
//! cargo run -p chatty-optimize --example select_candidate_debug
//! cargo run -p chatty-optimize --example select_candidate_debug -- --trials 1000 --seed 42
//! ```
//!
//! Debugger (lldb):
//! ```bash
//! cargo build -p chatty-optimize --example select_candidate_debug
//! rust-lldb target/debug/examples/select_candidate_debug
//! (lldb) b chatty_optimize::gepa::select::select_candidate
//! (lldb) run
//! ```

use chatty_optimize::gepa::select::{paper_dominance_matrix, select_candidate};
use std::collections::BTreeMap;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut trials = 1usize;
    let mut seed = 42u64;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--trials" => {
                i += 1;
                trials = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
            }
            "--seed" => {
                i += 1;
                seed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(42);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: select_candidate_debug [--trials N] [--seed S]\n\
                     Exercises select_candidate on the paper dominance matrix."
                );
                return;
            }
            other => {
                eprintln!("unknown arg: {other} (try --help)");
                return;
            }
        }
        i += 1;
    }

    let matrix = paper_dominance_matrix();
    print_matrix(&matrix);

    if trials == 1 {
        match select_candidate(&matrix) {
            Ok(idx) => println!("selected candidate: {idx}"),
            Err(e) => eprintln!("error: {e}"),
        }
        return;
    }

    let mut counts: BTreeMap<usize, u32> = BTreeMap::new();
    for t in 0..trials {
        let idx = select_candidate(&matrix).unwrap_or_else(|e| panic!("trial {t}: {e}"));
        *counts.entry(idx).or_insert(0) += 1;
    }
    println!("histogram over {trials} trials (seed {seed} is for your RNG if wired later):");
    for (idx, n) in counts {
        println!("  candidate {idx}: {n}");
    }
}

fn print_matrix(matrix: &[Vec<f64>]) {
    println!("score_matrix (rows = candidates, cols = instances):");
    for (i, row) in matrix.iter().enumerate() {
        println!("  candidate {i}: {row:?}");
    }
}
