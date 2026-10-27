//! A malformed `execution_settings.json` must stop chatty-tui and name the
//! file and the field, not start with defaults (shell off) (AGE-919).

use std::process::Command;

fn run(config_file: Option<&str>) -> std::process::Output {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let config = base.join("config").join("chatty");
    std::fs::create_dir_all(&config).expect("config dir");
    if let Some(text) = config_file {
        std::fs::write(config.join("execution_settings.json"), text).expect("settings file");
    }
    Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(["--headless", "-m", "hi"])
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .env("CHATTY_SECRET_STORE", "file")
        .output()
        .expect("chatty-tui runs")
}

#[test]
fn a_malformed_execution_settings_file_fails_naming_file_and_field() {
    let out = run(Some("{\n  \"approval_mode\": \"bogus_mode\"\n}"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("execution_settings.json"), "{stderr}");
    assert!(stderr.contains("bogus_mode"), "{stderr}");
    assert!(stderr.contains("line 2"), "{stderr}");
}

#[test]
fn a_missing_execution_settings_file_is_not_that_error() {
    let out = run(None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("execution settings"), "{stderr}");
}
