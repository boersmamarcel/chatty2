//! Getting a Chrome binary onto the user's machine without bundling one.
//!
//! We do not ship Chromium in the app bundle — a few hundred MB plus a
//! security-update treadmill we would own. Instead, in order:
//!
//! 1. A pinned Chrome for Testing build we already downloaded.
//! 2. An installed Chrome / Chromium / Edge above a version floor.
//! 3. Download the pinned build, verified against a checksum committed here.
//!
//! The checksum is the trust anchor and it lives in this file. Chrome for
//! Testing publishes no hashes of its own, so bumping [`PINNED_VERSION`] means
//! re-running `scripts/pin-chrome.sh` and committing what it prints. That is a
//! release-time decision, never a background auto-update.

use std::path::{Path, PathBuf};

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use super::error::BrowserError;

/// The Chrome for Testing build we pin to. Bump with `scripts/pin-chrome.sh`.
pub const PINNED_VERSION: &str = "152.0.7977.75";

/// Oldest system Chrome we will drive. Chrome 136 is where
/// `--remote-debugging-port` stopped working against the default profile, which
/// is the behaviour our profile handling assumes.
pub const MIN_CHROME_MAJOR: u32 = 136;

/// How long a provisioning download may take before we give up.
const DOWNLOAD_TIMEOUT_SECS: u64 = 600;

/// One pinned build: where to get it, how to verify it, where the binary sits
/// inside the archive.
struct PinnedBuild {
    url: &'static str,
    sha256: &'static str,
    /// Path to the executable relative to the unpacked archive root.
    exe_rel: &'static str,
}

/// Progress sink for the download, in the range 0.0..=1.0.
///
/// Stage 1 is headless and logs progress; the artifact-window progress bar
/// (AGE-155) subscribes to this same callback.
pub type ProgressSink<'a> = dyn Fn(f32) + Send + Sync + 'a;

/// The pinned build for the platform we were compiled for, if we have one.
fn pinned_build() -> Option<PinnedBuild> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(PinnedBuild {
            url: "https://storage.googleapis.com/chrome-for-testing-public/152.0.7977.75/linux64/chrome-linux64.zip",
            sha256: "a16d36890636bd72251133b27f05825f7f9269c2425b3408fa3a76e10dccd8f1",
            exe_rel: "chrome-linux64/chrome",
        })
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some(PinnedBuild {
            url: "https://storage.googleapis.com/chrome-for-testing-public/152.0.7977.75/mac-arm64/chrome-mac-arm64.zip",
            sha256: "9f279f88b20934303003a435b161d4138daaf26bc354ae94c3d6cd68575e600f",
            exe_rel: "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        })
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some(PinnedBuild {
            url: "https://storage.googleapis.com/chrome-for-testing-public/152.0.7977.75/mac-x64/chrome-mac-x64.zip",
            sha256: "c264fc68e665e679cf8e0af0114ddfbb0bd0356bc5b59e785bfa256e7fdf494c",
            exe_rel: "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        })
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(PinnedBuild {
            url: "https://storage.googleapis.com/chrome-for-testing-public/152.0.7977.75/win64/chrome-win64.zip",
            sha256: "2a749ac992885a9e309254ec4d9045df57a6fa9ed0fb7d30d07efd4d93056860",
            exe_rel: "chrome-win64\\chrome.exe",
        })
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        None
    }
}

/// Root of the browser cache: `<dirs::data_dir>/chatty/browsers/`.
///
/// Matches the pdfium cache convention — `~/Library/Application Support/chatty`
/// on macOS, `~/.local/share/chatty` on Linux, `%APPDATA%\chatty` on Windows.
pub fn browsers_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("chatty").join("browsers"))
}

/// Where the pinned build unpacks to.
fn pinned_install_dir() -> Option<PathBuf> {
    browsers_dir().map(|d| d.join(PINNED_VERSION))
}

/// Resolve a usable Chrome executable, downloading the pinned build if needed.
pub async fn resolve_chrome(progress: Option<&ProgressSink<'_>>) -> Result<PathBuf, BrowserError> {
    if let Some(path) = cached_pinned_chrome() {
        debug!(path = %path.display(), "browser: using cached pinned Chrome");
        return Ok(path);
    }

    if let Some(path) = find_system_chrome() {
        info!(path = %path.display(), "browser: using system Chrome");
        return Ok(path);
    }

    info!(
        version = PINNED_VERSION,
        "browser: no usable Chrome found, downloading pinned Chrome for Testing"
    );
    download_pinned(progress).await
}

/// The pinned build, if it is already unpacked and executable.
fn cached_pinned_chrome() -> Option<PathBuf> {
    let build = pinned_build()?;
    let exe = pinned_install_dir()?.join(build.exe_rel);
    exe.is_file().then_some(exe)
}

/// An installed Chrome / Chromium / Edge at or above [`MIN_CHROME_MAJOR`].
fn find_system_chrome() -> Option<PathBuf> {
    for candidate in system_chrome_candidates() {
        if !candidate.is_file() {
            continue;
        }
        match chrome_major_version(&candidate) {
            Some(major) if major >= MIN_CHROME_MAJOR => return Some(candidate),
            Some(major) => {
                debug!(
                    path = %candidate.display(),
                    major,
                    floor = MIN_CHROME_MAJOR,
                    "browser: system Chrome is below the version floor"
                );
            }
            None => {
                debug!(path = %candidate.display(), "browser: could not read Chrome version");
            }
        }
    }
    None
}

/// Platform default install locations, plus anything on `PATH`.
fn system_chrome_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        out.push(PathBuf::from(
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ));
        out.push(PathBuf::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        for p in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
        ] {
            out.push(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "windows")]
    {
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            if let Ok(dir) = std::env::var(base) {
                out.push(PathBuf::from(&dir).join("Google\\Chrome\\Application\\chrome.exe"));
                out.push(PathBuf::from(&dir).join("Microsoft\\Edge\\Application\\msedge.exe"));
            }
        }
    }

    // Anything the user put on PATH wins over nothing, but ranks after the
    // well-known locations so we do not pick up a stray wrapper script.
    for name in ["google-chrome", "chromium", "chrome"] {
        if let Ok(found) = which::which(name) {
            out.push(found);
        }
    }

    out
}

/// Read the major version by running the binary with `--version`.
fn chrome_major_version(exe: &Path) -> Option<u32> {
    let output = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_chrome_major(&text)
}

/// Pull the major version out of a `--version` line.
///
/// Handles `Google Chrome 152.0.7977.75`, `Chromium 140.0.1.2`, and
/// `Microsoft Edge 141.0.1.2 (official build)`.
fn parse_chrome_major(text: &str) -> Option<u32> {
    text.split_whitespace().find_map(|word| {
        let major = word.split('.').next()?;
        // A bare version token: all digits, and the word had a dot in it.
        (word.contains('.') && !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()))
            .then(|| major.parse().ok())?
    })
}

/// Download, verify, and unpack the pinned build.
async fn download_pinned(progress: Option<&ProgressSink<'_>>) -> Result<PathBuf, BrowserError> {
    let build = pinned_build().ok_or_else(|| {
        BrowserError::Provisioning(format!(
            "no pinned Chrome build for {}-{}, and no system Chrome {}+ was found; \
             install Google Chrome and try again",
            std::env::consts::OS,
            std::env::consts::ARCH,
            MIN_CHROME_MAJOR
        ))
    })?;

    if build.sha256.starts_with("PLACEHOLDER") {
        return Err(BrowserError::Provisioning(format!(
            "the pinned Chrome build for this platform has no committed checksum; \
             install Google Chrome {MIN_CHROME_MAJOR}+ or run scripts/pin-chrome.sh"
        )));
    }

    let install_dir = pinned_install_dir()
        .ok_or_else(|| BrowserError::Provisioning("no user data directory".into()))?;

    let staging = install_dir.with_extension("incoming");
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging).await.map_err(|e| {
        BrowserError::Provisioning(format!("cannot create {}: {e}", staging.display()))
    })?;

    let archive = staging.join("chrome.zip");
    let downloaded = tokio::time::timeout(
        std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS),
        stream_to_file(build.url, &archive, progress),
    )
    .await
    .map_err(|_| {
        BrowserError::Timeout(
            DOWNLOAD_TIMEOUT_SECS,
            format!("downloading Chrome for Testing {PINNED_VERSION}"),
        )
    })?;

    if let Err(e) = downloaded {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e);
    }

    verify_sha256(&archive, build.sha256)
        .await
        .inspect_err(|_| {
            let staging = staging.clone();
            tokio::spawn(async move {
                let _ = tokio::fs::remove_dir_all(&staging).await;
            });
        })?;

    let unpack_root = staging.join("unpacked");
    unzip(&archive, &unpack_root).await?;
    let _ = tokio::fs::remove_file(&archive).await;

    let exe = unpack_root.join(build.exe_rel);
    if !exe.is_file() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(BrowserError::Provisioning(format!(
            "downloaded archive did not contain {}",
            build.exe_rel
        )));
    }
    make_executable(&unpack_root).await;

    // Publish atomically: an interrupted download never leaves a half-unpacked
    // tree at the path `cached_pinned_chrome` checks.
    let _ = tokio::fs::remove_dir_all(&install_dir).await;
    if let Some(parent) = install_dir.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::rename(&unpack_root, &install_dir)
        .await
        .map_err(|e| {
            BrowserError::Provisioning(format!(
                "cannot install into {}: {e}",
                install_dir.display()
            ))
        })?;
    let _ = tokio::fs::remove_dir_all(&staging).await;

    prune_other_versions().await;

    let exe = install_dir.join(build.exe_rel);
    info!(path = %exe.display(), version = PINNED_VERSION, "browser: provisioned Chrome for Testing");
    Ok(exe)
}

/// Stream a URL to disk, reporting fractional progress as it goes.
async fn stream_to_file(
    url: &str,
    path: &Path,
    progress: Option<&ProgressSink<'_>>,
) -> Result<(), BrowserError> {
    let client = crate::services::http_client::default_client(120);
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| BrowserError::Provisioning(format!("download failed: {e}")))?;

    if !response.status().is_success() {
        return Err(BrowserError::Provisioning(format!(
            "download failed: HTTP {}",
            response.status()
        )));
    }

    let total = response.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| BrowserError::Provisioning(format!("cannot write {}: {e}", path.display())))?;

    let mut downloaded: u64 = 0;
    let mut last_logged = 0u64;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| BrowserError::Provisioning(format!("download failed: {e}")))?;
        file.write_all(&chunk).await.map_err(|e| {
            BrowserError::Provisioning(format!("cannot write {}: {e}", path.display()))
        })?;
        downloaded += chunk.len() as u64;

        if total > 0 {
            let fraction = downloaded as f32 / total as f32;
            if let Some(sink) = progress {
                sink(fraction);
            }
            // A ~190MB download must not look like a hang even with no UI.
            let decile = (fraction * 10.0) as u64;
            if decile > last_logged {
                last_logged = decile;
                info!(
                    percent = decile * 10,
                    mb = downloaded / 1_048_576,
                    "browser: downloading Chrome for Testing"
                );
            }
        }
    }

    file.flush()
        .await
        .map_err(|e| BrowserError::Provisioning(format!("cannot flush {}: {e}", path.display())))?;
    Ok(())
}

/// Verify a file against the checksum committed in this module.
async fn verify_sha256(path: &Path, expected: &str) -> Result<(), BrowserError> {
    let path = path.to_path_buf();
    let expected = expected.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)
            .map_err(|e| BrowserError::Provisioning(format!("cannot read download: {e}")))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buf)
                .map_err(|e| BrowserError::Provisioning(format!("cannot hash download: {e}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
        }
        let actual = hex::encode(hasher.finalize());
        if actual.eq_ignore_ascii_case(&expected) {
            Ok(())
        } else {
            Err(BrowserError::Provisioning(format!(
                "checksum mismatch on the Chrome download: expected {expected}, got {actual}"
            )))
        }
    })
    .await
    .map_err(|e| BrowserError::Provisioning(format!("hashing task failed: {e}")))?
}

/// Unpack the archive, preserving the unix mode bits the zip carries.
async fn unzip(archive: &Path, dest: &Path) -> Result<(), BrowserError> {
    let archive = archive.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive)
            .map_err(|e| BrowserError::Provisioning(format!("cannot open archive: {e}")))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| BrowserError::Provisioning(format!("cannot read archive: {e}")))?;
        zip.extract(&dest)
            .map_err(|e| BrowserError::Provisioning(format!("cannot unpack archive: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| BrowserError::Provisioning(format!("unpack task failed: {e}")))?
}

/// `ZipArchive::extract` restores unix modes when the archive stores them, but
/// Chrome's helper binaries are unusable if it did not — set them defensively.
#[cfg(unix)]
async fn make_executable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let root = root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.permissions().mode() & 0o111 == 0 {
                    // Only widen files that carry no execute bit at all; leave
                    // data files alone by checking for the Mach-O/ELF magic.
                    if is_native_executable(&path) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o755);
                        let _ = std::fs::set_permissions(&path, perms);
                    }
                }
            }
        }
    })
    .await;
}

#[cfg(not(unix))]
async fn make_executable(_root: &Path) {}

#[cfg(unix)]
fn is_native_executable(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    // ELF, and the Mach-O / universal-binary magics.
    magic == [0x7f, b'E', b'L', b'F']
        || matches!(
            u32::from_be_bytes(magic),
            0xfeedface | 0xfeedfacf | 0xcafebabe | 0xcefaedfe | 0xcffaedfe
        )
}

/// Keep only the pinned version; older downloads are dead weight.
async fn prune_other_versions() {
    let Some(root) = browsers_dir() else { return };
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == PINNED_VERSION {
            continue;
        }
        debug!(dir = %name, "browser: pruning stale Chrome build");
        if let Err(e) = tokio::fs::remove_dir_all(entry.path()).await {
            warn!(error = ?e, dir = %name, "browser: failed to prune stale Chrome build");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chrome_version_lines() {
        assert_eq!(
            parse_chrome_major("Google Chrome 152.0.7977.75\n"),
            Some(152)
        );
        assert_eq!(parse_chrome_major("Chromium 140.0.1.2\n"), Some(140));
        assert_eq!(
            parse_chrome_major("Microsoft Edge 141.0.1.2 (official build)\n"),
            Some(141)
        );
        assert_eq!(
            parse_chrome_major("Google Chrome for Testing 152.0.7977.75"),
            Some(152)
        );
    }

    #[test]
    fn rejects_unparseable_version_lines() {
        assert_eq!(parse_chrome_major(""), None);
        assert_eq!(parse_chrome_major("not a version"), None);
    }

    #[tokio::test]
    async fn sha256_accepts_matching_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        tokio::fs::write(&path, b"hello").await.unwrap();
        // sha256("hello")
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(&path, expected).await.is_ok());
    }

    #[tokio::test]
    async fn sha256_rejects_corrupt_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        tokio::fs::write(&path, b"hello world").await.unwrap();
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let err = verify_sha256(&path, expected).await.unwrap_err();
        assert!(
            matches!(&err, BrowserError::Provisioning(m) if m.contains("checksum mismatch")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn browsers_dir_is_under_chatty_app_data() {
        if let Some(dir) = browsers_dir() {
            assert!(dir.ends_with("chatty/browsers") || dir.ends_with("chatty\\browsers"));
        }
    }

    #[test]
    fn pinned_build_has_a_plausible_exe_path() {
        if let Some(build) = pinned_build() {
            assert!(build.url.contains(PINNED_VERSION));
            assert!(!build.exe_rel.is_empty());
        }
    }
}
