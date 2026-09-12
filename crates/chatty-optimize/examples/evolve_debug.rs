//! Manual debug entry for Algorithm 1 [`evolve`](chatty_optimize::gepa::evolve::evolve).
//!
//! Paper: Agrawal et al., ICLR 2026, arXiv:2507.19457, Algorithm 1 / Figure 4.
//!
//! ```bash
//! cargo run -p chatty-optimize --example evolve_debug
//! ```
//!
//! Debugger (lldb):
//! ```bash
//! cargo build -p chatty-optimize --example evolve_debug
//! rust-lldb target/debug/examples/evolve_debug
//! (lldb) b chatty_optimize::gepa::evolve::evolve
//! (lldb) run
//! ```

use chatty_optimize::gepa::evolve::{GepaConfig, evolve};
use chatty_optimize::gepa::system::{CompoundSystem, DualKeywordSystem, KeywordSystem};

fn main() {
    let feedback = vec!["cat".into(), "dog".into(), "bird".into()];
    let pareto = vec!["cat".into(), "dog".into()];
    let cfg = GepaConfig::default();

    println!("config: {cfg:?}");
    println!("D_feedback = {feedback:?}");
    println!("D_pareto   = {pareto:?}");

    println!("\n--- KeywordSystem (1 module) ---");
    run(KeywordSystem::new("seed"), &feedback, &pareto, &cfg);

    println!("\n--- DualKeywordSystem (2 modules) ---");
    run(
        DualKeywordSystem::new("seed-0", "seed-1"),
        &feedback,
        &pareto,
        &cfg,
    );
}

fn run<S: CompoundSystem>(seed: S, feedback: &[String], pareto: &[String], cfg: &GepaConfig) {
    println!("n_modules = {}", seed.n_modules());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        evolve(seed, feedback, pareto, cfg)
    }));
    match caught {
        Ok(Ok(result)) => println!(
            "best_index={} feedback_rollouts={} pareto_rollouts={}",
            result.best_index, result.state.rollouts.feedback, result.state.rollouts.pareto
        ),
        Ok(Err(e)) => eprintln!("error: {e}"),
        Err(_) => eprintln!("evolve still stubbed (todo! human: Algorithm 1)"),
    }
}
