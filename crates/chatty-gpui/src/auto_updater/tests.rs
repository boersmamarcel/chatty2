//! Tests for the auto-updater (semver comparisons, asset selection, etc.).

use super::*;


    use super::*;

    #[test]
    fn test_version_comparison() {
        let current = Version::parse("0.1.0").unwrap();
        let newer = Version::parse("0.2.0").unwrap();
        let older = Version::parse("0.0.9").unwrap();

        assert!(newer > current);
        assert!(older < current);
    }

    #[test]
    fn test_auto_updater_creation() {
        let updater = AutoUpdater::new("0.1.0");
        assert_eq!(updater.current_version().to_string(), "0.1.0");
        assert!(matches!(updater.status(), AutoUpdateStatus::Idle));
    }

    #[test]
    fn test_asset_matching() {
        let assets = vec![
            GitHubAsset {
                name: "chatty-macos-aarch64.dmg".to_string(),
                browser_download_url: "https://example.com/macos-arm".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-x86_64.AppImage".to_string(),
                browser_download_url: "https://example.com/linux-x64".to_string(),
            },
        ];

        let result = find_matching_asset(&assets, "macos", "aarch64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-macos-aarch64.dmg");

        let result = find_matching_asset(&assets, "linux", "x86_64");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "chatty-linux-x86_64.AppImage");

        let result = find_matching_asset(&assets, "windows", "x86_64");
        assert!(result.is_none());
    }

    // --- parse_checksums tests ---

    #[test]
    fn test_parse_checksums_standard_format() {
        // Each SHA-256 hash must be exactly 64 hex characters
        let hash1 = "a".repeat(64);
        let hash2 = "b".repeat(64);
        let text = format!(
            "{}  chatty-linux-x86_64.AppImage\n{}  chatty-macos-aarch64.dmg",
            hash1, hash2
        );
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
        assert!(checksums.contains_key("chatty-macos-aarch64.dmg"));
    }

    #[test]
    fn test_parse_checksums_single_space_separator() {
        let hash = "a".repeat(64);
        let text = format!("{} chatty-linux-x86_64.AppImage", hash);
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
    }

    #[test]
    fn test_parse_checksums_colon_format() {
        let hash = "a".repeat(64);
        let text = format!("chatty-linux-x86_64.AppImage: {}", hash);
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("chatty-linux-x86_64.AppImage"));
    }

    #[test]
    fn test_parse_checksums_skips_empty_lines_and_comments() {
        let hash = "a".repeat(64);
        let text = format!(
            "# This is a comment\n\n{}  file.tar.gz\n\n# Another comment",
            hash
        );
        let checksums = AutoUpdater::parse_checksums(&text);

        assert_eq!(checksums.len(), 1);
        assert!(checksums.contains_key("file.tar.gz"));
    }

    #[test]
    fn test_parse_checksums_empty_input() {
        let checksums = AutoUpdater::parse_checksums("");
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_invalid_hash_length() {
        // Hash too short (not 64 hex chars)
        let text = "abc123  file.tar.gz";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_non_hex_chars() {
        // 64 chars but contains non-hex characters
        let text = "zzzz567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef  file.tar.gz";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_normalizes_to_lowercase() {
        let text = "ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12ABCDEF12  file.bin";
        let checksums = AutoUpdater::parse_checksums(text);

        assert_eq!(checksums.len(), 1);
        let hash = checksums.get("file.bin").unwrap();
        assert_eq!(
            hash, "abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12abcdef12",
            "hash should be lowercased"
        );
    }

    #[test]
    fn test_parse_checksums_only_comments() {
        let text = "# comment 1\n# comment 2\n# comment 3";
        let checksums = AutoUpdater::parse_checksums(text);
        assert!(checksums.is_empty());
    }

    // --- verify_checksum tests ---

    #[tokio::test]
    async fn test_verify_checksum_matching() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        let content = b"hello world";
        tokio::fs::write(&file_path, content).await.unwrap();

        // Compute the expected SHA-256
        let expected = hex::encode(Sha256::digest(content));

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum should match");
    }

    #[tokio::test]
    async fn test_verify_checksum_mismatch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        tokio::fs::write(&file_path, b"hello world").await.unwrap();

        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), wrong_hash).await;
        assert!(result.is_ok());
        assert!(!result.unwrap(), "checksum should not match");
    }

    #[tokio::test]
    async fn test_verify_checksum_case_insensitive() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test_file.bin");
        let content = b"test data";
        tokio::fs::write(&file_path, content).await.unwrap();

        let expected = hex::encode(Sha256::digest(content)).to_uppercase();

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum should match case-insensitively");
    }

    #[tokio::test]
    async fn test_verify_checksum_nonexistent_file() {
        let path = PathBuf::from("/tmp/does_not_exist_at_all_12345.bin");
        let hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let result = AutoUpdater::verify_checksum(&path, hash).await;
        assert!(result.is_err(), "should error for nonexistent file");
    }

    #[tokio::test]
    async fn test_verify_checksum_empty_file() {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("empty.bin");
        tokio::fs::write(&file_path, b"").await.unwrap();

        let expected = hex::encode(Sha256::digest(b""));

        let result = AutoUpdater::verify_checksum(&file_path.to_path_buf(), &expected).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "checksum of empty file should match");
    }

    // --- asset matching edge cases ---

    #[test]
    fn test_asset_matching_all_platforms() {
        let assets = vec![
            GitHubAsset {
                name: "chatty-macos-aarch64.dmg".to_string(),
                browser_download_url: "https://example.com/1".to_string(),
            },
            GitHubAsset {
                name: "chatty-macos-x86_64.dmg".to_string(),
                browser_download_url: "https://example.com/2".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-x86_64.AppImage".to_string(),
                browser_download_url: "https://example.com/3".to_string(),
            },
            GitHubAsset {
                name: "chatty-linux-aarch64.AppImage".to_string(),
                browser_download_url: "https://example.com/4".to_string(),
            },
            GitHubAsset {
                name: "chatty-windows-x86_64.exe".to_string(),
                browser_download_url: "https://example.com/5".to_string(),
            },
        ];

        assert_eq!(
            find_matching_asset(&assets, "macos", "aarch64")
                .unwrap()
                .name,
            "chatty-macos-aarch64.dmg"
        );
        assert_eq!(
            find_matching_asset(&assets, "macos", "x86_64")
                .unwrap()
                .name,
            "chatty-macos-x86_64.dmg"
        );
        assert_eq!(
            find_matching_asset(&assets, "linux", "x86_64")
                .unwrap()
                .name,
            "chatty-linux-x86_64.AppImage"
        );
        assert_eq!(
            find_matching_asset(&assets, "linux", "aarch64")
                .unwrap()
                .name,
            "chatty-linux-aarch64.AppImage"
        );
        assert_eq!(
            find_matching_asset(&assets, "windows", "x86_64")
                .unwrap()
                .name,
            "chatty-windows-x86_64.exe"
        );
    }

    #[test]
    fn test_asset_matching_unsupported_platform() {
        let assets = vec![GitHubAsset {
            name: "chatty-linux-x86_64.AppImage".to_string(),
            browser_download_url: "https://example.com/1".to_string(),
        }];

        assert!(find_matching_asset(&assets, "freebsd", "x86_64").is_none());
    }

    #[test]
    fn test_asset_matching_empty_assets() {
        assert!(find_matching_asset(&[], "linux", "x86_64").is_none());
    }

    // --- dismiss_error test ---

    #[test]
    fn test_dismiss_error() {
        let mut updater = AutoUpdater::new("1.0.0");
        updater.status = AutoUpdateStatus::Error("something went wrong".to_string());

        updater.dismiss_error();
        assert_eq!(*updater.status(), AutoUpdateStatus::Idle);
    }

    #[test]
    fn test_dismiss_error_noop_when_not_error() {
        let mut updater = AutoUpdater::new("1.0.0");
        updater.status = AutoUpdateStatus::Checking;

        updater.dismiss_error();
        assert_eq!(*updater.status(), AutoUpdateStatus::Checking);
    }

    #[test]
    fn test_auto_updater_invalid_version_fallback() {
        let updater = AutoUpdater::new("not-a-version");
        assert_eq!(updater.current_version().to_string(), "0.0.0");
        assert!(matches!(updater.status(), AutoUpdateStatus::Idle));
    }
