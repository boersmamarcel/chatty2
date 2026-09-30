#[cfg(test)]
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

// -----------------------------------------------------------------------
// `/agent ` picker (AGE-761)
// -----------------------------------------------------------------------

fn agent(name: &str) -> super::AgentPickerEntry {
    super::AgentPickerEntry {
        name: name.to_string(),
        description: format!("{name} does things"),
    }
}

fn agent_names(items: &[super::SlashMenuItem]) -> Vec<String> {
    items.iter().map(|item| item.display_command()).collect()
}

#[test]
fn slash_agent_space_lists_the_roster() {
    use super::{SlashMenuItem, agent_menu_items};

    let agents = vec![agent("local-agent"), agent("panel-lead")];
    let items = agent_menu_items("/agent ", &agents).expect("the picker is in play");
    assert_eq!(agent_names(&items), vec!["local-agent", "panel-lead"]);
    assert_eq!(items[1].insert_text(), "/agent panel-lead ");
    assert_eq!(items[1].description(), "panel-lead does things");
    assert!(items.iter().all(SlashMenuItem::is_selectable));
    // Before the space it is still the command list.
    assert!(agent_menu_items("/agent", &agents).is_none());
}

#[test]
fn slash_agent_query_puts_prefix_matches_before_substring_matches() {
    use super::agent_menu_items;

    let agents = vec![
        agent("data-panel"),
        agent("panel-lead"),
        agent("reviewer"),
        agent("Pantry"),
    ];
    let items = agent_menu_items("/agent pan", &agents).expect("the picker is in play");
    assert_eq!(
        agent_names(&items),
        vec!["panel-lead", "Pantry", "data-panel"]
    );
}

#[test]
fn slash_agent_picker_closes_once_a_second_space_follows_the_name() {
    use super::{SkillEntry, agent_menu_items, slash_menu_items};

    let agents = vec![agent("panel-lead")];
    let skills: Vec<SkillEntry> = Vec::new();
    assert!(agent_menu_items("/agent panel-lead x", &agents).is_none());
    assert!(agent_menu_items("/agent panel-lead ", &agents).is_none());
    assert!(slash_menu_items("/agent panel-lead x", &skills, &agents).is_empty());
    // Enter's newline, appended before `PressEnter`, is not a second space.
    assert_eq!(
        agent_names(&agent_menu_items("/agent panel\n", &agents).unwrap()),
        vec!["panel-lead"]
    );
}

#[test]
fn slash_agent_with_no_agents_shows_one_unselectable_row() {
    use super::{SlashMenuItem, agent_menu_items};

    let items = agent_menu_items("/agent ", &[]).expect("the picker is in play");
    assert_eq!(items, vec![SlashMenuItem::NoAgents]);
    assert!(!items[0].is_selectable());
    assert_eq!(items[0].display_command(), "No agents available");
    // A typed word goes to the default sub-agent: nothing to pick from.
    assert_eq!(agent_menu_items("/agent summarize", &[]), Some(Vec::new()));
}

/// Enter on a highlighted agent row (what `PressEnter` calls while the
/// picker is open) writes `/agent <name> ` into the composer.
#[gpui::test]
fn chat_input_enter_on_an_agent_row_inserts_its_name(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let vcx = cx.add_empty_window();
    let state = vcx.update(|window, cx| {
        let input = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value("/agent pan", window, cx));
        cx.new(|_| super::ChatInputState::new(input))
    });
    vcx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.set_available_agents(vec![agent("data-panel"), agent("panel-lead")], cx);
            let text = state.input.read(cx).text().to_string();
            assert!(state.is_agent_picker_open(&text));
            // Rows are panel-lead, data-panel: pick the second.
            state.move_slash_menu_down(2);
            state.apply_slash_command(cx);
            state.clear_if_needed(window, cx);
            assert_eq!(
                state.input.read(cx).text().to_string(),
                "/agent data-panel "
            );
            assert!(state.slash_menu_items("/agent data-panel ").is_empty());
        });
    });
}

/// Escape closes the picker without clearing the text; the next edit opens
/// it again, and Enter's newline is not an edit.
#[gpui::test]
fn chat_input_escape_dismisses_the_agent_picker(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;

    cx.update(gpui_component::init);
    let vcx = cx.add_empty_window();
    let state = vcx.update(|window, cx| {
        let input = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        cx.new(|_| super::ChatInputState::new(input))
    });
    vcx.update(|_window, cx| {
        state.update(cx, |state, cx| {
            state.set_available_agents(vec![agent("panel-lead")], cx);
            assert!(state.is_agent_picker_open("/agent pa"));
            state.dismiss_agent_picker("/agent pa");
            assert!(!state.is_agent_picker_open("/agent pa"));
            assert!(!state.is_agent_picker_open("/agent pa\n"));
            assert!(state.is_agent_picker_open("/agent pan"));
        });
    });
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

// -----------------------------------------------------------------------
// model picker selection repair (AGE-149)
// -----------------------------------------------------------------------

fn option(id: &str, name: &str) -> super::ModelOption {
    super::ModelOption::new(
        id.to_string(),
        name.to_string(),
        crate::settings::models::providers_store::ProviderType::OpenRouter,
    )
}

#[test]
fn resolve_selected_clears_when_list_empty() {
    assert_eq!(
        super::resolve_selected_model_id(Some("gone"), &[], Some("default".into())),
        None
    );
}

#[test]
fn resolve_selected_keeps_current_when_still_present() {
    let models = vec![option("a", "A"), option("b", "B")];
    assert_eq!(
        super::resolve_selected_model_id(Some("b"), &models, Some("a".into())),
        Some("b".into())
    );
}

#[test]
fn resolve_selected_falls_back_when_current_deleted() {
    let models = vec![option("a", "A"), option("c", "C")];
    assert_eq!(
        super::resolve_selected_model_id(Some("b"), &models, Some("c".into())),
        Some("c".into())
    );
}

#[test]
fn resolve_selected_uses_first_when_default_missing() {
    let models = vec![option("a", "A"), option("c", "C")];
    assert_eq!(
        super::resolve_selected_model_id(Some("b"), &models, Some("gone".into())),
        Some("a".into())
    );
}

#[test]
fn resolve_selected_picks_default_when_none_selected() {
    let models = vec![option("a", "A"), option("b", "B")];
    assert_eq!(
        super::resolve_selected_model_id(None, &models, Some("b".into())),
        Some("b".into())
    );
}

// -----------------------------------------------------------------------
// Composer model label elision (AGE-184)
//
// The composer's bottom row runs out of width before Send does, so the model
// label — the widest and most variable element — is what gives up space.
// -----------------------------------------------------------------------

#[test]
fn model_label_kept_intact_when_it_fits() {
    let label = "Haiku 4.5 · Anthropic";
    assert!(label.chars().count() <= super::MODEL_LABEL_MAX_CHARS);
    assert_eq!(super::truncate_model_label(label, 28), label);
}

#[test]
fn long_model_label_is_elided_to_the_cap() {
    // The reported case: long enough to clip Send at half window width.
    let label = "Google: Gemini 3.8 Flash · OpenRouter";
    let out = super::truncate_model_label(label, 28);
    assert_eq!(out.chars().count(), 28);
    assert!(out.ends_with('\u{2026}'));
    assert!(label.starts_with(out.trim_end_matches('\u{2026}')));
}

#[test]
fn model_label_elision_counts_chars_not_bytes() {
    // A multi-byte name must not panic on a slice boundary.
    let label = "モデル名がとても長いモデル · プロバイダー";
    let out = super::truncate_model_label(label, 8);
    assert_eq!(out.chars().count(), 8);
    assert!(out.ends_with('\u{2026}'));
}

#[test]
fn model_label_cap_leaves_room_for_send() {
    // Guards the constant itself: a label at the cap plus the fixed-width
    // controls has to stay well under a half-width chat column. A const block
    // makes this a compile-time check rather than a runtime one.
    const { assert!(super::MODEL_LABEL_MAX_CHARS <= 32) };
}

// -----------------------------------------------------------------------
// `/agent ` picker in the composer, keystroke by keystroke (AGE-761)
// -----------------------------------------------------------------------

/// A chat view whose only reachable agent is the enabled remote A2A agent
/// `remote-reviewer` (no module settings, so no local roster).
fn composer_with_a_remote_agent(
    cx: &mut gpui::TestAppContext,
) -> (gpui::Entity<super::ChatInputState>, gpui::VisualTestContext) {
    use crate::chatty::views::chat_view::ChatView;
    use crate::settings::models::{ExecutionSettingsModel, ExtensionsModel, GeneralSettingsModel};
    use chatty_core::settings::models::a2a_store::A2aAgentConfig;
    use chatty_core::settings::models::extensions_store::{
        ExtensionKind, ExtensionSource, InstalledExtension,
    };
    use gpui::AppContext as _;
    use std::cell::RefCell;
    use std::rc::Rc;

    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(GeneralSettingsModel::default());
        cx.set_global(ExecutionSettingsModel {
            workspace_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            ..ExecutionSettingsModel::default()
        });
        let mut extensions = ExtensionsModel::default();
        extensions.add(InstalledExtension {
            id: "remote-reviewer".into(),
            display_name: "Remote reviewer".into(),
            description: "Reviews pull requests".into(),
            kind: ExtensionKind::A2aAgent(A2aAgentConfig {
                name: "remote-reviewer".into(),
                url: "https://example.invalid/a2a".into(),
                api_key: None,
                enabled: true,
                skills: Vec::new(),
                allow_private_network: false,
            }),
            source: ExtensionSource::Custom,
            pricing_model: None,
            enabled: true,
        });
        cx.set_global(extensions);
        cx.set_global(chatty_core::models::ErrorStore::new(100));
        cx.set_global(crate::auto_updater::AutoUpdater::new("0.0.0"));
        cx.set_global(chatty_core::models::ConversationsStore::new());
    });
    let slot: Rc<RefCell<Option<gpui::Entity<ChatView>>>> = Rc::default();
    let slot_for_window = slot.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| ChatView::new(window, cx));
        *slot_for_window.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = slot.borrow_mut().take().expect("ChatView captured");
    let state = cx.read(|cx| view.read(cx).chat_input_state().clone());
    (
        state,
        gpui::VisualTestContext::from_window(window.into(), cx),
    )
}

fn composer_text(
    state: &gpui::Entity<super::ChatInputState>,
    vcx: &mut gpui::VisualTestContext,
) -> String {
    vcx.update(|window, cx| {
        state.update(cx, |state, cx| state.clear_if_needed(window, cx));
        state.read(cx).input.read(cx).text().to_string()
    })
}

#[gpui::test]
fn chat_input_agent_picker_enter_tab_and_escape(cx: &mut gpui::TestAppContext) {
    let (state, mut vcx) = composer_with_a_remote_agent(cx);

    vcx.simulate_input("/agent ");
    let items = vcx.update(|_, cx| state.read(cx).slash_menu_items("/agent "));
    assert_eq!(agent_names(&items), vec!["remote-reviewer"]);
    assert_eq!(items[0].description(), "Reviews pull requests");

    // Enter inserts the highlighted agent instead of sending.
    vcx.simulate_input("rev");
    vcx.simulate_keystrokes("enter");
    assert_eq!(composer_text(&state, &mut vcx), "/agent remote-reviewer ");
    let items = vcx.update(|_, cx| state.read(cx).slash_menu_items("/agent remote-reviewer "));
    assert!(items.is_empty(), "the picker closes after the name");

    // Tab does the same.
    vcx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state
                .input
                .update(cx, |input, cx| input.set_value("", window, cx))
        })
    });
    vcx.simulate_input("/agent r");
    vcx.simulate_keystrokes("tab");
    assert_eq!(composer_text(&state, &mut vcx), "/agent remote-reviewer ");

    // Escape closes the picker and keeps the text.
    vcx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state
                .input
                .update(cx, |input, cx| input.set_value("", window, cx))
        })
    });
    vcx.simulate_input("/agent re");
    vcx.simulate_keystrokes("escape");
    assert_eq!(composer_text(&state, &mut vcx), "/agent re");
    let open = vcx.update(|_, cx| state.read(cx).is_agent_picker_open("/agent re"));
    assert!(!open, "Escape closed the picker");
}
