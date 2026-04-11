mod mcp_helpers;
mod preamble_builder;
mod tool_collector;
mod tool_registry;

use anyhow::{Context, Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;
use std::sync::OnceLock;

use crate::auth::{AzureTokenCache, azure_auth};
use crate::sandbox::{SandboxConfig, SandboxManager};
use crate::services::filesystem_service::FileSystemService;
use crate::services::git_service::GitService;
use crate::services::memory_service::MemoryService;
use crate::services::search_service::CodeSearchService;
use crate::services::shell_service::ShellSession;
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig};
use crate::settings::models::providers_store::{AzureAuthMethod, ProviderConfig, ProviderType};
use crate::tools::{
    AddAttachmentTool, AddMcpTool, ApplyDiffTool, BrowserUseTool, CompileTypstTool,
    CreateChartTool, CreateDirectoryTool, DaytonaTool, DeleteFileTool, DeleteMcpTool,
    DescribeDataTool, EditExcelTool, EditMcpTool, ExecuteCodeTool, FetchTool, FindDefinitionTool,
    FindFilesTool, GitAddTool, GitCommitTool, GitCreateBranchTool, GitDiffTool, GitLogTool,
    GitStatusTool, GitSwitchBranchTool, GlobSearchTool, InvokeAgentTool, ListAgentsTool,
    ListDirectoryTool, ListMcpTool, ListToolsTool, LocalModuleAgentSummary, MoveFileTool,
    PdfExtractTextTool, PdfInfoTool, PdfToImageTool, PendingArtifacts, PublishModuleTool,
    QueryDataTool, ReadBinaryTool, ReadExcelTool, ReadFileTool, ReadSkillTool, RememberTool,
    SaveSkillTool, SearchCodeTool, SearchMemoryTool, SearchWebTool, ShellCdTool, ShellExecuteTool,
    ShellSetEnvTool, ShellStatusTool, SubAgentTool, WriteExcelTool, WriteFileTool,
};

use mcp_helpers::{
    McpTools, build_with_mcp_tools, filter_mcp_tool_info, sanitize_mcp_tools_for_openai,
};
use preamble_builder::build_preamble;
use tool_collector::*;
use tool_registry::active_native_tool_names;

pub use tool_registry::ToolAvailability;

static AZURE_TOKEN_CACHE: OnceLock<Option<AzureTokenCache>> = OnceLock::new();

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
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

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
        let mut pdf_to_image_tool: Option<PdfToImageTool> = None;
        let mut pdf_info_tool: Option<PdfInfoTool> = None;
        let mut pdf_extract_text_tool: Option<PdfExtractTextTool> = None;
        let mut search_tools: Option<SearchTools> = None;
        #[allow(clippy::type_complexity)]
        let (fs_read_tools, fs_write_tools, excel_read_tool, excel_write_tools, data_query_tools): (
            Option<FsReadTools>,
            Option<FsWriteTools>,
            Option<ReadExcelTool>,
            Option<ExcelWriteTools>,
            Option<DataQueryTools>,
        ) = match exec_settings
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
                            pdf_to_image_tool =
                                Some(PdfToImageTool::new(service.clone(), artifacts.clone()));
                        }

                        pdf_info_tool = Some(PdfInfoTool::new(service.clone()));
                        pdf_extract_text_tool = Some(PdfExtractTextTool::new(service.clone()));

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
                    let excel_read_tool = if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Excel read tool enabled");
                        Some(ReadExcelTool::new(service.clone()))
                    } else {
                        None
                    };

                    // Excel write tools
                    let excel_write_tools = if write_tools.is_some() {
                        pending_write_approvals.as_ref().map(|approvals| {
                            tracing::info!(workspace = %workspace_dir, "Excel write tools enabled");
                            (
                                WriteExcelTool::new(service.clone(), approvals.clone()),
                                EditExcelTool::new(service.clone(), approvals.clone()),
                            )
                        })
                    } else {
                        None
                    };

                    // Data query tools
                    let data_query_tools = if read_tools.is_some() {
                        tracing::info!(workspace = %workspace_dir, "Data query tools enabled");
                        Some((
                            QueryDataTool::new(service.clone()),
                            DescribeDataTool::new(service.clone()),
                        ))
                    } else {
                        None
                    };

                    (read_tools, write_tools, excel_read_tool, excel_write_tools, data_query_tools)
                }
                Err(e) => {
                    tracing::warn!(error = ?e, workspace = %workspace_dir, "Failed to initialize filesystem tools");
                    (None, None, None, None, None)
                }
            },
            None => (None, None, None, None, None),
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

        // Create MCP management tools (all four are gated on the same setting)
        let mcp_mgmt_tools = {
            let enabled = exec_settings
                .as_ref()
                .map(|s| s.mcp_service_tool_enabled)
                .unwrap_or(false);
            if enabled {
                let (add, delete, edit) = match (
                    crate::MCP_UPDATE_SENDER.get().cloned(),
                    crate::MCP_SERVICE.get().cloned(),
                ) {
                    (Some(sender), Some(service)) => (
                        AddMcpTool::new_with_services(
                            crate::mcp_repository(),
                            sender.clone(),
                            service.clone(),
                        ),
                        DeleteMcpTool::new_with_services(
                            crate::mcp_repository(),
                            sender.clone(),
                            service.clone(),
                        ),
                        EditMcpTool::new_with_services(crate::mcp_repository(), sender, service),
                    ),
                    _ => {
                        tracing::warn!(
                            "MCP_UPDATE_SENDER or MCP_SERVICE not initialized; \
                             MCP tools created without live services"
                        );
                        (
                            AddMcpTool::new(crate::mcp_repository()),
                            DeleteMcpTool::new(crate::mcp_repository()),
                            EditMcpTool::new(crate::mcp_repository()),
                        )
                    }
                };
                McpTools {
                    add: Some(add),
                    delete: Some(delete),
                    edit: Some(edit),
                    list: Some(ListMcpTool::new(crate::mcp_repository())),
                }
            } else {
                tracing::info!("MCP management tools disabled by execution settings");
                McpTools::none()
            }
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
                            std::sync::Arc::new(std::sync::Mutex::new(
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
            add_mcp: mcp_mgmt_tools.is_enabled(),
            fetch: fetch_tool.is_some(),
            shell: shell_tools.is_some(),
            git: git_tools.is_some(),
            search: search_tools.is_some(),
            add_attachment: add_attachment_tool.is_some(),
            excel_read: excel_read_tool.is_some(),
            excel_write: excel_write_tools.is_some(),
            pdf_to_image: pdf_to_image_tool.is_some(),
            pdf_info: pdf_info_tool.is_some(),
            pdf_extract_text: pdf_extract_text_tool.is_some(),
            data_query: data_query_tools.is_some(),
            compile_typst: typst_tool.is_some(),
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
            &tool_availability,
            &search_settings,
            &mcp_mgmt_tools,
            &mcp_tool_info,
            &secret_key_names,
        );

        match &provider_config.provider_type {
            ProviderType::Anthropic => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

                let client = rig::providers::anthropic::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::Anthropic(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble);

                let is_reasoning_model = {
                    let id = model_config.model_identifier.as_str();
                    let is_o_series = {
                        let mut chars = id.chars();
                        chars.next() == Some('o')
                            && chars.next().is_some_and(|c| c.is_ascii_digit())
                    };
                    is_o_series || id.starts_with("gpt-5")
                };

                if model_config.supports_temperature && !is_reasoning_model {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                if is_reasoning_model || !model_config.supports_temperature {
                    // TODO(#127): Remove once rig-core handles reasoning IDs correctly.
                    builder = builder.additional_params(serde_json::json!({
                        "reasoning": {
                            "summary": "auto"
                        }
                    }));
                }

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::OpenAI(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::Gemini(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
            ProviderType::Mistral => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Mistral provider"))?;

                let client = rig::providers::mistral::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::Mistral(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble)
                    .temperature(model_config.temperature as f64);

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::Ollama(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
            ProviderType::AzureOpenAI => {
                let raw_endpoint = base_url.ok_or_else(|| {
                    anyhow!("Endpoint URL not configured for Azure OpenAI provider")
                })?;

                let endpoint = normalize_azure_endpoint(&raw_endpoint);

                let api_version = model_config
                    .extra_params
                    .get("api_version")
                    .map(|s| s.as_str())
                    .unwrap_or(AZURE_DEFAULT_API_VERSION);

                if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                    return Err(anyhow!(
                        "Invalid Azure endpoint URL (must start with https://): '{}'",
                        endpoint
                    ));
                }

                let auth = match provider_config.azure_auth_method() {
                    AzureAuthMethod::EntraId => {
                        tracing::info!("Using Entra ID authentication with token cache");

                        let cache = AZURE_TOKEN_CACHE.get_or_init(|| {
                            match AzureTokenCache::new() {
                                Ok(cache) => Some(cache),
                                Err(e) => {
                                    tracing::warn!(
                                        error = ?e,
                                        "Failed to create Azure token cache, will fetch tokens directly each time"
                                    );
                                    None
                                }
                            }
                        });

                        let token = if let Some(cache) = cache {
                            cache
                                .get_token()
                                .await
                                .context("Failed to get cached Entra ID token")?
                        } else {
                            tracing::debug!("Using direct token fetch (cache unavailable)");
                            azure_auth::fetch_entra_id_token()
                                .await
                                .context("Failed to fetch Entra ID token")?
                        };

                        rig::providers::azure::AzureOpenAIAuth::Token(token)
                    }
                    AzureAuthMethod::ApiKey => {
                        tracing::info!("Using API Key authentication for Azure OpenAI");
                        let key = api_key.ok_or_else(|| {
                            anyhow!("API key not configured for Azure OpenAI provider")
                        })?;
                        rig::providers::azure::AzureOpenAIAuth::ApiKey(key)
                    }
                };

                tracing::info!(
                    endpoint = %endpoint,
                    deployment = %model_config.model_identifier,
                    api_version = %api_version,
                    auth_method = ?provider_config.azure_auth_method(),
                    "Building Azure OpenAI client"
                );

                let client = rig::providers::azure::Client::builder()
                    .api_key(auth)
                    .azure_endpoint(endpoint.clone())
                    .api_version(api_version)
                    .build()
                    .map_err(|e| {
                        anyhow!(
                            "Failed to build Azure client with endpoint '{}': {}",
                            endpoint,
                            e
                        )
                    })?;

                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&preamble);

                let is_reasoning_model = {
                    let id = model_config.model_identifier.as_str();
                    let is_o_series = {
                        let mut chars = id.chars();
                        chars.next() == Some('o')
                            && chars.next().is_some_and(|c| c.is_ascii_digit())
                    };
                    is_o_series || id.starts_with("gpt-5")
                };

                if model_config.supports_temperature && !is_reasoning_model {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                if is_reasoning_model || !model_config.supports_temperature {
                    builder = builder.additional_params(serde_json::json!({
                        "reasoning_effort": "medium"
                    }));
                }

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                let tool_vec = NativeTools {
                    list_tools,
                    fs_read: fs_read_tools,
                    fs_write: fs_write_tools,
                    add_attachment: add_attachment_tool.clone(),
                    pdf_to_image: pdf_to_image_tool.clone(),
                    pdf_info: pdf_info_tool.clone(),
                    pdf_extract_text: pdf_extract_text_tool.clone(),
                    mcp_mgmt: mcp_mgmt_tools,
                    fetch_tool: fetch_tool.clone(),
                    shell_tools,
                    git_tools,
                    search_tools,
                    excel_read: excel_read_tool.clone(),
                    excel_write: excel_write_tools.clone(),
                    data_query: data_query_tools.clone(),
                    chart_tool: chart_tool.clone(),
                    typst_tool: typst_tool.clone(),
                    execute_code_tool: execute_code_tool.clone(),
                    remember_tool: remember_tool.clone(),
                    save_skill_tool: save_skill_tool.clone(),
                    search_memory_tool: search_memory_tool.clone(),
                    read_skill_tool: read_skill_tool.clone(),
                    search_web_tool: search_web_tool.clone(),
                    sub_agent_tool: sub_agent_tool.clone(),
                    browser_use_tool: browser_use_tool.clone(),
                    daytona_tool: daytona_tool.clone(),
                    list_agents_tool: list_agents_tool.clone(),
                    invoke_agent_tool: invoke_agent_tool.clone(),
                    publish_module_tool: publish_module_tool.clone(),
                }
                .into_tool_vec();
                let mcp_tools = sanitize_mcp_tools_for_openai(mcp_tools);
                let agent =
                    build_with_mcp_tools!(builder.tools(tool_vec), mcp_tools, &native_tool_names);

                Ok((
                    AgentClient::AzureOpenAI(agent),
                    shell_session_out,
                    invoke_agent_progress_slot.clone(),
                ))
            }
        }
    }

    /// Returns the provider name for logging/debugging.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &'static str {
        match self {
            AgentClient::Anthropic(_) => "Anthropic",
            AgentClient::OpenAI(_) => "OpenAI",
            AgentClient::Gemini(_) => "Gemini",
            AgentClient::Ollama(_) => "Ollama",
            AgentClient::Mistral(_) => "Mistral",
            AgentClient::AzureOpenAI(_) => "Azure OpenAI",
        }
    }
}

/// Normalize Azure endpoint URL:
/// 1. Strip trailing slashes
/// 2. Add https:// if missing
/// 3. Extract base URL if user provided full path (e.g., .../openai/deployments/...)
fn normalize_azure_endpoint(raw_endpoint: &str) -> String {
    let raw_endpoint = raw_endpoint.trim_end_matches('/').to_string();
    let mut endpoint =
        if raw_endpoint.starts_with("http://") || raw_endpoint.starts_with("https://") {
            raw_endpoint
        } else {
            format!("https://{}", raw_endpoint)
        };

    let hostname_end = endpoint.find("://").and_then(|scheme_pos| {
        endpoint[scheme_pos + 3..]
            .find('/')
            .map(|p| scheme_pos + 3 + p)
    });

    if let Some(path_start) = hostname_end
        && let Some(pos) = endpoint[path_start..].find("/openai")
    {
        endpoint.truncate(path_start + pos);
    }

    endpoint
}

#[cfg(test)]
mod tests {
    use super::mcp_helpers::filter_mcp_tool_info;
    use super::normalize_azure_endpoint;
    use super::tool_registry::{ToolAvailability, active_native_tool_names};

    #[test]
    fn test_azure_url_normalization_basic() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_https() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_http() {
        assert_eq!(
            normalize_azure_endpoint("http://myresource.openai.azure.com"),
            "http://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_trailing_slash() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_multiple_trailing_slashes() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com///"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_path() {
        assert_eq!(
            normalize_azure_endpoint("https://my.openai.azure.com/openai/deployments/gpt4"),
            "https://my.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_with_openai_deployments_path() {
        assert_eq!(
            normalize_azure_endpoint("https://test.openai.azure.com/openai/deployments/"),
            "https://test.openai.azure.com"
        );
    }

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

    #[test]
    fn test_azure_url_normalization_openai_in_hostname() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.openai.azure.com"),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_openai_in_subdomain() {
        assert_eq!(
            normalize_azure_endpoint("https://openai.example.com"),
            "https://openai.example.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_complex_path() {
        assert_eq!(
            normalize_azure_endpoint(
                "myresource.openai.azure.com/openai/deployments/model/chat/completions"
            ),
            "https://myresource.openai.azure.com"
        );
    }

    #[test]
    fn test_azure_url_normalization_path_without_openai() {
        assert_eq!(
            normalize_azure_endpoint("https://myresource.azure.com/api/v1"),
            "https://myresource.azure.com/api/v1"
        );
    }

    #[test]
    fn test_azure_url_normalization_custom_port() {
        assert_eq!(
            normalize_azure_endpoint("https://localhost:8080/openai/deployments"),
            "https://localhost:8080"
        );
    }

    #[test]
    fn test_azure_url_normalization_no_scheme_with_path() {
        assert_eq!(
            normalize_azure_endpoint("myresource.openai.azure.com/openai"),
            "https://myresource.openai.azure.com"
        );
    }
}
