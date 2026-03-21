mod app;
mod engine;
mod events;
mod headless;
mod ui;

use anyhow::{Context, Result, bail};
use chatty_core::MCP_SERVICE;
use chatty_core::services::McpService;
use chatty_core::settings::models::ModelsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use clap::Parser;
use tokio::sync::mpsc;
use tracing::{info, warn};

use engine::ChatEngine;
use events::AppEvent;

#[derive(Parser)]
#[command(
    name = "chatty-tui",
    version,
    about = "Terminal chat interface for Chatty — chat with LLMs from your terminal",
    long_about = "\
Terminal chat interface for Chatty — chat with LLMs from your terminal.

chatty-tui provides three operating modes:

  INTERACTIVE (default):  Full TUI with message history, streaming responses,
                          tool approval prompts, and inline model/tool switching.
                          Keybindings: Enter=send, Ctrl+C=stop/quit, Ctrl+Q=quit,
                          y/n=approve/deny tool calls. Slash commands: /model,
                          /tools, /add-dir, /agent, /clear(/new), /compact,
                          /context, /copy, /cwd(/cd).

  HEADLESS (--headless):  Send a single message via --message, print the full
                          response to stdout, then exit. Useful for scripting
                          and automation. Requires --message.

  PIPE (--pipe):          Read input from stdin, send it as a message, print
                          the response to stdout. Works with shell pipes:
                          echo \"explain this\" | chatty-tui --pipe

PREREQUISITES:
  Providers and models must be configured first. chatty-tui reads settings from
  the shared Chatty config directory (~/.config/chatty/ or platform equivalent).
  Run the Chatty desktop app once to configure providers and API keys, or edit
  the JSON config files directly.

TOOL GROUPS:
  The LLM can use built-in tools during conversations. Each tool group can be
  enabled or disabled at launch with --enable/--disable, or toggled at runtime
  with the /tools command. Available groups:

    shell        Shell command execution (run commands, read env vars)
    fs-read      Read files, list directories, glob, search, PDF/Excel reading
    fs-write     Write, delete, move files, apply diffs, Excel writing
    fetch        HTTP GET requests (zero-config web access)
    git          Git operations (status, diff, log, add, branch, commit)
    mcp-manage   Add/edit/delete/list MCP server configurations
    docker-exec  Run code in a Docker sandbox (requires Docker)
    browser      Browse web pages with Verso browser engine (requires versoview)

  Defaults come from the persisted Chatty execution settings. CLI flags override
  those defaults for the session.

EXAMPLES:
  chatty-tui                                     # Interactive, default model
  chatty-tui --model claude-3.5-sonnet           # Use a specific model
  chatty-tui --enable git,shell --disable fetch   # Custom tool set
  chatty-tui --headless -m \"What is Rust?\"        # One-shot query
  cat src/main.rs | chatty-tui --pipe             # Pipe file contents as input"
)]
struct Cli {
    /// Select which LLM model to use for the conversation.
    ///
    /// Accepts a model ID, display name, or partial model identifier.
    /// Matching is tried in this order: exact ID, case-insensitive name,
    /// substring match on model identifier. If omitted, uses the first
    /// configured model. On mismatch, lists all available models.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,

    /// Run in headless mode: send one message and print the response to stdout.
    ///
    /// Requires --message (-m). No TUI is displayed. The process exits after
    /// the response completes. Exit code 0 on success, non-zero on error.
    /// Logging is suppressed to keep stdout clean.
    #[arg(long)]
    headless: bool,

    /// The message to send in headless mode.
    ///
    /// Only used with --headless. The message is sent as the user prompt
    /// and the full LLM response is printed to stdout.
    #[arg(short, long, value_name = "TEXT")]
    message: Option<String>,

    /// Run in pipe mode: read stdin as the message, print the response to stdout.
    ///
    /// Reads all of stdin until EOF, sends it as the user prompt, and prints
    /// the LLM response to stdout. Useful for shell pipelines:
    ///   cat file.rs | chatty-tui --pipe
    ///   echo "summarize this" | chatty-tui --pipe
    /// Logging is suppressed to keep stdout clean.
    #[arg(long)]
    pipe: bool,

    /// Enable specific tool groups for this session (comma-separated).
    ///
    /// Overrides the persisted Chatty execution settings. Multiple groups
    /// can be specified as a comma-separated list. Valid tool group names:
    ///   shell, fs-read, fs-write, fetch, git, mcp-manage, docker-exec, browser
    ///
    /// Example: --enable shell,git,fetch
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    enable: Vec<String>,

    /// Disable specific tool groups for this session (comma-separated).
    ///
    /// Overrides the persisted Chatty execution settings. Same valid group
    /// names as --enable. Applied after --enable, so if a group appears in
    /// both, it will be disabled.
    ///
    /// Example: --disable fetch,docker-exec
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    disable: Vec<String>,

    /// Auto-approve all tool executions without prompting.
    ///
    /// Skips the y/n approval prompt for shell commands, file writes,
    /// git operations, and all other tool calls. Useful for scripting,
    /// automation, and AI agent workflows where human confirmation is
    /// not needed. Use with caution — the LLM will be able to execute
    /// any enabled tool without user review.
    #[arg(long)]
    auto_approve: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    if cli.headless || cli.pipe {
        // Headless/pipe: suppress all logging to keep stdout clean
    } else {
        // Interactive TUI: log to file to avoid corrupting the terminal
        let log_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("chatty");
        std::fs::create_dir_all(&log_dir).ok();
        let log_file = std::fs::File::create(log_dir.join("chatty-tui.log"))
            .unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap());

        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::WARN.into()),
            )
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .init();
    }

    // Initialize repositories
    chatty_core::init_repositories()
        .context("Failed to initialize settings repositories (is HOME set?)")?;

    // Load providers, models, and execution settings
    let (providers_result, models_result, exec_settings_result) = tokio::join!(
        chatty_core::provider_repository().load_all(),
        chatty_core::models_repository().load_all(),
        chatty_core::execution_settings_repository().load(),
    );

    let providers = providers_result.context("Failed to load providers")?;
    let models_list = models_result.context("Failed to load models")?;
    let mut execution_settings = exec_settings_result.unwrap_or_default();

    // Default workspace_dir to CWD at launch so tools have an explicit root
    if execution_settings.workspace_dir.is_none() {
        if let Ok(cwd) = std::env::current_dir() {
            execution_settings.workspace_dir = Some(cwd.to_string_lossy().to_string());
        }
    }

    // Apply CLI tool overrides
    apply_tool_overrides(&mut execution_settings, &cli.enable, &cli.disable);

    // Apply auto-approve if requested
    if cli.auto_approve {
        use chatty_core::settings::models::execution_settings::ApprovalMode;
        execution_settings.approval_mode = ApprovalMode::AutoApproveAll;
        chatty_core::tools::filesystem_write_tool::set_global_write_approval_mode(
            ApprovalMode::AutoApproveAll,
        );
    }

    let models = {
        let mut m = ModelsModel::new();
        // Apply default capabilities
        let models_with_defaults: Vec<ModelConfig> = models_list
            .into_iter()
            .map(|mut mc| {
                if !mc.supports_images && !mc.supports_pdf {
                    let (img, pdf) = mc.provider_type.default_capabilities();
                    mc.supports_images = img;
                    mc.supports_pdf = pdf;
                }
                mc
            })
            .collect();
        m.replace_all(models_with_defaults);
        m
    };

    // Resolve which model to use
    let model_config = resolve_model(&cli, &models)?;

    // Find the provider config for this model
    let provider_config = providers
        .iter()
        .find(|p| p.provider_type == model_config.provider_type)
        .cloned()
        .context(format!(
            "No provider configured for {:?}",
            model_config.provider_type
        ))?;

    info!(
        model = %model_config.name,
        provider = ?model_config.provider_type,
        "Using model"
    );

    // Parallel batch 2: user secrets, MCP servers, memory service, and search settings
    // are all independent of each other — run them concurrently to reduce startup latency.
    let memory_enabled = execution_settings.memory_enabled;
    let (user_secrets, mcp_service, memory_service, search_settings) = tokio::join!(
        async {
            match chatty_core::user_secrets_repository().load().await {
                Ok(secrets) => secrets.as_env_pairs(),
                Err(_) => vec![],
            }
        },
        start_mcp_servers(),
        async {
            if !memory_enabled {
                info!("Agent memory disabled by settings");
                return None;
            }
            let Some(data_dir) = chatty_core::services::memory_service::memory_data_dir() else {
                warn!("Could not determine data directory for agent memory");
                return None;
            };
            match chatty_core::services::MemoryService::open_or_create(&data_dir).await {
                Ok(service) => {
                    info!("Agent memory service initialized");
                    Some(service)
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to initialize agent memory service");
                    None
                }
            }
        },
        async {
            match chatty_core::search_settings_repository().load().await {
                Ok(settings) => Some(settings),
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to load search settings, using None");
                    None
                }
            }
        },
    );

    // Initialize embedding service for semantic memory search (if configured)
    let embedding_service = if execution_settings.embedding_enabled {
        if let (Some(embed_provider_type), Some(embed_model)) = (
            execution_settings.embedding_provider.as_ref(),
            execution_settings.embedding_model.as_ref(),
        ) {
            // Find the provider config for the embedding provider
            let embed_provider_config = providers
                .iter()
                .find(|p| &p.provider_type == embed_provider_type);
            let api_key = embed_provider_config.and_then(|p| p.api_key.as_deref());
            let base_url = embed_provider_config.and_then(|p| p.base_url.as_deref());

            // Fetch Entra ID token if the Azure provider uses Entra ID auth
            let azure_token = if *embed_provider_type
                == chatty_core::settings::models::providers_store::ProviderType::AzureOpenAI
                && embed_provider_config.map(|p| p.azure_auth_method())
                    == Some(
                        chatty_core::settings::models::providers_store::AzureAuthMethod::EntraId,
                    ) {
                match chatty_core::auth::azure_auth::fetch_entra_id_token().await {
                    Ok(token) => Some(token),
                    Err(e) => {
                        warn!(error = ?e, "Failed to fetch Entra ID token for Azure OpenAI embeddings");
                        None
                    }
                }
            } else {
                None
            };

            let svc = chatty_core::services::embedding_service::try_create_embedding_service(
                embed_provider_type,
                embed_model,
                api_key,
                base_url,
                azure_token,
            );

            // Enable vector index on memory service if embedding service is available
            if let (Some(embed_svc), Some(mem_svc)) = (&svc, &memory_service) {
                if let Err(e) = mem_svc.enable_vec().await {
                    warn!(error = ?e, "Failed to enable vector index on memory service");
                } else if let Err(e) = mem_svc.set_vec_model(&embed_svc.model_identifier()).await {
                    warn!(error = ?e, "Failed to set vector model — falling back to BM25-only");
                }
            }

            svc
        } else {
            info!("Semantic search enabled but no embedding provider/model configured");
            None
        }
    } else {
        None
    };

    // Create event channel
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AppEvent>();

    let mut engine = ChatEngine::new(
        model_config,
        provider_config,
        execution_settings,
        models,
        providers,
        mcp_service,
        memory_service,
        search_settings,
        embedding_service,
        user_secrets,
        event_tx,
    );

    // Route based on mode
    if cli.pipe {
        engine.init_conversation().await?;
        headless::run_pipe(engine, event_rx).await
    } else if cli.headless {
        let message = cli
            .message
            .context("--message is required in headless mode")?;
        engine.init_conversation().await?;
        headless::run_headless(engine, event_rx, message).await
    } else {
        // Interactive mode: show TUI immediately, init conversation in background
        engine.spawn_init_conversation();
        app::run(engine, event_rx).await
    }
}

fn resolve_model(cli: &Cli, models: &ModelsModel) -> Result<ModelConfig> {
    let all_models = models.models();

    if all_models.is_empty() {
        bail!(
            "No models configured. Please configure a model in Chatty's settings first \
             (run the desktop app or edit the config files)."
        );
    }

    if let Some(ref model_id) = cli.model {
        // Try exact match on id first, then name
        if let Some(config) = models.get_model(model_id) {
            return Ok(config.clone());
        }
        // Try case-insensitive name match
        if let Some(config) = all_models
            .iter()
            .find(|m| m.name.to_lowercase() == model_id.to_lowercase())
        {
            return Ok(config.clone());
        }
        // Try partial match on model identifier
        if let Some(config) = all_models
            .iter()
            .find(|m| m.model_identifier.contains(model_id.as_str()))
        {
            return Ok(config.clone());
        }

        bail!(
            "Model '{}' not found. Available models:\n{}",
            model_id,
            all_models
                .iter()
                .map(|m| format!("  - {} ({})", m.name, m.id))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // Default: use first model
    Ok(all_models[0].clone())
}

fn apply_tool_overrides(
    settings: &mut chatty_core::settings::models::ExecutionSettingsModel,
    enable: &[String],
    disable: &[String],
) {
    for name in enable {
        match name.as_str() {
            "shell" => settings.enabled = true,
            "fs-read" => settings.filesystem_read_enabled = true,
            "fs-write" => settings.filesystem_write_enabled = true,
            "fetch" => settings.fetch_enabled = true,
            "git" => settings.git_enabled = true,
            "mcp-manage" => settings.mcp_service_tool_enabled = true,
            "docker-exec" => settings.docker_code_execution_enabled = true,
            "browser" => settings.browser_enabled = true,
            other => {
                tracing::warn!(
                    name = other,
                    "Unknown tool group in --enable (valid: shell, fs-read, fs-write, fetch, git, mcp-manage, docker-exec, browser)"
                );
            }
        }
    }
    for name in disable {
        match name.as_str() {
            "shell" => settings.enabled = false,
            "fs-read" => settings.filesystem_read_enabled = false,
            "fs-write" => settings.filesystem_write_enabled = false,
            "fetch" => settings.fetch_enabled = false,
            "git" => settings.git_enabled = false,
            "mcp-manage" => settings.mcp_service_tool_enabled = false,
            "docker-exec" => settings.docker_code_execution_enabled = false,
            "browser" => settings.browser_enabled = false,
            other => {
                tracing::warn!(
                    name = other,
                    "Unknown tool group in --disable (valid: shell, fs-read, fs-write, fetch, git, mcp-manage, docker-exec, browser)"
                );
            }
        }
    }
}

async fn start_mcp_servers() -> Option<McpService> {
    let mcp_repo = chatty_core::mcp_repository();
    let servers = match mcp_repo.load_all().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "Failed to load MCP server configs");
            return None;
        }
    };

    if servers.is_empty() {
        return None;
    }

    let service = McpService::new();
    MCP_SERVICE
        .set(service.clone())
        .map_err(|_| tracing::warn!("MCP_SERVICE already initialized"))
        .ok();

    // Fire server connections in the background so startup is not blocked.
    // Whatever servers have connected by the time init_conversation() runs
    // will have their tools available; the rest become available after /clear.
    let svc = service.clone();
    tokio::spawn(async move {
        if let Err(e) = svc.start_all(servers).await {
            tracing::error!(error = ?e, "Failed to start MCP servers");
        }
    });

    Some(service)
}
