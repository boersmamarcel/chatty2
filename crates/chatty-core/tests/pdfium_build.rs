//! Runs the unit tests in `scripts/pdfium_build.rs` (shared by the chatty-core and
//! chatty-gpui build scripts). Cargo never runs tests from a `build.rs`, so the file is
//! compiled here as a test target instead.
#![allow(dead_code)]

include!("../../../scripts/pdfium_build.rs");
