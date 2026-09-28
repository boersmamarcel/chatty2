//! `swarm_tree_frame_time` (TB-4, AGE-666, invariant 5 of
//! `fabric-trace-and-bill`): a 20-node swarm tree, live in the real desktop
//! app, scrolled under Xvfb with the `desktop-screenshot` skill's harness.
//! The median frame time over 300 frames must be 18 ms or less (≥ 55 fps).
//!
//! There is no desktop job in CI, so this is PR evidence, run by hand and
//! `#[ignore]`d:
//!
//! ```text
//! cargo build -p chatty-gpui && cp $CARGO_TARGET_DIR/debug/chatty /tmp/chatty-bench
//! SWARM_TREE_BIN=/tmp/chatty-bench \
//!   cargo test -p chatty-gpui --test swarm_tree_frame_time -- --ignored --nocapture
//! ```
//!
//! The copy matters: see [`binary`].
//!
//! Nothing here stubs the desktop. The conversation is a hosted one
//! (AGE-298) whose server is this test: its turn is a scripted
//! `SessionEvent` stream — a delegation to `lead`, then the broker's tagged
//! batches (TB-1) for nineteen runs under it on three levels — which reaches
//! the tree through the same `StreamManager` path a local swarm's does. No
//! model is called and nothing leaves loopback.
//!
//! Needs `Xvfb`, lavapipe and a `python3` with `python-xlib` and `Pillow`
//! (see the skill). Screenshots and the app log land in
//! `$SWARM_TREE_DIR` (default `/tmp/swarm-tree-frame-time`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use chatty_core::models::message_types::ToolSource;
use chatty_core::models::token_usage::{ModelRef, TokenUsage};
use chatty_core::repositories::{
    ConversationData, ConversationRepository, ConversationSqliteRepository,
};
use chatty_core::session::SessionEvent;
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_fabric::{CallChain, SwarmEvent, SwarmItem};
use serde_json::json;

const CONVERSATION: &str = "swarm-tree-frame-time";
const FRAMES: usize = 300;
const BUDGET_MS: f64 = 18.0;

/// One run of the scripted swarm: its node name, the chain it runs under,
/// its model and what it does.
struct Run {
    node: String,
    chain: CallChain,
    model: &'static str,
    tools: Vec<(&'static str, Result<&'static str, &'static str>)>,
    tokens: (u32, u32),
    end: &'static str,
}

fn model(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderType::OpenRouter,
        model_id: id.to_string(),
    }
}

fn wire_lines(lines: &[(&str, u32, u32)]) -> serde_json::Value {
    let lines: Vec<_> = lines
        .iter()
        .map(|(m, i, o)| {
            json!({ "model": { "provider": "open_router", "model_id": m },
                    "inputTokens": i, "outputTokens": o })
        })
        .collect();
    json!({ "lines": lines })
}

/// `lead` → four sub-leaders → fifteen workers: twenty runs on three levels
/// under the root's delegation. The testers finish first, one of them
/// failed, so a live frame shows failed, done and running runs at once; the
/// writer has ten drafters, more than a node shows before it folds its
/// finished ones into a "+N more" line.
fn runs() -> (CallChain, Vec<(Run, usize)>) {
    let lead = CallChain::root("t-bench").extend("lead").unwrap();
    let mut runs = Vec::new();
    let teams: [(&str, &str, &str, usize, &str); 4] = [
        (
            "tester",
            "runner",
            "shell_execute",
            2,
            "mistralai/devstral-small",
        ),
        ("researcher", "searcher", "web_search", 2, "qwen/qwen3-32b"),
        (
            "writer",
            "drafter",
            "write_file",
            10,
            "mistralai/devstral-small",
        ),
        ("reviewer", "checker", "read_file", 1, "qwen/qwen3-32b"),
    ];
    for (team_ix, (leader, worker, tool, size, worker_model)) in teams.into_iter().enumerate() {
        let leader_chain = lead.extend(leader).unwrap();
        let worker_chain = leader_chain.extend(worker).unwrap();
        let leader_ix = runs.len();
        runs.push((
            Run {
                node: format!("{leader}-0"),
                chain: leader_chain,
                model: "anthropic/claude-sonnet-4.5",
                tools: vec![("invoke_agent", Ok("delegated"))],
                tokens: (6_000 + 900 * team_ix as u32, 700),
                end: "completed",
            },
            usize::MAX,
        ));
        for n in 0..size {
            let (outcome, end) = match (worker, n) {
                ("runner", 1) => (Err("exit status 101: 2 tests failed"), "failed"),
                _ => (Ok("ok"), "completed"),
            };
            runs.push((
                Run {
                    node: format!("{worker}-{n}"),
                    chain: worker_chain.clone(),
                    model: worker_model,
                    tools: vec![(tool, Ok("ok")), (tool, outcome)],
                    tokens: (2_400 + 310 * n as u32, 380 + 40 * n as u32),
                    end,
                },
                leader_ix,
            ));
        }
    }
    (lead, runs)
}

fn batch(run: &Run, inner: Vec<SwarmItem>) -> SessionEvent {
    SessionEvent::SwarmEvent(SwarmEvent {
        root_task_id: run.chain.root_task_id.clone(),
        node: run.node.clone(),
        chain: run.chain.clone(),
        inner,
    })
}

/// The hosted turn, in order: the root delegates to `lead`, every run
/// starts, calls its tools and reports (a sub-leader after its workers,
/// with their usage folded into its own, as AGE-415 has it), `lead`
/// finishes, and the root answers.
fn script() -> Vec<SessionEvent> {
    let (_, runs) = runs();
    let mut events = vec![
        SessionEvent::ToolCallStarted {
            id: "call-lead".into(),
            name: "invoke_agent".into(),
        },
        SessionEvent::ToolCallInput {
            id: "call-lead".into(),
            arguments: json!({ "agent": "lead", "prompt": "Prepare the 0.5 release" }).to_string(),
        },
        SessionEvent::Delegation(InvokeAgentProgress::Started {
            agent_name: "lead".into(),
            prompt: "Prepare the 0.5 release".into(),
            source: ToolSource::Local,
        }),
    ];
    for (run, _) in &runs {
        events.push(batch(
            run,
            vec![
                SwarmItem::TurnStarted,
                SwarmItem::ToolCallStarted {
                    id: format!("{}-t0", run.node),
                    name: run.tools[0].0.into(),
                },
            ],
        ));
    }
    let own = |run: &Run| (run.model, run.tokens.0, run.tokens.1);
    let mut leader_lines: Vec<Vec<(&str, u32, u32)>> = vec![Vec::new(); runs.len()];
    let finish = |run: &Run, lines: &[(&str, u32, u32)]| {
        let mut inner = Vec::new();
        for (ix, (tool, outcome)) in run.tools.iter().enumerate() {
            let id = format!("{}-t{ix}", run.node);
            if ix > 0 {
                inner.push(SwarmItem::ToolCallStarted {
                    id: id.clone(),
                    name: (*tool).into(),
                });
            }
            inner.push(match outcome {
                Ok(result) => SwarmItem::ToolCallResult {
                    id,
                    result: (*result).into(),
                },
                Err(error) => SwarmItem::ToolCallError {
                    id,
                    error: (*error).into(),
                },
            });
        }
        inner.push(SwarmItem::Text { bytes: 1_800 });
        inner.push(SwarmItem::Usage {
            usage: wire_lines(lines),
        });
        inner.push(SwarmItem::Ended {
            state: run.end.into(),
        });
        batch(run, inner)
    };
    for (ix, (run, leader)) in runs.iter().enumerate() {
        if *leader == usize::MAX {
            continue;
        }
        events.push(finish(run, &[own(run)]));
        leader_lines[*leader].push(own(run));
        let next_is_worker = runs.get(ix + 1).is_some_and(|(_, l)| *l == *leader);
        if !next_is_worker {
            let (leader_run, _) = &runs[*leader];
            let mut lines = vec![own(leader_run)];
            lines.extend(leader_lines[*leader].iter().copied());
            events.push(finish(leader_run, &lines));
        }
    }
    let mut usage: Vec<TokenUsage> = vec![TokenUsage {
        model: Some(model("anthropic/claude-opus-4.1")),
        ..TokenUsage::new(9_000, 1_200)
    }];
    for (run, _) in &runs {
        usage.push(TokenUsage {
            model: Some(model(run.model)),
            ..TokenUsage::new(run.tokens.0, run.tokens.1)
        });
    }
    events.push(SessionEvent::Delegation(InvokeAgentProgress::Finished {
        success: true,
        result: Some("Release notes drafted, reviewed and tested.".into()),
        usage,
    }));
    events.push(SessionEvent::ToolCallResult {
        id: "call-lead".into(),
        result: "Release notes drafted, reviewed and tested.".into(),
    });
    events.push(SessionEvent::Text(
        "The team finished: notes drafted and reviewed; one test run failed.".into(),
    ));
    events
}

/// Answer one HTTP request: the turn's POST gets the script as SSE, one
/// event per `pace`; anything else a 404.
fn serve_one(mut stream: TcpStream, events: &[SessionEvent], pace: Duration) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    if !(request_line.starts_with("POST ") && request_line.contains("/turns")) {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    );
    for event in events {
        let frame = format!(
            "event: session_event\ndata: {}\n\n",
            serde_json::to_string(event).expect("serialize event")
        );
        if stream.write_all(frame.as_bytes()).is_err() {
            return;
        }
        let _ = stream.flush();
        thread::sleep(pace);
    }
}

fn start_server(pace: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let events = script();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve_one(stream, &events, pace);
        }
    });
    port
}

/// The swarm's models, priced so the tree shows spend. The provider points
/// at a closed loopback port: nothing here may reach a real model.
const ROSTER: [(&str, f64, f64); 4] = [
    ("anthropic/claude-sonnet-4.5", 3.0, 15.0),
    ("anthropic/claude-opus-4.1", 15.0, 75.0),
    ("qwen/qwen3-32b", 0.1, 0.3),
    ("mistralai/devstral-small", 0.07, 0.28),
];

/// A hosted conversation pointing at `port`, with enough earlier history
/// that the transcript scrolls, and a model roster that prices the swarm.
fn seed(dir: &Path, port: u16) {
    let db = dir.join("config/chatty/conversations.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let providers = json!([{
        "name": "OpenRouter",
        "provider_type": "open_router",
        "api_key": "not-a-key",
        "base_url": "http://127.0.0.1:9",
    }]);
    let models: Vec<_> = ROSTER
        .iter()
        .map(|(id, input, output)| {
            json!({
                "id": id,
                "name": id,
                "provider_type": "open_router",
                "model_identifier": id,
                "cost_per_million_input_tokens": input,
                "cost_per_million_output_tokens": output,
            })
        })
        .collect();
    std::fs::write(
        dir.join("config/chatty/providers.json"),
        providers.to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("config/chatty/models.json"),
        serde_json::Value::from(models).to_string(),
    )
    .unwrap();
    let mut history = Vec::new();
    for n in 0..6 {
        history.push(rig_core::completion::Message::user(format!(
            "Question {n}: what changed in the release pipeline since the last tag?"
        )));
        history.push(rig_core::completion::Message::assistant(
            "The pipeline now signs artifacts, runs the conformance suite on every \
             merge group, and publishes the tarballs from one job. "
                .repeat(6),
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let data: ConversationData = serde_json::from_value(json!({
        "id": CONVERSATION,
        "title": "Swarm tree",
        "model_id": ROSTER[1].0,
        "message_history": serde_json::to_string(&history).unwrap(),
        "system_traces": serde_json::to_string(&vec![serde_json::Value::Null; history.len()]).unwrap(),
        "created_at": now,
        "updated_at": now,
        "mode": json!({
            "kind": "hosted",
            "server_url": format!("http://127.0.0.1:{port}"),
            "remote_id": "bench",
        }).to_string(),
    }))
    .expect("conversation row");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(ConversationSqliteRepository::deferred_with_path(db).save(CONVERSATION, data))
        .expect("seed the conversation");
}

fn skill_scripts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.claude/skills/desktop-screenshot/scripts")
}

fn x(args: &[&str]) {
    let status = Command::new("python3")
        .arg(skill_scripts().join("x.py"))
        .args(args)
        .env("DISPLAY", display())
        .status()
        .expect("x.py");
    assert!(status.success(), "x.py {args:?}");
}

/// A display of its own, so a screenshot session on the skill's default
/// `:99` is left alone. Never a real one (`:0`, `:1`).
fn display() -> String {
    std::env::var("SWARM_TREE_DISPLAY").unwrap_or_else(|_| ":97".into())
}

/// The app to drive. Not `target/debug/chatty` as this test leaves it:
/// building an integration test rebuilds the binary with the dev-only
/// `gpui/test-support` feature, which draws every frame synchronously
/// outside the path `ZED_MEASUREMENTS` times, so nothing gets measured.
fn binary() -> PathBuf {
    if let Ok(bin) = std::env::var("SWARM_TREE_BIN") {
        return PathBuf::from(bin);
    }
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    target.join("debug/chatty")
}

/// Median and 95th percentile of the last `FRAMES` frame durations the app
/// logged with `ZED_MEASUREMENTS=1`, in milliseconds.
fn frame_times(log: &str) -> Vec<f64> {
    log.lines()
        .filter(|line| line.contains("frame duration"))
        .filter_map(|line| {
            let tail = line.rsplit("frame duration:").next()?.trim();
            let (value, unit) = tail.split_at(tail.find(|c: char| c.is_alphabetic() || c == 'µ')?);
            let value: f64 = value.trim().parse().ok()?;
            Some(match unit.trim() {
                "ns" => value / 1e6,
                "µs" | "us" => value / 1e3,
                "ms" => value,
                "s" => value * 1e3,
                _ => return None,
            })
        })
        .collect()
}

/// Scroll the transcript up and down, a notch at a time, until the app
/// has logged `FRAMES` more frames; their sorted durations in ms.
fn scroll_frames(log_path: &Path) -> Vec<f64> {
    let logged = || frame_times(&std::fs::read_to_string(log_path).unwrap_or_default());
    let skip = logged().len();
    let mut round = 0;
    while logged().len() < skip + FRAMES && round < 4 * FRAMES {
        // Five notches each way keeps the tree on screen the whole time.
        let notches = if (round / 5) % 2 == 0 { "1" } else { "-1" };
        x(&["wheel", "800", "500", notches]);
        round += 1;
    }
    thread::sleep(Duration::from_millis(500));
    let mut times: Vec<f64> = logged().into_iter().skip(skip).take(FRAMES).collect();
    assert!(
        times.len() == FRAMES,
        "only {} frames logged while scrolling; was the app built by `cargo build` \
         (see `binary`)?",
        times.len()
    );
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times
}

fn summary(label: &str, times: &[f64]) -> String {
    format!(
        "{label}: {} frames, median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        times.len(),
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1],
    )
}

#[test]
#[ignore = "PR evidence: drives the real app under Xvfb; see the module docs"]
fn swarm_tree_frame_time() {
    let dir = PathBuf::from(
        std::env::var("SWARM_TREE_DIR").unwrap_or_else(|_| "/tmp/swarm-tree-frame-time".into()),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    let port = start_server(Duration::from_millis(120));
    seed(&dir, port);

    let started = Command::new(skill_scripts().join("start.sh"))
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--bin",
            binary().to_str().unwrap(),
        ])
        .args(["--display", &display()])
        .env("ZED_MEASUREMENTS", "1")
        .status()
        .expect("start.sh");
    assert!(started.success(), "start.sh failed");
    let stop = || {
        let _ = Command::new(skill_scripts().join("stop.sh"))
            .args(["--display", &display()])
            .status();
    };
    let shot = |name: &str| x(&["shot", dir.join("shots").join(name).to_str().unwrap()]);
    if let Ok(minutes) = std::env::var("SWARM_TREE_HOLD_MINUTES") {
        // Leave the app and the fake server up to drive by hand.
        println!("holding: DISPLAY={}, scratch {}", display(), dir.display());
        thread::sleep(Duration::from_secs(
            60 * minutes.parse::<u64>().unwrap_or(10),
        ));
        stop();
        return;
    }
    let log = dir.join("app.log");

    // The seeded conversation is the only one: open it from the sidebar and
    // time the same scroll over its plain history first, as the baseline.
    x(&["click", "80", "100"]);
    thread::sleep(Duration::from_secs(2));
    let baseline = scroll_frames(&log);

    // Then send the turn the fake server answers, and time the tree.
    x(&["click", "900", "876"]);
    x(&["type", "Prepare the 0.5 release with the team"]);
    x(&["key", "Return"]);
    thread::sleep(Duration::from_millis(3500));
    shot("01-live.png");
    thread::sleep(Duration::from_secs(9));
    shot("02-settled.png");
    let tree = scroll_frames(&log);
    shot("03-scrolled.png");
    stop();

    println!("{}", summary("baseline, history only", &baseline));
    println!("{}", summary("swarm_tree_frame_time, 20-node tree", &tree));
    println!(
        "budget {BUDGET_MS} ms; shots in {}",
        dir.join("shots").display()
    );
    let median = tree[FRAMES / 2];
    assert!(
        median <= BUDGET_MS,
        "median frame {median:.2} ms > {BUDGET_MS} ms"
    );
}
