#[cfg(test)]
mod tests {
    use super::{apply_at_to_input, at_menu_items_for, at_query_from, slash_menu_items_for};

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
            "src".to_string(),
            "scripts".to_string(),
        ];
        let items = at_menu_items_for("@src", &files);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "src");
    }

    #[test]
    fn test_at_menu_items_case_insensitive() {
        let files = vec!["README.md".to_string(), "Makefile".to_string()];
        let items = at_menu_items_for("@readme", &files);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "README.md");
    }

    #[test]
    fn test_at_menu_items_empty_when_no_at() {
        let files = vec!["README.md".to_string()];
        assert!(at_menu_items_for("", &files).is_empty());
        assert!(at_menu_items_for("hello", &files).is_empty());
    }

    #[test]
    fn test_at_menu_items_closes_on_space() {
        let files = vec!["README.md".to_string()];
        assert!(at_menu_items_for("@readme ", &files).is_empty());
    }

    #[test]
    fn test_apply_at_replaces_trigger() {
        assert_eq!(apply_at_to_input("@read", "README.md"), "@README.md ");
    }

    #[test]
    fn test_apply_at_mid_sentence() {
        assert_eq!(
            apply_at_to_input("check @src please", "src/main.rs"),
            "check @src/main.rs "
        );
    }

    // -----------------------------------------------------------------------
    // slash-command menu tests (pure, no GPUI context required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_slash_menu_bare_slash() {
        let items = slash_menu_items_for("/");
        assert!(!items.is_empty(), "Bare '/' should return menu items");
    }

    #[test]
    fn test_slash_menu_compact() {
        let items = slash_menu_items_for("/comp");
        assert!(
            items.iter().any(|i| i.command == "/compact"),
            "/compact should match /comp prefix"
        );
    }

    #[test]
    fn test_slash_menu_clear() {
        let items = slash_menu_items_for("/cl");
        assert!(
            items.iter().any(|i| i.command == "/clear"),
            "/clear should match /cl prefix"
        );
    }

    #[test]
    fn test_slash_menu_no_match() {
        let items = slash_menu_items_for("/zzz");
        assert!(items.is_empty(), "Unknown prefix should return no items");
    }

    // -----------------------------------------------------------------------
    // slash_menu_items_with_skills tests (pure, no GPUI context)
    // -----------------------------------------------------------------------

    #[test]
    fn test_skills_appear_in_slash_menu_with_skills() {
        use super::{SkillEntry, SlashMenuItem, slash_menu_items_with_skills};

        let skills = vec![
            SkillEntry {
                name: "fix-ci".to_string(),
                description: "Diagnoses CI failures.".to_string(),
            },
            SkillEntry {
                name: "build-and-check".to_string(),
                description: "Run build pipeline.".to_string(),
            },
        ];

        // Bare "/" should return all built-ins AND all skills
        let items = slash_menu_items_with_skills("/", &skills);
        let has_skill = |name: &str| {
            items
                .iter()
                .any(|i| matches!(i, SlashMenuItem::Skill(s) if s.name == name))
        };
        assert!(has_skill("fix-ci"), "fix-ci skill should appear");
        assert!(
            has_skill("build-and-check"),
            "build-and-check skill should appear"
        );
        // A built-in should also be present
        assert!(
            items
                .iter()
                .any(|i| matches!(i, SlashMenuItem::Command(c) if c.command == "/compact"))
        );
    }

    #[test]
    fn test_skills_filtered_by_prefix() {
        use super::{SkillEntry, SlashMenuItem, slash_menu_items_with_skills};

        let skills = vec![
            SkillEntry {
                name: "fix-ci".to_string(),
                description: "Fix CI.".to_string(),
            },
            SkillEntry {
                name: "build-and-check".to_string(),
                description: "Build.".to_string(),
            },
        ];

        let items = slash_menu_items_with_skills("/fix", &skills);
        let names: Vec<String> = items
            .iter()
            .map(|i: &SlashMenuItem| i.display_command())
            .collect();
        assert!(names.contains(&"/fix-ci".to_string()));
        assert!(!names.contains(&"/build-and-check".to_string()));
        // No built-in starts with "fix"
        assert!(!items.iter().any(|i| matches!(i, SlashMenuItem::Command(_))));
    }

    #[test]
    fn test_skill_menu_item_properties() {
        use super::{SkillEntry, SlashMenuItem};

        let item = SlashMenuItem::Skill(SkillEntry {
            name: "my-skill".to_string(),
            description: "Does stuff.".to_string(),
        });
        assert!(item.is_skill());
        assert!(!item.execute_immediately());
        assert_eq!(item.display_command(), "/my-skill");
        assert_eq!(item.insert_text(), "Use the 'my-skill' skill: ");
        assert_eq!(item.description(), "Does stuff.");
    }

    #[test]
    fn test_skills_menu_empty_when_no_slash() {
        use super::{SkillEntry, slash_menu_items_with_skills};

        let skills = vec![SkillEntry {
            name: "fix-ci".to_string(),
            description: "Fix CI.".to_string(),
        }];
        assert!(slash_menu_items_with_skills("", &skills).is_empty());
        assert!(slash_menu_items_with_skills("hello", &skills).is_empty());
    }

    #[test]
    fn test_skills_menu_closes_on_space() {
        use super::{SkillEntry, slash_menu_items_with_skills};

        let skills = vec![SkillEntry {
            name: "fix-ci".to_string(),
            description: "Fix.".to_string(),
        }];
        assert!(slash_menu_items_with_skills("/fix-ci extra", &skills).is_empty());
    }

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
}
