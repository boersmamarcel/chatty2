//! Dynamic tool loading: a small core of tools up front, the rest in named
//! groups the model loads when the task needs them.
//!
//! Every tool the settings allow is still registered on the agent; what
//! changes is what each request *advertises*. With every tool advertised a
//! headless coding run carried about fifty schemas — half of a 32k context
//! before the task began — and a local model spent calls on tools it had no
//! use for. Under [`ToolLoading::Dynamic`](crate::settings::models::ToolLoading)
//! [`ToolLoader`], a hook on the agent, narrows each request to the core
//! tools plus the groups loaded so far (rig's per-call `active_tools`), and
//! `load_tools` loads a group.
//!
//! Loading is explicit and by whole group, and a loaded group never
//! unloads: the advertised set only grows, a handful of times per session at
//! most, so the provider's prompt cache (the tool block sits at the top of
//! the prompt) is invalidated once per group and never flip-flops. rig's own
//! `dynamic_tools` (RAG over tool descriptions) was the alternative: it
//! picks a different set per prompt, needs an embedding index, and can drop
//! a tool the history already called — so it was not used.
//!
//! A tool called anywhere in the history keeps its group loaded, so a
//! history persisted under a wider set (or before a rebuild) stays valid
//! against what is advertised. A call to a registered tool whose group is
//! not loaded yet loads that group and answers the call with a note to make
//! it again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::Mutex;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, InvalidToolCallAction,
    InvalidToolCallContext, RequestPatch,
};
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use rig_core::completion::Message;
use rig_core::message::{AssistantContent, UserContent};
use serde::{Deserialize, Serialize};

use crate::services::context_shaper::ContextShaper;
use crate::tools::ToolError;

/// Advertised from the first request on: what a coding task needs every
/// step — run a command, read, edit, find — plus the todo plan, skills and
/// `load_tools` itself. Registered tools outside every group (anything added
/// to the registry without a group here) are advertised too, so nothing
/// disappears silently.
pub const CORE_TOOLS: &[&str] = &[
    "shell_execute",
    "read_file",
    "write_file",
    "apply_diff",
    "search_code",
    "glob_search",
    "write_todos",
    "update_todo",
    "verify_completion",
    "read_skill",
    LoadToolsTool::NAME_STR,
];

/// The group `final_answer` is in. Loaded from the start when the run's
/// task asks for an answer file.
pub const ANSWER_GROUP: &str = "answer";

/// The group every MCP tool is in.
pub const MCP_GROUP: &str = "mcp";

/// One loadable group: its name, when to load it (one line in the system
/// prompt), and its tools.
struct GroupSpec {
    name: &'static str,
    when: &'static str,
    tools: &'static [&'static str],
}

/// The groups, in the order the catalog lists them.
const GROUPS: &[GroupSpec] = &[
    GroupSpec {
        name: "web",
        when: "when the answer needs information from the internet",
        tools: &["search_web", "fetch", "browser_use"],
    },
    GroupSpec {
        name: "files",
        when: "to list, create, move or delete files and directories, read binary files, \
               or look up where a symbol is defined",
        tools: &[
            "list_directory",
            "read_binary",
            "create_directory",
            "delete_file",
            "move_file",
            "find_files",
            "find_definition",
            "doc_retriever",
            "add_attachment",
        ],
    },
    GroupSpec {
        name: "shell",
        when: "to change the shell's working directory or environment, or check its state",
        tools: &["shell_cd", "shell_set_env", "shell_status"],
    },
    GroupSpec {
        name: "git",
        when: "to read or change git history with dedicated tools (shell_execute runs git too)",
        tools: &[
            "git_status",
            "git_diff",
            "git_log",
            "git_add",
            "git_create_branch",
            "git_switch_branch",
            "git_commit",
            "git_merge",
        ],
    },
    GroupSpec {
        name: "code",
        when: "to run code in an isolated sandbox instead of the shell",
        tools: &["execute_code", "daytona_run"],
    },
    GroupSpec {
        name: "documents",
        when: "to read or write PDF, Word or PowerPoint files, or compile Typst to PDF",
        tools: &[
            "pdf_info",
            "pdf_extract_text",
            "pdf_to_image",
            "read_docx",
            "write_docx",
            "read_pptx",
            "write_pptx",
            "compile_typst",
        ],
    },
    GroupSpec {
        name: "data",
        when: "to analyse spreadsheets or CSV/JSON/Parquet data with SQL, or draw a chart",
        tools: &[
            "read_excel",
            "write_excel",
            "edit_excel",
            "file_structure_detector",
            "profile_data",
            "describe_data",
            "query_data",
            "create_chart",
        ],
    },
    GroupSpec {
        name: "browser",
        when: "to render and inspect a local web page in a real browser",
        tools: &[
            "browser_navigate",
            "browser_snapshot",
            "browser_screenshot",
            "browser_console",
            "browser_network",
            "browser_resize",
            "browser_click",
            "browser_type",
        ],
    },
    GroupSpec {
        name: "memory",
        when: "to remember something across conversations or look up what was remembered",
        tools: &["remember", "save_skill", "search_memory"],
    },
    GroupSpec {
        name: "agents",
        when: "to hand work to another agent",
        tools: &["list_agents", "invoke_agent", "publish_wasm_module"],
    },
    GroupSpec {
        name: "user",
        when: "when only the user can settle a question before you go on",
        tools: &["ask_user"],
    },
    GroupSpec {
        name: "tools",
        when: "to list every tool and MCP service with its description",
        tools: &["list_tools", "list_mcp_services"],
    },
    GroupSpec {
        name: ANSWER_GROUP,
        when: "when the task asks for its answer in a file (answer.txt)",
        tools: &["final_answer"],
    },
];

const MCP_WHEN: &str = "to use the tools of the connected MCP servers";

/// The static group of a native tool, if it has one.
fn native_group_of(tool: &str) -> Option<&'static str> {
    GROUPS
        .iter()
        .find(|group| group.tools.contains(&tool))
        .map(|group| group.name)
}

/// A group this agent actually has: the tools of it that are registered.
#[derive(Debug, Clone)]
struct Group {
    name: &'static str,
    when: &'static str,
    tools: Vec<String>,
}

struct Inner {
    /// The groups with at least one tool on this agent, catalog order.
    groups: Vec<Group>,
    /// Names of the MCP tools (their group is [`MCP_GROUP`]).
    mcp_tools: BTreeSet<String>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    loaded: BTreeSet<&'static str>,
    /// Every tool the built agent registered, and its schema's size in
    /// tokens. Empty until [`ToolLoader::calibrate`]: until then nothing is
    /// filtered, since `active_tools` may only name registered tools.
    registered: BTreeMap<String, usize>,
    /// The context guard whose base is the preamble plus the advertised
    /// schemas, re-measured whenever a group loads.
    shaper: Option<(ContextShaper, usize)>,
}

/// The loaded-groups state of one agent and the hook that narrows its
/// requests to it. Cloning shares the state: the hook, `load_tools` and the
/// factory hold the same loader.
#[derive(Clone)]
pub struct ToolLoader {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ToolLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolLoader")
            .field("loaded", &self.loaded_groups())
            .finish_non_exhaustive()
    }
}

impl ToolLoader {
    /// A loader over the tools the agent is about to be built with
    /// (`native_tools`, then `mcp_tools`), with `preload` groups loaded
    /// from the start.
    pub fn new<'a>(
        native_tools: impl IntoIterator<Item = &'a str>,
        mcp_tools: impl IntoIterator<Item = String>,
        preload: &[&str],
    ) -> Self {
        let native: BTreeSet<&str> = native_tools.into_iter().collect();
        let mcp_tools: BTreeSet<String> = mcp_tools.into_iter().collect();
        let mut groups: Vec<Group> = GROUPS
            .iter()
            .map(|spec| Group {
                name: spec.name,
                when: spec.when,
                tools: spec
                    .tools
                    .iter()
                    .filter(|tool| native.contains(**tool))
                    .map(|tool| tool.to_string())
                    .collect(),
            })
            .filter(|group| !group.tools.is_empty())
            .collect();
        if !mcp_tools.is_empty() {
            groups.push(Group {
                name: MCP_GROUP,
                when: MCP_WHEN,
                tools: mcp_tools.iter().cloned().collect(),
            });
        }
        let loaded = groups
            .iter()
            .map(|group| group.name)
            .filter(|name| preload.contains(name))
            .collect();
        Self {
            inner: Arc::new(Inner {
                groups,
                mcp_tools,
                state: Mutex::new(State {
                    loaded,
                    ..State::default()
                }),
            }),
        }
    }

    /// The group `tool` belongs to, or `None` for a core or ungrouped tool
    /// (always advertised).
    fn group_of(&self, tool: &str) -> Option<&'static str> {
        if CORE_TOOLS.contains(&tool) {
            return None;
        }
        native_group_of(tool).or_else(|| self.inner.mcp_tools.contains(tool).then_some(MCP_GROUP))
    }

    fn is_active(&self, tool: &str, loaded: &BTreeSet<&'static str>) -> bool {
        self.group_of(tool)
            .is_none_or(|group| loaded.contains(group))
    }

    /// Whether `tool` is advertised on the first request: the system prompt
    /// describes only these.
    pub fn starts_active(&self, tool: &str) -> bool {
        self.is_active(tool, &self.inner.state.lock().loaded)
    }

    /// The names of the groups this agent has, catalog order.
    pub fn group_names(&self) -> Vec<&'static str> {
        self.inner.groups.iter().map(|group| group.name).collect()
    }

    /// The groups loaded so far.
    pub fn loaded_groups(&self) -> Vec<&'static str> {
        self.inner.state.lock().loaded.iter().copied().collect()
    }

    /// The system prompt's catalog: one line per group not loaded yet,
    /// saying when to load it. Empty when every group is loaded.
    pub fn catalog(&self) -> String {
        let loaded = self.inner.state.lock().loaded.clone();
        let lines: Vec<String> = self
            .inner
            .groups
            .iter()
            .filter(|group| !loaded.contains(group.name))
            .map(|group| {
                format!(
                    "- **{}** — load {} ({})",
                    group.name,
                    group.when,
                    group.tools.join(", ")
                )
            })
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        format!(
            "\n\n## Tool Groups\n\
             Only the core tools above are loaded. More tools come in groups: call `load_tools` \
             with a group's name and its tools are available from your next step on, for the \
             rest of the conversation. Load a group only when the task needs it:\n{}",
            lines.join("\n")
        )
    }

    /// Load `group` (a no-op if it is loaded): the names of its tools, or
    /// the error the model sees for a group this agent does not have.
    pub fn load(&self, group: &str) -> Result<Vec<String>, String> {
        let Some(found) = self.inner.groups.iter().find(|g| g.name == group) else {
            return Err(format!(
                "Unknown tool group `{group}`. The groups are: {}.",
                self.group_names().join(", ")
            ));
        };
        let newly = self.inner.state.lock().loaded.insert(found.name);
        if newly {
            tracing::info!(group = found.name, "Tool group loaded");
            self.recalibrate();
        }
        Ok(found.tools.clone())
    }

    /// Load the group of every tool called in `messages`: a call in the
    /// history is only valid against a request that still advertises it.
    fn load_groups_called_in(&self, messages: &[Message]) {
        let called: Vec<&'static str> = messages
            .iter()
            .flat_map(called_tool_names)
            .filter_map(|name| self.group_of(name))
            .collect();
        if called.is_empty() {
            return;
        }
        let changed = {
            let mut state = self.inner.state.lock();
            let mut changed = false;
            for group in called {
                changed |= state.loaded.insert(group);
            }
            changed
        };
        if changed {
            self.recalibrate();
        }
    }

    /// The registered tools to advertise now, registration order (rig keeps
    /// its own order; this is the allow-list). `None` before calibration.
    pub fn active_tools(&self) -> Option<Vec<String>> {
        let state = self.inner.state.lock();
        if state.registered.is_empty() {
            return None;
        }
        Some(
            state
                .registered
                .keys()
                .filter(|tool| self.is_active(tool, &state.loaded))
                .cloned()
                .collect(),
        )
    }

    /// Record what the built agent registered (each tool's schema size in
    /// tokens) and set `shaper`'s base to the preamble plus the schemas
    /// advertised now; loading a group later moves the base with it.
    pub fn calibrate(
        &self,
        shaper: &ContextShaper,
        preamble_tokens: usize,
        tool_tokens: BTreeMap<String, usize>,
    ) {
        {
            let mut state = self.inner.state.lock();
            state.registered = tool_tokens;
            state.shaper = Some((shaper.clone(), preamble_tokens));
        }
        self.recalibrate();
    }

    fn recalibrate(&self) {
        let state = self.inner.state.lock();
        let Some((shaper, preamble_tokens)) = state.shaper.as_ref() else {
            return;
        };
        let tools: usize = state
            .registered
            .iter()
            .filter(|(tool, _)| self.is_active(tool, &state.loaded))
            .map(|(_, tokens)| tokens)
            .sum();
        shaper.set_base_tokens(preamble_tokens + tools);
    }

    /// The group of `tool` if it is registered here but its group is not
    /// loaded yet.
    fn unloaded_group_of(&self, tool: &str) -> Option<&'static str> {
        let group = self.group_of(tool)?;
        let state = self.inner.state.lock();
        (state.registered.contains_key(tool) && !state.loaded.contains(group)).then_some(group)
    }
}

/// The tool names an assistant message calls.
fn called_tool_names(message: &Message) -> Vec<&str> {
    match message {
        Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(call) => Some(call.function.name.as_str()),
                _ => None,
            })
            .collect(),
        Message::User { content } => content
            .iter()
            .filter_map(|item| match item {
                UserContent::ToolResult(result) => Some(result.name.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl AgentHook for ToolLoader {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        self.load_groups_called_in(event.history);
        self.load_groups_called_in(std::slice::from_ref(event.prompt));
        match self.active_tools() {
            Some(active) => CompletionCallAction::patch(RequestPatch::new().active_tools(active)),
            None => CompletionCallAction::Continue,
        }
    }

    /// A call to a tool this agent has but has not loaded: load its group
    /// and tell the model to make the call again, rather than end the turn.
    /// Not on a call that advertised no tools at all (the turn budget's
    /// tool-free wrap-up): that one is the budget's to end.
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        if event.available_tools.is_empty() {
            return None;
        }
        let group = self.unloaded_group_of(&event.tool_name)?;
        self.load(group).ok()?;
        // Skip, not retry: the call is answered with this note and the run
        // goes on to its next model call, which advertises the group. rig
        // allows no invalid-call retries unless a request asks for them.
        Some(InvalidToolCallAction::skip(format!(
            "`{tool}` is in the `{group}` tool group, which was not loaded, so this call did \
             not run. The group is loaded now: make the call again.",
            tool = event.tool_name
        )))
    }
}

// ── load_tools ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
pub struct LoadToolsArgs {
    /// The group to load.
    pub group: String,
}

#[derive(Debug, Serialize)]
pub struct LoadToolsOutput {
    pub group: String,
    pub tools: Vec<String>,
    pub note: &'static str,
}

/// The tool the model loads a group with.
#[derive(Clone)]
pub struct LoadToolsTool {
    loader: ToolLoader,
}

impl LoadToolsTool {
    const NAME_STR: &'static str = "load_tools";

    pub fn new(loader: ToolLoader) -> Self {
        Self { loader }
    }
}

impl Tool for LoadToolsTool {
    const NAME: &'static str = Self::NAME_STR;
    type Error = ToolError;
    type Args = LoadToolsArgs;
    type Output = LoadToolsOutput;

    fn description(&self) -> String {
        "Load a group of tools for the rest of this conversation. The groups, and when each is \
         needed, are listed in the system prompt under Tool Groups. The group's tools can be \
         called from your next step on."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "group": {
                    "type": "string",
                    "enum": self.loader.group_names(),
                    "description": "The group to load."
                }
            },
            "required": ["group"]
        })
    }

    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let tools = self
            .loader
            .load(args.group.trim())
            .map_err(ToolError::OperationFailed)?;
        Ok(LoadToolsOutput {
            group: args.group,
            tools,
            note: "Loaded; these tools can be called from your next step on.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{ToolCall, ToolCallId, ToolFunction};

    fn loader(preload: &[&str]) -> ToolLoader {
        ToolLoader::new(
            [
                "shell_execute",
                "read_file",
                "write_file",
                "load_tools",
                "search_web",
                "fetch",
                "git_diff",
                "final_answer",
                "some_new_tool",
            ],
            ["mcp_lookup".to_string()],
            preload,
        )
    }

    fn calibrated(preload: &[&str]) -> (ToolLoader, ContextShaper) {
        let loader = loader(preload);
        let shaper = ContextShaper::new(
            Default::default(),
            crate::token_budget::counter::TokenCounter::for_model("test"),
            Some(32_000),
        );
        let tokens = [
            "shell_execute",
            "read_file",
            "write_file",
            "load_tools",
            "search_web",
            "fetch",
            "git_diff",
            "final_answer",
            "some_new_tool",
            "mcp_lookup",
        ]
        .into_iter()
        .map(|name| (name.to_string(), 100))
        .collect();
        loader.calibrate(&shaper, 1_000, tokens);
        (loader, shaper)
    }

    fn call(name: &str) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall::new(
                ToolCallId::new("call_1").unwrap(),
                ToolFunction::new(name.to_string(), serde_json::json!({})),
            ))],
        }
    }

    #[test]
    fn only_the_core_and_ungrouped_tools_start_active() {
        let (loader, _) = calibrated(&[]);
        assert_eq!(
            loader.active_tools().unwrap(),
            [
                "load_tools",
                "read_file",
                "shell_execute",
                "some_new_tool",
                "write_file"
            ]
        );
        assert!(loader.starts_active("read_file"));
        assert!(!loader.starts_active("search_web"));
        assert!(!loader.starts_active("mcp_lookup"));
    }

    #[test]
    fn a_loaded_group_is_advertised_and_stays_loaded() {
        let (loader, _) = calibrated(&[]);
        assert_eq!(loader.load("web").unwrap(), ["search_web", "fetch"]);
        let active = loader.active_tools().unwrap();
        assert!(active.contains(&"search_web".to_string()));
        assert!(active.contains(&"fetch".to_string()));
        assert!(!active.contains(&"git_diff".to_string()));

        loader.load("git").unwrap();
        loader.load("web").unwrap();
        let active = loader.active_tools().unwrap();
        assert!(active.contains(&"fetch".to_string()), "monotonic");
        assert!(active.contains(&"git_diff".to_string()));
        assert_eq!(loader.loaded_groups(), ["git", "web"]);
    }

    #[test]
    fn an_unknown_group_lists_the_groups_there_are() {
        let (loader, _) = calibrated(&[]);
        let error = loader.load("internet").unwrap_err();
        assert!(error.contains("Unknown tool group `internet`"), "{error}");
        assert!(error.contains("web, git, answer, mcp"), "{error}");
    }

    #[test]
    fn the_answer_group_can_be_preloaded() {
        let (loader, _) = calibrated(&[ANSWER_GROUP]);
        assert!(
            loader
                .active_tools()
                .unwrap()
                .contains(&"final_answer".to_string())
        );
        assert!(!loader.catalog().contains("**answer**"));
        assert!(
            loader
                .catalog()
                .contains("**web** — load when the answer needs information")
        );
    }

    #[test]
    fn a_tool_called_in_the_history_keeps_its_group_loaded() {
        let (loader, _) = calibrated(&[]);
        loader.load_groups_called_in(&[Message::user("task"), call("git_diff")]);
        assert!(
            loader
                .active_tools()
                .unwrap()
                .contains(&"git_diff".to_string())
        );
        loader.load_groups_called_in(&[call("mcp_lookup")]);
        assert!(loader.loaded_groups().contains(&MCP_GROUP));
    }

    #[test]
    fn the_context_guard_counts_only_the_advertised_schemas() {
        let (loader, shaper) = calibrated(&[]);
        let before = shaper.history_budget(0);
        loader.load("web").unwrap();
        assert_eq!(
            before - shaper.history_budget(0),
            200,
            "two more schemas of 100"
        );
    }

    #[test]
    fn nothing_is_filtered_before_calibration() {
        assert!(loader(&[]).active_tools().is_none());
    }

    // ── On rig's own loop, with a mock model ─────────────────────────────

    use futures::StreamExt;
    use rig_agent::agent::AgentBuilder;
    use rig_agent::streaming::StreamingPrompt;
    use rig_agent::test_utils::MockAddTool;
    use rig_core::completion::CompletionRequest;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    /// A stand-in for the real `fetch`, in the `web` group.
    #[derive(Clone)]
    struct FakeFetch;

    impl Tool for FakeFetch {
        const NAME: &'static str = "fetch";
        type Error = ToolError;
        type Args = serde_json::Value;
        type Output = String;

        fn description(&self) -> String {
            "fetch a URL".into()
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn call(
            &self,
            _context: &mut ToolContext,
            _args: Self::Args,
        ) -> Result<Self::Output, Self::Error> {
            Ok("page".into())
        }
    }

    fn tool_call(n: u32, name: &str, args: serde_json::Value) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::tool_call(format!("tool_call_{n}"), name, args)
                .with_call_id(format!("call_{n}")),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]
    }

    fn done() -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::text("done"),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]
    }

    /// An agent with `add` (ungrouped, so always on), `fetch` (web) and
    /// `load_tools`, its loader calibrated the way the factory does it.
    async fn mock_agent(
        turns: Vec<Vec<MockStreamEvent>>,
    ) -> (rig_agent::Agent, MockCompletionModel, ToolLoader) {
        let model = MockCompletionModel::from_stream_turns(turns);
        let loader = ToolLoader::new(["add", "fetch", "load_tools"], Vec::new(), &[]);
        let agent = AgentBuilder::new(model.clone())
            .tool(MockAddTool)
            .tool(FakeFetch)
            .tool(LoadToolsTool::new(loader.clone()))
            .add_hook(loader.clone())
            .build();
        let registered = agent
            .tool_definitions(None)
            .await
            .unwrap()
            .into_iter()
            .map(|definition| (definition.name, 10))
            .collect();
        let shaper = ContextShaper::new(
            Default::default(),
            crate::token_budget::counter::TokenCounter::for_model("test"),
            Some(32_000),
        );
        loader.calibrate(&shaper, 100, registered);
        (agent, model, loader)
    }

    fn advertised(request: &CompletionRequest) -> Vec<String> {
        let mut names: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names
    }

    async fn run(agent: &rig_agent::Agent, history: Vec<Message>) {
        let mut stream = agent
            .stream_prompt("go")
            .history(history)
            .max_turns(5)
            .await;
        while let Some(item) = stream.next().await {
            item.expect("the run succeeds");
        }
    }

    /// The first call advertises the core only; `load_tools` adds the
    /// group from the next call on, and it stays for the calls after.
    #[tokio::test]
    async fn load_tools_advertises_the_group_from_the_next_call() {
        let (agent, model, _loader) = mock_agent(vec![
            tool_call(1, "load_tools", serde_json::json!({"group": "web"})),
            tool_call(2, "fetch", serde_json::json!({})),
            done(),
        ])
        .await;

        run(&agent, Vec::new()).await;

        let requests = model.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(advertised(&requests[0]), ["add", "load_tools"]);
        assert_eq!(advertised(&requests[1]), ["add", "fetch", "load_tools"]);
        assert_eq!(advertised(&requests[2]), ["add", "fetch", "load_tools"]);
    }

    /// A history that already called `fetch` keeps `fetch` advertised.
    #[tokio::test]
    async fn an_earlier_call_in_the_history_stays_valid() {
        let (agent, model, loader) = mock_agent(vec![done()]).await;
        let earlier = vec![
            Message::user("earlier task"),
            Message::Assistant {
                id: None,
                content: vec![AssistantContent::ToolCall(ToolCall::new(
                    ToolCallId::new("call_0").unwrap(),
                    ToolFunction::new("fetch".into(), serde_json::json!({})),
                ))],
            },
            Message::User {
                content: vec![UserContent::ToolResult(rig_core::message::ToolResult {
                    call: ToolCallId::new("call_0").unwrap(),
                    name: "fetch".into(),
                    content: vec![rig_core::message::ToolResultContent::text("page")],
                    provider: None,
                })],
            },
            Message::assistant("fetched"),
        ];

        run(&agent, earlier).await;

        assert_eq!(
            advertised(&model.requests()[0]),
            ["add", "fetch", "load_tools"]
        );
        assert_eq!(loader.loaded_groups(), ["web"]);
    }

    /// Calling a registered tool whose group is not loaded loads the group
    /// and lets the model call it again, instead of ending the turn.
    #[tokio::test]
    async fn a_call_to_an_unloaded_tool_loads_its_group_and_retries() {
        let (agent, model, loader) = mock_agent(vec![
            tool_call(1, "fetch", serde_json::json!({})),
            tool_call(2, "fetch", serde_json::json!({})),
            done(),
        ])
        .await;

        run(&agent, Vec::new()).await;

        let requests = model.requests();
        assert_eq!(advertised(&requests[0]), ["add", "load_tools"]);
        assert_eq!(advertised(&requests[1]), ["add", "fetch", "load_tools"]);
        assert_eq!(loader.loaded_groups(), ["web"]);
    }

    /// `load_tools` with a group that does not exist fails with the list
    /// of groups there are; the schema names them too.
    #[tokio::test]
    async fn load_tools_rejects_an_unknown_group_with_the_list() {
        let (_agent, _model, loader) = mock_agent(Vec::new()).await;
        let tool = LoadToolsTool::new(loader);
        let error = tool
            .call(
                &mut ToolContext::new(),
                LoadToolsArgs {
                    group: "internet".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unknown tool group `internet`. The groups are: web."
        );
        assert_eq!(
            tool.parameters()["properties"]["group"]["enum"],
            serde_json::json!(["web"])
        );
    }
}
