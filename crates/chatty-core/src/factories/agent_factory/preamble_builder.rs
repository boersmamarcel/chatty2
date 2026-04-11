use crate::settings::models::search_settings::SearchSettingsModel;

use super::mcp_helpers::McpTools;
use super::tool_registry::ToolAvailability;

/// Build the augmented preamble with tool summary, formatting guide,
/// memory instructions, and secret key names.
pub(super) fn build_preamble(
    base_preamble: &str,
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
            "- **search_web**: Search the web for up-to-date information ({search_note}). \
             Use this first when you need current information.\n\
             - **fetch**: Fetch any web URL and return its readable text content. \
             Use this to read specific pages, documentation, or articles."
        ));
    }
    if tools.shell {
        tool_sections.push(
            "- **shell_execute / shell_cd / shell_set_env / shell_status**: \
             Run any shell/terminal command in a persistent session that preserves \
             working directory and environment variables across calls. \
             Prefer this over asking the user to run commands manually."
                .to_string(),
        );
    }
    if tools.fs_read {
        tool_sections.push(
            "- **read_file / read_binary / list_directory / glob_search**: \
             Read files and explore the workspace directory."
                .to_string(),
        );
    }
    if tools.fs_write {
        tool_sections.push(
            "- **write_file / apply_diff / create_directory / delete_file / move_file**: \
             Create, edit, and manage files in the workspace. \
             Use apply_diff for targeted edits to existing files."
                .to_string(),
        );
    }
    if tools.search {
        tool_sections.push(
            "- **search_code / find_files / find_definition**: \
             Search for patterns, files, and symbol definitions in the workspace."
                .to_string(),
        );
    }
    if tools.git {
        tool_sections.push(
            "- **git_status / git_diff / git_log / git_add / git_commit / \
             git_create_branch / git_switch_branch**: \
             Inspect and manage git history and branches."
                .to_string(),
        );
    }
    if tools.add_attachment {
        tool_sections.push(
            "- **add_attachment**: Display an image or PDF inline in the chat response. \
             Useful for showing generated plots, screenshots, or documents."
                .to_string(),
        );
    }
    // Chart tool is always available (no filesystem/service dependencies)
    tool_sections.push(
        "- **create_chart**: Create and display a chart inline in the chat response. \
         Supports bar (with value labels), line, pie, donut, area, and candlestick charts. \
         Use this to visualize data for the user."
            .to_string(),
    );
    if tools.compile_typst {
        tool_sections.push(
            "- **compile_typst**: Compile Typst markup into a PDF file saved to disk. \
             Use for generating formatted documents: reports, papers, documents with math, \
             tables, headings, and code blocks. Typst syntax is markdown-like — \
             `= Heading`, `*bold*`, `_italic_`, `$ math $`, `#table(...)`, etc."
                .to_string(),
        );
    }
    if tools.excel_read || tools.excel_write {
        let mut excel_desc = Vec::new();
        if tools.excel_read {
            excel_desc.push("**read_excel**");
        }
        if tools.excel_write {
            excel_desc.push("**write_excel** / **edit_excel**");
        }
        tool_sections.push(format!(
            "- {}: Read, create, and edit Excel spreadsheets (.xlsx, .xls, .xlsm, .xlsb, .ods). \
             Supports cell data, formatting, formulas, merged cells, and auto-filters.",
            excel_desc.join(" / ")
        ));
    }
    if tools.pdf_to_image || tools.pdf_info || tools.pdf_extract_text {
        let mut pdf_desc = String::from("- **PDF tools**:");
        if tools.pdf_info {
            pdf_desc.push_str(" `pdf_info` (page count, dimensions, metadata),");
        }
        if tools.pdf_extract_text {
            pdf_desc.push_str(" `pdf_extract_text` (extract text from pages),");
        }
        if tools.pdf_to_image {
            pdf_desc
                .push_str(" `pdf_to_image` (render pages as PNG images for visual inspection),");
        }
        if pdf_desc.ends_with(',') {
            pdf_desc.pop();
        }
        pdf_desc.push('.');
        tool_sections.push(pdf_desc);
    }
    if tools.data_query {
        tool_sections.push(
            "- **query_data / describe_data**: Run SQL queries against local Parquet, CSV, \
             and JSON files using DuckDB. Use `describe_data` to inspect schema first, \
             then `query_data` for analytical SQL (aggregations, joins, window functions)."
                .to_string(),
        );
    }
    if mcp_mgmt_tools.is_enabled() {
        tool_sections.push(
            "- **list_mcp_services / add_mcp_service / edit_mcp_service / delete_mcp_service**: \
             Manage MCP server configurations."
                .to_string(),
        );
    }
    // list_agents + invoke_agent are always present
    tool_sections.push(
        "- **list_agents**: List all available agents (remote A2A and local WASM modules). \
         Call this to discover agents before invoking them."
            .to_string(),
    );
    tool_sections.push(
        "- **invoke_agent**: Invoke a named agent with a prompt. Use `list_agents` first to \
         discover available agents, then call `invoke_agent` with the agent's name and your \
         prompt. The agent runs autonomously and returns its response."
            .to_string(),
    );
    if tools.execute_code {
        tool_sections.push(
            "- **execute_code**: Execute code in an isolated Docker sandbox. \
             Supports python, javascript, typescript, rust, and bash. \
             State (variables, installed packages) persists throughout the conversation. \
             No network access. Use this for running code snippets, \
             data analysis, or verifying solutions."
                .to_string(),
        );
    }
    if tools.memory {
        tool_sections.push(
            "- **remember**: Store important information in persistent cross-conversation memory.\n\
             - **save_skill**: Save a reusable multi-step procedure to persistent memory for \
             automatic recall in future conversations.\n\
             - **search_memory**: Search previously stored memories by natural language query."
                .to_string(),
        );
    }
    if tools.sub_agent {
        tool_sections.push(
            "- **sub_agent**: Delegate a task to an independent sub-agent that has access to \
             the same tools. The sub-agent runs autonomously and returns the result. \
             Use this to parallelize work or isolate complex sub-tasks. Each sub-agent \
             starts fresh — include all necessary context in the task description. \
             You can optionally pass a `model` parameter to run the sub-agent with a \
             different model (e.g., a faster model for simple tasks)."
                .to_string(),
        );
    }
    if tools.browser_use {
        tool_sections.push(
            "- **browser_use**: Automate browser tasks using the browser-use cloud service. \
             Describe what you want the browser agent to do in natural language and it will \
             control a real browser and return the result."
                .to_string(),
        );
    }
    if tools.daytona {
        tool_sections.push(
            "- **daytona_run**: Execute code in an isolated Daytona cloud sandbox. \
             Creates a secure, ephemeral environment, runs the code, returns output, \
             and cleans up automatically. Useful for running code with internet access \
             or in a fresh environment."
                .to_string(),
        );
    }
    if tools.publish_module {
        tool_sections.push(
            "- **publish_wasm_module**: Publish a WASM module to the hive registry. \
             Provide the path to the .wasm file and a TOML manifest string. The tool \
             reads the binary, base64-encodes it, and uploads it automatically. \
             Use this instead of manually reading and encoding the file."
                .to_string(),
        );
    }
    // read_skill is always present
    tool_sections.push(
        "- **read_skill**: Load the full step-by-step instructions for a skill by name. \
         Skills are listed with a one-line description in the automatic context — \
         call this tool to get the complete procedure before executing it."
            .to_string(),
    );
    // Always present
    tool_sections.push(
        "- **list_tools**: Call this at any time to get the full, up-to-date list of \
         available tools with their exact names and descriptions."
            .to_string(),
    );

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
             You have access to the following tools. Use them proactively to help the user \
             instead of asking them to do things manually:\n\n{}",
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
        "\n\n## Memory\n\
             You have persistent memory that survives across conversations and app restarts. \
             Relevant memories are automatically injected as context before each of your responses — \
             you do not need to call `search_memory` proactively on every message. \
             Use `search_memory` only when you want to look up something specific that \
             may not have appeared in the automatic context (e.g., a detail mentioned several \
             conversations ago, or a narrow keyword search).\n\n\
             When the user explicitly asks you to remember, store, note, or keep in mind \
             any information, you MUST invoke the `remember` tool with the information as \
             the content parameter. Responding with text like \"I'll remember that\" or \
             \"I've noted that\" without calling the tool means the information is lost \
             permanently. Always call the tool FIRST, then confirm to the user.\n\n\
             **Saving skills**: After successfully solving a new type of multi-step task \
             (deployment, data analysis, build process, API integration, etc.), consider \
             using `save_skill` to record the steps as a reusable procedure. Saved skills \
             are automatically surfaced in future conversations when a similar task arises. \
             Only save skills for tasks with clear, reproducible steps — not one-off actions. \
             Skills created with `save_skill` live in persistent memory and can be found again \
             with `search_memory`. User-provided `SKILL.md` files are a separate filesystem-backed \
             source; when those appear in context, rely on the source hints in the injected skill \
             block and use normal file-reading tools if you need to revisit the file contents.\n\n\
             **Python in skills**: When following a skill that needs Python package management in \
             the shell, prefer `uv` over `pip`. If the `execute_code` tool is available and an \
             isolated environment is helpful, prefer Docker-backed `execute_code` for the Python run.\n\n\
             **Keyword-rich storage**: Memory search uses keyword matching (not semantic similarity). \
             When storing a memory, always include synonyms, related terms, and category words \
             so the memory can be found by different search terms. For example, if the user says \
             \"I like bananas\", store: \"User likes bananas. Categories: fruit, food, preference.\" \
             This ensures a future search for \"fruit\" or \"food preferences\" will find this memory."
    } else {
        ""
    };

    // Skills instructions — always injected because read_skill is always available.
    let skills_instructions = "\n\n## Skills\n\
         When relevant skills are detected for your query, a `[Relevant skills available]` \
         block is included in your context showing only the skill name and a \
         one-line description — the full instructions are intentionally omitted to save \
         context space. Call `read_skill` with the exact skill name before executing any \
         skill procedure so you have the complete, up-to-date steps.";

    let mut p = base_preamble.to_string();
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

#[cfg(test)]
mod tests {
    use super::super::mcp_helpers::McpTools;
    use super::super::tool_registry::ToolAvailability;
    use super::*;

    fn default_preamble_args() -> (
        ToolAvailability,
        Option<SearchSettingsModel>,
        McpTools,
        Vec<(String, String, String)>,
        Vec<String>,
    ) {
        (
            ToolAvailability::default(),
            None,
            McpTools::none(),
            vec![],
            vec![],
        )
    }

    #[test]
    fn empty_tools_still_includes_chart_and_formatting() {
        let (tools, search, mcp, mcp_info, secrets) = default_preamble_args();
        let result = build_preamble("Base prompt.", &tools, &search, &mcp, &mcp_info, &secrets);
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
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
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
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("read_file"));
        assert!(result.contains("write_file"));
        assert!(result.contains("apply_diff"));
    }

    #[test]
    fn git_tools_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.git = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("git_status"));
        assert!(result.contains("git_diff"));
        assert!(result.contains("git_commit"));
    }

    #[test]
    fn memory_section_included_when_enabled() {
        let mut tools = ToolAvailability::default();
        tools.memory = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("## Memory"));
        assert!(result.contains("remember"));
        assert!(result.contains("save_skill"));
        assert!(result.contains("search_memory"));
    }

    #[test]
    fn memory_section_excluded_when_disabled() {
        let tools = ToolAvailability::default();
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(!result.contains("## Memory"));
    }

    #[test]
    fn secret_keys_appended() {
        let tools = ToolAvailability::default();
        let secrets = vec!["API_KEY".to_string(), "DB_PASSWORD".to_string()];
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &secrets);
        assert!(result.contains("API_KEY"));
        assert!(result.contains("DB_PASSWORD"));
        assert!(result.contains("environment variables"));
    }

    #[test]
    fn secret_keys_not_appended_when_empty() {
        let tools = ToolAvailability::default();
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
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
        let result = build_preamble("", &tools, &None, &McpTools::none(), &mcp_info, &[]);
        assert!(result.contains("MCP tools"));
        assert!(result.contains("my_tool"));
        assert!(result.contains("my-server"));
    }

    #[test]
    fn excel_section_shows_read_only_when_write_disabled() {
        let mut tools = ToolAvailability::default();
        tools.excel_read = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("read_excel"));
        assert!(!result.contains("write_excel"));
    }

    #[test]
    fn excel_section_shows_both_when_both_enabled() {
        let mut tools = ToolAvailability::default();
        tools.excel_read = true;
        tools.excel_write = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("read_excel"));
        assert!(result.contains("write_excel"));
        assert!(result.contains("edit_excel"));
    }

    #[test]
    fn pdf_tools_section_included() {
        let mut tools = ToolAvailability::default();
        tools.pdf_info = true;
        tools.pdf_extract_text = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("pdf_info"));
        assert!(result.contains("pdf_extract_text"));
    }

    #[test]
    fn data_query_section_included() {
        let mut tools = ToolAvailability::default();
        tools.data_query = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("query_data"));
        assert!(result.contains("describe_data"));
    }

    #[test]
    fn sub_agent_section_included() {
        let mut tools = ToolAvailability::default();
        tools.sub_agent = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("sub_agent"));
        assert!(result.contains("Delegate a task"));
    }

    #[test]
    fn skills_section_always_present() {
        let tools = ToolAvailability::default();
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("## Skills"));
        assert!(result.contains("read_skill"));
    }

    #[test]
    fn search_web_with_fetch_shows_web_section() {
        let mut tools = ToolAvailability::default();
        tools.fetch = true;
        tools.search_web = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("search_web"));
        assert!(result.contains("fetch"));
    }

    #[test]
    fn compile_typst_section_included() {
        let mut tools = ToolAvailability::default();
        tools.compile_typst = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("compile_typst"));
        assert!(result.contains("Typst markup"));
    }

    #[test]
    fn execute_code_section_included() {
        let mut tools = ToolAvailability::default();
        tools.execute_code = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("execute_code"));
        assert!(result.contains("Docker sandbox"));
    }

    #[test]
    fn browser_use_section_included() {
        let mut tools = ToolAvailability::default();
        tools.browser_use = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("browser_use"));
    }

    #[test]
    fn daytona_section_included() {
        let mut tools = ToolAvailability::default();
        tools.daytona = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("daytona_run"));
    }

    #[test]
    fn publish_module_section_included() {
        let mut tools = ToolAvailability::default();
        tools.publish_module = true;
        let result = build_preamble("", &tools, &None, &McpTools::none(), &[], &[]);
        assert!(result.contains("publish_wasm_module"));
    }
}
