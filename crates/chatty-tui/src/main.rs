mod app;
mod engine;
mod events;
mod headless;
// Participant mode reaches the broker over a Unix socket; there is no Windows
// equivalent yet, and `chatty_protocol_gateway::worker`'s server half is
// `#[cfg(unix)]` too.
#[cfg(unix)]
mod participant;
mod ui;

use anyhow::{Context, Result, bail};
use chatty_core::services::McpService;
use chatty_core::settings::models::ModelsModel;
use chatty_core::settings::models::extensions_store::ExtensionsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::tools::LocalModuleAgentSummary;
use clap::Parser;
use tokio::sync::mpsc;
use tracing::{info, warn};

use engine::{ChatEngine, ChatEngineConfig};
use events::AppEvent;

pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

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
                          /tools, /modules, /add-dir, /agent, /clear(/new), /compact,
                           /context, /copy, /update, /cwd(/cd), /online.

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
    code-exec    Expose the execute_code tool (Monty-backed Python fast path)
    docker-exec  Allow Docker fallback for execute_code (requires Docker)

  Defaults come from the persisted Chatty execution settings. CLI flags override
  those defaults for the session.

EXAMPLES:
  chatty-tui                                     # Interactive, default model
  chatty-tui --model claude-3.5-sonnet           # Use a specific model
  chatty-tui --ollama                            # Auto-discover local Ollama models
  chatty-tui --ollama --model llama3.2           # Use a specific Ollama model
  chatty-tui --openai-compat-url http://localhost:8000  # Connect to vllm/llama.cpp
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
    ///   shell, fs-read, fs-write, fetch, git, code-exec, docker-exec
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

    /// Connect to a local Ollama instance and auto-discover models.
    ///
    /// No pre-configuration needed — chatty-tui queries the Ollama API
    /// to discover available models and starts immediately. Combine with
    /// --model to pick a specific Ollama model.
    ///
    /// Examples:
    ///   chatty-tui --ollama
    ///   chatty-tui --ollama --model llama3.2
    ///   chatty-tui --ollama http://remote:11434
    #[arg(long, value_name = "URL", default_missing_value = "http://localhost:11434", num_args = 0..=1)]
    ollama: Option<String>,

    /// Connect to any OpenAI-compatible server (vllm, llama.cpp, LM Studio, etc.)
    /// and auto-discover models via /v1/models.
    ///
    /// No pre-configuration needed — just point chatty-tui at your server.
    /// Combine with --model to pick a specific model and --api-key if auth
    /// is required.
    ///
    /// Examples:
    ///   chatty-tui --openai-compat-url http://localhost:8000
    ///   chatty-tui --openai-compat-url http://localhost:8000 --model my-model
    ///   chatty-tui --openai-compat-url https://api.example.com --api-key sk-...
    #[arg(long, value_name = "URL")]
    openai_compat_url: Option<String>,

    /// API key for the OpenAI-compatible server (used with --openai-compat-url).
    ///
    /// Some servers (e.g. hosted vllm endpoints) require an API key.
    /// For servers that don't need auth, this can be omitted.
    #[arg(long, value_name = "KEY")]
    api_key: Option<String>,

    /// Workspace root for this session, overriding the persisted setting.
    ///
    /// Every filesystem, shell and git tool resolves paths against this root.
    /// Without it a process falls back to the shared `execution_settings.json`
    /// value, which is why parallel sub-agents all wrote one tree (AGE-314);
    /// a worker is given its own `git worktree` through this flag.
    ///
    /// Example: --workspace /repo/.chatty/worktrees/w1
    #[arg(long, value_name = "DIR")]
    workspace: Option<String>,

    /// Run as a participant of the broker listening on this Unix socket.
    ///
    /// The process registers, waits for one delegated task, runs it, and
    /// reports its progress and result over the socket rather than on
    /// stderr (ADR-0011 / AGE-301). Implies the
    /// headless turn loop; `--message` is not used, the prompt arrives from
    /// the broker.
    ///
    /// Example: --participant-socket ~/.local/state/chatty/participants.sock
    #[arg(long, value_name = "PATH")]
    participant_socket: Option<std::path::PathBuf>,

    /// The name to register under, which is how callers address this worker.
    ///
    /// Required with --participant-socket: the broker allocated it before
    /// spawning this process and is already routing a task to it.
    #[arg(long, value_name = "NAME", requires = "participant_socket")]
    participant_name: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    if cli.headless || cli.pipe || cli.participant_socket.is_some() {
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

    // Load providers, models, execution settings, module settings, and A2A agents
    let (
        providers_result,
        models_result,
        exec_settings_result,
        module_settings_result,
        extensions_result,
        a2a_agents_result,
    ) = tokio::join!(
        chatty_core::provider_repository().load_all(),
        chatty_core::models_repository().load_all(),
        chatty_core::execution_settings_repository().load(),
        chatty_core::module_settings_repository().load(),
        chatty_core::extensions_repository().load(),
        chatty_core::a2a_repository().load_all(),
    );

    let mut providers = providers_result.context("Failed to load providers")?;
    let mut models_list = models_result.context("Failed to load models")?;
    let mut execution_settings = exec_settings_result.unwrap_or_default();
    let module_settings = module_settings_result.unwrap_or_default();
    let extensions = extensions_result.unwrap_or_default();
    let remote_agents = a2a_agents_result.unwrap_or_default();
    let module_agents = discover_module_agents(&module_settings, &extensions);

    // --ollama / --openai-compat-url: auto-discover models from a running server
    // and inject ephemeral provider + model configs so no pre-configuration is needed.
    if let Some(ref ollama_url) = cli.ollama {
        let discovered = discover_ollama(ollama_url).await?;
        inject_discovered(
            &mut providers,
            &mut models_list,
            discovered,
            ProviderType::Ollama,
            "Ollama (CLI)",
            Some(ollama_url.clone()),
            None,
        );
    }
    if let Some(ref compat_url) = cli.openai_compat_url {
        let discovered = discover_openai_compat(compat_url, cli.api_key.as_deref()).await?;
        // OpenAI-compatible servers may not require auth, but rig's OpenAI
        // client always needs an API key string. Use a placeholder when none
        // is provided — most local servers (vllm, llama.cpp) ignore it.
        let api_key = match cli.api_key.clone() {
            Some(key) => key,
            None => {
                info!(
                    "No --api-key provided; using placeholder (most local servers don't need auth)"
                );
                "no-key-required".to_string()
            }
        };
        inject_discovered(
            &mut providers,
            &mut models_list,
            discovered,
            ProviderType::OpenRouter,
            "OpenAI-compat (CLI)",
            Some(compat_url.clone()),
            Some(api_key),
        );
    }

    // `--workspace` overrides the persisted setting, at the same precedence as
    // `--enable` / `--disable` / `--auto-approve`. AGE-314: a sub-agent needs a
    // root of its own, and the settings file is shared with the parent that
    // spawned it, so the flag is the only seam that can separate them.
    if let Some(workspace) = cli.workspace.as_deref() {
        let path = std::path::Path::new(workspace);
        if !path.is_dir() {
            anyhow::bail!("--workspace '{workspace}' is not an existing directory");
        }
        let root = std::fs::canonicalize(path)
            .with_context(|| format!("Failed to resolve --workspace '{workspace}'"))?;
        execution_settings.workspace_dir = Some(root.to_string_lossy().to_string());
    }

    // Default workspace_dir to CWD at launch so tools have an explicit root
    if execution_settings.workspace_dir.is_none()
        && let Ok(cwd) = std::env::current_dir()
    {
        execution_settings.workspace_dir = Some(cwd.to_string_lossy().to_string());
    }

    // Apply CLI tool overrides
    apply_tool_overrides(&mut execution_settings, &cli.enable, &cli.disable);

    // Apply auto-approve if requested
    if cli.auto_approve {
        use chatty_core::settings::models::execution_settings::ApprovalMode;
        execution_settings.approval_mode = ApprovalMode::AutoApproveAll;
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

    // Create event channel
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Route based on mode — headless/pipe load all services eagerly (latency
    // doesn't matter for non-interactive use), while the interactive TUI defers
    // heavy services to a background task so the UI appears instantly.
    let participant_mode = cli.participant_socket.is_some();
    let result = if cli.pipe || cli.headless || participant_mode {
        // ── Headless / pipe / participant: load everything before running ──
        let (user_secrets, mcp_service, memory_service, search_settings) =
            load_deferred_services(&execution_settings).await;

        let embedding_service =
            init_embedding_service(&execution_settings, &providers, &memory_service).await;

        // Headless rides the session directly: no engine, no terminal state
        // (AGE-196).
        let mut engine = headless::HeadlessRunner::new(
            ChatEngineConfig {
                model_config,
                provider_config,
                execution_settings,
                module_settings,
                models,
                providers,
                mcp_service,
                memory_service,
                search_settings,
                embedding_service,
                user_secrets,
                remote_agents,
                module_agents: module_agents.clone(),
                is_sub_agent: true,
                services_loaded: true,
                surface: chatty_core::services::StreamSurface::Headless,
            },
            event_tx,
        );

        engine.init_conversation().await?;
        if let Some(socket) = cli.participant_socket.as_deref() {
            #[cfg(unix)]
            {
                let name = cli
                    .participant_name
                    .as_deref()
                    .context("--participant-name is required with --participant-socket")?;
                participant::run_participant(engine, event_rx, socket, name).await
            }
            #[cfg(not(unix))]
            {
                // `bail!` would return from `main` and skip the shutdown below;
                // this branch has to hand back an `Err` like every other arm.
                let _ = (socket, event_rx, engine);
                Err(anyhow::anyhow!(
                    "--participant-socket needs a Unix socket, which this platform has not got"
                ))
            }
        } else if cli.pipe {
            headless::run_pipe(engine, event_rx).await
        } else {
            let message = cli
                .message
                .context("--message is required in headless mode")?;
            headless::run_headless(engine, event_rx, message).await
        }
    } else {
        // ── Interactive TUI: start immediately, load services in background ──
        let mut engine = ChatEngine::new(
            ChatEngineConfig {
                model_config,
                provider_config,
                execution_settings: execution_settings.clone(),
                module_settings,
                models,
                providers: providers.clone(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: vec![],
                remote_agents,
                module_agents,
                is_sub_agent: false,
                services_loaded: false,
                surface: chatty_core::services::StreamSurface::InteractiveTui,
            },
            event_tx.clone(),
        );

        // Show TUI first, then init conversation once services arrive.
        // Spawn background tasks for heavy services and git branch detection.
        let bg_tx = event_tx.clone();
        tokio::spawn(async move {
            let (user_secrets, mcp_service, memory_service, search_settings) =
                load_deferred_services(&execution_settings).await;

            let embedding_service =
                init_embedding_service(&execution_settings, &providers, &memory_service).await;

            let _ = bg_tx.send(AppEvent::ServicesReady(Box::new(
                events::DeferredServices {
                    user_secrets,
                    mcp_service,
                    memory_service,
                    search_settings,
                    embedding_service,
                },
            )));
        });

        // Detect the git branch and its pull request in the background
        // (avoids blocking on subprocess spawn).
        engine.refresh_workspace_context();

        // Start conversation init immediately (without heavy services).
        // It will be re-initialized once ServicesReady arrives with full context.
        engine.spawn_init_conversation();
        app::run(engine, event_rx).await
    };

    // The engine (and with it any `SandboxManager`) is gone, but its `Drop`
    // could only spawn a detached cleanup task, which dies with the runtime
    // this function returns into. Sandbox containers run `sleep infinity`
    // with no `--rm`, so tear them down here instead. Headless and
    // participant runs are short-lived and spawned per task, so this is the
    // path that would accumulate containers fastest.
    if let Err(e) = chatty_core::sandbox::shutdown_all().await {
        warn!(error = %e, "Failed to destroy sandbox containers during shutdown");
    }

    result
}

/// Load all deferred services concurrently (MCP, memory, user secrets, search settings).
async fn load_deferred_services(
    execution_settings: &chatty_core::settings::models::ExecutionSettingsModel,
) -> (
    Vec<(String, String)>,
    Option<McpService>,
    Option<chatty_core::services::MemoryService>,
    Option<chatty_core::settings::models::search_settings::SearchSettingsModel>,
) {
    let memory_enabled = execution_settings.memory_enabled;
    tokio::join!(
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
    )
}

/// Initialize embedding service for semantic memory search (if configured).
async fn init_embedding_service(
    execution_settings: &chatty_core::settings::models::ExecutionSettingsModel,
    providers: &[ProviderConfig],
    memory_service: &Option<chatty_core::services::MemoryService>,
) -> Option<chatty_core::services::EmbeddingService> {
    if !execution_settings.embedding_enabled {
        return None;
    }

    let (embed_provider_type, embed_model) = match (
        execution_settings.embedding_provider.as_ref(),
        execution_settings.embedding_model.as_ref(),
    ) {
        (Some(pt), Some(m)) => (pt, m),
        _ => {
            info!("Semantic search enabled but no embedding provider/model configured");
            return None;
        }
    };

    let embed_provider_config = providers
        .iter()
        .find(|p| &p.provider_type == embed_provider_type);
    let api_key = embed_provider_config.and_then(|p| p.api_key.as_deref());
    let base_url = embed_provider_config.and_then(|p| p.base_url.as_deref());

    // Fetch Entra ID token if the Azure provider uses Entra ID auth
    let azure_token = if *embed_provider_type
        == chatty_core::settings::models::providers_store::ProviderType::AzureOpenAI
        && embed_provider_config.map(|p| p.azure_auth_method())
            == Some(chatty_core::settings::models::providers_store::AzureAuthMethod::EntraId)
    {
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
    if let (Some(embed_svc), Some(mem_svc)) = (&svc, memory_service) {
        if let Err(e) = mem_svc.enable_vec().await {
            warn!(error = ?e, "Failed to enable vector index on memory service");
        } else if let Err(e) = mem_svc.set_vec_model(&embed_svc.model_identifier()).await {
            warn!(error = ?e, "Failed to set vector model — falling back to BM25-only");
        }
    }

    svc
}

fn discover_module_agents(
    module_settings: &chatty_core::settings::models::module_settings::ModuleSettingsModel,
    extensions: &ExtensionsModel,
) -> Vec<LocalModuleAgentSummary> {
    let enabled_ids: std::collections::HashSet<&str> =
        extensions.wasm_module_ids().into_iter().collect();
    let root = std::path::Path::new(&module_settings.module_dir);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut agents = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("module.toml");
        if !manifest_path.is_file() {
            continue;
        }

        match chatty_module_registry::ModuleManifest::from_file(&manifest_path) {
            Ok(manifest)
                if manifest.capabilities.agent && enabled_ids.contains(manifest.name.as_str()) =>
            {
                agents.push(LocalModuleAgentSummary {
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    tools: manifest.capabilities.tools,
                    supports_a2a: manifest.protocols.a2a,
                    execution_mode: manifest.execution_mode,
                });
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    error = ?error,
                    manifest = %manifest_path.display(),
                    "Failed to parse module manifest for TUI agent discovery"
                );
            }
        }
    }

    agents.sort_by(|left, right| left.name.cmp(&right.name));
    agents
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

    // No --model: the model marked default in the desktop settings UI, which
    // is the only place the marker can be set. Falling straight through to
    // list order here meant the TUI silently ignored it (#583).
    if let Some(config) = models.default_model() {
        return Ok(config.clone());
    }

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
            "code-exec" => settings.execute_code_enabled = true,
            "docker-exec" => {
                settings.execute_code_enabled = true;
                settings.docker_code_execution_enabled = true;
            }
            other => {
                tracing::warn!(
                    name = other,
                    "Unknown tool group in --enable (valid: shell, fs-read, fs-write, fetch, git, code-exec, docker-exec)"
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
            "code-exec" => settings.execute_code_enabled = false,
            "docker-exec" => settings.docker_code_execution_enabled = false,
            other => {
                tracing::warn!(
                    name = other,
                    "Unknown tool group in --disable (valid: shell, fs-read, fs-write, fetch, git, code-exec, docker-exec)"
                );
            }
        }
    }
}

async fn start_mcp_servers() -> Option<McpService> {
    let mcp_repo = chatty_core::mcp_repository();
    let mut servers = match mcp_repo.load_all().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "Failed to load MCP server configs");
            vec![]
        }
    };

    // Load extensions and ensure the built-in Hive MCP server exists
    let ext_repo = chatty_core::extensions_repository();
    let hive_repo = chatty_core::hive_settings_repository();
    let (ext_result, hive_result) = tokio::join!(ext_repo.load(), hive_repo.load());

    let mut extensions = ext_result.unwrap_or_default();
    let hive_settings = hive_result.unwrap_or_default();

    let hive_added = chatty_core::install::ensure_default_hive_mcp(
        &hive_settings.registry_url,
        &mut extensions,
        &mut servers,
    );
    // Seed the curated catalog of well-known external MCP servers
    // (Hugging Face, Notion, Atlassian, …). Entries default to disabled.
    let curated_added =
        chatty_core::curated_mcp::ensure_curated_mcp_servers(&mut extensions, &mut servers);

    // Merge enabled MCP servers from extensions into the server list
    for ext_server in extensions.mcp_servers() {
        if !servers.iter().any(|s| s.name == ext_server.name) {
            servers.push(ext_server.clone());
        }
    }

    // Persist if we added the default Hive MCP entry or any curated entries
    if hive_added || curated_added {
        if let Err(e) = ext_repo.save(extensions).await {
            tracing::warn!(error = ?e, "Failed to persist seeded MCP extensions");
        }
        if let Err(e) = mcp_repo.save_all(servers.clone()).await {
            tracing::warn!(error = ?e, "Failed to persist MCP servers after seeding defaults");
        }
    }

    let enabled_servers: Vec<_> = servers.into_iter().filter(|s| s.enabled).collect();
    if enabled_servers.is_empty() {
        return None;
    }

    let service = McpService::new();

    let svc = service.clone();
    tokio::spawn(async move {
        if let Err(e) = svc.connect_all(enabled_servers).await {
            tracing::error!(error = ?e, "Failed to connect to MCP servers");
        }
    });

    Some(service)
}

// ---------------------------------------------------------------------------
// Zero-config server discovery (--ollama / --openai-compat-url)
// ---------------------------------------------------------------------------

/// A discovered model from a running server (identifier + display name + vision flag).
struct DiscoveredModel {
    identifier: String,
    display_name: String,
    supports_vision: bool,
}

/// Query a running Ollama instance at `base_url` via `/api/tags` (and `/api/show`
/// for vision detection). Returns an error if Ollama is unreachable.
async fn discover_ollama(base_url: &str) -> Result<Vec<DiscoveredModel>> {
    use chatty_core::settings::providers::ollama::discovery::discover_ollama_models;

    let models = discover_ollama_models(base_url).await.with_context(|| {
        format!(
            "Could not connect to Ollama at {base_url} — is it running?\n\
                 Start it with: ollama serve"
        )
    })?;

    if models.is_empty() {
        bail!(
            "Ollama is running at {base_url} but has no models installed.\n\
             Pull one with: ollama pull llama3.2"
        );
    }

    Ok(models
        .into_iter()
        .map(
            |(identifier, display_name, supports_vision)| DiscoveredModel {
                identifier,
                display_name,
                supports_vision,
            },
        )
        .collect())
}

/// JSON shape returned by the OpenAI-compatible `/v1/models` endpoint
/// (used by vllm, llama.cpp, LM Studio, etc.).
#[derive(serde::Deserialize)]
struct OpenAIModelList {
    data: Vec<OpenAIModelEntry>,
}

#[derive(serde::Deserialize)]
struct OpenAIModelEntry {
    id: String,
}

/// Query an OpenAI-compatible server at `base_url` via `GET /v1/models`.
async fn discover_openai_compat(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = chatty_core::services::http_client::default_client(15);

    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.with_context(|| {
        format!("Could not connect to OpenAI-compatible server at {base_url} — is it running?")
    })?;

    if !resp.status().is_success() {
        bail!(
            "Server at {} returned HTTP {} when listing models",
            url,
            resp.status()
        );
    }

    let list: OpenAIModelList = resp
        .json()
        .await
        .context("Failed to parse /v1/models response (is this an OpenAI-compatible server?)")?;

    if list.data.is_empty() {
        bail!(
            "Server at {base_url} returned an empty model list.\n\
             Make sure at least one model is loaded."
        );
    }

    Ok(list
        .data
        .into_iter()
        .map(|entry| {
            let display_name = entry.id.clone();
            DiscoveredModel {
                identifier: entry.id,
                display_name,
                supports_vision: false,
            }
        })
        .collect())
}

/// Inject discovered models and a synthetic provider config into the existing
/// provider/model lists. This is ephemeral — nothing is persisted to disk.
fn inject_discovered(
    providers: &mut Vec<ProviderConfig>,
    models_list: &mut Vec<ModelConfig>,
    discovered: Vec<DiscoveredModel>,
    provider_type: ProviderType,
    provider_name: &str,
    base_url: Option<String>,
    api_key: Option<String>,
) {
    // Add a synthetic provider if one of this type doesn't already exist
    if !providers.iter().any(|p| p.provider_type == provider_type) {
        let mut config = ProviderConfig::new(provider_name.to_string(), provider_type.clone());
        config.base_url = base_url.clone();
        config.api_key = api_key;
        providers.push(config);
    } else if let Some(existing) = providers
        .iter_mut()
        .find(|p| p.provider_type == provider_type)
    {
        // Update base_url if the user provided one via CLI
        if let Some(ref url) = base_url {
            existing.base_url = Some(url.clone());
        }
    }

    // Add discovered models that aren't already configured
    let existing_identifiers: std::collections::HashSet<String> = models_list
        .iter()
        .filter(|m| m.provider_type == provider_type)
        .map(|m| m.model_identifier.clone())
        .collect();

    for dm in discovered {
        if existing_identifiers.contains(&dm.identifier) {
            continue;
        }
        let id = format!(
            "cli-{}-{}",
            provider_type
                .display_name()
                .to_lowercase()
                .replace(' ', "-"),
            dm.identifier.replace([':', '/'], "-")
        );
        let mut mc = ModelConfig::new(id, dm.display_name, provider_type.clone(), dm.identifier);
        mc.supports_images = dm.supports_vision;
        models_list.push(mc);
    }
}

#[cfg(test)]
mod resolve_model_tests {
    use super::{Cli, resolve_model};
    use chatty_core::settings::models::ModelsModel;
    use chatty_core::settings::models::models_store::ModelConfig;
    use chatty_core::settings::models::providers_store::ProviderType;
    use clap::Parser;

    fn store(ids: &[&str]) -> ModelsModel {
        let mut store = ModelsModel::new();
        for id in ids {
            store.add_model(ModelConfig::new(
                id.to_string(),
                id.to_string(),
                ProviderType::OpenRouter,
                format!("vendor/{id}"),
            ));
        }
        store
    }

    fn cli(args: &[&str]) -> Cli {
        let mut argv = vec!["chatty-tui"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv)
    }

    /// #583: the marker is set in the desktop settings UI, and the TUI used
    /// to fall straight through to list order and ignore it.
    #[test]
    fn no_model_flag_prefers_the_marked_default_over_list_order() {
        let mut models = store(&["first", "second"]);
        assert!(models.set_default("second"));

        let resolved = resolve_model(&cli(&[]), &models).expect("a model resolves");

        assert_eq!(resolved.id, "second");
    }

    #[test]
    fn no_model_flag_and_no_marker_still_takes_the_first_model() {
        let models = store(&["first", "second"]);

        let resolved = resolve_model(&cli(&[]), &models).expect("a model resolves");

        assert_eq!(resolved.id, "first");
    }

    /// An explicit --model is the user speaking about this session; it wins
    /// over the persisted marker.
    #[test]
    fn an_explicit_model_flag_beats_the_marked_default() {
        let mut models = store(&["first", "second"]);
        assert!(models.set_default("second"));

        let resolved =
            resolve_model(&cli(&["--model", "first"]), &models).expect("a model resolves");

        assert_eq!(resolved.id, "first");
    }

    #[test]
    fn no_models_configured_is_an_error() {
        assert!(resolve_model(&cli(&[]), &ModelsModel::new()).is_err());
    }
}

#[cfg(test)]
mod cli_smoke_tests {
    use super::Cli;
    use clap::CommandFactory;

    /// Replaces the old CI `cargo build -p chatty-tui && ./target/debug/chatty-tui --help`
    /// step. `cargo test` already compiles this crate; a second non-test bin
    /// build was rebuilding for ~15 minutes on GitHub-hosted runners.
    #[test]
    fn help_renders_and_names_core_modes() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("chatty-tui"), "{help}");
        assert!(help.contains("--headless"), "{help}");
        assert!(help.contains("--pipe"), "{help}");
    }
}
