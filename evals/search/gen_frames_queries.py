#!/usr/bin/env python3
"""AGE-516: generate FRAMES fan-out sub-queries ONCE and commit the output
(frames_queries.jsonl), so fan-out runs are deterministic and query generation
is not a hidden variable. Do not regenerate for a comparison run.

Model used for the committed file: RedHatAI/Qwen3.8-27B-INT4 on local vLLM
(OpenAI-compatible API), temperature 0, thinking off.

usage: gen_frames_queries.py <frames.jsonl> <ids.txt> <out.jsonl> [base_url] [model]
"""
import json, re, sys, urllib.request

PROMPT = """You are preparing web searches for a multi-hop question.
Write 3 short, self-contained web search queries (each under 12 words) that
together retrieve the facts needed to answer it. Name specific entities; do
not answer the question. Output exactly 3 lines, one query per line, with no
numbering, quotes or other text.

Question: {q}"""

def main():
    data, ids, out = sys.argv[1:4]
    base = sys.argv[4] if len(sys.argv) > 4 else "http://172.17.0.1:8000/v1"
    model = sys.argv[5] if len(sys.argv) > 5 else "RedHatAI/Qwen3.8-27B-INT4"
    items = {r["id"]: r for r in map(json.loads, open(data))}
    with open(out, "w") as f:
        for id_ in open(ids).read().split():
            body = {
                "model": model,
                "temperature": 0,
                "seed": 516,
                "max_tokens": 200,
                "chat_template_kwargs": {"enable_thinking": False},
                "messages": [{"role": "user", "content": PROMPT.format(q=items[id_]["prompt"])}],
            }
            req = urllib.request.Request(base + "/chat/completions", json.dumps(body).encode(),
                                         {"Content-Type": "application/json"})
            text = json.load(urllib.request.urlopen(req, timeout=300))["choices"][0]["message"]["content"]
            # Strip list markers ("1. ", "2) ", "- ") but never a leading
            # number that is part of the query ("50th most populous ...").
            queries = [re.sub(r'^\s*(\d+[.)]|[-*])\s+', '', l).strip().strip('"')
                       for l in text.strip().splitlines() if l.strip()][:3]
            f.write(json.dumps({"id": id_, "queries": queries}) + "\n")
            f.flush()

if __name__ == "__main__":
    main()
