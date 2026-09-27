//! Test kits other crates build their tests on (AGE-632).
//!
//! [`fake_model`] is a localhost model server that answers from a script and
//! records every request, so a test that spans processes — a leader, real
//! `chatty-tui` workers, a broker — runs with no network and no real model.

pub mod fake_model;
