# Claude Code implementation specification

## Purpose

This spec defines the Claude module for the `katachi` prototype.

The module must support:

- discovery of Claude-native artifacts
- graph construction with packaging and semantic edges
- roster selection and closure
- validation
- projection to CLI and SDK backends
- transcript capture

The initial priority order is:

1. CLI backend
2. TypeScript SDK backend
3. Python SDK backend

## Module boundaries

Crate name:

```text
crates/katachi-harness-claude
```

Public entry points:

- `scan`
- `explain`
- `resolve`
- `plan`
- `execute`

## Module configuration

Suggested config section:

```toml
[harnesses.claude]
enabled = true
binary = "claude"
default_backend = "cli"

# Do not hardcode undocumented paths if these are empty.
plugin_roots = ["~/.claude/plugins"]
user_root = "~/.claude"
project_roots = ["."]

default_setting_sources = ["user", "project"]
prefer_materialized_cli = true
preserve_failed_overlays = true
```

Notes:

- `plugin_roots` should be configurable.
- `user_root` and project roots should be discoverable from cwd and config.
- The module should not require marketplace support in v1.

## Internal item kinds

The Claude module should define a harness-specific enum.

```rust
pub enum ClaudeItemKind {
    Plugin,
    Skill,
    Agent,
    HookSet,
    McpServer,
    InstructionSource,
    OutputStyle,
    RunProfile,
}
```

`InstructionSource` covers:

- `CLAUDE.md`
- `.claude/rules/*.md`
- optionally other prompt overlays explicitly managed by `katachi`

## Discovery model

### Inputs to discovery

Discovery should read from:

- user `.claude/` tree
- project `.claude/` tree
- explicitly configured plugin roots
- `settings.json` layers when present
- `CLAUDE.md` and rules files
- loose `.claude/skills/*/SKILL.md`
- loose `.claude/agents/*.md`

### Discovery output

A `RosterCatalog` containing:

- discovered items
- edges
- diagnostics
- raw parsed metadata
- source precedence/scope metadata

## Discovery rules

### 1. Settings scopes

Capture these scopes explicitly:

- user
- project
- local
- managed if visible through materialized config inputs

The module does not need to fully emulate every managed deployment pattern in v1, but it should preserve scope metadata where visible.

### 2. Plugin scan

For each configured plugin root:

- enumerate plugin directories
- parse `plugin.json` when present
- fall back to conventional subdirectories where appropriate
- discover packaged:
  - skills / commands
  - agents
  - hooks
  - MCP servers
  - output styles

Create:

- one `Plugin` item
- one item per packaged component
- `contains` edges from plugin to contained items

### 3. Loose skills

Discover:

- `~/.claude/skills/*/SKILL.md`
- `<project>/.claude/skills/*/SKILL.md`

Parse:

- frontmatter
- description
- `context`
- designated `agent`
- tool-related metadata

### 4. Loose agents

Discover:

- `~/.claude/agents/*.md`
- `<project>/.claude/agents/*.md`

Parse:

- frontmatter
- description
- model/effort/tools if present
- preloaded skills
- MCP references if present
- isolation hints

### 5. Hooks

Discover hook configuration from:

- user/project/local settings layers
- plugin hook configs

Store hook definitions as `HookSet` items rather than individual hook actions in v1. The raw payload should preserve full hook structure.

### 6. MCP definitions

Discover:

- explicit MCP fragments in settings or external config files
- plugin-packaged MCP servers
- any temp/generated fragments defined by a roster

### 7. Instruction sources

Discover:

- `~/.claude/CLAUDE.md`
- project `CLAUDE.md`
- `.claude/CLAUDE.md`
- `.claude/rules/*.md`
- local-only instruction files when explicitly visible

### 8. Output styles

Treat output styles as first-class discoverable items if present, but they can remain secondary in resolution if the prototype does not actively select them yet.

## Edge model

Required edge kinds:

```rust
pub enum ClaudeEdgeKind {
    Contains,
    SkillUsesAgent,
    AgentPreloadsSkill,
    ItemSuggestsMcp,
    ItemRequiresMcp,
    ItemAddsHook,
    ScopeOverrides,
    BackendIncompatible,
}
```

### Examples

- `Plugin(web-a11y) --contains--> Skill(axe-runner)`
- `Skill(pdf-review) --skill-uses-agent--> Agent(pdf-specialist)`
- `Agent(a11y-reviewer) --agent-preloads-skill--> Skill(wcag-guide)`
- `PluginAgent(x) --backend-incompatible--> BackendField(hooks)`

Keep the edge model descriptive rather than overly rigid. Some relationships are “suggested” rather than formally encoded.

## Claude roster file format

Suggested file:

```toml
version = 1
id = "accessibility-auditor"
description = "Claude accessibility audit loadout"

[selection]
plugins = ["web-a11y"]
skills = ["axe-runner"]
agents = ["a11y-reviewer"]
hooks = ["a11y-report-hooks"]
mcp_servers = ["chrome-devtools"]
instructions = ["project:CLAUDE.md", "rule:a11y-review"]
output_styles = []

[run_profile]
backend = "cli"
model = "sonnet"
permission_mode = "plan"
setting_sources = ["project", "user"]
output_format = "stream-json"
include_partial_messages = true
append_system_prompt = "Focus on WCAG 2.2 AA issues and produce a concise findings list."

[resolution]
include_transitive = true
materialization = "temp-overlay"
strict_mcp_config = true
bare = false
```

Notes:

- `selection` values refer to discovered item ids.
- `run_profile` stays harness-native; do not over-normalize.
- `resolution` controls `katachi` behavior, not Claude behavior.

## Resolve algorithm

Given a Claude roster:

1. load directly selected items
2. expand packaging closure
   - if a selected item is plugin-packaged, include the parent plugin
3. expand semantic closure
   - skill -> agent
   - agent -> preloaded skills
   - item -> required MCP
   - item -> added hooks
4. apply scope/override precedence
5. validate unsupported or conflicting fields
6. validate backend projection
7. produce `ResolvedHarnessSelection`

## Validation rules

Minimum validations:

### Packaging validations

- selected packaged item without its plugin should be auto-closed or reported
- multiple plugins exporting colliding names should be surfaced

### Semantic validations

- referenced agent missing
- preloaded skill missing
- required MCP missing
- instruction source missing

### Backend validations

- plugin-shipped agent fields not supported by upstream surface
- SDK projection loses hook behavior or CLI-only skill frontmatter behavior
- selected `settingSources` would exclude required filesystem items

## CLI projection

The Claude CLI plan should be the highest-fidelity path.

### Planning rules

- prefer `--print`
- prefer `--output-format stream-json`
- include partial messages when useful
- use `--setting-sources` to precisely control discovery
- use `--settings` with generated JSON fragments for temp overlays
- use `--plugin-dir` for selected plugin roots
- use `--mcp-config` and optionally strict mode for materialized MCP sets
- use `--agents` for dynamically supplied session-only agents if necessary
- use append vs replace system prompt flags according to roster intent
- optionally use `--bare` when materializing a tightly controlled overlay

### Materialized CLI overlay

In `temp-overlay` mode, create a temp directory containing only what the run should see.

Example overlay contents:

```text
/tmp/katachi-claude-<run-id>/
  settings.json
  mcp.json
  plugins/
  project/
    .claude/
      skills/
      agents/
      rules/
    CLAUDE.md
```

Execution should then:

- point `cwd` at the overlay project dir or the target repo depending on plan
- load selected plugin dirs explicitly
- inject generated settings/MCP configs
- preserve enough metadata to reconstruct the plan later

## SDK projection

SDK projections are useful but must be marked as potentially lossy.

### TypeScript and Python support

The module should expose two SDK projection types:

- `sdk-ts`
- `sdk-py`

### SDK planning rules

- set `settingSources` explicitly
- ensure the correct system prompt mode is chosen
- include required tools for skills or agents
- map CLI-like hooks to callbacks only when a faithful mapping exists
- report projection loss for file-based hook configurations or CLI-only skill behaviors

### SDK-specific warnings

Produce warnings when:

- a skill relies on CLI-only `allowed-tools` frontmatter behavior
- a hook exists only as shell/HTTP/prompt file config
- ambient project/user discovery is required but not enabled in `settingSources`

## Transcript capture

The Claude transcript adapter should store:

- raw stdout lines
- raw stderr lines
- parsed JSON events
- final assistant result
- tool call and tool result summaries where parseable

The parser must be tolerant:

- structured output drift should not destroy the run record
- preserve raw lines when JSON parsing fails

## Commands to expose

```text
katachi harness claude scan
katachi harness claude explain <item-id>
katachi harness claude graph [--format json|dot]
katachi harness claude plan <roster-id> execute <prompt>
katachi harness claude execute <roster-id> <prompt>
```

Suggested extras:

```text
katachi harness claude doctor
katachi harness claude dump-roster <roster-id>
```

## Error handling

Distinguish at least:

- `ClaudeDiscoveryError`
- `ClaudeResolveError`
- `ClaudeValidationError`
- `ClaudeProjectionError`
- `ClaudeExecutionError`

## Acceptance criteria

The Claude module is sufficient for prototype use when it can:

- scan a repo with loose skills and agents
- scan at least one plugin tree
- build a graph with packaging and semantic edges
- materialize a CLI overlay
- run a headless Claude invocation
- capture a transcript and final result
- produce at least one clear SDK projection-loss diagnostic
