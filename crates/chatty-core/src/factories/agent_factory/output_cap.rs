//! The output cap an agent's requests carry, when one is configured
//! (AGE-936).
//!
//! The cap is opt-in, decided once per agent:
//!
//! 1. the spec's `[agent] max_tokens` (a role that needs a cap);
//! 2. else the model's own `max_tokens` (its roster entry; zero or less is
//!    unset);
//! 3. else none: the request carries no `max_tokens`, exactly as before.
//!
//! There is deliberately no default. A fixed cap counts a thinking model's
//! reasoning too, and an ordinary call can need most of 8k tokens; a cut-off
//! costs a paid retry or leaves a partial answer. Fitting a call to a task's
//! budget is the hosted egress's job: it can lower a call's `max_tokens` to
//! what the budget has left. Either way a call that reaches its cap is
//! handled — see `empty_turn_retry` and the truncation note in
//! `llm_service`.
//!
//! The capped config is what the agent is built with, so the context guard
//! reserves the same number for the reply (vLLM refuses a request whose
//! prompt plus `max_tokens` exceeds the window), and a spec's plugins
//! (`llm::complete`) send the same cap.

use crate::settings::models::models_store::ModelConfig;

/// `model_config` as an agent with `spec_max_tokens` (the spec's
/// `[agent] max_tokens`) is built with: its `max_tokens` the spec's, else
/// its own when positive, else none (per the module docs).
pub(crate) fn with_output_cap(
    model_config: &ModelConfig,
    spec_max_tokens: Option<u32>,
) -> ModelConfig {
    let mut capped = model_config.clone();
    capped.max_tokens = spec_max_tokens
        .map(|n| n.min(i32::MAX as u32) as i32)
        .or(model_config.max_tokens.filter(|n| *n > 0));
    capped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

    fn model(provider: ProviderType) -> ModelConfig {
        ModelConfig::new(
            "m".to_string(),
            "m".to_string(),
            provider,
            "qwen/qwen3-30b".to_string(),
        )
    }

    #[test]
    fn the_spec_beats_the_model_and_nothing_is_no_cap() {
        let mut model = model(ProviderType::OpenRouter);
        assert_eq!(with_output_cap(&model, None).max_tokens, None);
        assert_eq!(with_output_cap(&model, Some(500)).max_tokens, Some(500));
        model.max_tokens = Some(2_000);
        assert_eq!(with_output_cap(&model, None).max_tokens, Some(2_000));
        assert_eq!(with_output_cap(&model, Some(500)).max_tokens, Some(500));
        // A zero or negative model cap is no cap, as the context guard reads it.
        model.max_tokens = Some(0);
        assert_eq!(with_output_cap(&model, None).max_tokens, None);
    }

    /// With nothing configured the agent is built from the model config as
    /// it was, so the context guard keeps its 4k reply reserve
    /// (`RESPONSE_RESERVE`), unchanged from before AGE-936.
    #[test]
    fn with_nothing_configured_the_config_and_reply_reserve_are_unchanged() {
        use crate::services::context_shaper::{ContextShaper, ContextShaperSettings};
        use crate::token_budget::counter::TokenCounter;

        for provider in [
            ProviderType::OpenRouter,
            ProviderType::Ollama,
            ProviderType::AzureOpenAI,
        ] {
            let model = model(provider);
            let capped = with_output_cap(&model, None);
            assert_eq!(
                serde_json::to_value(&capped).unwrap(),
                serde_json::to_value(&model).unwrap()
            );
            let default_reserve = ContextShaper::new(
                ContextShaperSettings::default(),
                TokenCounter::for_model(&model.model_identifier),
                None,
            );
            assert_eq!(
                ContextShaper::for_model(&capped).history_budget(0),
                default_reserve.history_budget(0)
            );
        }
    }

    // ── Against the fake model ───────────────────────────────────────────

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::agent_spec::AgentSpec;
    use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
    use crate::services::llm_service::TRUNCATION_NOTE;
    use crate::services::{RecoveryAction, StreamErrorKind, StreamSurface};
    use crate::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
    use crate::settings::models::execution_settings::ExecutionSettingsModel;
    use crate::testing::fake_model::{FakeDaemon, Reply, Script};

    use super::super::OUTPUT_CAP_FOLLOW_UP;
    use crate::session::CUT_OFF_FOLLOW_UP;

    const CHEAP_MODEL: &str = "age-936-cheap-model";

    /// An OpenAI-compatible model behind `daemon`, as a hosted guest's roster
    /// entry is, with `max_tokens` as its own cap.
    fn hosted(daemon: &FakeDaemon, max_tokens: Option<i32>) -> (ModelConfig, ProviderConfig) {
        let mut model = ModelConfig::new(
            CHEAP_MODEL.to_string(),
            CHEAP_MODEL.to_string(),
            ProviderType::OpenRouter,
            CHEAP_MODEL.to_string(),
        );
        model.max_tokens = max_tokens;
        let provider = ProviderConfig::new("Hive".to_string(), ProviderType::OpenRouter)
            .with_api_key("lease-token".to_string())
            .with_base_url(daemon.base_url());
        (model, provider)
    }

    /// The first request body an agent for `spec`, on a model capped at
    /// `model_max_tokens`, sends to the fake model.
    async fn first_request(
        spec: &AgentSpec,
        model_max_tokens: Option<i32>,
    ) -> (serde_json::Value, usize) {
        let _ = crate::init_repositories();
        let daemon = FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, [Reply::text("done")]));
        let (model, provider) = hosted(&daemon, model_max_tokens);
        let built =
            AgentBuildContext::from_spec(spec, AgentServices::default()).expect("the spec builds");
        let agent = super::super::AgentClient::from_model_config_with_tools(
            &model,
            &provider,
            built.context,
        )
        .await
        .expect("the agent builds");
        // Only the request matters here.
        let _ = agent.client.agent.prompt("go").await;
        let request = daemon.requests().into_iter().next().expect("one request");
        (request.json(), request.body.len())
    }

    /// What hive's egress reserves for a request (`Usage::worst_case` in
    /// hive `services/chatty-server/src/egress.rs`, AGE-787/902), mirrored:
    /// one input token per three body bytes, plus the body's
    /// `max_completion_tokens` or `max_tokens`, else 32,768, at the price.
    fn egress_reservation_usd(
        body: &serde_json::Value,
        body_len: usize,
        input_per_m: f64,
        output_per_m: f64,
    ) -> f64 {
        let output = ["max_completion_tokens", "max_tokens"]
            .iter()
            .find_map(|field| body.get(*field).and_then(serde_json::Value::as_u64))
            .unwrap_or(32_768);
        let input = (body_len as u64).div_ceil(3);
        (input as f64 * input_per_m + output as f64 * output_per_m) / 1e6
    }

    /// With no cap configured an OpenAI-compatible request carries no
    /// `max_tokens`, as before AGE-936: no default is invented.
    #[tokio::test]
    async fn no_max_tokens_in_the_body_when_nothing_is_configured() {
        let (body, _) = first_request(&AgentSpec::named("worker"), None).await;
        assert!(body.get("max_tokens").is_none(), "{}", body["max_tokens"]);
        assert!(body.get("max_completion_tokens").is_none());
    }

    /// AGE-936: a configured cap reaches the body as `max_tokens` — a field
    /// the egress reads — so its reservation is the input estimate plus that
    /// cap at the price, and a $0.10 task on a cheap model ($0.50 in / $3 out
    /// per million) capped at 8k fits its first call, which the uncapped 32k
    /// worst case did not.
    #[tokio::test]
    async fn a_configured_cap_reaches_the_body_and_the_reservation_follows_it() {
        let (body, len) = first_request(&AgentSpec::named("worker"), Some(8_192)).await;
        assert_eq!(body["max_tokens"], 8_192, "{}", body["max_tokens"]);

        let (input_per_m, output_per_m) = (0.50, 3.00);
        let reserved = egress_reservation_usd(&body, len, input_per_m, output_per_m);
        let expected =
            (len as u64).div_ceil(3) as f64 * input_per_m / 1e6 + 8_192.0 * output_per_m / 1e6;
        assert!(
            (reserved - expected).abs() < 1e-12,
            "{reserved} vs {expected}"
        );

        let budget_usd = 0.10;
        assert!(
            reserved <= budget_usd,
            "the first call fits: ${reserved:.4}"
        );
        let mut uncapped = body.clone();
        uncapped.as_object_mut().unwrap().remove("max_tokens");
        let before = egress_reservation_usd(&uncapped, len, input_per_m, output_per_m);
        assert!(before > budget_usd, "uncapped it was refused: ${before:.4}");
    }

    /// The spec's `[agent] max_tokens` is the cap the request carries, over
    /// the model's own.
    #[tokio::test]
    async fn the_specs_cap_reaches_the_request() {
        let mut spec = AgentSpec::named("lead");
        spec.agent.max_tokens = Some(1_024);
        let (body, _) = first_request(&spec, Some(8_192)).await;
        assert_eq!(body["max_tokens"], 1_024);
    }

    /// One turn on a hosted model scripted with `replies`, on the headless
    /// surface: the events, the request bodies, and the session after it.
    async fn capped_turn(replies: Vec<Reply>) -> (Vec<SessionEvent>, Vec<String>, AgentSession) {
        let daemon = FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, replies));
        // The cap a hosted egress would set near the end of a budget.
        let (model, provider) = hosted(&daemon, Some(8_192));
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;
        (events, bodies(&daemon), session)
    }

    /// A headless session on `model`, with its workspace (kept alive by the
    /// caller).
    async fn hosted_session(
        model: &ModelConfig,
        provider: &ProviderConfig,
    ) -> (AgentSession, tempfile::TempDir) {
        let _ = crate::init_repositories();
        let workspace = tempfile::tempdir().expect("a workspace");
        let settings = ExecutionSettingsModel {
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            fetch_enabled: false,
            ..ExecutionSettingsModel::default()
        };
        let built = AgentBuildContext::from_spec(
            &AgentSpec::named("worker"),
            AgentServices {
                exec_settings: Some(settings.clone()),
                ..AgentServices::default()
            },
        )
        .expect("the spec builds");
        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings: settings,
            surface: StreamSurface::Headless,
            loop_guard: false,
        });
        session
            .create_conversation(
                "age-936".to_string(),
                "Capped".to_string(),
                model,
                provider,
                built.context,
            )
            .await
            .expect("the conversation builds");
        (session, workspace)
    }

    /// Run `input` as one turn of `session`, applying its events.
    async fn run_turn(session: &mut AgentSession, input: TurnInput) -> Vec<SessionEvent> {
        let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
        let sink = events.clone();
        session
            .begin_turn(input, move |event| sink.borrow_mut().push(event))
            .expect("the turn starts")
            .await;
        let events = Rc::try_unwrap(events).unwrap().into_inner();
        for event in &events {
            session.apply(event);
        }
        events
    }

    fn bodies(daemon: &FakeDaemon) -> Vec<String> {
        daemon
            .requests()
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect()
    }

    fn follow_ups(events: &[SessionEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::FollowUp(prompt) => Some(prompt.clone()),
                _ => None,
            })
            .collect()
    }

    fn answer(session: &AgentSession) -> String {
        session
            .conversation()
            .and_then(|conversation| conversation.streaming_message())
            .cloned()
            .unwrap_or_default()
    }

    fn errors(events: &[SessionEvent]) -> Vec<crate::services::StreamError> {
        events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Error(error) => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    /// A reply cut off at the cap is retried once inside the turn, with
    /// feedback naming the cap, and the retry's answer ends the turn.
    #[tokio::test]
    async fn a_capped_turn_recovers_with_one_retry() {
        let (events, bodies, session) = capped_turn(vec![
            Reply::Truncated("The report, part one".to_string()),
            Reply::text(" and the end."),
        ])
        .await;
        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(bodies.len(), 2, "one call, one retry");
        assert!(!bodies[0].contains(OUTPUT_CAP_FOLLOW_UP));
        assert!(bodies[1].contains(OUTPUT_CAP_FOLLOW_UP), "{}", bodies[1]);
        assert!(bodies[1].contains("of 8192 tokens"), "{}", bodies[1]);
        let answer = session
            .conversation()
            .and_then(|conversation| conversation.streaming_message())
            .cloned()
            .unwrap_or_default();
        assert!(answer.ends_with(" and the end."), "{answer}");
        assert!(!answer.contains(TRUNCATION_NOTE), "{answer}");
    }

    /// A reply cut off again after the retry is accepted as the answer, and
    /// the answer says it may be incomplete, so a leader reading a worker's
    /// result (or a human) does not take it for a complete one.
    #[tokio::test]
    async fn an_answer_accepted_after_a_second_cut_off_carries_the_truncation_note() {
        let (events, bodies, session) = capped_turn(vec![
            Reply::Truncated("The report, part one".to_string()),
            Reply::Truncated(" and part two".to_string()),
            Reply::text("never asked for"),
        ])
        .await;
        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(bodies.len(), 2, "one call, one retry, no more");
        let answer = session
            .conversation()
            .and_then(|conversation| conversation.streaming_message())
            .cloned()
            .unwrap_or_default();
        assert!(answer.ends_with(TRUNCATION_NOTE), "{answer}");
        assert!(answer.contains(" and part two"), "{answer}");
        let streamed: String = events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(streamed.ends_with(TRUNCATION_NOTE), "{streamed}");
    }

    /// A model that only ever hits the cap without answering costs the run
    /// two calls (the call and the feedback retry: the cap is chatty's, not
    /// the window's, so a shorter history would not help, and the turn did
    /// not think), then the turn ends with a follow-up that continues the
    /// task (AGE-967). A follow-up turn cut off the same way ends with the
    /// empty-completion error, which even headless stops on: no loop.
    #[tokio::test]
    async fn a_capped_turn_with_no_answer_continues_once_then_stops() {
        let daemon = FakeDaemon::scripted(
            Script::new().route(CHEAP_MODEL, vec![Reply::Truncated(String::new()); 6]),
        );
        let (model, provider) = hosted(&daemon, Some(8_192));
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;
        assert_eq!(bodies(&daemon).len(), 2, "one call, one retry, no more");
        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(follow_ups(&events), vec![CUT_OFF_FOLLOW_UP.to_string()]);
        session.finish_turn(None, vec![]);

        let events = run_turn(
            &mut session,
            TurnInput::protocol_follow_up(CUT_OFF_FOLLOW_UP),
        )
        .await;
        assert_eq!(bodies(&daemon).len(), 4, "the follow-up's call and retry");
        assert!(follow_ups(&events).is_empty(), "{events:?}");
        let errors = errors(&events);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            errors[0].kind,
            StreamErrorKind::EmptyCompletion,
            "{errors:?}"
        );
        assert!(
            errors[0].message.contains("finish_reason=Length"),
            "{errors:?}"
        );
        assert_eq!(session.recovery_action(&errors[0]), RecoveryAction::Stop);
    }

    // ── AGE-967: a turn the window ran out under ─────────────────────────

    /// What a request body carries when thinking is switched off on an
    /// OpenAI-compatible server (vLLM's Qwen chat template).
    const THINKING_OFF: &str = r#""chat_template_kwargs":{"enable_thinking":false}"#;

    /// What it carries for OpenRouter behind the same URL.
    const REASONING_LOW: &str = r#""reasoning":{"effort":"low"}"#;

    /// The soak's leader on local vLLM: no cap in the request, so the
    /// window is the limit, and a call that reasons until it is full ends
    /// with `finish_reason: length` and no answer. The fake model does that
    /// until a request carries thinking off; the turn is repeated with it
    /// (not answered with feedback: the rejected reasoning stays out of the
    /// history) and completes.
    #[tokio::test]
    async fn a_turn_that_reasoned_until_the_window_ran_out_completes_with_thinking_off() {
        let daemon = FakeDaemon::scripted(
            Script::new()
                .route_body(THINKING_OFF, [Reply::text("The report.")])
                .route(
                    CHEAP_MODEL,
                    [Reply::reason(3, 0), Reply::Truncated(String::new())],
                ),
        );
        let (model, provider) = hosted(&daemon, None);
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert!(follow_ups(&events).is_empty(), "{events:?}");
        let bodies = bodies(&daemon);
        assert_eq!(bodies.len(), 2, "the call and its repeat");
        assert!(!bodies[0].contains(THINKING_OFF), "{}", bodies[0]);
        assert!(bodies[1].contains(THINKING_OFF), "{}", bodies[1]);
        // The fake model sits on a loopback URL, like the hive egress a
        // hosted guest calls, which may forward to OpenRouter: both knobs.
        assert!(bodies[1].contains(REASONING_LOW), "{}", bodies[1]);
        assert!(!bodies[1].contains(OUTPUT_CAP_FOLLOW_UP), "{}", bodies[1]);
        assert_eq!(answer(&session), "The report.");
    }

    /// A model that runs the window out even with thinking off gets one more
    /// repeat with room kept for the reply, then the turn ends with a
    /// follow-up instead of `EmptyCompletion`; thinking stays off for every
    /// call of the run after the first cut-off.
    #[tokio::test]
    async fn a_turn_still_cut_off_with_thinking_off_ends_with_a_follow_up() {
        let thinking = || [Reply::reason(2, 0), Reply::Truncated(String::new())];
        let daemon = FakeDaemon::scripted(
            Script::new().route(CHEAP_MODEL, [thinking(), thinking(), thinking()].concat()),
        );
        let (model, provider) = hosted(&daemon, None);
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(follow_ups(&events), vec![CUT_OFF_FOLLOW_UP.to_string()]);
        let bodies = bodies(&daemon);
        assert_eq!(
            bodies.len(),
            3,
            "the call, thinking off, room for the reply"
        );
        assert!(!bodies[0].contains(THINKING_OFF));
        assert!(bodies[1].contains(THINKING_OFF), "{}", bodies[1]);
        assert!(bodies[2].contains(THINKING_OFF), "{}", bodies[2]);
    }

    /// An output cap of half the window or more already keeps that much
    /// for the reply, so the reply-room step could only repeat the same
    /// request: it is skipped, and the feedback retry takes its place. No
    /// two requests of the turn are identical.
    #[tokio::test]
    async fn with_a_cap_of_half_the_window_the_reply_room_step_is_skipped() {
        let thinking = || [Reply::reason(2, 0), Reply::Truncated(String::new())];
        let daemon = FakeDaemon::scripted(
            Script::new().route(CHEAP_MODEL, [thinking(), thinking(), thinking()].concat()),
        );
        // Clamped to the window (so the cut-off is the window's), with the
        // cap above half of it.
        let (mut model, provider) = hosted(&daemon, Some(30_000));
        model.max_context_window = Some(32_768);
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(follow_ups(&events), vec![CUT_OFF_FOLLOW_UP.to_string()]);
        let bodies = bodies(&daemon);
        assert_eq!(bodies.len(), 3, "the call, thinking off, feedback");
        assert!(bodies[1].contains(THINKING_OFF), "{}", bodies[1]);
        assert!(bodies[2].contains(OUTPUT_CAP_FOLLOW_UP), "{}", bodies[2]);
        for pair in bodies.windows(2) {
            assert_ne!(pair[0], pair[1], "a repeat of the identical request");
        }
    }

    /// A request that leaves the reply less than the clamp's floor in a
    /// configured window is cut off because the window is full: the error
    /// the follow-up turn ends with says so, not "raise max_tokens".
    #[tokio::test]
    async fn a_cut_off_in_an_exhausted_window_says_the_window_is_full() {
        use crate::services::context_shaper::MIN_CLAMPED_REPLY_TOKENS;
        use crate::services::llm_service::WINDOW_EXHAUSTED_ERROR;

        assert!(WINDOW_EXHAUSTED_ERROR.contains(&format!("less than {MIN_CLAMPED_REPLY_TOKENS}")));
        let daemon = FakeDaemon::scripted(
            Script::new().route(CHEAP_MODEL, vec![Reply::Truncated(String::new()); 8]),
        );
        // A window the base and prompt alone fill.
        let (mut model, provider) = hosted(&daemon, Some(2_048));
        model.max_context_window = Some(1_024);
        let (mut session, _workspace) = hosted_session(&model, &provider).await;
        let events = run_turn(&mut session, TurnInput::text("Write the report.")).await;
        assert_eq!(follow_ups(&events), vec![CUT_OFF_FOLLOW_UP.to_string()]);
        session.finish_turn(None, vec![]);

        let events = run_turn(
            &mut session,
            TurnInput::protocol_follow_up(CUT_OFF_FOLLOW_UP),
        )
        .await;
        let errors = errors(&events);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].kind, StreamErrorKind::EmptyCompletion);
        assert_eq!(errors[0].message, WINDOW_EXHAUSTED_ERROR, "{errors:?}");
    }

    /// The thinking switch is the model's own request fields with thinking
    /// flipped off, per wire, and none where it is unknown or already off.
    #[test]
    fn thinking_off_params_per_provider() {
        use super::super::provider_builder::thinking_off_params;
        let daemon_url = "http://127.0.0.1:9/v1".to_string();
        let compat =
            ProviderConfig::new("vllm".into(), ProviderType::OpenRouter).with_base_url(daemon_url);
        let both = serde_json::json!({
            "chat_template_kwargs": { "enable_thinking": false },
            "reasoning": { "effort": "low" },
        });
        let mut qwen = model(ProviderType::OpenRouter);
        assert_eq!(thinking_off_params(&qwen, &compat), Some(both.clone()));
        qwen.extra_params.insert("think".into(), "true".into());
        assert_eq!(thinking_off_params(&qwen, &compat), Some(both));
        qwen.extra_params.insert("think".into(), "false".into());
        assert_eq!(thinking_off_params(&qwen, &compat), None, "already off");

        let plain = model(ProviderType::OpenRouter);
        let openrouter = ProviderConfig::new("or".into(), ProviderType::OpenRouter);
        assert_eq!(
            thinking_off_params(&plain, &openrouter),
            Some(serde_json::json!({ "reasoning": { "effort": "low" } }))
        );
        let openai = ProviderConfig::new("openai".into(), ProviderType::OpenRouter)
            .with_base_url("https://api.openai.com/v1".into());
        assert_eq!(thinking_off_params(&plain, &openai), None);

        let ollama = ProviderConfig::new("ollama".into(), ProviderType::Ollama);
        assert_eq!(
            thinking_off_params(&model(ProviderType::Ollama), &ollama),
            Some(serde_json::json!({ "think": false }))
        );
        let azure = ProviderConfig::new("azure".into(), ProviderType::AzureOpenAI);
        assert_eq!(
            thinking_off_params(&model(ProviderType::AzureOpenAI), &azure),
            None
        );
    }

    /// A configured cap the window has no room for is clamped to the room
    /// left, so vLLM does not refuse the request outright (its prompt plus
    /// `max_tokens` over the window); a cap that fits goes out unchanged.
    #[tokio::test]
    async fn a_cap_larger_than_the_window_has_room_for_is_clamped() {
        use crate::services::context_shaper::MIN_CLAMPED_REPLY_TOKENS;

        let _ = crate::init_repositories();
        for (window, cap, clamped) in [(32_768, 30_000, true), (32_768, 2_048, false)] {
            let daemon =
                FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, [Reply::text("done")]));
            let (mut model, provider) = hosted(&daemon, Some(cap));
            model.max_context_window = Some(window);
            let built =
                AgentBuildContext::from_spec(&AgentSpec::named("worker"), AgentServices::default())
                    .expect("the spec builds");
            let agent = super::super::AgentClient::from_model_config_with_tools(
                &model,
                &provider,
                built.context,
            )
            .await
            .expect("the agent builds");
            let _ = agent.client.agent.prompt("go").await;
            let body = daemon.requests()[0].json();
            let sent = body["max_tokens"].as_u64().expect("a cap");
            if clamped {
                assert!(sent < cap as u64, "{sent}");
                assert!(sent >= MIN_CLAMPED_REPLY_TOKENS as u64, "{sent}");
                // Near the window: the base and the prompt are all it lost,
                // beyond the tenth kept for the tokenizer mismatch.
                assert!(sent as f64 > window as f64 * 0.5, "{sent}");
            } else {
                assert_eq!(sent, cap as u64);
            }
        }
    }

    // ── AGE-978: a worker whose tool results outgrow the window ──────────

    /// vLLM's 32k window, as the AGE-900 acceptance run served Qwen.
    const VLLM_WINDOW: usize = 32_768;

    /// A data file of `chars` characters: rows of numbers, the text a
    /// DABstep worker reads.
    fn data_file(chars: usize) -> String {
        let mut text = String::from("id,amount,fee,merchant\n");
        let mut i = 0u64;
        while text.len() < chars {
            text.push_str(&format!(
                "{i},{}.{:02},{}.{:03},{}\n",
                (i * 7919) % 100_000,
                i % 100,
                (i * 104_729) % 1_000,
                i % 1_000,
                (i * 31) % 9_973
            ));
            i += 1;
        }
        text.truncate(chars);
        text
    }

    /// A hosted worker on a model with vLLM's window, whose workspace holds
    /// `files` data files of `chars` characters each.
    async fn worker_with_files(
        daemon: &FakeDaemon,
        files: &[&str],
        chars: usize,
    ) -> (AgentSession, tempfile::TempDir) {
        let (mut model, provider) = hosted(daemon, None);
        model.max_context_window = Some(VLLM_WINDOW as i32);
        let (session, workspace) = hosted_session(&model, &provider).await;
        for name in files {
            std::fs::write(workspace.path().join(name), data_file(chars)).unwrap();
        }
        (session, workspace)
    }

    fn read(path: &str) -> (String, serde_json::Value) {
        ("read_file".to_string(), serde_json::json!({ "path": path }))
    }

    /// AGE-978, criterion 1: a worker reads four data files in one round —
    /// each read under `read_file`'s own cap, together more than the whole
    /// window. A model server strict about its window refuses nothing: the
    /// next request fits, and the worker finishes.
    #[tokio::test]
    async fn tool_results_larger_than_the_window_do_not_end_the_worker() {
        let files = ["a.csv", "b.csv", "c.csv", "d.csv"];
        let daemon = FakeDaemon::scripted(Script::new().route(
            CHEAP_MODEL,
            [
                Reply::ToolCalls(files.iter().map(|f| read(f)).collect()),
                Reply::text("done"),
            ],
        ))
        .with_window(VLLM_WINDOW);
        let (mut session, _workspace) = worker_with_files(&daemon, &files, 20_000).await;
        let counter = crate::token_budget::counter::TokenCounter::for_model("fake");
        assert!(
            counter.count(&data_file(20_000)) * files.len() > VLLM_WINDOW,
            "fixture: the four reads together are larger than the window"
        );

        let events = run_turn(&mut session, TurnInput::text("Total the fees.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(answer(&session), "done");
        assert_eq!(daemon.refused(), 0, "no request was over the window");
        let bodies = bodies(&daemon);
        assert_eq!(bodies.len(), 2, "the reads, then the answer");
        for name in files {
            assert!(bodies[1].contains(name), "every read is answered: {name}");
        }
        assert!(
            bodies[1].contains("omitted here"),
            "the reads the window had no room for are cut, with the marker"
        );
    }

    /// AGE-978, criterion 2: a context-length 400 leads to one
    /// compaction-and-retry, not a failure: the same request goes out again
    /// with its oldest tool result stubbed, the worker finishes, and its
    /// guard keeps more headroom from then on.
    #[tokio::test]
    async fn a_context_length_400_is_compacted_and_retried_once() {
        let overflow = serde_json::json!({ "error": {
            "message": "This model's maximum context length is 32768 tokens. However, you \
                        requested 0 output tokens and your prompt contains at least 32769 \
                        input tokens, for a total of at least 32769 tokens. Please reduce the \
                        length of the input prompt or the number of requested output tokens. \
                        (parameter=input_tokens, value=32769)",
            "type": "BadRequestError", "param": "input_tokens", "code": 400
        }});
        let daemon = FakeDaemon::scripted(Script::new().route(
            CHEAP_MODEL,
            [
                Reply::ToolCalls(vec![read("a.csv")]),
                Reply::ToolCalls(vec![read("b.csv")]),
                Reply::ErrorBody(400, overflow),
                Reply::text("done"),
            ],
        ));
        let (mut session, _workspace) =
            worker_with_files(&daemon, &["a.csv", "b.csv"], 2_000).await;
        let guard = session
            .conversation()
            .expect("the conversation")
            .agent()
            .context_shaper()
            .clone();
        let budget = guard.history_budget(0);

        let events = run_turn(&mut session, TurnInput::text("Total the fees.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(answer(&session), "done");
        let bodies = bodies(&daemon);
        assert_eq!(bodies.len(), 4, "two reads, the refused request, its retry");
        assert!(!bodies[2].contains("[tool result dropped"));
        assert!(
            bodies[3].contains("[tool result dropped"),
            "the retry carries the oldest result compacted"
        );
        assert_eq!(guard.window_overflows().count(), 1, "one refusal counted");
        assert!(
            guard.history_budget(0) < budget,
            "later requests keep more headroom"
        );
    }

    // ── AGE-985: a request that asks for more output than the window leaves

    /// A user message of about `words` tokens.
    fn long_prompt(words: usize) -> TurnInput {
        TurnInput::text("word ".repeat(words))
    }

    /// Criterion 1: a model server whose tokenizer counts 5% more than ours
    /// and that counts the requested `max_tokens` against its window refuses
    /// nothing: the cap leaves the margin, and the agent finishes.
    #[tokio::test]
    async fn an_input_undercounted_by_five_percent_still_fits_with_its_output_cap() {
        let daemon = FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, [Reply::text("done")]))
            .with_window(VLLM_WINDOW)
            .with_tokenizer_gap(1.05);
        // The cap a typesetter asks for: all but the prompt of the window.
        let (mut model, provider) = hosted(&daemon, Some(VLLM_WINDOW as i32 - 2_000));
        model.max_context_window = Some(VLLM_WINDOW as i32);
        let (mut session, _workspace) = hosted_session(&model, &provider).await;

        let events = run_turn(&mut session, long_prompt(2_500)).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(answer(&session), "done");
        assert_eq!(daemon.refused(), 0);
        let sent = daemon.requests()[0].json()["max_tokens"].as_u64().unwrap();
        assert!(sent < VLLM_WINDOW as u64 - 2_000, "{sent}");
    }

    /// Criterion 2: a 400 that says `requested N output tokens` is retried
    /// once with `max_tokens` lowered by the overshoot plus the margin, and
    /// the agent finishes. The model has no configured window here, so the
    /// guard cannot have clamped the cap beforehand.
    #[tokio::test]
    async fn a_requested_output_400_is_retried_once_with_a_lower_max_tokens() {
        let daemon = FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, [Reply::text("done")]))
            .with_window(VLLM_WINDOW);
        let (model, provider) = hosted(&daemon, Some(30_500));
        let (mut session, _workspace) = hosted_session(&model, &provider).await;

        let events = run_turn(&mut session, long_prompt(2_500)).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(answer(&session), "done");
        assert_eq!(daemon.refused(), 1, "refused once, then accepted");
        let requests = daemon.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].json()["max_tokens"], 30_500);
        let lowered = requests[1].json()["max_tokens"].as_u64().unwrap();
        assert!(lowered < 30_500 - 1_000, "{lowered}");
    }

    // ── AGE-988: a long run on a known window, its output cap added downstream

    /// A DABstep analyst's run (#4): `ROUNDS` tool rounds, each reading a
    /// data file, then the answer, on vLLM's 32k window with Qwen's
    /// tokenizer counting 5% more than ours. The model has hive's
    /// configured window, and `max_tokens` as given; its last request
    /// carries more than `min_tokens` (the fixture's size).
    const ROUNDS: usize = 10;

    async fn long_analyst_run(daemon: &FakeDaemon, max_tokens: Option<i32>, min_tokens: usize) {
        let files: Vec<String> = (0..ROUNDS).map(|i| format!("part-{i}.csv")).collect();
        let (mut model, provider) = hosted(daemon, max_tokens);
        model.max_context_window = Some(VLLM_WINDOW as i32);
        let (mut session, workspace) = hosted_session(&model, &provider).await;
        for name in &files {
            std::fs::write(workspace.path().join(name), data_file(7_000)).unwrap();
        }
        let events = run_turn(&mut session, TurnInput::text("Total the fees.")).await;
        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        assert_eq!(answer(&session), "done");
        let counter = crate::token_budget::counter::TokenCounter::for_model("fake");
        let last = daemon.requests().last().unwrap().body.clone();
        let tokens = counter.count(&String::from_utf8_lossy(&last));
        assert!(
            tokens > min_tokens,
            "fixture: the last request is {tokens} tokens"
        );
    }

    fn analyst_script() -> Script {
        let mut replies: Vec<Reply> = (0..ROUNDS)
            .map(|i| Reply::ToolCalls(vec![read(&format!("part-{i}.csv"))]))
            .collect();
        replies.push(Reply::text("done"));
        Script::new().route(CHEAP_MODEL, replies)
    }

    /// Criterion 1: with the window known and a model whose max output is
    /// large (32k, the window itself), the cap the guard clamps to counts
    /// the whole request — base, history, prompt — so no request of a long
    /// run is over the window: every `max_tokens` plus the real input fits.
    /// (A cap that large is also the history's reply reserve, so the
    /// history is shaped to some 12k tokens, not 20k.)
    #[tokio::test]
    async fn a_long_run_with_a_large_output_cap_fits_every_request() {
        let daemon = FakeDaemon::scripted(analyst_script())
            .with_window(VLLM_WINDOW)
            .with_tokenizer_gap(1.05);
        long_analyst_run(&daemon, Some(VLLM_WINDOW as i32), 12_000).await;
        assert_eq!(daemon.refused(), 0, "no request was over the window");
        for request in daemon.requests() {
            let sent = request.json()["max_tokens"].as_u64().unwrap();
            assert!(sent < VLLM_WINDOW as u64, "clamped: {sent}");
        }
    }

    /// Criterion 2, run #4 as it happened: chatty sends no `max_tokens`
    /// (none configured), and hive's egress, short of budget, adds one of
    /// 32,186 — the window less nothing. vLLM refuses, naming only a lower
    /// bound on the input. Each such request is sent once more with a cap
    /// counted from the request itself, which fits (the egress keeps the
    /// lower of the two), and the analyst finishes.
    #[tokio::test]
    async fn a_near_window_cap_added_downstream_is_answered_with_one_that_fits() {
        let daemon = FakeDaemon::scripted(analyst_script())
            .with_window(VLLM_WINDOW)
            .with_tokenizer_gap(1.05)
            .with_egress_cap(32_186);
        // The incident's size: about 20k tokens of history.
        long_analyst_run(&daemon, None, 18_000).await;

        let requests = daemon.requests();
        let calls = ROUNDS + 1;
        assert_eq!(daemon.refused(), calls, "each call refused once");
        assert_eq!(requests.len(), 2 * calls, "and each retried once");
        for pair in requests.chunks(2) {
            assert!(
                pair[0].json().get("max_tokens").is_none(),
                "none configured"
            );
            let cap = pair[1].json()["max_tokens"]
                .as_u64()
                .expect("the retry's cap");
            assert!(cap >= 1_024, "{cap}");
        }
    }
}
