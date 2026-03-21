use std::env;
use std::fs;
use std::path::PathBuf;

const VERSOVIEW_VERSION: &str = "versoview-v0.0.2";

fn main() {
    setup_versoview();
}

fn setup_versoview() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let bin_dir = out_dir.join("versoview-bin");
    fs::create_dir_all(&bin_dir).ok();

    let (bin_name, archive_name) = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => ("versoview", "verso-aarch64-apple-darwin.tar.gz"),
        ("macos", "x86_64") => ("versoview", "verso-x86_64-apple-darwin.tar.gz"),
        ("linux", "x86_64") => ("versoview", "verso-x86_64-unknown-linux-gnu.tar.gz"),
        ("windows", "x86_64") => ("versoview.exe", "verso-x86_64-pc-windows-msvc.tar.gz"),
        _ => {
            println!(
                "cargo:warning=Unsupported platform for versoview: {}-{}",
                target_os, target_arch
            );
            return;
        }
    };

    let bin_path = bin_dir.join(bin_name);

    if !bin_path.exists() {
        let download_url = format!(
            "https://github.com/versotile-org/versoview-release/releases/download/{}/{}",
            VERSOVIEW_VERSION, archive_name
        );
        println!(
            "cargo:warning=Downloading versoview for {}-{}...",
            target_os, target_arch
        );
        if let Err(e) = download_and_extract(&download_url, &bin_dir, bin_name) {
            println!("cargo:warning=Failed to download versoview: {}", e);
            println!(
                "cargo:warning=Browser session capture will require versoview to be installed manually."
            );
            return;
        }
        println!("cargo:warning=versoview downloaded successfully");
    }

    // Embed the path so engine.rs can find it as a fallback
    println!(
        "cargo:rustc-env=VERSOVIEW_BUNDLED_PATH={}",
        bin_path.display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}

fn download_and_extract(
    url: &str,
    dest_dir: &std::path::Path,
    bin_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    let bytes = response.bytes()?;

    let tar = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes));
    let mut archive = tar::Archive::new(tar);

    // Try bin_name and common alternatives (verso, versoview)
    let candidates = [bin_name, "versoview", "verso", "versoview.exe", "verso.exe"];

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if let Some(file_name) = path.file_name() {
            let file_name_str = file_name.to_string_lossy();
            if candidates.iter().any(|c| file_name_str == *c) {
                let dest_path = dest_dir.join(bin_name);
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

                return Ok(());
            }
        }
    }

    Err(format!(
        "versoview binary not found in archive (tried: {:?})",
        candidates
    )
    .into())
}
