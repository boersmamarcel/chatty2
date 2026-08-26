# Semver policy — shipping research crates (AGE-26)

Applies to public types in `chatty-trace`, `chatty-playbook`, and `chatty-flow`
(and to new public API added to `chatty-core` while working nearby).

## Surface that is semver-sensitive

Once stabilized (after AGE-22 → AGE-5 extraction), treat these as API:

| Crate | Types |
| -- | -- |
| `chatty-trace` | `Trajectory`, `Step`, `Action`, `Outcome`, `FeedbackFn`, `Recorder` |
| `chatty-playbook` | `Playbook`, `Bullet`, `DeltaOp`, `apply`, `grow_and_refine` |
| `chatty-flow` | `WorkflowRepr`, `WfNode`, `IrRepr` |

Until those land, crates are `publish = false` stubs — **no stability promise**.

## Rules

1. **Breaking change → major** (field rename/remove, trait method change, enum variant remove/reorder when exhaustive matches matter).
2. **Additive → minor** (new optional field with `#[serde(default)]`, new non-default trait method with default body, new module).
3. **Fix / docs / private → patch**.
4. Prefer **wrapping** upstream (`rig-*`) types behind local traits so a rig bump is an internal change, not a chatty semver bump.
5. Workspace version is shared today (`version.workspace = true`); when a crate needs an independent bump, split its version and document why.

## What is out of policy

- `chatty-optimize` — build-time / CI research tooling; held to "correct", not semver.
- Harbor Stage B adapters (`harbor-chatty`) — separate repo.
