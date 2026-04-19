# katachi implementation overview

## Summary

This document describes a pragmatic Rust implementation for a `katachi` prototype.

The recommended architecture is:

- Rust workspace
- `clap` for command parsing
- shared core library for config, graphs, planning, execution, and transcripts
- one harness crate per initial harness
- CLI-first execution adapters
- optional SDK adapters behind the same harness module boundary

The core execution model is **plan -> execute -> record**.

## Prototype scope

The prototype should support:

- harness discovery and explanation
- named katachi selection
- ambiguity handling
- plan inspection
- real headless execution
- transcript capture
- basic run history

The prototype does **not** need:

- remote services
- background daemons
- distributed execution
- full marketplace/package-management behavior
- cross-machine state sync

## Technology choices

Recommended Rust dependencies:

- `clap` for CLI parsing
- `serde`, `serde_json`, `toml` for config and persistence
- `tokio` for async process and filesystem work
- `camino` for UTF-8 paths
- `indexmap` for stable-order maps
- `petgraph` or a lightweight custom graph layer for dependency edges
- `thiserror` for typed errors
- `anyhow` for top-level command composition
- `schemars` optionally for JSON schemas for plan/run record output
- `tempfile` for materialization
- `tracing` + `tracing-subscriber` for internal diagnostics
- `regex` / `globset` for discovery and filters
- `uuid`, `time` for run metadata

Optional later dependencies:

- `jsonschema` for validating structured outputs
- `duct` or direct `tokio::process::Command` wrappers for child processes
- `insta` for snapshot tests

## Recommended workspace layout

Keep the initial workspace modest. Avoid too many crates in v1.

```text
katachi/
  Cargo.toml
  crates/
    katachi-cli/
    katachi-core/
    katachi-harness-claude/
    katachi-harness-codex/
    katachi-harness-gemini/
    katachi-test-support/
```

### `katachi-cli`

Responsibilities:

- parse CLI commands
- load global config
- choose harness module
- render plans, diagnostics, and final output
- write run records

### `katachi-core`

Responsibilities:

- config loading
- common data model
- roster graph model
- ambiguity resolution
- execution plan structs
- transcript normalization
- temp materialization helpers
- child process helpers
- redaction and persistence utilities

### `katachi-harness-*`

One crate per harness. Each crate owns:

- discovery
- parsing of native artifacts
- edge extraction
- resolver rules
- backend planning
- backend execution and transcript adaptation

### `katachi-test-support`

Responsibilities:

- fake harness executables
- fixture builders
- golden plan fixtures
- transcript normalization helpers for tests

## Command-line shape

A good prototype command tree:

```text
katachi
  have <katachi-id> <action> ...
  katachi <subcommand>
  harness <claude|codex|gemini> <subcommand>
  run <subcommand>
  doctor
```

Suggested actions:

```text
katachi have <id> describe
katachi have <id> plan execute <prompt>
katachi have <id> execute <prompt>
katachi have <id> graph
```

Suggested harness subcommands:

```text
katachi harness claude scan
katachi harness claude explain <item-id>
katachi harness claude graph
katachi harness claude plan <roster-id> execute <prompt>
katachi harness claude execute <roster-id> <prompt>
```

Similar groups should exist for `codex` and `gemini`.

Suggested katachi management subcommands:

```text
katachi katachi list
katachi katachi show <id>
katachi katachi validate <id>
```

Suggested run inspection:

```text
katachi run list
katachi run show <run-id>
katachi run transcript <run-id>
```

## Shared CLI behaviors

Global flags worth supporting early:

```text
--config <path>
--cwd <path>
--json
--materialization <ambient|temp-overlay>
--prefer-harness <name>...
--prefer-backend <name>...
--dry-run
--verbose
```

## Plan -> execute -> record pattern

This is the most important implementation pattern.

### Step 1: Parse into a request

`clap` output becomes a typed `InvocationRequest`.

### Step 2: Resolve

The requested `katachi` is resolved into one concrete harness/backend choice plus a closed set of selected items.

### Step 3: Plan

The harness module returns a concrete `ExecutionPlan`.

Example components of a plan:

- selected backend
- materialization strategy
- files to create/symlink
- environment variables to set
- command argv or SDK invocation details
- transcript parser to use
- final result extraction strategy

### Step 4: Execute

The plan runs through a child process or SDK adapter.

### Step 5: Record

All relevant data becomes an `ExecutionRecord`, even on failure.

This pattern is critical because it allows:

- dry runs
- plan inspection
- reproducible debugging
- later replay tooling

## Shared data model

The shared model should be intentionally small.

### Core enums

```rust
pub enum HarnessKind {
    Claude,
    Codex,
    Gemini,
}

pub enum BackendKind {
    Cli,
    SdkTs,
    SdkPy,
    McpServer,
    AppServer,
}

pub enum MaterializationMode {
    Ambient,
    TempOverlay,
}
```

### Common discovery envelope

```rust
pub struct DiscoveredItem {
    pub item_ref: ItemRef,
    pub display_name: String,
    pub source: ItemSource,
    pub packaging: Option<PackageRef>,
    pub raw: serde_json::Value,
    pub capabilities: Vec<String>,
    pub constraints: Vec<Constraint>,
}

pub struct DependencyEdge {
    pub from: ItemRef,
    pub to: ItemRef,
    pub kind: EdgeKind,
    pub required: bool,
    pub note: Option<String>,
}
```

Keep `raw` as a harness-native payload. Do not try to flatten every field.

### katachi selection model

```rust
pub struct KatachiDefinition {
    pub id: String,
    pub description: Option<String>,
    pub targets: Vec<KatachiTarget>,
}

pub struct KatachiTarget {
    pub harness: HarnessKind,
    pub roster_id: String,
    pub backend: BackendPreference,
    pub preference: i32,
    pub selectors: Vec<Selector>,
    pub run_profile_overlay: serde_json::Value,
}
```

### Execution model

```rust
pub struct ExecutionPlan {
    pub summary: String,
    pub harness: HarnessKind,
    pub backend: BackendKind,
    pub materialization: MaterializationPlan,
    pub execution: ExecutionBackendPlan,
    pub transcript_mode: TranscriptMode,
}
```

## Harness module trait

Keep the trait simple:

```rust
pub trait HarnessModule {
    fn kind(&self) -> HarnessKind;

    fn scan(&self, ctx: &ScanContext) -> Result<RosterCatalog>;
    fn explain(&self, ctx: &ExplainContext) -> Result<ExplainResult>;
    fn resolve(&self, ctx: &ResolveContext) -> Result<ResolvedHarnessSelection>;
    fn plan(&self, ctx: &PlanContext) -> Result<ExecutionPlan>;
    fn execute(&self, plan: &ExecutionPlan) -> Result<ExecutionRecord>;
}
```

Separate “scan” from “resolve”. Discovery and selection should not be conflated.

## Config model

Use TOML for user-facing `katachi` config.

Suggested top-level file:

```toml
version = 1

[defaults]
harness_priority = ["claude", "codex", "gemini"]
backend_priority = ["cli", "sdk-ts", "sdk-py"]
materialization = "temp-overlay"

[storage]
root = "~/.katachi"
cache_dir = "~/.cache/katachi"

[harnesses.claude]
enabled = true
binary = "claude"

[harnesses.codex]
enabled = true
binary = "codex"

[harnesses.gemini]
enabled = true
binary = "gemini"
```

Keep harness-specific configuration nested under `harnesses.<name>` and allow raw passthrough JSON/TOML blobs where necessary.

## On-disk layout

```text
~/.katachi/
  config.toml
  katachis/
  rosters/
    claude/
    codex/
    gemini/
  runs/
  cache/
```

Recommendations:

- `kattachis/`: user-facing named loadouts
- `rosters/`: harness-native definitions and selectors
- `runs/`: immutable execution records
- `cache/`: discovery caches and parsed graphs

## Transcript normalization

`katachi` should preserve raw harness output and also produce a normalized event stream.

Suggested event schema:

```rust
pub enum TranscriptEventKind {
    StdoutText,
    StderrText,
    JsonEvent,
    ToolUse,
    ToolResult,
    AssistantMessage,
    UserMessage,
    Warning,
    Result,
}
```

Do not throw away raw lines. Store both:

- raw child output
- parsed normalized events

This matters because structured event formats drift over time.

## Materialization strategy

The core library should provide generic helpers for:

- temp directory creation
- symlink or copy trees
- writing generated config fragments
- environment override setup
- cleanup policy

Recommended initial policies:

- keep failed run overlays by default
- optionally keep successful overlays with `--keep-overlay`
- persist a manifest of what was written

## Failure model

Distinguish:

- discovery errors
- ambiguity errors
- validation errors
- projection errors
- execution errors
- transcript parsing warnings

Not every parse drift should fail the whole run. For example, a CLI transcript parser may partially parse events while preserving raw output.

## Testing strategy

### Unit tests

- config parsing
- graph construction
- selector logic
- ambiguity resolution
- plan generation

### Fixture tests

Use sample harness directories to test discovery:

- Claude plugin fixtures
- Codex `.codex/` + `AGENTS.md` fixtures
- Gemini extension fixtures

### Stub executable tests

Provide fake `claude`, `codex`, and `gemini` executables that emit stable JSON/text traces to verify execution and transcript capture.

### Snapshot tests

Snapshot:

- scan results
- execution plans
- validation diagnostics
- normalized transcripts

## Recommended implementation order

1. shared config + command tree
2. shared roster graph + run record model
3. harness scanning commands
4. single-harness planning and execution
5. cross-harness `have` resolution
6. SDK adapters
7. polish, diagnostics, run inspection

## Practical prototype boundary

For the initial prototype:

- prioritize **CLI execution** for all three harnesses
- implement **SDK projection** as a second step
- keep **roster items typed enough to reason about dependencies**
- avoid trying to perfectly mirror every upstream field from day one
