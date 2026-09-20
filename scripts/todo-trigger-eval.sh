#!/usr/bin/env bash
# Todo-tool trigger-rate eval (AGE-479): run `chatty-tui --headless` over a
# fixed prompt set in a scratch Rust crate against a local OpenAI-compatible
# server, and report how often `write_todos` was called for single-step
# prompts (should be rare) versus multi-step prompts (should be common).
#
#   scripts/todo-trigger-eval.sh [label]
#
#   CHATTY_TUI=<path>        binary to run (default: build target/release/chatty-tui)
#   EVAL_URL=<url>           OpenAI-compatible base URL (default http://127.0.0.1:8000/v1)
#   EVAL_MODEL=<id>          model id as listed by GET /v1/models (default RedHatAI/Qwen3.8-27B-INT4)
#   EVAL_API_KEY=<key>       bearer key, written to the throwaway providers.json (default none)
#   EVAL_RUNS=<n>            runs per prompt (default 3; sampling is not deterministic)
#   EVAL_TIMEOUT=<seconds>   wall-clock cap per run (default 600)
#   EVAL_MAX_TURNS=<n>       agent turn budget per run (default 12; a plan is written first or not at all)
#   EVAL_ONLY=<regex>        run only prompts whose id matches (e.g. '^M' for the multi-step set)
#
# Output: target/todo-trigger-eval/<label>-<timestamp>/ with one stderr trace
# per run, results.tsv, and summary.txt (also printed). Targets from the
# issue: single-step trigger rate <= 10%, multi-step >= 66%.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LABEL="${1:-eval}"
EVAL_URL="${EVAL_URL:-http://127.0.0.1:8000/v1}"
EVAL_MODEL="${EVAL_MODEL:-RedHatAI/Qwen3.8-27B-INT4}"
EVAL_API_KEY="${EVAL_API_KEY:-no-key-required}"
EVAL_RUNS="${EVAL_RUNS:-3}"
EVAL_TIMEOUT="${EVAL_TIMEOUT:-600}"
EVAL_MAX_TURNS="${EVAL_MAX_TURNS:-12}"
EVAL_ONLY="${EVAL_ONLY:-.}"

# 1. The binary, as in scripts/team-smoke.sh.
if [ -n "${CHATTY_TUI:-}" ]; then
  BIN="$(cd "$(dirname "$CHATTY_TUI")" && pwd)/$(basename "$CHATTY_TUI")"
  [ -x "$BIN" ] || { echo "CHATTY_TUI=$CHATTY_TUI is not an executable" >&2; exit 2; }
else
  BIN="$ROOT/target/release/chatty-tui"
  if [ ! -x "$BIN" ]; then
    echo "building target/release/chatty-tui (this takes a while)"
    (cd "$ROOT" && cargo build --release -p chatty-tui)
  fi
fi

# 2. The prompt set: id, class (S = single-step, M = multi-step), text.
PROMPT_IDS=()
PROMPT_TEXT=()
prompt() { PROMPT_IDS+=("$1"); PROMPT_TEXT+=("$2"); }
prompt S1 "What's in config.toml?"
prompt S2 "Rename src/foo.rs to src/bar.rs."
prompt S3 "What does fn parse in src/main.rs do?"
prompt S4 "Add a doc comment to fn parse in src/main.rs."
prompt S5 "List the files in src/."
prompt S6 "Explain this error:

error[E0502]: cannot borrow \`items\` as mutable because it is also borrowed as immutable
  --> src/main.rs:12:5
   |
11 |     let first = &items[0];
   |                 -------- immutable borrow occurs here
12 |     items.push(4);
   |     ^^^^^^^^^^^^^ mutable borrow occurs here
13 |     println!(\"{first}\");
   |               ------- immutable borrow later used here"
prompt S7 "What port does config.toml set?"
prompt S8 "How many lines does src/main.rs have?"
prompt S9 "In one paragraph: what is the difference between String and &str in Rust?"
prompt M1 "Refactor the auth module: move token validation out of src/auth/login.rs into a new src/auth/token.rs, re-export it from src/auth/mod.rs, update the caller in src/auth/session.rs, and update tests/auth.rs so \`cargo test\` still passes."
prompt M2 "Add a --verbose CLI flag: parse it in src/main.rs, store it on Config in src/config.rs, make Logger in src/log.rs honour it, and add a unit test for each of the three layers."
prompt M3 "Add a \`retries\` setting: read it from config.toml in src/config.rs with a default of 3, use it in the fetch loop in src/client.rs, document it in README.md, and add tests for the parsing and the retry loop."

# 3. A throwaway HOME: the OpenAI-compatible provider (the key goes into the
#    file, never onto a command line), the model with thinking off, and the
#    execution settings the default main agent runs with.
RUN_DIR="$ROOT/target/todo-trigger-eval/$LABEL-$(date +%Y%m%d-%H%M%S)"
HOME_DIR="$RUN_DIR/home"
CFG="$HOME_DIR/.config/chatty"
mkdir -p "$CFG" "$HOME_DIR/.local/share/chatty" "$RUN_DIR/traces"
EVAL_URL="$EVAL_URL" EVAL_MODEL="$EVAL_MODEL" EVAL_API_KEY="$EVAL_API_KEY" EVAL_MAX_TURNS="$EVAL_MAX_TURNS" \
python3 - "$CFG" <<'PY'
import json, os, sys
cfg = sys.argv[1]
json.dump([{"provider_type": "open_router", "name": "OpenAI-compat (eval)",
            "base_url": os.environ["EVAL_URL"], "api_key": os.environ["EVAL_API_KEY"]}],
          open(os.path.join(cfg, "providers.json"), "w"), indent=2)
json.dump([{"id": "eval", "name": os.environ["EVAL_MODEL"], "provider_type": "open_router",
            "model_identifier": os.environ["EVAL_MODEL"], "temperature": 0.3,
            "extra_params": {"think": "false"}}],
          open(os.path.join(cfg, "models.json"), "w"), indent=2)
json.dump({"enabled": True, "approval_mode": "AutoApproveAll", "workspace_dir": None,
           "filesystem_read_enabled": True, "filesystem_write_enabled": True,
           "fetch_enabled": False, "git_enabled": True, "browser_enabled": False,
           "execute_code_enabled": False, "docker_code_execution_enabled": False,
           "docker_host": None, "timeout_seconds": 60, "max_output_bytes": 1048576,
           "network_isolation": False, "max_agent_turns": int(os.environ["EVAL_MAX_TURNS"]),
           "memory_enabled": False, "embedding_enabled": False,
           "hosted_conversations_enabled": False},
          open(os.path.join(cfg, "execution_settings.json"), "w"), indent=2)
PY
printf '[user]\n\temail = todo-eval@chatty.invalid\n\tname = todo-eval\n' > "$HOME_DIR/.gitconfig"

# 4. The fixture crate, rebuilt fresh for every run so one run's edits never
#    leak into the next.
FIXTURE="$RUN_DIR/fixture"
mkdir -p "$FIXTURE/src/auth" "$FIXTURE/tests"
cat > "$FIXTURE/Cargo.toml" <<'EOF'
[package]
name = "todo-eval-fixture"
version = "0.1.0"
edition = "2021"

# Not part of the chatty workspace the run dir sits under.
[workspace]

[[bin]]
name = "fixture"
path = "src/main.rs"

[lib]
name = "fixture"
path = "src/lib.rs"
EOF
cat > "$FIXTURE/config.toml" <<'EOF'
[server]
host = "127.0.0.1"
port = 8080

[client]
timeout_ms = 2500
EOF
cat > "$FIXTURE/README.md" <<'EOF'
# fixture

A small service used as a scratch repository.

## Settings

`config.toml` carries the server address and the client timeout.
EOF
cat > "$FIXTURE/src/lib.rs" <<'EOF'
pub mod auth;
pub mod client;
pub mod config;
pub mod foo;
pub mod log;
EOF
cat > "$FIXTURE/src/main.rs" <<'EOF'
use fixture::config::Config;
use fixture::log::Logger;

fn parse(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--config" {
            return iter.next().cloned();
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = parse(&args).unwrap_or_else(|| "config.toml".to_string());
    let config = Config::load(&path).expect("config loads");
    let logger = Logger::new();
    logger.info(&format!("listening on {}:{}", config.host, config.port));
}
EOF
cat > "$FIXTURE/src/config.rs" <<'EOF'
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = Config { host: "127.0.0.1".into(), port: 8080, timeout_ms: 1000 };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "host" => config.host = value.to_string(),
                "port" => config.port = value.parse().map_err(|_| "bad port")?,
                "timeout_ms" => config.timeout_ms = value.parse().map_err(|_| "bad timeout")?,
                _ => {}
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port() {
        let config = Config::parse("port = 9000").unwrap();
        assert_eq!(config.port, 9000);
    }
}
EOF
cat > "$FIXTURE/src/log.rs" <<'EOF'
pub struct Logger;

impl Logger {
    pub fn new() -> Self {
        Logger
    }

    pub fn info(&self, message: &str) {
        println!("[info] {message}");
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}
EOF
cat > "$FIXTURE/src/client.rs" <<'EOF'
pub fn fetch(url: &str, attempt: impl Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let mut last_error = String::new();
    for _ in 0..3 {
        match attempt(url) {
            Ok(body) => return Ok(body),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}
EOF
cat > "$FIXTURE/src/foo.rs" <<'EOF'
pub fn foo() -> &'static str {
    "foo"
}
EOF
cat > "$FIXTURE/src/auth/mod.rs" <<'EOF'
pub mod login;
pub mod session;
EOF
cat > "$FIXTURE/src/auth/login.rs" <<'EOF'
pub fn validate_token(token: &str) -> bool {
    token.len() >= 8 && token.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn login(user: &str, token: &str) -> Result<String, String> {
    if validate_token(token) {
        Ok(format!("session-{user}"))
    } else {
        Err("invalid token".to_string())
    }
}
EOF
cat > "$FIXTURE/src/auth/session.rs" <<'EOF'
use super::login::validate_token;

pub struct Session {
    pub user: String,
}

impl Session {
    pub fn open(user: &str, token: &str) -> Option<Session> {
        validate_token(token).then(|| Session { user: user.to_string() })
    }
}
EOF
cat > "$FIXTURE/tests/auth.rs" <<'EOF'
use fixture::auth::login::{login, validate_token};
use fixture::auth::session::Session;

#[test]
fn short_tokens_are_rejected() {
    assert!(!validate_token("abc"));
}

#[test]
fn login_returns_a_session_id() {
    assert_eq!(login("ann", "abcdefgh").unwrap(), "session-ann");
}

#[test]
fn session_opens_with_a_valid_token() {
    assert!(Session::open("ann", "abcdefgh").is_some());
}
EOF
git -C "$FIXTURE" -c init.defaultBranch=main init -q
git -C "$FIXTURE" -c user.name=todo-eval -c user.email=todo-eval@chatty.invalid add -A
git -C "$FIXTURE" -c user.name=todo-eval -c user.email=todo-eval@chatty.invalid commit -q -m init

# 5. The runs. Each one gets a fresh copy of the fixture and a stderr trace;
#    `[tool: <name>] ... running` lines are the tool calls.
RESULTS="$RUN_DIR/results.tsv"
printf 'prompt\trun\twrite_todos\ttool_calls\twall_s\texit\n' > "$RESULTS"
echo "run dir: $RUN_DIR"
echo "binary: $BIN"
echo "endpoint: $EVAL_URL  model: $EVAL_MODEL  runs: $EVAL_RUNS  max turns: $EVAL_MAX_TURNS"
for i in "${!PROMPT_IDS[@]}"; do
  id="${PROMPT_IDS[$i]}"
  [[ "$id" =~ $EVAL_ONLY ]] || continue
  for run in $(seq 1 "$EVAL_RUNS"); do
    work="$RUN_DIR/work/$id-$run"
    mkdir -p "$(dirname "$work")"
    cp -r "$FIXTURE" "$work"
    trace="$RUN_DIR/traces/$id-$run.err"
    start=$(date +%s)
    set +e
    HOME="$HOME_DIR" XDG_CONFIG_HOME="$HOME_DIR/.config" XDG_DATA_HOME="$HOME_DIR/.local/share" \
      timeout --signal=INT --kill-after=20 "$EVAL_TIMEOUT" \
      "$BIN" --headless --model eval --auto-approve --workspace "$work" \
      --max-agent-turns "$EVAL_MAX_TURNS" -m "${PROMPT_TEXT[$i]}" \
      > "$RUN_DIR/traces/$id-$run.out" 2> "$trace"
    code=$?
    set -e
    wall=$(( $(date +%s) - start ))
    if [ "$code" -ne 0 ] && ! grep -q '^  \[tool: ' "$trace" && ! grep -q '^Turn\|^Agent\|completed' "$trace"; then
      echo "run $id-$run produced no turn (exit $code); see $trace" >&2
      tail -5 "$trace" >&2
      exit 1
    fi
    todos=$(grep -c '^  \[tool: write_todos\] .*running' "$trace" || true)
    calls=$(grep -c '^  \[tool: [a-z_]*\] .*running' "$trace" || true)
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$id" "$run" "$todos" "$calls" "$wall" "$code" >> "$RESULTS"
    printf '%-3s run %s: write_todos=%s tool_calls=%s wall=%ss exit=%s\n' "$id" "$run" "$todos" "$calls" "$wall" "$code"
  done
done

# 6. The summary: per prompt, then the two rates against the targets.
awk -F'\t' 'NR > 1 {
  runs[$1]++; triggered[$1] += ($3 > 0); order[NR] = $1
  cls = substr($1, 1, 1); n[cls]++; hit[cls] += ($3 > 0)
}
END {
  print "== todo trigger rate (write_todos called at least once per run)"
  seen = ""
  for (i = 2; i <= NR; i++) { p = order[i]; if (index(seen, "|" p "|")) continue; seen = seen "|" p "|"
    printf "  %-3s %d/%d\n", p, triggered[p] + 0, runs[p] }
  s = (n["S"] ? 100 * hit["S"] / n["S"] : 0); m = (n["M"] ? 100 * hit["M"] / n["M"] : 0)
  printf "single-step: %d/%d = %.0f%% (target <= 10%%)\n", hit["S"] + 0, n["S"] + 0, s
  printf "multi-step:  %d/%d = %.0f%% (target >= 66%%)\n", hit["M"] + 0, n["M"] + 0, m
}' "$RESULTS" | tee "$RUN_DIR/summary.txt"
