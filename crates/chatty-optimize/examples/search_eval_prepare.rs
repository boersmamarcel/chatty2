//! AGE-515/516: convert the upstream SimpleQA CSV and FRAMES TSV to JSONL
//! and draw the committed ID lists (stratified sample, then a stratified
//! DEV/HOLDOUT split of that sample).
//!
//! ```text
//! curl -LO https://openaipublic.blob.core.windows.net/simple-evals/simple_qa_test_set.csv
//! curl -L -o frames_test.tsv https://huggingface.co/datasets/google/frames-benchmark/resolve/main/test.tsv
//! cargo run -p chatty-optimize --example search_eval_prepare -- <data_dir> evals/search
//! ```
//!
//! Writes `<data_dir>/simpleqa.jsonl` and `<data_dir>/frames.jsonl` (not
//! committed) and the ID lists under `evals/search/` (committed). Re-running
//! reproduces the same lists; the seeds below must never change.

use chatty_optimize::datasets::{
    FramesItem, SimpleQaItem, parse_py_str_list, parse_reasoning_types, parse_simpleqa_topic,
    stratified_sample,
};
use std::fs;
use std::io::Write;
use std::path::Path;

const SIMPLEQA_SAMPLE: usize = 300;
const SIMPLEQA_HOLDOUT: usize = 100;
const SIMPLEQA_SEED: u64 = 515;
const FRAMES_SAMPLE: usize = 150;
const FRAMES_HOLDOUT: usize = 50;
const FRAMES_SEED: u64 = 516;
/// Seed offset for the DEV/HOLDOUT split drawn from each sample.
const HOLDOUT_SEED_OFFSET: u64 = 1_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let data = args
        .next()
        .expect("usage: search_eval_prepare <data_dir> <ids_dir>");
    let ids = args
        .next()
        .expect("usage: search_eval_prepare <data_dir> <ids_dir>");
    let (data, ids) = (Path::new(&data), Path::new(&ids));
    fs::create_dir_all(ids).unwrap();

    let simpleqa = read_simpleqa(&data.join("simple_qa_test_set.csv"));
    write_jsonl(&data.join("simpleqa.jsonl"), &simpleqa);
    let strata: Vec<&str> = simpleqa.iter().map(|i| i.topic.as_str()).collect();
    write_split(
        ids,
        "simpleqa",
        &simpleqa,
        &strata,
        SIMPLEQA_SAMPLE,
        SIMPLEQA_HOLDOUT,
        SIMPLEQA_SEED,
    );

    let frames = read_frames(&data.join("frames_test.tsv"));
    write_jsonl(&data.join("frames.jsonl"), &frames);
    let strata: Vec<&str> = frames.iter().map(|i| i.primary_reasoning_type()).collect();
    write_split(
        ids,
        "frames",
        &frames,
        &strata,
        FRAMES_SAMPLE,
        FRAMES_HOLDOUT,
        FRAMES_SEED,
    );
}

fn read_simpleqa(path: &Path) -> Vec<SimpleQaItem> {
    let mut rdr =
        csv::Reader::from_path(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let headers = rdr.headers().unwrap().clone();
    assert_eq!(
        headers.iter().collect::<Vec<_>>(),
        ["metadata", "problem", "answer"],
        "unexpected SimpleQA columns"
    );
    rdr.records()
        .enumerate()
        .map(|(i, rec)| {
            let rec = rec.unwrap();
            SimpleQaItem {
                id: i.to_string(),
                question: rec[1].trim().to_string(),
                answer: rec[2].trim().to_string(),
                topic: parse_simpleqa_topic(&rec[0]).unwrap_or_else(|| panic!("row {i}: no topic")),
            }
        })
        .collect()
}

fn read_frames(path: &Path) -> Vec<FramesItem> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_path(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let headers = rdr.headers().unwrap().clone();
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("FRAMES: no column {name:?}"))
    };
    let (id, prompt, answer, links, types) = (
        col(""),
        col("Prompt"),
        col("Answer"),
        col("wiki_links"),
        col("reasoning_types"),
    );
    rdr.records()
        .map(|rec| {
            let rec = rec.unwrap();
            FramesItem {
                id: rec[id].to_string(),
                prompt: rec[prompt].trim().to_string(),
                answer: rec[answer].trim().to_string(),
                wiki_links: parse_py_str_list(&rec[links]),
                reasoning_types: parse_reasoning_types(&rec[types]),
            }
        })
        .collect()
}

fn write_jsonl<T: serde::Serialize>(path: &Path, items: &[T]) {
    let mut f = fs::File::create(path).unwrap();
    for item in items {
        writeln!(f, "{}", serde_json::to_string(item).unwrap()).unwrap();
    }
    println!("wrote {} rows to {}", items.len(), path.display());
}

fn write_split<T: chatty_optimize::DatasetItem>(
    dir: &Path,
    name: &str,
    items: &[T],
    strata: &[&str],
    n: usize,
    n_holdout: usize,
    seed: u64,
) {
    let sample = stratified_sample(strata, n, seed);
    let sample_strata: Vec<&str> = sample.iter().map(|&i| strata[i]).collect();
    let holdout_pos = stratified_sample(&sample_strata, n_holdout, seed + HOLDOUT_SEED_OFFSET);
    let ids = |pos: &mut dyn Iterator<Item = usize>| -> String {
        pos.map(|p| format!("{}\n", items[sample[p]].id()))
            .collect()
    };
    let all = ids(&mut (0..sample.len()));
    let holdout = ids(&mut holdout_pos.iter().copied());
    let dev = ids(&mut (0..sample.len()).filter(|p| !holdout_pos.contains(p)));
    let n_dev = n - n_holdout;
    fs::write(dir.join(format!("{name}_ids_{n}.txt")), all).unwrap();
    fs::write(dir.join(format!("{name}_dev_{n_dev}.txt")), dev).unwrap();
    fs::write(dir.join(format!("{name}_holdout_{n_holdout}.txt")), holdout).unwrap();
    println!("{name}: sample {n} → dev {n_dev} / holdout {n_holdout}");
}
