//! The serialized tool block must be byte-identical across processes (AGE-276).
//!
//! ADR-0010 leases a fresh microVM — and therefore a fresh process — per turn.
//! Any hash-seeded ordering that reaches the provider request makes the prompt
//! prefix differ between turns, which pins the provider cache hit rate at zero
//! for every hosted conversation. The tool block is the largest hash-ordering
//! risk in the prompt, so it is checked directly, across two real processes.
//!
//! The block compared here is the one the request carries: `agent.tool_definitions()`
//! resolves through the same `ToolServerHandle` that
//! `provider_builder.rs`'s agent uses to fill `CompletionRequest::tools`.
//!
//! MCP tools are deliberately excluded — their ordering is AGE-206.

use std::process::Command;

use super::{AgentBuildContext, AgentClient, AgentServices};
use crate::models::clarification_store::ClarificationStore;
use crate::models::execution_approval_store::ExecutionApprovalStore;
use crate::models::write_approval_store::WriteApprovalStore;
use crate::settings::models::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// Set on the child so it dumps the block instead of silently passing. Run the
/// dump by hand with:
///
/// ```text
/// CHATTY_DUMP_TOOL_BLOCK=1 cargo test -p chatty-core \
///     dump_tool_block_for_parent -- --ignored --nocapture
/// ```
const DUMP_ENV: &str = "CHATTY_DUMP_TOOL_BLOCK";

/// Fully qualified name of the child entry point, as libtest filters it.
const DUMP_TEST: &str =
    "factories::agent_factory::tool_block_determinism::dump_tool_block_for_parent";

const BEGIN: &str = "CHATTY_TOOL_BLOCK_BEGIN";
const END: &str = "CHATTY_TOOL_BLOCK_END";

/// A workspace path that is a pure function of the environment, so both child
/// processes build the same workspace-dependent tools. Empty on purpose: the
/// tools are built from the directory, never from its contents.
fn fixture_workspace() -> String {
    let dir = std::env::temp_dir().join("chatty-age276-tool-block-fixture");
    std::fs::create_dir_all(&dir).expect("fixture workspace is creatable");
    dir.canonicalize()
        .unwrap_or(dir)
        .to_string_lossy()
        .into_owned()
}

/// The one fixed `ExecutionSettingsModel` both processes build against.
///
/// Everything that needs no external process is on, so the block covers the
/// filesystem, document, chart and search tool schemas rather than only the
/// always-on handful. Git, code execution and the browser stay off: they spawn
/// subprocesses or download a Chrome build, and neither adds a schema shape the
/// others do not already cover.
fn fixture_execution_settings() -> ExecutionSettingsModel {
    ExecutionSettingsModel {
        workspace_dir: Some(fixture_workspace()),
        ..ExecutionSettingsModel::default()
    }
}

fn fixture_model_config() -> ModelConfig {
    ModelConfig::new(
        "age-276".to_string(),
        "Determinism Fixture".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    )
}

/// Ollama, because its client is built without network access or credentials.
/// The tool block is provider-independent: only MCP tool schemas are sanitized
/// per provider, and this fixture has no MCP servers.
fn fixture_provider_config() -> ProviderConfig {
    ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
}

fn fixture_build_context() -> AgentBuildContext {
    AgentBuildContext {
        pending_approvals: Some(ExecutionApprovalStore::new().get_pending_approvals()),
        pending_clarifications: Some(ClarificationStore::new().get_pending_clarifications()),
        pending_write_approvals: Some(WriteApprovalStore::new().get_pending_approvals()),
        ..AgentBuildContext::from_services(AgentServices {
            exec_settings: Some(fixture_execution_settings()),
            ..AgentServices::default()
        })
    }
}

/// Build the fixture agent and serialize exactly the definitions the provider
/// request would carry.
async fn serialized_tool_block() -> String {
    // Agent construction resolves the MCP repository for the always-on
    // `list_mcp` tool; this only resolves paths and is a no-op after the first
    // call.
    let _ = crate::init_repositories();

    let built = AgentClient::from_model_config_with_tools(
        &fixture_model_config(),
        &fixture_provider_config(),
        fixture_build_context(),
    )
    .await
    .expect("fixture agent builds without network access");

    let definitions = built
        .client
        .agent
        .tool_definitions(None)
        .await
        .expect("tool definitions resolve");

    serde_json::to_string(&definitions).expect("tool definitions serialize")
}

/// Run this test binary again, in a new process, and return the block it dumps.
fn tool_block_from_child_process() -> Vec<u8> {
    let exe = std::env::current_exe().expect("test binary path is known");
    let output = Command::new(&exe)
        .args(["--exact", DUMP_TEST, "--ignored", "--nocapture"])
        .env(DUMP_ENV, "1")
        .output()
        .unwrap_or_else(|e| panic!("spawning {} failed: {e}", exe.display()));

    assert!(
        output.status.success(),
        "child process failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    extract_block(&output.stdout)
}

/// Take the single line the child printed between the markers.
fn extract_block(stdout: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines().skip_while(|line| line.trim() != BEGIN);
    lines.next().expect("child printed the begin marker");
    let block = lines
        .next()
        .expect("child printed a block after the marker");
    assert_ne!(block.trim(), END, "child printed an empty block");
    block.as_bytes().to_vec()
}

/// Byte offset of the first difference, or `None` when the two are equal.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    if let Some(offset) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
        return Some(offset);
    }
    // Equal up to the shorter one: the difference is where it ran out.
    (a.len() != b.len()).then_some(a.len().min(b.len()))
}

/// 80 bytes of `bytes` from `offset`, as lossy text.
fn context_at(bytes: &[u8], offset: usize) -> String {
    let end = (offset + 80).min(bytes.len());
    String::from_utf8_lossy(&bytes[offset.min(bytes.len())..end]).into_owned()
}

/// The check ADR-0010 §2b binds the hosted implementation to.
#[test]
fn tool_block_is_byte_identical_across_processes() {
    let first = tool_block_from_child_process();
    let second = tool_block_from_child_process();

    if let Some(offset) = first_difference(&first, &second) {
        panic!(
            "tool block differs between processes at byte {offset} \
             (lengths {} and {})\n  first:  {}\n  second: {}",
            first.len(),
            second.len(),
            context_at(&first, offset),
            context_at(&second, offset),
        );
    }

    assert!(
        !first.is_empty(),
        "the fixture agent produced an empty tool block"
    );
}

/// Child entry point. Ignored so a normal run does not build a second agent
/// for nothing; the parent above invokes it explicitly.
#[tokio::test]
#[ignore = "child process entry point for tool_block_is_byte_identical_across_processes"]
async fn dump_tool_block_for_parent() {
    if std::env::var(DUMP_ENV).is_err() {
        return;
    }
    let block = serialized_tool_block().await;
    println!("{BEGIN}");
    println!("{block}");
    println!("{END}");
}

/// Mutation check: proves the test above can actually fail (AGE-276 step 2).
///
/// The one-off version of this check inserted an extra tool into the block
/// through a `HashMap` iteration and ran the parent test twice; the two blocks
/// differed, so the assertion is live rather than vacuously true. That
/// injection helper has been removed — it had no non-test caller — and what is
/// kept here is the property it depended on: `HashMap` iteration order over the
/// fixture's own tool names is seeded per process, so a hash-ordered block
/// *would* be caught by the cross-process comparison above.
///
/// Ignored because it spawns 20 processes. Run it by hand with
/// `cargo test -p chatty-core hash_order -- --ignored --nocapture`.
#[test]
#[ignore = "mutation check: spawns 20 processes to observe per-process hash seeding"]
fn hash_order_over_tool_names_differs_across_processes() {
    let exe = std::env::current_exe().expect("test binary path is known");
    let baseline = hash_order_from_child(&exe);

    let differed = (0..20).any(|_| hash_order_from_child(&exe) != baseline);

    assert!(
        differed,
        "20 processes all produced the same HashMap order ({baseline}), \
         so the cross-process comparison could not detect hash-seeded ordering"
    );
}

fn hash_order_from_child(exe: &std::path::Path) -> String {
    let output = Command::new(exe)
        .args(["--exact", HASH_ORDER_TEST, "--ignored", "--nocapture"])
        .env(DUMP_ENV, "1")
        .output()
        .expect("spawning the hash-order child succeeds");
    String::from_utf8_lossy(&extract_block(&output.stdout)).into_owned()
}

const HASH_ORDER_TEST: &str =
    "factories::agent_factory::tool_block_determinism::dump_hash_order_for_parent";

/// Child entry point for the mutation check.
#[test]
#[ignore = "child process entry point for hash_order_over_tool_names_differs_across_processes"]
fn dump_hash_order_for_parent() {
    if std::env::var(DUMP_ENV).is_err() {
        return;
    }
    let names: std::collections::HashMap<&str, ()> = [
        "list_tools",
        "write_todos",
        "update_todo",
        "verify_completion",
        "list_agents",
        "invoke_agent",
        "read_file",
        "write_file",
        "fetch",
        "search_web",
    ]
    .into_iter()
    .map(|name| (name, ()))
    .collect();

    println!("{BEGIN}");
    println!("{}", names.keys().copied().collect::<Vec<_>>().join(","));
    println!("{END}");
}
