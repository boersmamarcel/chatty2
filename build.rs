use std::env;
use std::fs;
use std::path::PathBuf;

const PDFIUM_VERSION: &str = "7543";

fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app_icon/icon.ico");
        res.compile().unwrap();
    }

    setup_pdfium();
}

fn setup_pdfium() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let libs_dir = PathBuf::from(&manifest_dir).join("libs").join("lib");
    fs::create_dir_all(&libs_dir).ok();

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
        if let Err(e) = download_and_extract(&download_url, &libs_dir) {
            println!("cargo:warning=Failed to download pdfium: {}", e);
            println!("cargo:warning=PDF thumbnail generation will not be available");
            return;
        }
        println!("cargo:warning=Pdfium library downloaded successfully");
    }

    // Tell the binary where to find pdfium at runtime
    println!("cargo:rustc-env=PDFIUM_LIB_DIR={}", libs_dir.display());
    println!("cargo:rerun-if-changed=libs/lib");
}

fn download_and_extract(
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
                if let Ok(metadata) = fs::metadata(&dest_path) {
                    let mut perms = metadata.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&dest_path, perms).ok();
                }
            }
        }
    }

    Ok(())
}
