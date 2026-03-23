fn main() {
    // On macOS, the `dispatch2` crate (pulled in via wry → objc2-web-kit →
    // objc2-core-foundation) links `-ldispatch`.  Non-Apple LLVM toolchains
    // (e.g. Homebrew LLVM) don't automatically search the macOS SDK library
    // paths, so the linker can't find `libdispatch`.  Add the SDK's usr/lib
    // to the native library search path to fix the link.
    #[cfg(target_os = "macos")]
    {
        // Re-run if the SDK root changes (e.g. Xcode update or xcode-select switch).
        println!("cargo:rerun-if-env-changed=SDKROOT");

        if let Ok(output) = std::process::Command::new("xcrun")
            .args(["--show-sdk-path"])
            .output()
            && output.status.success()
        {
            let sdk_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-link-search=native={sdk_path}/usr/lib");
        }
    }
}
