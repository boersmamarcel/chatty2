//! Fuzz the `FrameCodec` in both directions of the worker socket (EN-0b,
//! moved to the codec by EN-1).
//!
//! The input is a sequence of lines, fed in order to a broker codec and a
//! worker codec, each of which first saw a hello and a `task.run` so the
//! id state is live. Any line must decode, be dropped, or fail with a
//! `FrameError` — never panic. A frame the broker decodes, the worker's
//! codec can encode (and the other way round), and that encoding decodes
//! again on the first side.

#![no_main]

use chatty_protocol_gateway::participant::{
    BrokerCodec, BrokerFrame, ParticipantCard, ParticipantFrame, WorkerCodec,
};
use libfuzzer_sys::fuzz_target;

/// A broker and a worker codec after a hello, its welcome and one task.
fn connected() -> (BrokerCodec, WorkerCodec) {
    let broker = BrokerCodec::new();
    let worker = WorkerCodec::new();
    let hello = worker
        .encode(&ParticipantFrame::Hello {
            card: ParticipantCard::default(),
        })
        .unwrap()
        .unwrap();
    broker.decode(&hello).unwrap();
    let welcome = r#"{"v":3,"id":1,"result":{"name":"fuzz-0","scope":"root","owner":null}}"#;
    worker.decode(welcome).unwrap();
    let task = BrokerFrame::Task {
        task_id: "task-1".into(),
        text: "fuzz".into(),
        bearer: None,
        capture_conversation: false,
        spawn_context: None,
        handoff: None,
        budget: Box::default(),
        swarm_events: false,
    };
    let run = broker.encode(&task).unwrap().unwrap();
    worker.decode(&run).unwrap();
    (broker, worker)
}

fuzz_target!(|data: &[u8]| {
    // The socket reads UTF-8 lines; anything else is refused before decode.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (broker, worker) = connected();
    for line in text.lines() {
        // Worker → broker: re-encode what the broker read on the worker's
        // side, and the broker reads it back.
        if let Ok(Some(frame)) = broker.decode(line) {
            let (echo_broker, echo_worker) = connected();
            if let Ok(Some(encoded)) = echo_worker.encode(&frame) {
                echo_broker
                    .decode(&encoded)
                    .expect("an encoded frame decodes");
            }
        }
        // Broker → worker, the same way round.
        if let Ok(Some(frame)) = worker.decode(line) {
            let (echo_broker, echo_worker) = connected();
            if let Ok(Some(encoded)) = echo_broker.encode(&frame) {
                echo_worker
                    .decode(&encoded)
                    .expect("an encoded frame decodes");
            }
        }
    }
});
