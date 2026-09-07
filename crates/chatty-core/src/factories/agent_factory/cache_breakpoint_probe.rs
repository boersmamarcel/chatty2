//! Does Anthropic-behind-OpenRouter hash `cache_control` markers into the
//! cached prefix, or strip them before matching? (AGE-291)
//!
//! The moving breakpoint (`prompt_cache_http.rs`, AGE-205) rewrites the
//! previously-last message every turn: it carried `cache_control` in turn N
//! and does not in turn N+1. `session::append_only_prefix` pins that on the
//! recorded bytes. If the provider strips the marker before matching, the
//! prefix written in turn N is still hit in turn N+1 and the rewrite is free.
//! If it hashes the marker, every turn invalidates the cache from the
//! previous turn's last message onward and the moving breakpoint buys only
//! the system block, which rig's own breakpoint already covered.
//!
//! This is a measurement against the real provider, not a unit test, so it
//! is ignored by default and needs a key:
//!
//! ```text
//! OPENROUTER_API_KEY=… cargo test -p chatty-core cache_breakpoint_probe \
//!     -- --ignored --nocapture
//! ```
//!
//! `AGE_291_MODEL` picks the model (default `anthropic/claude-haiku-4.5`).
//! Two arms, each a fresh conversation with its own nonce so they cannot hit
//! each other's cache: the production client with the moving breakpoint, and
//! a control built on the plain `reqwest` client, whose only breakpoint is
//! rig's on the system message. Each arm runs two turns and reports the
//! per-request usage the `LLM completion call usage` log line carries
//! (AGE-207). The number that answers the question is turn 2's
//! `cache_read` in the moving arm: at the control's level it covers the
//! system block alone (marker hashed); above it by about turn 1's user
//! message it covers the prefix turn 1 wrote (marker stripped).

use futures::StreamExt;
use rig_agent::agent::AgentBuilder;
use rig_core::client::CompletionClient;
use rig_core::completion::Message;
use rig_core::message::UserContent;

use super::AgentClient;
use super::prompt_cache_http::PromptCachingHttpClient;
use crate::models::token_usage::{ApiCallUsage, format_hit_rate};
use crate::services::AgentTaskController;
use crate::services::http_client::llm_client;
use crate::services::llm_service::{StreamChunk, stream_prompt};
use crate::settings::models::providers_store::ProviderType;

const KEY_ENV: &str = "OPENROUTER_API_KEY";
const MODEL_ENV: &str = "AGE_291_MODEL";
const DEFAULT_MODEL: &str = "anthropic/claude-haiku-4.5";

/// Anthropic caches nothing below a per-model minimum (4096 tokens on Haiku
/// 4.5, 1024 on the larger models). Both the system block and turn 1's user
/// message sit well above it, so each is a cacheable prefix on its own.
const FILLER_WORDS: usize = 6_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// The production client: rig's system breakpoint plus the moving one.
    MovingBreakpoint,
    /// Plain `reqwest` under rig: the system breakpoint only.
    SystemBreakpointOnly,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::MovingBreakpoint => "moving breakpoint",
            Arm::SystemBreakpointOnly => "system breakpoint only",
        }
    }
}

/// Deterministic prose of about `words` words. Prose rather than a repeated
/// token so the provider's tokenizer counts it like a real prompt.
fn filler(words: usize, seed: u64) -> String {
    const POOL: &str = "the lease restores a warm snapshot and rehydrates from the store \
                        before first token while every turn is a fresh process with no cache";
    let pool: Vec<&str> = POOL.split_whitespace().collect();
    let mut state = seed | 1;
    let mut out = String::with_capacity(words * 7);
    for index in 0..words {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let word = pool[(state >> 33) as usize % pool.len()];
        if index > 0 {
            out.push(if index % 17 == 0 { '\n' } else { ' ' });
        }
        out.push_str(word);
    }
    out
}

fn build_arm(arm: Arm, key: &str, model: &str, preamble: &str) -> AgentClient {
    let agent = match arm {
        Arm::MovingBreakpoint => {
            let client = rig_core::providers::openrouter::Client::builder()
                .api_key(key)
                .http_client(PromptCachingHttpClient::new(llm_client().clone()))
                .build()
                .expect("the OpenRouter client builds");
            let model = client.completion_model(model).with_prompt_caching();
            AgentBuilder::new(model).preamble(preamble).build()
        }
        Arm::SystemBreakpointOnly => {
            let client = rig_core::providers::openrouter::Client::builder()
                .api_key(key)
                .http_client(llm_client().clone())
                .build()
                .expect("the OpenRouter client builds");
            let model = client.completion_model(model).with_prompt_caching();
            AgentBuilder::new(model).preamble(preamble).build()
        }
    };
    AgentClient {
        agent: agent.clone(),
        task_controller: AgentTaskController::new(),
        provider: ProviderType::OpenRouter,
        utility: agent,
    }
}

/// One turn through the production stream path; the reply text and one
/// usage record per provider request.
async fn run_turn(
    client: &AgentClient,
    history: Vec<Message>,
    text: String,
) -> (String, Vec<ApiCallUsage>) {
    let mut stream = stream_prompt(
        client,
        history,
        vec![UserContent::text(text)],
        None,
        None,
        None,
        1,
    )
    .await
    .expect("the stream opens");

    let mut reply = String::new();
    let mut calls = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk.expect("a stream item") {
            StreamChunk::Text(text) => reply.push_str(&text),
            StreamChunk::ApiCallUsage(usage) => calls.push(usage),
            StreamChunk::Error(error) => panic!("provider error: {error:?}"),
            StreamChunk::Done => break,
            _ => {}
        }
    }
    (reply, calls)
}

struct Measurement {
    arm: Arm,
    turn: usize,
    usage: ApiCallUsage,
}

fn print_row(measurement: &Measurement) {
    let usage = &measurement.usage;
    eprintln!(
        "{:<24}{:<6}{:>8}{:>12}{:>13}{:>10}",
        measurement.arm.label(),
        measurement.turn,
        usage.input_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        usage
            .cache_hit_rate()
            .map(format_hit_rate)
            .unwrap_or_else(|| "-".to_string()),
    );
}

/// Two turns per arm against the real provider. Prints the table AGE-291
/// asks for; the verdict line reads it the way the module docs describe.
#[tokio::test]
#[ignore = "measures the real provider; needs OPENROUTER_API_KEY (AGE-291)"]
async fn measure_openrouter_moving_breakpoint_cache_hit_rate() {
    let key = std::env::var(KEY_ENV)
        .unwrap_or_else(|_| panic!("{KEY_ENV} is not set; this probe talks to OpenRouter"));
    let model = std::env::var(MODEL_ENV).unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let run = uuid::Uuid::new_v4();

    let mut rows = Vec::new();
    for (index, arm) in [Arm::MovingBreakpoint, Arm::SystemBreakpointOnly]
        .into_iter()
        .enumerate()
    {
        // The nonce leads every prefix, so neither arm can hit a block the
        // other wrote, or one a previous run wrote.
        let preamble = format!(
            "Probe run {run}, arm {index}. Answer with one word.\n\n{}",
            filler(FILLER_WORDS, 0x2910 + index as u64)
        );
        let turn_1 = format!(
            "Run {run}, arm {index}, turn 1. Read this and reply \"ok\".\n\n{}",
            filler(FILLER_WORDS, 0x2911 + index as u64)
        );
        let client = build_arm(arm, &key, &model, &preamble);

        let (reply, calls) = run_turn(&client, Vec::new(), turn_1.clone()).await;
        assert!(!calls.is_empty(), "turn 1 reported no usage");
        rows.extend(calls.into_iter().map(|usage| Measurement {
            arm,
            turn: 1,
            usage,
        }));

        let history = vec![Message::user(turn_1), Message::assistant(reply)];
        let (_, calls) = run_turn(&client, history, "Turn 2. Reply \"ok\" again.".into()).await;
        assert!(!calls.is_empty(), "turn 2 reported no usage");
        rows.extend(calls.into_iter().map(|usage| Measurement {
            arm,
            turn: 2,
            usage,
        }));
    }

    eprintln!("\nAGE-291 cache breakpoint probe — model {model}, run {run}");
    eprintln!("system block and turn-1 user message: {FILLER_WORDS} words of filler each\n");
    eprintln!(
        "{:<24}{:<6}{:>8}{:>12}{:>13}{:>10}",
        "arm", "turn", "input", "cache_read", "cache_write", "hit_rate"
    );
    for row in &rows {
        print_row(row);
    }

    // Turn 2's first request in each arm is the comparison the issue asks
    // for. The control's cache read is the system block; the moving arm's
    // exceeds it by turn 1's user message only if the marker is not content.
    let turn_2_read = |arm: Arm| {
        rows.iter()
            .find(|row| row.arm == arm && row.turn == 2)
            .map(|row| row.usage.cache_read_tokens)
            .expect("turn 2 reported usage")
    };
    let moving = turn_2_read(Arm::MovingBreakpoint);
    let control = turn_2_read(Arm::SystemBreakpointOnly);
    let turn_1_prompt = rows
        .iter()
        .find(|row| row.arm == Arm::MovingBreakpoint && row.turn == 1)
        .map(|row| row.usage.prompt_tokens())
        .expect("turn 1 reported usage");
    // The system block is roughly half of turn 1's prompt; the user message
    // is the other half.
    let user_message_estimate = turn_1_prompt / 2;
    let verdict = if moving >= control.saturating_add(user_message_estimate / 2) {
        "STRIPPED: turn 2 re-read the prefix turn 1 wrote past the moved marker; \
         the moving breakpoint is free and ADR-0010 §2b is about content"
    } else {
        "HASHED: turn 2 re-read only the system block; the moved marker \
         invalidated turn 1's prefix and the breakpoint design needs a follow-up"
    };
    eprintln!(
        "\nturn-2 cache_read: moving {moving}, control {control}, \
         turn-1 user message ≈ {user_message_estimate} tokens\n{verdict}\n"
    );
}
