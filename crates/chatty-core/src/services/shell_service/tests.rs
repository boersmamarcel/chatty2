//! Tests for `shell_service` (extracted from the production file).

use super::*;

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

        let result = session.execute("sleep 10").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_output_truncation() {
        let session = ShellSession::with_secrets(None, 30, 100, false, vec![]); // 100 byte limit

        let result = session.execute("seq 1 1000").await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.truncated);
        assert!(output.stdout.contains("[truncated"));
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
    fn test_truncate_output_at_char_boundary_preserves_utf8() {
        let mut output = format!("{}📈bbb📈tail", "a".repeat(995));
        ShellSession::truncate_output_at_char_boundary(&mut output, 1004);
        assert!(output.contains('📈'));
        assert!(output.contains("[truncated"));
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
