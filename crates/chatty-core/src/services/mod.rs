//! Application service layer.
//!
//! Services encapsulate domain logic that doesn't belong in models (data) or
//! repositories (persistence). Use this module for:
//!
//! - **External integrations**: LLM streaming (`llm_service`), MCP connections
//!   (`mcp_service`), A2A protocol (`a2a_client`), search engines (`search_service`).
//! - **Orchestration**: Multi-step flows like message augmentation (`message_orchestrator`),
//!   stream lifecycle (`stream_processor`), and title generation (`title_generator`).
//! - **System operations**: Shell execution (`shell_service`), filesystem access
//!   (`filesystem_service`), path validation (`path_validator`), git operations (`git_service`).
//! - **Rendering**: Math/LaTeX (`math_renderer_service`), Mermaid diagrams
//!   (`mermaid_renderer_service`), chart SVGs (`chart_svg_renderer`), PDF thumbnails
//!   (`pdf_thumbnail`).
//! - **Memory & context**: Agent memory (`memory_service`), auto-context enrichment
//!   (`auto_context`), skill persistence (`skill_service`).
//!
//! ## When to use services vs tools vs repositories
//!
//! | Layer | Purpose | Example |
//! |-------|---------|---------|
//! | **Service** | Reusable domain logic callable from any crate | `shell_service::execute_command()` |
//! | **Tool** | LLM-callable function with JSON schema | `ShellTool` (wraps `shell_service`) |
//! | **Repository** | Data persistence (load/save to disk) | `ConversationRepository` |

pub mod a2a_client;
pub mod auto_context;
pub mod chart_svg_renderer;
pub mod embedding_service;
pub mod error_collector_layer;
pub mod filesystem_service;
pub mod git_service;
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
pub mod message_orchestrator;
pub mod path_validator;
#[cfg(feature = "pdf")]
pub mod pdf_thumbnail;
#[cfg(feature = "pdf")]
pub mod pdfium_utils;
pub mod search_service;
pub mod shell_service;
pub mod skill_service;
pub mod stream_processor;
pub mod title_generator;
#[cfg(feature = "math-render")]
pub mod typst_compiler_service;

pub use a2a_client::{A2aClient, A2aStreamEvent};
pub use auto_context::{AutoContextRequest, load_auto_context_block};
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
pub use message_orchestrator::{augment_with_memory, extract_user_text, gather_mcp_tools};
#[cfg(feature = "pdf")]
pub use pdf_thumbnail::cleanup_thumbnails;
pub use skill_service::SkillService;
pub use stream_processor::{
    ChunkAction, StreamChunkHandler, install_progress_channel, run_stream_loop,
};
pub use title_generator::generate_title;
