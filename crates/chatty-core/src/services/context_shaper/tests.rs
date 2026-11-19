use super::*;
use rig_core::completion::message::{AssistantContent, CallId};

fn user_text(s: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text::new(s.to_string()))],
    }
}

fn tool_result(id: &str, content: ToolResultContent) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            call: CallId::from_wire(id),
            name: crate::testing::tool_name("test_tool"),
            content: vec![content],
            is_error: false,
        })],
    }
}

/// A tool result of `words` words of prose, so its token count is
/// predictable and its byte length is `words * 5 - 1`.
fn text_result(id: &str, words: usize) -> Message {
    tool_result(
        id,
        ToolResultContent::Text(Text::new(vec!["word"; words].join(" "))),
    )
}

fn json_result(id: &str, words: usize) -> Message {
    tool_result(
        id,
        ToolResultContent::Json {
            value: serde_json::json!({ "content": vec!["word"; words].join(" ") }),
        },
    )
}

fn result_text(message: &Message) -> String {
    let Message::User { content } = message else {
        panic!("expected a user message");
    };
    match content.first() {
        Some(UserContent::ToolResult(tr)) => match tr.content.first() {
            Some(ToolResultContent::Text(t)) => t.text.clone(),
            Some(ToolResultContent::Json { value }) => value.to_string(),
            other => panic!("unexpected block {other:?}"),
        },
        Some(UserContent::Text(t)) => t.text.clone(),
        other => panic!("unexpected content {other:?}"),
    }
}

fn settings(cap: usize) -> ContextShaperSettings {
    ContextShaperSettings {
        tool_result_cap_bytes: cap,
        keep_head: 1,
        keep_tail: 2,
        response_reserve: 0,
        headroom_fraction: 0.0,
        ..ContextShaperSettings::default()
    }
}

fn counter() -> TokenCounter {
    TokenCounter::for_model("test")
}

/// A guard whose budget is exactly `tokens(history) - 1`: over by one,
/// so the cheapest stage that frees anything is the one that fires.
fn shaper_just_over(history: &[Message], settings: ContextShaperSettings) -> ContextShaper {
    let counter = counter();
    let total: usize = history.iter().map(|m| counter.count_message(m)).sum();
    ContextShaper::new(settings, counter, Some(total - 1))
}

/// AGE-967: a configured cap the request leaves no room for is clamped
/// to the window less the headroom, the base and the request, never
/// under the floor; a cap that fits, or no cap, is left alone.
#[test]
fn the_output_cap_is_clamped_to_the_room_the_window_has_left() {
    let history = vec![user_text(&"word ".repeat(400))];
    let prompt = user_text("and now?");
    let c = counter();
    let request = c.count_message(&history[0]) + c.count_message(&prompt);
    let shaper = |cap: Option<usize>| {
        let shaper = ContextShaper::new(
            ContextShaperSettings {
                headroom_fraction: 0.1,
                output_cap: cap,
                ..ContextShaperSettings::default()
            },
            counter(),
            Some(10_000),
        );
        shaper.set_base_tokens(1_000);
        shaper
    };
    let room = 10_000 - 1_000 - 1_000 - request;
    assert_eq!(
        shaper(Some(9_000)).clamped_output_cap(&history, &prompt),
        Some(room as u64)
    );
    assert_eq!(
        shaper(Some(room)).clamped_output_cap(&history, &prompt),
        None,
        "a cap that fits"
    );
    assert_eq!(shaper(None).clamped_output_cap(&history, &prompt), None);
    let long = vec![user_text(&"word ".repeat(9_000))];
    assert_eq!(
        shaper(Some(9_000)).clamped_output_cap(&long, &prompt),
        Some(MIN_CLAMPED_REPLY_TOKENS as u64),
        "never under the floor"
    );
}

/// AGE-985: the cap leaves at least 10% of the counted input plus the
/// slack, even with no window headroom, and the margin is configurable.
#[test]
fn the_output_cap_keeps_a_margin_on_the_counted_input() {
    let history = vec![user_text(&"word ".repeat(2_000))];
    let prompt = user_text("and now?");
    let c = counter();
    let input = c.count_message(&history[0]) + c.count_message(&prompt);
    let shaper = |margin: OutputMargin| {
        ContextShaper::new(
            ContextShaperSettings {
                headroom_fraction: 0.0,
                output_cap: Some(32_000),
                output_margin: margin,
                ..ContextShaperSettings::default()
            },
            counter(),
            Some(32_768),
        )
    };
    let default = shaper(OutputMargin::default())
        .clamped_output_cap(&history, &prompt)
        .unwrap() as usize;
    assert_eq!(default, 32_768 - input - (input / 10 + 256), "{default}");
    let wide = shaper(OutputMargin {
        fraction: 0.2,
        slack: 1_000,
    })
    .clamped_output_cap(&history, &prompt)
    .unwrap() as usize;
    assert!(wide < default - 200, "{wide} {default}");
}

/// AGE-967: a run that ran out of window keeps half of it for the
/// reply, so a history that fits as usual is shaped once the run
/// carries `ReplyRoom`.
#[tokio::test]
async fn room_for_the_reply_shapes_a_history_that_otherwise_fits() {
    let history = tool_loop(6);
    let prompt = user_text("and now?");
    let c = counter();
    let total: usize =
        history.iter().map(|m| c.count_message(m)).sum::<usize>() + c.count_message(&prompt);
    // Fits with room to spare under the usual reserve, not with half
    // the window kept free.
    let shaper = ContextShaper::new(settings(1_000), counter(), Some(total * 3 / 2));
    assert!(shaper.prepare_request(&history, &prompt, 1).await.is_none());
    let extra = shaper.reply_room_reserve();
    assert_eq!(extra, total * 3 / 4, "half the window, the reserve being 0");
    let shaped = shaper
        .prepare_request_reserving(&history, &prompt, 1, extra)
        .await
        .expect("shaped for the reply's room");
    let sent: usize = shaped.iter().map(|m| c.count_message(m)).sum();
    assert!(sent < total, "{sent} < {total}");
}

#[test]
fn budget_takes_reserve_base_and_prompt_off_the_window() {
    let shaper = ContextShaper::new(
        ContextShaperSettings {
            response_reserve: 100,
            headroom_fraction: 0.1,
            ..ContextShaperSettings::default()
        },
        counter(),
        Some(1_000),
    );
    shaper.set_base_tokens(300);
    assert_eq!(
        shaper.history_budget(50),
        450,
        "1000 − 100 headroom − 100 reserve − 300 base − 50 prompt"
    );
    assert_eq!(
        shaper.history_budget(10_000),
        0,
        "saturates, never underflows"
    );
}

#[test]
fn a_model_without_a_window_gets_the_assumed_one() {
    let config = ModelConfig::new(
        "id".into(),
        "name".into(),
        crate::settings::models::providers_store::ProviderType::Ollama,
        "qwen3".into(),
    );
    let shaper = ContextShaper::for_model(&config);
    assert_eq!(shaper.context_window(), ASSUMED_CONTEXT_WINDOW);
    assert_eq!(
        shaper.history_budget(0),
        ASSUMED_CONTEXT_WINDOW - ASSUMED_CONTEXT_WINDOW / 10 - RESPONSE_RESERVE
    );
}

#[test]
fn a_configured_window_and_max_tokens_win() {
    let mut config = ModelConfig::new(
        "id".into(),
        "name".into(),
        crate::settings::models::providers_store::ProviderType::OpenRouter,
        "vendor/model".into(),
    );
    config.max_context_window = Some(200_000);
    config.max_tokens = Some(8_000);
    let shaper = ContextShaper::for_model(&config);
    assert_eq!(shaper.history_budget(0), 200_000 - 20_000 - 8_000);
}

#[test]
fn a_history_that_fits_is_left_alone() {
    let history = vec![user_text("hello"), text_result("t1", 50)];
    let shaper = ContextShaper::new(settings(100), counter(), Some(10_000));
    assert!(shaper.shape(&history, 10).is_none());
}

/// AGE-587: an old terminal snapshot is the first thing to go, before
/// any tool result, and the newest one stays whole.
#[test]
fn old_terminal_snapshots_go_first() {
    use crate::services::terminal::context::{
        TerminalContext, TerminalContextInput, last_attached,
    };
    let snapshot = |text: &str| {
        TerminalContext::build(
            TerminalContextInput {
                tab: "bash",
                cwd: None,
                last_command: None,
                text,
            },
            &counter(),
        )
        .block
    };
    let with_snapshot = |typed: &str, block: String| Message::User {
        content: vec![
            UserContent::Text(Text::new(typed.to_string())),
            UserContent::Text(Text::new(block)),
        ],
    };
    let old = snapshot(&"old output line\n".repeat(30));
    let new = snapshot("new output");
    let history = vec![
        with_snapshot("why?", old),
        text_result("old", 200),
        with_snapshot("and now?", new.clone()),
        text_result("recent", 100),
    ];
    let shaper = shaper_just_over(&history, settings(100));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::TerminalContext);
    assert_eq!(
        result_text(&shaped.messages[1]),
        result_text(&history[1]),
        "tool results are untouched"
    );
    assert_eq!(last_attached(shaped.messages.iter()), Some(new.as_str()));
    let Message::User { content } = &shaped.messages[0] else {
        unreachable!()
    };
    assert!(
        matches!(content.last(), Some(UserContent::Text(t)) if t.text.starts_with("[older terminal snapshot"))
    );
}

/// The AGE-500 shape: the run is over budget, and there is old material
/// to give up before the tail is touched.
#[test]
fn caps_old_results_before_touching_the_tail() {
    let history = vec![
        user_text("task"),
        text_result("old", 200),
        user_text("ok"),
        text_result("recent", 100),
    ];
    let shaper = shaper_just_over(&history, settings(100));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::Cap);
    assert!(result_text(&shaped.messages[1]).starts_with("[tool result truncated — 999 chars]"));
    assert_eq!(
        result_text(&shaped.messages[3]),
        result_text(&history[3]),
        "the tail stays whole"
    );
    assert!(shaped.tokens_after <= shaper.history_budget(0));
    assert!(shaped.tokens_after < shaped.tokens_before);
}

#[test]
fn json_results_count_and_cap_like_text() {
    let history = vec![
        user_text("task"),
        json_result("old", 200),
        user_text("ok"),
        json_result("recent", 100),
    ];
    let shaper = shaper_just_over(&history, settings(100));
    let shaped = shaper
        .shape(&history, 0)
        .expect("a JSON result is not invisible");
    assert_eq!(shaped.stage_applied, ContextShaperStage::Cap);
    assert!(result_text(&shaped.messages[1]).starts_with("[tool result truncated — "));
    assert_eq!(result_text(&shaped.messages[3]), result_text(&history[3]));
}

#[test]
fn one_lines_before_snipping() {
    let mut history = vec![user_text("task")];
    for i in 0..4 {
        history.push(text_result(&format!("t{i}"), 100));
    }
    // Every result is under the 1 KB cap, so capping frees nothing;
    // one-lining the two outside the tail does, and nothing is dropped.
    let shaper = shaper_just_over(&history, settings(1_000));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::Compact);
    assert_eq!(shaped.messages.len(), history.len());
    assert!(result_text(&shaped.messages[1]).starts_with("[compacted] "));
    assert!(result_text(&shaped.messages[2]).starts_with("[compacted] "));
    assert_eq!(result_text(&shaped.messages[4]), result_text(&history[4]));
}

#[test]
fn snips_when_capping_and_compacting_free_nothing() {
    // Six results too short to cap or one-line: only dropping helps.
    let mut history = vec![user_text("task")];
    for i in 0..6 {
        history.push(text_result(&format!("t{i}"), 20));
    }
    let shaper = shaper_just_over(&history, settings(1_000));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
    assert_eq!(shaped.messages.len(), 1 + 1 + 2);
    assert!(result_text(&shaped.messages[1]).contains("4 messages snipped"));
    assert_eq!(result_text(&shaped.messages[3]), result_text(&history[6]));
}

/// Every tool result in `messages` is answered by a call before it, and
/// every call is answered after it — what the providers check.
fn assert_round_trips_intact(messages: &[Message]) {
    let mut called: HashSet<CallId> = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        for id in result_ids(message) {
            assert!(
                called.contains(id),
                "message {index} answers {id:?}, which nothing before it called"
            );
        }
        called.extend(call_ids(message).cloned());
    }
    let answered: HashSet<&CallId> = messages.iter().flat_map(result_ids).collect();
    for (index, message) in messages.iter().enumerate() {
        for id in call_ids(message) {
            assert!(
                answered.contains(id),
                "message {index} calls {id:?}, which nothing answers"
            );
        }
    }
}

fn tool_call(id: &str) -> Message {
    Message::Assistant(rig_core::message::AssistantMessage::new(vec![
        AssistantContent::tool_call(
            id,
            crate::testing::tool_name("test_tool"),
            serde_json::json!({}),
        ),
    ]))
}

/// `n` tool round-trips after a user message: the shape every step of a
/// live tool loop has, and the one the in-loop guard shapes.
fn tool_loop(n: usize) -> Vec<Message> {
    let mut history = vec![user_text("task")];
    for i in 0..n {
        history.push(tool_call(&format!("t{i}")));
        history.push(text_result(&format!("t{i}"), 20));
    }
    history
}

/// The request a live tool loop makes: the history ends on the call, and
/// the result answering it is the prompt rig passes separately. Nothing
/// is dangling, so the request goes out exactly as recorded — the
/// AGE-277 property, and the thing a naive validator would break by
/// synthesizing a second answer for the id the prompt carries.
#[test]
fn a_call_the_prompt_answers_is_not_a_reason_to_patch() {
    let history = vec![user_text("task"), tool_call("t0")];
    let prompt = text_result("t0", 20);
    let shaper = ContextShaper::new(settings(1_000), counter(), Some(10_000));

    assert_eq!(shaper.request_history(&history, &prompt, 2), None);
}

/// A history that fits but carries an orphaned tool result is still
/// patched: the guard is the last place that can catch it, whatever left
/// it that way (AGE-513).
#[test]
fn an_orphaned_result_is_repaired_even_under_budget() {
    let history = vec![user_text("task"), text_result("nobody-called-this", 20)];
    let shaper = ContextShaper::new(settings(1_000), counter(), Some(10_000));

    let patched = shaper
        .request_history(&history, &user_text("and now?"), 1)
        .expect("a malformed history is patched even when it fits");

    assert_eq!(patched.len(), 1, "the orphaned result is gone");
    assert_round_trips_intact(&patched);
}

/// Shaping and repair compose: an over-budget history that also ends on
/// a call nothing answers comes back both shorter and whole.
#[test]
fn a_shaped_history_is_repaired_too() {
    let mut history = tool_loop(3);
    // The run was cut off after this call and before its result.
    history.push(tool_call("never-answered"));
    let settings = ContextShaperSettings {
        keep_head: 1,
        keep_tail: 2,
        ..settings(1_000)
    };
    let shaper = shaper_just_over(&history, settings);

    // The prompt is a new user turn, so it answers nothing.
    let patched = shaper
        .request_history(&history, &user_text("and now?"), 4)
        .expect("over budget");

    assert!(patched.len() < history.len(), "still shaped");
    assert_round_trips_intact(&patched);
    assert!(
        result_text(patched.last().expect("a repaired history is not empty"))
            .starts_with("Cancelled:"),
        "the unanswered call got its placeholder"
    );
}

/// The AGE-512 400: the cut fell between an assistant `tool_calls`
/// message and the result answering it, and the provider rejected the
/// next request with "messages with role 'tool' must be a response to a
/// preceding message with 'tool_calls'".
#[test]
fn snip_keeps_the_call_the_tail_answers() {
    let history = tool_loop(3);
    let settings = ContextShaperSettings {
        keep_head: 1,
        keep_tail: 3,
        ..settings(1_000)
    };
    // Unadjusted, the tail would start on `t1`'s result, whose call is
    // the message before it.
    assert!(matches!(history[history.len() - 3], Message::User { .. }));

    let shaper = shaper_just_over(&history, settings);
    let shaped = shaper.shape(&history, 0).expect("over budget");

    assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
    assert_round_trips_intact(&shaped.messages);
    assert!(result_text(&shaped.messages[1]).contains("2 messages snipped"));
    assert!(
        call_ids(&shaped.messages[2]).any(|id| id.to_string() == "t1"),
        "the kept tail opens with the call it answers"
    );
}

/// The other end of the same cut: the kept head must not end on a call
/// whose result the snip drops.
#[test]
fn snip_never_keeps_a_head_call_it_drops_the_result_of() {
    let history = tool_loop(3);
    let settings = ContextShaperSettings {
        keep_head: 2,
        keep_tail: 2,
        ..settings(1_000)
    };
    // Unadjusted, the head would end on `t0`'s call, whose result is the
    // first message the snip drops.
    assert!(call_ids(&history[1]).any(|id| id.to_string() == "t0"));

    let shaper = shaper_just_over(&history, settings);
    let shaped = shaper.shape(&history, 0).expect("over budget");

    assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
    assert_round_trips_intact(&shaped.messages);
    assert_eq!(shaped.messages.len(), 1 + 1 + 2);
}

/// Two fetches and nothing older — trial `2dfc4c37…` in AGE-500. The
/// older one is stubbed, the newest stays whole.
#[test]
fn caps_the_tail_oldest_first_and_never_the_last_message() {
    let history = vec![
        user_text("task"),
        text_result("first-fetch", 300),
        text_result("second-fetch", 200),
    ];
    let shaper = shaper_just_over(&history, settings(100));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::CapTail);
    assert!(result_text(&shaped.messages[1]).starts_with("[tool result truncated — 1499 chars]"));
    assert_eq!(result_text(&shaped.messages[2]), result_text(&history[2]));
}

#[test]
fn a_history_still_over_budget_is_sent_shaped_not_dropped() {
    let history = vec![user_text("task"), text_result("only", 1_000)];
    let shaper = ContextShaper::new(settings(100), counter(), Some(10));
    let shaped = shaper.shape(&history, 0).expect("over budget");
    assert_eq!(shaped.messages.len(), 2);
    assert_eq!(
        result_text(&shaped.messages[1]),
        result_text(&history[1]),
        "the last message is never touched"
    );
    assert!(shaped.tokens_after > shaper.history_budget(0));
}

#[test]
fn the_prompt_counts_against_the_budget() {
    let history = vec![user_text("task"), text_result("old", 100), user_text("ok")];
    let counter = counter();
    let total: usize = history.iter().map(|m| counter.count_message(m)).sum();
    let shaper = ContextShaper::new(settings(100), counter, Some(total));
    assert!(shaper.shape(&history, 0).is_none());
    assert!(
        shaper.shape(&history, 1).is_some(),
        "a large prompt leaves less for the history"
    );
}

#[test]
fn images_are_left_alone() {
    use rig_core::completion::message::{DocumentSourceKind, Image, ImageMediaType};
    let mut message = tool_result(
        "img",
        ToolResultContent::Image(Image {
            data: DocumentSourceKind::base64("AAAA"),
            media_type: Some(ImageMediaType::PNG),
            detail: None,
            ..Default::default()
        }),
    );
    assert!(!cap_tool_results(&mut message, 1));
    assert!(!compact_tool_results(&mut message));
}

#[test]
fn a_result_under_the_recording_cap_is_recorded_whole() {
    let shaper = ContextShaper::new(settings(100), counter(), Some(10_000));
    let output = ToolOutput::json(serde_json::json!({ "content": vec!["word"; 200].join(" ") }));
    assert!(shaper.record_tool_output(&output).is_none());
}

#[test]
fn a_result_over_the_recording_cap_is_cut_to_the_cap_once() {
    // Budget 2 000 → cap 800 tokens; 3 000 words are ~3 000 tokens.
    let shaper = ContextShaper::new(settings(100), counter(), Some(2_000));
    let words = vec!["word"; 3_000].join(" ");
    let output = ToolOutput::json(serde_json::json!({ "content": words }));
    let recorded = shaper.record_tool_output(&output).expect("over the cap");
    let text = recorded.as_text().expect("recorded as text");
    assert!(text.starts_with("[tool result truncated — kept "));
    let kept = shaper.counter().count(text);
    assert!(
        kept <= shaper.recording_cap() + 40,
        "kept {kept} tokens against a cap of {}",
        shaper.recording_cap()
    );
    assert!(
        kept > shaper.recording_cap() / 2,
        "keeps most of what fits, not a stub"
    );
    // Recording again is a no-op: the truncated form fits.
    assert!(shaper.record_tool_output(&recorded).is_none());
}

#[test]
fn the_recording_cap_has_a_floor() {
    let shaper = ContextShaper::new(settings(100), counter(), Some(10));
    assert_eq!(shaper.recording_cap(), RECORDING_CAP_FLOOR_TOKENS);
}

#[test]
fn a_result_with_an_image_is_recorded_whole() {
    use rig_core::completion::message::{DocumentSourceKind, Image, ImageMediaType};
    let shaper = ContextShaper::new(settings(100), counter(), Some(10));
    let output = ToolOutput::content(vec![
        ToolResultContent::Text(Text::new(vec!["word"; 3_000].join(" "))),
        ToolResultContent::Image(Image {
            data: DocumentSourceKind::base64("AAAA"),
            media_type: Some(ImageMediaType::PNG),
            detail: None,
            ..Default::default()
        }),
    ])
    .unwrap();
    assert!(shaper.record_tool_output(&output).is_none());
}

/// AGE-978: a result larger than the whole window is recorded as its
/// head and its tail, with a marker in the middle saying how to read
/// the rest, and what is recorded fits the window.
#[test]
fn a_result_larger_than_the_window_keeps_its_head_and_tail() {
    let window = 2_000;
    let shaper = ContextShaper::new(settings(100), counter(), Some(window));
    let text = format!("FIRST-LINE {} LAST-LINE", vec!["word"; 10_000].join(" "));
    let recorded = shaper
        .record_tool_output(&ToolOutput::text(text))
        .expect("over the cap");
    let kept = recorded.as_text().expect("recorded as text");
    assert!(kept.starts_with("[tool result truncated — kept "), "{kept}");
    assert!(kept.contains("FIRST-LINE"), "the head is kept");
    assert!(kept.ends_with("LAST-LINE"), "the tail is kept");
    assert!(
        kept.contains("omitted here") && kept.contains("start_line"),
        "the marker says how to read the rest: {kept}"
    );
    let tokens = shaper.counter().count(kept);
    assert!(
        tokens <= shaper.recording_cap() + 40 && tokens < window,
        "{tokens} tokens fit the window"
    );
    // Recording again is a no-op: the truncated form fits.
    assert!(shaper.record_tool_output(&recorded).is_none());
}

/// AGE-978: the results of one tool round go out together in the one
/// message the guard cannot shape, so each is capped at a share of the
/// room the earlier ones left, and together they fit the budget. A new
/// round starts over.
#[test]
fn the_results_of_one_round_share_the_room_left() {
    let window = 10_000;
    let shaper = ContextShaper::new(settings(100), counter(), Some(window));
    let output = ToolOutput::text(vec!["word"; 3_000].join(" "));
    assert!(
        shaper.counter().count(output.as_text().unwrap()) < shaper.recording_cap(),
        "fixture: one result alone fits the cap"
    );
    shaper.start_round();
    let mut total = 0;
    for _ in 0..4 {
        let recorded = shaper.record_round_output(&output);
        let sent = recorded.as_ref().unwrap_or(&output);
        total += shaper.counter().count(sent.as_text().unwrap());
    }
    assert!(
        total <= shaper.history_budget(0),
        "four results of one round take {total} of {} tokens",
        shaper.history_budget(0)
    );
    shaper.start_round();
    assert!(
        shaper.record_round_output(&output).is_none(),
        "a new round has the whole room again"
    );
}

/// AGE-978: a request the provider refused as over its window means the
/// count ran short, so every later request keeps more headroom — a tenth
/// of the window per refusal, up to three.
#[test]
fn a_refused_request_tightens_the_budget() {
    let shaper = ContextShaper::new(
        ContextShaperSettings {
            response_reserve: 0,
            headroom_fraction: 0.1,
            ..ContextShaperSettings::default()
        },
        counter(),
        Some(10_000),
    );
    assert_eq!(shaper.history_budget(0), 9_000);
    let overflows = shaper.window_overflows();
    overflows.record();
    assert_eq!(shaper.history_budget(0), 8_000);
    for _ in 0..10 {
        overflows.record();
    }
    assert_eq!(shaper.history_budget(0), 6_000, "at most three tenths more");
}
