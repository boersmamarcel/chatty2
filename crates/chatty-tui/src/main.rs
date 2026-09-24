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
use chatty_core::settings::models::models_store::{ModelConfig, resolve_model_query};
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::tools::LocalModuleAgentSummary;
use clap::Parser;
use std::path::Path;
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
    ask-user     Allow the model to ask the user a clarifying question

  Defaults come from the persisted Chatty execution settings. CLI flags override
  those defaults for the session. --only replaces the defaults outright with a
  strict allow-list instead of adding to or subtracting from them.

EXAMPLES:
  chatty-tui                                     # Interactive, default model
  chatty-tui --model claude-3.5-sonnet           # Use a specific model
  chatty-tui --ollama                            # Auto-discover local Ollama models
  chatty-tui --ollama --model llama3.2           # Use a specific Ollama model
  chatty-tui --openai-compat-url http://localhost:8000  # Connect to vllm/llama.cpp
  chatty-tui --enable git,shell --disable fetch   # Custom tool set
  chatty-tui --only fs-read,fs-write              # Strict allow-list
  chatty-tui --headless -m \"What is Rust?\"        # One-shot query
  cat src/main.rs | chatty-tui --pipe             # Pipe file contents as input"
)]
struct Cli {
    /// Select which LLM model to use for the conversation.
    ///
    /// Accepts a model ID, display name, or partial model identifier.
    /// Matching is tried in this order: exact ID, case-insensitive name,
    /// substring match on model identifier. If omitted, uses the model marked
    /// default in the desktop app's model list (the first configured model
    /// when none is). On mismatch, lists all available models.
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
    ///   shell, fs-read, fs-write, fetch, git, code-exec, docker-exec, ask-user
    ///
    /// An unknown name is a hard error, not a warning.
    ///
    /// Example: --enable shell,git,fetch
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    enable: Vec<String>,

    /// Disable specific tool groups for this session (comma-separated).
    ///
    /// Overrides the persisted Chatty execution settings. Same valid group
    /// names as --enable. Applied after --enable, so if a group appears in
    /// both, it will be disabled. An unknown name is a hard error.
    ///
    /// Example: --disable fetch,docker-exec
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    disable: Vec<String>,

    /// Run with exactly these tool groups and no others (comma-separated
    /// strict allow-list). Same group names as --enable/--disable. Unlike
    /// --enable, which adds to the persisted defaults, --only ignores them:
    /// every unnamed group is turned off. Applied after --enable/--disable
    /// (which are otherwise unaffected — --only just replaces their effect
    /// for the groups it manages) and before --tools/--tool-loading.
    ///
    /// Example: --only fs-read,fs-write
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    only: Vec<String>,

    /// How the model is offered its tools: `all` (default) sends every
    /// enabled tool's schema with every request; `dynamic` sends a small
    /// core — shell_execute, read_file, write_file, apply_diff, search_code,
    /// glob_search, the todo plan — plus `load_tools`, with which the model
    /// loads a group (web, git, files, documents, data, ...) when the task
    /// needs it. A loaded group stays loaded. Overrides the persisted
    /// setting for this session; valid with --headless, --pipe and the
    /// interactive TUI.
    ///
    /// Example: --tool-loading dynamic
    #[arg(long, value_name = "MODE")]
    tool_loading: Option<chatty_core::settings::models::ToolLoading>,

    /// Run with a named tool profile: an allowlist of tool *names*.
    ///
    /// Where --enable/--disable work on tool groups, a profile is the whole
    /// tool set of a role (ADR-0011 C11) — anything it does not name is
    /// dropped, MCP tools included. It only ever removes tools: a profile
    /// cannot turn a group back on that the settings switched off.
    /// Valid profiles: coordinator, coder, reviewer.
    ///
    /// Applied after --enable/--disable, and it wins over both.
    ///
    /// Example: --tools reviewer
    #[arg(long, value_name = "PROFILE")]
    tools: Option<String>,

    /// Standing instructions for this process's role, appended to the system
    /// prompt after the base preamble (ADR-0011 C11).
    ///
    /// Declared as a virtual agent's `preamble` in module settings and
    /// forwarded here; the broker passes it verbatim.
    ///
    /// Example: --preamble "You are the reviewer. Never edit the tree."
    #[arg(long, value_name = "TEXT")]
    preamble: Option<String>,

    /// Override this process's turn budget, replacing the persisted
    /// `execution_settings.max_agent_turns` for this run. `0` is no cap.
    ///
    /// A run without a human (--headless, --pipe, a participant) ignores the
    /// persisted value: it runs under this flag, else a team's budget, else
    /// no turn cap and a --max-duration budget.
    ///
    /// Declared as a virtual agent's `max_agent_turns` in module settings
    /// and forwarded here so a delegated worker can run longer than the
    /// default without raising the leader's own budget (`--team`'s
    /// `max_agent_turns`, which this does not change). Applied after
    /// `--team`, so it wins over a team's persisted budget too.
    ///
    /// Example: --max-agent-turns 30
    #[arg(long, value_name = "N")]
    max_agent_turns: Option<u32>,

    /// Wall-clock budget of a run without a human (--headless, --pipe, a
    /// participant): seconds, or numbers with `s`, `m` or `h` (`90`, `30m`,
    /// `1h30m`). From 85 % of it on, tool results tell the model how much
    /// time is left; once it is spent the model answers with tools
    /// disabled, and a pass that overruns it is stopped for that answer.
    ///
    /// Defaults to 30m when the run has no turn cap (no --max-agent-turns
    /// or team budget, or an explicit 0). Ignored by the interactive TUI,
    /// which has Stop.
    ///
    /// Example: --max-duration 2h
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    max_duration: Option<std::time::Duration>,

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

    /// Force a reasoning model's extended-thinking mode on or off for this
    /// session, overriding any persisted model config (AGE-455).
    ///
    /// Ollama gets the request's top-level `think` field (the same switch
    /// AGE-400 wired up via a persisted `extra_params.think`); OpenAI-compat
    /// servers with a reasoning parser (e.g. vLLM's `--reasoning-parser`) get
    /// `chat_template_kwargs.enable_thinking`. Works with a bare
    /// `--openai-compat-url`/`--ollama` session — no `~/.config/chatty/`
    /// directory required. Omit to leave the provider's default behavior
    /// untouched.
    ///
    /// Example: --think false
    #[arg(long, value_name = "BOOL")]
    think: Option<bool>,

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

    /// Run this leader's own broker, so it can delegate to `local-agent`.
    ///
    /// Starts a protocol gateway on an ephemeral port and a participant
    /// socket next to it, and offers a `local-agent` virtual worker
    /// (ADR-0011 C2) that `invoke_agent`/`list_agents` can reach — the same
    /// wiring the desktop's module settings turn on, minus the WASM module
    /// runtime. Valid with --headless, --pipe and the interactive TUI.
    /// A worker is a `chatty-tui` process next to this one; when the
    /// workspace is a git repository each worker gets its own `git
    /// worktree`, as on the desktop. Unix only.
    #[arg(long)]
    broker: bool,

    /// Run as the leader of a team directory (ADR-0011 C13): `teams/<ID>/`
    /// holds `team.json` — the roster, the leader's profile and preamble,
    /// the verification command, the skill and the turn budget — and the
    /// `SKILL.md` beside it.
    ///
    /// Implies --broker. The roster replaces module settings'
    /// `virtual_agents` for this run (nothing is written back), the
    /// leader's profile and preamble apply unless --tools / --preamble are
    /// given, its model applies unless --model is, `max_agent_turns`
    /// replaces the persisted budget, and the first turn opens with
    /// "read_skill <skill> and follow it". Searched in
    /// `<workspace>/.chatty/teams/`, then the platform data directory's
    /// `chatty/teams/`, then the presets compiled in: `coder-reviewer`.
    /// Valid with --headless, --pipe and the interactive TUI.
    ///
    /// Example: --team coder-reviewer --headless -m "Fix the overdraft bug."
    #[arg(long, value_name = "ID")]
    team: Option<String>,
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
        // AGE-403: accept either `http://host:port` or `http://host:port/v1`
        // and discover/store the same canonical `.../v1` base either way —
        // the OpenRouter-shaped client this injects posts `/chat/completions`
        // under the stored base_url, so it must already end in `/v1`.
        let compat_base_url = normalize_openai_compat_base_url(compat_url);
        let discovered = discover_openai_compat(&compat_base_url, cli.api_key.as_deref()).await?;
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
            Some(compat_base_url),
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

    // --team (AGE-407): the team directory is declared for this run only —
    // its roster and verification go to the broker as a copy of the module
    // settings, so `/modules` still saves exactly what was on disk
    // (AGE-382), and its turn budget to this run's execution settings.
    // Resolved after `--workspace`, since the workspace is the first place
    // a team is looked for.
    let team = match cli.team.as_deref() {
        Some(id) => Some(
            chatty_core::services::team::load_team(
                id,
                execution_settings.workspace_dir.as_deref().map(Path::new),
                dirs::data_dir().as_deref(),
            )
            .with_context(|| format!("--team '{id}' could not be loaded"))?,
        ),
        None => None,
    };
    let broker_module_settings = match team.as_ref() {
        Some(team) => {
            team.apply_turn_budget(&mut execution_settings);
            info!(team = %team.id, source = ?team.source, "Running as a team leader");
            team.run_module_settings(&module_settings)
        }
        None => module_settings.clone(),
    };
    let leader = team.as_ref().map(|t| &t.file.leader);

    // --max-agent-turns (AGE-440): a delegated worker's own turn budget,
    // set via its `VirtualAgentConfig`/`extra_args`. Applied after --team
    // so it wins over a team's persisted (leader) budget too. A run without
    // a human never takes the persisted cap (see `unattended_run_limits`).
    let unattended = cli.headless || cli.pipe || cli.participant_socket.is_some();
    let max_duration = if unattended {
        let (turns, duration) = unattended_run_limits(
            cli.max_agent_turns,
            team.as_ref().and_then(|t| t.file.max_agent_turns),
            cli.max_duration,
        );
        execution_settings.max_agent_turns = turns;
        duration
    } else {
        if let Some(turns) = cli.max_agent_turns {
            execution_settings.max_agent_turns = turns;
        }
        None
    };

    // Apply CLI tool overrides
    apply_tool_overrides(&mut execution_settings, &cli.enable, &cli.disable)?;
    if !cli.only.is_empty() {
        apply_tool_only(&mut execution_settings, &cli.only)?;
    }
    if let Some(tool_loading) = cli.tool_loading {
        execution_settings.tool_loading = tool_loading;
    }

    // --tools / --preamble: the role this process runs as (ADR-0011 C11);
    // a team's leader role fills in whichever flag was not given.
    let role = resolve_role(
        cli.tools
            .as_deref()
            .or(leader.and_then(|l| l.profile.as_deref())),
        cli.preamble
            .as_deref()
            .or(leader.and_then(|l| l.preamble.as_deref())),
    )?;

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

    // Resolve which model to use: --model, else the team leader's, else the
    // roster's default.
    let mut model_config = resolve_model(
        cli.model
            .as_deref()
            .or(leader.and_then(|l| l.model.as_deref())),
        &models,
    )?;

    // --think (AGE-455): explicit override of the model's `extra_params.think`
    // switch, so a bare `--openai-compat-url`/`--ollama` session can toggle
    // extended thinking with no persisted config. `ollama_think` and
    // `openai_compat_think` in provider_builder.rs both read this same field.
    if let Some(think) = cli.think {
        model_config
            .extra_params
            .insert("think".to_string(), think.to_string());
    }

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

    // --broker (AGE-376): run this leader's own protocol gateway so
    // `invoke_agent`/`list_agents` can reach its virtual agents —
    // `local-agent`, or the named team module settings declare (AGE-377) —
    // the same wiring chatty-gpui's module-settings controller turns on for
    // the desktop. The leader's own provider flags ride along to every
    // worker: a leader configured by `--ollama`/`--openai-compat-url` has
    // no config dir a child could read. Unix only — the participant socket
    // underneath it does not exist elsewhere yet.
    // `--team` implies `--broker`: a team is nothing without its workers.
    let run_broker = cli.broker || cli.team.is_some();
    #[cfg(unix)]
    let broker = if run_broker {
        match participant::broker::Broker::start(
            models.models(),
            &providers,
            &broker_module_settings,
            execution_settings.workspace_dir.clone(),
            matches!(
                execution_settings.approval_mode,
                chatty_core::settings::models::execution_settings::ApprovalMode::AutoApproveAll
            ),
            &participant::broker::provider_flags(
                cli.ollama.as_deref(),
                cli.openai_compat_url.as_deref(),
                cli.api_key.as_deref(),
            ),
        )
        .await
        {
            Ok(broker) => Some(broker),
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to start the broker; --broker delegation is unavailable"
                );
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(unix))]
    if run_broker {
        bail!("--broker needs a Unix socket, which this platform has not got");
    }

    // The broker's ephemeral port, kept out of `module_settings` (AGE-382):
    // `--broker` only threads it into this run's `AgentBuildContext`, it
    // never persists it, so `/modules` sees and saves only what was on disk.
    #[cfg(unix)]
    let broker_port = broker.as_ref().map(|b| b.port);
    #[cfg(not(unix))]
    let broker_port: Option<u16> = None;

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
                broker_port,
                models,
                providers,
                mcp_service,
                memory_service,
                search_settings,
                embedding_service,
                user_secrets,
                remote_agents,
                module_agents: module_agents.clone(),
                role: role.clone(),
                team: team.clone(),
                is_sub_agent: true,
                services_loaded: true,
                surface: chatty_core::services::StreamSurface::Headless,
            },
            event_tx,
        );

        engine.set_max_duration(max_duration);
        // A --headless run knows its task before its agent exists; pipe
        // and participant runs read theirs later.
        if cli.headless
            && !cli.pipe
            && !participant_mode
            && let Some(message) = cli.message.as_deref()
        {
            engine.note_task(message);
        }
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
                broker_port,
                models,
                providers: providers.clone(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: vec![],
                remote_agents,
                module_agents,
                role,
                team,
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

    // Stop serving once the turn (or the interactive session) ends; workers
    // already spawned are reaped by the runner's own `Drop`, not by this
    // (Do item 4).
    #[cfg(unix)]
    if let Some(broker) = broker {
        broker.shutdown();
    }

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

/// The role this process runs as, from `--tools` and `--preamble`
/// (ADR-0011 C11).
///
/// An unknown profile name is fatal rather than silently full-tooled: a typo
/// in a declared reviewer would otherwise hand it every tool there is, and a
/// worker that fails to start is a delegation the leader sees fail.
fn resolve_role(
    tools: Option<&str>,
    preamble: Option<&str>,
) -> Result<chatty_core::factories::AgentRole> {
    let profile = match tools {
        Some(name) => Some(chatty_core::factories::tool_profile(name).with_context(|| {
            format!(
                "--tools '{name}' is not a tool profile; valid profiles: {}",
                chatty_core::factories::tool_profile_names().join(", ")
            )
        })?),
        None => None,
    };
    Ok(chatty_core::factories::AgentRole {
        preamble: preamble.map(str::to_string),
        profile,
    })
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

fn resolve_model(query: Option<&str>, models: &ModelsModel) -> Result<ModelConfig> {
    let all_models = models.models();

    if all_models.is_empty() {
        bail!(
            "No models configured. Please configure a model in Chatty's settings first \
             (run the desktop app or edit the config files)."
        );
    }

    // Exact id, then case-insensitive name, then a substring of the model
    // identifier; without --model, the model marked default in the desktop
    // settings UI (the only place the marker can be set — falling straight
    // through to list order here meant the TUI silently ignored it, #583),
    // else the first. The rule lives in chatty-core because the broker
    // meters a worker on the endpoint of the model this will pick for it
    // (ADR-0011 C10).
    if let Some(config) = resolve_model_query(all_models, query) {
        return Ok(config.clone());
    }

    bail!(
        "Model '{}' not found. Available models:\n{}",
        query.unwrap_or_default(),
        all_models
            .iter()
            .map(|m| format!("  - {} ({})", m.name, m.id))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The tool group names recognized by --enable/--disable/--only.
const VALID_TOOL_GROUPS: &str =
    "shell, fs-read, fs-write, fetch, git, code-exec, docker-exec, ask-user";

/// Flip one named tool group on `settings`. Shared by --enable, --disable
/// and --only so the group vocabulary (and its docker-exec/code-exec
/// coupling) is defined in exactly one place.
fn set_tool_group(
    settings: &mut chatty_core::settings::models::ExecutionSettingsModel,
    name: &str,
    on: bool,
) -> Result<()> {
    match name {
        "shell" => settings.enabled = on,
        "fs-read" => settings.filesystem_read_enabled = on,
        "fs-write" => settings.filesystem_write_enabled = on,
        "fetch" => settings.fetch_enabled = on,
        "git" => settings.git_enabled = on,
        "code-exec" => settings.execute_code_enabled = on,
        "docker-exec" => {
            if on {
                // Docker execution implies code-exec is on too.
                settings.execute_code_enabled = true;
            }
            settings.docker_code_execution_enabled = on;
        }
        "ask-user" => settings.ask_user_enabled = on,
        other => bail!("Unknown tool group '{other}' (valid: {VALID_TOOL_GROUPS})"),
    }
    Ok(())
}

fn apply_tool_overrides(
    settings: &mut chatty_core::settings::models::ExecutionSettingsModel,
    enable: &[String],
    disable: &[String],
) -> Result<()> {
    for name in enable {
        set_tool_group(settings, name, true)
            .with_context(|| "invalid name in --enable".to_string())?;
    }
    for name in disable {
        set_tool_group(settings, name, false)
            .with_context(|| "invalid name in --disable".to_string())?;
    }
    Ok(())
}

/// All tool groups --only can turn off before turning the named ones back on.
const ALL_TOOL_GROUPS: &[&str] = &[
    "shell",
    "fs-read",
    "fs-write",
    "fetch",
    "git",
    "code-exec",
    "docker-exec",
    "ask-user",
];

/// Strict allow-list: turn every known group off, then turn on exactly the
/// ones named in `only`. Unlike --enable/--disable, which adjust the
/// persisted defaults, this ignores them entirely for the groups it manages.
fn apply_tool_only(
    settings: &mut chatty_core::settings::models::ExecutionSettingsModel,
    only: &[String],
) -> Result<()> {
    for group in ALL_TOOL_GROUPS {
        set_tool_group(settings, group, false)
            .expect("ALL_TOOL_GROUPS entries are always valid group names");
    }
    for name in only {
        set_tool_group(settings, name, true)
            .with_context(|| "invalid name in --only".to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tool_override_tests {
    use super::{apply_tool_only, apply_tool_overrides};
    use chatty_core::settings::models::ExecutionSettingsModel;

    #[test]
    fn enable_ask_user_turns_the_group_on() {
        let mut settings = ExecutionSettingsModel {
            ask_user_enabled: false,
            ..Default::default()
        };
        apply_tool_overrides(&mut settings, &["ask-user".to_string()], &[]).unwrap();
        assert!(settings.ask_user_enabled);
    }

    #[test]
    fn disable_ask_user_turns_the_group_off() {
        let mut settings = ExecutionSettingsModel::default();
        assert!(settings.ask_user_enabled);
        apply_tool_overrides(&mut settings, &[], &["ask-user".to_string()]).unwrap();
        assert!(!settings.ask_user_enabled);
    }

    #[test]
    fn unknown_enable_name_is_a_hard_error() {
        let mut settings = ExecutionSettingsModel::default();
        let err = format!(
            "{:#}",
            apply_tool_overrides(&mut settings, &["not-a-group".to_string()], &[]).unwrap_err()
        );
        assert!(err.contains("not-a-group"), "error was: {err}");
    }

    #[test]
    fn unknown_disable_name_is_a_hard_error() {
        let mut settings = ExecutionSettingsModel::default();
        let err = format!(
            "{:#}",
            apply_tool_overrides(&mut settings, &[], &["not-a-group".to_string()]).unwrap_err()
        );
        assert!(err.contains("not-a-group"), "error was: {err}");
    }

    #[test]
    fn docker_exec_enable_implies_code_exec() {
        let mut settings = ExecutionSettingsModel::default();
        assert!(!settings.execute_code_enabled);
        apply_tool_overrides(&mut settings, &["docker-exec".to_string()], &[]).unwrap();
        assert!(settings.execute_code_enabled);
        assert!(settings.docker_code_execution_enabled);
    }

    #[test]
    fn only_turns_off_every_group_not_named() {
        // A permissive baseline: --only must still cut it down.
        let mut settings = ExecutionSettingsModel {
            git_enabled: true,
            browser_enabled: true,
            ..Default::default()
        };
        assert!(settings.filesystem_read_enabled); // on by default

        apply_tool_only(&mut settings, &["fs-read".to_string()]).unwrap();

        assert!(settings.filesystem_read_enabled);
        assert!(!settings.filesystem_write_enabled);
        assert!(!settings.fetch_enabled);
        assert!(!settings.git_enabled);
        assert!(!settings.enabled);
        assert!(!settings.ask_user_enabled);
    }

    #[test]
    fn only_rejects_an_unknown_name() {
        let mut settings = ExecutionSettingsModel::default();
        let err = format!(
            "{:#}",
            apply_tool_only(&mut settings, &["not-a-group".to_string()]).unwrap_err()
        );
        assert!(err.contains("not-a-group"), "error was: {err}");
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

/// Normalize an `--openai-compat-url` value to the canonical form the
/// OpenRouter-shaped client this injects expects as a provider `base_url`:
/// always ending in `/v1`, with no trailing slash.
///
/// Accepts either `http://host:port` (the flag's documented form) or
/// `http://host:port/v1` (how vllm/llama.cpp document their own base URL) so
/// both discover at the same `/v1/models` endpoint and store the same
/// `base_url` for chat completions (AGE-403).
fn normalize_openai_compat_base_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let base = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    format!("{base}/v1")
}

/// Query an OpenAI-compatible server via `GET {base_url}/models`, where
/// `base_url` is already normalized to end in `/v1`
/// (see [`normalize_openai_compat_base_url`]).
async fn discover_openai_compat(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>> {
    let url = format!("{base_url}/models");
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

/// A headless run's time budget when neither --max-duration nor a turn cap
/// is given.
const DEFAULT_UNATTENDED_MAX_DURATION: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// The turn cap and time budget of a run without a human: the turn cap is
/// `--max-agent-turns`, else the team's budget, else none (`0`) -- never the
/// persisted interactive setting. The time budget is `--max-duration`, else
/// [`DEFAULT_UNATTENDED_MAX_DURATION`] when the run has no turn cap (none
/// given, or an explicit `0`), so no unattended run is unbounded; with a
/// cap and no --max-duration the run keeps its old, cap-only shape.
fn unattended_run_limits(
    turns_flag: Option<u32>,
    team_turns: Option<u32>,
    duration_flag: Option<std::time::Duration>,
) -> (u32, Option<std::time::Duration>) {
    let turns = turns_flag.or(team_turns).unwrap_or(0);
    let duration = duration_flag.or((turns == 0).then_some(DEFAULT_UNATTENDED_MAX_DURATION));
    (turns, duration)
}

/// `--max-duration`: seconds (`90`), or numbers with `s`/`m`/`h` units
/// (`45s`, `30m`, `2h`, `1h30m`). Zero is refused.
fn parse_duration(text: &str) -> Result<std::time::Duration, String> {
    let text = text.trim();
    let invalid = || format!("invalid duration '{text}': use seconds or e.g. 90s, 30m, 2h, 1h30m");
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        let secs: u64 = text.parse().map_err(|_| invalid())?;
        return match secs {
            0 => Err(format!(
                "--max-duration must be more than zero, got '{text}'"
            )),
            secs => Ok(std::time::Duration::from_secs(secs)),
        };
    }
    let mut total = 0u64;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return Err(invalid()),
        };
        let value: u64 = number.parse().map_err(|_| invalid())?;
        total = value
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(invalid)?;
        number.clear();
    }
    // `1h30` has a number with no unit; an empty text has nothing at all.
    if !number.is_empty() || text.is_empty() {
        return Err(invalid());
    }
    if total == 0 {
        return Err(format!(
            "--max-duration must be more than zero, got '{text}'"
        ));
    }
    Ok(std::time::Duration::from_secs(total))
}

#[cfg(test)]
mod resolve_model_tests {
    use super::{
        Cli, DEFAULT_UNATTENDED_MAX_DURATION, parse_duration, resolve_model, unattended_run_limits,
    };
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

        let resolved = resolve_model(cli(&[]).model.as_deref(), &models).expect("a model resolves");

        assert_eq!(resolved.id, "second");
    }

    #[test]
    fn no_model_flag_and_no_marker_still_takes_the_first_model() {
        let models = store(&["first", "second"]);

        let resolved = resolve_model(cli(&[]).model.as_deref(), &models).expect("a model resolves");

        assert_eq!(resolved.id, "first");
    }

    /// An explicit --model is the user speaking about this session; it wins
    /// over the persisted marker.
    #[test]
    fn an_explicit_model_flag_beats_the_marked_default() {
        let mut models = store(&["first", "second"]);
        assert!(models.set_default("second"));

        let resolved = resolve_model(cli(&["--model", "first"]).model.as_deref(), &models)
            .expect("a model resolves");

        assert_eq!(resolved.id, "first");
    }

    #[test]
    fn no_models_configured_is_an_error() {
        assert!(resolve_model(cli(&[]).model.as_deref(), &ModelsModel::new()).is_err());
    }

    #[test]
    fn max_duration_accepts_seconds_and_unit_suffixes() {
        let secs = |s: &str| parse_duration(s).map(|d| d.as_secs());
        assert_eq!(secs("90"), Ok(90));
        assert_eq!(secs("45s"), Ok(45));
        assert_eq!(secs("30m"), Ok(1800));
        assert_eq!(secs("2h"), Ok(7200));
        assert_eq!(secs("1h30m"), Ok(5400));
        for bad in ["", "0", "0m", "m", "1h30", "10x", "-5", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be refused");
        }
        let cli = cli(&["--headless", "-m", "hi", "--max-duration", "30m"]);
        assert_eq!(cli.max_duration, Some(std::time::Duration::from_secs(1800)));
    }

    /// A run without a human: a given cap keeps the old cap-only shape;
    /// with no cap (none given, or `0`) it gets a time budget,
    /// whatever the persisted setting says.
    #[test]
    fn unattended_runs_get_a_turn_cap_or_a_time_budget() {
        let thirty = DEFAULT_UNATTENDED_MAX_DURATION;
        let hour = std::time::Duration::from_secs(3600);
        assert_eq!(unattended_run_limits(None, None, None), (0, Some(thirty)));
        assert_eq!(unattended_run_limits(Some(50), None, None), (50, None));
        // An explicit `0` (flag or team) is no cap, never no bound at all.
        assert_eq!(
            unattended_run_limits(Some(0), None, None),
            (0, Some(thirty))
        );
        assert_eq!(
            unattended_run_limits(None, Some(0), None),
            (0, Some(thirty))
        );
        assert_eq!(
            unattended_run_limits(Some(0), Some(40), None),
            (0, Some(thirty))
        );
        assert_eq!(unattended_run_limits(None, Some(40), None), (40, None));
        assert_eq!(unattended_run_limits(Some(50), Some(40), None), (50, None));
        assert_eq!(
            unattended_run_limits(None, None, Some(hour)),
            (0, Some(hour))
        );
        assert_eq!(
            unattended_run_limits(Some(20), None, Some(hour)),
            (20, Some(hour))
        );
    }

    /// AGE-440: unset leaves `execution_settings.max_agent_turns` at
    /// whatever it already was (the persisted default, or a team's), and an
    /// explicit flag replaces it — the same shape `main` applies it in,
    /// after `--team`'s own budget.
    #[test]
    fn max_agent_turns_flag_overrides_execution_settings_when_set() {
        let mut execution_settings =
            chatty_core::settings::models::ExecutionSettingsModel::default();
        let before = execution_settings.max_agent_turns;

        if let Some(turns) = cli(&[]).max_agent_turns {
            execution_settings.max_agent_turns = turns;
        }
        assert_eq!(
            execution_settings.max_agent_turns, before,
            "no flag must not change the default"
        );

        if let Some(turns) = cli(&["--max-agent-turns", "30"]).max_agent_turns {
            execution_settings.max_agent_turns = turns;
        }
        assert_eq!(execution_settings.max_agent_turns, 30);
    }

    /// AGE-455: unset leaves a resolved model's `extra_params` untouched (the
    /// provider default applies), and an explicit `--think` writes the same
    /// `extra_params.think` field `ollama_think`/`openai_compat_think` read —
    /// the override `main` applies right after `resolve_model`.
    #[test]
    fn think_flag_overrides_model_extra_params_when_set() {
        let models = store(&["first"]);

        let mut model =
            resolve_model(cli(&[]).model.as_deref(), &models).expect("a model resolves");
        if let Some(think) = cli(&[]).think {
            model
                .extra_params
                .insert("think".to_string(), think.to_string());
        }
        assert_eq!(
            model.extra_params.get("think"),
            None,
            "no flag must not set it"
        );

        let mut model =
            resolve_model(cli(&[]).model.as_deref(), &models).expect("a model resolves");
        if let Some(think) = cli(&["--think", "false"]).think {
            model
                .extra_params
                .insert("think".to_string(), think.to_string());
        }
        assert_eq!(
            model.extra_params.get("think").map(String::as_str),
            Some("false")
        );
    }
}

#[cfg(test)]
mod cli_smoke_tests {
    use super::Cli;
    use clap::CommandFactory;
    use clap::Parser;

    /// Replaces the old CI `cargo build -p chatty-tui && ./target/debug/chatty-tui --help`
    /// step. `cargo test` already compiles this crate; a second non-test bin
    /// build was rebuilding for ~15 minutes on GitHub-hosted runners.
    #[test]
    fn help_renders_and_names_core_modes() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("chatty-tui"), "{help}");
        assert!(help.contains("--headless"), "{help}");
        assert!(help.contains("--pipe"), "{help}");
        assert!(help.contains("--broker"), "{help}");
    }

    /// AGE-376: `--broker` is valid with `--headless`, `--pipe` and the bare
    /// interactive TUI — clap must not reject any combination.
    #[test]
    fn broker_flag_parses_with_headless_pipe_and_interactive() {
        assert!(Cli::try_parse_from(["chatty-tui", "--broker"]).is_ok());
        assert!(Cli::try_parse_from(["chatty-tui", "--broker", "--headless", "-m", "hi"]).is_ok());
        assert!(Cli::try_parse_from(["chatty-tui", "--broker", "--pipe"]).is_ok());
    }

    /// AGE-407: `--team <id>` is valid with `--headless`, `--pipe` and the
    /// bare interactive TUI, and needs no `--broker` beside it.
    #[test]
    fn team_flag_parses_with_headless_pipe_and_interactive() {
        let team = |args: &[&str]| {
            let mut argv = vec!["chatty-tui", "--team", "coder-reviewer"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).expect("--team parses")
        };
        assert_eq!(team(&[]).team.as_deref(), Some("coder-reviewer"));
        assert!(
            !team(&[]).broker,
            "--broker is implied at run time, not parsed"
        );
        assert!(team(&["--headless", "-m", "hi"]).headless);
        assert!(team(&["--pipe"]).pipe);
        assert!(Cli::try_parse_from(["chatty-tui", "--team"]).is_err());
    }

    /// `--tool-loading all|dynamic`, absent by default so the persisted
    /// setting (default `all`) applies.
    #[test]
    fn tool_loading_flag_parses() {
        use chatty_core::settings::models::ToolLoading;
        let parse = |args: &[&str]| {
            let mut argv = vec!["chatty-tui"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| cli.tool_loading)
        };
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(
            parse(&["--tool-loading", "dynamic", "--headless", "-m", "hi"]).unwrap(),
            Some(ToolLoading::Dynamic)
        );
        assert_eq!(
            parse(&["--tool-loading", "all"]).unwrap(),
            Some(ToolLoading::All)
        );
        assert!(parse(&["--tool-loading", "some"]).is_err());
    }
}

#[cfg(test)]
mod openai_compat_url_tests {
    use super::normalize_openai_compat_base_url;

    /// AGE-403: discovery used to append `/v1/models` to whatever the flag
    /// was given, while the stored provider `base_url` (used for
    /// `/chat/completions`) kept the flag's value unchanged. `--openai-compat-url
    /// http://host:port` (the documented form) and `--openai-compat-url
    /// http://host:port/v1` (how vllm/llama.cpp document their own base URL)
    /// must normalize to the same `.../v1` base — used both as the stored
    /// provider `base_url` and to build the `/v1/models` discovery URL —
    /// or exactly one of the two shapes 404s.
    #[test]
    fn bare_host_and_trailing_v1_normalize_to_the_same_base_url() {
        let bare = normalize_openai_compat_base_url("http://172.17.0.1:11434");
        let with_v1 = normalize_openai_compat_base_url("http://172.17.0.1:11434/v1");

        assert_eq!(bare, with_v1);
        assert_eq!(bare, "http://172.17.0.1:11434/v1");
    }

    /// The same normalized base is what `discover_openai_compat` appends
    /// `/models` to, so the discovery URL must match too.
    #[test]
    fn bare_host_and_trailing_v1_produce_the_same_discovery_url() {
        let bare = normalize_openai_compat_base_url("http://localhost:8000");
        let with_v1 = normalize_openai_compat_base_url("http://localhost:8000/v1");

        let discovery_url_bare = format!("{bare}/models");
        let discovery_url_with_v1 = format!("{with_v1}/models");

        assert_eq!(discovery_url_bare, discovery_url_with_v1);
        assert_eq!(discovery_url_bare, "http://localhost:8000/v1/models");
    }

    /// A trailing slash on either shape must not produce a double slash or
    /// an extra `/v1`.
    #[test]
    fn trailing_slashes_are_tolerated() {
        assert_eq!(
            normalize_openai_compat_base_url("http://localhost:8000/"),
            "http://localhost:8000/v1"
        );
        assert_eq!(
            normalize_openai_compat_base_url("http://localhost:8000/v1/"),
            "http://localhost:8000/v1"
        );
    }
}
