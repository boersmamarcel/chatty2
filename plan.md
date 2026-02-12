# Plan: Adding Code Execution Capabilities to Chatty Agents

## Overview

Add the ability for LLM agents in Chatty to execute terminal commands and run code in a sandboxed environment, with a user approval flow for safety.

---

## Architecture Decision: Native rig-core Tools vs. In-Process MCP Server

There are two viable paths for exposing code execution to the agent:

### Option A: Native rig-core `Tool` trait (Recommended)

- Implement the `rig::tool::Tool` trait for a `ShellExecuteTool` struct.
- Register it on the `AgentBuilder` via `.tool(shell_tool)` alongside existing `.rmcp_tools()`.
- Simpler, fewer moving parts, no MCP protocol overhead.
- rig-core handles schema generation, argument parsing, and result serialization.

### Option B: In-Process MCP Server

- Implement an MCP server using rmcp's `#[tool_router]` / `#[tool]` macros.
- Connect in-process via `tokio::io::duplex()` or the `rmcp-in-process-transport` crate.
- Pass the resulting `(Vec<Tool>, ServerSink)` to the existing `.rmcp_tools()` path.
- More complex but keeps all tools in a uniform MCP interface.

**Recommendation**: Option A. It's simpler, avoids an extra dependency, and rig-core's `Tool` trait is well-suited for this. MCP is better for external/third-party tool servers. A native tool co-exists cleanly with existing MCP tools on the same agent.

---

## Implementation Plan

### Phase 1: Core Tool Definition

**File: `src/chatty/tools/shell_tool.rs` (new)**

Define a `ShellExecuteTool` that implements `rig::tool::Tool`:

- **Name**: `execute_shell_command`
- **Args** (derive `JsonSchema` + `Deserialize`):
  - `command: String` — the shell command to execute
  - `working_directory: Option<String>` — optional cwd (defaults to a project/workspace directory)
  - `timeout_seconds: Option<u64>` — optional timeout (default 30s, max 300s)
- **Output**: JSON with `{ stdout, stderr, exit_code, timed_out }`
- **Execution**: spawns a `tokio::process::Command` with:
  - Configurable timeout via `tokio::time::timeout`
  - Captured stdout/stderr (piped)
  - Truncation of output to a reasonable limit (e.g., 50KB) to avoid blowing up context

**File: `src/chatty/tools/mod.rs` (new)**

Module declaration for the tools subsystem.

### Phase 2: Sandboxing Layer

**File: `src/chatty/tools/sandbox.rs` (new)**

A `SandboxConfig` struct and a `sandboxed_command()` function that wraps the user's command in platform-specific sandboxing:

#### Linux: Bubblewrap (`bwrap`)

- Check if `bwrap` is available on PATH at startup; log a warning if not.
- Wrap commands as:
  ```
  bwrap \
    --ro-bind /usr /usr \
    --ro-bind /lib /lib \
    --ro-bind /lib64 /lib64 \
    --ro-bind /bin /bin \
    --ro-bind /sbin /sbin \
    --ro-bind /etc/alternatives /etc/alternatives \
    --ro-bind /etc/ld.so.cache /etc/ld.so.cache \
    --proc /proc \
    --dev /dev \
    --tmpfs /tmp \
    --bind <workspace> <workspace> \    # Read-write access to workspace only
    --unshare-net \                     # No network access
    --unshare-pid \                     # Isolated PID namespace
    --die-with-parent \                 # Kill sandbox if Chatty exits
    -- <command>
  ```
- Deny access to `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config` by default.
- The workspace directory (project root) gets read-write bind mount.

#### macOS: Seatbelt (`sandbox-exec`)

- Always available, no dependency check needed.
- Generate a dynamic Seatbelt profile string:
  ```scheme
  (version 1)
  (deny default)
  (allow process-exec)
  (allow process-fork)
  (allow file-read* (subpath "/usr") (subpath "/bin") (subpath "/Library") ...)
  (allow file-read* file-write* (subpath "<workspace>"))
  (allow file-read* file-write* (subpath "/tmp"))
  (deny network*)
  (deny file-read* (subpath "/Users/<user>/.ssh"))
  ```
- Execute as: `sandbox-exec -p <profile> /bin/bash -c <command>`

#### Fallback (no sandbox available)

- If neither bwrap nor sandbox-exec is available, commands still execute but require explicit user approval every time (no auto-approve mode).
- Log a prominent warning at startup.

#### Configuration

```rust
pub struct SandboxConfig {
    pub workspace_dir: PathBuf,          // Read-write directory
    pub additional_read_paths: Vec<PathBuf>,  // Extra read-only paths
    pub network_access: bool,            // Default: false
    pub timeout: Duration,               // Default: 30s
}
```

### Phase 3: User Approval Flow

This is critical for safety. The LLM should not execute arbitrary commands without user consent.

**File: `src/chatty/tools/approval.rs` (new)**

#### Approval Model

```rust
pub enum ApprovalMode {
    AlwaysAsk,          // Show every command for approval (default)
    AutoApproveSandboxed, // Auto-approve if sandbox is available; ask otherwise
    AutoApproveAll,     // Full autonomy (power users, explicit opt-in)
}

pub enum ApprovalDecision {
    Approve,
    ApproveAndRemember,  // Don't ask again for similar commands this session
    Deny,
    DenyAndStop,         // Deny and stop the agent's current turn
}
```

#### Approval UI

Since rig-core's `Tool::call()` is async, the approval flow works as follows:

1. **Tool receives command** → Before executing, it sends an approval request to the UI thread via a channel.
2. **UI shows approval dialog** → A modal or inline prompt in the chat view showing:
   - The exact command to be executed
   - The working directory
   - Whether it will be sandboxed
   - Approve / Deny buttons
3. **User responds** → Response sent back via a oneshot channel.
4. **Tool proceeds or returns error** → If denied, returns a structured error that the LLM sees as "Command denied by user".

**Channel mechanism**:
- Store a `tokio::sync::mpsc::Sender<ApprovalRequest>` in the tool.
- The `ApprovalRequest` contains the command details and a `tokio::sync::oneshot::Sender<ApprovalDecision>` for the response.
- The UI side (in `app_controller.rs` or a new controller) holds the `Receiver` and listens for approval requests.
- When a request arrives, it updates the chat view to show the approval prompt and waits for user input.

**Integration with GPUI**: The receiver is polled in a `cx.spawn()` loop or via `cx.on_app_event()`. When an approval request arrives, it triggers a UI update showing the command approval widget in the active chat.

### Phase 4: Agent Integration

**File: `src/chatty/factories/agent_factory.rs` (modify)**

- Accept a new parameter: whether code execution is enabled for this agent.
- When enabled, instantiate `ShellExecuteTool` with the appropriate `SandboxConfig` and approval channel sender.
- Register it via `.tool(shell_tool)` on the `AgentBuilder`.
- This works alongside existing MCP tools — rig-core supports both native tools and MCP tools on the same agent.

**File: `src/chatty/controllers/app_controller.rs` (modify)**

- When creating a conversation, set up the approval channel pair.
- Spawn an async task that listens on the approval receiver and bridges to the UI.
- Wire up the approval UI widget in the chat view.

### Phase 5: UI for Command Execution Display

**File: `src/chatty/views/chat_view.rs` (modify)**
**File: `src/chatty/views/message_types.rs` (modify)**

The existing `ToolCallBlock` and `SystemTraceView` infrastructure already handles tool call display. Extend it:

1. **Approval prompt widget**: When a tool call is `execute_shell_command` and pending approval, render an inline widget with:
   - Code block showing the command
   - Working directory label
   - Sandbox status indicator (sandboxed/unsandboxed)
   - Approve / Deny buttons

2. **Execution result display**: After execution, show:
   - Exit code (with color: green for 0, red for non-zero)
   - Collapsible stdout/stderr sections
   - Duration
   - Truncation indicator if output was cut

3. **Running state**: While a command is executing, show:
   - A spinner/animation
   - The command being run
   - A "Cancel" button that kills the child process

The existing `ToolCallState` enum (`Running`, `Success`, `Error`) maps well. Add a new state:

```rust
pub enum ToolCallState {
    PendingApproval,  // NEW — waiting for user to approve
    Running,
    Success,
    Error(String),
    Cancelled,        // NEW — user cancelled mid-execution
}
```

### Phase 6: Settings & Configuration

**File: `src/settings/models/` (modify existing settings)**

Add code execution settings to the general settings model:

- `code_execution_enabled: bool` (default: false — opt-in)
- `approval_mode: ApprovalMode` (default: AlwaysAsk)
- `default_workspace_dir: Option<PathBuf>`
- `sandbox_network_access: bool` (default: false)
- `command_timeout_seconds: u64` (default: 30)
- `max_output_bytes: usize` (default: 50_000)

**Settings UI**: Add a "Code Execution" section in the settings view with toggles for the above.

### Phase 7: System Prompt Enhancement

When code execution is enabled, append tool usage instructions to the agent's system prompt/preamble:

```
You have access to a shell command execution tool. Use it to:
- Run code, tests, and build commands
- Inspect files and directories
- Execute terminal operations the user requests

Guidelines:
- Prefer simple, focused commands over complex pipelines
- Check command output before proceeding
- If a command fails, explain why and suggest fixes
- Never run destructive commands (rm -rf /, etc.) without explicit user instruction
- Commands execute in a sandboxed environment with limited filesystem and no network access
```

This goes into the preamble in `agent_factory.rs` when the tool is registered.

---

## Dependency Changes

**Cargo.toml additions**:
- `schemars = "0.8"` — Already likely a transitive dep of rig-core, but needed directly for `#[derive(JsonSchema)]` on tool args.
- `tokio` — Already present; ensure `process` feature is enabled for `tokio::process::Command`.
- No new major dependencies required.

**Optional**:
- `which = "7"` — For detecting `bwrap` availability on Linux at startup.

---

## File Change Summary

| File | Action | Purpose |
|------|--------|---------|
| `src/chatty/tools/mod.rs` | New | Module declaration |
| `src/chatty/tools/shell_tool.rs` | New | `ShellExecuteTool` implementing `rig::tool::Tool` |
| `src/chatty/tools/sandbox.rs` | New | Platform-specific sandboxing (bwrap/seatbelt) |
| `src/chatty/tools/approval.rs` | New | Approval model, channel types, approval logic |
| `src/chatty/factories/agent_factory.rs` | Modify | Register native tool on agent builder |
| `src/chatty/controllers/app_controller.rs` | Modify | Approval channel setup, UI bridge |
| `src/chatty/views/chat_view.rs` | Modify | Approval prompt widget, execution result display |
| `src/chatty/views/message_types.rs` | Modify | New `ToolCallState` variants |
| `src/settings/models/general_settings.rs` | Modify | Code execution settings fields |
| `src/settings/views/` | Modify | Settings UI for code execution config |
| `Cargo.toml` | Modify | Add `schemars`, enable tokio `process` feature |

---

## Security Considerations

1. **Defense in depth**: Sandbox enforcement at the OS level (not just application-level checks). Even if the LLM crafts a malicious command, the sandbox restricts what it can access.

2. **Sensitive path exclusion**: Always deny access to `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/chatty` (app settings), credential files, etc.

3. **No network by default**: Sandbox blocks all network access unless explicitly enabled by the user in settings.

4. **Output truncation**: Prevent the LLM from running commands that produce massive output (e.g., `cat /dev/urandom`) by truncating at a configurable byte limit.

5. **Timeout enforcement**: Kill commands that exceed the timeout to prevent hanging.

6. **Process cleanup**: Track child PIDs and ensure they're killed on conversation end, app quit, or user cancellation. Use the existing PID tracking pattern from `mcp_service.rs`.

7. **Command logging**: Log all executed commands and their outcomes for auditability.

8. **Gradual trust**: Default to `AlwaysAsk` approval mode. Users must explicitly opt into auto-approve modes.

---

## Recommended Implementation Order

1. **Phase 1** (Core tool) → Get basic command execution working without sandbox
2. **Phase 3** (Approval flow) → Add safety gate before any command runs
3. **Phase 5** (UI) → Make approval and results visible in the chat
4. **Phase 2** (Sandbox) → Add OS-level sandboxing
5. **Phase 4** (Agent integration) → Wire everything into the agent factory
6. **Phase 6** (Settings) → Make it configurable
7. **Phase 7** (System prompt) → Optimize LLM behavior with tool instructions

Phases 1+3 together form the minimum viable feature. Phase 2 (sandboxing) is critical before any "auto-approve" mode is offered.
