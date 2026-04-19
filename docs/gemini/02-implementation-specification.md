# Gemini implementation specification

## Purpose

This spec defines the Gemini module for the `katachi` prototype.

The module must model Gemini CLI as an extension-first, settings-layered harness with a narrower SDK projection surface.

Backend priority:

1. CLI
2. TypeScript SDK

Python SDK support is out of scope for now because there is no official Gemini CLI Python SDK yet.

## Module boundaries

Crate name:

```text
crates/katachi-harness-gemini
```

Responsibilities:

- discover settings-layer and extension-based artifacts
- build extension-aware roster graphs
- validate security/admin/policy constraints
- project to CLI or supported SDK subset
- execute headless Gemini runs and record transcripts

## Module configuration

Suggested config section:

```toml
[harnesses.gemini]
enabled = true
binary = "gemini"
default_backend = "cli"

home = "~/.gemini"
extension_roots = ["~/.gemini/extensions"]
user_roots = ["~/.gemini"]
project_roots = ["."]

preserve_failed_overlays = true
treat_preview_features_as_opt_in = true
```

Notes:

- `extension_roots` should be configurable.
- preview/experimental behavior should be explicit in diagnostics.
- the module should prefer CLI for anything extension-heavy.

## Internal item kinds

```rust
pub enum GeminiItemKind {
    SettingsLayer,
    ContextSource,
    Extension,
    Skill,
    Subagent,
    HookSet,
    McpServer,
    PolicySet,
    RunProfile,
}
```

`Extension` is the key packaging item.

`SettingsLayer` captures user/project/system overlays and generated temp settings.

`ContextSource` covers `GEMINI.md` or alternate configured context file names.

## Discovery model

### Inputs to discovery

- user/project settings.json layers
- user/project context files
- configured extension roots
- loose skills roots
- loose subagent roots
- hook configuration files
- MCP settings
- security/admin settings
- experimental feature flags relevant to selected capabilities

### Discovery output

- discovered items
- extension containment edges
- settings precedence metadata
- conflict and legality diagnostics
- raw parsed metadata for unstable surfaces

## Discovery rules

### 1. Settings layers

Discover and preserve:

- user settings
- project settings
- generated/temporary overlays
- relevant environment/CLI-derived overrides if provided by the current request

Preserve precedence and source path.

### 2. Context sources

Discover:

- `GEMINI.md`
- alternate context file names when configured
- extension-local context files

Emit `ContextSource` items with source scope and effective file name.

### 3. Extensions

Scan configured extension roots for installed extensions.

Parse `gemini-extension.json` and preserve:

- name
- version
- description
- `mcpServers`
- `contextFileName`
- `excludeTools`
- plan directory settings
- extension installation settings
- other raw manifest fields

Also scan extension subdirs for:

- `commands/`
- `hooks/hooks.json`
- `skills/`
- `agents/`
- `policies/`
- themes where present

Emit:

- one `Extension` item
- one item per packaged skill/subagent/hook set/policy set/MCP server/context source where useful
- `Contains` edges from extension to contained items

### 4. Loose skills

Discover loose skills in user/project roots when available.

Because Gemini skill docs have moved quickly, keep parsing tolerant:

- preserve raw frontmatter/body
- capture description and path
- record whether it is loose or extension-packaged

### 5. Subagents

Discover built-in, user, project, and extension-provided subagents where visible.

Preserve:

- description
- path
- tool restrictions
- model hints
- preview/experimental requirement flags where inferable

### 6. Hooks

Discover hook configuration and model it as `HookSet` items.

Important implementation note:

- keep hook parsing schema-tolerant
- preserve raw JSON
- let the effective settings schema drive validation
- do not overfit to stale docs

### 7. MCP

Discover MCP servers from:

- settings layers
- extensions
- generated temp overlays

Preserve transport and conflict metadata.

### 8. Policies and security/admin constraints

Discover:

- extension policies
- security settings relevant to extensions/MCP/approval modes
- admin settings that disable extensions or MCP

These should either appear as `PolicySet` items or as validation inputs attached to the run context.

## Edge model

Required edge kinds:

```rust
pub enum GeminiEdgeKind {
    Contains,
    SettingsOverrides,
    SettingsWinsMcpNameConflict,
    ExtensionAddsContext,
    ExtensionAddsPolicy,
    ExtensionAddsHook,
    SubagentNeedsPreviewFeatures,
    PolicyConstrainsRun,
    BackendIncompatible,
}
```

Examples:

- `Extension(workspace-a11y) --contains--> Skill(accessibility-audit)`
- `Settings(project) --settings-wins-mcp-name-conflict--> McpServer(openai-docs)`
- `Subagent(codebase_investigator) --subagent-needs-preview-features--> Experimental(subagents)`

## Gemini roster file format

Suggested file:

```toml
version = 1
id = "accessibility-auditor"
description = "Gemini accessibility audit loadout"

[selection]
extensions = ["workspace-a11y"]
context = ["project:GEMINI.md"]
skills = ["accessibility-audit"]
subagents = ["codebase_investigator"]
hooks = ["a11y-hooks"]
mcp_servers = ["chrome-devtools"]
policies = ["readonly-audit"]

[run_profile]
backend = "cli"
model = "gemini-3-pro-preview"
approval_mode = "plan"
output_format = "stream-json"
include_directories = ["docs", "apps/web"]
extensions_mode = "selected-only"

[resolution]
include_transitive = true
materialization = "temp-overlay"
require_preview_features = true
```

## Resolve algorithm

1. collect selected extensions and loose items
2. expand packaging closure from extensions
3. expand semantic closure:
   - extension context
   - extension policies
   - required MCP
   - preview feature requirements for subagents
4. merge settings precedence
5. validate admin/security legality
6. validate backend projection
7. produce resolved selection + diagnostics

## Validation rules

### Extension conflict validation

- same-named extension commands must be handled according to Gemini’s precedence rules
- same-named MCP definitions should respect settings-layer precedence over extension-provided definitions

### Policy validation

- extension policies cannot silently grant dangerous allow/yolo behavior
- requested approval mode must respect admin/security constraints

### Preview feature validation

- if selected subagents require preview/experimental settings, planning must either ensure those settings are present or fail clearly

### Backend validation

Reject SDK projection when the resolved set depends on unsupported features such as:

- extensions
- hooks
- subagents
- policy engine behavior

## CLI projection

This is the primary execution path.

### Planning rules

The Gemini CLI plan should:

- use headless mode
- prefer `--output-format stream-json`
- select or disable extensions explicitly
- pass approval mode explicitly
- pass include-directories explicitly
- set model explicitly
- use materialized settings for reproducibility
- preserve raw stdout/stderr

### Materialized Gemini environment

Recommended overlay:

```text
/tmp/katachi-gemini-<run-id>/
  home/
    .gemini/
      settings.json
      extensions/
  project/
    .gemini/
      settings.json
      skills/
      agents/
      hooks/
    GEMINI.md
```

Depending on platform constraints, the module may set `HOME` (or equivalent user-dir env) for the child process so the CLI sees the temp home.

The planner should support:

- `selected-only` extension mode
- disabling all extensions when desired
- generated settings overrides for MCP, security, and experimental flags

## SDK projection

Only support `sdk-ts`.

### Projection rules

The module should allow SDK projection only for the supported subset:

- instructions/context
- tools
- skills
- model/cwd/session behavior

The module should reject plans depending on:

- extension lifecycle
- hook execution
- subagent behavior
- policy engine behavior
- admin/security surfaces not modeled by the SDK

This must be a planning-time diagnostic, not a runtime surprise.

## Transcript capture

The Gemini transcript adapter should understand headless `stream-json` events such as:

- `init`
- `message`
- `tool_use`
- `tool_result`
- `error`
- `result`

Also preserve:

- raw stdout lines
- raw stderr lines
- final response text
- stats payload

## Commands to expose

```text
katachi harness gemini scan
katachi harness gemini explain <item-id>
katachi harness gemini graph
katachi harness gemini plan <roster-id> execute <prompt>
katachi harness gemini execute <roster-id> <prompt>
```

Suggested extras:

```text
katachi harness gemini doctor
katachi harness gemini dump-settings <roster-id>
```

## Error handling

Distinguish:

- `GeminiDiscoveryError`
- `GeminiResolveError`
- `GeminiValidationError`
- `GeminiProjectionError`
- `GeminiExecutionError`

## Acceptance criteria

The Gemini module is sufficient for prototype use when it can:

- scan installed extensions
- discover loose and extension-packaged skills/subagents/hooks
- model settings precedence and extension containment
- validate admin/security constraints
- run a headless Gemini CLI session with selected extensions
- capture stream-json transcript events
- reject unsupported SDK projections clearly
