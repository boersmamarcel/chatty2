// Shared pdfium download logic for build scripts.
//
// Include this file in `build.rs` with:
//   include!("../../scripts/pdfium_build.rs");

const PDFIUM_VERSION: &str = "7543";

/// Environment variable naming a local, pre-fetched pdfium library
/// (`CHATTY_PDFIUM_LIB=/abs/path/libpdfium.so`). When set, the build uses it instead of
/// downloading (AGE-1056: a coding lease reaches only the crates proxy, not GitHub).
/// The file must match the pinned sha256 of the release's library for this platform.
const PDFIUM_LIB_ENV: &str = "CHATTY_PDFIUM_LIB";

// Pinned sha256 digests for bblanchon/pdfium-binaries chromium/7543. `*_TGZ` is the
// release asset; `*_LIB` is the library inside it (what `CHATTY_PDFIUM_LIB` points at).
// Bumping PDFIUM_VERSION means re-pinning all of these.
const PDFIUM_SHA256_MAC_ARM64_TGZ: &str =
    "41c269723b4711793de70ff34e65c00fa79907d6c023741837579e906b846faa";
const PDFIUM_SHA256_MAC_ARM64_LIB: &str =
    "858f0676a1ac5b666673fc6e56b4401f95907a3fc66fa4635d626097a04c205b";
const PDFIUM_SHA256_MAC_X64_TGZ: &str =
    "2510460ac106f14b884598a0da3f53a99e23d79512acf027c5e101c2bb2f26cb";
const PDFIUM_SHA256_MAC_X64_LIB: &str =
    "c4ae7ca1583e04449d07f1985ce258a3f935583279fd46fa89f528106301b929";
const PDFIUM_SHA256_LINUX_X64_TGZ: &str =
    "9329a3c4b19b3c8d0a93af5440f44be84e4bd879a204e47b1a7a160e96809da4";
const PDFIUM_SHA256_LINUX_X64_LIB: &str =
    "2383a414050dd21ae5300b119ad8a72360ef92cff820b4c685c047dc272c2794";
const PDFIUM_SHA256_WIN_X64_TGZ: &str =
    "0b08b606792a6cc593426efdefc6622611bce446d9e0270743846956ea1554ca";
const PDFIUM_SHA256_WIN_X64_LIB: &str =
    "6b963c2be9cacbaa0c0c7f4bf6d20d2fd16729ebdaa9989978b0f7b119c1c1cb";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Check `bytes` against a pinned lowercase-hex sha256. `what` names the thing being
/// checked for the error message.
fn verify_sha256(bytes: &[u8], expected: &str, what: &str) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "sha256 mismatch for {what}: expected {expected}, got {actual}"
        ))
    }
}

/// Verify the library at `src` against `expected_sha256` and install it at `dest`.
/// Nothing is written to `dest` unless the checksum matches.
fn install_local_pdfium(
    src: &std::path::Path,
    dest: &std::path::Path,
    expected_sha256: &str,
) -> Result<(), String> {
    let bytes = std::fs::read(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    verify_sha256(&bytes, expected_sha256, &src.display().to_string())?;
    // Write via a temp name + rename so a concurrent build never sees a partial file.
    let tmp = dest.with_extension("tmp-local");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).ok();
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("cannot install {}: {e}", dest.display()))
}

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

    let (lib_name, download_url, tgz_sha256, lib_sha256) = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => (
            "libpdfium.dylib",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-mac-arm64.tgz",
                PDFIUM_VERSION
            ),
            PDFIUM_SHA256_MAC_ARM64_TGZ,
            PDFIUM_SHA256_MAC_ARM64_LIB,
        ),
        ("macos", "x86_64") => (
            "libpdfium.dylib",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-mac-x64.tgz",
                PDFIUM_VERSION
            ),
            PDFIUM_SHA256_MAC_X64_TGZ,
            PDFIUM_SHA256_MAC_X64_LIB,
        ),
        ("linux", "x86_64") => (
            "libpdfium.so",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-linux-x64.tgz",
                PDFIUM_VERSION
            ),
            PDFIUM_SHA256_LINUX_X64_TGZ,
            PDFIUM_SHA256_LINUX_X64_LIB,
        ),
        ("windows", "x86_64") => (
            "pdfium.dll",
            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-win-x64.tgz",
                PDFIUM_VERSION
            ),
            PDFIUM_SHA256_WIN_X64_TGZ,
            PDFIUM_SHA256_WIN_X64_LIB,
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

    println!("cargo:rerun-if-env-changed={PDFIUM_LIB_ENV}");
    let local_lib = std::env::var_os(PDFIUM_LIB_ENV).filter(|v| !v.is_empty());
    if let Some(local_lib) = local_lib {
        // A local library is verified and wins over whatever is already in libs/lib,
        // so a stale or unverified file there is replaced. No network is touched.
        let local_lib = std::path::PathBuf::from(local_lib);
        println!("cargo:rerun-if-changed={}", local_lib.display());
        if let Err(e) = install_local_pdfium(&local_lib, &lib_path, lib_sha256) {
            panic!(
                "{PDFIUM_LIB_ENV}={} was rejected: {e}\n\
                 It must be the {lib_name} from bblanchon/pdfium-binaries chromium/{PDFIUM_VERSION} \
                 (expected sha256 {lib_sha256}). Unset {PDFIUM_LIB_ENV} to download it instead.",
                local_lib.display()
            );
        }
        println!(
            "cargo:warning=Using local pdfium library from {PDFIUM_LIB_ENV} ({}), sha256 verified",
            local_lib.display()
        );
    } else if !lib_path.exists() {
        println!(
            "cargo:warning=Downloading pdfium library for {}-{}...",
            target_os, target_arch
        );
        if let Err(e) =
            download_with_retries(|| download_and_extract_pdfium(&download_url, tgz_sha256, &libs_dir))
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
    expected_sha256: &str,
    dest_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    let bytes = response.bytes()?;
    // Never unpack an archive that is not the pinned release asset.
    verify_sha256(&bytes, expected_sha256, url)?;

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
// `#[cfg(test)]` target, so the tests below are run through
//   cargo test -p chatty-core --test pdfium_build
// `crates/chatty-core/tests/pdfium_build.rs`, which `include!`s this file too:
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn verify_sha256_accepts_match_and_rejects_mismatch() {
        // sha256("abc")
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(b"abc", abc, "t").is_ok());
        let err = verify_sha256(b"abd", abc, "thing").unwrap_err();
        assert!(err.contains("thing") && err.contains(abc), "{err}");
    }

    #[test]
    fn local_install_copies_only_when_checksum_matches() {
        let dir = std::env::temp_dir().join(format!("pdfium_build_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.so");
        let dest = dir.join("dest.so");
        std::fs::write(&src, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(install_local_pdfium(&src, &dest, &"0".repeat(64)).is_err());
        assert!(!dest.exists(), "a rejected library must not be installed");
        install_local_pdfium(&src, &dest, abc).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        std::fs::remove_dir_all(&dir).ok();
    }

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
