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
fn test_decode_output_line_lossy_decodes_non_utf8() {
    let decoded = ShellSession::decode_output_line(b"\xff\xfeabc\n");
    assert!(decoded.contains("abc"));
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
