//! Named tool profiles: a small, named allowlist of tool names a worker may
//! call (ADR-0011 C11 / AGE-405).
//!
//! `--enable` / `--disable` work on tool *groups*, which is the wrong grain
//! for a role: a reviewer wants `git_diff` but not `git_commit`, and a coder
//! wants none of the agent tools. A profile is therefore an allowlist of tool
//! *names*, applied on top of whatever the execution settings already allow —
//! it only ever takes tools away, it never turns a disabled group back on.
//!
//! Anything not named is dropped, MCP tools included: a profile is the whole
//! tool set, not a filter over the native half of it. That is the point of the
//! issue behind it — a 4B coder was handed 53 tool schemas (~13k tokens)
//! before it could read a file.

use super::tool_registry::ToolAvailability;

/// The read-only set every profile starts from: look at the tree, read files,
/// search, read the git history, run the todo protocol, and ask the leader.
///
/// `git_merge` belongs here too once AGE-404 adds it to the git tool; the tool
/// does not exist yet, so naming it now would only be a dead string.
const READ_SET: &[&str] = &[
    "read_file",
    "list_directory",
    "glob_search",
    "search_code",
    "git_status",
    "git_log",
    "git_diff",
    "read_skill",
    "write_todos",
    "update_todo",
    "verify_completion",
    // Every profile keeps it: dropping it would cut the input-required chain
    // that parks a worker's question on its leader (AGE-306).
    "ask_user",
];

/// Delegation: what a leader needs and nothing else.
const AGENT_TOOLS: &[&str] = &["list_agents", "invoke_agent"];

/// Editing the workspace.
const FS_WRITE: &[&str] = &[
    "write_file",
    "apply_diff",
    "create_directory",
    "delete_file",
    "move_file",
    "final_answer",
];

/// The persistent shell session — how a reviewer runs the tests.
const SHELL: &[&str] = &["shell_execute", "shell_cd", "shell_set_env", "shell_status"];

/// The writing half of the git tool.
const GIT_WRITE: &[&str] = &[
    "git_add",
    "git_create_branch",
    "git_switch_branch",
    "git_commit",
];

/// Running code in the sandbox.
const CODE_EXEC: &[&str] = &["execute_code"];

/// One named tool set, as a list of groups so the three profiles share their
/// common parts rather than repeating them.
#[derive(Debug, PartialEq, Eq)]
pub struct ToolProfile {
    name: &'static str,
    groups: &'static [&'static [&'static str]],
}

impl ToolProfile {
    /// The name settings and `--tools` address it by.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Whether a tool of this name survives the profile.
    pub fn allows(&self, tool: &str) -> bool {
        self.groups.iter().any(|group| group.contains(&tool))
    }

    /// Every name the profile allows, in declaration order.
    pub fn tool_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.groups.iter().flat_map(|group| group.iter().copied())
    }

    /// The availability the preamble should describe: a tool group with no
    /// allowed member is gone entirely, so the prompt stops advertising a
    /// section of tools the agent cannot call.
    ///
    /// Within a group that survives, the prose still names every tool of the
    /// group; [`build_preamble`](super::preamble_builder::build_preamble)
    /// spells out the exact set alongside it.
    pub(super) fn narrow(&self, tools: &ToolAvailability) -> ToolAvailability {
        narrow_availability(tools, |name| self.allows(name))
    }
}

/// [`ToolProfile::narrow`], over any predicate — the tests probe it one tool
/// name at a time to prove the table below covers every name the registry can
/// produce.
fn narrow_availability(tools: &ToolAvailability, allow: impl Fn(&str) -> bool) -> ToolAvailability {
    let keep = |flag: bool, names: &[&str]| flag && names.iter().any(|name| allow(name));
    ToolAvailability {
        fs_read: keep(
            tools.fs_read,
            &["read_file", "read_binary", "list_directory", "glob_search"],
        ),
        doc_retriever: keep(tools.doc_retriever, &["doc_retriever"]),
        fs_write: keep(tools.fs_write, FS_WRITE),
        list_mcp: keep(tools.list_mcp, &["list_mcp_services"]),
        fetch: keep(tools.fetch, &["fetch"]),
        shell: keep(tools.shell, SHELL),
        git: keep(
            tools.git,
            &[
                "git_status",
                "git_diff",
                "git_log",
                "git_add",
                "git_create_branch",
                "git_switch_branch",
                "git_commit",
            ],
        ),
        search: keep(
            tools.search,
            &["search_code", "find_files", "find_definition"],
        ),
        add_attachment: keep(tools.add_attachment, &["add_attachment"]),
        excel_read: keep(tools.excel_read, &["read_excel"]),
        excel_write: keep(tools.excel_write, &["write_excel", "edit_excel"]),
        docx_read: keep(tools.docx_read, &["read_docx"]),
        docx_write: keep(tools.docx_write, &["write_docx"]),
        pptx_read: keep(tools.pptx_read, &["read_pptx"]),
        pptx_write: keep(tools.pptx_write, &["write_pptx"]),
        pdf_to_image: keep(tools.pdf_to_image, &["pdf_to_image"]),
        pdf_info: keep(tools.pdf_info, &["pdf_info"]),
        pdf_extract_text: keep(tools.pdf_extract_text, &["pdf_extract_text"]),
        data_query: keep(
            tools.data_query,
            &[
                "query_data",
                "describe_data",
                "profile_data",
                "file_structure_detector",
            ],
        ),
        compile_typst: keep(tools.compile_typst, &["compile_typst"]),
        execute_code: keep(tools.execute_code, CODE_EXEC),
        memory: keep(tools.memory, &["remember", "save_skill", "search_memory"]),
        search_web: keep(tools.search_web, &["search_web"]),
        browser: keep(
            tools.browser,
            &[
                "browser_navigate",
                "browser_snapshot",
                "browser_screenshot",
                "browser_console",
                "browser_network",
                "browser_resize",
            ],
        ),
        browser_use: keep(tools.browser_use, &["browser_use"]),
        daytona: keep(tools.daytona, &["daytona_run"]),
        publish_module: keep(tools.publish_module, &["publish_wasm_module"]),
        ask_user: keep(tools.ask_user, &["ask_user"]),
    }
}

/// A leader: read the repository, plan, and delegate. It edits nothing itself.
pub static COORDINATOR: ToolProfile = ToolProfile {
    name: "coordinator",
    groups: &[READ_SET, AGENT_TOOLS],
};

/// A worker that writes code: the read set plus everything needed to change
/// the tree and prove it builds. It does not delegate further.
pub static CODER: ToolProfile = ToolProfile {
    name: "coder",
    groups: &[READ_SET, FS_WRITE, SHELL, GIT_WRITE, CODE_EXEC],
};

/// A worker that judges someone else's work: the read set plus a shell to run
/// the tests with. No writes, no commits, no delegation.
pub static REVIEWER: ToolProfile = ToolProfile {
    name: "reviewer",
    groups: &[READ_SET, SHELL],
};

/// Every profile, in the order `--tools` documents them.
pub static TOOL_PROFILES: &[&ToolProfile] = &[&COORDINATOR, &CODER, &REVIEWER];

/// The profile of that name, or `None` — the caller decides whether an unknown
/// name is a warning or a hard error.
pub fn tool_profile(name: &str) -> Option<&'static ToolProfile> {
    TOOL_PROFILES
        .iter()
        .copied()
        .find(|profile| profile.name == name)
}

/// The profile names, for a `--tools` error message.
pub fn tool_profile_names() -> Vec<&'static str> {
    TOOL_PROFILES.iter().map(|profile| profile.name).collect()
}

#[cfg(test)]
mod tests {
    use super::super::tool_registry::active_native_tool_names;
    use super::*;
    use std::collections::HashSet;

    fn everything() -> ToolAvailability {
        ToolAvailability {
            fs_read: true,
            doc_retriever: true,
            fs_write: true,
            list_mcp: true,
            fetch: true,
            shell: true,
            git: true,
            search: true,
            add_attachment: true,
            excel_read: true,
            excel_write: true,
            docx_read: true,
            docx_write: true,
            pptx_read: true,
            pptx_write: true,
            pdf_to_image: true,
            pdf_info: true,
            pdf_extract_text: true,
            data_query: true,
            compile_typst: true,
            execute_code: true,
            memory: true,
            search_web: true,
            browser: true,
            browser_use: true,
            daytona: true,
            publish_module: true,
            ask_user: true,
        }
    }

    #[test]
    fn profiles_are_addressable_by_name() {
        assert_eq!(tool_profile("reviewer"), Some(&REVIEWER));
        assert_eq!(tool_profile("coder"), Some(&CODER));
        assert_eq!(tool_profile("coordinator"), Some(&COORDINATOR));
        assert_eq!(tool_profile("Reviewer"), None, "names are exact");
        assert_eq!(tool_profile("nope"), None);
    }

    /// The three profiles the issue specifies, by what they must and must not
    /// let through. A reviewer that can commit is the failure this exists to
    /// prevent.
    #[test]
    fn each_profile_allows_its_own_tools_and_nothing_else() {
        for tool in ["read_file", "search_code", "git_log", "write_todos"] {
            for profile in TOOL_PROFILES {
                assert!(profile.allows(tool), "{} lost {tool}", profile.name());
            }
        }

        assert!(COORDINATOR.allows("invoke_agent"));
        assert!(!COORDINATOR.allows("write_file"));
        assert!(!COORDINATOR.allows("shell_execute"));
        assert!(!COORDINATOR.allows("git_commit"));

        assert!(CODER.allows("write_file"));
        assert!(CODER.allows("shell_execute"));
        assert!(CODER.allows("git_commit"));
        assert!(CODER.allows("execute_code"));
        assert!(!CODER.allows("invoke_agent"), "a coder does not delegate");
        assert!(!CODER.allows("list_agents"));

        assert!(REVIEWER.allows("shell_execute"), "it runs the tests");
        assert!(REVIEWER.allows("git_diff"));
        assert!(!REVIEWER.allows("write_file"));
        assert!(!REVIEWER.allows("apply_diff"));
        assert!(!REVIEWER.allows("git_add"));
        assert!(!REVIEWER.allows("git_commit"));
        assert!(!REVIEWER.allows("invoke_agent"));
    }

    /// A profile naming a tool that does not exist would silently allow
    /// nothing, so every name has to be one the factory can actually build.
    #[test]
    fn every_profile_names_only_real_tools() {
        let real = active_native_tool_names(&everything());
        for profile in TOOL_PROFILES {
            for tool in profile.tool_names() {
                assert!(
                    real.contains(tool),
                    "{} names {tool}, which no tool registers",
                    profile.name()
                );
            }
        }
    }

    /// The narrowing table has to cover every name the registry can produce:
    /// a name missing from it means a profile that allows only that tool
    /// switches its whole group off, and the preamble stops describing a tool
    /// the agent still has.
    #[test]
    fn every_registry_tool_name_survives_a_profile_that_allows_only_it() {
        let baseline = active_native_tool_names(&ToolAvailability::default());
        let all = active_native_tool_names(&everything());
        for tool in all.difference(&baseline) {
            let narrowed = narrow_availability(&everything(), |name| name == tool);
            assert!(
                active_native_tool_names(&narrowed).contains(tool),
                "{tool} is missing from the narrowing table"
            );
        }
    }

    /// What a reviewer's prompt should stop describing: no write section at
    /// all, while the groups it keeps stay on.
    #[test]
    fn narrowing_drops_a_group_with_no_allowed_member() {
        let narrowed = REVIEWER.narrow(&everything());
        assert!(!narrowed.fs_write, "a reviewer cannot write");
        assert!(!narrowed.execute_code);
        assert!(!narrowed.memory);
        assert!(!narrowed.browser);
        assert!(!narrowed.list_mcp);
        assert!(narrowed.ask_user, "a worker can still ask its leader");
        assert!(narrowed.fs_read);
        assert!(narrowed.shell);
        assert!(narrowed.search);
        assert!(narrowed.git, "git_diff keeps the group");
    }

    /// A profile never turns a group back on: it is applied on top of the
    /// execution settings, not instead of them.
    #[test]
    fn narrowing_never_adds_a_group_the_settings_switched_off() {
        let nothing = ToolAvailability::default();
        let narrowed = CODER.narrow(&nothing);
        let names: HashSet<String> = active_native_tool_names(&narrowed);
        assert_eq!(names, active_native_tool_names(&nothing));
    }
}
