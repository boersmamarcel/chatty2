# chatty-optimize

Research crate: GEPA/AFlow optimization drivers, paired statistics, and QA loaders.

## Role in the system

```mermaid
flowchart LR
  Traces[chatty-trace] --> Opt[chatty-optimize]
  Playbook[chatty-playbook] --> Opt
  Flow[chatty-flow] --> Opt
  Opt --> Offline[Offline optimizer runs]
  Offline -.->|never blocks| Chat[User chat path]
```

**Test-time / offline only** — optimizer runs must not block the interactive chat path.

## Boundaries

Reserved symbols in [`RESERVED.md`](../../RESERVED.md).

## Related docs

- [cost-model](../../docs/research/cost-model.md)
