# Codex implementation specification

## Purpose

This spec defines the Codex module for the `katachi` prototype.

The module must handle both:

- discrete artifacts (skills, agents, plugins, hooks, rules)
- configuration layers that materially affect how Codex behaves

Initial backend priority:

1. CLI
2. TypeScript SDK
3. Python SDK (feature-gated)

App-server is an intentional future adapter, not required in v1.

## Module boundaries

Crate name:

```text
crates/katachi-harness-codex
```

Responsibilities:

- scan active Codex config and customization surfaces
- build effective-config-aware roster graphs
- validate trust/policy legality
- materialize temp Codex environments
- execute through CLI or SDK adapters

## Module configuration

Suggested config section:

```toml
[harnesses.codex]
enabled = true
binary = "codex"
default_backend = "cli"

codex_home = "~/.codex"
project_roots = ["."]
marketplace_roots = ["~/.agents/plugins", ".agents/plugins"]

respect_project_trust = true
preserve_failed_overlays = true
enable_python_sdk = false
```

Notes:

- `marketplace_roots` should be configurable.
- `respect_project_trust` should default to true.
- `enable_python_sdk` should remain off by default because the upstream Python SDK is still experimental.

## Internal item kinds

```rust
pub enum CodexItemKind {
    ConfigLayer,
    Profile,
    InstructionDoc,
    Skill,
    CustomAgent,
    HookSet,
    McpServer,
    RuleSet,
    Plugin,
    RequirementPolicy,
    RunProfile,
}
```

`InstructionDoc` represents `AGENTS.md` (or fallback equivalents).

`ConfigLayer` represents a specific source of config such as:

- user `~/.codex/config.toml`
- project `.codex/config.toml`
- system config
- generated temp config

## Discovery model

### Inputs to discovery

- user `~/.codex/config.toml`
- system config when present
- project `.codex/config.toml` files from root to cwd
- `AGENTS.md` / `AGENTS.override.md` chain
- skills roots
- custom agents dirs
- `hooks.json` near active config layers
- rules directories
- configured MCP entries
- plugin marketplace catalogs and plugin dirs

### Discovery output

- discovered items
- edge graph
- effective precedence metadata
- trust applicability metadata
- legality diagnostics

## Discovery rules

### 1. Config layers

Discover each config layer separately and preserve:

- source path
- precedence rank
- whether it is currently active
- whether trust is required for activation

The Codex module should not flatten all config into one blob too early. Keep layer provenance intact.

### 2. Profiles

Parse `[profiles.<name>]` blocks from active config layers and emit `Profile` items.

Profiles should be selectable from rosters and runnable plans.

### 3. Instruction docs

Build the Codex instruction chain by scanning:

- global Codex home
- project root through cwd

Preserve order and override semantics.

Emit:

- one `InstructionDoc` item per discovered file
- precedence/order metadata
- edges representing chain order if helpful

### 4. Skills

Discover skill directories and parse:

- `SKILL.md`
- optional `agents/openai.yaml`
- optional scripts/references/assets presence
- scope and source

Emit `Skill` items plus dependency edges to MCP where metadata suggests it.

### 5. Custom agents

Discover custom agents from:

- user agent dirs
- project agent dirs

Parse agent config files and preserve raw config.

Emit edges:

- `AgentUsesConfigLayer`
- `AgentInheritsSessionDefaults`
- `AgentNeedsSkill` where applicable

### 6. Hooks

Discover `hooks.json` files near active config layers.

Preserve additive behavior metadata. In v1, model hooks as `HookSet` items instead of splitting every handler.

### 7. Rules

Discover rule files from team-config rule directories. Preserve:

- source
- precedence
- rule decisions
- whether rule is local/user/admin-enforced

### 8. MCP servers

Discover configured MCP servers from active config layers. Preserve transport information and auth hints in raw metadata.

### 9. Plugins

Discover plugin catalogs and plugin directories from configured roots.

Emit:

- `Plugin` items
- optionally discovered packaged skills or app/MCP mappings
- packaging edges from plugin to contained items where parseable

### 10. Requirement policies

When managed `requirements.toml` or cloud-managed equivalents are visible to the local client, expose them as `RequirementPolicy` items or as global validation inputs.

For v1 it is acceptable to model them as validation state rather than directly selectable items, but the run record should preserve their effect.

## Edge model

Required edge kinds:

```rust
pub enum CodexEdgeKind {
    LayerOverrides,
    ProfileOverrides,
    InstructionChainBefore,
    SkillRequiresMcp,
    AgentUsesConfigLayer,
    AgentInheritsSessionDefaults,
    HookAddsBehavior,
    RuleConstrainsExecution,
    PluginContains,
    RequirementConstrainsRun,
}
```

## Codex roster file format

Suggested file:

```toml
version = 1
id = "accessibility-auditor"
description = "Codex accessibility audit loadout"

[selection]
profiles = ["review"]
instructions = ["global:AGENTS.md", "project:AGENTS.md"]
skills = ["accessibility-audit"]
agents = ["readonly-reviewer"]
hooks = ["reporting-hooks"]
mcp_servers = ["chrome-devtools", "openaiDeveloperDocs"]
rules = ["readonly-shell"]
plugins = []

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
profile = "review"
output_mode = "machine-readable"
output_schema_file = "schemas/a11y-report.json"

[resolution]
include_transitive = true
materialization = "temp-overlay"
respect_project_trust = true
```

## Resolve algorithm

1. collect selected config/profile/instruction items
2. build effective layer order
3. expand semantic closure
   - skill -> MCP
   - agent -> config / inherited session
   - selected rules/hooks/instructions
4. validate trust applicability
5. validate requirements legality
6. validate backend projection
7. produce resolved selection + diagnostics

## Validation rules

### Trust validation

If a selected project-scoped item would be inactive under current trust state, emit a validation error or require materialized mode that explicitly recreates the intended environment.

### Requirements validation

Reject or warn when a requested run profile violates managed requirements, including:

- approval policy
- sandbox mode
- web search mode
- admin-enforced rules

### Hook merge validation

Explain that multiple matching hook files all run. Do not model hook selection as naive replacement.

### Plugin packaging validation

If a selected capability is only available through a plugin, auto-close the plugin or report the missing package.

## CLI projection

The CLI path should be the primary implementation path.

### Planning rules

The Codex CLI plan should:

- target documented non-interactive `codex exec`
- prefer a machine-readable output/event mode
- support structured output schemas when requested
- inject profile/config overrides through temp config or CLI overrides
- set working directory explicitly
- grant additional writable dirs only when the roster requests them
- record approval/sandbox settings explicitly

Important implementation guidance:

- do not hardcode internal undocumented flags when avoidable
- centralize all exact CLI flag names in one compatibility module
- preserve raw stdout/stderr even when parsing machine-readable events

### Materialized Codex environment

The recommended path is:

- create temp `CODEX_HOME`
- generate `config.toml`
- generate `AGENTS.md` chain as needed
- generate `.codex/hooks.json`, `.codex/rules/`, and `.codex/agents/` overlays
- optionally mirror plugin catalogs or symlink plugin sources

Example overlay:

```text
/tmp/katachi-codex-<run-id>/
  home/
    config.toml
    AGENTS.md
    hooks.json
    rules/
  project/
    .codex/
      config.toml
      hooks.json
      agents/
      rules/
    AGENTS.md
```

Execution should point Codex at the materialized home and project dirs rather than mutating the user’s real environment.

## SDK projections

### TypeScript SDK

Support `sdk-ts` as the main non-CLI projection.

Use it for:

- threads
- streaming
- structured output
- resuming
- app-embedded workflows

But keep in mind that many Codex behaviors still come from filesystem/home config, so the projection often still needs a temp `CODEX_HOME` or config overlay.

### Python SDK

Gate `sdk-py` behind explicit enablement.

Use it only when the operator explicitly requests it or a future profile prefers it.

### App-server note

Leave an internal trait or adapter seam so app-server can be added later without redesigning the whole crate.

## Transcript capture

Store:

- raw stdout/stderr
- parsed event stream
- final response
- usage if available
- structured output object when present

The parser should tolerate version drift.

## Commands to expose

```text
katachi harness codex scan
katachi harness codex explain <item-id>
katachi harness codex graph
katachi harness codex plan <roster-id> execute <prompt>
katachi harness codex execute <roster-id> <prompt>
```

Suggested extras:

```text
katachi harness codex effective-config <roster-id>
katachi harness codex doctor
```

`effective-config` is valuable for Codex because so much behavior depends on layered config.

## Error handling

Distinguish:

- `CodexDiscoveryError`
- `CodexResolveError`
- `CodexValidationError`
- `CodexProjectionError`
- `CodexExecutionError`

## Acceptance criteria

The Codex module is sufficient for prototype use when it can:

- scan config layers and preserve precedence
- scan and order `AGENTS.md`
- discover skills, agents, hooks, rules, and MCP
- materialize a temp `CODEX_HOME`
- run a headless Codex task
- capture transcript/final response
- reject an illegal approval/sandbox combination before execution
