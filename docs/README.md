# katachi prototype documentation suite

This folder contains a planning-grade markdown suite for building an initial `katachi` prototype.

`katachi` is a Rust CLI that provides an indirection layer between a **named loadout** (`katachi`) and a **harness-native execution plan** (Claude Code, Codex, Gemini). The design goal is not to flatten these harnesses into one abstract model. The goal is to give coding agents a stable place to discover, resolve, validate, materialize, and execute **harness-native** configurations in a uniform way.

## Recommended reading order

1. [01-katachi-conceptual-overview.md](./01-katachi-conceptual-overview.md)
2. [02-katachi-implementation-overview.md](./02-katachi-implementation-overview.md)
3. [03-katachi-high-level-implementation-plan.md](./03-katachi-high-level-implementation-plan.md)
4. Harness-specific docs:
   - Claude: [conceptual overview](./claude/01-conceptual-overview.md), [implementation spec](./claude/02-implementation-specification.md), [step-by-step plan](./claude/03-step-by-step-plan.md)
   - Codex: [conceptual overview](./codex/01-conceptual-overview.md), [implementation spec](./codex/02-implementation-specification.md), [step-by-step plan](./codex/03-step-by-step-plan.md)
   - Gemini: [conceptual overview](./gemini/01-conceptual-overview.md), [implementation spec](./gemini/02-implementation-specification.md), [step-by-step plan](./gemini/03-step-by-step-plan.md)

## What this suite is for

This suite is written for coding agents or humans who need enough detail to build a working prototype without repeatedly re-deciding the architecture.

It deliberately emphasizes:

- a **resolver** instead of a lowest-common-denominator abstraction
- **rosters** as harness-native inventories with dependency edges
- **materialized execution** for reproducibility
- **CLI-first backends** for the prototype, with SDK support added where it materially improves capability or fidelity
- a **plan -> execute -> record** lifecycle

## What this suite is not

- It is not a full product spec for plugin marketplaces, remote registries, or enterprise policy management.
- It is not a guarantee that every upstream feature is stable. Several surfaces, especially Gemini hooks/subagents and some Codex/Gemini SDK areas, are still moving.
- It is not a proposal to erase harness-specific semantics.

## Document map

| File | Purpose |
|---|---|
| `01-katachi-conceptual-overview.md` | Core vocabulary, goals, lifecycle, and system-level design constraints |
| `02-katachi-implementation-overview.md` | Rust workspace, clap command tree, shared data model, planner/executor flow |
| `03-katachi-high-level-implementation-plan.md` | Shared implementation order and milestones |
| `claude/*` | Claude Code roster model, CLI/SDK differences, resolver and executor design |
| `codex/*` | Codex Team Config / AGENTS.md / skills / agents / hooks / rules / plugin design |
| `gemini/*` | Gemini settings / extensions / hooks / skills / subagents / MCP / policy design |

## Last verification window

The harness-specific assumptions in this suite were prepared against upstream documentation current on **2026-04-18**. Because all three harnesses evolve quickly, implementation should keep a small compatibility layer around command flags, structured output parsing, and discovery paths.
