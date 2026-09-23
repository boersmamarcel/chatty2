//! Application service layer.
//!
//! Services encapsulate domain logic that doesn't belong in models (data) or
//! repositories (persistence). Use this module for:
//!
//! - **External integrations**: LLM streaming (`llm_service`), MCP connections
//!   (`mcp_service`), A2A protocol (`a2a_client`), search engines (`search_service`).
//! - **Orchestration**: Stream lifecycle (`stream_processor`) and title generation (`title_generator`).
//! - **System operations**: Shell execution (`shell_service`), filesystem access
//!   (`filesystem_service`), path validation (`path_validator`), git operations (`git_service`).
//! - **Rendering**: Math/LaTeX (`math_renderer_service`), Mermaid diagrams
//!   (`mermaid_renderer_service`), chart SVGs (`chart_svg_renderer`), PDF thumbnails
//!   (`pdf_thumbnail`), PPTX slides (`pptx_render`).
//! - **Memory & context**: Agent memory (`memory_service`), skill persistence (`skill_service`).
//!
//! ## When to use services vs tools vs repositories
//!
//! | Layer | Purpose | Example |
//! |-------|---------|---------|
//! | **Service** | Reusable domain logic callable from any crate | `shell_service::execute_command()` |
//! | **Tool** | LLM-callable function with JSON schema | `ShellTool` (wraps `shell_service`) |
//! | **Repository** | Data persistence (load/save to disk) | `ConversationRepository` |

pub mod a2a_client;
pub mod agent_loop_guard;
pub mod agent_task_controller;
#[cfg(feature = "browser")]
pub mod browser;
pub mod chart_svg_renderer;
pub mod context_shaper;
pub mod embedding_service;
pub mod error_collector_layer;
pub mod filesystem_service;
pub mod git_service;
pub mod github_pr_service;
pub mod http_client;
pub mod llm_service;
#[cfg(feature = "math-render")]
pub mod math_renderer_service;
pub mod mcp_service;
pub mod mcp_token_store;
pub mod memory_query;
pub mod memory_service;
#[cfg(feature = "mermaid")]
pub mod mermaid_renderer_service;
pub mod message_helpers;
pub mod path_validator;
#[cfg(feature = "pdf")]
pub mod pdf_thumbnail;
#[cfg(feature = "pdf")]
pub mod pdfium_utils;
#[cfg(feature = "pptx")]
pub mod pptx_render;
pub mod search_service;
pub mod shell_service;
pub mod skill_service;
/// The hosted per-user spend cap `invoke_agent` asks before delegating
/// (AGE-416 / ADR-0010). chatty2 ships the trait; hive implements it.
pub mod spend_gate;
pub mod ssrf_guard;
/// Scripted stream fixtures for the frontends' characterization tests (AGE-191).
/// Test-only: enable `chatty-core/test-support` from a dev-dependency.
#[cfg(any(test, feature = "test-support"))]
pub mod stream_fixtures;
pub mod stream_processor;
/// The team directory: roster, leader role, verification, skill and turn
/// budget in one place, with the presets compiled in (ADR-0011 C13 / AGE-407).
pub mod team;
pub mod title_generator;
/// The model's view of its tool-turn budget and the tool-free wrap-up call
/// that replaces `MaxTurnsError`.
pub mod turn_budget;
#[cfg(feature = "math-render")]
pub mod typst_compiler_service;
/// The broker's named virtual agents — each worker's argv and endpoint —
/// built once for both frontends (ADR-0011 C10 / AGE-377).
pub mod virtual_agents;
/// Which model endpoint a broker worker talks to, and its budget on it
/// (ADR-0011 C6 / AGE-376).
pub mod worker_endpoint;
/// ADR-0012 worker isolation: a `git worktree` per worker (AGE-314 / AGE-301).
pub mod worker_tree;

pub use a2a_client::{A2aClient, A2aStreamEvent};
pub use agent_loop_guard::AgentLoopGuard;
pub use agent_task_controller::{
    AgentTaskController, AgentTaskResponse, AgentTaskSnapshot, AgentTodo, AgentTodoStatus,
    is_agent_todo_tool, is_protocol_follow_up_text, snapshot_from_tool_output,
};
pub use context_shaper::{ContextShaper, ContextShaperSettings, ContextShaperStage, ShapedContext};
pub use embedding_service::EmbeddingService;
pub use error_collector_layer::ErrorCollectorLayer;
pub use llm_service::{StreamChunk, stream_prompt};
#[cfg(feature = "math-render")]
pub use math_renderer_service::MathRendererService;
pub use mcp_service::McpService;
pub use memory_query::simplify_memory_query;
pub use memory_service::MemoryService;
#[cfg(feature = "mermaid")]
pub use mermaid_renderer_service::MermaidRendererService;
pub use message_helpers::{
    call_ids, enforce_tool_round_trips, exchange_count, extract_user_text, extract_user_text_lines,
    gather_mcp_tools, is_persisted_tool_round_trip, is_tool_call_message, is_tool_message,
    is_tool_result_message, result_ids, tool_round_trips_intact,
};
#[cfg(feature = "pdf")]
pub use pdf_thumbnail::cleanup_thumbnails;
pub use skill_service::SkillService;
pub use spend_gate::{CapExceeded, SpendGate};
#[cfg(any(test, feature = "test-support"))]
pub use stream_fixtures::{
    Scenario, ScriptedItem, assert_golden, clarification_scenario, scenarios, scripted_stream,
};
pub use stream_processor::{
    ChunkAction, HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS, HEADLESS_TRANSPORT_RETRY_ATTEMPTS,
    RecoveryAction, STALL_TICK, STALL_TIMEOUT, STALLED_STREAM_MESSAGE, StreamChunkHandler,
    StreamError, StreamErrorKind, StreamSurface, decide_recovery, install_progress_channel,
    run_stream_loop,
};
pub use title_generator::generate_title;
