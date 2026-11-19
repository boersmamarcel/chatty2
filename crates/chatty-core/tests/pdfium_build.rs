//! Runs the unit tests in `scripts/pdfium_build.rs` (shared by the chatty-core and
//! chatty-gpui build scripts). Cargo never runs tests from a `build.rs`, so the file is
//! compiled here as a test target instead.

// The included build-script file has items this test target does not use.
#![allow(dead_code)]

include!("../../../scripts/pdfium_build.rs");
