---
title: Give each A2A agent its own network flag
type: adr
status: proposed
decision-date: 2026-09-30
deciders: []
tags: [a2a, networking, security]
synthesis: null
repo: boersmamarcel/chatty2
linear-issue: null
linear-project: null
implemented-in: null
kill-criteria-met: null
supersedes: []
superseded-by: []
---
# ADR-0001: Give each A2A agent its own network flag

## Context

A fixture for the format check, not a real decision. Remote agents share one network policy.

## Decision

We will add a per-agent flag.

## Consequences

### Positive
- One agent can reach a private network without the others.

### Negative / trade-offs
- One more setting per agent.

### Neutral
- The default is unchanged.

## Kill criteria

- **What would have to be true:** no user sets the flag within two releases.
- **How it gets measured:** the settings telemetry count.
- **What happens then:** remove the flag.

## Alternatives considered

- One global flag: rejected, it opens the network to every agent.

## References

- `docs/agents-and-specs.md`
