# Codex step-by-step implementation plan

## Step 1: crate skeleton

Create `katachi-harness-codex` and wire it into the top-level CLI.

**Done when:** `katachi harness codex scan` exists as a stub.

## Step 2: config-layer loader

Implement discovery for:

- user config
- system config
- project `.codex/config.toml` files from root to cwd
- profile blocks

Preserve precedence metadata and trust requirements.

**Done when:** explain output can show active and inactive config layers.

## Step 3: AGENTS instruction-chain loader

Implement Codex instruction discovery:

- `AGENTS.override.md`
- `AGENTS.md`
- configured fallback names where appropriate

Preserve root-to-cwd order.

**Done when:** a fixture repo yields the correct instruction chain.

## Step 4: skills parser

Parse skills:

- `SKILL.md`
- directory structure
- optional `agents/openai.yaml`
- metadata relevant to MCP or appearance

**Done when:** skill items can be explained with source and dependency hints.

## Step 5: custom agent parser

Discover and parse user/project custom agents.

Preserve raw config and inferred session-inheritance behavior.

**Done when:** agent items are linked to their config sources.

## Step 6: hooks and rules discovery

Add support for:

- `hooks.json`
- rules directories / rule files

Preserve the important semantics:
- hooks are additive across layers
- rules constrain execution behavior

**Done when:** explain output can show all matching hook/rule sources.

## Step 7: MCP discovery

Parse MCP server config from active layers.

Preserve:
- transport type
- auth hints
- source layer

**Done when:** MCP items can be selected in rosters and displayed by explain.

## Step 8: plugin discovery

Scan configured plugin marketplace roots and plugin sources.

Build packaging edges from plugin to packaged capabilities where possible.

**Done when:** at least one plugin fixture is visible in the catalog.

## Step 9: roster file parser

Add support for `rosters/codex/*.toml`.

Parse:
- selections
- run profile
- resolution settings

**Done when:** a Codex roster can be loaded and validated against discovered item ids.

## Step 10: effective-config builder

Implement the internal builder that composes:

- config layers
- profile overrides
- instruction docs
- hooks
- rules
- MCP
- agents
- skills

This builder is the heart of the module.

**Done when:** `effective-config` JSON can be emitted without running Codex.

## Step 11: legality validator

Validate:

- project trust applicability
- requirements constraints
- missing dependencies
- unsupported backend projections

**Done when:** an illegal run fails during planning with a good diagnostic.

## Step 12: CLI planner

Build a concrete CLI execution plan around non-interactive Codex execution.

Prefer:
- machine-readable output
- explicit cwd
- explicit approval/sandbox settings
- temp overlays over ambient mutation

**Done when:** `katachi harness codex plan ...` prints a full command/materialization plan.

## Step 13: temp `CODEX_HOME` materializer

Implement filesystem generation for:

- temp home config
- temp rules
- temp hooks
- temp instruction files
- temp project `.codex/` overlays

**Done when:** the materializer produces a reproducible manifest.

## Step 14: CLI executor and transcript adapter

Run Codex and capture:

- raw stdout/stderr
- parsed events
- final response
- usage / structured output when available

**Done when:** a stub or real Codex run produces an `ExecutionRecord`.

## Step 15: SDK projections

Add:

- TypeScript SDK projection
- Python SDK projection behind a feature flag

Keep them secondary to CLI fidelity.

**Done when:** one simple roster can be projected through `sdk-ts` or rejected with a projection diagnostic.

## Step 16: operator commands

Finish:

- `explain`
- `graph`
- `effective-config`
- `doctor`

**Done when:** operators can understand why a Codex plan was assembled the way it was.

## Step 17: test matrix

Add tests for:

- trusted vs untrusted project
- multiple nested `.codex/config.toml`
- layered `AGENTS.md`
- additive hooks
- rules conflicts
- requirements violations
- plugin packaging

**Done when:** the main Codex behaviors are covered by fixture-based tests.

## Final Codex milestone

The Codex milestone is complete when this works:

```bash
katachi harness codex scan
katachi harness codex effective-config accessibility-auditor
katachi harness codex plan accessibility-auditor execute "Audit this repo"
katachi harness codex execute accessibility-auditor "Audit this repo"
```
