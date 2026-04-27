mod mcp_helpers;
mod preamble_builder;
mod provider_builder;
mod tool_collector;
mod tool_registry;

use anyhow::Result;
use rig::agent::Agent;

use crate::sandbox::{SandboxConfig, SandboxManager};
use crate::services::filesystem_service::FileSystemService;
use crate::services::git_service::GitService;
use crate::services::memory_service::MemoryService;
use crate::services::search_service::CodeSearchService;
use crate::services::shell_service::ShellSession;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;
#[cfg(feature = "math-render")]
use crate::tools::CompileTypstTool;
use crate::tools::{
    AddAttachmentTool, ApplyDiffTool, BrowserUseTool, CreateChartTool, CreateDirectoryTool,
    DaytonaTool, DeleteFileTool, ExecuteCodeTool, FetchTool, FindDefinitionTool, FindFilesTool,
    GitAddTool, GitCommitTool, GitCreateBranchTool, GitDiffTool, GitLogTool, GitStatusTool,
    GitSwitchBranchTool, GlobSearchTool, InvokeAgentTool, ListAgentsTool, ListDirectoryTool,
    ListMcpTool, ListToolsTool, LocalModuleAgentSummary, MoveFileTool, PendingArtifacts,
    PublishModuleTool, ReadBinaryTool, ReadFileTool, ReadSkillTool, RememberTool, SaveSkillTool,
    SearchCodeTool, SearchMemoryTool, SearchWebTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, SubAgentTool, WriteFileTool,
};
#[cfg(feature = "duckdb")]
use crate::tools::{DescribeDataTool, QueryDataTool};
#[cfg(feature = "excel")]
use crate::tools::{EditExcelTool, ReadExcelTool, WriteExcelTool};
#[cfg(feature = "pdf")]
use crate::tools::{PdfExtractTextTool, PdfInfoTool, PdfToImageTool};

use mcp_helpers::{McpTools, filter_mcp_tool_info};
use preamble_builder::build_preamble;
use tool_collector::*;
use tool_registry::active_native_tool_names;

pub use tool_registry::ToolAvailability;

/// Contextual dependencies for building an agent.
///
/// Groups the many optional services and settings needed by
/// `AgentClient::from_model_config_with_tools()` and `Conversation::new/from_data()`.
pub struct AgentBuildContext {
    pub mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
    pub exec_settings: Option<crate::settings::models::ExecutionSettingsModel>,
    pub pending_approvals: Option<crate::models::execution_approval_store::PendingApprovals>,
    pub pending_write_approvals: Option<crate::models::write_approval_store::PendingWriteApprovals>,
    pub pending_artifacts: Option<PendingArtifacts>,
    pub shell_session: Option<std::sync::Arc<ShellSession>>,
    pub user_secrets: Vec<(String, String)>,
    pub theme_colors: Option<[String; 5]>,
    pub memory_service: Option<MemoryService>,
    pub search_settings: Option<crate::settings::models::search_settings::SearchSettingsModel>,
    pub embedding_service: Option<crate::services::embedding_service::EmbeddingService>,
    pub allow_sub_agent: bool,
    pub module_agents: Vec<LocalModuleAgentSummary>,
    pub gateway_port: Option<u16>,
    pub remote_agents: Vec<crate::settings::models::a2a_store::A2aAgentConfig>,
    pub available_model_ids: Vec<String>,
}

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    /// OpenAI-compatible server (vLLM, llama.cpp) using the Chat Completions API
    OpenAICompletions(Agent<rig::providers::openai::completion::CompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Mistral(Agent<rig::providers::mistral::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
    AzureOpenAI(Agent<rig::providers::azure::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig, ProviderConfig and build context
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        ctx: AgentBuildContext,
    ) -> Result<(
        Self,
        Option<std::sync::Arc<ShellSession>>,
        crate::tools::invoke_agent_tool::InvokeAgentProgressSlot,
    )> {
        // Destructure context for local use
        let AgentBuildContext {
            mcp_tools,
            exec_settings,
            pending_approvals,
            pending_write_approvals,
            pending_artifacts,
            shell_session,
            user_secrets,
            theme_colors,
            memory_service,
            search_settings,
            embedding_service,
            allow_sub_agent,
            module_agents,
            gateway_port,
            remote_agents,
            available_model_ids,
        } = ctx;

        // Extract secret key names before user_secrets is moved into ShellSession.
        let secret_key_names: Vec<String> = user_secrets.iter().map(|(k, _)| k.clone()).collect();

        // Ensure shell session exists when execution is enabled (factory-level fallback).
        let shell_session = if shell_session.is_some() {
            shell_session
        } else {
            exec_settings.as_ref().and_then(|settings| {
                if settings.enabled {
                    tracing::info!(
                        workspace = ?settings.workspace_dir,
                        "Creating shell session in factory (caller did not provide one)"
                    );
                    Some(std::sync::Arc::new(ShellSession::with_secrets(
                        settings.workspace_dir.clone(),
                        settings.timeout_seconds,
                        settings.max_output_bytes,
                        settings.network_isolation,
                        user_secrets,
                    )))
                } else {
                    tracing::info!(
                        enabled = settings.enabled,
                        workspace = ?settings.workspace_dir,
                        "Shell session not created: execution disabled in settings"
                    );
                    None
                }
            })
        };

        let shell_session_out = shell_session.clone();

        // Create shell session tools if execution is enabled and a session is provided
        let shell_tools: Option<ShellTools> =
            if let (Some(session), Some(settings), Some(approvals)) =
                (&shell_session, &exec_settings, &pending_approvals)
            {
                if settings.enabled {
                    tracing::info!("Shell session tools enabled");
                    Some((
                        ShellExecuteTool::new(session.clone(), settings.clone(), approvals.clone()),
                        ShellSetEnvTool::new(session.clone(), settings.clone()),
                        ShellCdTool::new(session.clone(), settings.clone()),
                        ShellStatusTool::new(session.clone()),
                    ))
                } else {
                    tracing::info!("Shell session tools skipped: execution disabled");
                    None
                }
            } else {
                tracing::info!(
                    has_shell_session = shell_session.is_some(),
                    has_exec_settings = exec_settings.is_some(),
                    has_pending_approvals = pending_approvals.is_some(),
                    "Shell session tools skipped: missing required components"
                );
                None
            };

        // Start git service initialization early (spawns subprocesses that take 50-200ms).
        let git_service_handle = if exec_settings
            .as_ref()
            .map(|s| s.git_enabled)
            .unwrap_or(false)
        {
            if let Some(workspace_dir) = exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
            {
                let wd = workspace_dir.clone();
                Some(tokio::spawn(async move { GitService::new(&wd).await }))
            } else {
                None
            }
        } else {
            None
        };

        // Create filesystem tools if a workspace directory is configured
        let mut add_attachment_tool: Option<AddAttachmentTool> = None;
        #[cfg(feature = "pdf")]
        let mut pdf_to_image_tool: Option<PdfToImageTool> = None;
        #[cfg(feature = "pdf")]
        let mut pdf_info_tool: Option<PdfInfoTool> = None;
        #[cfg(feature = "pdf")]
        let mut pdf_extract_text_tool: Option<PdfExtractTextTool> = None;
        let mut search_tools: Option<SearchTools> = None;
        #[cfg(feature = "excel")]
        let mut excel_read_tool: Option<ReadExcelTool> = None;
        #[cfg(feature = "excel")]
        let mut excel_write_tools: Option<ExcelWriteTools> = None;
        #[cfg(feature = "duckdb")]
        let mut data_query_tools: Option<DataQueryTools> = None;
        let (fs_read_tools, fs_write_tools) = match exec_settings
            .as_ref()
            .and_then(|s| s.workspace_dir.as_ref())
        {
            Some(workspace_dir) => match FileSystemService::new(workspace_dir).await {
                Ok(service) => {
                    let service = std::sync::Arc::new(service);

                    // Read tools
                    let read_tools = if exec_settings
                        .as_ref()
                        .map(|s| s.filesystem_read_enabled)
                        .unwrap_or(false)
                    {
                        tracing::info!(workspace = %workspace_dir, "Filesystem read tools enabled");

                        if let Some(ref artifacts) = pending_artifacts {
                            add_attachment_tool =
                                Some(AddAttachmentTool::new(service.clone(), artifacts.clone()));
                            #[cfg(feature = "pdf")]
                            {
                                pdf_to_image_tool =
                                    Some(PdfToImageTool::new(service.clone(), artifacts.clone()));
                            }
                        }

                        #[cfg(feature = "pdf")]
                        {
                            pdf_info_tool = Some(PdfInfoTool::new(service.clone()));
                            pdf_extract_text_tool = Some(PdfExtractTextTool::new(service.clone()));
                        }

                        // Create code search tools alongside filesystem read tools
                        match CodeSearchService::new(workspace_dir) {
                            Ok(search_service) => {
                                let search_service = std::sync::Arc::new(search_service);
                                tracing::info!(workspace = %workspace_dir, "Code search tools enabled");
                                search_tools = Some((
                                    SearchCodeTool::new(search_service.clone()),
                                    FindFilesTool::new(search_service.clone()),
                                    FindDefinitionTool::new(search_service.clone()),
                                ));
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize code search tools");
                            }
                        }

                        Some((
                            ReadFileTool::new(service.clone()),
                            ReadBinaryTool::new(service.clone()),
                            ListDirectoryTool::new(service.clone()),
                            GlobSearchTool::new(service.clone()),
                        ))
                    } else {
                        tracing::info!(workspace = %workspace_dir, "Filesystem read tools disabled");
                        None
                    };

                    // Write tools
                    let write_tools = if exec_settings
                        .as_ref()
                        .map(|s| s.filesystem_write_enabled)
                        .unwrap_or(false)
                    {
                        tracing::info!(workspace = %workspace_dir, "Filesystem write tools enabled");
                        pending_write_approvals.as_ref().map(|approvals| {
                            (
                                WriteFileTool::new(service.clone(), approvals.clone()),
                                CreateDirectoryTool::new(service.clone()),
                                DeleteFileTool::new(service.clone(), approvals.clone()),
                                MoveFileTool::new(service.clone(), approvals.clone()),
                                ApplyDiffTool::new(service.clone(), approvals.clone()),
                            )
                        })
                    } else {
                        tracing::info!(workspace = %workspace_dir, "Filesystem write tools disabled");
                        None
                    };

                    // Excel read tool
                    #[cfg(feature = "excel")]
                    if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Excel read tool enabled");
                        excel_read_tool = Some(ReadExcelTool::new(service.clone()));
                    }

                    // Excel write tools
                    #[cfg(feature = "excel")]
                    if write_tools.is_some() {
                        excel_write_tools = pending_write_approvals.as_ref().map(|approvals| {
                            tracing::info!(workspace = %workspace_dir, "Excel write tools enabled");
                            (
                                WriteExcelTool::new(service.clone(), approvals.clone()),
                                EditExcelTool::new(service.clone(), approvals.clone()),
                            )
                        });
                    }

                    // Data query tools
                    #[cfg(feature = "duckdb")]
                    if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Data query tools enabled");
                        data_query_tools = Some((
                            QueryDataTool::new(service.clone()),
                            DescribeDataTool::new(service.clone()),
                        ));
                    }

                    (read_tools, write_tools)
                }
                Err(e) => {
                    tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize filesystem tools");
                    (None, None)
                }
            },
            None => (None, None),
        };

        // Extract MCP tool metadata so list_tools can report them to the model
        let mcp_tool_info: Vec<(String, String, String)> = mcp_tools
            .as_ref()
            .map(|tools_list| {
                tracing::info!(
                    server_count = tools_list.len(),
                    "Extracting MCP tool metadata for list_tools"
                );
                tools_list
                    .iter()
                    .flat_map(|(server_name, tools, _sink)| {
                        tracing::info!(
                            server = %server_name,
                            tool_count = tools.len(),
                            "Processing MCP tools from server"
                        );
                        let server_name = server_name.clone();
                        tools.iter().map(move |tool| {
                            tracing::debug!(
                                server = %server_name,
                                tool_name = %tool.name,
                                "Adding MCP tool to metadata list"
                            );
                            (
                                server_name.clone(),
                                tool.name.to_string(),
                                tool.description
                                    .as_deref()
                                    .unwrap_or("No description available")
                                    .to_string(),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        tracing::info!(
            mcp_tool_count = mcp_tool_info.len(),
            "Total MCP tools registered with list_tools"
        );

        // Create MCP listing tool (always available; mutation tools removed)
        let mcp_mgmt_tools = McpTools {
            list: Some(ListMcpTool::new(crate::mcp_repository())),
        };

        // Create fetch tool if enabled in settings
        let fetch_tool = if exec_settings
            .as_ref()
            .map(|s| s.fetch_enabled)
            .unwrap_or(true)
        {
            let workspace = exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
                .map(std::path::PathBuf::from);
            tracing::info!(?workspace, "Fetch tool enabled");
            Some(FetchTool::new(workspace))
        } else {
            tracing::info!("Fetch tool disabled by execution settings");
            None
        };

        // Create search web tool
        let search_web_tool: Option<SearchWebTool> = if exec_settings
            .as_ref()
            .map(|s| s.fetch_enabled)
            .unwrap_or(true)
        {
            use crate::settings::models::search_settings::SearchProvider;
            let max_results = search_settings.as_ref().map(|s| s.max_results).unwrap_or(5);
            let api_tool = search_settings.as_ref().and_then(|search_cfg| {
                let api_key = match search_cfg.active_provider {
                    SearchProvider::Tavily => search_cfg.tavily_api_key.clone(),
                    SearchProvider::Brave => search_cfg.brave_api_key.clone(),
                };
                api_key.filter(|k| !k.is_empty()).map(|key| {
                    tracing::info!(provider = %search_cfg.active_provider, "Search web tool enabled with API provider");
                    SearchWebTool::new(search_cfg.active_provider.clone(), key, max_results)
                })
            });
            Some(api_tool.unwrap_or_else(|| {
                tracing::info!(
                    "Search web tool enabled with DuckDuckGo fallback (no API key configured)"
                );
                SearchWebTool::new_fallback(max_results)
            }))
        } else {
            tracing::info!("Search web tool disabled (internet access is off)");
            None
        };

        // Create browser-use tool
        let browser_use_tool: Option<BrowserUseTool> = if exec_settings
            .as_ref()
            .map(|s| s.fetch_enabled)
            .unwrap_or(true)
        {
            search_settings.as_ref().and_then(|s| {
                if s.browser_use_enabled {
                    s.browser_use_api_key
                        .clone()
                        .filter(|k| !k.is_empty())
                        .map(|key| {
                            tracing::info!("browser-use tool enabled");
                            BrowserUseTool::new(key)
                        })
                } else {
                    tracing::info!("browser-use tool disabled (toggle is off)");
                    None
                }
            })
        } else {
            tracing::info!("browser-use tool disabled (internet access is off)");
            None
        };

        // Create Daytona tool
        let daytona_tool: Option<DaytonaTool> = if exec_settings
            .as_ref()
            .map(|s| s.fetch_enabled)
            .unwrap_or(true)
        {
            search_settings.as_ref().and_then(|s| {
                if s.daytona_enabled {
                    s.daytona_api_key
                        .clone()
                        .filter(|k| !k.is_empty())
                        .map(|key| {
                            tracing::info!("Daytona sandbox tool enabled");
                            let workspace =
                                exec_settings.as_ref().and_then(|s| s.workspace_dir.clone());
                            DaytonaTool::new(key, workspace)
                        })
                } else {
                    tracing::info!("Daytona tool disabled (toggle is off)");
                    None
                }
            })
        } else {
            tracing::info!("Daytona tool disabled (internet access is off)");
            None
        };

        // Create git tools from the handle started earlier.
        let git_tools: Option<GitTools> = if let Some(handle) = git_service_handle {
            match handle.await {
                Ok(Ok(service)) => {
                    if let Some(workspace_dir) = exec_settings
                        .as_ref()
                        .and_then(|s| s.workspace_dir.as_ref())
                    {
                        let service = std::sync::Arc::new(service);
                        let approval_mode = exec_settings
                            .as_ref()
                            .map(|s| s.approval_mode.clone())
                            .unwrap_or_default();
                        let approvals = pending_approvals.clone().unwrap_or_else(|| {
                            std::sync::Arc::new(parking_lot::Mutex::new(
                                std::collections::HashMap::new(),
                            ))
                        });

                        tracing::info!(workspace = %workspace_dir, "Git tools enabled");
                        Some((
                            GitStatusTool::new(service.clone()),
                            GitDiffTool::new(service.clone()),
                            GitLogTool::new(service.clone()),
                            GitAddTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitCreateBranchTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitSwitchBranchTool::new(
                                service.clone(),
                                approval_mode.clone(),
                                approvals.clone(),
                            ),
                            GitCommitTool::new(service, approval_mode, approvals),
                        ))
                    } else {
                        tracing::error!("git_service_handle exists but workspace_dir is None");
                        None
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        error = ?e,
                        "Failed to initialize git tools (workspace may not be a git repository)"
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "Git service init task panicked");
                    None
                }
            }
        } else {
            if exec_settings
                .as_ref()
                .map(|s| s.git_enabled)
                .unwrap_or(false)
            {
                tracing::info!("Git tools skipped: no workspace directory configured");
            } else {
                tracing::info!("Git tools disabled by execution settings");
            }
            None
        };

        // Memory tools
        let (remember_tool, save_skill_tool, search_memory_tool): (
            Option<RememberTool>,
            Option<SaveSkillTool>,
            Option<SearchMemoryTool>,
        ) = if let Some(ref mem_svc) = memory_service {
            let has_embeddings = embedding_service.is_some();
            tracing::info!(semantic_search = has_embeddings, "Memory tools enabled");
            (
                Some(RememberTool::new(
                    mem_svc.clone(),
                    embedding_service.clone(),
                )),
                Some(SaveSkillTool::new(
                    mem_svc.clone(),
                    embedding_service.clone(),
                )),
                Some(SearchMemoryTool::new(mem_svc.clone(), embedding_service)),
            )
        } else {
            tracing::info!("Memory tools disabled: no MemoryService provided");
            (None, None, None)
        };

        // read_skill tool — always available
        let read_skill_tool = ReadSkillTool::new(
            exec_settings
                .as_ref()
                .and_then(|s| s.workspace_dir.as_ref())
                .map(|d| std::path::Path::new(d).join(".claude").join("skills")),
        );

        // Chart tool is always available
        let chart_tool: Option<CreateChartTool> = Some(CreateChartTool::new(
            exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
            theme_colors,
        ));

        // Typst compile tool
        #[cfg(feature = "math-render")]
        let typst_tool: Option<CompileTypstTool> = if exec_settings
            .as_ref()
            .map(|s| s.filesystem_write_enabled)
            .unwrap_or(false)
        {
            tracing::info!("Typst compile tool enabled");
            Some(CompileTypstTool::new(
                exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
            ))
        } else {
            None
        };

        // Docker code execution tool
        let execute_code_tool: Option<ExecuteCodeTool> = if exec_settings
            .as_ref()
            .map(|s| s.docker_code_execution_enabled)
            .unwrap_or(false)
        {
            tracing::info!("Docker code execution tool enabled");
            let sandbox_config = SandboxConfig {
                timeout_secs: exec_settings
                    .as_ref()
                    .map(|s| s.timeout_seconds as u64)
                    .unwrap_or(30),
                network: !exec_settings
                    .as_ref()
                    .map(|s| s.network_isolation)
                    .unwrap_or(true),
                workspace_path: exec_settings.as_ref().and_then(|s| s.workspace_dir.clone()),
                docker_host: exec_settings.as_ref().and_then(|s| s.docker_host.clone()),
                ..SandboxConfig::default()
            };
            let manager = std::sync::Arc::new(SandboxManager::new(sandbox_config));
            Some(ExecuteCodeTool::new(manager))
        } else {
            tracing::info!("Docker code execution tool disabled by execution settings");
            None
        };

        // Sub-agent tool
        let sub_agent_tool: Option<SubAgentTool> =
            if allow_sub_agent && exec_settings.as_ref().map(|s| s.enabled).unwrap_or(false) {
                let sub_model_id = model_config.id.clone();
                let sub_auto_approve = exec_settings
                    .as_ref()
                    .map(|s| {
                        matches!(
                        s.approval_mode,
                        crate::settings::models::execution_settings::ApprovalMode::AutoApproveAll
                    )
                    })
                    .unwrap_or(false);
                tracing::debug!("Sub-agent tool enabled");
                Some(SubAgentTool::new(
                    sub_model_id,
                    sub_auto_approve,
                    available_model_ids,
                ))
            } else {
                if !allow_sub_agent {
                    tracing::debug!("Sub-agent tool disabled: running as a sub-agent");
                } else {
                    tracing::debug!("Sub-agent tool disabled: execution not enabled");
                }
                None
            };

        let tool_availability = ToolAvailability {
            fs_read: fs_read_tools.is_some(),
            fs_write: fs_write_tools.is_some(),
            list_mcp: mcp_mgmt_tools.is_enabled(),
            fetch: fetch_tool.is_some(),
            shell: shell_tools.is_some(),
            git: git_tools.is_some(),
            search: search_tools.is_some(),
            add_attachment: add_attachment_tool.is_some(),
            excel_read: {
                #[cfg(feature = "excel")]
                {
                    excel_read_tool.is_some()
                }
                #[cfg(not(feature = "excel"))]
                {
                    false
                }
            },
            excel_write: {
                #[cfg(feature = "excel")]
                {
                    excel_write_tools.is_some()
                }
                #[cfg(not(feature = "excel"))]
                {
                    false
                }
            },
            pdf_to_image: {
                #[cfg(feature = "pdf")]
                {
                    pdf_to_image_tool.is_some()
                }
                #[cfg(not(feature = "pdf"))]
                {
                    false
                }
            },
            pdf_info: {
                #[cfg(feature = "pdf")]
                {
                    pdf_info_tool.is_some()
                }
                #[cfg(not(feature = "pdf"))]
                {
                    false
                }
            },
            pdf_extract_text: {
                #[cfg(feature = "pdf")]
                {
                    pdf_extract_text_tool.is_some()
                }
                #[cfg(not(feature = "pdf"))]
                {
                    false
                }
            },
            data_query: {
                #[cfg(feature = "duckdb")]
                {
                    data_query_tools.is_some()
                }
                #[cfg(not(feature = "duckdb"))]
                {
                    false
                }
            },
            compile_typst: {
                #[cfg(feature = "math-render")]
                {
                    typst_tool.is_some()
                }
                #[cfg(not(feature = "math-render"))]
                {
                    false
                }
            },
            execute_code: execute_code_tool.is_some(),
            memory: remember_tool.is_some(),
            search_web: search_web_tool.is_some(),
            sub_agent: sub_agent_tool.is_some(),
            browser_use: browser_use_tool.is_some(),
            daytona: daytona_tool.is_some(),
            publish_module: false, // set below after publish_module_tool is created
        };

        let native_tool_names = active_native_tool_names(&tool_availability);
        let mcp_tool_info = filter_mcp_tool_info(mcp_tool_info, &native_tool_names);

        // Create list_tools tool (always available)
        let list_tools = ListToolsTool::new_with_config(&tool_availability, mcp_tool_info.clone());

        // Create list_agents tool (always available)
        let list_agents_tool =
            ListAgentsTool::new_with_modules(remote_agents.clone(), module_agents.clone());

        // Create invoke_agent tool (always available)
        let invoke_agent_tool = InvokeAgentTool::new(remote_agents, module_agents, gateway_port);
        let invoke_agent_progress_slot = invoke_agent_tool.progress_slot();

        // Publish module tool (if an MCP server exposes `publish_module`)
        let publish_module_tool: Option<PublishModuleTool> = mcp_tools.as_ref().and_then(|servers| {
            for (_name, tools, sink) in servers {
                if tools.iter().any(|t| &*t.name == "publish_module") {
                    let ws = exec_settings
                        .as_ref()
                        .and_then(|s| s.workspace_dir.clone());
                    tracing::info!(
                        server = %_name,
                        "Creating publish_wasm_module composite tool (backed by MCP publish_module)"
                    );
                    return Some(PublishModuleTool::new(sink.clone(), ws));
                }
            }
            None
        });

        // Update publish_module availability now that we know
        let tool_availability = ToolAvailability {
            publish_module: publish_module_tool.is_some(),
            ..tool_availability
        };

        // Build the augmented preamble
        let preamble = build_preamble(
            &model_config.preamble,
            &model_config.provider_type,
            &tool_availability,
            &search_settings,
            &mcp_mgmt_tools,
            &mcp_tool_info,
            &secret_key_names,
        );

        // Build native tools once (all providers use the same set)
        let tool_vec = native_tools!(
            list_tools: list_tools,
            fs_read: fs_read_tools,
            fs_write: fs_write_tools,
            add_attachment: add_attachment_tool,
            pdf_to_image: pdf_to_image_tool,
            pdf_info: pdf_info_tool,
            pdf_extract_text: pdf_extract_text_tool,
            mcp_mgmt: mcp_mgmt_tools,
            fetch_tool: fetch_tool,
            shell_tools: shell_tools,
            git_tools: git_tools,
            search_tools: search_tools,
            excel_read: excel_read_tool,
            excel_write: excel_write_tools,
            data_query: data_query_tools,
            chart_tool: chart_tool,
            typst_tool: typst_tool,
            execute_code_tool: execute_code_tool,
            remember_tool: remember_tool,
            save_skill_tool: save_skill_tool,
            search_memory_tool: search_memory_tool,
            read_skill_tool: read_skill_tool,
            search_web_tool: search_web_tool,
            sub_agent_tool: sub_agent_tool,
            browser_use_tool: browser_use_tool,
            daytona_tool: daytona_tool,
            list_agents_tool: list_agents_tool,
            invoke_agent_tool: invoke_agent_tool,
            publish_module_tool: publish_module_tool,
        )
        .into_tool_vec();

        let agent = provider_builder::build_provider_agent(
            model_config,
            provider_config,
            &preamble,
            tool_vec,
            mcp_tools,
            &native_tool_names,
        )
        .await?;

        Ok((agent, shell_session_out, invoke_agent_progress_slot))
    }

    /// Returns the provider name for logging/debugging.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &'static str {
        match self {
            AgentClient::Anthropic(_) => "Anthropic",
            AgentClient::OpenAI(_) => "OpenAI",
            AgentClient::OpenAICompletions(_) => "OpenAI (Completions)",
            AgentClient::Gemini(_) => "Gemini",
            AgentClient::Ollama(_) => "Ollama",
            AgentClient::Mistral(_) => "Mistral",
            AgentClient::AzureOpenAI(_) => "Azure OpenAI",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mcp_helpers::filter_mcp_tool_info;
    use super::tool_registry::{ToolAvailability, active_native_tool_names};

    #[test]
    fn active_native_tool_names_always_includes_read_skill() {
        let names = active_native_tool_names(&ToolAvailability::default());
        assert!(
            names.contains("read_skill"),
            "read_skill must always be reserved to prevent MCP conflicts"
        );
        assert!(names.contains("list_tools"));
        assert!(names.contains("list_agents"));
    }

    #[test]
    fn active_native_tool_names_includes_search_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            search: true,
            ..Default::default()
        });

        assert!(names.contains("list_tools"));
        assert!(names.contains("search_code"));
        assert!(names.contains("find_files"));
        assert!(names.contains("find_definition"));
    }

    #[test]
    fn filter_mcp_tool_info_skips_native_and_mcp_duplicates() {
        let reserved = active_native_tool_names(&ToolAvailability {
            search: true,
            ..Default::default()
        });

        let filtered = filter_mcp_tool_info(
            vec![
                (
                    "server-a".to_string(),
                    "search_code".to_string(),
                    "Conflicts with native tool".to_string(),
                ),
                (
                    "server-a".to_string(),
                    "custom_lookup".to_string(),
                    "Unique MCP tool".to_string(),
                ),
                (
                    "server-b".to_string(),
                    "custom_lookup".to_string(),
                    "Duplicate MCP tool".to_string(),
                ),
            ],
            &reserved,
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "server-a");
        assert_eq!(filtered[0].1, "custom_lookup");
    }
}
