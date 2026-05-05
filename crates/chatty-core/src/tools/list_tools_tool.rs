use crate::factories::agent_factory::ToolAvailability;
use crate::tools::ToolError;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

/// Arguments for the list_tools tool (no arguments needed)
#[derive(Deserialize, Serialize)]
pub struct ListToolsArgs {}

/// Output from the list_tools tool
#[derive(Debug, Serialize)]
pub struct ListToolsOutput {
    pub tools: Vec<ToolInfo>,
    pub note: String,
}

/// Information about a single tool
#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub source: String, // "native" or "mcp:{server_name}"
}

/// Tool that lists all available tools (both native and MCP)
#[derive(Clone)]
pub struct ListToolsTool {
    native_tools: Vec<ToolInfo>,
    mcp_tools: Vec<ToolInfo>,
}

impl ListToolsTool {
    /// Create a new ListToolsTool with the specified tool availability and optional MCP tools.
    ///
    /// `mcp_tool_info` is a list of (server_name, tool_name, tool_description) tuples
    /// extracted from the MCP service so the model can discover them via `list_tools`.
    pub fn new_with_config(
        tools: &ToolAvailability,
        mcp_tool_info: Vec<(String, String, String)>,
    ) -> Self {
        let mut native_tools = vec![ToolInfo {
            name: "list_tools".to_string(),
            description: "List all available tools (both native and MCP)".to_string(),
            source: "native".to_string(),
        }];

        if tools.list_mcp {
            native_tools.push(ToolInfo {
                name: "list_mcp_services".to_string(),
                description: "List all configured MCP servers (names, enabled state). Use to see which MCP integrations are available.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.fetch {
            native_tools.push(ToolInfo {
                name: "fetch".to_string(),
                description: "Fetch a URL and return its content as readable text. HTML pages are converted to plain text. Use for documentation lookups, web pages, or API responses.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.fs_read {
            native_tools.extend(vec![
                ToolInfo {
                    name: "read_file".to_string(),
                    description: "Read the contents of a text file within the workspace. Supports optional start_line and end_line for range-based reads; large reads are auto-chunked and return next_start_line so you can continue incrementally."
                        .to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "read_binary".to_string(),
                    description: "Read a binary file and return base64-encoded data".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "list_directory".to_string(),
                    description: "List the contents of a directory within the workspace"
                        .to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "glob_search".to_string(),
                    description: "Search for files matching a glob pattern within the workspace"
                        .to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.fs_write {
            native_tools.extend(vec![
                ToolInfo {
                    name: "write_file".to_string(),
                    description: "Create or overwrite a file within the workspace. Best for new files or small rewrites; for large existing files, prefer apply_diff or an in-place shell edit to avoid very large tool payloads.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "create_directory".to_string(),
                    description: "Create a new directory within the workspace".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "delete_file".to_string(),
                    description: "Delete a file within the workspace".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "move_file".to_string(),
                    description: "Move or rename a file within the workspace".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "apply_diff".to_string(),
                    description: "Apply a targeted edit to a file by replacing specific content. Prefer this over write_file when changing only part of a larger file."
                        .to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.shell {
            native_tools.extend(vec![
                ToolInfo {
                    name: "shell_execute".to_string(),
                    description: "Execute a command in a persistent shell session that preserves state (env vars, working directory) across calls".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "shell_set_env".to_string(),
                    description: "Set an environment variable in the persistent shell session".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "shell_cd".to_string(),
                    description: "Change the working directory in the persistent shell session".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "shell_status".to_string(),
                    description: "Get the current status of the persistent shell session (cwd, env vars, pid, uptime)".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.git {
            native_tools.extend(vec![
                ToolInfo {
                    name: "git_status".to_string(),
                    description: "Check the current status of the git repository (branch, staged, modified, untracked files)".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_diff".to_string(),
                    description: "View changes in the git repository (staged or unstaged)".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_log".to_string(),
                    description: "View recent commit history".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_add".to_string(),
                    description: "Stage files for the next commit (requires user confirmation)".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_create_branch".to_string(),
                    description: "Create a new git branch from current HEAD".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_switch_branch".to_string(),
                    description: "Switch to an existing git branch".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "git_commit".to_string(),
                    description: "Commit staged changes with a message (requires user confirmation)".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.search {
            native_tools.extend(vec![
                ToolInfo {
                    name: "search_code".to_string(),
                    description: "Search for a text pattern or regex in the workspace using ripgrep. Returns matching lines with file paths and line numbers. Requires 'rg' (ripgrep) to be installed.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "find_files".to_string(),
                    description: "Find files matching a glob pattern within the workspace. Returns matching file paths relative to the workspace root.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "find_definition".to_string(),
                    description: "Find definitions of a symbol (function, class, struct, etc.) in the workspace. Searches Rust, JavaScript/TypeScript, and Python files.".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.add_attachment {
            native_tools.push(ToolInfo {
                name: "add_attachment".to_string(),
                description: "Display an image or PDF file inline in the chat response. Use this to show generated plots, charts, screenshots, or documents. Supported formats: PNG, JPG, JPEG, GIF, WebP, SVG, BMP (images), PDF (documents). Maximum file size: 5MB.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.excel_read {
            native_tools.push(ToolInfo {
                name: "read_excel".to_string(),
                description: "Read an Excel spreadsheet and return structured data as JSON with a markdown table preview. Supports .xlsx, .xls, .xlsm, .xlsb, .ods formats.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.excel_write {
            native_tools.extend(vec![
                ToolInfo {
                    name: "write_excel".to_string(),
                    description: "Create a new Excel (.xlsx) file with data, formatting, formulas, merged cells, and auto-filters.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "edit_excel".to_string(),
                    description: "Edit an existing Excel file by applying targeted modifications (set cells, add sheets, delete rows, formulas, formatting). Warning: rewrites the file, which may lose original formatting/macros.".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.pdf_to_image {
            native_tools.push(ToolInfo {
                name: "pdf_to_image".to_string(),
                description: "Convert PDF pages to PNG images and display them inline in chat. Use when you need to visually inspect PDF content or the model lacks native PDF support. Maximum 20 pages per call, configurable DPI (72-300).".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.pdf_info {
            native_tools.push(ToolInfo {
                name: "pdf_info".to_string(),
                description: "Get metadata and structural information about a PDF file: page count, page dimensions, title, author, creation date, etc.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.pdf_extract_text {
            native_tools.push(ToolInfo {
                name: "pdf_extract_text".to_string(),
                description: "Extract text content from PDF pages. Returns raw text from specified pages or all pages. Maximum 50 pages per call.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.data_query {
            native_tools.extend(vec![
                ToolInfo {
                    name: "query_data".to_string(),
                    description: "Run SQL queries against local Parquet, CSV, or JSON files using DuckDB. Supports aggregations, joins, window functions, and all standard SQL.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "describe_data".to_string(),
                    description: "Inspect the schema and statistics of a local data file (Parquet, CSV, JSON). Returns column names, types, row count, and file size.".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.compile_typst {
            native_tools.push(ToolInfo {
                name: "compile_typst".to_string(),
                description: "Compile Typst markup into a PDF file and save it to disk. Supports headings, paragraphs, tables, math expressions, code blocks, lists, and multi-page documents.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.execute_code {
            native_tools.push(ToolInfo {
                name: "execute_code".to_string(),
                description: "Execute code in an isolated sandbox. Python may use the built-in Monty interpreter for simple snippets and fall back to Docker automatically; javascript, typescript, rust, and bash use Docker. State persists throughout the conversation. No network access.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.memory {
            native_tools.extend(vec![
                ToolInfo {
                    name: "remember".to_string(),
                    description: "Store important information in persistent memory for future conversations. Use for key facts, decisions, user preferences, or project context.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "save_skill".to_string(),
                    description: "Save a reusable multi-step procedure to persistent memory for automatic recall in future conversations. Use after successfully solving a new type of multi-step task.".to_string(),
                    source: "native".to_string(),
                },
                ToolInfo {
                    name: "search_memory".to_string(),
                    description: "Search persistent memory for previously stored information. Use to recall facts, decisions, or context from past conversations.".to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if tools.search_web {
            native_tools.push(ToolInfo {
                name: "search_web".to_string(),
                description: "Search the web for up-to-date information. Use this first when you need current information, recent events, or anything not in your training data.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.sub_agent {
            native_tools.push(ToolInfo {
                name: "sub_agent".to_string(),
                description: "Delegate a task to an independent sub-agent that has access to the same tools. The sub-agent runs autonomously in its own process, executes the task (including any tool calls it needs), and returns the result. Use this to parallelize work or isolate complex sub-tasks. Supports an optional `model` parameter to run the sub-agent with a different model.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.browser_use {
            native_tools.push(ToolInfo {
                name: "browser_use".to_string(),
                description: "Automate browser tasks using the browser-use cloud service. Describe what you want the browser agent to do in natural language. The agent controls a real browser and returns the result.".to_string(),
                source: "native".to_string(),
            });
        }

        if tools.daytona {
            native_tools.push(ToolInfo {
                name: "daytona_run".to_string(),
                description: "Execute code in an isolated Daytona cloud sandbox. Creates a secure, ephemeral environment, runs your code, returns the output, and cleans up automatically.".to_string(),
                source: "native".to_string(),
            });
        }

        // read_skill is always available — it's the on-demand companion to the slim
        // skill descriptions shown in the automatic context block.
        native_tools.push(ToolInfo {
            name: "read_skill".to_string(),
            description: "Load the full step-by-step instructions for a skill by name. \
                          Skills are listed with a one-line description in automatic context — \
                          call this before executing any skill to get the complete procedure."
                .to_string(),
            source: "native".to_string(),
        });

        let mcp_tools = mcp_tool_info
            .into_iter()
            .map(|(server_name, tool_name, tool_description)| ToolInfo {
                name: tool_name,
                description: tool_description,
                source: format!("mcp:{}", server_name),
            })
            .collect();

        Self {
            native_tools,
            mcp_tools,
        }
    }

    /// Create a new ListToolsTool (for backward compatibility)
    pub fn new() -> Self {
        Self::new_with_config(&ToolAvailability::default(), Vec::new())
    }
}

impl Tool for ListToolsTool {
    const NAME: &'static str = "list_tools";
    type Error = ToolError;
    type Args = ListToolsArgs;
    type Output = ListToolsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_tools".to_string(),
            description: "List all available tools including:\n\
                         - fetch: Fetch web URLs and return readable text content\n\
                         - shell_execute: Execute shell/terminal commands in a persistent session\n\
                         - Filesystem tools: read_file, write_file, list_directory, etc.\n\
                         - Git tools: git_status, git_diff, git_log, git_add, git_create_branch, git_switch_branch, git_commit\n\
                         - add_attachment: Display images or PDFs inline in chat responses\n\
                         - PDF tools: pdf_info, pdf_extract_text, pdf_to_image\n\
                         - Data query tools: query_data, describe_data (SQL on Parquet/CSV/JSON via DuckDB)\n\
                         - MCP tools: External tools from connected servers\n\
                         \n\
                         Use this to discover what capabilities you have for task execution. \
                         The returned list reflects ONLY the tools currently available in this session."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Return all tools: native + MCP
        let mut all_tools = self.native_tools.clone();
        all_tools.extend(self.mcp_tools.clone());

        let has_shell = self.native_tools.iter().any(|t| t.name == "shell_execute");
        let has_mcp = !self.mcp_tools.is_empty();

        tracing::info!(
            native_tool_count = self.native_tools.len(),
            mcp_tool_count = self.mcp_tools.len(),
            total_tool_count = all_tools.len(),
            "list_tools called - returning tool inventory"
        );

        // Log each MCP tool for debugging
        for tool in &self.mcp_tools {
            tracing::debug!(
                tool_name = %tool.name,
                source = %tool.source,
                "MCP tool in list_tools output"
            );
        }

        let note = match (has_shell, has_mcp) {
            (true, true) => "IMPORTANT: The 'shell_execute' tool listed above can execute ANY shell/terminal command (ls, pwd, cd, grep, find, cat, curl, git, npm, cargo, etc.) in a persistent session. Use it for all command-line operations. For multi-line Python or shell logic, prefer writing a script via here-doc or a temp file and then running it, instead of large `python -c '...'` one-liners. For verbose commands, prefer quiet flags and targeted slices (for example `curl -fsSL`, `head`, or `sed -n`) so you do not flood the context window. MCP tools from connected servers are also listed above — use them by name.".to_string(),
            (true, false) => "IMPORTANT: The 'shell_execute' tool listed above can execute ANY shell/terminal command (ls, pwd, cd, grep, find, cat, curl, git, npm, cargo, etc.) in a persistent session. Use it for all command-line operations. For multi-line Python or shell logic, prefer writing a script via here-doc or a temp file and then running it, instead of large `python -c '...'` one-liners. For verbose commands, prefer quiet flags and targeted slices (for example `curl -fsSL`, `head`, or `sed -n`) so you do not flood the context window.".to_string(),
            (false, true) => "These are the available tools. MCP tools from connected servers are also listed — use them by name.".to_string(),
            (false, false) => "These are the native tools available.".to_string(),
        };

        Ok(ListToolsOutput {
            tools: all_tools,
            note,
        })
    }
}

impl Default for ListToolsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    fn no_tools() -> ToolAvailability {
        ToolAvailability {
            fs_read: false,
            fs_write: false,
            list_mcp: false,
            fetch: false,
            shell: false,
            git: false,
            search: false,
            add_attachment: false,
            excel_read: false,
            excel_write: false,
            pdf_to_image: false,
            pdf_info: false,
            pdf_extract_text: false,
            data_query: false,
            compile_typst: false,
            execute_code: false,
            memory: false,
            search_web: false,
            sub_agent: false,
            browser_use: false,
            daytona: false,
            publish_module: false,
        }
    }

    fn all_tools() -> ToolAvailability {
        ToolAvailability {
            fs_read: true,
            fs_write: true,
            list_mcp: true,
            fetch: true,
            shell: true,
            git: true,
            search: true,
            add_attachment: true,
            excel_read: true,
            excel_write: true,
            pdf_to_image: true,
            pdf_info: true,
            pdf_extract_text: true,
            data_query: true,
            compile_typst: true,
            execute_code: true,
            memory: true,
            search_web: true,
            sub_agent: true,
            browser_use: true,
            daytona: true,
            publish_module: true,
        }
    }

    fn tool_names(output: &ListToolsOutput) -> Vec<String> {
        output.tools.iter().map(|t| t.name.clone()).collect()
    }

    #[tokio::test]
    async fn test_default_has_list_tools_and_read_skill() {
        let tool = ListToolsTool::new();
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let names = tool_names(&output);
        assert!(names.contains(&"list_tools".to_string()));
        assert!(names.contains(&"read_skill".to_string()));
    }

    #[tokio::test]
    async fn test_no_tools_returns_minimal_set() {
        let tool = ListToolsTool::new_with_config(&no_tools(), Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let names = tool_names(&output);
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"list_tools".to_string()));
        assert!(names.contains(&"read_skill".to_string()));
        assert_eq!(output.note, "These are the native tools available.");
    }

    #[tokio::test]
    async fn test_fs_read_adds_four_tools() {
        let mut avail = no_tools();
        avail.fs_read = true;
        let tool = ListToolsTool::new_with_config(&avail, Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let names = tool_names(&output);
        for expected in &["read_file", "read_binary", "list_directory", "glob_search"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        // 2 always-present + 4 fs_read
        assert_eq!(names.len(), 6);
    }

    #[tokio::test]
    async fn test_shell_adds_four_tools() {
        let mut avail = no_tools();
        avail.shell = true;
        let tool = ListToolsTool::new_with_config(&avail, Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let names = tool_names(&output);
        for expected in &["shell_execute", "shell_set_env", "shell_cd", "shell_status"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        // 2 always-present + 4 shell
        assert_eq!(names.len(), 6);
    }

    #[tokio::test]
    async fn test_all_tools_includes_everything() {
        let tool = ListToolsTool::new_with_config(&all_tools(), Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let names = tool_names(&output);
        let expected = vec![
            "list_tools",
            "read_skill",
            "list_mcp_services",
            "fetch",
            "read_file",
            "read_binary",
            "list_directory",
            "glob_search",
            "write_file",
            "create_directory",
            "delete_file",
            "move_file",
            "apply_diff",
            "shell_execute",
            "shell_set_env",
            "shell_cd",
            "shell_status",
            "git_status",
            "git_diff",
            "git_log",
            "git_add",
            "git_create_branch",
            "git_switch_branch",
            "git_commit",
            "search_code",
            "find_files",
            "find_definition",
            "add_attachment",
            "read_excel",
            "write_excel",
            "edit_excel",
            "pdf_to_image",
            "pdf_info",
            "pdf_extract_text",
            "query_data",
            "describe_data",
            "compile_typst",
            "execute_code",
            "remember",
            "save_skill",
            "search_memory",
            "search_web",
            "sub_agent",
            "browser_use",
            "daytona_run",
        ];
        for name in &expected {
            assert!(names.contains(&name.to_string()), "missing {name}");
        }
    }

    #[tokio::test]
    async fn test_mcp_tools_have_correct_source() {
        let mcp = vec![(
            "my-server".to_string(),
            "my_tool".to_string(),
            "desc".to_string(),
        )];
        let tool = ListToolsTool::new_with_config(&no_tools(), mcp);
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        let mcp_tool = output.tools.iter().find(|t| t.name == "my_tool").unwrap();
        assert_eq!(mcp_tool.source, "mcp:my-server");
    }

    #[tokio::test]
    async fn test_note_with_shell_and_mcp() {
        let mut avail = no_tools();
        avail.shell = true;
        let mcp = vec![("srv".into(), "t".into(), "d".into())];
        let tool = ListToolsTool::new_with_config(&avail, mcp);
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        assert!(output.note.contains("shell_execute"));
        assert!(output.note.contains("MCP"));
    }

    #[tokio::test]
    async fn test_note_without_shell_or_mcp() {
        let tool = ListToolsTool::new_with_config(&no_tools(), Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        assert_eq!(output.note, "These are the native tools available.");
    }

    #[tokio::test]
    async fn test_all_native_tools_have_native_source() {
        let tool = ListToolsTool::new_with_config(&all_tools(), Vec::new());
        let output = tool.call(ListToolsArgs {}).await.unwrap();
        for t in &output.tools {
            assert_eq!(t.source, "native", "{} has source {}", t.name, t.source);
        }
    }
}
