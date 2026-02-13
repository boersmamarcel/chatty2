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
}

impl ListToolsTool {
    /// Create a new ListToolsTool with the specified tool availability
    pub fn new_with_config(has_bash: bool, has_fs_read: bool, has_fs_write: bool) -> Self {
        let mut native_tools = vec![ToolInfo {
            name: "list_tools".to_string(),
            description: "List all available tools (both native and MCP)".to_string(),
            source: "native".to_string(),
        }];

        if has_bash {
            native_tools.push(ToolInfo {
                name: "bash".to_string(),
                description: "**PRIMARY SHELL TOOL** - Execute ANY bash/shell/terminal command including: ls (list files), cd, pwd, grep, find, cat, echo, curl, git, npm, cargo, python, etc. Use this tool whenever you need to run command-line operations.".to_string(),
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

        Self { native_tools }
    }

    /// Create a new ListToolsTool (for backward compatibility)
    pub fn new() -> Self {
        Self::new_with_config(false, false, false)
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
                         - bash: Execute shell/terminal commands (ls, grep, find, cat, etc.)\n\
                         - Filesystem tools: read_file, write_file, list_directory, etc.\n\
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
        // Return the configured native tools
        let note = if self.native_tools.iter().any(|t| t.name == "bash") {
            "IMPORTANT: The 'bash' tool listed above can execute ANY shell/terminal command (ls, pwd, cd, grep, find, cat, curl, git, npm, cargo, etc.). Use it for all command-line operations.".to_string()
        } else {
            "These are the native tools available. Additional MCP tools may also be available."
                .to_string()
        };

        Ok(ListToolsOutput {
            tools: self.native_tools.clone(),
            note,
        })
    }
}

impl Default for ListToolsTool {
    fn default() -> Self {
        Self::new()
    }
}
