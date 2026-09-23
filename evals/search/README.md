# search_web retrieval eval (AGE-515 / AGE-516 / AGE-517)

Tool-level eval: calls the real `SearchWebTool` on SimpleQA (single-fact) and
FRAMES (multi-hop) items and scores what it returns. No agent loop.

## Committed files (never edit to change a number)

| File | What |
|---|---|
| `simpleqa_ids_300.txt` | Stratified-by-topic sample of SimpleQA (seed 515) |
| `simpleqa_dev_200.txt` / `simpleqa_holdout_100.txt` | DEV / HOLDOUT split of that sample |
| `frames_ids_150.txt` | Stratified-by-primary-reasoning-type sample of FRAMES (seed 516) |
| `frames_dev_100.txt` / `frames_holdout_50.txt` | DEV / HOLDOUT split |
| `frames_queries.jsonl` | Fan-out sub-queries, generated once by `gen_frames_queries.py` |
| `freshqa_ids_100.txt` | FreshQA check set: 100 valid-premise TEST questions from the 2026-04-21 release, stratified by fact type (seed 519). A one-off freshness check, not for tuning |

Iterate on DEV only. Run HOLDOUT at milestones.

## Setup

```bash
D=/path/to/data
curl -L -o $D/simple_qa_test_set.csv https://openaipublic.blob.core.windows.net/simple-evals/simple_qa_test_set.csv
curl -L -o $D/frames_test.tsv https://huggingface.co/datasets/google/frames-benchmark/resolve/main/test.tsv
# optional: FreshQA — export the latest release's Google Sheet as CSV to $D/freshqa.csv
cargo run -p chatty-optimize --example search_eval_prepare -- $D evals/search   # also rewrites the ID lists; they must not change
```

## Run

```bash
cargo run -p chatty-core --example search_eval -- run --dataset simpleqa \
  --data $D/simpleqa.jsonl --ids evals/search/simpleqa_dev_200.txt \
  --provider tavily|brave|fallback|keyless --k 5 --cache $D/cache --out $D/runs/<name>.jsonl
# FRAMES: --dataset frames --data $D/frames.jsonl --k 10 [--mode fanout --queries evals/search/frames_queries.jsonl]
# Replay from the recorded responses (no credits, no scraping):  add --replay [--replay-latency]
# Keyless with and without Wikipedia:  add --exclude-source wikipedia  (or: search_eval rescore RUN.jsonl --exclude-source wikipedia)
# Keyless cross-encoder rerank:  add --reranker http://HOST:PORT/rerank [--reranker-model M]
# Never let a paid API go live (misses become errors):  add --replay-only tavily
# FreshQA:  --dataset freshqa --data $D/freshqa.jsonl --ids evals/search/freshqa_ids_100.txt
# McNemar:  search_eval paired A.jsonl B.jsonl --metric hit5 --out paired.csv
#           cargo run -p chatty-optimize --example paired_report -- paired.csv
```

Keyless live runs: `--concurrency 1 --delay-ms 1500` or more; with the reranker each
search makes up to 11 Wikipedia calls, so keep under Wikimedia's 200 req/min (`--delay-ms
6000`). Tavily *dev* keys get `429`-blocked above concurrency 1.

Scoring lives in `crates/chatty-optimize/src/search_eval.rs` and is frozen
once baselines exist: answer normalization, the leak blocklist, and the hit
rules. A leaked result (a domain that republishes the dataset) keeps its rank
but never counts as a hit.
