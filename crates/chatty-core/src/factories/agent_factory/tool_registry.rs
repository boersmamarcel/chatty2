use std::collections::HashSet;

/// Boolean flags indicating which native tools are available.
///
/// Used to register tool names, generate the tool listing for the LLM,
/// and build the system prompt tool summary.
#[derive(Clone, Debug, Default)]
pub struct ToolAvailability {
    pub fs_read: bool,
    pub fs_write: bool,
    pub add_mcp: bool,
    pub fetch: bool,
    pub shell: bool,
    pub git: bool,
    pub search: bool,
    pub add_attachment: bool,
    pub excel_read: bool,
    pub excel_write: bool,
    pub pdf_to_image: bool,
    pub pdf_info: bool,
    pub pdf_extract_text: bool,
    pub data_query: bool,
    pub compile_typst: bool,
    pub execute_code: bool,
    pub memory: bool,
    pub search_web: bool,
    pub sub_agent: bool,
    pub browser_use: bool,
    pub daytona: bool,
    pub publish_module: bool,
}

pub(super) fn active_native_tool_names(tools: &ToolAvailability) -> HashSet<String> {
    let mut names = HashSet::from([
        String::from("list_tools"),
        String::from("read_skill"),
        String::from("list_agents"),
        String::from("invoke_agent"),
    ]);

    if tools.add_mcp {
        names.extend(
            [
                "list_mcp_services",
                "add_mcp_service",
                "delete_mcp_service",
                "edit_mcp_service",
            ]
            .into_iter()
            .map(String::from),
        );
    }
    if tools.fetch {
        names.insert(String::from("fetch"));
    }
    if tools.fs_read {
        names.extend(
            ["read_file", "read_binary", "list_directory", "glob_search"]
                .into_iter()
                .map(String::from),
        );
    }
    if tools.fs_write {
        names.extend(
            [
                "write_file",
                "create_directory",
                "delete_file",
                "move_file",
                "apply_diff",
            ]
            .into_iter()
            .map(String::from),
        );
    }
    if tools.shell {
        names.extend(
            ["shell_execute", "shell_set_env", "shell_cd", "shell_status"]
                .into_iter()
                .map(String::from),
        );
    }
    if tools.git {
        names.extend(
            [
                "git_status",
                "git_diff",
                "git_log",
                "git_add",
                "git_create_branch",
                "git_switch_branch",
                "git_commit",
            ]
            .into_iter()
            .map(String::from),
        );
    }
    if tools.search {
        names.extend(
            ["search_code", "find_files", "find_definition"]
                .into_iter()
                .map(String::from),
        );
    }
    if tools.add_attachment {
        names.insert(String::from("add_attachment"));
    }
    if tools.excel_read {
        names.insert(String::from("read_excel"));
    }
    if tools.excel_write {
        names.extend(["write_excel", "edit_excel"].into_iter().map(String::from));
    }
    if tools.pdf_to_image {
        names.insert(String::from("pdf_to_image"));
    }
    if tools.pdf_info {
        names.insert(String::from("pdf_info"));
    }
    if tools.pdf_extract_text {
        names.insert(String::from("pdf_extract_text"));
    }
    if tools.data_query {
        names.extend(
            ["query_data", "describe_data"]
                .into_iter()
                .map(String::from),
        );
    }
    if tools.compile_typst {
        names.insert(String::from("compile_typst"));
    }
    if tools.execute_code {
        names.insert(String::from("execute_code"));
    }
    if tools.memory {
        names.extend(
            ["remember", "save_skill", "search_memory"]
                .into_iter()
                .map(String::from),
        );
    }
    if tools.search_web {
        names.insert(String::from("search_web"));
    }
    if tools.sub_agent {
        names.insert(String::from("sub_agent"));
    }
    if tools.browser_use {
        names.insert(String::from("browser_use"));
    }
    if tools.daytona {
        names.insert(String::from("daytona_run"));
    }
    if tools.publish_module {
        names.insert(String::from("publish_wasm_module"));
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_includes_read_skill() {
        let names = active_native_tool_names(&ToolAvailability::default());
        assert!(
            names.contains("read_skill"),
            "read_skill must always be reserved to prevent MCP conflicts"
        );
        assert!(names.contains("list_tools"));
        assert!(names.contains("list_agents"));
    }

    #[test]
    fn includes_search_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            search: true,
            ..Default::default()
        });

        assert!(names.contains("list_tools"));
        assert!(names.contains("search_code"));
        assert!(names.contains("find_files"));
        assert!(names.contains("find_definition"));
    }
}
