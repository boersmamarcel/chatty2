//! Calls to tool names the agent does not have, answered inside the turn.
//!
//! Local models reach for the names they were trained on — `grep_code`,
//! `cat`, `bash` — rather than the ones this agent registers. rig fails such
//! a call with `UnknownToolCall`, which ended the turn and cost a whole
//! corrective follow-up (AGE-497). [`ToolNameRepair`] resolves it where rig
//! raises it instead: a known alias of a tool this turn allows is renamed to
//! it and runs; any other name is answered with the nearest real tool names,
//! and the run goes on.

use rig_agent::agent::{AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext};

/// Names models use for tools this agent has under another name, with the
/// tool each one means.
const ALIASES: &[(&str, &str)] = &[
    ("grep_code", "search_code"),
    ("grep", "search_code"),
    ("rg", "search_code"),
    ("ripgrep", "search_code"),
    ("code_search", "search_code"),
    ("cat", "read_file"),
    ("read", "read_file"),
    ("open_file", "read_file"),
    ("view_file", "read_file"),
    ("ls", "list_directory"),
    ("list_dir", "list_directory"),
    ("list_files", "list_directory"),
    ("glob", "glob_search"),
    ("bash", "shell_execute"),
    ("sh", "shell_execute"),
    ("shell", "shell_execute"),
    ("run_command", "shell_execute"),
    ("execute_command", "shell_execute"),
    ("web_search", "search_web"),
    ("fetch_url", "fetch"),
    ("create_file", "write_file"),
];

/// The parameters of each alias target, `(tool, required, optional)`. An
/// alias is only renamed when the call's arguments fit its target: the
/// rename keeps the arguments as emitted, and `grep {pattern, path}` run as
/// `search_code` would silently drop the path and search everything.
const TARGET_PARAMS: &[(&str, &[&str], &[&str])] = &[
    (
        "search_code",
        &["pattern"],
        &["case_insensitive", "file_type", "max_results"],
    ),
    ("read_file", &["path"], &["start_line", "end_line"]),
    ("list_directory", &["path"], &[]),
    ("glob_search", &["pattern"], &[]),
    ("shell_execute", &["command"], &["timeout_seconds"]),
    ("search_web", &["query"], &["max_results"]),
    (
        "fetch",
        &["url"],
        &["max_length", "start_index", "find", "user_agent"],
    ),
    ("write_file", &["path", "content"], &[]),
];

/// How many real tool names an unknown call is answered with.
const SUGGESTIONS: usize = 3;

/// What to do with a call to `name`, given the tools this turn allows.
#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    /// Run it as this allowed tool.
    Rename(String),
    /// An alias of this allowed tool, called with arguments it does not
    /// take: answer with the tool and its parameters instead of running it.
    Retarget { tool: String, params: String },
    /// No tool it clearly means: these are the closest allowed names.
    Suggest(Vec<String>),
}

/// `name` as the tool it most likely means: itself once case, `-` for `_`
/// and a `functions.` style namespace are normalised away, else a known
/// alias whose target takes `args` — either only when this turn allows the
/// result.
fn resolve(name: &str, args: Option<&str>, allowed: &[String]) -> Resolution {
    let bare = name.rsplit(['.', ':', '/']).next().unwrap_or(name);
    let normal = bare.trim().to_ascii_lowercase().replace('-', "_");
    if normal != name && allowed.contains(&normal) {
        return Resolution::Rename(normal);
    }
    let alias = ALIASES
        .iter()
        .find(|(alias, _)| *alias == normal)
        .map(|(_, tool)| (*tool).to_string());
    if let Some(tool) = alias.filter(|tool| allowed.contains(tool)) {
        let Some((_, required, optional)) = TARGET_PARAMS.iter().find(|(t, _, _)| *t == tool)
        else {
            return Resolution::Rename(tool);
        };
        if args_fit(args, required, optional) {
            return Resolution::Rename(tool);
        }
        let params = required
            .iter()
            .map(|p| format!("`{p}` (required)"))
            .chain(optional.iter().map(|p| format!("`{p}`")))
            .collect::<Vec<_>>()
            .join(", ");
        return Resolution::Retarget { tool, params };
    }

    let mut ranked: Vec<(usize, &String)> = allowed
        .iter()
        .map(|tool| (distance(&normal, tool), tool))
        .collect();
    ranked.sort();
    Resolution::Suggest(
        ranked
            .into_iter()
            .take(SUGGESTIONS)
            .map(|(_, tool)| tool.clone())
            .collect(),
    )
}

/// Whether the emitted `args` (a JSON object, or nothing) carry every
/// `required` key and no key outside `required` and `optional`.
fn args_fit(args: Option<&str>, required: &[&str], optional: &[&str]) -> bool {
    let object = match args.map(str::trim).filter(|a| !a.is_empty()) {
        None => serde_json::Map::new(),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(object)) => object,
            _ => return false,
        },
    };
    required.iter().all(|key| object.contains_key(*key))
        && object
            .keys()
            .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
}

/// Levenshtein distance, lowered for a name that contains the other or
/// shares a `_`-separated word with it (`grep_code` → `search_code`).
fn distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let mut row: Vec<usize> = (0..=a_chars.len()).collect();
    for (i, cb) in b.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, ca) in a_chars.iter().enumerate() {
            let substitute = previous + usize::from(*ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(previous + 1);
        }
    }
    let edits = row[a_chars.len()];
    let shares_word = a
        .split('_')
        .any(|word| word.len() > 2 && b.split('_').any(|other| other == word));
    if a.contains(b) || b.contains(a) || shares_word {
        edits / 2
    } else {
        edits
    }
}

/// The hook: see the module docs. Goes after the tool loader, which answers
/// a call to a registered tool whose group is not loaded yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolNameRepair;

impl AgentHook for ToolNameRepair {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        // A call that advertised no tools (the turn budget's wrap-up) is the
        // budget's to end.
        if event.available_tools.is_empty() || event.allowed_tools.is_empty() {
            return None;
        }
        match resolve(
            &event.tool_name,
            event.args.as_deref(),
            &event.allowed_tools,
        ) {
            Resolution::Rename(tool) => {
                tracing::info!(called = %event.tool_name, tool = %tool, "Renaming a call to an aliased tool name");
                Some(InvalidToolCallAction::repair(tool))
            }
            Resolution::Retarget { tool, params } => {
                tracing::warn!(called = %event.tool_name, tool = %tool, "Aliased tool name called with arguments its tool does not take");
                Some(InvalidToolCallAction::skip(format!(
                    "There is no tool named `{called}`, so this call did not run. The tool for \
                     this is `{tool}`, which takes: {params}. Call `{tool}` with those arguments.",
                    called = event.tool_name,
                )))
            }
            Resolution::Suggest(names) => {
                tracing::warn!(called = %event.tool_name, "Unknown tool call; answering with the nearest tool names");
                Some(InvalidToolCallAction::skip(format!(
                    "There is no tool named `{called}`, so this call did not run. The closest \
                     tools are: {names}. Call one of those by its exact name (check its \
                     parameters), or pick another tool from your list.",
                    called = event.tool_name,
                    names = names
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn aliases_rename_to_an_allowed_tool() {
        let tools = allowed(&[
            "search_code",
            "read_file",
            "list_directory",
            "shell_execute",
        ]);
        for (called, args, tool) in [
            ("grep_code", r#"{"pattern":"fn main"}"#, "search_code"),
            (
                "rg",
                r#"{"pattern":"x","case_insensitive":true}"#,
                "search_code",
            ),
            ("cat", r#"{"path":"README.md"}"#, "read_file"),
            ("ls", r#"{"path":"."}"#, "list_directory"),
            ("bash", r#"{"command":"pwd"}"#, "shell_execute"),
            ("Read_File", r#"{"whatever":1}"#, "read_file"),
            ("functions.read-file", "", "read_file"),
        ] {
            assert_eq!(
                resolve(called, Some(args), &tools),
                Resolution::Rename(tool.to_string()),
                "{called}"
            );
        }
    }

    #[test]
    fn an_alias_of_a_tool_this_turn_lacks_is_only_suggested() {
        let tools = allowed(&["find_files", "read_file", "glob_search", "write_file"]);
        let Resolution::Suggest(names) = resolve("grep_code", Some(r#"{"pattern":"x"}"#), &tools)
        else {
            panic!("search_code is not allowed, so nothing to rename to");
        };
        assert_eq!(names.len(), SUGGESTIONS);
    }

    /// The rename keeps the emitted arguments, so an alias whose arguments
    /// its target does not take is answered with the target's parameters.
    #[test]
    fn an_alias_with_arguments_its_tool_lacks_is_retargeted_not_run() {
        let tools = allowed(&["search_code", "read_file", "shell_execute"]);
        for (called, args, tool) in [
            ("grep", r#"{"pattern":"x","path":"src"}"#, "search_code"),
            ("cat", r#"{"file_path":"a.txt"}"#, "read_file"),
            ("bash", r#"{"cmd":"ls"}"#, "shell_execute"),
            ("bash", "", "shell_execute"),
            ("bash", "[1]", "shell_execute"),
        ] {
            match resolve(called, Some(args), &tools) {
                Resolution::Retarget { tool: t, params } => {
                    assert_eq!(t, tool, "{called} {args}");
                    assert!(params.contains("(required)"), "{params}");
                }
                other => panic!("{called} {args}: {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_names_suggest_the_nearest_tools() {
        let tools = allowed(&[
            "search_code",
            "search_web",
            "read_file",
            "fetch",
            "git_diff",
        ]);
        let Resolution::Suggest(names) = resolve("search_codebase", None, &tools) else {
            panic!("no alias");
        };
        assert_eq!(names[0], "search_code");
        let Resolution::Suggest(names) = resolve("read_files", None, &tools) else {
            panic!("no alias");
        };
        assert_eq!(names[0], "read_file");
    }

    // ── On rig's own loop, with a mock model ─────────────────────────────

    use crate::tools::ToolError;
    use futures::StreamExt;
    use rig_agent::agent::AgentBuilder;
    use rig_agent::streaming::StreamingPrompt;
    use rig_agent::tool::{Tool, ToolContext};
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct FakeSearchCode(Arc<AtomicUsize>);

    impl Tool for FakeSearchCode {
        const NAME: &'static str = "search_code";
        type Error = ToolError;
        type Args = serde_json::Value;
        type Output = String;

        fn description(&self) -> String {
            "search the code".into()
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn call(
            &self,
            _context: &mut ToolContext,
            _args: Self::Args,
        ) -> Result<Self::Output, Self::Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("match".into())
        }
    }

    fn tool_call(name: &str, args: serde_json::Value) -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::tool_call("tool_call_1", name, args).with_call_id("call_1"),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]
    }

    fn done() -> Vec<MockStreamEvent> {
        vec![
            MockStreamEvent::text("done"),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]
    }

    async fn run(called: &str, args: serde_json::Value) -> (MockCompletionModel, usize) {
        let model = MockCompletionModel::from_stream_turns(vec![tool_call(called, args), done()]);
        let search = FakeSearchCode::default();
        let agent = AgentBuilder::new(model.clone())
            .tool(search.clone())
            .add_hook(ToolNameRepair)
            .build();
        let mut stream = agent.stream_prompt("go").max_turns(5).await;
        while let Some(item) = stream.next().await {
            item.expect("the run goes on past the unknown name");
        }
        (model, search.0.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn an_aliased_call_runs_the_real_tool() {
        let (model, calls) = run("grep_code", serde_json::json!({"pattern": "fn main"})).await;
        assert_eq!(calls, 1);
        assert_eq!(model.requests().len(), 2);
    }

    #[tokio::test]
    async fn an_aliased_call_with_foreign_arguments_is_answered_not_run() {
        let (model, calls) = run("grep", serde_json::json!({"pattern": "x", "path": "src"})).await;
        assert_eq!(calls, 0);
        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        let history = format!("{:?}", requests[1].chat_history);
        assert!(
            history.contains("The tool for this is `search_code`"),
            "{history}"
        );
    }

    #[tokio::test]
    async fn an_unknown_call_is_answered_and_the_turn_goes_on() {
        let (model, calls) = run("frobnicate", serde_json::json!({})).await;
        assert_eq!(calls, 0);
        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        let history = format!("{:?}", requests[1].chat_history);
        assert!(history.contains("no tool named `frobnicate`"), "{history}");
        assert!(history.contains("`search_code`"), "{history}");
    }
}
