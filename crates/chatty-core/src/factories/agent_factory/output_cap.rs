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
        use rig_agent::completion::Prompt;

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
        let _ = crate::init_repositories();
        let daemon = FakeDaemon::scripted(Script::new().route(CHEAP_MODEL, replies));
        // The cap a hosted egress would set near the end of a budget.
        let (model, provider) = hosted(&daemon, Some(8_192));
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
                &model,
                &provider,
                built.context,
            )
            .await
            .expect("the conversation builds");
        let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
        let sink = events.clone();
        session
            .begin_turn(TurnInput::text("Write the report."), move |event| {
                sink.borrow_mut().push(event)
            })
            .expect("the turn starts")
            .await;
        let events = Rc::try_unwrap(events).unwrap().into_inner();
        for event in &events {
            session.apply(event);
        }
        let bodies = daemon
            .requests()
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();
        (events, bodies, session)
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
    /// two calls, then the turn ends with an empty-completion error, which
    /// even headless stops on: no loop of truncated answers.
    #[tokio::test]
    async fn a_capped_turn_with_no_answer_stops_without_a_loop() {
        let (events, bodies, mut session) =
            capped_turn(vec![Reply::Truncated(String::new()); 6]).await;
        assert_eq!(bodies.len(), 2, "one call, one retry, no more");
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
}
