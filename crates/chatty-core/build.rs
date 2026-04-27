include!("../../scripts/pdfium_build.rs");

fn main() {
    // Only download pdfium when the "pdf" feature is enabled
    if std::env::var("CARGO_FEATURE_PDF").is_ok() {
        setup_pdfium();
    }
}
