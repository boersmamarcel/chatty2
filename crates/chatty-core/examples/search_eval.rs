//! AGE-515/516/517: tool-level retrieval eval for `search_web`. Calls the real
//! `SearchWebTool` (error mapping included) on SimpleQA or FRAMES items and
//! writes one JSONL row per item plus a summary. No agent loop, no retries:
//! it measures what the tool itself returns.
//!
//! ```text
//! # run (records raw responses under --cache; reuses them on a re-run)
//! cargo run -p chatty-core --example search_eval -- run \
//!     --dataset simpleqa --data <dir>/simpleqa.jsonl --ids evals/search/simpleqa_dev_200.txt \
//!     --provider tavily --k 5 --concurrency 4 --cache runs/cache --out runs/2026-09-23-tavily.jsonl
//! # same run from the cache only (no network; a miss is an error row)
//! ... run ... --replay [--replay-latency]
//! # rescore an existing run, e.g. without the Wikipedia source
//! cargo run -p chatty-core --example search_eval -- rescore runs/x.jsonl --exclude-source wikipedia
//! # paired.csv for examples/paired_report.rs (McNemar)
//! cargo run -p chatty-core --example search_eval -- paired a.jsonl b.jsonl --metric hit5 --out paired.csv
//! ```
//!
//! Providers: `tavily` / `brave` (keys from `TAVILY_API_KEY` / `BRAVE_API_KEY`),
//! `fallback` (the keyless Bing → DuckDuckGo scrape) and `keyless` (the tool as
//! a user with no key gets it; identical to `fallback` until AGE-517 lands).

use chatty_core::settings::models::search_settings::SearchProvider;
use chatty_core::tools::SearchWebTool;
use chatty_core::tools::response_cache::{CacheMode, ResponseCache};
use chatty_core::tools::search_web_tool::SearchWebToolArgs;
use chatty_optimize::datasets::{load_frames, load_simpleqa};
use chatty_optimize::search_eval::{
    CallStatus, EvalResult, PairedMetric, SearchEvalRow, Summary, by_primary_stratum,
    classify_error, paired_csv, score_results,
};
use futures::StreamExt;
use rig_agent::tool::{Tool, ToolContext};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Item {
    id: String,
    query: String,
    answer: String,
    gold_links: Vec<String>,
    strata: Vec<String>,
}

#[derive(Default)]
struct RunArgs {
    dataset: String,
    data: PathBuf,
    ids: Option<PathBuf>,
    provider: String,
    mode: String,
    queries: Option<PathBuf>,
    k: usize,
    concurrency: usize,
    delay_ms: u64,
    out: PathBuf,
    cache: Option<PathBuf>,
    replay: bool,
    replay_latency: bool,
    replay_only: Vec<String>,
    reranker: Option<String>,
    reranker_model: Option<String>,
    exclude: Vec<String>,
    limit: Option<usize>,
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  search_eval run --dataset simpleqa|frames --data FILE [--ids FILE] \
         --provider tavily|brave|fallback|keyless [--mode single|fanout --queries FILE] \
         [--k N] [--concurrency N] [--delay-ms N] [--cache DIR] [--replay] [--replay-latency] [--replay-only SOURCE]... [--reranker URL [--reranker-model M]] \
         [--exclude-source S]... [--limit N] --out FILE\n  \
         search_eval rescore RUN.jsonl [--exclude-source S]...\n  \
         search_eval paired A.jsonl B.jsonl --metric hit1|hit5|all_sources5|all_sources10|ok \
         [--exclude-source S]... --out FILE"
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(parse_run(&args[1..])).await,
        Some("rescore") => rescore(&args[1..]),
        Some("paired") => paired(&args[1..]),
        _ => usage(),
    }
}

fn parse_run(args: &[String]) -> RunArgs {
    let mut r = RunArgs {
        mode: "single".into(),
        k: 5,
        concurrency: 1,
        ..Default::default()
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--dataset" => r.dataset = val(),
            "--data" => r.data = val().into(),
            "--ids" => r.ids = Some(val().into()),
            "--provider" => r.provider = val(),
            "--mode" => r.mode = val(),
            "--queries" => r.queries = Some(val().into()),
            "--k" => r.k = val().parse().unwrap_or_else(|_| usage()),
            "--concurrency" => r.concurrency = val().parse().unwrap_or_else(|_| usage()),
            "--delay-ms" => r.delay_ms = val().parse().unwrap_or_else(|_| usage()),
            "--out" => r.out = val().into(),
            "--cache" => r.cache = Some(val().into()),
            "--replay" => r.replay = true,
            "--replay-latency" => r.replay_latency = true,
            "--replay-only" => r.replay_only.push(val()),
            "--reranker" => r.reranker = Some(val()),
            "--reranker-model" => r.reranker_model = Some(val()),
            "--exclude-source" => r.exclude.push(val()),
            "--limit" => r.limit = Some(val().parse().unwrap_or_else(|_| usage())),
            _ => usage(),
        }
    }
    if r.dataset.is_empty() || r.provider.is_empty() || r.out.as_os_str().is_empty() {
        usage();
    }
    if r.replay && r.cache.is_none() {
        eprintln!("--replay needs --cache DIR");
        usage();
    }
    r
}

fn load_items(r: &RunArgs) -> Vec<Item> {
    let mut items: Vec<Item> = match r.dataset.as_str() {
        "simpleqa" => load_simpleqa(&r.data)
            .expect("load simpleqa")
            .into_iter()
            .map(|i| Item {
                id: i.id,
                query: i.question,
                answer: i.answer,
                gold_links: vec![],
                strata: vec![i.topic],
            })
            .collect(),
        "frames" => load_frames(&r.data)
            .expect("load frames")
            .into_iter()
            .map(|i| Item {
                id: i.id,
                query: i.prompt,
                answer: i.answer,
                gold_links: i.wiki_links,
                strata: i.reasoning_types,
            })
            .collect(),
        _ => usage(),
    };
    if let Some(ids) = &r.ids {
        let text = std::fs::read_to_string(ids).expect("read ids");
        let by_id: HashMap<String, Item> = items.into_iter().map(|i| (i.id.clone(), i)).collect();
        let mut by_id = by_id;
        items = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|id| {
                by_id
                    .remove(id)
                    .unwrap_or_else(|| panic!("id {id} not in dataset"))
            })
            .collect();
    }
    if let Some(n) = r.limit {
        items.truncate(n);
    }
    items
}

/// `{"id": "...", "queries": ["...", ...]}` per line.
fn load_queries(path: &Path) -> HashMap<String, Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        queries: Vec<String>,
    }
    std::fs::read_to_string(path)
        .expect("read queries")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let row: Row = serde_json::from_str(l).expect("queries row");
            (row.id, row.queries)
        })
        .collect()
}

fn build_tool(provider: &str, k: usize) -> SearchWebTool {
    let key = |var: &str| std::env::var(var).unwrap_or_else(|_| panic!("{var} is not set"));
    match provider {
        "tavily" => SearchWebTool::new(SearchProvider::Tavily, key("TAVILY_API_KEY"), k),
        "brave" => SearchWebTool::new(SearchProvider::Brave, key("BRAVE_API_KEY"), k),
        "fallback" | "keyless" => SearchWebTool::new_fallback(k),
        _ => usage(),
    }
}

async fn run(r: RunArgs) {
    let items = load_items(&r);
    let fanout = if r.mode == "fanout" {
        Some(load_queries(
            r.queries.as_deref().expect("--mode fanout needs --queries"),
        ))
    } else {
        None
    };
    let mut tool = build_tool(&r.provider, r.k);
    if let Some(url) = &r.reranker {
        let model = r
            .reranker_model
            .clone()
            .unwrap_or_else(|| "BAAI/bge-reranker-v2-m3".to_string());
        tool = tool.with_reranker(url, model);
    }
    if let Some(dir) = &r.cache {
        let mode = if r.replay {
            CacheMode::Replay
        } else {
            CacheMode::Record
        };
        tool = tool.with_response_cache(Arc::new(
            ResponseCache::new(dir, mode, r.replay_latency).with_replay_only(r.replay_only.clone()),
        ));
    }
    let tool = Arc::new(tool);
    eprintln!(
        "search_eval: {} items, dataset={} provider={} mode={} k={} replay={}",
        items.len(),
        r.dataset,
        r.provider,
        r.mode,
        r.k,
        r.replay
    );

    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let total = items.len();
    let rows: Vec<(usize, SearchEvalRow)> = futures::stream::iter(items.into_iter().enumerate())
        .map(|(idx, item)| {
            let tool = tool.clone();
            let queries = match &fanout {
                Some(q) => q
                    .get(&item.id)
                    .cloned()
                    .unwrap_or_else(|| panic!("no fan-out queries for id {}", item.id)),
                None => vec![item.query.clone()],
            };
            let (dataset, provider, mode, k, delay, replayed) = (
                r.dataset.clone(),
                r.provider.clone(),
                r.mode.clone(),
                r.k,
                r.delay_ms,
                r.replay,
            );
            let done = done.clone();
            async move {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let row = eval_item(&tool, item, queries, &dataset, &provider, &mode, k).await;
                let row = SearchEvalRow { replayed, ..row };
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n.is_multiple_of(20) || n == total {
                    eprintln!("  {n}/{total}");
                }
                (idx, row)
            }
        })
        .buffer_unordered(r.concurrency.max(1))
        .collect()
        .await;
    let mut rows = rows;
    rows.sort_by_key(|(i, _)| *i);
    let rows: Vec<SearchEvalRow> = rows.into_iter().map(|(_, row)| row).collect();

    if let Some(parent) = r.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let jsonl: String = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap() + "\n")
        .collect();
    std::fs::write(&r.out, jsonl).expect("write out");
    let report = render_report(&rows, &r.dataset, &r.exclude);
    std::fs::write(r.out.with_extension("summary.txt"), &report).ok();
    println!("{report}");
    eprintln!("wrote {}", r.out.display());
}

async fn eval_item(
    tool: &SearchWebTool,
    item: Item,
    queries: Vec<String>,
    dataset: &str,
    provider: &str,
    mode: &str,
    k: usize,
) -> SearchEvalRow {
    let started = Instant::now();
    let mut per_query: Vec<Vec<EvalResult>> = Vec::new();
    let mut per_query_candidates: Vec<Vec<EvalResult>> = Vec::new();
    let mut first_error: Option<String> = None;
    let mut bytes = 0;
    let mut n_errors = 0;
    for q in &queries {
        let args = SearchWebToolArgs {
            query: q.clone(),
            max_results: Some(k),
        };
        match tool.call(&mut ToolContext::new(), args).await {
            Ok(out) => {
                bytes += serde_json::to_string(&out).map_or(0, |s| s.len());
                let convert = |list: Vec<chatty_core::tools::search_web_tool::SearchResult>| {
                    list.into_iter()
                        .map(|r| EvalResult {
                            title: r.title,
                            url: r.url,
                            snippet: r.snippet,
                            source: r.source,
                        })
                        .collect::<Vec<_>>()
                };
                per_query.push(convert(out.results));
                per_query_candidates.push(convert(out.candidates));
            }
            Err(e) => {
                n_errors += 1;
                first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    let latency_ms = started.elapsed().as_millis() as u64;
    let results = merge_round_robin(per_query);
    let candidates = merge_round_robin(per_query_candidates);
    let status = if n_errors == queries.len() {
        CallStatus::Error
    } else if results.is_empty() {
        CallStatus::Empty
    } else {
        CallStatus::Ok
    };
    let error_class = first_error.as_deref().map(classify_error);
    let score = score_results(&item.answer, &item.gold_links, &results);
    SearchEvalRow {
        id: item.id,
        dataset: dataset.to_string(),
        provider: provider.to_string(),
        mode: mode.to_string(),
        n_calls: queries.len(),
        queries,
        status,
        error_class: if status == CallStatus::Error {
            error_class
        } else {
            None
        },
        error_text: first_error,
        gold_answer: item.answer,
        gold_links: item.gold_links,
        strata: item.strata,
        results,
        candidates,
        k,
        latency_ms,
        bytes,
        escalated: false,
        replayed: false,
        score: Some(score),
    }
}

/// Interleave per-query result lists (1st of each, then 2nd of each, …),
/// dropping repeated URLs. With one query this is the identity.
fn merge_round_robin(lists: Vec<Vec<EvalResult>>) -> Vec<EvalResult> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..longest {
        for list in &lists {
            if let Some(r) = list.get(i)
                && seen.insert(r.url.clone())
            {
                out.push(r.clone());
            }
        }
    }
    out
}

fn render_report(rows: &[SearchEvalRow], dataset: &str, exclude: &[String]) -> String {
    let frames = dataset == "frames";
    let mut o = String::new();
    o += &format!(
        "== all sources ==\n{}",
        Summary::from_rows(rows, &[]).render(frames)
    );
    if !exclude.is_empty() {
        o += &format!(
            "\n== excluding {:?} ==\n{}",
            exclude,
            Summary::from_rows(rows, exclude).render(frames)
        );
    }
    o += "\n== by primary stratum (n, err+empty, hit@5";
    o += if frames {
        ", recall@5, recall@10, all@5, all@10) ==\n"
    } else {
        ") ==\n"
    };
    for (stratum, group) in by_primary_stratum(rows) {
        let s = Summary::from_rows(group.iter().copied(), exclude);
        let pct = |k: usize| 100.0 * k as f64 / s.n.max(1) as f64;
        o += &format!(
            "  {:<24} {:>4} {:>6.1}% {:>6.1}%",
            stratum,
            s.n,
            pct(s.errors + s.empty),
            pct(s.hits[2])
        );
        if frames {
            o += &format!(
                " {:>6.1}% {:>6.1}% {:>6.1}% {:>6.1}%",
                100.0 * s.source_recall_5,
                100.0 * s.source_recall_10,
                pct(s.all_sources_5),
                pct(s.all_sources_10)
            );
        }
        o += "\n";
    }
    o
}

fn read_rows(path: &str) -> Vec<SearchEvalRow> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("row"))
        .collect()
}

fn take_excludes(args: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut rest, mut exclude) = (Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--exclude-source" {
            exclude.push(it.next().cloned().unwrap_or_else(|| usage()));
        } else {
            rest.push(a.clone());
        }
    }
    (rest, exclude)
}

fn rescore(args: &[String]) {
    let (rest, exclude) = take_excludes(args);
    let path = rest.first().unwrap_or_else(|| usage());
    let rows = read_rows(path);
    let dataset = rows
        .first()
        .map_or("simpleqa", |r| r.dataset.as_str())
        .to_string();
    let exclude_all: Vec<String> = exclude;
    println!("{}", render_report(&rows, &dataset, &exclude_all));
}

fn paired(args: &[String]) {
    let (rest, exclude) = take_excludes(args);
    let mut metric = PairedMetric::Hit5;
    let mut out: Option<String> = None;
    let mut files = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--metric" => {
                metric = it
                    .next()
                    .and_then(|m| PairedMetric::parse(m))
                    .unwrap_or_else(|| usage())
            }
            "--out" => out = it.next().cloned(),
            _ => files.push(a.clone()),
        }
    }
    let [a, b] = files.as_slice() else { usage() };
    let csv = paired_csv(&read_rows(a), &read_rows(b), metric, &exclude);
    match out {
        Some(path) => std::fs::write(&path, csv).expect("write paired csv"),
        None => print!("{csv}"),
    }
}
