#!/usr/bin/env python3
"""Scripted stand-in for an Ollama server, used to record demo animations.

Speaks just enough of the Ollama HTTP API for Chatty to list a model and
stream a chat completion, so recordings are deterministic and need no API
key, GPU or network. Replies come from a scenario JSON file:

    {
      "model": "llama3.2:latest",
      "delay_ms": 18,
      "turns": [
        {
          "match": "structure",           # substring of the last user message
          "replies": [                    # one entry per model turn
            {"tool_calls": [{"name": "list_directory", "arguments": {"path": "."}}]},
            {"text": "Markdown answer streamed word by word."}
          ]
        },
        {"replies": [{"text": "Fallback answer when nothing matches."}]}
      ]
    }

Within a turn, reply N is used after N tool results have come back since the
last user message, so a turn can chain tool calls before its final text.

Usage: mock_ollama.py --port 11435 --scenario scenario.json
"""

import argparse
import json
import re
import sys
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def log(msg):
    print(f"[mock-ollama] {msg}", file=sys.stderr, flush=True)


class Handler(BaseHTTPRequestHandler):
    scenario = {}
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # quieter than the default access log
        pass

    # -- helpers -----------------------------------------------------------
    def _json(self, payload, status=200):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_json(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return {}

    @property
    def model(self):
        return self.scenario.get("model", "llama3.2:latest")

    # -- routes ------------------------------------------------------------
    def do_GET(self):
        log(f"GET {self.path}")
        if self.path.startswith("/api/tags"):
            # Chatty's Ollama sync replaces sync-owned models with whatever is
            # listed here (dropping the context window the profile seeds), so
            # advertise nothing unless a scenario opts in with "list_model".
            models = [self._model_entry()] if self.scenario.get("list_model") else []
            self._json({"models": models})
        elif self.path.startswith("/api/version"):
            self._json({"version": "0.6.0"})
        elif self.path.startswith("/api/ps"):
            self._json({"models": []})
        else:
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", "17")
            self.end_headers()
            self.wfile.write(b"Ollama is running")

    def do_POST(self):
        body = self._read_json()
        log(f"POST {self.path}")
        if self.path.startswith("/api/show"):
            self._json(
                {
                    "modelfile": "",
                    "parameters": "",
                    "template": "",
                    "details": self._model_entry()["details"],
                    "model_info": {"general.architecture": "llama"},
                    "capabilities": ["completion", "tools"],
                }
            )
        elif self.path.startswith("/api/chat"):
            self._chat(body)
        else:
            self._json({"error": f"unsupported path {self.path}"}, 404)

    def _model_entry(self):
        return {
            "name": self.model,
            "model": self.model,
            "modified_at": now(),
            "size": 2_019_393_189,
            "digest": "a80c4f17acd55265feec403c7aef86be0c25983ab279d83f3bcd3abbcb5b8b72",
            "details": {
                "parent_model": "",
                "format": "gguf",
                "family": "llama",
                "families": ["llama"],
                "parameter_size": "3.2B",
                "quantization_level": "Q4_K_M",
            },
        }

    # -- chat --------------------------------------------------------------
    def _pick_reply(self, messages):
        last_user_ix = max(
            (i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1
        )
        user_text = str(messages[last_user_ix].get("content", "")) if last_user_ix >= 0 else ""
        tool_results = sum(1 for m in messages[last_user_ix + 1 :] if m.get("role") == "tool")

        chosen = None
        for turn in self.scenario.get("turns", []):
            needle = turn.get("match")
            if needle is None or needle.lower() in user_text.lower():
                chosen = turn
                break
        if chosen is None:
            return {"text": "I have no scripted answer for that."}
        replies = chosen.get("replies", [])
        if not replies:
            return {"text": ""}
        return replies[min(tool_results, len(replies) - 1)]

    @staticmethod
    def _is_title_request(messages):
        # chatty-core's title_generator asks for "a concise, descriptive title".
        text = str(messages[-1].get("content", "")) if messages else ""
        return "concise, descriptive title" in text

    def _usage(self, messages, out_tokens):
        prompt_tokens = sum(len(str(m.get("content", "")).split()) for m in messages) * 4 // 3
        return {
            "done_reason": "stop",
            "total_duration": 1_500_000_000,
            "load_duration": 20_000_000,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 300_000_000,
            "eval_count": out_tokens,
            "eval_duration": 1_000_000_000,
        }

    def _complete(self, reply, messages):
        """Non-streaming /api/chat: one JSON object (used by title generation)."""
        text = reply.get("text", "")
        payload = {
            "model": self.model,
            "created_at": now(),
            "message": {"role": "assistant", "content": text},
            "done": True,
        }
        payload.update(self._usage(messages, len(text.split())))
        self._json(payload)
        log("title complete" if self._is_title_request(messages) else "turn complete")

    def _chunk(self, message, done=False, extra=None):
        payload = {"model": self.model, "created_at": now(), "message": message, "done": done}
        if extra:
            payload.update(extra)
        self.wfile.write((json.dumps(payload) + "\n").encode())
        self.wfile.flush()

    def _chat(self, body):
        messages = body.get("messages", [])
        roles = ",".join(m.get("role", "?")[0] for m in messages)
        last = str(messages[-1].get("content", ""))[:60].replace("\n", " ") if messages else ""
        log(f"chat stream={body.get('stream', True)} roles={roles} last={last!r}")
        if self._is_title_request(messages):
            reply = {"text": self.scenario.get("title", "Quick chat")}
        else:
            reply = self._pick_reply(messages)
        if body.get("stream", True) is False:
            self._complete(reply, messages)
            return
        delay = float(reply.get("delay_ms", self.scenario.get("delay_ms", 18))) / 1000.0

        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        # Hand-rolled chunked encoding so each NDJSON line is flushed at once.
        real_write = self.wfile.write

        def chunked_write(data):
            real_write(b"%x\r\n" % len(data) + data + b"\r\n")

        self.wfile.write = chunked_write

        out_tokens = 0
        if "tool_calls" in reply:
            calls = [
                {"function": {"name": c["name"], "arguments": c.get("arguments", {})}}
                for c in reply["tool_calls"]
            ]
            time.sleep(reply.get("think_ms", 600) / 1000.0)
            self._chunk({"role": "assistant", "content": "", "tool_calls": calls})
            out_tokens = 24 * len(calls)
        else:
            text = reply.get("text", "")
            time.sleep(reply.get("think_ms", 400) / 1000.0)
            # Stream in word-sized pieces, keeping whitespace attached so the
            # markdown renderer sees the same bytes a real model would emit.
            for piece in re.findall(r"\S+\s*|\s+", text):
                self._chunk({"role": "assistant", "content": piece})
                out_tokens += 1
                time.sleep(delay)

        self._chunk(
            {"role": "assistant", "content": ""},
            done=True,
            extra=self._usage(messages, out_tokens),
        )
        real_write(b"0\r\n\r\n")
        self.wfile.write = real_write
        if "tool_calls" not in reply:
            # record.sh's wait_reply polls for this line.
            log("turn complete")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=11435)
    ap.add_argument("--scenario", required=True, help="scenario JSON file")
    args = ap.parse_args()
    with open(args.scenario) as fh:
        Handler.scenario = json.load(fh)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    log(f"serving {args.scenario} on http://127.0.0.1:{args.port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
