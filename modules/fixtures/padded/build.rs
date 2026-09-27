//! Generates a ~5 MiB non-uniform, non-zero byte blob at build time and
//! writes it to `$OUT_DIR/pad.bin`, included via `include_bytes!` in
//! `src/lib.rs`. Done in a build script (plain runtime code) rather than
//! `const fn` in the crate itself, since a 5,000,000-iteration const-eval
//! loop trips rustc's `long_running_const_eval` lint.
use std::env;
use std::fs;
use std::path::Path;

const PAD_LEN: usize = 5 * 1024 * 1024;

fn main() {
    let mut buf = vec![0u8; PAD_LEN];
    let mut state: u32 = 0x2545_F491;
    for b in buf.iter_mut() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (state >> 24) as u8;
    }
    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR set by cargo");
    fs::write(Path::new(&out_dir).join("pad.bin"), &buf).expect("write pad.bin");
}
