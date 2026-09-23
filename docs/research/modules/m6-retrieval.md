# M6 — Retrieval: one pipeline, two corpora

**Papers:** no single paper. This module collects the retrieval literature behind two
systems Chatty already ships: open-web search (`search_web`, `fetch`) and the agent's
own memory (`search_memory`, `doc_retriever`). Every citation below was checked against
the paper itself (arXiv, ACL Anthology, OpenReview or the publisher). The
[Literature by stage](#literature-by-stage) section lists them.

**Linear:** [AGE-515](https://linear.app/agents-research/issue/AGE-515) (SimpleQA eval),
[AGE-516](https://linear.app/agents-research/issue/AGE-516) (FRAMES eval),
[AGE-517](https://linear.app/agents-research/issue/AGE-517) (keyless tier) · **Crates:**
`chatty-core` (tools), `chatty-optimize` (eval scoring) · **Promotion:** keyless tier
changes shipped as the default, cross-encoder rerank opt-in
([AGE-521](https://linear.app/agents-research/issue/AGE-521))

M5 is DGM, which lives outside this repo, so this module is M6.

## The pipeline

Both systems do the same five things; only the corpus differs.

```mermaid
flowchart LR
  Q[Query formulation] --> C[Candidate retrieval]
  C --> F[Fusion]
  F --> R[Reranking]
  R --> P[Context packing]
```

State after this module (keyless = no search API key):

| Stage | Question it answers | Part A: open web | Part B: memory |
|---|---|---|---|
| Query formulation | What string do we search for? | The model writes a `search_web` query; the Wikipedia backend reduces it to content words joined with `OR` | The model writes a `search_memory` query |
| Candidate retrieval | Which items could be relevant? | Keyed: Tavily / Brave API. Keyless: Wikipedia search API **in parallel with** a Bing → DuckDuckGo scrape | memvid BM25 (always), memvid vector search (opt-in) |
| Fusion | How do several ranked lists become one? | Keyless: Wikipedia and web lists interleaved (RRF tried, reverted) | Raw max-score merge of the BM25 and vector lists |
| Reranking | Which few candidates are best? | Keyless: BM25 over passages of the fetched pages; with a reranker configured, a cross-encoder orders a 10-candidate pool | None |
| Context packing | What goes into the prompt, in what order? | Up to `max_results` results; keyless snippets lead with the page's best passage (≤ 1,000 chars total) | Up to `top_k` (default 5) hits; facts first, then skills |

The rest of this page explains why each stage exists (the papers), what Chatty does at
each stage today (the code), and what we measured.

## Literature by stage

Each entry gives the claim, the mechanism, and where it applies: **A** (open web),
**B** (memory), or both. "Preprint" means no peer-reviewed venue could be found.

### 1. Query formulation

A search engine can only return what the query asks for. For multi-hop questions, a
single query written from the whole question is usually the wrong query. It names the
final entity only indirectly ("the 15th first lady's mother"), so no page matches it.

- **Self-Ask**: Press, Zhang, Min, Schmidt, Smith, Lewis. *Measuring and Narrowing the
  Compositionality Gap in Language Models.* Findings of EMNLP 2023.
  [arXiv:2210.03350](https://arxiv.org/abs/2210.03350)
  - **Claim:** as models grow, single-hop accuracy improves faster than multi-hop
    composition, and explicit decomposition narrows that gap.
  - **Mechanism:** the model asks itself follow-up questions and answers each before the
    final answer. Because the follow-ups have a fixed shape, each can be sent to a search
    engine instead of being answered from the model's weights.
  - **Applies to:** A. FRAMES-style questions need sub-queries, and the fan-out mode of
    the eval simulates exactly this.
- **IRCoT**: Trivedi, Balasubramanian, Khot, Sabharwal. *Interleaving Retrieval with
  Chain-of-Thought Reasoning for Knowledge-Intensive Multi-Step Questions.* ACL 2023.
  [arXiv:2212.10509](https://arxiv.org/abs/2212.10509)
  - **Claim:** one retrieve-then-read step is not enough for multi-step questions.
    Retrieving after every reasoning step improves both retrieval and answers: up to +21
    points retrieval and +15 QA with GPT-3, with similar gains for Flan-T5.
  - **Mechanism:** each new chain-of-thought sentence becomes the next retrieval query,
    so "what to retrieve depends on what has already been derived."
  - **Applies to:** A, through the agent loop. The model already calls `search_web`
    repeatedly; IRCoT is the argument for letting it do so, not for one big query.
- **Query rewriting (Rewrite-Retrieve-Read)**: Ma, Gong, He, Zhao, Duan. EMNLP 2023.
  [arXiv:2305.14283](https://arxiv.org/abs/2305.14283). The ACL Anthology title reads
  "Query Rewriting *in*…", the arXiv title "…*for*…".
  - **Claim:** rewriting the input into a search query before retrieval beats plain
    retrieve-then-read.
  - **Mechanism:** an LLM rewrites, a web search engine retrieves, and a frozen LLM reads.
    A small rewriter can also be trained with RL from the reader's feedback.
  - **Applies to:** A. It is the published version of what the agent does when it turns
    a chat message into a `search_web` string.
- **query2doc**: Wang, Yang, Wei. *Query2doc: Query Expansion with Large Language
  Models.* EMNLP 2023. [arXiv:2303.07678](https://arxiv.org/abs/2303.07678)
  - **Claim:** appending an LLM-written pseudo-document to the query improves BM25 by
    3–15% on MS MARCO and TREC DL, and also helps dense retrievers.
  - **Mechanism:** the pseudo-document adds the vocabulary a relevant document would use
    (expansion terms), which is the thing lexical matching lacks.
  - **Applies to:** both, especially the BM25 stages (memvid lex, `doc_retriever`, and a
    future web passage ranker).
- **HyDE**: Gao, Ma, Lin, Callan. *Precise Zero-Shot Dense Retrieval without Relevance
  Labels.* ACL 2023. [arXiv:2212.10496](https://arxiv.org/abs/2212.10496)
  - **Claim:** embedding an LLM-written hypothetical answer, rather than the question,
    gives strong zero-shot dense retrieval with no relevance labels.
  - **Mechanism:** the LLM writes N hypothetical documents, an unsupervised encoder
    (Contriever) embeds and averages them, and the nearest real documents are returned.
    The encoder's bottleneck is meant to wash out the invented details.
  - **Applies to:** B (memvid vector search). It has no effect on keyword web engines.
- **RAG-Fusion**: Rackauckas. IJNLC 13(1), 2024.
  [arXiv:2402.03367](https://arxiv.org/abs/2402.03367)
  - **Claim:** generating several queries and merging their result lists with RRF gives
    more complete answers.
  - **Mechanism:** multi-query generation, then RRF (below).
  - **Applies to:** both.
  - **Weak evidence:** a single-author industry case study with manual evaluation. Cite it
    for the practice, not as proof.
- **FLARE**: Jiang, Xu, Gao, et al. *Active Retrieval Augmented Generation.* EMNLP 2023.
  [arXiv:2305.06983](https://arxiv.org/abs/2305.06983)
  - **Claim:** deciding *when* to retrieve during generation beats retrieving once up
    front.
  - **Mechanism:** draft the next sentence; if it has low-confidence tokens, use the
    draft as the query and regenerate.
  - **Applies to:** A, as an argument for conditional search. It needs token
    probabilities, which the agent loop does not expose.
- **Search-R1**: Jin, Zeng, Yue, et al. COLM 2025.
  [arXiv:2503.09516](https://arxiv.org/abs/2503.09516)
  - **Claim:** RL with an outcome-only reward teaches a model when and what to search
    across several turns: +41% (Qwen2.5-7B) and +20% (3B) over RAG baselines.
  - **Mechanism:** the model emits `<search>…</search>`, results come back inside
    `<information>`, and those retrieved tokens are masked out of the loss.
  - **Applies to:** A. Evidence that query policy is learnable, including by small
    models. Chatty prompts instead of training.

### 2. Candidate retrieval

This stage trades **recall** against cost. It has to be cheap enough to run over the
whole corpus, so it uses either an inverted index (lexical) or a vector index (dense).

- **BM25**: Robertson, Zaragoza. *The Probabilistic Relevance Framework: BM25 and
  Beyond.* Foundations and Trends in IR 3(4):333–389, 2009.
  [doi:10.1561/1500000019](https://doi.org/10.1561/1500000019). Crossref now lists the
  same DOI as vol. 4(1–2); the author PDF and ACM DL say 3(4).
  - **Claim:** BM25 follows from the probabilistic relevance model. A term's weight is an
    idf-like weight times a *saturating*, *length-normalised* term frequency.
  - **Mechanism:** `tf / (k1·((1−b) + b·dl/avdl) + tf) · idf`, summed over query terms.
    - `k1` sets how fast repeated occurrences stop adding score. The tenth "Paris" in a
      page is worth far less than the first.
    - `b` sets how much long documents are penalised for simply containing more words.
    - The authors suggest 0.5 < b < 0.8 and 1.2 < k1 < 2.
  - **Applies to:** both. memvid's lexical search (Tantivy BM25) and `doc_retriever`
    (k1 = 1.2, b = 0.75) are BM25 today. A web passage ranker would be too, but there
    `avdl` and idf come from a handful of fetched pages, so the statistics are noisy.
- **DPR**: Karpukhin, Oğuz, Min, et al. *Dense Passage Retrieval for Open-Domain
  Question Answering.* EMNLP 2020. [arXiv:2004.04906](https://arxiv.org/abs/2004.04906)
  - **Claim:** a dual encoder trained on a modest set of question–passage pairs beats
    BM25 by 9–19 points top-20 accuracy on open-domain QA.
  - **Mechanism:**
    - Two BERT encoders; relevance is the dot product.
    - Training uses the other questions' gold passages as in-batch negatives, plus one
      BM25 hard negative.
    - The paper also tests a **linear hybrid** `BM25 + 1.1·sim`, a score-level fusion to
      contrast with RRF.
  - **Applies to:** B. memvid vector search is this paradigm. There is no web-scale
    vector index in Part A.
- **Contriever**: Izacard, Caron, Hosseini, et al. *Unsupervised Dense Information
  Retrieval with Contrastive Learning.* TMLR 2022.
  [arXiv:2112.09118](https://arxiv.org/abs/2112.09118)
  - **Claim:** contrastive training without labels gives a dense retriever that beats
    BM25 on 11 of 15 BEIR datasets for Recall@100.
  - **Mechanism:** two spans from the same document are positives; everything else is a
    negative.
  - **Applies to:** B. Personal memory has no relevance labels, so this is the relevant
    regime.
- **E5**: Wang, Yang, Huang, et al. *Text Embeddings by Weakly-Supervised Contrastive
  Pre-training.* Preprint. [arXiv:2212.03533](https://arxiv.org/abs/2212.03533)
  - **Claim:** contrastive pre-training on web-mined pairs gives embeddings that beat
    BM25 zero-shot on BEIR.
  - **Mechanism:** one vector per text.
  - **Applies to:** B. It is the class of embedder memvid would use.
- **SPLADE**: Formal, Piwowarski, Clinchant. SIGIR 2021.
  [arXiv:2107.05720](https://arxiv.org/abs/2107.05720)
  - **Claim:** a learned *sparse* retriever can match dense retrievers while still
    running on an inverted index.
  - **Mechanism:** a BERT MLM head predicts weights for every vocabulary term, including
    terms the text does not contain (expansion). A sparsity penalty keeps the vectors
    small.
  - **Applies to:** B, as an upgrade path that keeps memvid's lexical index.
  - **SPLADE v2** (preprint, [arXiv:2109.10086](https://arxiv.org/abs/2109.10086)) can
    expand documents only, which moves all neural cost to ingest time.
- **ColBERT**: Khattab, Zaharia. SIGIR 2020.
  [arXiv:2004.12832](https://arxiv.org/abs/2004.12832). **ColBERTv2**: Santhanam et al.
  NAACL 2022. [arXiv:2112.01488](https://arxiv.org/abs/2112.01488)
  - **Claim:** late interaction keeps most of a cross-encoder's quality at a fraction of
    the cost. v2 cuts the index size 6–10×.
  - **Mechanism:** one embedding per token. The score sums, over query tokens, the best
    match among the document's tokens (MaxSim), and document vectors are precomputed.
  - **Applies to:** B only. In Part A the candidates are freshly fetched pages, so
    nothing can be precomputed.
- **M3-Embedding (BGE-M3)**: Chen, Xiao, Zhang, et al. Findings of ACL 2024.
  [arXiv:2402.03216](https://arxiv.org/abs/2402.03216)
  - **Claim:** one model produces dense, sparse and multi-vector scores, in 100+
    languages and on inputs up to 8,192 tokens.
  - **Mechanism:** three heads, trained with self-distillation in which the three modes'
    combined score is the teacher.
  - **Applies to:** B, if a single multilingual (Dutch and English) model is wanted.
- **BEIR**: Thakur, Reimers, Rücklé, Srivastava, Gurevych. NeurIPS 2021 Datasets &
  Benchmarks. [arXiv:2104.08663](https://arxiv.org/abs/2104.08663)
  - **Claim:** across 18 zero-shot datasets, BM25 is a robust baseline. Rerankers and
    late interaction are best but cost the most, and trained dense retrievers often
    generalise worse out of domain.
  - **Applies to:** both. It is the main evidence for keeping BM25 as the backbone.
    Personal memory is out of domain for every trained retriever.

### 3. Fusion

Several retrievers or queries each return a ranked list, and fusion turns them into one.
The core difficulty is that scores from different systems are **not on the same scale**.
A BM25 score is unbounded and depends on the query length and the collection. A cosine
similarity lives in [−1, 1]. memvid's vector score is `1/(1+L2)`, which is in (0, 1].

- **Reciprocal Rank Fusion (RRF)**: Cormack, Clarke, Büttcher. *Reciprocal Rank Fusion
  outperforms Condorcet and individual Rank Learning Methods.* SIGIR 2009, pp. 758–759.
  [doi:10.1145/1571941.1572114](https://doi.org/10.1145/1571941.1572114)
  - **Claim:** a trivially simple, unsupervised rank combination beats every input
    system, Condorcet fuse, and CombMNZ.
  - **Mechanism:** `RRF(d) = Σ_r 1/(k + rank_r(d))` with **k = 60**, which was fixed in a
    pilot run and never tuned afterwards.
    - Only ranks enter the formula, so nothing needs normalising.
    - A document that several systems rank moderately high beats one that a single
      system ranks first.
    - `k` stops any one system's top rank from dominating.
  - **Applies to:** both. In Part A it can merge several engines (Tavily, Wikipedia,
    Bing) or several queries. In Part B it can replace today's raw max-score merge.
- Score-level alternatives covered above: DPR's linear hybrid, M3's weighted mode sum,
  and Generative Agents' normalised weighted sum (below).

### 4. Reranking

A reranker is too expensive to run over the whole corpus, but it reads the query and a
candidate *together*, so it can judge relevance far better than the first stage. It runs
on the top few dozen candidates only.

- **monoBERT**: Nogueira, Cho. *Passage Re-ranking with BERT.* Preprint.
  [arXiv:1901.04085](https://arxiv.org/abs/1901.04085)
  - **Claim:** a BERT cross-encoder over BM25's top 1,000 raises MS MARCO MRR@10 from
    16.7 (BM25) to 36.5 on dev.
  - **Mechanism:** `[CLS] query [SEP] passage` goes into a binary relevance classifier,
    and candidates are sorted by P(relevant).
  - **Applies to:** A mainly, to rerank extracted web passages. It costs one forward pass
    per passage.
- **monoT5**: Nogueira, Jiang, Lin. *Document Ranking with a Pretrained
  Sequence-to-Sequence Model.* Findings of EMNLP 2020.
  [arXiv:2003.06713](https://arxiv.org/abs/2003.06713)
  - **Claim:** a seq2seq model trained to emit "true"/"false" ranks at least as well as
    encoder classifiers, and better when data is scarce.
  - **Mechanism:** the input is `Query: q Document: d Relevant:`. A softmax over the
    "true" and "false" logits gives the relevance probability.
  - **Applies to:** A. It is also the template for using an LLM as a pointwise relevance
    judge.
- **RankGPT**: Sun, Yan, Ma, et al. *Is ChatGPT Good at Search?* EMNLP 2023.
  [arXiv:2304.09542](https://arxiv.org/abs/2304.09542)
  - **Claim:** an instructed LLM reranks competitively with supervised rerankers, and a
    distilled 440M model beats a 3B supervised one on BEIR.
  - **Mechanism:** the LLM outputs a *permutation* of a numbered passage window, and the
    window slides back to front (window 20, step 10).
  - **Applies to:** A. The agent's own LLM could rerank, at the cost of extra calls.
- ColBERT (above) can also rerank BM25 candidates.

### 5. Context packing

Retrieval ends at the model's context window, not at a ranked list. How many items go
in, how long each is, and in what order all change the answer.

- **Lost in the Middle**: Liu, Lin, Hewitt, Paranjape, Bevilacqua, Petroni, Liang. TACL
  12:157–173, 2024. [arXiv:2307.03172](https://arxiv.org/abs/2307.03172)
  - **Claim:** models use long contexts unevenly. Accuracy is highest when the relevant
    passage is first or last, and drops in the middle, for GPT-3.5 below its closed-book
    56.1%.
  - **Mechanism:** a controlled sweep of the gold passage's position among distractors
    gives a U-shaped curve (primacy and recency).
  - **Applies to:** both. Put the best items at the edges, and cap k instead of filling
    the window.
- **FreshLLMs**: Vu, Iyyer, Wang, et al. Findings of ACL 2024.
  [arXiv:2310.03214](https://arxiv.org/abs/2310.03214)
  - **Claim:** injecting up-to-date search evidence (FreshPrompt) fixes much of LLMs'
    failure on fast-changing facts, and "both the number of retrieved evidences and their
    order play a key role."
  - **Mechanism:** evidence is sorted by date and the most relevant is kept closest to
    the end of the prompt.
  - **Applies to:** A. It speaks directly to how `search_web` output should be laid out.
- **RECOMP**: Xu, Shi, Choi. ICLR 2024.
  [arXiv:2310.04408](https://arxiv.org/abs/2310.04408)
  - **Claim:** compressing retrieved documents to about 6% of their length loses little,
    and returning *nothing* when they do not help ("selective augmentation") is part of
    the method.
  - **Mechanism:** trained extractive and abstractive compressors, either of which may
    output an empty string.
  - **Applies to:** both. Fetched pages are long, and memory recall should be allowed to
    return nothing.
- **Self-RAG**: Asai, Wu, Wang, Sil, Hajishirzi. ICLR 2024.
  [arXiv:2310.11511](https://arxiv.org/abs/2310.11511)
  - **Claim:** retrieving a fixed k whether or not it is needed hurts. A model that
    decides when to retrieve and critiques passages does better.
  - **Mechanism:** trained reflection tokens gate retrieval and grade passages.
  - **Applies to:** both. It is the main argument against unconditional recall.
- **Generative Agents**: Park, O'Brien, Cai, Morris, Liang, Bernstein. UIST 2023.
  [arXiv:2304.03442](https://arxiv.org/abs/2304.03442)
  - **Claim:** an agent that stores all its experiences as text, retrieves them by
    recency, importance and relevance, and reflects on them behaves believably over long
    runs.
  - **Mechanism:**
    - Each score is min-max normalised to [0, 1]:
      - recency: exponential decay, factor 0.995 per game hour;
      - importance: the LLM rates each memory 1–10 at write time;
      - relevance: cosine similarity to the query.
    - The final score is their sum, with all weights = 1.
    - The top memories that fit are packed into the prompt.
  - **Applies to:** B. Recency and importance are signals memvid does not have.
- **MemGPT**: Packer, Wooders, Lin, Fang, Patil, Stoica, Gonzalez. Preprint.
  [arXiv:2310.08560](https://arxiv.org/abs/2310.08560)
  - **Claim:** OS-style virtual context (memory tiers with explicit paging) gives
    effectively unbounded context.
  - **Mechanism:**
    - The main context holds the system prompt, a small working context, and a FIFO
      message queue that starts with a recursive summary.
    - Recall and archival storage live outside the prompt, and the LLM pages them in with
      function calls.
    - The system warns the model at 70% of the window and flushes at 100%.
  - **Applies to:** B. Chatty's memory is MemGPT-shaped today: the model decides when to
    call `search_memory`.

### Frame and evaluation

- **RAG**: Lewis, Perez, Piktus, et al. NeurIPS 2020.
  [arXiv:2005.11401](https://arxiv.org/abs/2005.11401)
  - Origin of the term: parametric generator + non-parametric retriever. Only the framing
    applies here, since Chatty's models are frozen.
- **ReAct**: Yao, Zhao, Yu, et al. ICLR 2023.
  [arXiv:2210.03629](https://arxiv.org/abs/2210.03629)
  - Thought → search action → observation. This is the loop that calls `search_web`
    ([M1](./m1-react.md)).
- **WebGPT**: Nakano, Hilton, Balaji, et al. Preprint.
  [arXiv:2112.09332](https://arxiv.org/abs/2112.09332)
  - A model browses a text browser with search, click, scroll and quote actions. It is
    trained by behaviour cloning, and answers are then chosen against a reward model.
    It has to collect quotes that support its answer: the Part A loop in trained form.
- **SimpleQA**: Wei, Karina, Chung, et al. *Measuring short-form factuality in large
  language models.* OpenAI, preprint. [arXiv:2411.04368](https://arxiv.org/abs/2411.04368)
  - 4,326 short fact-seeking questions with one indisputable answer, adversarially
    collected against GPT-4.
- **FRAMES**: Krishna, Krishna, Mohananey, et al. *Fact, Fetch, and Reason.* NAACL 2025.
  [arXiv:2409.12941](https://arxiv.org/abs/2409.12941)
  - 824 multi-hop questions that each need several Wikipedia articles, labelled by
    reasoning type. The paper reports accuracy of 0.40 with no retrieval and 0.66 with its
    multi-step retrieval pipeline.

## Part A — open-web retrieval

### How it worked before this module

`crates/chatty-core/src/tools/search_web_tool.rs`, as of `main` on 2026-09-23 (for the
state after this module see the pipeline table above and
[Where it ended](#where-it-ended)):

- **Keyed:**
  - A Tavily key calls `api.tavily.com/search` with `search_depth: basic`.
  - A Brave key calls the Brave web search API.
  - There is no retry and no failover. An HTTP error becomes a tool error.
- **Keyless:**
  - The tool scrapes Bing (`b_algo` blocks, with tracking redirects decoded, AGE-506).
    If Bing fails, it tries DuckDuckGo lite.
  - A **decoy detector** (`results_match_query`) rejects Bing pages whose results share
    no content term with the query. A **challenge detector** rejects DDG's 202
    "anomaly" page (AGE-495). Both fail loudly rather than hand the model unrelated
    links.
- Every result carries a `source` tag (`tavily`, `brave`, `bing`, `duckduckgo`),
  added for this eval.
- `fetch` (`fetch_tool.rs`) is separate: it turns HTML into text with its own scanner and
  pages through long documents with `start_index`. Nothing selects passages yet.

Measured on 2026-09-23, before any Phase 2 change (full numbers below):

- Bing serves our scraper **decoy pages** (casino and Google-help results) for ordinary
  questions, and the decoy detector correctly rejects them.
- DuckDuckGo lite now answers `200` with real results, but in a new markup: single-quoted
  `class='result-link'` and `//duckduckgo.com/l/?uddg=` redirect hrefs. The parser
  expects double quotes and `http` hrefs, so it finds **zero** results. The challenge
  detector does not fire either, so the tool reports an *empty* result, not an error.
  This is a silent failure: exactly the "no working search" of
  [AGE-498](https://linear.app/agents-research/issue/AGE-498).

### The eval

Details: [`evals/search/README.md`](https://github.com/boersmamarcel/chatty2/blob/main/evals/search/README.md). The harness calls the
real tool, with no agent loop:

- **SimpleQA (AGE-515):**
  - 300 questions, stratified by topic, split 200 DEV / 100 HOLDOUT.
  - Metric: **answer hit@k**, meaning the normalised gold answer appears as a whole-token
    run in the title+snippet of a result ranked ≤ k.
- **FRAMES (AGE-516):**
  - 150 questions, stratified by primary reasoning type, split 100 DEV / 50 HOLDOUT.
  - Metrics: **source recall@k** (share of gold Wikipedia pages among the top k URLs) and
    **all-sources@k**.
  - Two modes: *single* (the prompt as the query) and *fan-out* (3 committed sub-queries,
    merged round-robin, scored at the same k).
- **Leak blocklist:** domains that republish the datasets (Hugging Face, Kaggle, the
  simple-evals repo, …) keep their rank but never count as hits.
- **Record/replay:** raw responses are recorded per backend and query, so reruns replay
  them through the current parsers and cost nothing.
- **Keyless runs** are reported with and without the `wikipedia` source. The version
  without Wikipedia is the honest general-web number, because FRAMES is all-Wikipedia
  and much of SimpleQA is too.

### Baselines (2026-09-23)

Keyed means Tavily `basic`. Brave was not run: there is no key, and it was skipped by
decision. Tavily was measured live once, on SimpleQA DEV only. There were then no more
live calls, to protect the credit budget (179 credits used); later Tavily numbers are
replays of those recordings.

| Config | Set | error | empty | hit@1 / @5 | FRAMES recall@10 | p50 / p95 ms |
|---|---|---|---|---|---|---|
| Keyed (Tavily) | SimpleQA DEV | 10.5%¹ | 0% | 59.5% / **70.0%** | — | 1200 / 3517 |
| Keyless (Bing → DDG) | SimpleQA DEV | 47% | 46% | 0% / **0%** | — | 683 / 1271 |
| Keyless | SimpleQA HOLDOUT | 74% | 15% | 0% / 0% | — | 688 / 788 |
| Keyless | FRAMES single DEV | 57% | 15% | 0% / 0% | 0% | 221 / 846 |
| Keyless | FRAMES fan-out DEV | 26% | 29% | 0% / 0% | 0.5% | 1523 / 2194 |

¹ All 21 are replay misses (items never recorded). On the 179 recorded items, Tavily had
0 errors and hit@5 ≈ 78%.

Two findings from the baseline itself:

- **A Tavily development key rate-limits hard.** At concurrency 4, 65 of 200 calls got
  `429`, and the block then held for minutes. At concurrency 1 there were none. An agent
  firing parallel searches on a dev key will see this.
- **Keyless was worse than "no working search": it returned wrong results as if they were
  right.**
  - Bing answers our scraper with decoy pages.
  - The one-term decoy guard let 14 of them through as "ok": "architect" matched
    architecture firms in Cyprus for a question about a palace in Buenos Aires.
  - DuckDuckGo's real result pages parsed to nothing.

### Iteration log

One entry per Phase 2 change, kept or reverted, in the form "paper says X; in Chatty we
measured Y; because Z". Predictions and results were posted on
[AGE-517](https://linear.app/agents-research/issue/AGE-517) before and after each run.

**1. DuckDuckGo lite parser. Kept.**
- **Says:** nothing to cite; this is a recall bug at the candidate stage.
- **Measured:** SimpleQA DEV hit@5 0% → 22% (McNemar 44/0, p < 0.001). FRAMES did not
  move, and the prediction was 3–8%.
- **Because:** DDG serves roughly the first 25 requests of a session and then blocks. The
  SimpleQA DEV run went first and got the working pages. The gain is real but depends on
  DDG's block state. HOLDOUT, recorded while blocked, shows 0% from it.

**2. Wikipedia API in parallel with the scrape. Kept.**
- **Says:**
  - ReAct's action space was a keyless Wikipedia search API.
  - Rewrite-Retrieve-Read and query2doc: shape the query for the retriever.
- **Measured:**
  - SimpleQA DEV hit@5 22% → 33% (29/7, p = 0.0005). Error+empty 57% → 0%.
  - FRAMES recall@10: single 0% → 34%, fan-out 0.5% → 44%. All-sources@10 fan-out
    0% → 13%.
  - HOLDOUT confirms: error+empty 0%; SimpleQA hit@5 24%; FRAMES recall@10 38% / 43%.
- **Because:**
  - Wikipedia's search ANDs every term, so a whole question finds nothing. The content
    words joined with `OR` let rare entity names drive the ranking.
  - A search snippet is only ~150 characters, so the right article at rank 1 often still
    misses the answer string. That is the job of the passage stage.
  - The first attempt failed on Wikimedia's 10 requests/minute cap for unidentified
    User-Agents. Wikipedia calls now send a User-Agent with the project URL (200/minute).
- **Without Wikipedia**, SimpleQA DEV stays at 21.5%.

**3. Browser escalation (headless Chrome when the scrape is blocked). Reverted.**
- **Says:** AGE-517's premise was that a real browser on a home IP passes checks plain
  HTTP fails. One manual probe agreed: Chrome got the real Jerlov Award results where
  reqwest got decoys.
- **Measured:** 342 escalations, 0 hits gained. 188 were Bing "There are no results for
  …" pages and 154 were connection refusals (`chrome-error://`). Latency got worse.
- **Because:** Bing blocks by IP, not by client. After a day of evaluation traffic this
  workstation's IP was flagged, and a real Chrome, even at human pace, got the same
  empty or decoy pages. Some of those decoys were adult-site forum threads.
- **Caveat:** this says little about a user doing a few searches a day on a clean IP.
  The patch is kept outside the repo for a re-test from another network.

**4. Fetch the top pages and put the best BM25 passage in the snippet. Kept, keyless only.**
- **Says:**
  - BM25 (term saturation, length normalisation) is the robust zero-shot scorer (BEIR).
  - RECOMP: pass the useful part of a document, not the engine's excerpt.
- **Measured:**
  - Keyless SimpleQA hit@5 33% → 48% (30/0, p < 0.001); hit@1 13% → 27%; p95
    1.3 s → 2.3 s.
  - Keyed (Tavily) hit@5 70% → 72.5% (5/0, p = 0.07), with p95 up to about 7.8 s.
- **Because:**
  - A Wikipedia search snippet is about 150 characters around the query terms, while the
    answer usually sits a sentence or two away in the article. The passage window
    catches it.
  - Tavily's `content` is already a query-focused extract, so a second lexical selection
    adds little there and costs a slow page fetch. The stage therefore runs on the
    keyless path only.
- **Statistics caveat:** idf here comes from the passages of 5 pages, a collection far
  smaller than BM25 assumes. It still works, because the rare entity names in a question
  are rare in almost any collection.

**5. Widen the pool to 10 and rerank it by best BM25 passage. Reverted.**
- **Says:** two-stage retrieval (monoBERT): a cheap stage for recall, then a precise
  stage over a small pool.
- **Measured:**
  - SimpleQA hit@5 48% → 46.5% (11 gained, 14 lost, p = 0.69); hit@1 27% → 30.5%.
  - FRAMES fan-out recall@10 44% → 40%.
  - p95 rose by about 1.5 s.
- **Because:** here the second stage was still lexical. A page that repeats the question's
  rare words, such as a list or an overview page, outscores the page that answers it. A
  bigger pool adds as many near-misses as answers.
- **Confound:** 10 page fetches per search made each search cost 11 Wikipedia calls, and
  763 of them hit the rate limit.

**6. The same pool, ordered by a cross-encoder. Kept, optional.**
- **Setup:** `bge-reranker-v2-m3` on the local GPU behind a `/rerank` endpoint.
- **Says:**
  - monoBERT/monoT5: reading question and passage together judges relevance, not just
    overlap.
  - BEIR: rerankers are the strongest zero-shot systems.
- **Measured:**
  - SimpleQA hit@5 48% → 54% (17/5, p = 0.019); hit@1 27% → 43%.
  - FRAMES fan-out all-sources@10 13% → 20% (8/1, p = 0.046).
  - p95 2.3 s → 3.5 s.
- **Because:** the pool from iteration 5 was fine; its ranking was not. With a
  cross-encoder the same candidates give 17 wins against 5 losses, where BM25 gave 11
  against 14.
- **Without a reranker configured, the pool is not widened.** A BM25 rerank of a larger
  pool was measured to be worse.

**7. The cross-encoder chooses the passage, not only the page. Kept (small).**
- **Change:** each candidate's top 3 BM25 passages go to the cross-encoder. The best one
  ranks the page and leads its snippet.
- **Says:** monoBERT's rerank-a-shortlist applied inside the page; RECOMP on selecting what
  text reaches the model.
- **Measured:**
  - SimpleQA hit@5 54% → 57% (11/5, p = 0.21, not significant); hit@1 43% → 49%.
  - FRAMES answer hit@5 +3 points on both modes.
  - Latency unchanged.
- **Because:** BM25 picks the passage that repeats the question's words, while the
  cross-encoder picks the one that answers it. Recall is now the limit: the answer has to
  be in one of 10 pages' top 3 passages.

**8. Multi-query `queries` argument with RRF. Reverted.**
- **Says:** RRF (Cormack et al.) fuses rankings by rank alone. RAG-Fusion and Self-Ask
  support searching sub-questions together.
- **Measured:**
  - FRAMES, one call with sub-queries vs. iteration 7's three-call fan-out:
    all-sources@10 21% → 14% (1/8, p = 0.046). The only win was p95, 6.1 s → 2.7 s.
  - The single-query RRF swap cost SimpleQA 1.5 points.
- **Because:** RRF rewards *agreement*, but multi-hop recall needs *coverage*. Each hop
  has its own gold page, which only its own sub-query retrieves. Fused into one 10-slot
  pool and judged against the whole question, those pages rank out.
  - Fan-out keeps coverage by construction: each sub-query's results are ranked against
    that sub-query, and each gets its own slots.
  - This is the clearest case in this module of a paper's mechanism being right, but for
    a different objective.

**9. Widen the cross-encoder pool from 10 to 20. Reverted. The loop stops here.**
- **Says:** rerankers in the literature work over the BM25 top 100 to 1,000 (monoBERT,
  RankGPT), so a 10-deep first stage is shallow.
- **Measured:**
  - SimpleQA hit@5 57% → 59% (6/2, p = 0.29), but p95 3.2 s → 7.2 s.
  - FRAMES fan-out all-sources@10 21% → 18%.
- **Because:** 20 page fetches mean the slowest of 20 sets the latency. The extra depth
  adds near-duplicate distractors for multi-hop.
- Iterations 8 and 9 were reverted back to back, which is the stop condition.

### Where it ended

The final configuration is iteration 7: DDG parser fix, Wikipedia API, BM25 passages and,
with a reranker endpoint configured, a cross-encoder over a 10-candidate pool. Keyless
numbers are **with the reranker** (`bge-reranker-v2-m3`); without one the keyless tool is the
iteration 4 pipeline. Keyed (Tavily) code is unchanged by this module.

**DEV, baseline → final**

| Config | Set | error+empty | hit@1 | hit@5 | hit@5 without Wikipedia | recall@10 | all-sources@10 | p50 / p95 |
|---|---|---|---|---|---|---|---|---|
| Keyless | SimpleQA (200) | 93% → **0%** | 0% → 49% | 0% → **57%** | 0% → 27% | — | — | 0.7/1.3 s → 1.4/3.2 s |
| Keyless | FRAMES single (100) | 72% → 0% | 0% → 7% | 0% → 18% | 0% → 0% | 0% → 33.7% | 0% → 8% | 0.2/0.8 s → 1.4/2.4 s |
| Keyless | FRAMES fan-out (100) | 55% → 0% | 0% → 4% | 0% → 19% | 0% → 2% | 0.5% → **48.8%** | 0% → **21%** | 1.5/2.2 s → 3.8/6.1 s |
| Keyed (Tavily) | SimpleQA (200) | 10.5%¹ | 59.5% | **70.0%** | — | — | — | 1.2/3.5 s (live) |

**HOLDOUT, baseline → final** (run once per two kept changes, and once at the end)

| Config | Set | error+empty | hit@1 | hit@5 | hit@5 without Wikipedia | recall@10 | all-sources@10 |
|---|---|---|---|---|---|---|---|
| Keyless | SimpleQA (100) | 89% → **0%** | 0% → 40% | 0% → **43%** | 0% → 0% | — | — |
| Keyless | FRAMES single (50) | 76% → 0% | 0% → 4% | 0% → 12% | 0% → 2% | 0% → 38.3% | 0% → 12% |
| Keyless | FRAMES fan-out (50) | 52% → 0% | 0% → 2% | 0% → 18% | 0% → 4% | 0% → 43.3% | 0% → 14% |

¹ Replay misses, not errors: Tavily had 0 errors on the 179 recorded items (hit@5 ≈ 78%
there). The keyed HOLDOUT was not run, to protect the credit budget.

**FreshQA check** (100 valid-premise questions, 2026-04-21 release, hit@5; one-off, not
tuned on)

| | Tavily | Keyless (final) | Keyless without Wikipedia |
|---|---|---|---|
| All | **78%** | **48%** | 14% |
| Never-changing (33) | 91% | 55% | 21% |
| Slow-changing (32) | 91% | 62.5% | 16% |
| Fast-changing (35) | 54% | **29%** | 6% |
| error+empty | 0% | 0% | 69% |

**Targets**

| Target | Result |
|---|---|
| Keyless error+empty < 5% | **Met** (0% on DEV and HOLDOUT) |
| Keyless SimpleQA hit@5 within 10 points of Tavily | **Not met**: DEV 57% vs 70% (−13). HOLDOUT 43%. FreshQA 48% vs 78% (−30). |
| Keyed error+empty < 3% | Met on DEV at concurrency 1. A Tavily *dev* key gets `429`-blocked at concurrency 4. Retry/failover was not built, because the decision rule did not select it at 0% errors. |

**What the numbers say.** The keyless tier went from worse than nothing (93% error or
empty, plus decoys returned as results) to reliable, and to good on encyclopedic
questions. Almost all of its accuracy comes from Wikipedia:
- Without Wikipedia, SimpleQA HOLDOUT is 0% and FreshQA 14%.
- On fast-changing facts it finds the answer only 29% of the time.

From this workstation the general-web side is blocked (Bing decoys, DDG blocks), and a
real browser did not get past that. Keyless search should be presented as
"encyclopedic, not current", and a keyed provider remains the recommendation for current
events.

**What did not work, and why**

| Change | Paper's promise | What happened | Why |
|---|---|---|---|
| Browser escalation | a real browser passes bot checks | 0 gains, slower | blocking is per IP, not per client |
| BM25 rerank of a wider pool | two-stage retrieval | 11 gained / 14 lost | lexical overlap cannot separate the answer page from pages that repeat the question |
| Multi-query + RRF | fusion by agreement | all-sources@10 21% → 14% | multi-hop needs coverage, and RRF rewards agreement |
| Pool of 20 | deeper first stage | +2 n.s., p95 ×2.3 | the slowest page fetch decides; distractors grow with depth |
| Passages on the keyed path | select what the model reads | +2.5 n.s., p95 ×2 | Tavily's snippets are already query-focused extracts |

## Part B — internal memory retrieval

How it works today, from the code (`crates/chatty-core/src`) and
[agent-memory.md](../../agent-memory.md). Nothing here was changed by this module.

**Storage.**
- One memvid file (`memory.mv2`). Every `remember` call is one frame with no chunking,
  plus an optional title and tags.
- The text is indexed by Tantivy (lex). If embeddings are enabled (off by default), the
  content is also embedded by the configured provider (OpenRouter, Ollama or Azure) and
  stored with the frame.
- There are no timestamps or importance scores in Chatty's metadata.

**Query formulation.**
- The model's `search_memory` query is used verbatim; there is no rewrite.
- **There is no automatic recall.** The per-turn injection (`auto_context.rs`) was
  removed in `6cef438f` ("improved ttft", 2026-04-29). The preamble now tells the model to
  call `search_memory` proactively.
- `simplify_memory_query` and `build_memory_context_block` survive with test-only
  callers.
- Chatty's memory is therefore **MemGPT-shaped** (the model pages memory in), not
  Generative-Agents-shaped (the system pushes memories in).

**Candidate retrieval.**
- BM25 via memvid/Tantivy always runs, with Tantivy's default k1/b and 500-char
  snippets.
- Vector search runs when embeddings are on. It is an *exact brute-force L2 scan*: the
  memvid `vec` feature (HNSW) is not compiled in, although code comments say "HNSW". The
  score is `1/(1+L2)`.

**Fusion.**
- `merge_search_results` (`tools/search_memory_tool.rs`) deduplicates on the first 200
  characters, keeps the **higher raw score**, then sorts by raw score.
- This compares an unbounded BM25 score with a score ≤ 1, so a lexical hit almost always
  outranks a vector hit. The vector list barely matters once both lists are non-empty.
- This is the textbook case for rank fusion (RRF) instead of score fusion.

**Reranking.** None, and no score threshold.

**Context packing.**
- `select_context_hits` keeps up to 3 facts, then up to 2 skills, then fills from the
  rest.
- Output is JSON `{text, title, relevance_score, source}`. Lexical hits are 500-char
  snippets; vector hits are the full frame text.

**`doc_retriever`** (local Markdown/text files):
- One chunk per Markdown heading section, with no size limit and no overlap. The title
  is indexed twice as a boost.
- Tokens are lowercase alphanumeric runs, with no stemming.
- Hand-written BM25 with k1 = 1.2, b = 0.75 and the non-negative
  `ln((N−df+0.5)/(df+0.5)+1)` idf.
- Top 3 results (max 5), chunks cut to 900 chars.
- The index is rebuilt on every call.

**Skills** (`SkillService::load_hits`):
- Cosine similarity to a cached skill embedding when embeddings are on.
- Otherwise keyword overlap, with 0.5 for an empty query.

**Trust.** Nothing marks recalled memory, skills or fetched web text as untrusted when it
enters the context.

## Part C — what transfers

Proposals only, filed as
[AGE-520](https://linear.app/agents-research/issue/AGE-520); none is built here. Two
measured lessons from Part A change the proposals:
- **RRF fits memory, not multi-hop.** Memory's lexical and vector lists are two views of
  the *same* need, so agreement is the right signal. That is unlike web sub-queries,
  where each hop needs its own page.
- **The shared reranker is the bigger win.** A cross-encoder puts both lists on one
  scale, which fixes the max-score merge bug as a side effect. The concrete proposal is on
  AGE-520: one `/rerank` endpoint for web and memory, and BGE-M3 as the matching
  embedder, which needs a re-embedding migration.

| Stage | Share between A and B? | Why / why not |
|---|---|---|
| Query formulation | **Partly.** A shared "sub-query" prompt pattern (Self-Ask / RAG-Fusion) | Memory queries are short and personal; web queries need entity names. The decomposition idea transfers, the prompt does not. |
| Candidate retrieval | **No** | Different indexes: a remote engine vs. a local Tantivy + vector store. Different failure modes: rate limits and bot detection vs. empty stores. |
| Fusion | **Only for B.** RRF for memory's lexical + vector lists | Both merge incomparable scores. In A, RRF across sub-queries was measured to hurt multi-hop (iteration 8), so A keeps interleaving. |
| Reranking | **Yes: one cross-encoder endpoint**, plus the BM25 passage scorer (`tools/passages.rs`) | The cross-encoder was the largest precision gain in A (hit@1 27% → 49%). `doc_retriever::bm25_rank` and `passages::score_passages` are the same formula over different corpus statistics. |
| Context packing | **Yes: one packer** (budget, edge ordering, allow empty) | Lost in the Middle and RECOMP apply to both. Trust differs, so web and memory need separate delimiters and labels. |

What differs, and why it matters:

| Property | Open web (A) | Memory (B) | Consequence |
|---|---|---|---|
| Corpus size | Effectively infinite, remote | Hundreds to thousands of frames, local | A has to delegate candidate retrieval to an engine; B can afford exact search |
| Freshness | Changes daily | Changes only when the user or agent writes | A needs no cache TTL beyond a session; B can cache embeddings indefinitely |
| Trust | Adversarial (SEO, injection, decoys) | Mostly the user's own words, but contains past web text | A needs decoy/challenge detection and untrusted-content labels; B needs provenance |
| Latency | 0.5–3 s per call, 1–3 s more for a browser | Milliseconds | A should run backends in parallel; B can run several retrievers sequentially |
| Relevance labels | Public benchmarks (SimpleQA, FRAMES) | None | A can be tuned on data; B needs zero-shot methods (BM25, Contriever-style) |

## Eval protocol

- Iterate on DEV; run HOLDOUT once after every two kept changes. A DEV gain that
  HOLDOUT does not show is overfitting.
- Report for **both** configs:
  - error rate and empty rate;
  - SimpleQA hit@1/3/5;
  - FRAMES source recall@5 and all-sources hit;
  - p50/p95 latency (keyless: split into fast path and browser-escalated calls);
  - Tavily credits used;
  - McNemar against the previous best (`search_eval paired` + `paired_report`).
- Keep a change if its target config improves on the primary metric and the other
  config does not regress on error rate, hit@5 or p95 latency.
- Targets (DEV, confirmed on HOLDOUT):
  - keyed: error+empty < 3%;
  - keyless: error+empty < 5%, and SimpleQA hit@5 within 10 points of Tavily.
- Scoring, normalisation, the leak blocklist and the ID lists are frozen.

## Production landing

| Mechanism | Outcome |
|---|---|
| DDG parser fix, Wikipedia API in parallel, loud failures | **Default** for keyless (shipped with this module) |
| Fetch + BM25 best passage in snippets | **Default** for keyless; not used with Tavily/Brave (no significant gain, twice the p95) |
| Cross-encoder rerank of a 10-candidate pool | **Setting**: needs a local `/rerank` server ([AGE-521](https://linear.app/agents-research/issue/AGE-521)) |
| Browser escalation, multi-query + RRF, pool of 20 | Rejected on measurement; patches kept outside the repo for a re-test |
| Keyed retry + keyless failover | Not built: 0% keyed errors at a human pace. The dev-key `429` risk is real for parallel agents. |

## Reserved-function candidates

These functions hold the core ideas of this module. On 2026-09-23 the human chose to
reserve **none** of them (and lifted the M6 reflection gate,
[AGE-518](https://linear.app/agents-research/issue/AGE-518), for this loop). Nothing was
added to [`RESERVED.md`](../../../RESERVED.md).

1. **`rrf_fuse`**: built in iteration 8 and reverted with it. The patch is kept outside the
   repo. It is the right function for memory (Part C) but not for multi-hop web.
2. **BM25 passage scoring**: `tools/passages.rs::score_passages`, shipped (iteration 4).
   Its statistics come from a tiny, per-query collection.
3. **`pack_context`**: not built as a function. Packing is currently "the best passage
   leads the snippet"; a budgeted, order-aware packer remains a Part C proposal.

## Depends on

- [M1 ReAct](./m1-react.md): the loop that issues search calls.
- [M4 ACE](./m4-ace.md): shares the memory store that Part B reads.

## Further reading

- [agent-memory.md](../../agent-memory.md): memory architecture.
- [App ↔ research bridge](../app-research-bridge.md).
- [`evals/search/README.md`](https://github.com/boersmamarcel/chatty2/blob/main/evals/search/README.md): how to run the eval.
