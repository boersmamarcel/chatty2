//! AGE-808: check documents the `architecture-review` team wrote against
//! their format (`chatty_core::services::architecture_doc`), one line per
//! rule. Exits non-zero when any rule fails.
//!
//! ```text
//! cargo run -p chatty-core --example check_architecture_doc -- docs/adr/ADR-0001-x.md [more.md ...]
//! ```

use std::path::Path;
use std::process::ExitCode;

use chatty_core::services::architecture_doc::check;

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: check_architecture_doc <document.md> [...]");
        return ExitCode::from(2);
    }
    let mut all_passed = true;
    for path in &paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("{path}: {e}");
                all_passed = false;
                continue;
            }
        };
        let name = Path::new(path).file_name().and_then(|n| n.to_str());
        let report = check(name, &text);
        println!("{path} ({:?})", report.kind);
        for rule in &report.rules {
            let mark = if rule.passed { "PASS" } else { "FAIL" };
            println!("  {mark}  {}: {}", rule.rule, rule.detail);
        }
        all_passed &= report.passed();
    }
    if all_passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
