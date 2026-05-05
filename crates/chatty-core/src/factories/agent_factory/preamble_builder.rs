use crate::settings::models::providers_store::ProviderType;
use crate::settings::models::search_settings::SearchSettingsModel;

use super::mcp_helpers::McpTools;
use super::tool_registry::ToolAvailability;

/// Build the augmented preamble with tool summary, formatting guide,
/// memory instructions, and secret key names.
pub(super) fn build_preamble(
    base_preamble: &str,
    provider_type: &ProviderType,
    tools: &ToolAvailability,
    search_settings: &Option<SearchSettingsModel>,
    mcp_mgmt_tools: &McpTools,
    mcp_tool_info: &[(String, String, String)],
    secret_key_names: &[String],
) -> String {
    let mut tool_sections: Vec<String> = Vec::new();

    if tools.fetch || tools.search_web {
        let has_search_api = tools.search_web
            && search_settings.as_ref().is_some_and(|s| {
                use crate::settings::models::search_settings::SearchProvider;
                let key = match s.active_provider {
                    SearchProvider::Tavily => &s.tavily_api_key,
                    SearchProvider::Brave => &s.brave_api_key,
                };
                key.as_ref().is_some_and(|k| !k.is_empty())
            });
        let search_note = if has_search_api {
            "search API"
        } else {
            "DuckDuckGo fallback"
        };
        tool_sections.push(format!(
            "- **search_web** ({search_note}), **fetch** (read any URL)"
        ));
    }
    if tools.shell {
        tool_sections.push(
            "- **shell_execute / shell_cd / shell_set_env / shell_status** \
             (persistent session; prefer over asking the user to run commands; for multi-line Python or shell logic, prefer writing a script via here-doc / temp file and running it instead of `python -c '...'` one-liners)"
                .to_string(),
        );
    }
    if tools.fs_read {
        tool_sections.push(
            "- **read_file / read_binary / list_directory / glob_search** \
             (for large files, prefer `read_file` with `start_line` / `end_line` instead of reading the whole file)"
                .to_string(),
        );
    }
    if tools.fs_write {
        tool_sections.push(
            "- **write_file / apply_diff / create_directory / delete_file / move_file** \
             (use apply_diff for targeted edits; avoid huge full-file rewrites in tool arguments when a targeted diff or in-place shell script will do)"
                .to_string(),
        );
    }
    if tools.search {
        tool_sections.push("- **search_code / find_files / find_definition**".to_string());
    }
    if tools.git {
        tool_sections.push(
            "- **git_status / git_diff / git_log / git_add / git_commit / \
             git_create_branch / git_switch_branch**"
                .to_string(),
        );
    }
    if tools.add_attachment {
        tool_sections.push("- **add_attachment** (display image or PDF inline)".to_string());
    }
    // Chart tool is always available (no filesystem/service dependencies)
    tool_sections.push("- **create_chart** (bar, line, pie, donut, area, candlestick)".to_string());
    if tools.compile_typst {
        tool_sections.push("- **compile_typst** (Typst markup → PDF)".to_string());
    }
    if tools.excel_read || tools.excel_write {
        let mut excel_desc = Vec::new();
        if tools.excel_read {
            excel_desc.push("**read_excel**");
        }
        if tools.excel_write {
            excel_desc.push("**write_excel** / **edit_excel**");
        }
        tool_sections.push(format!("- {} (.xlsx, .xls, .ods)", excel_desc.join(" / ")));
    }
    if tools.pdf_to_image || tools.pdf_info || tools.pdf_extract_text {
        let mut pdf_names = Vec::new();
        if tools.pdf_info {
            pdf_names.push("`pdf_info`");
        }
        if tools.pdf_extract_text {
            pdf_names.push("`pdf_extract_text`");
        }
        if tools.pdf_to_image {
            pdf_names.push("`pdf_to_image`");
        }
        tool_sections.push(format!("- **PDF tools**: {}", pdf_names.join(", ")));
    }
    if tools.data_query {
        tool_sections.push(
            "- **query_data / describe_data** (SQL via DuckDB on Parquet/CSV/JSON)".to_string(),
        );
    }
    if mcp_mgmt_tools.is_enabled() {
        tool_sections.push("- **list_mcp_services**".to_string());
    }
    // list_agents + invoke_agent are always present
    tool_sections
        .push("- **list_agents** / **invoke_agent** (discover and call agents)".to_string());
    if tools.execute_code {
        tool_sections.push(
            "- **execute_code** (isolated sandbox; Python may use Monty or Docker, other languages use Docker)".to_string(),
        );
    }
    if tools.memory {
        tool_sections.push(
            "- **remember** / **save_skill** / **search_memory** (persistent cross-conversation memory)"
                .to_string(),
        );
    }
    if tools.sub_agent {
        tool_sections.push(
            "- **sub_agent** (delegate tasks to an independent sub-agent with the same tools)"
                .to_string(),
        );
    }
    if tools.browser_use {
        tool_sections
            .push("- **browser_use** (automate browser tasks via natural language)".to_string());
    }
    if tools.daytona {
        tool_sections
            .push("- **daytona_run** (execute code in an isolated cloud sandbox)".to_string());
    }
    if tools.publish_module {
        tool_sections
            .push("- **publish_wasm_module** (publish WASM module to hive registry)".to_string());
    }
    // read_skill is always present
    tool_sections
        .push("- **read_skill** (load full skill instructions before executing)".to_string());
    // Always present
    tool_sections
        .push("- **list_tools** (get full tool list with descriptions at any time)".to_string());

    // Add MCP tools to the tool summary
    if !mcp_tool_info.is_empty() {
        let mut mcp_section = String::from("- **MCP tools** (from connected servers):\n");
        for (server_name, tool_name, tool_desc) in mcp_tool_info {
            mcp_section.push_str(&format!("  - `{tool_name}` ({server_name}): {tool_desc}\n"));
        }
        tool_sections.push(mcp_section);
    }

    let tool_summary = if tool_sections.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n## Available Tools\n\
             Use tools proactively instead of asking the user to do things manually. \
             When a task requires multiple steps, execute them yourself by chaining \
             tool calls rather than listing instructions for the user. \
             For large file edits, prefer targeted diffs or shell scripts that edit \
             files in place instead of emitting very large inline file contents in a \
             tool call. \
             Each tool's full schema is provided separately; here is a quick reference:\n\n{}",
            tool_sections.join("\n")
        )
    };

    // Formatting capabilities the app always renders, regardless of tool settings.
    let formatting_guide = "\n\n## Formatting Capabilities\n\
         \n### Math (Typst/LaTeX)\n\
         The app renders math expressions natively. Use any of these delimiters:\n\
         - Inline math: `$...$` or `\\(...\\)` — e.g. `$E = mc^2$`\n\
         - Block (display) math: `$$...$$` on its own line, `\\[...\\]`, or fenced blocks \
         with ` ```math ` or ` ```latex `\n\
         - LaTeX environments: `\\begin{equation}`, `\\begin{align}`, `\\begin{gather}`, \
         `\\begin{matrix}`, `\\begin{cases}`, and all standard starred variants\n\
         Math is compiled via MiTeX → Typst → SVG and cached; use standard LaTeX notation freely.\n\
         \n### Mermaid Diagrams\n\
         The app renders Mermaid diagrams natively in ` ```mermaid ` fenced code blocks.\n\
         Supported diagram types: flowchart, sequence, class, state, ER, gantt, pie, mindmap, \
         timeline, git graph, C4, architecture, and more.\n\
         Use mermaid diagrams to visualize workflows, architectures, relationships, and processes.\n\
         Example: ` ```mermaid\\nflowchart TD\\n  A[Start] --> B{Decision}\\n  B -->|Yes| C[OK]\\n  B -->|No| D[Cancel]\\n``` `\n\
         \n### Thinking / Reasoning\n\
         Wrap internal reasoning in `<thinking>...</thinking>`, `<think>...</think>`, or `<thought>...</thought>` tags. \
         The app renders these as a visually distinct, collapsible block so the user can inspect \
         your reasoning without it cluttering the main response. \
         Use thinking blocks for multi-step reasoning, planning, or working through a problem \
         before giving a final answer.";

    // Memory recall instructions — only if memory tools are available.
    let memory_instructions = if tools.memory {
        "\n\n## Memory\
             You have persistent memory that survives across conversations and app restarts. \
             Use `search_memory` when you need to recall facts, decisions, user preferences, \
             or context from past conversations that is not already in the current conversation \
             history. Call it proactively whenever a question might benefit from stored context \
             (e.g., user preferences, prior decisions, project-specific conventions).\
\
             When the user explicitly asks you to remember, store, note, or keep in mind \
             any information, you MUST invoke the `remember` tool with the information as \
             the content parameter. Responding with text like \"I'll remember that\" or \
             \"I've noted that\" without calling the tool means the information is lost \
             permanently. Always call the tool FIRST, then confirm to the user.\n\n\
             **Saving skills**: After successfully solving a new type of multi-step task \
             (deployment, data analysis, build process, API integration, etc.), consider \
             using `save_skill` to record the steps as a reusable procedure. Saved skills \
             live in persistent memory and can be found again with `search_memory`. \
             Only save skills for tasks with clear, reproducible steps — not one-off actions. \n\n\
             **Python in skills**: When following a skill that needs Python package management in \
             the shell, prefer `uv` over `pip`. If the `execute_code` tool is available and an \
             isolated environment is helpful, prefer `execute_code` for the Python run — it may \
             use Monty for simple snippets or Docker for fuller environments.\n\n\
             **Keyword-rich storage**: Memory search uses keyword matching (not semantic similarity). \
             When storing a memory, always include synonyms, related terms, and category words \
             so the memory can be found by different search terms. For example, if the user says \
             \"I like bananas\", store: \"User likes bananas. Categories: fruit, food, preference.\" \
             This ensures a future search for \"fruit\" or \"food preferences\" will find this memory."
    } else {
        ""
    };

    // Skills instructions — always injected because read_skill is always available.
    let skills_instructions = "\n\n## Skills\
         Use `search_memory` to discover relevant skills when a task might benefit from a \
         saved procedure. The search results will show skill names and short descriptions. \
         Call `read_skill` with the exact skill name before executing any skill procedure \
         so you have the complete, up-to-date steps.";

    let mut p = if base_preamble.trim().is_empty() {
        default_system_prompt(provider_type)
    } else {
        base_preamble.to_string()
    };
    p.push_str(&tool_summary);
    p.push_str(formatting_guide);
    p.push_str(memory_instructions);
    p.push_str(skills_instructions);
    if !secret_key_names.is_empty() {
        p.push_str(&format!(
            "\n\nThe following environment variables with sensitive information are \
             pre-loaded in the shell session: {}. When generating code that needs \
             these values, access them directly (e.g., os.environ[\"KEY\"] in Python, \
             $KEY in bash). Do not ask the user to provide these values.",
            secret_key_names.join(", ")
        ));
    }
    p
}

fn default_system_prompt(provider_type: &ProviderType) -> String {
    let provider_name = provider_type.display_name();
    let provider_specific_guidance = match provider_type {
        ProviderType::OpenRouter | ProviderType::AzureOpenAI => {
            "Prefer concise structured markdown and explicit assumptions for technical tasks. \
             For multi-step tasks, state a brief numbered plan before executing it."
        }
        ProviderType::Ollama => {
            "Keep responses direct and efficient, and verify important details with tools \
             when available. Prefer explicit step-by-step prose over open-ended descriptions."
        }
    };

    format!(
        "<identity>\n\
You are Chatty, a capable and trustworthy AI assistant inside the Chatty desktop app.\n\
</identity>\n\
\n\
<provider_context>\n\
Current model provider: {provider_name}.\n\
{provider_specific_guidance}\n\
</provider_context>\n\
\n\
<agentic_behavior>\n\
Use available tools proactively. When a task requires multiple steps, execute them yourself by \
chaining tool calls rather than listing instructions for the user. If shell or filesystem tools \
are available, run commands and read files directly — do not ask the user to do things you can \
do yourself. Prefer doing over describing.\n\
</agentic_behavior>\n\
\n\
<clarification_policy>\n\
Ask for clarification only when a missing detail genuinely blocks you from completing the task. \
Otherwise, proceed and begin your response with a brief \"Assuming [X] — let me know if that's \
wrong.\" If you must ask, ask at most one focused question per response.\n\
</clarification_policy>\n\
\n\
<objectivity>\n\
Prioritize accuracy over agreement. Do not open responses with empty validation like \
\"Great question!\", \"Absolutely!\", or \"You're right!\" Correct factual errors respectfully \
but directly. Distinguish clearly between established fact, your best estimate, and genuine \
uncertainty.\n\
</objectivity>\n\
\n\
<engineering_standards>\n\
When working in a codebase: follow the conventions already present — naming, formatting, \
patterns, and style. Verify that a library or framework is actually used in the project \
(check Cargo.toml, package.json, requirements.txt, etc.) before importing it. After making \
changes, run the relevant lints or tests. Never suppress warnings, disable type checks, or \
bypass safety checks unless the user explicitly instructs it.\n\
</engineering_standards>\n\
\n\
<formatting>\n\
Match format to content. Use prose for conversational replies, explanations, and single-topic \
answers. Reserve bullet points and numbered lists for genuinely enumerable or sequential items. \
Avoid headers and heavy markdown structure for short responses. Bold text sparingly — only for \
truly critical terms. No emoji unless the user uses them first.\n\
</formatting>\n\
\n\
<refusal_handling>\n\
Refuse requests that are unsafe, illegal, or meaningfully harmful. When refusing, be brief, \
respectful, and provide a safer alternative when possible.\n\
</refusal_handling>\n\
\n\
<wellbeing>\n\
Avoid escalating distress, manipulation, or harmful dependency. Encourage healthy, \
reality-based next steps when users appear vulnerable.\n\
</wellbeing>\n\
\n\
<evenhandedness>\n\
Present balanced, evidence-aware perspectives on disputed topics. Distinguish facts, \
uncertainty, and opinion.\n\
</evenhandedness>\n\
\n\
<knowledge_cutoff>\n\
Knowledge limits vary by provider and model. If asked about your cutoff and you do not have a \
reliable date in context, say that the cutoff is provider/model-dependent and unknown \
in-session, then use available tools for current or uncertain information.\n\
</knowledge_cutoff>"
    )
}

#[cfg(test)]
mod tests {
    use super::super::mcp_helpers::McpTools;
    use super::super::tool_registry::ToolAvailability;
    use super::*;

    fn default_preamble_args() -> (
        ProviderType,
        ToolAvailability,
        Option<SearchSettingsModel>,
        McpTools,
        Vec<(String, String, String)>,
        Vec<String>,
    ) {
        (
            ProviderType::OpenRouter,
            ToolAvailability::default(),
            None,
            McpTools::none(),
            vec![],
            vec![],
        )
    }

    #[test]
    fn empty_tools_still_includes_chart_and_formatting() {
        let (provider, tools, search, mcp, mcp_info, secrets) = default_preamble_args();
        let result = build_preamble(
            "Base prompt.",
            &provider,
            &tools,
            &search,
            &mcp,
            &mcp_info,
            &secrets,
        );
        assert!(result.starts_with("Base prompt."));
        assert!(result.contains("create_chart"));
        assert!(result.contains("## Formatting Capabilities"));
        assert!(result.contains("Math (Typst/LaTeX)"));
        assert!(result.contains("Mermaid Diagrams"));
    }

    #[test]
    fn shell_tools_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.shell = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("shell_execute"));
        assert!(result.contains("shell_cd"));
        assert!(result.contains("shell_set_env"));
        assert!(result.contains("shell_status"));
    }

    #[test]
    fn fs_tools_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.fs_read = true;
        tools.fs_write = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("read_file"));
        assert!(result.contains("write_file"));
        assert!(result.contains("apply_diff"));
    }

    #[test]
    fn git_tools_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.git = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("git_status"));
        assert!(result.contains("git_diff"));
        assert!(result.contains("git_commit"));
    }

    #[test]
    fn memory_section_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.memory = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("## Memory"));
        assert!(result.contains("remember"));
        assert!(result.contains("save_skill"));
        assert!(result.contains("search_memory"));
    }

    #[test]
    fn memory_section_excluded_when_disabled() {
        let tools = ToolAvailability::default();
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(!result.contains("## Memory"));
    }

    #[test]
    fn secret_keys_appended() {
        let tools = ToolAvailability::default();
        let secrets = vec!["API_KEY".to_string(), "DB_PASSWORD".to_string()];
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &secrets,
        );
        assert!(result.contains("API_KEY"));
        assert!(result.contains("DB_PASSWORD"));
        assert!(result.contains("environment variables"));
    }

    #[test]
    fn secret_keys_not_appended_when_empty() {
        let tools = ToolAvailability::default();
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(!result.contains("environment variables with sensitive"));
    }

    #[test]
    fn mcp_tool_info_included() {
        let tools = ToolAvailability::default();
        let mcp_info = vec![(
            "my-server".to_string(),
            "my_tool".to_string(),
            "does something".to_string(),
        )];
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &mcp_info,
            &[],
        );
        assert!(result.contains("MCP tools"));
        assert!(result.contains("my_tool"));
        assert!(result.contains("my-server"));
    }

    #[test]
    fn excel_section_shows_read_only_when_write_disabled() {
        let mut tools = ToolAvailability::default();
        tools.excel_read = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("read_excel"));
        assert!(!result.contains("write_excel"));
    }

    #[test]
    fn excel_section_shows_both_when_both_enabled() {
        let mut tools = ToolAvailability::default();
        tools.excel_read = true;
        tools.excel_write = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("read_excel"));
        assert!(result.contains("write_excel"));
        assert!(result.contains("edit_excel"));
    }

    #[test]
    fn pdf_tools_section_included() {
        let mut tools = ToolAvailability::default();
        tools.pdf_info = true;
        tools.pdf_extract_text = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("pdf_info"));
        assert!(result.contains("pdf_extract_text"));
    }

    #[test]
    fn data_query_section_included() {
        let mut tools = ToolAvailability::default();
        tools.data_query = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("query_data"));
        assert!(result.contains("describe_data"));
    }

    #[test]
    fn sub_agent_section_included() {
        let mut tools = ToolAvailability::default();
        tools.sub_agent = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("sub_agent"));
        assert!(result.contains("delegate"));
    }

    #[test]
    fn skills_section_always_present() {
        let tools = ToolAvailability::default();
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("## Skills"));
        assert!(result.contains("read_skill"));
    }

    #[test]
    fn search_web_with_fetch_shows_web_section() {
        let mut tools = ToolAvailability::default();
        tools.fetch = true;
        tools.search_web = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("search_web"));
        assert!(result.contains("fetch"));
    }

    #[test]
    fn compile_typst_section_included() {
        let mut tools = ToolAvailability::default();
        tools.compile_typst = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("compile_typst"));
        assert!(result.contains("Typst markup"));
    }

    #[test]
    fn execute_code_section_included() {
        let mut tools = ToolAvailability::default();
        tools.execute_code = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("execute_code"));
        assert!(result.contains("Monty or Docker"));
    }

    #[test]
    fn browser_use_section_included() {
        let mut tools = ToolAvailability::default();
        tools.browser_use = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("browser_use"));
    }

    #[test]
    fn daytona_section_included() {
        let mut tools = ToolAvailability::default();
        tools.daytona = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("daytona_run"));
    }

    #[test]
    fn publish_module_section_included() {
        let mut tools = ToolAvailability::default();
        tools.publish_module = true;
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("publish_wasm_module"));
    }

    #[test]
    fn default_prompt_is_used_when_base_is_empty() {
        let tools = ToolAvailability::default();
        let result = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(result.contains("<identity>"));
        assert!(result.contains("Current model provider: OpenRouter."));
    }

    #[test]
    fn default_prompt_is_dynamic_for_provider() {
        let tools = ToolAvailability::default();
        let openrouter = build_preamble(
            "",
            &ProviderType::OpenRouter,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        let ollama = build_preamble(
            "",
            &ProviderType::Ollama,
            &tools,
            &None,
            &McpTools::none(),
            &[],
            &[],
        );
        assert!(openrouter.contains("concise structured markdown"));
        assert!(ollama.contains("direct and efficient"));
    }
}
