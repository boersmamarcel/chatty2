//! AGE-941: a headless `--team` leader that outgrows its context window
//! still writes its final report from what its workers actually returned.
//!
//! The lead delegates a draft and a review per chapter to two worker
//! processes (`data-analyst`, `reviewer`) on the scripted fake model. Its
//! window is small enough that the context guard compacts and shapes the
//! older worker results out of its requests, the way the AGE-937 soak's
//! 32k-window leader lost them. The request its final answer is written
//! from must still carry every chapter's worked-example figure — in the
//! results ledger — and a final answer written from that request contains
//! no figure absent from the workers' results.

use std::path::Path;
use std::process::Command;

use chatty_core::services::results_ledger::{LEDGER_HEADER, figures_not_in};
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::json;

const LEAD_MODEL: &str = "ledger-lead-model";
const ANALYST_MODEL: &str = "ledger-analyst-model";
const REVIEWER_MODEL: &str = "ledger-reviewer-model";
const CHAPTERS: usize = 6;
/// The lead's context window, in tokens: its preamble and tool schemas
/// plus room for about two chapters' worth of results.
const LEAD_WINDOW: i32 = 10_000;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
    std::fs::write(path, text).expect("file");
}

/// Chapter `i`'s worked-example result: distinct for every chapter.
fn figure(i: usize) -> String {
    format!("{}.{:02}", 300 + i * 11, (i * 7) % 100)
}

fn prose(words: usize) -> String {
    vec!["prose"; words].join(" ")
}

fn draft(i: usize) -> String {
    format!(
        "# Chapter {i}\n\n{}\nWorked example: total = {} over the sample\n{}",
        prose(500),
        figure(i),
        prose(100)
    )
}

fn review(i: usize) -> String {
    format!(
        "APPROVE\n1. Recomputed total = {} ✓\n{}",
        figure(i),
        prose(40)
    )
}

/// The table a lead grounded in its workers' results writes.
fn grounded_table() -> String {
    let mut table = String::from("| Chapter | Rounds | Verdict | Worked-example result |\n");
    for i in 1..=CHAPTERS {
        table.push_str(&format!("| {i} | 1 | APPROVE | total = {} |\n", figure(i)));
    }
    table
}

/// Run `chatty-tui --headless --team guide` against `daemon`; the lead
/// (`LEAD_MODEL`) delegates to `data-analyst` and `reviewer`.
fn team_run(daemon: &FakeDaemon) {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");

    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    let models: Vec<ModelConfig> = [LEAD_MODEL, ANALYST_MODEL, REVIEWER_MODEL]
        .iter()
        .map(|m| {
            let mut model = ModelConfig::new(
                m.to_string(),
                m.to_string(),
                ProviderType::Ollama,
                m.to_string(),
            );
            if *m == LEAD_MODEL {
                model.max_context_window = Some(LEAD_WINDOW);
                model.max_tokens = Some(512);
            }
            model
        })
        .collect();
    let execution = ExecutionSettingsModel {
        enabled: false,
        git_enabled: false,
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        approval_mode: ApprovalMode::AutoApproveAll,
        ..ExecutionSettingsModel::default()
    };
    write(
        &config.join("providers.json"),
        &json!([provider]).to_string(),
    );
    write(&config.join("models.json"), &json!(models).to_string());
    write(
        &config.join("execution_settings.json"),
        &serde_json::to_string(&execution).unwrap(),
    );
    let agents = workspace.join(".chatty/agents");
    write(
        &agents.join("lead.toml"),
        &format!(
            "[agent]\nname = \"lead\"\nmodel = \"{LEAD_MODEL}\"\n[tools]\nprofile = \"coordinator\"\n\
             [swarm]\ndelegates_to = [\"data-analyst\", \"reviewer\"]\n"
        ),
    );
    for (name, model) in [
        ("data-analyst", ANALYST_MODEL),
        ("reviewer", REVIEWER_MODEL),
    ] {
        write(
            &agents.join(format!("{name}.toml")),
            &format!(
                "[agent]\nname = \"{name}\"\nmodel = \"{model}\"\n[tools]\nprofile = \"coder\"\n"
            ),
        );
    }
    write(
        &workspace.join(".chatty/teams/guide/team.json"),
        r#"{"leader":"lead","agents":["data-analyst","reviewer"]}"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args([
            "--headless",
            "-m",
            "Write the guide: draft and review each chapter, then reply with a table.",
            "--team",
            "guide",
        ])
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .env("CHATTY_SECRET_STORE", "file")
        .output()
        .expect("chatty-tui runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstderr:\n{stderr}"
    );
}

#[test]
fn a_shaped_leaders_final_request_carries_every_workers_figures() {
    let mut lead = Vec::new();
    for i in 1..=CHAPTERS {
        lead.push(Reply::tool_call(
            "invoke_agent",
            json!({ "agent": "data-analyst", "prompt": format!("Draft chapter {i}") }),
        ));
        lead.push(Reply::tool_call(
            "invoke_agent",
            json!({
                "agent": "reviewer",
                "prompt": format!("Review chapter {i}\n\n{}", draft(i)),
            }),
        ));
    }
    lead.push(Reply::text(grounded_table()));
    let daemon = FakeDaemon::scripted(
        Script::new()
            // A compaction's summary call runs on the lead's own model.
            .route_body(
                "You are compacting the working history",
                (0..2 * CHAPTERS).map(|_| Reply::text("Done so far: chapters delegated.")),
            )
            .route(LEAD_MODEL, lead)
            .route(ANALYST_MODEL, (1..=CHAPTERS).map(|i| Reply::text(draft(i))))
            .route(
                REVIEWER_MODEL,
                (1..=CHAPTERS).map(|i| Reply::text(review(i))),
            ),
    );
    team_run(&daemon);

    let lead_requests = daemon.requests_for(LEAD_MODEL);
    assert_eq!(
        lead_requests.len(),
        2 * CHAPTERS + 1,
        "every scripted lead turn ran"
    );
    let last: serde_json::Value =
        serde_json::from_slice(&lead_requests.last().unwrap().body).expect("a JSON request");
    let messages = last["messages"].as_array().expect("messages");
    // rig 0.46 joins adjacent user messages (AGE-1151; accepted, Marcel
    // 2026-11-12): the ledger, unchanged, follows the compaction summary in
    // the same user message, which this wire (Ollama's `/api/chat`) sends as
    // one string, its parts joined by a newline. The ledger runs from its
    // header to the end of that message.
    let ledger_text = |m: &serde_json::Value| -> Option<String> {
        let content = m["content"].as_str()?;
        let at = content.find(LEDGER_HEADER)?;
        (at == 0 || content[..at].ends_with('\n')).then(|| content[at..].to_owned())
    };
    let is_ledger = |m: &serde_json::Value| ledger_text(m).is_some();
    let ledger: Vec<&serde_json::Value> = messages.iter().filter(|m| is_ledger(m)).collect();
    assert_eq!(ledger.len(), 1, "the final request carries one ledger");
    let ledger = ledger_text(ledger[0]).unwrap();
    let ledger = ledger.as_str();
    // Everything the request carries but the ledger's own text, which may
    // share its message with other parts.
    let escaped = serde_json::to_string(ledger).unwrap();
    let rest = serde_json::to_string(messages)
        .unwrap()
        .replace(&escaped[1..escaped.len() - 1], "");

    // The headless lead compacted, and the older results really were taken
    // out of the request…
    assert!(rest.contains("[CONTEXT COMPACTED]"), "{rest}");
    assert!(
        !daemon
            .requests_for("You are compacting the working history")
            .is_empty(),
        "the summary call had its own route"
    );
    let not_verbatim: Vec<usize> = (1..=CHAPTERS)
        .filter(|i| !rest.contains(&figure(*i)))
        .collect();
    assert!(
        not_verbatim.len() >= CHAPTERS / 2,
        "most results were shaped out of the final request: {not_verbatim:?}"
    );
    // …and the ledger carries each of their figures in their place.
    for i in &not_verbatim {
        assert!(
            ledger.contains(&figure(*i)),
            "chapter {i}'s figure is in the ledger:\n{ledger}"
        );
    }

    // The answer written from it invents nothing.
    let results: Vec<String> = (1..=CHAPTERS).flat_map(|i| [draft(i), review(i)]).collect();
    let sources: Vec<&str> = results.iter().map(String::as_str).collect();
    assert_eq!(
        figures_not_in(&grounded_table(), &sources),
        Vec::<String>::new()
    );
}
