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

/// Error type for list_tools tool
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum ListToolsError {
    #[error("Error listing tools: {0}")]
    Error(String),
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
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        has_fs_read: bool,
        has_fs_write: bool,
        has_add_mcp: bool,
        has_fetch: bool,
        has_shell: bool,
        has_git: bool,
        has_search: bool,
        has_add_attachment: bool,
        has_excel_read: bool,
        has_excel_write: bool,
        has_pdf_to_image: bool,
        has_pdf_info: bool,
        has_pdf_extract_text: bool,
        has_data_query: bool,
        has_compile_typst: bool,
        has_execute_code: bool,
        has_memory: bool,
        has_search_web: bool,
        has_sub_agent: bool,
        has_browse: bool,
        mcp_tool_info: Vec<(String, String, String)>,
    ) -> Self {
        let mut native_tools = vec![ToolInfo {
            name: "list_tools".to_string(),
            description: "List all available tools (both native and MCP)".to_string(),
            source: "native".to_string(),
        }];

        if has_add_mcp {
            native_tools.push(ToolInfo {
                name: "list_mcp_services".to_string(),
                description: "List all configured MCP servers (names, commands, args, enabled state, masked env vars). Call this FIRST before editing or deleting to confirm the exact server name.".to_string(),
                source: "native".to_string(),
            });
            native_tools.push(ToolInfo {
                name: "add_mcp_service".to_string(),
                description: "Add a new MCP server configuration so it becomes available in future conversations".to_string(),
                source: "native".to_string(),
            });
            native_tools.push(ToolInfo {
                name: "delete_mcp_service".to_string(),
                description: "Delete an existing MCP server configuration and stop it if running"
                    .to_string(),
                source: "native".to_string(),
            });
            native_tools.push(ToolInfo {
                name: "edit_mcp_service".to_string(),
                description:
                    "Edit an existing MCP server's command, args, or env vars (enabling/disabling is user-only via Settings)"
                        .to_string(),
                source: "native".to_string(),
            });
        }

        if has_fetch {
            native_tools.push(ToolInfo {
                name: "fetch".to_string(),
                description: "Fetch a URL and return its content as readable text. HTML pages are converted to plain text. Use for documentation lookups, web pages, or API responses.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_fs_read {
            native_tools.extend(vec![
                ToolInfo {
                    name: "read_file".to_string(),
                    description: "Read the contents of a text file within the workspace"
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

        if has_fs_write {
            native_tools.extend(vec![
                ToolInfo {
                    name: "write_file".to_string(),
                    description: "Create or overwrite a file within the workspace".to_string(),
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
                    description: "Apply a targeted edit to a file by replacing specific content"
                        .to_string(),
                    source: "native".to_string(),
                },
            ]);
        }

        if has_shell {
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

        if has_git {
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

        if has_search {
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

        if has_add_attachment {
            native_tools.push(ToolInfo {
                name: "add_attachment".to_string(),
                description: "Display an image or PDF file inline in the chat response. Use this to show generated plots, charts, screenshots, or documents. Supported formats: PNG, JPG, JPEG, GIF, WebP, SVG, BMP (images), PDF (documents). Maximum file size: 5MB.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_excel_read {
            native_tools.push(ToolInfo {
                name: "read_excel".to_string(),
                description: "Read an Excel spreadsheet and return structured data as JSON with a markdown table preview. Supports .xlsx, .xls, .xlsm, .xlsb, .ods formats.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_excel_write {
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

        if has_pdf_to_image {
            native_tools.push(ToolInfo {
                name: "pdf_to_image".to_string(),
                description: "Convert PDF pages to PNG images and display them inline in chat. Use when you need to visually inspect PDF content or the model lacks native PDF support. Maximum 20 pages per call, configurable DPI (72-300).".to_string(),
                source: "native".to_string(),
            });
        }

        if has_pdf_info {
            native_tools.push(ToolInfo {
                name: "pdf_info".to_string(),
                description: "Get metadata and structural information about a PDF file: page count, page dimensions, title, author, creation date, etc.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_pdf_extract_text {
            native_tools.push(ToolInfo {
                name: "pdf_extract_text".to_string(),
                description: "Extract text content from PDF pages. Returns raw text from specified pages or all pages. Maximum 50 pages per call.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_data_query {
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

        if has_compile_typst {
            native_tools.push(ToolInfo {
                name: "compile_typst".to_string(),
                description: "Compile Typst markup into a PDF file and save it to disk. Supports headings, paragraphs, tables, math expressions, code blocks, lists, and multi-page documents.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_execute_code {
            native_tools.push(ToolInfo {
                name: "execute_code".to_string(),
                description: "Execute code in an isolated Docker sandbox. Supports python, javascript, typescript, rust, and bash. State persists throughout the conversation. No network access.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_memory {
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

        if has_search_web {
            native_tools.push(ToolInfo {
                name: "search_web".to_string(),
                description: "Search the web for up-to-date information. Use this first when you need current information, recent events, or anything not in your training data.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_sub_agent {
            native_tools.push(ToolInfo {
                name: "sub_agent".to_string(),
                description: "Delegate a task to an independent sub-agent that has access to the same tools. The sub-agent runs autonomously in its own process, executes the task (including any tool calls it needs), and returns the result. Use this to parallelize work or isolate complex sub-tasks.".to_string(),
                source: "native".to_string(),
            });
        }

        if has_browse {
            native_tools.push(ToolInfo {
                name: "browse".to_string(),
                description: "Navigate to a URL using a built-in browser engine that executes JavaScript. Returns structured page content including text, interactive elements (buttons, inputs), forms, and links. Use this for dynamic web pages and SPAs that require JS rendering.".to_string(),
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
        Self::new_with_config(
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            Vec::new(),
        )
    }
}

impl Tool for ListToolsTool {
    const NAME: &'static str = "list_tools";
    type Error = ListToolsError;
    type Args = ListToolsArgs;
    type Output = ListToolsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_tools".to_string(),
            description: "List all available tools including:\n\
                         - fetch: Fetch web URLs and return readable text content\n\
                         - browse: Navigate web pages with a built-in browser engine (JS rendering)\n\
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
            (true, true) => "IMPORTANT: The 'shell_execute' tool listed above can execute ANY shell/terminal command (ls, pwd, cd, grep, find, cat, curl, git, npm, cargo, etc.) in a persistent session. Use it for all command-line operations. MCP tools from connected servers are also listed above — use them by name.".to_string(),
            (true, false) => "IMPORTANT: The 'shell_execute' tool listed above can execute ANY shell/terminal command (ls, pwd, cd, grep, find, cat, curl, git, npm, cargo, etc.) in a persistent session. Use it for all command-line operations.".to_string(),
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
