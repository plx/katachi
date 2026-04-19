# katachi conceptual overview

## Summary

`katachi` is a command-line indirection layer for coding-agent harnesses. A user asks for a **named loadout**:

```bash
katachi have accessibility-auditor execute "$prompt"
```

`katachi` then resolves that name against one or more harness-native definitions and produces a concrete execution plan for a specific backend such as:

- Claude Code CLI
- Claude Agent SDK
- Codex CLI
- Codex SDK
- Gemini CLI
- Gemini CLI SDK where feasible

The key architectural principle is this:

> `katachi` should normalize the *lifecycle* of discovery, resolution, validation, execution, and recording — not normalize the *meaning* of every harness feature.

## Goals

### 1. Stable indirection

Users should refer to a durable logical name such as `accessibility-auditor`, `pr-reviewer`, or `release-notes-writer`, without caring whether that loadout is implemented through Claude plugins, Codex skills plus AGENTS.md, or a Gemini extension.

### 2. Harness-native power retention

`katachi` must preserve access to the distinctive capabilities of each harness:

- Claude plugins, skills, subagents, hooks, MCP, output styles, CLAUDE.md/rules
- Codex AGENTS.md layering, Team Config, skills, custom agents, rules, hooks, profiles, requirements
- Gemini extensions, context files, skills, hooks, MCP, subagents, policies, browser agent settings

### 3. Reproducible headless execution

A `katachi` invocation should be executable non-interactively and should produce:

- a concrete plan
- a recorded transcript
- a captured final response
- enough metadata to explain what happened later

### 4. Uniform operator experience

Even though harnesses differ, the outer interaction model should feel consistent:

- discover
- explain
- resolve ambiguity
- plan
- execute
- inspect transcript/result

### 5. Explicit projection boundaries

If a requested loadout can run faithfully through a CLI backend but not through an SDK backend, `katachi` should say so before execution.

## Non-goals

### 1. Lowest-common-denominator abstraction

`katachi` is not trying to define a universal “agent configuration format” that all harnesses are reduced into.

### 2. Perfect cross-harness portability

A Claude roster and a Gemini roster may represent the same user intent, but they are not expected to contain equivalent native features or be mechanically interchangeable.

### 3. Full package-manager behavior in v1

The prototype does not need a complete plugin/extension marketplace client. It only needs enough inventory and materialization logic to support real prototype executions.

### 4. Cloud orchestration or background job systems

The prototype is local, synchronous, and foreground-oriented.

### 5. Hiding upstream complexity

Some harnesses have scope rules, trust rules, preview features, or policy restrictions. `katachi` should surface that complexity rather than pretend it does not exist.

## Core concepts

### Harness

A harness is the native execution environment for a coding agent. Examples: Claude Code, Codex, Gemini CLI.

### Backend

A backend is a concrete execution route for a harness, such as:

- `cli`
- `sdk-ts`
- `sdk-py`
- future adapters like `mcp-server` or `app-server`

### Roster

A **roster** is a harness-native inventory of configurable artifacts plus their relationships.

A roster is not just a flat list of assets. It also includes:

- provenance
- scope
- packaging boundaries
- dependency edges
- policy constraints
- projection constraints

### Roster item

A roster item is a harness-native artifact such as:

- Claude plugin, skill, agent, hook set, MCP server, instruction source
- Codex AGENTS layer, skill, custom agent, hook set, rule file, profile, plugin
- Gemini extension, skill, subagent, hook set, MCP server, policy set, context source

### Packaging dependency

A packaging dependency says “this item exists only because a larger package is installed or enabled.”

Examples:

- Claude skill inside a plugin
- Codex skill distributed inside a plugin
- Gemini subagent packaged inside an extension

### Semantic dependency

A semantic dependency says “this item expects another item to exist.”

Examples:

- a skill that expects a specific subagent
- a custom agent that preloads a skill
- a skill intended to use a specific MCP server
- a policy or rule required for a safe run profile

### Constraint

A constraint is anything that can make a requested loadout illegal or incomplete, such as:

- “this plugin-shipped Claude agent cannot define hooks”
- “Codex project config is ignored when the project is untrusted”
- “Gemini extension policies cannot grant allow/yolo”
- “Gemini SDK does not currently support extensions or hooks”

### katachi

A `katachi` is a **named selection of roster items plus a run profile**.

It is the user-facing loadout object.

### Run profile

A run profile contains per-invocation behavior like:

- backend preference
- model / effort / reasoning
- approval mode / permission mode / sandbox
- output formatting
- prompt overlays
- materialization mode
- transcript behavior

### Materialization

Materialization means constructing an ephemeral filesystem/configuration overlay so a run is reproducible.

Common examples:

- temp settings files
- temp home dirs like `CODEX_HOME`
- temp plugin/extension selection
- generated MCP config fragments
- temporary instruction files

### Projection

Projection means translating a resolved `katachi` into a specific backend.

Example: the same resolved Claude selection may be projectable to:

- a CLI plan using `claude -p ...`
- an SDK plan using `ClaudeAgentOptions(...)`

But the projection may be lossy.

### Execution record

A normalized record of what was run and what happened, including:

- request metadata
- resolved harness/backend
- generated command or SDK call plan
- transcript events
- final result
- redacted environment and file materialization metadata

## Lifecycle

`katachi` should use the same lifecycle for every harness:

1. **Discover**  
   Inventory harness-native artifacts and build a roster graph.

2. **Explain**  
   Show the operator what exists, where it came from, and what it depends on.

3. **Resolve**  
   Choose a harness/backend and expand the requested `katachi` into a closed set of required items.

4. **Validate**  
   Check packaging closures, semantic dependencies, policy legality, and backend projection limits.

5. **Project**  
   Create a backend-specific execution plan.

6. **Execute**  
   Run the plan through the target harness.

7. **Record**  
   Persist transcript, final output, structured metadata, and diagnostics.

## Ambient mode vs materialized mode

`katachi` should support two broad execution modes.

### Ambient mode

Use the user’s existing harness installation and discovery behavior as-is.

Use this when:

- the operator wants speed
- the loadout relies on already-installed artifacts
- reproducibility is less important than convenience

Risks:

- implicit state leaks in from user/project config
- harder to explain why two machines differ

### Materialized mode

Write an explicit ephemeral overlay and run the harness against it.

Use this when:

- headless reproducibility matters
- CI or automation must be deterministic
- the roster contains many implicit packaging/setting relationships

Recommendation for the prototype:

- support both
- prefer **materialized mode** for actual `katachi have ... execute ...`
- keep ambient mode for fast local experiments and debugging

## Ambiguity resolution

`katachi have <katachi-id>` should resolve like this:

1. find all matching katachis across enabled harnesses
2. if exactly one match exists, use it
3. if multiple matches exist:
   - use an explicit harness selection if provided
   - otherwise use configured harness/backend preference if the ambiguity is fully resolved by config
   - otherwise fail with a useful ambiguity report

Ambiguity is a feature, not a bug. It allows the same logical loadout name to exist on multiple harnesses.

## Minimal normalized data model

The shared layer should be intentionally small.

A good common envelope looks like this:

```rust
pub struct InvocationRequest {
    pub katachi_id: String,
    pub action: ActionRequest,
    pub preferred_harnesses: Vec<HarnessKind>,
    pub preferred_backends: Vec<BackendKind>,
    pub cwd: Utf8PathBuf,
    pub materialization: MaterializationMode,
}

pub struct ResolvedKatachi {
    pub katachi_id: String,
    pub harness: HarnessKind,
    pub backend: BackendKind,
    pub selected_items: Vec<ResolvedItemRef>,
    pub run_profile: RunProfile,
    pub warnings: Vec<Diagnostic>,
}

pub struct ExecutionRecord {
    pub run_id: Uuid,
    pub request: InvocationRequest,
    pub resolved: ResolvedKatachi,
    pub plan: ExecutionPlanSummary,
    pub events: Vec<TranscriptEvent>,
    pub result: FinalResult,
}
```

Everything deeper than that should remain harness-native.

## Example end-to-end flow

```bash
katachi have accessibility-auditor execute "$prompt"
```

High-level behavior:

1. Load `katachi.toml`.
2. Query enabled harness modules for `accessibility-auditor`.
3. If multiple matches exist, apply harness priority or error.
4. Resolve requested roster items and dependency closure.
5. Validate:
   - packaging dependencies satisfied
   - semantic edges closed
   - selected backend supports required features
6. Materialize an overlay if requested.
7. Execute through harness CLI or SDK.
8. Capture transcript + final response.
9. Persist a run record.
10. Print a concise operator-facing result.

## Suggested storage layout

```text
~/.katachi/
  config.toml
  runs/
  cache/
  catalogs/
    claude/
    codex/
    gemini/
  katachis/
    shared/
  rosters/
    claude/
    codex/
    gemini/
```

A reasonable split is:

- `config.toml`: user configuration, search roots, harness preferences
- `catalogs/`: discovered inventory caches
- `rosters/`: harness-native roster definitions
- `katachis/`: named selection objects that point at rosters
- `runs/`: transcripts and execution records

## Prototype design principles

1. **Resolver-first, not abstraction-first**
2. **CLI-first for fidelity**
3. **SDK adapters where they are genuinely useful**
4. **Typed shared envelope, raw harness payloads underneath**
5. **Explicit diagnostics for projection loss**
6. **Materialization is a first-class feature**
7. **Do not guess undocumented install paths if they can be configured instead**

## Important prototype decisions

### Decision 1: rosters are per-harness

There is no single cross-harness roster type.

### Decision 2: katachi is a selector, not a full config blob

A `katachi` should point to and select from harness-native rosters, instead of duplicating the entire harness configuration inside the `katachi` object.

### Decision 3: execution records are normalized

This is the main place where cross-harness uniformity is valuable.

### Decision 4: SDK support is secondary to CLI fidelity

For the prototype, the most important path is:

- resolve
- plan
- execute headlessly
- capture transcript

If an SDK path cannot faithfully reproduce a CLI loadout, the CLI path wins.

## Prototype success criteria

The prototype is successful if it can:

- represent at least one realistic loadout for each of Claude Code, Codex, and Gemini
- discover and explain native roster items
- resolve ambiguity across harnesses
- run headlessly in a reproducible way
- capture a stable execution record
- clearly distinguish unsupported or lossy backend projections
