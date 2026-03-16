#[cfg(test)]
mod tests {
    use super::super::chat_input::{
        ChatInputState, IMAGE_EXTENSIONS, MAX_FILE_SIZE, PDF_EXTENSION, apply_at_to_input,
        at_menu_items_for, at_query_from, slash_menu_items_for,
    };
    use gpui::{App, Context, Entity, TestWindow};
    use gpui_component::input::InputState;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    // -----------------------------------------------------------------------
    // @ mention menu tests (pure, no GPUI context required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_at_query_none_without_at() {
        assert!(at_query_from("").is_none());
        assert!(at_query_from("hello world").is_none());
        assert!(at_query_from("/clear").is_none());
    }

    #[test]
    fn test_at_query_bare_at() {
        assert_eq!(at_query_from("@"), Some(String::new()));
    }

    #[test]
    fn test_at_query_with_word() {
        assert_eq!(at_query_from("@readme"), Some("readme".into()));
        assert_eq!(at_query_from("hello @src"), Some("src".into()));
    }

    #[test]
    fn test_at_query_closes_on_space() {
        assert!(at_query_from("@readme ").is_none());
        assert!(at_query_from("@file.txt and more").is_none());
    }

    #[test]
    fn test_at_menu_items_all_for_bare_at() {
        let files = vec!["README.md".to_string(), "src".to_string()];
        let items = at_menu_items_for("@", &files);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn test_at_menu_items_filter_by_query() {
        let files = vec![
            "README.md".to_string(),
            "Cargo.toml".to_string(),
            "src".to_string(),
        ];
        let items = at_menu_items_for("@README", &files);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "README.md");
    }

    #[test]
    fn test_at_menu_items_empty_when_no_match() {
        let files = vec!["README.md".to_string()];
        assert!(at_menu_items_for("@zzz", &files).is_empty());
    }

    #[test]
    fn test_at_menu_items_empty_when_no_at() {
        let files = vec!["README.md".to_string()];
        assert!(at_menu_items_for("hello", &files).is_empty());
    }

    #[test]
    fn test_apply_at_to_input_bare_at() {
        assert_eq!(apply_at_to_input("@", "README.md"), "@README.md ");
    }

    #[test]
    fn test_apply_at_to_input_with_query() {
        assert_eq!(apply_at_to_input("@read", "README.md"), "@README.md ");
    }

    #[test]
    fn test_apply_at_to_input_with_prefix_text() {
        assert_eq!(
            apply_at_to_input("please look at @read", "README.md"),
            "please look at @README.md "
        );
    }

    // -----------------------------------------------------------------------
    // Slash-command menu tests (pure, no GPUI context required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_slash_menu_empty_when_no_slash() {
        assert!(slash_menu_items_for("").is_empty());
        assert!(slash_menu_items_for("hello").is_empty());
        assert!(slash_menu_items_for("say /clear").is_empty());
    }

    #[test]
    fn test_slash_menu_all_commands_when_just_slash() {
        let items = slash_menu_items_for("/");
        assert!(!items.is_empty(), "Should show all commands for bare '/'");
    }

    #[test]
    fn test_slash_menu_filters_by_prefix() {
        let items = slash_menu_items_for("/cl");
        let commands: Vec<&str> = items.iter().map(|c| c.command).collect();
        assert!(commands.contains(&"/clear"), "should match /clear");
        assert!(!commands.contains(&"/copy"), "should not match /copy");
    }

    #[test]
    fn test_slash_menu_exact_match() {
        let items = slash_menu_items_for("/clear");
        let commands: Vec<&str> = items.iter().map(|c| c.command).collect();
        assert!(commands.contains(&"/clear"));
    }

    #[test]
    fn test_slash_menu_closes_when_space_follows() {
        // Once the user types a space (argument separator), close the menu.
        assert!(slash_menu_items_for("/clear ").is_empty());
        assert!(slash_menu_items_for("/add-dir /some/path").is_empty());
    }

    #[test]
    fn test_slash_menu_case_insensitive() {
        let items = slash_menu_items_for("/CL");
        let commands: Vec<&str> = items.iter().map(|c| c.command).collect();
        assert!(
            commands.contains(&"/clear"),
            "filter should be case-insensitive"
        );
    }

    #[test]
    fn test_slash_menu_no_match() {
        let items = slash_menu_items_for("/zzz");
        assert!(items.is_empty(), "Unknown prefix should return no items");
    }

    // -----------------------------------------------------------------------
    // Arg-based slash-command extraction tests (pure, no GPUI context)
    // -----------------------------------------------------------------------

    /// Verify that the /agent command prefix extraction used by
    /// `try_handle_arg_slash_command` works correctly.
    #[test]
    fn test_agent_prefix_extraction() {
        let msg = "/agent summarize this file";
        assert_eq!(
            msg.strip_prefix("/agent "),
            Some("summarize this file"),
            "/agent prefix should be strippable"
        );
        // No-arg case: `/agent` alone (no trailing space) should NOT match.
        assert!(
            "/agent".strip_prefix("/agent ").is_none(),
            "bare /agent without space should not match"
        );
        // Empty arg case.
        assert_eq!("/agent  ".strip_prefix("/agent "), Some(" "));
    }

    /// Verify /cd prefix extraction.
    #[test]
    fn test_cd_prefix_extraction() {
        let msg = "/cd /tmp/myproject";
        assert_eq!(msg.strip_prefix("/cd "), Some("/tmp/myproject"));
        assert!("/cd".strip_prefix("/cd ").is_none());
    }

    /// Verify /add-dir prefix extraction.
    #[test]
    fn test_add_dir_prefix_extraction() {
        let msg = "/add-dir ./src";
        assert_eq!(msg.strip_prefix("/add-dir "), Some("./src"));
    }

    /// Helper to create a test file of a specific size
    fn create_test_file(path: &PathBuf, size: u64, extension: &str) -> std::io::Result<()> {
        let full_path = path.with_extension(extension);
        let mut file = fs::File::create(&full_path)?;

        // Write dummy data
        let data = vec![0u8; size as usize];
        file.write_all(&data)?;
        Ok(())
    }

    /// Helper to create a ChatInputState for testing
    fn create_test_chat_input(cx: &mut Context<ChatInputState>) -> ChatInputState {
        let input = cx.new(|_cx| InputState::new("test_input"));
        ChatInputState::new(input)
    }

    #[gpui::test]
    fn test_add_valid_image_attachment(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let image_path = temp_dir.join("test_image.png");

        // Create a small valid image file
        create_test_file(&image_path, 1024, "png").expect("Failed to create test image");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![image_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should have one attachment"
            );
            assert_eq!(
                state.get_attachments()[0],
                image_path,
                "Attachment path should match"
            );
        });

        // Clean up
        let _ = fs::remove_file(&image_path);
    }

    #[gpui::test]
    fn test_add_valid_pdf_attachment(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let pdf_path = temp_dir.join("test_document.pdf");

        // Create a small valid PDF file
        create_test_file(&pdf_path, 2048, "pdf").expect("Failed to create test PDF");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![pdf_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should have one PDF attachment"
            );
        });

        // Clean up
        let _ = fs::remove_file(&pdf_path);
    }

    #[gpui::test]
    fn test_reject_file_too_large(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let large_file_path = temp_dir.join("large_image.jpg");

        // Create a file larger than MAX_FILE_SIZE (5MB)
        create_test_file(&large_file_path, MAX_FILE_SIZE + 1, "jpg")
            .expect("Failed to create large file");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![large_file_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                0,
                "Should reject file larger than 5MB"
            );
        });

        // Clean up
        let _ = fs::remove_file(&large_file_path);
    }

    #[gpui::test]
    fn test_reject_unsupported_extension(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let unsupported_path = temp_dir.join("test_file.txt");

        // Create a text file (unsupported)
        create_test_file(&unsupported_path, 1024, "txt").expect("Failed to create test file");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![unsupported_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                0,
                "Should reject unsupported file type"
            );
        });

        // Clean up
        let _ = fs::remove_file(&unsupported_path);
    }

    #[gpui::test]
    fn test_reject_duplicate_attachment(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let image_path = temp_dir.join("test_duplicate.png");

        create_test_file(&image_path, 1024, "png").expect("Failed to create test image");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);

            // Add the same file twice
            state.add_attachments(vec![image_path.clone()], cx);
            state.add_attachments(vec![image_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should only add attachment once (reject duplicate)"
            );
        });

        // Clean up
        let _ = fs::remove_file(&image_path);
    }

    #[gpui::test]
    fn test_reject_file_without_extension(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let no_ext_path = temp_dir.join("no_extension_file");

        // Create file without extension
        let mut file = fs::File::create(&no_ext_path).expect("Failed to create file");
        file.write_all(b"test data").expect("Failed to write");
        drop(file);

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![no_ext_path.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                0,
                "Should reject file without extension"
            );
        });

        // Clean up
        let _ = fs::remove_file(&no_ext_path);
    }

    #[gpui::test]
    fn test_reject_nonexistent_file(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let nonexistent_path = temp_dir.join("nonexistent_file.png");

        // Ensure file doesn't exist
        let _ = fs::remove_file(&nonexistent_path);

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![nonexistent_path], cx);

            assert_eq!(
                state.get_attachments().len(),
                0,
                "Should reject nonexistent file"
            );
        });
    }

    #[gpui::test]
    fn test_add_multiple_valid_attachments(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let image1 = temp_dir.join("image1.png");
        let image2 = temp_dir.join("image2.jpg");
        let pdf = temp_dir.join("document.pdf");

        create_test_file(&image1, 1024, "png").expect("Failed to create image1");
        create_test_file(&image2, 2048, "jpg").expect("Failed to create image2");
        create_test_file(&pdf, 3072, "pdf").expect("Failed to create PDF");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![image1.clone(), image2.clone(), pdf.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                3,
                "Should add all three valid attachments"
            );
        });

        // Clean up
        let _ = fs::remove_file(&image1);
        let _ = fs::remove_file(&image2);
        let _ = fs::remove_file(&pdf);
    }

    #[gpui::test]
    fn test_add_mixed_valid_invalid_attachments(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let valid_image = temp_dir.join("valid.png");
        let invalid_txt = temp_dir.join("invalid.txt");
        let too_large = temp_dir.join("toolarge.jpg");

        create_test_file(&valid_image, 1024, "png").expect("Failed to create valid image");
        create_test_file(&invalid_txt, 1024, "txt").expect("Failed to create txt");
        create_test_file(&too_large, MAX_FILE_SIZE + 1, "jpg")
            .expect("Failed to create large file");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(
                vec![valid_image.clone(), invalid_txt.clone(), too_large.clone()],
                cx,
            );

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should only add the valid image"
            );
            assert_eq!(
                state.get_attachments()[0],
                valid_image,
                "Only valid image should be attached"
            );
        });

        // Clean up
        let _ = fs::remove_file(&valid_image);
        let _ = fs::remove_file(&invalid_txt);
        let _ = fs::remove_file(&too_large);
    }

    #[gpui::test]
    fn test_remove_attachment_by_index(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let image1 = temp_dir.join("remove1.png");
        let image2 = temp_dir.join("remove2.jpg");

        create_test_file(&image1, 1024, "png").expect("Failed to create image1");
        create_test_file(&image2, 1024, "jpg").expect("Failed to create image2");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![image1.clone(), image2.clone()], cx);

            assert_eq!(state.get_attachments().len(), 2);

            // Remove first attachment
            state.remove_attachment(0);

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should have one attachment left"
            );
            assert_eq!(
                state.get_attachments()[0],
                image2,
                "Second attachment should remain"
            );
        });

        // Clean up
        let _ = fs::remove_file(&image1);
        let _ = fs::remove_file(&image2);
    }

    #[gpui::test]
    fn test_clear_all_attachments(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let image = temp_dir.join("clear.png");

        create_test_file(&image, 1024, "png").expect("Failed to create image");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![image.clone()], cx);

            assert_eq!(state.get_attachments().len(), 1);

            state.clear_attachments();

            assert_eq!(
                state.get_attachments().len(),
                0,
                "All attachments should be cleared"
            );
        });

        // Clean up
        let _ = fs::remove_file(&image);
    }

    #[gpui::test]
    fn test_file_size_exactly_at_limit(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let max_size_file = temp_dir.join("max_size.png");

        // Create a file exactly at MAX_FILE_SIZE
        create_test_file(&max_size_file, MAX_FILE_SIZE, "png")
            .expect("Failed to create max size file");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![max_size_file.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                1,
                "Should accept file at exactly 5MB"
            );
        });

        // Clean up
        let _ = fs::remove_file(&max_size_file);
    }

    #[gpui::test]
    fn test_all_supported_image_extensions(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let mut paths = Vec::new();

        // Test all supported image extensions
        for ext in IMAGE_EXTENSIONS {
            let path = temp_dir.join(format!("test_image.{}", ext));
            create_test_file(&path, 1024, ext).expect("Failed to create image");
            paths.push(path);
        }

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(paths.clone(), cx);

            assert_eq!(
                state.get_attachments().len(),
                IMAGE_EXTENSIONS.len(),
                "Should accept all supported image extensions"
            );
        });

        // Clean up
        for path in paths {
            let _ = fs::remove_file(&path);
        }
    }

    #[gpui::test]
    fn test_case_insensitive_extensions(cx: &mut TestWindow) {
        let temp_dir = std::env::temp_dir();
        let uppercase_png = temp_dir.join("test.PNG");
        let mixedcase_jpg = temp_dir.join("test.JpG");

        create_test_file(&uppercase_png, 1024, "PNG").expect("Failed to create PNG");
        create_test_file(&mixedcase_jpg, 1024, "JpG").expect("Failed to create JPG");

        cx.update(|cx| {
            let mut state = create_test_chat_input(cx);
            state.add_attachments(vec![uppercase_png.clone(), mixedcase_jpg.clone()], cx);

            assert_eq!(
                state.get_attachments().len(),
                2,
                "Should accept case-insensitive extensions"
            );
        });

        // Clean up
        let _ = fs::remove_file(&uppercase_png);
        let _ = fs::remove_file(&mixedcase_jpg);
    }
}
