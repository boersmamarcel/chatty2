//! Tests for `shell_service` (extracted from the production file).

use super::*;

#[tokio::test]
async fn test_basic_command_execution() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let result = session.execute("echo 'hello world'").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("hello world"));
}

#[tokio::test]
async fn test_environment_persistence() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Set an env var
    let result = session.set_env("MY_TEST_VAR", "test_value_123").await;
    assert!(result.is_ok());

    // Verify it persists
    let result = session.execute("echo $MY_TEST_VAR").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.stdout.contains("test_value_123"));
}

#[tokio::test]
async fn test_working_directory_persistence() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Change to /tmp
    let result = session.cd("/tmp").await;
    assert!(result.is_ok());

    // Verify it persists
    let result = session.execute("pwd").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.stdout.contains("/tmp"));
}

#[tokio::test]
async fn test_exit_code_capture() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    let result = session.execute("exit 42").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.exit_code, 42);

    // After `exit 42`, the shell process dies, but it should respawn on the next command.
    let result = session.execute("false").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert_eq!(output.exit_code, 1);
}

#[tokio::test]
async fn test_stderr_captured() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    let result = session.execute("echo 'error message' >&2").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    // stderr is redirected to stdout
    assert!(output.stdout.contains("error message"));
}

#[tokio::test]
async fn test_command_sequence() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Create a file, write to it, read it back
    session.execute("export MYVAR=hello").await.unwrap();
    let result = session.execute("echo $MYVAR").await.unwrap();
    assert!(result.stdout.contains("hello"));

    session.execute("MYVAR=world").await.unwrap();
    let result = session.execute("echo $MYVAR").await.unwrap();
    assert!(result.stdout.contains("world"));
}

#[tokio::test]
async fn test_timeout_enforcement() {
    let session = ShellSession::with_secrets(None, 1, 51200, false, vec![]); // 1 second timeout

    // A timeout is not an error any more: the caller gets back whatever
    // output was captured before the kill, plus a note, instead of losing it
    // (AGE evidence: models were retrying with hand-written `timeout N ...
    // &` wrappers because the old error swallowed all prior output).
    let result = session.execute("sleep 10").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.timed_out);
    assert!(output.stdout.contains("timed out"));
}

#[tokio::test]
async fn test_timeout_preserves_partial_output() {
    let session = ShellSession::with_secrets(None, 1, 51200, false, vec![]); // 1 second timeout

    let result = session
        .execute("echo before-timeout; sleep 10")
        .await
        .unwrap();
    assert!(result.timed_out);
    assert!(
        result.stdout.contains("before-timeout"),
        "expected partial output to be preserved, got: {}",
        result.stdout
    );
}

#[tokio::test]
async fn test_per_call_timeout_override() {
    // The session's configured default is generous; a short per-call
    // override should still fire.
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    let result = session
        .execute_with_timeout("sleep 10", Some(1))
        .await
        .unwrap();
    assert!(result.timed_out);
}

/// A timeout kills the command, not just the shell running it: an orphaned
/// test suite would otherwise keep running (and writing) behind the model's
/// next commands.
#[cfg(unix)]
#[tokio::test]
async fn test_timeout_kills_the_running_command() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    if session.is_sandboxed().await {
        // The sandbox's PID namespace hides the pid; bubblewrap's
        // --die-with-parent covers this case.
        return;
    }

    let result = session
        .execute_with_timeout("sleep 30 & echo \"pid=$!\"; wait", Some(1))
        .await
        .unwrap();
    assert!(result.timed_out);
    let pid: i32 = result
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.trim().parse().ok())
        .unwrap_or_else(|| panic!("no pid in output: {}", result.stdout));

    // Gone, or a zombie waiting for a reaper (the test binary may be PID 1
    // in a container) — either way no longer running.
    let mut alive = true;
    for _ in 0..50 {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.trim_start().chars().next())
            });
        if matches!(state, None | Some('Z') | Some('X')) {
            alive = false;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if alive && std::path::Path::new("/proc").exists() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
        panic!("the timed-out command (pid {pid}) is still running");
    }

    // The session respawns and works after the kill.
    let after = session.execute("echo still-here").await.unwrap();
    assert!(!after.timed_out);
    assert_eq!(after.stdout, "still-here");
}

#[test]
fn test_resolve_call_timeout_seconds_bounds_override() {
    // No override: falls back to the session's configured default.
    assert_eq!(resolve_call_timeout_seconds(30, None), 30);
    // A reasonable override is used as-is.
    assert_eq!(resolve_call_timeout_seconds(30, Some(120)), 120);
    // 0 is not "kill immediately": it falls back to the default.
    assert_eq!(resolve_call_timeout_seconds(30, Some(0)), 30);
    // A caller asking for more than the max is clamped down to it, rather
    // than allowed to block a turn indefinitely.
    assert_eq!(
        resolve_call_timeout_seconds(30, Some(u32::MAX)),
        MAX_SHELL_CALL_TIMEOUT_SECONDS
    );
    assert_eq!(
        resolve_call_timeout_seconds(30, Some(MAX_SHELL_CALL_TIMEOUT_SECONDS + 1)),
        MAX_SHELL_CALL_TIMEOUT_SECONDS
    );
}

#[tokio::test]
async fn test_output_truncation() {
    let session = ShellSession::with_secrets(None, 30, 100, false, vec![]); // 100 byte limit

    let result = session.execute("seq 1 1000").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.truncated);
    assert!(
        output
            .stdout
            .contains("bytes omitted; rerun with a filter (grep/tail)")
    );
    assert!(output.stdout.starts_with("1\n"), "keeps the head");
    assert!(output.stdout.ends_with("1000"), "keeps the tail");
}

#[tokio::test]
async fn test_workspace_restriction() {
    let temp_dir = std::env::temp_dir();
    let workspace = temp_dir.join(format!("chatty_shell_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();

    let session = ShellSession::with_secrets(
        Some(workspace.to_str().unwrap().to_string()),
        30,
        51200,
        false,
        vec![],
    );

    // Should be able to cd within workspace (start is in workspace)
    let result = session.execute("pwd").await;
    assert!(result.is_ok());
    assert!(result.unwrap().stdout.contains(workspace.to_str().unwrap()));

    // Cleanup
    std::fs::remove_dir_all(&workspace).unwrap();
}

#[tokio::test]
async fn test_invalid_env_var_name() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let result = session.set_env("INVALID-NAME", "value").await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid environment variable name")
    );
}

#[tokio::test]
async fn test_status() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Before any command, session is not started
    let status = session.status().await.unwrap();
    assert!(!status.running);

    // Execute a command to start the session
    session.execute("echo 'start'").await.unwrap();

    // Now check status
    let status = session.status().await.unwrap();
    assert!(status.running);
    assert!(status.pid.is_some());
    assert!(!status.cwd.is_empty());
}

#[tokio::test]
async fn test_process_respawn_after_death() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Start a session
    session.execute("echo 'first'").await.unwrap();

    // Kill the process internally
    session.shutdown().await;

    // Next command should respawn
    let result = session.execute("echo 'respawned'").await;
    assert!(result.is_ok());
    assert!(result.unwrap().stdout.contains("respawned"));
}

#[tokio::test]
async fn test_shutdown() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("echo 'test'").await.unwrap();
    assert!(session.is_running().await);

    session.shutdown().await;
    assert!(!session.is_running().await);
}

#[tokio::test]
async fn test_special_characters_in_env_value() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Test value with special characters
    let result = session
        .set_env("SPECIAL_VAR", "hello 'world' \"test\" $HOME")
        .await;
    assert!(result.is_ok());

    let result = session.execute("echo $SPECIAL_VAR").await.unwrap();
    assert!(result.stdout.contains("hello 'world' \"test\""));
}

#[tokio::test]
async fn test_multiline_output() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    let result = session
        .execute("echo 'line1'; echo 'line2'; echo 'line3'")
        .await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.stdout.contains("line1"));
    assert!(output.stdout.contains("line2"));
    assert!(output.stdout.contains("line3"));
}

/// Regression test for AGE-505: a command whose only output has no trailing
/// newline used to glue the shell's end-of-command marker onto the end of
/// that output line, so the reader loop's `starts_with` check never matched
/// it and the call burned the full timeout before failing. Use a short
/// session timeout so a reintroduced bug shows up as a fast `Err` here
/// instead of this test actually waiting it out, and assert the elapsed
/// time stayed far below that timeout to prove the timeout path was never
/// taken.
#[tokio::test]
async fn test_no_trailing_newline_output_completes_without_timeout() {
    let session = ShellSession::with_secrets(None, 3, 51200, false, vec![]);
    // Start the shell first, so the bound below times the command and not
    // the PTY shell's startup (slow on a loaded machine).
    session.execute("true").await.unwrap();

    let start = tokio::time::Instant::now();
    let result = session.execute("printf abc").await;
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "command should complete normally, not time out: {result:?}"
    );
    let output = result.unwrap();
    assert_eq!(output.exit_code, 0);
    // Exactly the command's output: no marker/exit-code text, and no
    // spurious trailing blank line from the marker's forced leading newline.
    assert_eq!(output.stdout, "abc");
    assert!(
        elapsed < tokio::time::Duration::from_secs(2),
        "command took {elapsed:?}, expected near-instant completion (marker-gluing regression would take ~3s)"
    );
}

/// Same AGE-505 shape as above but matching one of the exact patterns from
/// the bug report: writing an answer file with `echo -n` (no trailing
/// newline) and then `cat`-ing it back out.
#[tokio::test]
async fn test_echo_n_then_cat_completes_without_timeout() {
    let temp_dir = std::env::temp_dir();
    let answer_file = temp_dir.join(format!("chatty_shell_test_{}.txt", uuid::Uuid::new_v4()));
    let answer_path = answer_file.to_str().unwrap();

    let session = ShellSession::with_secrets(None, 3, 51200, false, vec![]);
    // Start the shell first, so the bound below times the command and not
    // the PTY shell's startup (slow on a loaded machine).
    session.execute("true").await.unwrap();

    let start = tokio::time::Instant::now();
    let result = session
        .execute(&format!(
            "echo -n \"42\" > {answer_path} && cat {answer_path}"
        ))
        .await;
    let elapsed = start.elapsed();

    let _ = std::fs::remove_file(&answer_file);

    assert!(
        result.is_ok(),
        "command should complete normally, not time out: {result:?}"
    );
    let output = result.unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "42");
    assert!(
        elapsed < tokio::time::Duration::from_secs(2),
        "command took {elapsed:?}, expected near-instant completion (marker-gluing regression would take ~3s)"
    );
}

#[test]
fn test_can_sandbox() {
    let can = ShellSession::can_sandbox();
    // On Linux: depends on bwrap availability
    // On macOS: always true
    // On other platforms: false
    #[cfg(target_os = "macos")]
    assert!(can, "macOS should always support sandboxing");
    let _ = can; // Avoid unused variable warning
}

#[tokio::test]
async fn test_sandboxed_session_persistence() {
    // Verify that sandboxed sessions still maintain state
    if !ShellSession::can_sandbox() {
        eprintln!("skipped: no working sandbox here (ShellSession::can_sandbox() is false)");
        return;
    }

    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);

    // Set env var and verify it persists
    session
        .execute("export SANDBOX_TEST=hello_sandbox")
        .await
        .unwrap();
    let result = session.execute("echo $SANDBOX_TEST").await.unwrap();
    assert!(
        result.stdout.contains("hello_sandbox"),
        "Environment variables should persist in sandboxed session, got: {:?}",
        result.stdout
    );

    // Verify sandbox state
    assert!(session.is_sandboxed().await, "Session should be sandboxed");
}

#[tokio::test]
async fn test_sandboxed_session_with_workspace() {
    if !ShellSession::can_sandbox() {
        eprintln!("skipped: no working sandbox here (ShellSession::can_sandbox() is false)");
        return;
    }

    let temp_dir = std::env::temp_dir();
    let workspace = temp_dir.join(format!("chatty_sandbox_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();

    let session = ShellSession::with_secrets(
        Some(workspace.to_str().unwrap().to_string()),
        30,
        51200,
        false,
        vec![],
    );

    // Should be able to execute commands in workspace
    let result = session.execute("pwd").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.stdout.contains(workspace.to_str().unwrap()),
        "Should start in workspace directory, got: {:?}",
        output.stdout
    );

    // Should be able to create files in workspace
    let result = session
        .execute("echo 'test' > sandbox_test.txt && cat sandbox_test.txt")
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().stdout.contains("test"));

    // Cleanup
    std::fs::remove_dir_all(&workspace).unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandboxed_macos_tmpdir_is_tmp() {
    if !ShellSession::can_sandbox() {
        return;
    }

    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let tmpdir_output = session
        .execute("echo $TMPDIR")
        .await
        .expect("Failed to execute TMPDIR check in sandboxed macOS session");
    assert_eq!(tmpdir_output.stdout.trim(), "/tmp");

    session
        .execute("rm -f \"$TMPDIR/chatty_uv_tmp_test\"")
        .await
        .expect("Failed to clean up prior temp file in sandboxed macOS TMPDIR");
    let write_output = session
            .execute(
                "touch \"$TMPDIR/chatty_uv_tmp_test\" && test -f \"$TMPDIR/chatty_uv_tmp_test\" && echo ok",
            )
            .await
            .expect("Failed to write a temp file in sandboxed macOS TMPDIR");
    session
        .execute("rm -f \"$TMPDIR/chatty_uv_tmp_test\"")
        .await
        .expect("Failed to remove temp file in sandboxed macOS TMPDIR");
    assert!(
        write_output.stdout.contains("ok"),
        "Expected successful temp write in TMPDIR, got: {}",
        write_output.stdout
    );
}

#[tokio::test]
async fn test_network_isolation_flag() {
    // Just verify the session can be created with network_isolation=true
    let session = ShellSession::with_secrets(None, 30, 51200, true, vec![]);
    let result = session.execute("echo 'network test'").await;
    assert!(result.is_ok());
    assert!(result.unwrap().stdout.contains("network test"));
}

#[tokio::test]
async fn test_secret_injection_on_startup() {
    let secrets = vec![
        ("DB_PASSWORD".into(), "s3cret_123".into()),
        ("API_KEY".into(), "key_xyz".into()),
    ];
    let session = ShellSession::with_secrets(None, 30, 51200, false, secrets);

    let r = session.execute("echo $DB_PASSWORD").await.unwrap();
    assert!(r.stdout.contains("s3cret_123"));

    let r = session.execute("echo $API_KEY").await.unwrap();
    assert!(r.stdout.contains("key_xyz"));
}

#[tokio::test]
async fn test_secrets_survive_shell_respawn() {
    let secrets = vec![("PERSIST_KEY".into(), "persist_val".into())];
    let session = ShellSession::with_secrets(None, 30, 51200, false, secrets);

    // First life
    let r = session.execute("echo $PERSIST_KEY").await.unwrap();
    assert!(r.stdout.contains("persist_val"));

    // Kill process
    session.shutdown().await;
    assert!(!session.is_running().await);

    // Second life — secret must survive via re-injection in ensure_started()
    let r = session.execute("echo $PERSIST_KEY").await.unwrap();
    assert!(r.stdout.contains("persist_val"));
}

#[tokio::test]
async fn test_special_characters_in_startup_secret() {
    // Value with single quotes — tests the ensure_started() escaping:
    // value.replace('\'', "'\\''")
    let tricky = "it's a 'quoted' value";
    let secrets = vec![("TRICKY_SECRET".into(), tricky.into())];
    let session = ShellSession::with_secrets(None, 30, 51200, false, secrets);

    // Use env to print the raw value without shell interpretation
    let r = session.execute("env | grep TRICKY_SECRET=").await.unwrap();
    assert!(
        r.stdout.contains(&format!("TRICKY_SECRET={}", tricky)),
        "Expected secret with single quotes to round-trip, got: {:?}",
        r.stdout
    );
}

#[tokio::test]
async fn test_shell_status_masks_secrets() {
    let secrets = vec![("MY_SECRET".into(), "top_secret_value".into())];
    let session = ShellSession::with_secrets(None, 30, 51200, false, secrets);

    // Start the session
    session.execute("echo init").await.unwrap();

    // secret_key_names() should list our key
    let key_names = session.secret_key_names();
    assert!(key_names.contains(&"MY_SECRET".to_string()));

    // Raw status contains the real value
    let status = session.status().await.unwrap();
    let secret_entry = status.env_vars.iter().find(|(k, _)| k == "MY_SECRET");
    assert!(
        secret_entry.is_some(),
        "Secret key should appear in env_vars"
    );
    let (_, raw_value) = secret_entry.unwrap();
    assert_eq!(raw_value, "top_secret_value");

    // Apply the same masking logic used by ShellStatusTool
    let masked: Vec<(String, String)> = status
        .env_vars
        .into_iter()
        .map(|(k, v)| {
            if key_names.contains(&k) {
                (k, "****".to_string())
            } else {
                (k, v)
            }
        })
        .collect();

    let masked_entry = masked.iter().find(|(k, _)| k == "MY_SECRET").unwrap();
    assert_eq!(masked_entry.1, "****", "Secret value should be masked");
}

#[test]
fn bound_output_keeps_head_and_tail_on_char_boundaries() {
    let mut output = format!("{}📈middle📈{}", "a".repeat(499), "z".repeat(499));
    assert!(ShellSession::bound_output(&mut output, 1_000));
    let (head, rest) = output.split_once("\n... [").expect("an omission line");
    let (note, tail) = rest.split_once("] ...\n").expect("the tail follows it");
    assert!(head.chars().all(|c| c == 'a'), "{head}");
    assert!(tail.ends_with(&"z".repeat(499)), "{tail}");
    assert!(note.contains("bytes omitted; rerun with a filter (grep/tail) to see more"));
    assert!(output.len() < 1_100);
}

#[test]
fn bound_output_uses_the_default_cap_under_a_larger_configured_max() {
    let mut small = "x".repeat(SHELL_OUTPUT_DEFAULT_CAP_BYTES);
    assert!(
        !ShellSession::bound_output(&mut small, 51_200),
        "at the cap: whole"
    );

    let mut output = (1..=5_000)
        .map(|n| format!("line {n}\n"))
        .collect::<String>();
    let original = output.len();
    assert!(ShellSession::bound_output(&mut output, 51_200));
    assert!(output.len() < SHELL_OUTPUT_DEFAULT_CAP_BYTES + 100);
    assert!(output.starts_with("line 1\n"));
    assert!(output.trim_end().ends_with("line 5000"));
    let (head, rest) = output.split_once("\n... [").expect("an omission line");
    let (note, tail) = rest.split_once("] ...\n").expect("the tail follows it");
    let omitted = original - head.len() - tail.len();
    assert!(
        note.starts_with(&format!("{omitted} bytes omitted")),
        "{note}"
    );
}

#[test]
fn bound_output_honours_a_smaller_configured_max() {
    let mut output = "y".repeat(3_000);
    assert!(ShellSession::bound_output(&mut output, 1_000));
    assert!(output.contains("[2000 bytes omitted"));
}

#[test]
fn test_exit_code_from_status_preserves_shell_exit_code() {
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 42")
        .status()
        .expect("failed to capture exit status");
    assert_eq!(ShellSession::exit_code_from_status(status), 42);
}

/// A scratch workspace with `home/` inside it, so the profile is visible to
/// a bubblewrap-sandboxed shell too (only the workspace is bound).
fn workspace_with_profile(profile: &str) -> (tempfile::TempDir, String, String) {
    let workspace = tempfile::tempdir().unwrap();
    let home = workspace.path().join("home");
    std::fs::create_dir_all(home.join("bin")).unwrap();
    std::fs::write(home.join(".profile"), profile).unwrap();
    std::fs::write(
        home.join("bin/chatty-profile-tool"),
        "#!/bin/sh\necho tool-ran\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            home.join("bin/chatty-profile-tool"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let workspace_str = workspace.path().to_str().unwrap().to_string();
    let home_str = home.to_str().unwrap().to_string();
    (workspace, workspace_str, home_str)
}

/// The project's environment lives in the login profile (a conda env, PATH
/// additions): the shell must see it from the first command. The profile's
/// own output, a `read` on stdin, a `cd` and a `set -e` must not leak into
/// the session.
#[tokio::test]
async fn test_login_profile_environment_is_loaded() {
    let (_workspace, workspace, home) = workspace_with_profile(
        "echo profile-noise\n\
         echo profile-noise-stderr >&2\n\
         read -r swallowed\n\
         export PATH=\"$HOME/bin:$PATH\"\n\
         export CHATTY_PROFILE_MARK=from-profile\n\
         cd /\n\
         set -e\n",
    );
    let session = ShellSession::with_secrets(Some(workspace.clone()), 30, 51200, false, vec![])
        .with_home(&home);

    let output = session
        .execute("echo \"$CHATTY_PROFILE_MARK\"; chatty-profile-tool; pwd")
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "{output:?}");
    assert_eq!(
        output.stdout,
        format!("from-profile\ntool-ran\n{workspace}"),
        "profile env visible, profile noise and cd not"
    );

    // `set -e` from the profile would end the shell on this failure.
    assert_eq!(session.execute("false").await.unwrap().exit_code, 1);
    let output = session.execute("echo still-here").await.unwrap();
    assert_eq!(output.stdout, "still-here");
}

/// Secrets are injected after the profile, so a profile cannot shadow them.
#[tokio::test]
async fn test_secrets_win_over_the_login_profile() {
    let (_workspace, workspace, home) = workspace_with_profile("export MY_SECRET=from-profile\n");
    let session = ShellSession::with_secrets(
        Some(workspace),
        30,
        51200,
        false,
        vec![("MY_SECRET".to_string(), "from-secrets".to_string())],
    )
    .with_home(&home);
    let output = session.execute("echo \"$MY_SECRET\"").await.unwrap();
    assert_eq!(output.stdout, "from-secrets");
}

/// A profile that hangs or exits costs one bounded wait, then the session
/// runs without it instead of losing the shell.
#[tokio::test]
async fn test_broken_login_profile_falls_back_to_a_plain_shell() {
    for profile in ["sleep 600\n", "exit 3\n"] {
        let (_workspace, workspace, home) = workspace_with_profile(profile);
        let session =
            ShellSession::with_secrets(Some(workspace), 30, 51200, false, vec![]).with_home(&home);
        let started = std::time::Instant::now();
        let output = session.execute("echo works").await.unwrap();
        assert_eq!(output.stdout, "works", "profile {profile:?}");
        assert!(
            started.elapsed() < LOGIN_PROFILE_TIMEOUT + std::time::Duration::from_secs(10),
            "profile {profile:?} took {:?}",
            started.elapsed()
        );
        assert!(!session.load_login_profile.load(Ordering::Relaxed));
    }
}

/// Debian's `/etc/profile` resets `PATH`; entries the shell inherited (a
/// container's `ENV PATH`, e.g. `/usr/local/cargo/bin`) must survive it,
/// after whatever the profile put first. A `set -x` in the profile must not
/// trace every later command into its output.
#[tokio::test]
async fn test_login_profile_keeps_inherited_path_and_drops_xtrace() {
    let (_workspace, workspace, home) =
        workspace_with_profile("PATH=\"$HOME/bin:/usr/bin:/bin\"\nexport PATH\nset -x\n");
    let session =
        ShellSession::with_secrets(Some(workspace), 30, 51200, false, vec![]).with_home(&home);

    let output = session.execute("echo \"$PATH\"").await.unwrap();
    assert_eq!(output.exit_code, 0, "{output:?}");
    let path = output.stdout;
    assert!(
        path.starts_with(&format!("{home}/bin:/usr/bin:/bin")),
        "profile PATH first: {path}"
    );
    let entries: Vec<&str> = path.split(':').collect();
    for inherited in std::env::var("PATH").unwrap().split(':') {
        if !inherited.is_empty() {
            assert!(entries.contains(&inherited), "{inherited} lost from {path}");
        }
    }
    assert_eq!(
        entries.len(),
        entries
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "no duplicates: {path}"
    );

    let output = session.execute("echo plain").await.unwrap();
    assert_eq!(output.stdout, "plain", "no xtrace in output");
}

// ── Characterization (AGE-585) ───────────────────────────────────────────────
// Written against the piped shell before it moved onto a PTY; the PTY shell
// must produce the same results.

/// Output text exactly: lines, blank lines, leading and trailing blanks
/// within lines, tabs, non-ASCII, stderr merged in order.
#[tokio::test]
async fn characterize_output_text_is_exact() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let cases: [(&str, &str); 6] = [
        ("printf 'a\\nb\\n'; echo err >&2; echo c", "a\nb\nerr\nc"),
        ("printf 'a\\n\\n\\nb\\n'", "a\n\n\nb"),
        ("printf '  x  \\ny\\n'", "  x  \ny"),
        ("printf 'a\\tb\\n'", "a\tb"),
        ("echo 'héllo 📈'", "héllo 📈"),
        ("true", ""),
    ];
    for (command, expected) in cases {
        let output = session.execute(command).await.unwrap();
        assert_eq!(output.stdout, expected, "{command}");
        assert_eq!(output.exit_code, 0, "{command}");
        assert!(!output.truncated && !output.timed_out, "{command}");
    }
}

/// Exit codes round-trip, the session survives a failing command, and an
/// `exit` ends the shell with its code and no output.
#[tokio::test]
async fn characterize_exit_codes() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    for (command, code) in [
        ("false", 1),
        ("sh -c 'exit 7'", 7),
        ("(exit 3)", 3),
        ("true", 0),
    ] {
        assert_eq!(
            session.execute(command).await.unwrap().exit_code,
            code,
            "{command}"
        );
    }
    let output = session.execute("echo bye; exit 5").await.unwrap();
    assert_eq!((output.stdout.as_str(), output.exit_code), ("bye", 5));
    assert!(!session.is_running().await);
    assert_eq!(session.execute("echo back").await.unwrap().stdout, "back");
}

/// The truncation message, byte for byte.
#[tokio::test]
async fn characterize_truncation_is_exact() {
    let session = ShellSession::with_secrets(None, 30, 100, false, vec![]);
    let output = session.execute("seq 1 1000").await.unwrap();
    // The piped shell's end marker started with a newline, so the text the
    // cap measured was the output plus one `\n`.
    let mut expected: String = (1..=1000).map(|n| format!("{n}\n")).collect::<String>() + "\n";
    assert!(ShellSession::bound_output(&mut expected, 100));
    assert_eq!(output.stdout, expected.trim_end());
    assert!(output.truncated);
}

/// The timeout result, byte for byte, and the session restarts clean.
#[tokio::test]
async fn characterize_timeout_is_exact() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("export BEFORE=1; cd /tmp").await.unwrap();
    let output = session
        .execute_with_timeout("echo partial; sleep 10", Some(1))
        .await
        .unwrap();
    assert_eq!(
        output.stdout,
        "partial\n[shell_execute: command timed out after 1 seconds and was killed; \
         output above is partial. The shell session was restarted: the working \
         directory and any exported variables are back to their defaults.]"
    );
    assert_eq!(output.exit_code, -1);
    assert!(output.timed_out && !output.truncated);
    let after = session.execute("echo \"[$BEFORE]\"").await.unwrap();
    assert_eq!(after.stdout, "[]");
}

/// A command that waits on stdin runs into the timeout.
#[tokio::test]
async fn characterize_stdin_reader_times_out() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session.execute_with_timeout("cat", Some(1)).await.unwrap();
    assert!(output.timed_out, "{output:?}");
    assert_eq!(session.execute("echo ok").await.unwrap().stdout, "ok");
}

/// cd, set_env and exported variables persist; cwd is tracked exactly.
#[tokio::test]
async fn characterize_state_persists() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.cd("/tmp").await.unwrap();
    session.set_env("CHATTY_A", "it's $x").await.unwrap();
    let output = session.execute("pwd; echo \"$CHATTY_A\"").await.unwrap();
    assert_eq!(output.stdout, "/tmp\nit's $x");
    let status = session.status().await.unwrap();
    assert_eq!(status.cwd, "/tmp");
    assert!(
        status
            .env_vars
            .contains(&("CHATTY_A".into(), "it's $x".into()))
    );
}

// ── The shell on a PTY (AGE-585) ─────────────────────────────────────────────

/// A scratch git repository with two commits.
fn git_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    for n in 1..=2 {
        std::fs::write(dir.path().join("f"), n.to_string()).unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", &format!("commit number {n}")]);
    }
    (dir, path)
}

/// `git log` on a terminal would open a pager and wait for a key.
#[tokio::test]
async fn git_log_returns_without_a_pager() {
    let (_dir, repo) = git_repo();
    let session = ShellSession::with_secrets(Some(repo), 10, 51200, false, vec![]);
    // Start the shell first, so the bound below times `git log` alone.
    session.execute("true").await.unwrap();
    let started = std::time::Instant::now();
    let output = session.execute("git log").await.unwrap();
    assert!(!output.timed_out, "{output:?}");
    assert_eq!(output.exit_code, 0, "{output:?}");
    assert!(output.stdout.contains("commit number 2"), "{output:?}");
    assert!(output.stdout.contains("commit number 1"), "{output:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

/// Colours are for the terminal view, not the model.
#[tokio::test]
async fn colourised_output_comes_back_as_plain_text() {
    let (_dir, repo) = git_repo();
    let session = ShellSession::with_secrets(Some(repo), 30, 51200, false, vec![]);
    std::fs::create_dir(format!("{}/subdir", session.workspace_dir().unwrap())).unwrap();
    let output = session.execute("ls --color=always -1").await.unwrap();
    assert_eq!(output.stdout, "f\nsubdir", "{:?}", output.stdout);
    let output = session
        .execute("git -c color.ui=always log --oneline -1")
        .await
        .unwrap();
    assert!(!output.stdout.contains('\x1b'), "{:?}", output.stdout);
    assert!(
        output.stdout.ends_with("commit number 2"),
        "{:?}",
        output.stdout
    );
}

/// A progress line redrawn with `\r` comes back as it ended.
#[tokio::test]
async fn carriage_return_progress_collapses_to_its_last_state() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session.execute("printf 'a\\r b\\r c\\n'").await.unwrap();
    assert_eq!(output.stdout.trim(), "c");
    let output = session
        .execute("for i in 1 2 3; do printf '\\r\\033[K%s%%' $((i*33)); done; echo; echo done")
        .await
        .unwrap();
    assert_eq!(output.stdout, "99%\ndone");
}

/// The output is read from the stream, not the grid: a line wider than the
/// terminal comes back as one line.
#[tokio::test]
async fn long_lines_do_not_wrap_at_the_terminal_width() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session.execute("printf '%0500d\\n' 0").await.unwrap();
    assert_eq!(output.stdout, "0".repeat(500));
}

/// A multi-line command runs as one unit: one result, the last exit code,
/// tabs in it are text (not completion), heredocs work.
#[tokio::test]
async fn multi_line_commands_run_as_one() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session
        .execute("for i in 1 2; do\n\techo \"n=$i\"\ndone\ncat <<'EOF'\n\tx!y\nEOF\nfalse")
        .await
        .unwrap();
    assert_eq!(output.stdout, "n=1\nn=2\n\tx!y");
    assert_eq!(output.exit_code, 1);
}

/// What an interactive shell would do differently is switched off: `!`
/// history expansion, aliases from the profile, job control.
#[tokio::test]
async fn interactive_shell_quirks_are_off() {
    let (_workspace, workspace, home) = workspace_with_profile("alias echo='echo aliased'\n");
    let session =
        ShellSession::with_secrets(Some(workspace), 30, 51200, false, vec![]).with_home(&home);
    let output = session
        .execute("echo \"hi!\" 'a!b'; echo $-")
        .await
        .unwrap();
    let lines: Vec<&str> = output.stdout.lines().collect();
    assert_eq!(lines[0], "hi! a!b", "{output:?}");
    assert!(
        !lines[1].contains('m') && !lines[1].contains('H'),
        "{output:?}"
    );
}

/// The agent's terminal: pagers print, the width is wide, TERM is set.
#[tokio::test]
async fn pty_environment_for_the_agent() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session
        .execute("echo $PAGER $GIT_PAGER $MANPAGER $LESS $TERM; tput cols; test -t 1 && echo tty")
        .await
        .unwrap();
    assert_eq!(output.stdout, "cat cat cat -FRX xterm-256color\n200\ntty");
}

/// Non-UTF-8 output is decoded lossily, not dropped.
#[tokio::test]
async fn non_utf8_output_is_decoded_lossily() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session.execute("printf '\\377\\376abc\\n'").await.unwrap();
    assert!(output.stdout.ends_with("abc"), "{:?}", output.stdout);
}

/// The marks are invisible: a terminal view shows a prompt, the command and
/// its output, and nothing of the protocol.
#[tokio::test]
async fn terminal_view_shows_no_marks() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("echo visible-output").await.unwrap();
    let terminal = session.terminal().expect("a running terminal");
    // The tap sees the bytes just before the grid does: wait for the next
    // prompt to be drawn.
    let mut screen = String::new();
    for _ in 0..100 {
        screen = terminal.snapshot(chatty_terminal::Region::Screen).text;
        if screen.lines().count() >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(screen.contains("echo visible-output"), "{screen}");
    assert!(screen.contains("\nvisible-output\n"), "{screen}");
    for leak in ["133;", "CHATTY", "__chatty", "\x1b"] {
        assert!(!screen.contains(leak), "{leak:?} in {screen}");
    }
    // Prompt + command, output, prompt: the line that ran the command file
    // was redrawn as the command itself.
    let lines: Vec<&str> = screen.lines().collect();
    assert_eq!(lines.len(), 3, "{screen}");
    // `$`, or `#` as root.
    let prompt_end = |l: &str| l.ends_with('$') || l.ends_with('#');
    let command_line = lines[0].strip_suffix(" echo visible-output");
    assert!(command_line.is_some_and(prompt_end), "{screen}");
    assert_eq!(lines[1], "visible-output");
    assert!(prompt_end(lines[2]), "{screen}");
}

/// Wait until the terminal's screen contains `needle`.
async fn wait_for_screen(terminal: &chatty_terminal::TerminalHandle, needle: &str) {
    for _ in 0..100 {
        if terminal
            .snapshot(chatty_terminal::Region::Screen)
            .text
            .contains(needle)
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!(
        "{needle:?} never showed: {}",
        terminal.snapshot(chatty_terminal::Region::Screen).text
    );
}

/// Wait until the tap's state satisfies `check`.
async fn wait_for_state(session: &ShellSession, check: impl Fn(&TapState) -> bool) {
    for _ in 0..400 {
        {
            let process = session.process.lock().await;
            if check(&process.as_ref().unwrap().tap.lock()) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let process = session.process.lock().await;
    let state = process.as_ref().unwrap().tap.lock();
    panic!(
        "the shell never reached the expected state: {:?}, typed {:?}",
        state.prompt,
        state.scanner.text().text()
    );
}

/// Shared keyboard: while someone types at the prompt the agent's command
/// is not sent (it would be glued onto their line).
#[tokio::test]
async fn busy_while_someone_is_typing() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("true").await.unwrap();
    let terminal = session.terminal().unwrap();
    // They type at the prompt they see, and the shell echoes it.
    wait_for_state(&session, |state| state.prompt == Prompt::Reading).await;
    terminal.write(b"echo half-typ").unwrap();
    wait_for_state(&session, |state| state.busy().is_some()).await;

    let err = session.execute("echo agent").await.unwrap_err().to_string();
    assert!(
        err.contains("the terminal is busy: someone is typing"),
        "{err}"
    );

    // Once they clear their line, the agent gets the prompt back.
    terminal.write(b"\x15").unwrap(); // Ctrl+U
    let output = session.execute("echo agent").await.unwrap();
    assert_eq!(output.stdout, "agent");
}

/// Shared keyboard: a command started from the terminal view keeps it; the
/// agent is told what is running instead of typing into it.
#[tokio::test]
async fn busy_while_a_command_runs() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("true").await.unwrap();
    let terminal = session.terminal().unwrap();
    wait_for_state(&session, |state| state.prompt == Prompt::Reading).await;
    terminal.write(b"sleep 30\r").unwrap();
    wait_for_state(&session, |state| {
        matches!(&state.prompt, Prompt::Running { command_line } if command_line == "sleep 30")
    })
    .await;

    let err = session.execute("echo agent").await.unwrap_err().to_string();
    assert!(
        err.contains("the terminal is busy: `sleep 30` is running"),
        "{err}"
    );

    terminal.write(b"\x03").unwrap(); // Ctrl+C
    let output = session.execute("echo agent").await.unwrap();
    assert_eq!(output.stdout, "agent");
}

/// Ctrl+C reaches the running command: the shell keeps its controlling
/// terminal, inside the sandbox too (no `--new-session`).
#[tokio::test]
async fn ctrl_c_interrupts_the_running_command() {
    let session = Arc::new(ShellSession::with_secrets(None, 60, 51200, false, vec![]));
    session.execute("true").await.unwrap();
    let sandboxed = session.is_sandboxed().await;
    let terminal = session.terminal().unwrap();

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.execute("echo started; sleep 100; echo after").await }
    });
    wait_for_screen(&terminal, "\nstarted").await;
    let started = std::time::Instant::now();
    terminal.write(b"\x03").unwrap();

    let output = running.await.unwrap().unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{output:?}"
    );
    assert!(!output.timed_out, "sandboxed={sandboxed} {output:?}");
    assert_eq!(output.exit_code, 130, "sandboxed={sandboxed} {output:?}");
    assert!(!output.stdout.contains("after"), "{output:?}");
    // Same shell, still there.
    assert_eq!(session.execute("echo same").await.unwrap().stdout, "same");
}

/// The bubblewrap path, run for real where bubblewrap works (it needs user
/// namespaces; not every machine allows them, and `can_sandbox()` runs it
/// to find out).
#[tokio::test]
async fn sandboxed_shell_runs_on_the_pty() {
    if !ShellSession::can_sandbox() {
        eprintln!("skipped: no working sandbox here (ShellSession::can_sandbox() is false)");
        return;
    }
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session
        .execute("test -t 0 && echo tty; test -e /home || echo no-home")
        .await
        .unwrap();
    assert!(session.is_sandboxed().await);
    assert_eq!(output.stdout, "tty\nno-home", "{output:?}");
}

// ── Review round (AGE-585): delivery through a command file, prompt loss ─────

/// A stock Ubuntu `~/.bashrc`: returns early unless interactive, then sets
/// its own `PS1` (with a title escape), an alias, shell options.
const UBUNTU_BASHRC: &str = r#"case $- in
    *i*) ;;
      *) return;;
esac
HISTCONTROL=ignoreboth
shopt -s checkwinsize
PS1='${debian_chroot:+($debian_chroot)}\u@\h:\w\$ '
case "$TERM" in
xterm*|rxvt*)
    PS1="\[\e]0;${debian_chroot:+($debian_chroot)}\u@\h: \w\a\]$PS1"
    ;;
esac
alias ls='ls --color=auto'
"#;

/// `source ~/.bashrc && conda activate …` replaces `PS1` in an interactive
/// shell. The marks must come back (keeping the new prompt and a venv
/// prefix), and later commands must run, not report a busy terminal.
#[tokio::test]
async fn sourcing_a_stock_bashrc_does_not_wedge_the_session() {
    let (_workspace, workspace, home) = workspace_with_profile("");
    std::fs::write(format!("{home}/.bashrc"), UBUNTU_BASHRC).unwrap();
    let session =
        ShellSession::with_secrets(Some(workspace), 30, 51200, false, vec![]).with_home(&home);

    let output = session
        .execute("source ~/.bashrc && echo sourced")
        .await
        .unwrap();
    assert_eq!(output.stdout, "sourced");
    for n in 1..=3 {
        let output = session.execute(&format!("echo after-{n}")).await.unwrap();
        assert_eq!(output.stdout, format!("after-{n}"));
    }
    // A venv/conda-style prefix is kept, inside the marks.
    session.execute("PS1=\"(venv) $PS1\"").await.unwrap();
    let output = session.execute("cd /tmp && pwd").await.unwrap();
    assert_eq!(output.stdout, "/tmp");
    let terminal = session.terminal().unwrap();
    wait_for_screen(&terminal, "(venv) ").await;
    assert_eq!(session.execute("echo last").await.unwrap().stdout, "last");
    // Typing detection still lines up with the new prompt.
    terminal.write(b"x").unwrap();
    wait_for_state(&session, |state| state.busy().is_some()).await;
    terminal.write(b"\x7f").unwrap(); // Backspace
    assert_eq!(
        session
            .execute("echo typed-and-erased")
            .await
            .unwrap()
            .stdout,
        "typed-and-erased"
    );
}

/// A command that replaces `PROMPT_COMMAND` and `PS1` outright: the end mark
/// does not depend on them, and both are put back (the new prompt command
/// still runs, after ours).
#[tokio::test]
async fn replacing_prompt_command_and_ps1_keeps_working() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session
        .execute("PROMPT_COMMAND='CHATTY_PC=ran'; PS1='plain> '; echo replaced")
        .await
        .unwrap();
    assert_eq!(output.stdout, "replaced");
    let output = session
        .execute("echo \"$CHATTY_PC $PROMPT_COMMAND\"")
        .await
        .unwrap();
    assert_eq!(output.stdout, "ran __chatty_prompt;CHATTY_PC=ran");
    let output = session
        .execute("unset PROMPT_COMMAND; PS1=; false")
        .await
        .unwrap();
    assert_eq!(output.exit_code, 1);
    assert_eq!(session.execute("echo fine").await.unwrap().stdout, "fine");
}

/// The command runs in the shell itself, not in a function: `declare` makes
/// globals, `$?` is left as typing would leave it.
#[tokio::test]
async fn command_state_persists_like_typing() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session
        .execute("declare -a arr=(x y); f() { echo \"f:$1\"; }; (exit 4)")
        .await
        .unwrap();
    let output = session.execute("echo \"$? ${arr[1]}\"; f z").await.unwrap();
    assert_eq!(output.stdout, "4 y\nf:z");
}

/// The command file is private and gone once the command finished.
#[cfg(unix)]
#[tokio::test]
async fn command_files_are_private_and_removed() {
    use std::os::unix::fs::PermissionsExt;
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session
        .execute("stat -c %a \"${__chatty_r%/*}\" \"${__chatty_r%/*}\"/cmd-*")
        .await
        .unwrap();
    assert_eq!(output.stdout, "700\n600", "{output:?}");
    let dir = session
        .process
        .lock()
        .await
        .as_ref()
        .unwrap()
        .dir
        .path()
        .to_path_buf();
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left, vec!["run".to_string()]);
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

/// Wait for the prompt after a command, then pretend its marks never came.
async fn lose_prompt(session: &ShellSession) {
    for _ in 0..100 {
        {
            let process = session.process.lock().await;
            let mut state = process.as_ref().unwrap().tap.lock();
            if state.prompt == Prompt::Reading {
                state.prompt = Prompt::Between;
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no prompt after the command");
}

/// Stuck with no prompt, nothing running and nobody typing (not after our
/// own command): the session restarts the shell instead of refusing
/// forever, and says so.
#[tokio::test]
async fn a_lost_prompt_restarts_the_shell() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.execute("export BEFORE=1").await.unwrap();
    lose_prompt(&session).await;
    {
        let process = session.process.lock().await;
        process.as_ref().unwrap().tap.lock().agent_last = false;
    }
    let output = session.execute("echo \"[$BEFORE]\"").await.unwrap();
    assert!(output.stdout.starts_with("[]\n"), "{output:?}");
    assert!(
        output.stdout.contains("lost its prompt and was restarted"),
        "{output:?}"
    );

    // Right after our own command, a missing prompt is just missing marks.
    session.execute("export KEPT=1").await.unwrap();
    lose_prompt(&session).await;
    {
        let process = session.process.lock().await;
        assert!(process.as_ref().unwrap().tap.lock().agent_last);
    }
    let output = session.execute("echo \"[$KEPT]\"").await.unwrap();
    assert_eq!(output.stdout, "[1]");
}

/// The multi-line protocol check run against each bash below.
async fn check_protocol(session: &ShellSession, version: &str) {
    let output = session
        .execute("for i in 1 2; do\n\techo \"n=$i\"\ndone\ncat <<'EOF'\nline1\n\tx!y\nEOF\nfalse")
        .await
        .unwrap_or_else(|e| panic!("bash {version}: {e}"));
    assert_eq!(output.stdout, "n=1\nn=2\nline1\n\tx!y", "bash {version}");
    assert_eq!(output.exit_code, 1, "bash {version}");
    let output = session.execute("cd /tmp && X=5").await.unwrap();
    assert_eq!(output.exit_code, 0, "bash {version}");
    let output = session
        .execute("pwd; echo \"$X\"; echo err >&2")
        .await
        .unwrap();
    assert_eq!(output.stdout, "/tmp\n5\nerr", "bash {version}");
    let output = session.execute("echo bye; exit 3").await.unwrap();
    assert_eq!(
        (output.stdout.as_str(), output.exit_code),
        ("bye", 3),
        "bash {version}"
    );
    assert_eq!(
        session.execute("echo back").await.unwrap().stdout,
        "back",
        "bash {version}"
    );
    // `set -e` ends the shell on the failure, as it did typed.
    let output = session.execute("set -e; false; echo no").await.unwrap();
    assert_eq!(
        (output.stdout.as_str(), output.exit_code),
        ("", 1),
        "bash {version}"
    );
    assert_eq!(
        session.execute("echo again").await.unwrap().stdout,
        "again",
        "bash {version}"
    );
}

/// Removes the test's containers (one per shell it spawned) when the test
/// ends, however it ends.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let listed = std::process::Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("label=chatty-shell-test={}", self.0),
            ])
            .output();
        if let Ok(listed) = listed {
            for id in String::from_utf8_lossy(&listed.stdout).split_whitespace() {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", id])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}

/// A session whose shell is `bash:<version>` in a container, or `None`
/// (skip) where docker or the image is not available.
fn docker_bash(version: &str) -> Option<(ShellSession, Container)> {
    let image = format!("bash:{version}");
    let quiet = |args: &[&str]| {
        std::process::Command::new("docker")
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if !quiet(&["image", "inspect", &image]) && !quiet(&["pull", "-q", &image]) {
        eprintln!("skipping bash {version}: docker or {image} not available");
        return None;
    }
    let name = format!("chatty-shell-test-{}", uuid::Uuid::new_v4().simple());
    let mut session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let container = name.clone();
    session.test_shell = Some(Box::new(move |dir| {
        let dir = dir.to_string_lossy();
        let label = format!("chatty-shell-test={container}");
        let mut args: Vec<String> = ["run", "--rm", "-i", "-t", "--label", &label]
            .map(String::from)
            .to_vec();
        args.extend(["-v".into(), format!("{dir}:{dir}:ro")]);
        for var in ["__CHATTY_INIT", "PROMPT_COMMAND", "TERM", "COLUMNS"] {
            args.extend(["-e".into(), var.into()]);
        }
        args.extend([image.clone(), "bash".into()]);
        args.extend(bash_args());
        TerminalConfig {
            shell: Some("docker".into()),
            args,
            ..TerminalConfig::default()
        }
    }));
    Some((session, Container(name)))
}

#[tokio::test]
async fn protocol_works_on_bash_3_2() {
    // macOS's /bin/bash.
    if let Some((session, _container)) = docker_bash("3.2") {
        check_protocol(&session, "3.2").await;
    }
}

#[tokio::test]
async fn protocol_works_on_bash_4_4() {
    if let Some((session, _container)) = docker_bash("4.4") {
        check_protocol(&session, "4.4").await;
    }
}

#[tokio::test]
async fn protocol_works_on_bash_5_0() {
    if let Some((session, _container)) = docker_bash("5.0") {
        check_protocol(&session, "5.0").await;
    }
}

#[tokio::test]
async fn protocol_works_on_bash_5_1() {
    if let Some((session, _container)) = docker_bash("5.1") {
        check_protocol(&session, "5.1").await;
    }
}

#[tokio::test]
async fn protocol_works_on_the_host_bash() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    check_protocol(&session, "host").await;
}

// --- The Agent tab (AGE-586): what a view of the agent's shell reads ---

/// Wait until `check` holds for the session, polling its public accessors.
async fn wait_until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("never reached: {what}");
}

/// "Start shell" starts the shell the agent then uses: the same terminal,
/// with the session's secrets, not a second one.
#[tokio::test]
async fn start_starts_the_agents_own_shell() {
    let session = ShellSession::with_secrets(
        None,
        30,
        51200,
        false,
        vec![("AGE586_SECRET".into(), "s3cret".into())],
    );
    assert_eq!(session.activity(), ShellActivity::NotStarted);
    assert!(session.terminal().is_none());
    assert_eq!(session.running_sandboxed(), None);

    session.start().await.unwrap();
    let terminal = session.terminal().expect("started");
    assert_eq!(
        session.running_sandboxed(),
        Some(session.is_sandboxed().await)
    );
    wait_until("idle", || session.activity() == ShellActivity::Idle).await;

    let output = session.execute("echo $AGE586_SECRET").await.unwrap();
    assert_eq!(output.stdout, "s3cret");
    assert!(Arc::ptr_eq(&terminal, &session.terminal().unwrap()));
    // Starting again is a no-op.
    session.start().await.unwrap();
    assert!(Arc::ptr_eq(&terminal, &session.terminal().unwrap()));
}

/// The status line: idle, the agent running its command, the human typing,
/// the human's command running.
#[tokio::test]
async fn activity_tells_the_agent_from_the_human() {
    let session = Arc::new(ShellSession::with_secrets(None, 30, 51200, false, vec![]));
    session.start().await.unwrap();
    let terminal = session.terminal().unwrap();
    wait_until("idle", || session.activity() == ShellActivity::Idle).await;

    terminal.write(b"echo half").unwrap();
    wait_until("typing", || {
        session.activity() == ShellActivity::HumanTyping
    })
    .await;
    terminal.write(b"\x15").unwrap(); // Ctrl+U
    wait_until("idle again", || session.activity() == ShellActivity::Idle).await;

    terminal.write(b"sleep 30\r").unwrap();
    wait_until("human running", || {
        session.activity()
            == ShellActivity::HumanRunning {
                command: "sleep 30".into(),
            }
    })
    .await;
    terminal.write(b"\x03").unwrap();
    wait_until("idle after ^C", || {
        session.activity() == ShellActivity::Idle
    })
    .await;

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.execute("sleep 1; echo slept").await }
    });
    wait_until("agent running", || {
        session.activity()
            == ShellActivity::AgentRunning {
                command: "sleep 1; echo slept".into(),
            }
    })
    .await;
    assert_eq!(running.await.unwrap().unwrap().stdout, "slept");
    assert_eq!(session.activity(), ShellActivity::Idle);
}

/// Mid-typing: the agent's command waits, and says which command waits,
/// until the human clears the line; then it runs.
#[tokio::test]
async fn a_waiting_agent_command_is_visible_until_the_line_is_cleared() {
    let session = Arc::new(ShellSession::with_secrets(None, 30, 51200, false, vec![]));
    session.start().await.unwrap();
    let terminal = session.terminal().unwrap();
    wait_until("idle", || session.activity() == ShellActivity::Idle).await;
    assert_eq!(session.waiting_command(), None);

    terminal.write(b"echo partial").unwrap();
    wait_until("typing", || {
        session.activity() == ShellActivity::HumanTyping
    })
    .await;
    let queued = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.execute("echo from-agent").await }
    });
    wait_until("waiting", || {
        session.waiting_command().as_deref() == Some("echo from-agent")
    })
    .await;
    terminal.write(b"\x15").unwrap(); // Ctrl+U: the line is cleared

    let output = queued.await.unwrap().unwrap();
    assert_eq!(output.stdout, "from-agent");
    assert_eq!(session.waiting_command(), None);
}

/// Handover: the human answers a prompt the agent's command is waiting on,
/// and the answer is in the tool result.
#[tokio::test]
async fn the_human_answers_the_agents_prompt() {
    let session = Arc::new(ShellSession::with_secrets(None, 30, 51200, false, vec![]));
    session.start().await.unwrap();
    let terminal = session.terminal().unwrap();

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .execute("read -p 'continue? ' x; echo \"got $x\"")
                .await
        }
    });
    wait_for_screen(&terminal, "continue? ").await;
    terminal.write(b"yes\r").unwrap();

    let output = running.await.unwrap().unwrap();
    assert_eq!(output.exit_code, 0, "{output:?}");
    assert!(output.stdout.contains("got yes"), "{output:?}");
}

/// Every grid row with its text and the OSC 8 link on it, if any, oldest
/// first.
fn rows_with_links(terminal: &chatty_terminal::TerminalHandle) -> Vec<(String, Option<String>)> {
    use chatty_terminal::alacritty_terminal::grid::Dimensions;
    use chatty_terminal::alacritty_terminal::index::Line;
    terminal.with_term(|term| {
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32;
        (top..bottom)
            .map(|line| {
                let row = &grid[Line(line)];
                let cells = &row[..];
                let text: String = cells.iter().map(|c| c.c).collect();
                let link = cells
                    .iter()
                    .find_map(|c| c.hyperlink().map(|h| h.uri().to_string()));
                (text.trim_end().to_string(), link)
            })
            .collect()
    })
}

/// Attribution: the line the agent's command shows on carries its id (an
/// invisible OSC 8 link); a typed line and the output do not. The model's
/// output is unchanged by it.
#[tokio::test]
async fn agent_command_lines_carry_their_id() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    let output = session.execute("echo agent-$((40+2))").await.unwrap();
    assert_eq!(output.stdout, "agent-42");
    let terminal = session.terminal().unwrap();
    let (id, command) = session.agent_commands().pop().unwrap();
    assert_eq!(command, "echo agent-$((40+2))");

    wait_until("prompt", || session.activity() == ShellActivity::Idle).await;
    terminal.write(b"echo human-$((6*7))\r").unwrap();
    wait_for_screen(&terminal, "\nhuman-42").await;

    let rows = rows_with_links(&terminal);
    let find = |needle: &str| {
        rows.iter()
            .find(|(text, _)| text.ends_with(needle))
            .unwrap_or_else(|| panic!("{needle:?} not in {rows:?}"))
            .1
            .clone()
    };
    let link = find(" echo agent-$((40+2))").expect("the agent's line is linked");
    assert_eq!(agent_command_id(&link), Some(id.as_str()));
    assert_eq!(find(" echo human-$((6*7))"), None, "typed lines are not");
    assert_eq!(find("agent-42"), None, "output is not");
    assert_eq!(find("human-42"), None);
}

/// A view attached at its own (narrow) size does not change what the model
/// reads: the output comes from the stream, not the grid.
#[tokio::test]
async fn a_narrow_view_does_not_wrap_the_models_output() {
    let session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.start().await.unwrap();
    session.terminal().unwrap().resize(40, 10).unwrap();
    let output = session.execute("printf '%0300d\\n' 0").await.unwrap();
    assert_eq!(output.stdout, "0".repeat(300));
    let (cols, rows) = HEADLESS_SIZE;
    session.terminal().unwrap().resize(cols, rows).unwrap();
}

/// AGE-587: the terminal's record of an agent command is the runner line;
/// its id finds the command itself.
#[test]
fn a_runner_line_names_its_agent_command() {
    assert_eq!(
        runner_command_id(". \"$__chatty_r\" f21804639ba7 6"),
        Some("f21804639ba7")
    );
    assert_eq!(runner_command_id("ls -la"), None);
}

/// AGE-627: before the shell exists, `is_sandboxed` used to answer from
/// `can_sandbox()` (`bwrap --version`), so the first command under
/// `AutoApproveSandboxed` was auto-approved even when the sandboxed spawn
/// then fell back to a plain bash. The answer must come from the shell that
/// actually runs, so that command asks.
#[tokio::test]
async fn a_shell_that_spawns_unsandboxed_asks_for_its_first_command() {
    use crate::models::execution_approval_store::{
        ApprovalDecision, ExecutionApprovalStore, request_execution_approval,
    };
    use crate::settings::models::execution_settings::ApprovalMode;

    let mut session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    // Stands in for a sandboxed spawn that failed and fell back.
    session.test_shell = Some(Box::new(|_| TerminalConfig {
        shell: Some("/bin/bash".into()),
        args: bash_args(),
        ..TerminalConfig::default()
    }));
    assert_eq!(session.running_sandboxed(), None, "no shell yet");

    let is_sandboxed = session.is_sandboxed().await;
    assert!(!is_sandboxed);
    assert_eq!(
        session.running_sandboxed(),
        Some(false),
        "started to answer"
    );

    let mut store = ExecutionApprovalStore::new();
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let (resolution_tx, _resolution_rx) = tokio::sync::mpsc::unbounded_channel();
    store.set_notifiers(approval_tx, resolution_tx);
    let pending = store.get_pending_approvals();
    let waiter = tokio::spawn(async move {
        request_execution_approval(
            &pending,
            &ApprovalMode::AutoApproveSandboxed,
            "[shell] echo first",
            is_sandboxed,
        )
        .await
    });
    let request = approval_rx.recv().await.expect("the command asks");
    assert!(!request.is_sandboxed);
    store.resolve(&request.id, ApprovalDecision::Denied);
    assert!(!waiter.await.unwrap().unwrap());
}

// ── AGE-687: probe the sandbox for real, fall back when it can't run ─────────

/// Write `script` as an executable fake `bwrap` in a fresh directory. Waits
/// until it can be run: another test's fork may briefly hold the write
/// descriptor (`ETXTBSY`).
#[cfg(target_os = "linux")]
fn fake_bwrap(script: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bwrap");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..200 {
        match std::process::Command::new(&path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
        {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => break,
        }
    }
    let path = path.to_string_lossy().into_owned();
    (dir, path)
}

/// This workstation before AGE-687: bwrap 0.9.0 installed, `--version`
/// works, anything that needs a user namespace exits 1.
#[cfg(target_os = "linux")]
const BWRAP_WITHOUT_USERNS: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then echo "bubblewrap 0.9.0"; exit 0; fi
echo "bwrap: setting up uid map: Permission denied" >&2
exit 1
"#;

/// A bwrap that works: skips its own options and runs the command, in the
/// `--chdir` directory.
#[cfg(target_os = "linux")]
const BWRAP_PASSTHROUGH: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then echo "bubblewrap 0.9.0"; exit 0; fi
while [ $# -gt 0 ]; do
  case "$1" in
    --ro-bind|--bind) shift 3 ;;
    --tmpfs|--proc|--dev) shift 2 ;;
    --chdir) cd "$2" || exit 1; shift 2 ;;
    --*) shift ;;
    *) break ;;
  esac
done
exec "$@"
"#;

/// bwrap installed but unable to create a user namespace: not available,
/// and the session runs unsandboxed instead of failing every command, and
/// says why.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn bwrap_without_user_namespaces_falls_back_to_an_unsandboxed_shell() {
    let (_dir, bwrap) = fake_bwrap(BWRAP_WITHOUT_USERNS);
    for network_isolation in [false, true] {
        let reason = ShellSession::sandbox_check(&bwrap, network_isolation).unwrap_err();
        assert!(reason.contains("setting up uid map"), "{reason}");

        let mut session = ShellSession::with_secrets(None, 30, 51200, network_isolation, vec![]);
        session.test_bwrap = Some(bwrap.clone());
        let output = session.execute("echo ok").await.unwrap();
        assert_eq!(output.stdout, "ok");
        assert_eq!(output.exit_code, 0);
        assert!(!session.is_sandboxed().await);
        assert_eq!(session.running_sandboxed(), Some(false));
        let reason = session.sandbox_unavailable_reason().expect("a reason");
        assert!(reason.contains("setting up uid map"), "{reason}");
    }
}

/// A bwrap that works keeps the sandboxed path, with no reason.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_working_bwrap_keeps_the_sandboxed_shell() {
    let (_dir, bwrap) = fake_bwrap(BWRAP_PASSTHROUGH);
    assert_eq!(ShellSession::sandbox_check(&bwrap, false), Ok(()));

    let mut session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.test_bwrap = Some(bwrap);
    let output = session.execute("echo ok").await.unwrap();
    assert_eq!(output.stdout, "ok");
    assert!(session.is_sandboxed().await);
    assert_eq!(session.sandbox_unavailable_reason(), None);
}

/// A sandboxed shell that passes the probe but dies before its first
/// prompt (a race, a workspace bind that fails) is replaced by an
/// unsandboxed one: once, not in a loop.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_sandboxed_shell_that_dies_before_its_prompt_falls_back_once() {
    let log = tempfile::tempdir().unwrap();
    let starts = log.path().join("starts");
    let (_dir, bwrap) = fake_bwrap(&format!(
        "#!/bin/sh\n\
         case \" $* \" in *\" -i \"*)\n\
           echo x >> '{}'\n\
           echo 'bwrap: setting up uid map: Permission denied' >&2; exit 1 ;;\n\
         esac\n{}",
        starts.display(),
        BWRAP_PASSTHROUGH.trim_start_matches("#!/bin/sh\n")
    ));
    assert_eq!(ShellSession::sandbox_check(&bwrap, false), Ok(()));

    let mut session = ShellSession::with_secrets(None, 30, 51200, false, vec![]);
    session.test_bwrap = Some(bwrap);
    let output = session.execute("echo ok").await.unwrap();
    assert_eq!(output.stdout, "ok");
    assert!(!session.is_sandboxed().await);
    let reason = session.sandbox_unavailable_reason().expect("a reason");
    assert!(reason.contains("before its first prompt"), "{reason}");
    // With the login profile, then without it (the profile could have been
    // what ended it), then unsandboxed: two sandboxed starts, no more.
    let started = std::fs::read_to_string(&starts).unwrap();
    assert_eq!(started.lines().count(), 2, "{started}");
}
