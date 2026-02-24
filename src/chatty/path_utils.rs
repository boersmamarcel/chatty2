/// Augments the process PATH to include common tool installation directories.
///
/// GUI apps on macOS/Linux do not inherit the shell PATH, so executables installed
/// via Homebrew, npm, pip, uv/uvx, cargo, etc. may not be findable. This probes
/// well-known locations and adds any that exist on disk.
pub fn augment_gui_app_path() {
    use std::sync::OnceLock;

    // OnceLock ensures PATH augmentation runs exactly once for the process lifetime,
    // regardless of how many times this function is called.
    static PATH_AUGMENTED: OnceLock<()> = OnceLock::new();

    PATH_AUGMENTED.get_or_init(|| {
        let current = std::env::var("PATH").unwrap_or_default();
        // ':' is the PATH separator on macOS and Linux only.
        let existing: Vec<&str> = current.split(':').collect();

        let mut candidates: Vec<String> = Vec::new();

        // Standard system / Homebrew paths
        let static_paths = [
            "/opt/homebrew/bin", // Apple Silicon Homebrew
            "/opt/homebrew/sbin",
            "/usr/local/bin", // Intel Homebrew / system
            "/usr/bin",
            "/bin",
        ];
        for p in &static_paths {
            candidates.push(p.to_string());
        }

        // Homebrew keg-only Node installs: probe /opt/homebrew/opt/node*/bin
        // and /usr/local/opt/node*/bin (Intel). These are not symlinked into
        // /opt/homebrew/bin, so npx/node won't be found without them.
        let homebrew_opt_roots = ["/opt/homebrew/opt", "/usr/local/opt"];
        for root in &homebrew_opt_roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                let mut node_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("node") {
                            let bin = format!("{}/{}/bin", root, name);
                            if std::path::Path::new(&bin).exists() {
                                Some(bin)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                // Sort descending so newer versions (node@22 > node@20) appear first
                node_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(node_dirs);
            }
        }

        // nvm-managed Node versions: probe ~/.nvm/versions/node/*/bin
        // Add all installed versions; the default alias is usually first alphabetically
        // (v16 < v20 < v22), so sort descending to prefer newer.
        if let Ok(home) = std::env::var("HOME") {
            let nvm_versions = format!("{}/.nvm/versions/node", home);
            if let Ok(entries) = std::fs::read_dir(&nvm_versions) {
                let mut nvm_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let bin =
                            format!("{}/{}/bin", nvm_versions, e.file_name().to_string_lossy());
                        if std::path::Path::new(&bin).exists() {
                            Some(bin)
                        } else {
                            None
                        }
                    })
                    .collect();
                nvm_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(nvm_dirs);
            }
        }

        // Python / uv / uvx paths: uvx is commonly installed via `uv` (Astral),
        // pipx, or cargo. GUI apps won't find these without explicit PATH entries.
        if let Ok(home) = std::env::var("HOME") {
            // ~/.local/bin — default install location for `uv`, pipx, pip --user
            let local_bin = format!("{}/.local/bin", home);
            if std::path::Path::new(&local_bin).exists() {
                candidates.push(local_bin);
            }

            // ~/.cargo/bin — uv can also be installed via `cargo install uv`
            let cargo_bin = format!("{}/.cargo/bin", home);
            if std::path::Path::new(&cargo_bin).exists() {
                candidates.push(cargo_bin);
            }

            // pyenv shims: ~/.pyenv/shims — if user manages Python via pyenv
            let pyenv_shims = format!("{}/.pyenv/shims", home);
            if std::path::Path::new(&pyenv_shims).exists() {
                candidates.push(pyenv_shims);
            }
        }

        // Homebrew keg-only Python installs: probe /opt/homebrew/opt/python*/bin
        // and /usr/local/opt/python*/bin (Intel). Similar to Node keg-only handling.
        for root in &homebrew_opt_roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                let mut python_dirs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("python") {
                            let bin = format!("{}/{}/bin", root, name);
                            if std::path::Path::new(&bin).exists() {
                                Some(bin)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                // Sort descending so newer versions appear first
                python_dirs.sort_by(|a, b| b.cmp(a));
                candidates.extend(python_dirs);
            }
        }

        // Build the new PATH: prepend candidates that aren't already present,
        // in reverse order so that after insertion at 0 the final order matches
        // the candidates order.
        let mut parts: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        for path in candidates.iter().rev() {
            if !parts.iter().any(|p| p == path) {
                parts.insert(0, path.clone());
            }
        }

        let new_path = parts.join(":");
        if new_path != current {
            tracing::debug!(path = %new_path, "Augmented PATH for GUI app");
            // SAFETY: PATH_AUGMENTED guarantees set_var is called at most once.
            // std::env::set_var is unsafe (UB with concurrent env readers), so we
            // minimise the window to a single call during early startup.
            unsafe {
                std::env::set_var("PATH", new_path);
            }
        }
    });
}
