//! Fuzz `decode_frame` in both directions of the worker socket (EN-0b).
//!
//! Any input must decode or fail with a `FrameError`, never panic. A frame
//! that decodes must encode, and its encoding must decode back to the same
//! encoding. ADR-0021 step 1 (EN-1) moves this target to `FrameCodec`.

#![no_main]

use chatty_protocol_gateway::participant::{
    BrokerFrame, ParticipantFrame, decode_frame, encode_frame,
};
use libfuzzer_sys::fuzz_target;
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trip<F: Serialize + DeserializeOwned>(line: &str) {
    let Ok(frame) = decode_frame::<F>(line) else {
        return;
    };
    let encoded = encode_frame(&frame).expect("a decoded frame encodes");
    let again: F = decode_frame(&encoded).expect("an encoded frame decodes");
    assert_eq!(
        encode_frame(&again).expect("and encodes again"),
        encoded,
        "the encoding is stable"
    );
}

fuzz_target!(|data: &[u8]| {
    // The socket reads UTF-8 lines; anything else is refused before decode.
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };
    round_trip::<ParticipantFrame>(line);
    round_trip::<BrokerFrame>(line);
});
