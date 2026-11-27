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

/// The read-only set every profile starts from: look at the tree, read files and
/// images, search, read the git history (`git_diff` takes a `base..head` range, so a
/// reviewer reads a worker's branch without a shell; AGE-404), and ask the
/// leader.
const READ_SET: &[&str] = &[
    "read_file",
    "list_directory",
    "glob_search",
    "search_code",
    "git_status",
    "git_log",
    "git_diff",
    // Looking at an image (a screenshot, a chart) with its own model's eyes;
    // the tool exists only for a model that takes images (AGE-1460).
    "view_image",
    "read_skill",
    // Every profile keeps it: dropping it would cut the input-required chain
    // that parks a worker's question on its leader (AGE-306).
    "ask_user",
];

/// The todo plan: for the roles that run open-ended multi-step work — the
/// unprofiled main agent and a leader. A worker gets one bounded task from
/// its leader and does not plan it again (AGE-479).
const TODO_PLAN: &[&str] = &["write_todos", "verify_completion"];

/// Delegation. No profile names these: a spec's `swarm.delegates_to`
/// decides whether an agent has them (PL-S2 DP-1), so every profile lets
/// them through and `ToolAvailability::agents` gates them.
pub(super) const AGENT_TOOLS: &[&str] = &["list_agents", "invoke_agent"];

/// Messaging the agent that gave this one its task (tree messages, TM-1).
/// Not delegation: every role has an owner to tell, so every profile keeps
/// it, and it exists only for a worker with a broker-made connection.
const MESSAGES: &[&str] = &["send_message"];

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
    "git_merge",
];

/// Taking a worker's branch: the one write a leader without a shell needs
/// (AGE-404). Not in the read set on purpose — a reviewer merges nothing.
const GIT_MERGE: &[&str] = &["git_merge"];

/// Running code in the sandbox.
const CODE_EXEC: &[&str] = &["execute_code"];

/// Querying CSV/Parquet/JSON/Excel files directly with DuckDB SQL, without
/// loading them through `execute_code` first.
const DATA_QUERY: &[&str] = &[
    "query_data",
    "describe_data",
    "profile_data",
    "file_structure_detector",
];

/// Exact decimal arithmetic and statistics (AGE-942). Not part of
/// `DATA_QUERY`: it is in every build, so a hosted worker built without
/// DuckDB still has a calculator.
const CALCULATE: &[&str] = &["calculate"];

/// Writing to the embedding-backed memory/skill store. Writes, so `CODER`
/// only — `REVIEWER`'s whole design invariant is no writes (AGE-456).
const MEMORY: &[&str] = &["remember", "save_skill", "search_memory"];

/// Reading the open web: a search, and the pages it finds (AGE-910). Still
/// gated by the execution settings' internet access: a profile never turns
/// a group back on.
const WEB: &[&str] = &["search_web", "fetch"];

/// Showing an image file to the user (AGE-1502). A hosted worker's image no
/// longer rides back on its terminal status (AGE-1747); it stays in the
/// workspace. Offered only where the host builds the tool (it needs a place
/// to queue the file).
const ATTACH: &[&str] = &["add_attachment"];

/// Reading Word, Excel, PowerPoint and PDF files (AGE-1775): how a worker
/// handed a document reads it without a shell. They exist only when
/// chatty-core is built with the `excel`/`docx`/`pptx`/`pdf` features and
/// filesystem reads are on.
const DOC_READ: &[&str] = &[
    "read_docx",
    "read_excel",
    "read_pptx",
    "pdf_extract_text",
    "pdf_info",
    "pdf_to_image",
];

/// Writing Word, Excel and PowerPoint files (AGE-1775). Same features, and
/// filesystem writes on.
const DOC_WRITE: &[&str] = &["write_docx", "write_excel", "edit_excel", "write_pptx"];

/// Zip archives (AGE-1785): how a worker handed a `.zip` lists it and
/// unpacks it into a fresh folder, to read the members with [`DOC_READ`].
/// They exist only when chatty-core is built with the `archive` feature;
/// `list_archive` needs filesystem reads, `extract_archive` writes. The
/// extraction only ever creates a new folder: it overwrites nothing.
const ARCHIVE: &[&str] = &["list_archive", "extract_archive"];

/// The built-in browser (AGE-1912): the eight tools of `tools/browser_tools.rs`.
/// They exist only when `browser_enabled` and a workspace are set; a profile
/// never turns the group on.
const BROWSER: &[&str] = &[
    "browser_navigate",
    "browser_snapshot",
    "browser_screenshot",
    "browser_console",
    "browser_network",
    "browser_resize",
    "browser_click",
    "browser_type",
    "browser_select",
];

/// Typst markup to PDF (AGE-946): the typesetter's alone. Every hosted role
/// shares one guest image with `math-render` compiled in, so the profile is
/// what keeps the compiler away from the roles that have no use for it.
const TYPESET: &[&str] = &["compile_typst"];

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

    /// Whether a tool of this name survives the profile. The delegation
    /// tools always do: the spec decides those, not the profile.
    pub fn allows(&self, tool: &str) -> bool {
        AGENT_TOOLS.contains(&tool) || self.groups.iter().any(|group| group.contains(&tool))
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
pub(super) fn narrow_availability(
    tools: &ToolAvailability,
    allow: impl Fn(&str) -> bool,
) -> ToolAvailability {
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
                "git_merge",
            ],
        ),
        search: keep(
            tools.search,
            &["search_code", "find_files", "find_definition"],
        ),
        add_attachment: keep(tools.add_attachment, &["add_attachment"]),
        view_image: keep(tools.view_image, &["view_image"]),
        excel_read: keep(tools.excel_read, &["read_excel"]),
        excel_write: keep(tools.excel_write, &["write_excel", "edit_excel"]),
        docx_read: keep(tools.docx_read, &["read_docx"]),
        docx_write: keep(tools.docx_write, &["write_docx"]),
        pptx_read: keep(tools.pptx_read, &["read_pptx"]),
        pptx_write: keep(tools.pptx_write, &["write_pptx"]),
        pdf_to_image: keep(tools.pdf_to_image, &["pdf_to_image"]),
        pdf_info: keep(tools.pdf_info, &["pdf_info"]),
        pdf_extract_text: keep(tools.pdf_extract_text, &["pdf_extract_text"]),
        archive_list: keep(tools.archive_list, &["list_archive"]),
        archive_extract: keep(tools.archive_extract, &["extract_archive"]),
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
        agents: keep(tools.agents, AGENT_TOOLS),
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
                "browser_click",
                "browser_type",
                "browser_select",
            ],
        ),
        browser_use: keep(tools.browser_use, &["browser_use"]),
        daytona: keep(tools.daytona, &["daytona_run"]),
        publish_module: keep(tools.publish_module, &["publish_wasm_module"]),
        ask_user: keep(tools.ask_user, &["ask_user"]),
        terminal: keep(tools.terminal, &["terminal_read"]),
        terminal_run: keep(tools.terminal_run, &["terminal_run"]),
        send_message: keep(tools.send_message, MESSAGES),
    }
}

/// A leader: read the repository, plan, and merge what a worker hands back.
/// It edits nothing itself. It delegates only if its spec says whom to.
pub static COORDINATOR: ToolProfile = ToolProfile {
    name: "coordinator",
    groups: &[READ_SET, TODO_PLAN, GIT_MERGE, MESSAGES],
};

/// A worker that writes code: the read set plus everything needed to change
/// the tree and prove it builds, plus querying data files directly with SQL,
/// plus building and consulting a playbook/skill memory (AGE-456), plus
/// reading and writing documents (AGE-1775) and opening zip archives
/// (AGE-1785).
pub static CODER: ToolProfile = ToolProfile {
    name: "coder",
    groups: &[
        READ_SET, FS_WRITE, SHELL, GIT_WRITE, CODE_EXEC, DATA_QUERY, CALCULATE, MEMORY, ATTACH,
        DOC_READ, DOC_WRITE, ARCHIVE, MESSAGES,
    ],
};

/// A worker that judges someone else's work: the read set plus a shell to run
/// the tests with, plus the same data-query tool a coder used, so it can
/// independently re-derive a claimed data-derived value instead of only
/// judging plausibility, `calculate` to recompute a figure exactly, the
/// document readers (AGE-1775) and the zip tools, so it can open the archive
/// the task came with (AGE-1785). No writes, no commits: `extract_archive`
/// only creates a new folder of the archive's own files.
pub static REVIEWER: ToolProfile = ToolProfile {
    name: "reviewer",
    groups: &[
        READ_SET, SHELL, DATA_QUERY, CALCULATE, DOC_READ, ARCHIVE, MESSAGES,
    ],
};

/// A worker that turns approved texts into one PDF (AGE-946): the read set,
/// the filesystem-write tools for its own workspace (a long text arrives as
/// a file it may have to fix in place), `compile_typst`, and `calculate`.
/// No shell, no git, no code execution.
pub static TYPESETTER: ToolProfile = ToolProfile {
    name: "typesetter",
    groups: &[READ_SET, FS_WRITE, TYPESET, CALCULATE, MESSAGES],
};

/// A worker that researches the open web (AGE-910): the read set, web
/// search and fetch, and `calculate`. It writes nothing, runs no shell or
/// code, and plans nothing: its leader hands it one bounded sub-question.
pub static WEB_RESEARCHER: ToolProfile = ToolProfile {
    name: "web-researcher",
    groups: &[READ_SET, WEB, CALCULATE, MESSAGES],
};

/// A hosted worker that operates a web page (AGE-1912): the read set, the
/// filesystem-write tools for its own workspace, the browser, attachments.
/// No shell, no git, no code execution.
pub static OPERATOR: ToolProfile = ToolProfile {
    name: "operator",
    groups: &[READ_SET, FS_WRITE, BROWSER, ATTACH, MESSAGES],
};

/// Every profile, in the order `--tools` documents them.
pub static TOOL_PROFILES: &[&ToolProfile] = &[
    &COORDINATOR,
    &CODER,
    &REVIEWER,
    &TYPESETTER,
    &WEB_RESEARCHER,
    &OPERATOR,
];

/// Joins profile names into their union: `coder+coordinator` allows what
/// either allows (TD-1, AGE-692). It is how a team's derived single agent
/// holds every role's tools in one spec.
pub const PROFILE_UNION_SEPARATOR: char = '+';

/// The profile of that name, or `None` — the caller decides whether an unknown
/// name is a warning or a hard error. Profiles joined by
/// [`PROFILE_UNION_SEPARATOR`], each named once, are their union.
pub fn tool_profile(name: &str) -> Option<&'static ToolProfile> {
    if name.contains(PROFILE_UNION_SEPARATOR) {
        return union_profile(name);
    }
    TOOL_PROFILES
        .iter()
        .copied()
        .find(|profile| profile.name == name)
}

/// The union [`tool_profile`] names, built once per set of parts and kept
/// for the process, since a role holds a `&'static ToolProfile`. Every part
/// must be a profile, named once, so there are only a few dozen such sets.
/// Its name is the canonical one, the parts sorted (`coder+coordinator`
/// for `coordinator+coder` too; AGE-1137).
fn union_profile(name: &str) -> Option<&'static ToolProfile> {
    static UNIONS: std::sync::LazyLock<parking_lot::Mutex<Vec<&'static ToolProfile>>> =
        std::sync::LazyLock::new(Default::default);

    let mut parts: Vec<&'static ToolProfile> = Vec::new();
    for part in name.split(PROFILE_UNION_SEPARATOR) {
        let profile = TOOL_PROFILES.iter().copied().find(|p| p.name == part)?;
        if parts.contains(&profile) {
            return None;
        }
        parts.push(profile);
    }
    parts.sort_by_key(|profile| profile.name);
    let name = parts
        .iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>()
        .join(&PROFILE_UNION_SEPARATOR.to_string());
    let mut unions = UNIONS.lock();
    if let Some(known) = unions.iter().find(|union| union.name == name) {
        return Some(known);
    }
    let groups: Vec<&'static [&'static str]> = parts
        .iter()
        .flat_map(|profile| profile.groups.iter().copied())
        .collect();
    let union: &'static ToolProfile = Box::leak(Box::new(ToolProfile {
        name: Box::leak(name.into_boxed_str()),
        groups: Box::leak(groups.into_boxed_slice()),
    }));
    unions.push(union);
    Some(union)
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
            view_image: true,
            excel_read: true,
            excel_write: true,
            docx_read: true,
            docx_write: true,
            pptx_read: true,
            pptx_write: true,
            pdf_to_image: true,
            pdf_info: true,
            pdf_extract_text: true,
            archive_list: true,
            archive_extract: true,
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
            terminal: true,
            terminal_run: true,
            agents: true,
            send_message: true,
        }
    }

    #[test]
    fn profiles_are_addressable_by_name() {
        assert_eq!(tool_profile("reviewer"), Some(&REVIEWER));
        assert_eq!(tool_profile("coder"), Some(&CODER));
        assert_eq!(tool_profile("coordinator"), Some(&COORDINATOR));
        assert_eq!(tool_profile("typesetter"), Some(&TYPESETTER));
        assert_eq!(tool_profile("Reviewer"), None, "names are exact");
        assert_eq!(tool_profile("nope"), None);
    }

    /// `a+b` is the union of profiles `a` and `b` (TD-1, AGE-692): it allows
    /// exactly what either allows, and every part must be a profile, once.
    #[test]
    fn profiles_joined_by_plus_are_their_union() {
        let union = tool_profile("coordinator+reviewer").expect("a union of two profiles");
        assert_eq!(union.name(), "coordinator+reviewer");
        for profile in TOOL_PROFILES {
            for name in profile.tool_names() {
                assert_eq!(
                    union.allows(name),
                    COORDINATOR.allows(name) || REVIEWER.allows(name),
                    "{name}"
                );
            }
        }
        assert!(union.allows("write_todos") && union.allows("shell_execute"));
        assert!(!union.allows("git_commit") && !union.allows("write_file"));
        assert!(
            std::ptr::eq(union, tool_profile("coordinator+reviewer").unwrap()),
            "one profile per name, built once"
        );
        let reversed = tool_profile("reviewer+coordinator").expect("the same union");
        assert!(
            std::ptr::eq(union, reversed),
            "the parts' order does not matter (AGE-1137)"
        );
        assert_eq!(
            reversed.name(),
            "coordinator+reviewer",
            "the canonical name"
        );
        assert_eq!(tool_profile("coder+nope"), None);
        assert_eq!(tool_profile("coder+coder"), None, "a part named twice");
        assert_eq!(tool_profile("coder+"), None);
    }

    /// The three profiles the issue specifies, by what they must and must not
    /// let through. A reviewer that can commit is the failure this exists to
    /// prevent.
    #[test]
    fn each_profile_allows_its_own_tools_and_nothing_else() {
        for tool in [
            "read_file",
            "search_code",
            "git_log",
            "ask_user",
            "send_message",
        ] {
            for profile in TOOL_PROFILES {
                assert!(profile.allows(tool), "{} lost {tool}", profile.name());
            }
        }

        for tool in ["write_todos", "verify_completion"] {
            assert!(
                COORDINATOR.allows(tool),
                "a leader plans open-ended work with {tool}"
            );
            assert!(
                !CODER.allows(tool),
                "a worker gets one bounded task and does not plan it again (AGE-479)"
            );
            assert!(!REVIEWER.allows(tool));
        }

        assert!(!COORDINATOR.allows("write_file"));
        assert!(!COORDINATOR.allows("shell_execute"));
        assert!(!COORDINATOR.allows("git_commit"));
        assert!(
            COORDINATOR.allows("git_merge"),
            "a leader without a shell takes a worker's branch with it (AGE-404)"
        );

        assert!(CODER.allows("write_file"));
        assert!(CODER.allows("shell_execute"));
        assert!(CODER.allows("git_commit"));
        assert!(CODER.allows("git_merge"));
        assert!(CODER.allows("execute_code"));
        assert!(
            CODER.allows("add_attachment"),
            "a coder shows its screenshot to the user (AGE-1502)"
        );
        assert!(CODER.narrow(&everything()).add_attachment);

        assert!(REVIEWER.allows("shell_execute"), "it runs the tests");
        assert!(REVIEWER.allows("git_diff"));
        assert!(!REVIEWER.allows("write_file"));
        assert!(!REVIEWER.allows("apply_diff"));
        assert!(!REVIEWER.allows("git_add"));
        assert!(!REVIEWER.allows("git_commit"));
        assert!(!REVIEWER.allows("git_merge"), "a reviewer merges nothing");

        // Delegation is the spec's `delegates_to`, not the profile's (DP-1):
        // no profile names the tools, and none takes them away.
        for profile in TOOL_PROFILES {
            assert!(!profile.tool_names().any(|tool| AGENT_TOOLS.contains(&tool)));
            for tool in AGENT_TOOLS {
                assert!(profile.allows(tool), "{} drops {tool}", profile.name());
            }
        }

        for tool in [
            "query_data",
            "describe_data",
            "profile_data",
            "file_structure_detector",
        ] {
            assert!(CODER.allows(tool), "a coder can query data files with SQL");
            assert!(
                REVIEWER.allows(tool),
                "a reviewer can independently re-query the data a coder claimed to derive from"
            );
            assert!(
                !COORDINATOR.allows(tool),
                "a leader delegates data analysis, it does not do it itself"
            );
        }

        assert!(
            CODER.allows("calculate"),
            "an analyst on the coder profile computes every number"
        );
        assert!(
            REVIEWER.allows("calculate"),
            "a reviewer recomputes figures without DuckDB (AGE-942)"
        );
        assert!(
            !COORDINATOR.allows("calculate"),
            "a leader delegates the arithmetic with the analysis"
        );

        for tool in ["remember", "save_skill", "search_memory"] {
            assert!(
                CODER.allows(tool),
                "a coder can build and consult a playbook/skill memory"
            );
            assert!(
                !REVIEWER.allows(tool),
                "a reviewer writes nothing, memory included"
            );
            assert!(
                !COORDINATOR.allows(tool),
                "no evidence yet a leader needs to write memory itself"
            );
        }
    }

    /// AGE-946: every hosted role runs in one guest image with the Typst
    /// compiler in it, so the profile is the only thing that decides who
    /// may call it — the typesetter, and nobody else.
    #[test]
    fn only_the_typesetter_compiles_typst() {
        for profile in TOOL_PROFILES {
            assert_eq!(
                profile.allows("compile_typst"),
                profile.name() == "typesetter",
                "{} and compile_typst",
                profile.name()
            );
        }
        for tool in ["read_file", "write_file", "apply_diff", "calculate"] {
            assert!(TYPESETTER.allows(tool), "a typesetter needs {tool}");
        }
        for tool in [
            "shell_execute",
            "git_commit",
            "execute_code",
            "query_data",
            "fetch",
            "remember",
            "write_todos",
        ] {
            assert!(!TYPESETTER.allows(tool), "a typesetter has no {tool}");
        }
        let narrowed = TYPESETTER.narrow(&everything());
        assert!(narrowed.compile_typst);
        assert!(narrowed.fs_write);
        assert!(!narrowed.shell);
        assert!(!narrowed.data_query);
        for profile in [&COORDINATOR, &CODER, &REVIEWER] {
            assert!(!profile.narrow(&everything()).compile_typst);
        }
    }

    /// AGE-910: the web researcher is the one profile that reaches the open
    /// web, and it writes nothing, runs nothing and plans nothing.
    #[test]
    fn only_the_web_researcher_searches_and_fetches() {
        assert_eq!(tool_profile("web-researcher"), Some(&WEB_RESEARCHER));
        for profile in TOOL_PROFILES {
            for tool in ["search_web", "fetch"] {
                assert_eq!(
                    profile.allows(tool),
                    profile.name() == "web-researcher",
                    "{} and {tool}",
                    profile.name()
                );
            }
        }
        for tool in ["read_file", "calculate", "ask_user", "send_message"] {
            assert!(WEB_RESEARCHER.allows(tool), "a web researcher needs {tool}");
        }
        for tool in [
            "write_file",
            "shell_execute",
            "execute_code",
            "git_commit",
            "write_todos",
            "remember",
            "browser_navigate",
            "compile_typst",
        ] {
            assert!(
                !WEB_RESEARCHER.allows(tool),
                "a web researcher has no {tool}"
            );
        }
        let narrowed = WEB_RESEARCHER.narrow(&everything());
        assert!(narrowed.fetch && narrowed.search_web);
        assert!(!narrowed.fs_write && !narrowed.shell && !narrowed.browser);
    }

    /// AGE-1775: a file-input lease (a `.xlsx`, `.docx`, `.pptx` or `.pdf`
    /// next to the task) is worked by a profiled worker, so the profile has to
    /// let the document tools through. A coder reads and writes them; a
    /// reviewer only reads them, since it writes nothing (AGE-456). The
    /// settings and the build's features still decide whether they exist.
    #[test]
    fn coder_and_reviewer_offer_the_document_tools() {
        let reads = [
            "read_docx",
            "read_excel",
            "read_pptx",
            "pdf_extract_text",
            "pdf_info",
            "pdf_to_image",
        ];
        let writes = ["write_docx", "write_excel", "edit_excel", "write_pptx"];
        for tool in reads {
            assert!(CODER.allows(tool), "a coder reads {tool}");
            assert!(REVIEWER.allows(tool), "a reviewer reads {tool}");
            assert!(!COORDINATOR.allows(tool), "a leader delegates {tool}");
        }
        for tool in writes {
            assert!(CODER.allows(tool), "a coder writes {tool}");
            assert!(!REVIEWER.allows(tool), "a reviewer writes nothing: {tool}");
            assert!(!COORDINATOR.allows(tool), "a leader delegates {tool}");
        }
        let coder = CODER.narrow(&everything());
        assert!(coder.excel_read && coder.excel_write && coder.docx_read && coder.docx_write);
        assert!(coder.pptx_read && coder.pptx_write);
        assert!(coder.pdf_extract_text && coder.pdf_info && coder.pdf_to_image);
        let reviewer = REVIEWER.narrow(&everything());
        assert!(reviewer.excel_read && reviewer.docx_read && reviewer.pptx_read);
        assert!(!reviewer.excel_write && !reviewer.docx_write && !reviewer.pptx_write);
    }

    /// AGE-1785: a file-input lease with a `.zip` next to the task is worked
    /// by a profiled worker with no shell, so the coder and the reviewer both
    /// get the zip tools; a leader delegates them.
    #[test]
    fn coder_and_reviewer_offer_the_archive_tools() {
        for tool in ["list_archive", "extract_archive"] {
            assert!(CODER.allows(tool), "a coder opens archives: {tool}");
            assert!(REVIEWER.allows(tool), "a reviewer opens archives: {tool}");
            assert!(!COORDINATOR.allows(tool), "a leader delegates {tool}");
            assert!(
                !TYPESETTER.allows(tool),
                "a typesetter has no use for {tool}"
            );
        }
        for profile in [&CODER, &REVIEWER] {
            let narrowed = profile.narrow(&everything());
            assert!(
                narrowed.archive_list && narrowed.archive_extract,
                "{}",
                profile.name()
            );
        }
        let leader = COORDINATOR.narrow(&everything());
        assert!(!leader.archive_list && !leader.archive_extract);
    }

    /// The tools a built `coder` agent offers the model: with the document
    /// features compiled in it has `read_excel` and `write_docx`; without
    /// them the build is what it was, with neither (AGE-1775).
    #[tokio::test]
    async fn a_built_coder_offers_document_tools_only_when_the_features_are_built() {
        use super::super::{AgentBuildContext, AgentClient, AgentServices};
        use crate::models::clarification_store::ClarificationStore;
        use crate::models::execution_approval_store::ExecutionApprovalStore;
        use crate::models::write_approval_store::WriteApprovalStore;
        use crate::settings::models::ExecutionSettingsModel;
        use crate::settings::models::models_store::ModelConfig;
        use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

        let _ = crate::init_repositories();
        let workspace = tempfile::tempdir().expect("a workspace");
        let mut context = AgentBuildContext {
            pending_approvals: Some(ExecutionApprovalStore::new().get_pending_approvals()),
            pending_clarifications: Some(ClarificationStore::new().get_pending_clarifications()),
            pending_write_approvals: Some(WriteApprovalStore::new().get_pending_approvals()),
            ..AgentBuildContext::from_services(AgentServices {
                exec_settings: Some(ExecutionSettingsModel {
                    workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
                    ..ExecutionSettingsModel::default()
                }),
                ..AgentServices::default()
            })
        };
        context.role.profile = Some(&CODER);
        let built = AgentClient::from_model_config_with_tools(
            &ModelConfig::new(
                "age-1775".to_string(),
                "Profile Fixture".to_string(),
                ProviderType::Ollama,
                "llama3.2".to_string(),
            ),
            &ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
            context,
        )
        .await
        .expect("a coder builds without network access");
        let offered: HashSet<String> = built
            .client
            .agent
            .tool_definitions(None)
            .await
            .expect("tool definitions resolve")
            .into_iter()
            .map(|definition| definition.name.to_string())
            .collect();

        assert!(offered.contains("read_file"), "{offered:?}");
        assert_eq!(
            offered.contains("read_excel"),
            cfg!(feature = "excel"),
            "{offered:?}"
        );
        assert_eq!(
            offered.contains("write_docx"),
            cfg!(feature = "docx"),
            "{offered:?}"
        );
        // AGE-1785: the zip tools, under the `archive` feature.
        for tool in ["list_archive", "extract_archive"] {
            assert_eq!(
                offered.contains(tool),
                cfg!(feature = "archive"),
                "{tool}: {offered:?}"
            );
        }
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
        assert!(
            narrowed.data_query,
            "a reviewer can query data files to check a claimed value"
        );
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

    /// The operator drives a web page and writes its own workspace: the
    /// browser tools, no shell, no git, no code execution (AGE-1912).
    #[test]
    fn operator_has_the_browser_and_no_shell_or_git() {
        assert_eq!(tool_profile("operator"), Some(&OPERATOR));
        assert!(tool_profile_names().contains(&"operator"));
        for tool in BROWSER {
            assert!(OPERATOR.allows(tool), "an operator needs {tool}");
        }
        for tool in ["write_file", "apply_diff", "read_file", "add_attachment"] {
            assert!(OPERATOR.allows(tool), "an operator needs {tool}");
        }
        // No shell, no git writes, no code execution. (The read set's
        // read-only git history is shared by every profile.)
        for tool in SHELL.iter().chain(GIT_WRITE) {
            assert!(!OPERATOR.allows(tool), "an operator has no {tool}");
        }
        assert!(!OPERATOR.allows("execute_code"));
        let names: Vec<_> = OPERATOR.tool_names().collect();
        assert!(!names.iter().any(|t| t.starts_with("shell")));
        assert!(
            !names
                .iter()
                .any(|t| t.starts_with("git") && !READ_SET.contains(t))
        );
    }
}
