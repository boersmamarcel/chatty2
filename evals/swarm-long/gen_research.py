#!/usr/bin/env python3
"""Generator for the research-write long tasks lr01..lr06 (EV-7 / AGE-826).

Deterministic: fixed seeds, no clocks. Re-running reproduces the bytes exactly
and rewrites only evals/swarm-long/tasks/lr0*. Hand-written fact documents live
in src-research/lr0N.py; src-research/filler.py pads them (and builds the pure
filler documents) from templates. Python 3.6, stdlib only.
"""
import json
import os
import random
import re
import shutil
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "src-research"))
import filler  # noqa: E402

IDS = ["lr01", "lr02", "lr03", "lr04", "lr05", "lr06"]
TARGET_WORDS = 1300  # per document (hand docs are padded up to this)


def load(tid):
    return __import__(tid)


def fact_patterns(parts):
    pats = []
    for p in parts:
        pats += p.get("all", []) + p.get("any", [])
    return [re.compile(x, re.I) for x in pats]


def seed(tid, name, salt):
    return random.Random(zlib.crc32(("%s/%s/%d" % (tid, name, salt)).encode()))


def padded(tid, name, words, pats, extra):
    """Filler text of ~words that matches none of the fact patterns."""
    salt = 0
    while True:
        r = seed(tid, name, salt)
        text = filler.pad(r, words, extra)
        if not any(p.search(text) for p in pats):
            return text
        salt += 1


def build_doc(tid, spec, pats, extra, is_filler=False):
    name, title = spec["name"], spec["title"]
    if is_filler:
        return "# %s\n\n%s\n" % (title, padded(tid, name, TARGET_WORDS, pats, extra))
    core = spec["core"].strip("\n")
    chunks = core.split("@@PAD@@")
    if len(chunks) == 1:
        chunks.append("")
    need = max(TARGET_WORDS - len(core.split()), 200)
    each = need // (len(chunks) - 1)
    out = ["# %s\n" % title]
    for i, c in enumerate(chunks):
        out.append(c.strip("\n"))
        if i < len(chunks) - 1:
            out.append(padded(tid, "%s#%d" % (name, i), each, pats, extra))
    return "\n\n".join(x for x in out if x.strip()) + "\n"


def main():
    only = sys.argv[1:] or IDS
    for tid in only:
        m = load(tid)
        root = os.path.join(HERE, "tasks", tid)
        if os.path.isdir(root):
            shutil.rmtree(root)
        docs_dir = os.path.join(root, "workspace", "docs")
        os.makedirs(docs_dir)
        os.makedirs(os.path.join(root, "solution"))
        pats = fact_patterns(m.PARTS)
        names = []
        for spec in m.DOCS:
            names.append(spec["name"])
            with open(os.path.join(docs_dir, spec["name"]), "w") as f:
                f.write(build_doc(tid, spec, pats, m.EXTRA_TOPICS))
        for name, title in m.FILLER:
            names.append(name)
            with open(os.path.join(docs_dir, name), "w") as f:
                f.write(build_doc(tid, dict(name=name, title=title), pats, m.EXTRA_TOPICS, True))
        qs = "\n".join("%d. %s" % (i + 1, q) for i, q in enumerate(m.QUESTIONS))
        request = (
            "# Request\n\n%s\n\nWrite `brief.md` in the workspace root answering the %d numbered questions below, "
            "in order. Give each answer explicitly (the number, date or name asked for, with units) and cite the "
            "source file in square brackets right after it, for example `[policy-v3.md]`. Cite only the files under "
            "`docs/`; use the corpus only, no web and no URLs. The documents contain superseded versions, "
            "similar-looking material for other entities or years, and places where sources disagree: "
            "state the answer that the rules in the documents make authoritative.\n\n## Questions\n\n%s\n"
            % (m.INTRO, len(m.QUESTIONS), qs))
        ws = os.path.join(root, "workspace")
        with open(os.path.join(ws, "REQUEST.md"), "w") as f:
            f.write(request)
        prompt = ("Read REQUEST.md and the documents in docs/ (there are many and they are long; search them, do not try to "
                  "read everything at once). Write brief.md in the workspace root answering all %d numbered questions, "
                  "each answer citing its source file in square brackets, e.g. [policy-v3.md]. Corpus only, no web. "
                  "Finish by summarising the answers in your final chat message." % len(m.QUESTIONS))
        with open(os.path.join(root, "task.json"), "w") as f:
            json.dump({"family": "research-write", "title": m.TITLE, "prompt": prompt,
                       "deliverable": "brief.md"}, f, indent=2)
            f.write("\n")
        parts = [{k: v for k, v in p.items()} for p in m.PARTS]
        with open(os.path.join(root, "check.json"), "w") as f:
            json.dump({"type": "parts", "deliverable": "brief.md", "corpus": sorted(names), "parts": parts},
                      f, indent=2)
            f.write("\n")
        with open(os.path.join(root, "solution", "brief.md"), "w") as f:
            f.write(m.BRIEF)
        words = sum(len(open(os.path.join(docs_dir, n)).read().split()) for n in names)
        # sanity: every regex matches the reference; none matches pure filler docs
        for p in m.PARTS:
            if p["kind"] == "fact":
                for x in p.get("all", []):
                    assert re.search(x, m.BRIEF, re.I), (tid, p["id"], x)
        fillers = [n for n, _ in m.FILLER]
        for n in fillers:
            t = open(os.path.join(docs_dir, n)).read()
            assert not any(p.search(t) for p in pats), (tid, n)
        print("%s: %d docs, %d words, %d parts" % (tid, len(names), words, len(m.PARTS)))


if __name__ == "__main__":
    main()
