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
    fn always_includes_baseline_tools() {
        let names = active_native_tool_names(&ToolAvailability::default());
        for tool in ["read_skill", "list_tools", "list_agents", "invoke_agent"] {
            assert!(names.contains(tool), "{tool} must always be present");
        }
        // Baseline count: 4 always-on tools
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn includes_fs_read_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            fs_read: true,
            ..Default::default()
        });
        for tool in ["read_file", "read_binary", "list_directory", "glob_search"] {
            assert!(names.contains(tool), "{tool} missing for fs_read");
        }
    }

    #[test]
    fn includes_fs_write_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            fs_write: true,
            ..Default::default()
        });
        for tool in [
            "write_file",
            "create_directory",
            "delete_file",
            "move_file",
            "apply_diff",
        ] {
            assert!(names.contains(tool), "{tool} missing for fs_write");
        }
    }

    #[test]
    fn includes_shell_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            shell: true,
            ..Default::default()
        });
        for tool in ["shell_execute", "shell_set_env", "shell_cd", "shell_status"] {
            assert!(names.contains(tool), "{tool} missing for shell");
        }
    }

    #[test]
    fn includes_git_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            git: true,
            ..Default::default()
        });
        for tool in [
            "git_status",
            "git_diff",
            "git_log",
            "git_add",
            "git_create_branch",
            "git_switch_branch",
            "git_commit",
        ] {
            assert!(names.contains(tool), "{tool} missing for git");
        }
    }

    #[test]
    fn includes_search_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            search: true,
            ..Default::default()
        });
        for tool in ["search_code", "find_files", "find_definition"] {
            assert!(names.contains(tool), "{tool} missing for search");
        }
    }

    #[test]
    fn includes_mcp_management_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            add_mcp: true,
            ..Default::default()
        });
        for tool in [
            "list_mcp_services",
            "add_mcp_service",
            "delete_mcp_service",
            "edit_mcp_service",
        ] {
            assert!(names.contains(tool), "{tool} missing for add_mcp");
        }
    }

    #[test]
    fn includes_excel_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            excel_read: true,
            excel_write: true,
            ..Default::default()
        });
        for tool in ["read_excel", "write_excel", "edit_excel"] {
            assert!(names.contains(tool), "{tool} missing for excel");
        }
    }

    #[test]
    fn includes_pdf_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            pdf_to_image: true,
            pdf_info: true,
            pdf_extract_text: true,
            ..Default::default()
        });
        for tool in ["pdf_to_image", "pdf_info", "pdf_extract_text"] {
            assert!(names.contains(tool), "{tool} missing for pdf");
        }
    }

    #[test]
    fn includes_data_query_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            data_query: true,
            ..Default::default()
        });
        for tool in ["query_data", "describe_data"] {
            assert!(names.contains(tool), "{tool} missing for data_query");
        }
    }

    #[test]
    fn includes_memory_tools() {
        let names = active_native_tool_names(&ToolAvailability {
            memory: true,
            ..Default::default()
        });
        for tool in ["remember", "save_skill", "search_memory"] {
            assert!(names.contains(tool), "{tool} missing for memory");
        }
    }

    #[test]
    fn includes_single_flag_tools() {
        // Tools that are a single flag → single tool name
        let cases = [
            ("fetch", "fetch"),
            ("add_attachment", "add_attachment"),
            ("compile_typst", "compile_typst"),
            ("execute_code", "execute_code"),
            ("search_web", "search_web"),
            ("sub_agent", "sub_agent"),
            ("browser_use", "browser_use"),
            ("daytona", "daytona_run"),
            ("publish_module", "publish_wasm_module"),
        ];

        for (flag, expected_tool) in cases {
            let mut tools = ToolAvailability::default();
            match flag {
                "fetch" => tools.fetch = true,
                "add_attachment" => tools.add_attachment = true,
                "compile_typst" => tools.compile_typst = true,
                "execute_code" => tools.execute_code = true,
                "search_web" => tools.search_web = true,
                "sub_agent" => tools.sub_agent = true,
                "browser_use" => tools.browser_use = true,
                "daytona" => tools.daytona = true,
                "publish_module" => tools.publish_module = true,
                _ => unreachable!(),
            }
            let names = active_native_tool_names(&tools);
            assert!(
                names.contains(expected_tool),
                "flag {flag} should register tool {expected_tool}"
            );
        }
    }

    #[test]
    fn all_flags_enabled_produces_superset() {
        let all = ToolAvailability {
            fs_read: true,
            fs_write: true,
            add_mcp: true,
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
        };
        let names = active_native_tool_names(&all);
        // Every individual flag's tools should be present
        let none_names = active_native_tool_names(&ToolAvailability::default());
        assert!(names.len() > none_names.len());
        // All baseline tools still present
        for tool in &none_names {
            assert!(names.contains(tool));
        }
    }

    #[test]
    fn disabled_flags_do_not_leak_tools() {
        let names = active_native_tool_names(&ToolAvailability::default());
        // These should NOT be present when all flags are false
        for tool in [
            "read_file",
            "write_file",
            "shell_execute",
            "git_status",
            "search_code",
            "read_excel",
            "pdf_info",
            "fetch",
            "search_web",
            "remember",
            "sub_agent",
            "browser_use",
            "daytona_run",
            "execute_code",
            "compile_typst",
        ] {
            assert!(
                !names.contains(tool),
                "{tool} should NOT be present when all flags are false"
            );
        }
    }
}
