// Shared pdfium download logic for build scripts.
//
// Include this file in `build.rs` with:
//   include!("../../scripts/pdfium_build.rs");

const PDFIUM_VERSION: &str = "7543";

/// Total attempts (including the first) made to download pdfium before giving up.
/// Bounded on purpose: this runs synchronously inside `cargo build` and must not hang
/// the build indefinitely on a persistent outage.
const PDFIUM_DOWNLOAD_MAX_ATTEMPTS: u32 = 3;

/// Base delay for the backoff between attempts; doubled after each failure
/// (500ms, 1000ms for 3 attempts), so the extra wall-clock cost of a fully
/// exhausted retry loop is on the order of a second or two.
const PDFIUM_DOWNLOAD_RETRY_BASE_DELAY_MS: u64 = 500;

fn setup_pdfium() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    let libs_dir = std::path::PathBuf::from(&manifest_dir)
        .join("libs")
        .join("lib");
    std::fs::create_dir_all(&libs_dir).ok();

    let (lib_name, download_url) = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => (
            "libpdfium.dylib",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-mac-arm64.tgz",
                PDFIUM_VERSION
            ),
        ),
        ("macos", "x86_64") => (
            "libpdfium.dylib",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-mac-x64.tgz",
                PDFIUM_VERSION
            ),
        ),
        ("linux", "x86_64") => (
            "libpdfium.so",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-linux-x64.tgz",
                PDFIUM_VERSION
            ),
        ),
        ("windows", "x86_64") => (
            "pdfium.dll",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-win-x64.tgz",
                PDFIUM_VERSION
            ),
        ),
        _ => {
            println!(
                "cargo:warning=Unsupported platform for pdfium: {}-{}",
                target_os, target_arch
            );
            return;
        }
    };

    let lib_path = libs_dir.join(lib_name);

    if !lib_path.exists() {
        println!(
            "cargo:warning=Downloading pdfium library for {}-{}...",
            target_os, target_arch
        );
        if let Err(e) =
            download_with_retries(|| download_and_extract_pdfium(&download_url, &libs_dir))
        {
            // Loud, build-failing error rather than a warning: a build that cannot
            // satisfy the "pdf" feature (or chatty-gpui's unconditional pdfium need)
            // should not succeed and silently produce a binary that fails PDF
            // operations at runtime. Naming the URL and attempt count here is what
            // lets a transient network blip be told apart from a real regression.
            panic!(
                "Failed to download pdfium from {} after {} attempt(s): {}\n\
                 This is required to build with the \"pdf\" feature. If this looks like a \
                 transient network problem (a GitHub blip, DNS hiccup, etc.), re-run the \
                 build. If it persists, check that {} is reachable from this network.",
                download_url, PDFIUM_DOWNLOAD_MAX_ATTEMPTS, e, download_url
            );
        }
        println!("cargo:warning=Pdfium library downloaded successfully");
    }

    // Tell the binary where to find pdfium at runtime
    println!("cargo:rustc-env=PDFIUM_LIB_DIR={}", libs_dir.display());
    println!("cargo:rerun-if-changed=libs/lib");
}

/// Run `download` up to [`PDFIUM_DOWNLOAD_MAX_ATTEMPTS`] times, with a bounded
/// exponential backoff between failed attempts. Returns the last error if every
/// attempt fails.
fn download_with_retries<F>(mut download: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut() -> Result<(), Box<dyn std::error::Error>>,
{
    let mut last_err = None;
    for attempt in 1..=PDFIUM_DOWNLOAD_MAX_ATTEMPTS {
        match download() {
            Ok(()) => return Ok(()),
            Err(e) => {
                println!(
                    "cargo:warning=pdfium download attempt {}/{} failed: {}",
                    attempt, PDFIUM_DOWNLOAD_MAX_ATTEMPTS, e
                );
                last_err = Some(e);
                if attempt < PDFIUM_DOWNLOAD_MAX_ATTEMPTS {
                    let backoff_ms = PDFIUM_DOWNLOAD_RETRY_BASE_DELAY_MS * (1u64 << (attempt - 1));
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
            }
        }
    }
    Err(last_err.expect("loop runs at least once, so an error was recorded"))
}

fn download_and_extract_pdfium(
    url: &str,
    dest_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    let bytes = response.bytes()?;

    let tar = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes));
    let mut archive = tar::Archive::new(tar);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy();

        // Only extract library files from lib/ directory
        if (path_str.contains("libpdfium") || path_str.contains("pdfium.dll"))
            && let Some(file_name) = path.file_name()
        {
            let dest_path = dest_dir.join(file_name);
            entry.unpack(&dest_path)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(&dest_path) {
                    let mut perms = metadata.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&dest_path, perms).ok();
                }
            }
        }
    }

    Ok(())
}

// NOTE ON TEST EXECUTION: this file is spliced into two `build.rs` files via
// `include!`. Cargo compiles `build.rs` as a build-script binary, never as a
// `#[cfg(test)]` target, so `cargo test` (workspace-wide or otherwise) does not
// run the tests below. They exercise `download_with_retries` in isolation as an
// executable spec; run them directly with, e.g.:
//   rustc --edition 2024 --test scripts/pdfium_build.rs -o /tmp/pdfium_build_test \
//     && /tmp/pdfium_build_test
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn retries_and_succeeds_after_transient_failures() {
        let attempts = Cell::new(0u32);
        let result = download_with_retries(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < PDFIUM_DOWNLOAD_MAX_ATTEMPTS {
                Err("simulated transient network error".into())
            } else {
                Ok(())
            }
        });

        assert!(result.is_ok(), "expected eventual success, got {result:?}");
        assert_eq!(
            attempts.get(),
            PDFIUM_DOWNLOAD_MAX_ATTEMPTS,
            "expected to retry until the last allowed attempt succeeded"
        );
    }

    #[test]
    fn gives_up_after_max_attempts_and_names_the_failure() {
        let attempts = Cell::new(0u32);
        let result = download_with_retries(|| {
            attempts.set(attempts.get() + 1);
            Err("connection refused".into())
        });

        let err = result.expect_err("every attempt fails, so the overall result must be Err");
        assert_eq!(
            attempts.get(),
            PDFIUM_DOWNLOAD_MAX_ATTEMPTS,
            "retry loop must be bounded, not unbounded"
        );
        assert!(
            err.to_string().contains("connection refused"),
            "the underlying error should be preserved so the failure can be diagnosed, got {err}"
        );
    }

    #[test]
    fn does_not_retry_after_first_success() {
        let attempts = Cell::new(0u32);
        let result = download_with_retries(|| {
            attempts.set(attempts.get() + 1);
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 1, "must not retry once a download succeeds");
    }
}
