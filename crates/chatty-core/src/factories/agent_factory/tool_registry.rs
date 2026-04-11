use std::collections::HashSet;

#[allow(clippy::too_many_arguments)]
pub(super) fn active_native_tool_names(
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
    has_browser_use: bool,
    has_daytona: bool,
) -> HashSet<String> {
    let mut names = HashSet::from([
        String::from("list_tools"),
        String::from("read_skill"),
        String::from("list_agents"),
        String::from("invoke_agent"),
    ]);

    if has_add_mcp {
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
    if has_fetch {
        names.insert(String::from("fetch"));
    }
    if has_fs_read {
        names.extend(
            ["read_file", "read_binary", "list_directory", "glob_search"]
                .into_iter()
                .map(String::from),
        );
    }
    if has_fs_write {
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
    if has_shell {
        names.extend(
            ["shell_execute", "shell_set_env", "shell_cd", "shell_status"]
                .into_iter()
                .map(String::from),
        );
    }
    if has_git {
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
    if has_search {
        names.extend(
            ["search_code", "find_files", "find_definition"]
                .into_iter()
                .map(String::from),
        );
    }
    if has_add_attachment {
        names.insert(String::from("add_attachment"));
    }
    if has_excel_read {
        names.insert(String::from("read_excel"));
    }
    if has_excel_write {
        names.extend(["write_excel", "edit_excel"].into_iter().map(String::from));
    }
    if has_pdf_to_image {
        names.insert(String::from("pdf_to_image"));
    }
    if has_pdf_info {
        names.insert(String::from("pdf_info"));
    }
    if has_pdf_extract_text {
        names.insert(String::from("pdf_extract_text"));
    }
    if has_data_query {
        names.extend(
            ["query_data", "describe_data"]
                .into_iter()
                .map(String::from),
        );
    }
    if has_compile_typst {
        names.insert(String::from("compile_typst"));
    }
    if has_execute_code {
        names.insert(String::from("execute_code"));
    }
    if has_memory {
        names.extend(
            ["remember", "save_skill", "search_memory"]
                .into_iter()
                .map(String::from),
        );
    }
    if has_search_web {
        names.insert(String::from("search_web"));
    }
    if has_sub_agent {
        names.insert(String::from("sub_agent"));
    }
    if has_browser_use {
        names.insert(String::from("browser_use"));
    }
    if has_daytona {
        names.insert(String::from("daytona_run"));
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_includes_read_skill() {
        let names = active_native_tool_names(
            false, false, false, false, false, false, false, false, false, false, false, false,
            false, false, false, false, false, false, false, false, false,
        );
        assert!(
            names.contains("read_skill"),
            "read_skill must always be reserved to prevent MCP conflicts"
        );
        assert!(names.contains("list_tools"));
        assert!(names.contains("list_agents"));
    }

    #[test]
    fn includes_search_tools() {
        let names = active_native_tool_names(
            false, false, false, false, false, false, true, false, false, false, false, false,
            false, false, false, false, false, false, false, false, false,
        );

        assert!(names.contains("list_tools"));
        assert!(names.contains("search_code"));
        assert!(names.contains("find_files"));
        assert!(names.contains("find_definition"));
    }
}
