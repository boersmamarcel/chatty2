//! ADR-0021 kill criterion 3: what the v3 envelope costs over v2 on the
//! worker socket (EN-1, AGE-769).
//!
//! One task's traffic: the broker encodes 1 `task` and the worker 1,000
//! `status`/`artifact` lines (`encode`); the worker decodes the task and the
//! broker the 1,000 lines (`decode`). The v2 run of this same scenario used
//! the free `encode_frame`/`decode_frame` on a branch from `1ea4bb0b`; this
//! file drives the per-connection `FrameCodec`s, including making them, as
//! a connection does. Both medians are in
//! `docs/research/fabric-codec-baseline.md`; a v3/v2 ratio above 2 fails
//! the step.
//!
//! Run: `CARGO_TARGET_DIR=<dir> cargo bench -p chatty-protocol-gateway --bench frame_codec`

use chatty_protocol_gateway::participant::{
    BrokerCodec, BrokerFrame, ParticipantFrame, TaskState, WorkerCodec,
};
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

const LINES: usize = 1_000;

fn task() -> BrokerFrame {
    BrokerFrame::Task {
        task_id: "task-0b6f1c2e-5d7a-4c1e-9a53-2f4e8b1d6c90".into(),
        text: "Summarise src/lib.rs and list its public functions.".into(),
        identity: None,
        capture_conversation: false,
        spawn_context: None,
        handoff: None,
        budget: Box::default(),
        swarm_events: false,
    }
}

/// Half `working` statuses naming a tool, half answer chunks.
fn updates() -> Vec<ParticipantFrame> {
    let task_id = "task-0b6f1c2e-5d7a-4c1e-9a53-2f4e8b1d6c90";
    (0..LINES)
        .map(|i| {
            if i % 2 == 0 {
                ParticipantFrame::Status {
                    task_id: task_id.into(),
                    state: TaskState::Working,
                    message: Some(format!("read_file src/module_{i}.rs")),
                    metadata: None,
                }
            } else {
                ParticipantFrame::Artifact {
                    task_id: task_id.into(),
                    text: format!("Chunk {i} of the answer: the module defines a parser."),
                    last_chunk: false,
                }
            }
        })
        .collect()
}

fn frame_codec(c: &mut Criterion) {
    let task = task();
    let updates = updates();
    let mut group = c.benchmark_group("frame_codec");

    group.bench_function("encode", |b| {
        b.iter(|| {
            let broker = BrokerCodec::new();
            let worker = WorkerCodec::new();
            let run = broker.encode(&task).unwrap().unwrap();
            // The worker learns the task.run's id by reading it.
            worker.decode(&run).unwrap();
            black_box(&run);
            for frame in &updates {
                black_box(worker.encode(frame).unwrap());
            }
        })
    });

    // The lines as they cross the socket, encoded once.
    let (run, lines) = {
        let broker = BrokerCodec::new();
        let worker = WorkerCodec::new();
        let run = broker.encode(&task).unwrap().unwrap();
        worker.decode(&run).unwrap();
        let lines: Vec<String> = updates
            .iter()
            .map(|frame| worker.encode(frame).unwrap().unwrap())
            .collect();
        (run, lines)
    };
    group.bench_function("decode", |b| {
        b.iter_batched(
            || {
                // The broker that sent the task.run, so its id is in flight.
                let broker = BrokerCodec::new();
                broker.encode(&task).unwrap();
                broker
            },
            |broker| {
                let worker = WorkerCodec::new();
                black_box(worker.decode(&run).unwrap());
                for line in &lines {
                    black_box(broker.decode(line).unwrap());
                }
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(benches, frame_codec);
criterion_main!(benches);
