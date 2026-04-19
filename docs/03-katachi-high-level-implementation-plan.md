# katachi high-level implementation plan

## Summary

This plan sequences the work so the prototype becomes useful early:

1. build the shared shell
2. implement discovery/explanation
3. implement one reliable execution path per harness
4. add cross-harness resolution
5. add SDK projections where worthwhile
6. finish with run inspection and polish

## Phase 0: repository bootstrap

### Deliverables

- Rust workspace with the recommended crates
- `clap` command skeleton
- config loading from `~/.katachi/config.toml`
- basic logging/tracing
- top-level help text and subcommands

### Exit criteria

- `katachi --help` works
- `katachi doctor` reports configured harness binaries and key directories
- crate boundaries are stable enough for feature work

## Phase 1: shared core model

### Deliverables

- harness/backend enums
- shared error/diagnostic types
- `InvocationRequest`, `ResolvedKatachi`, `ExecutionPlan`, `ExecutionRecord`
- temp overlay helper
- run persistence and run-id generation
- transcript event schema

### Exit criteria

- dry-run plans can be serialized to JSON
- failed and successful runs both produce persistent records
- a fake harness executor can be run end-to-end

## Phase 2: roster graph and selector engine

### Deliverables

- `DiscoveredItem`, `DependencyEdge`, `RosterCatalog`
- generic graph utilities
- ambiguity resolver
- selector language for choosing items in a roster
- validation framework

### Exit criteria

- graph closure works
- conflicts and missing dependencies are surfaced as diagnostics
- `katachi have <id> describe` can work without executing anything

## Phase 3: per-harness discovery commands

### Deliverables

- `katachi harness claude scan`
- `katachi harness codex scan`
- `katachi harness gemini scan`
- `explain` and `graph` subcommands for each harness

### Exit criteria

- each harness can inventory at least one realistic fixture repo
- scan output includes packaging edges and semantic edges
- discovery results can be cached and reloaded

## Phase 4: Claude module execution

### Deliverables

- Claude resolver
- Claude CLI planner
- Claude CLI executor
- Claude transcript adapter
- initial Claude roster/katachi fixtures

### Exit criteria

- one real Claude katachi can be planned and executed
- plugin packaging closures are respected
- projection-loss diagnostics exist for unsupported SDK cases

## Phase 5: Codex module execution

### Deliverables

- Codex resolver
- Codex CLI planner
- Codex CLI executor
- Codex transcript adapter
- temp `CODEX_HOME` materialization
- AGENTS/rules/hooks/profile handling

### Exit criteria

- one real Codex katachi can be planned and executed
- trusted/untrusted behavior is handled explicitly
- requirements/policy validation exists

## Phase 6: Gemini module execution

### Deliverables

- Gemini resolver
- Gemini CLI planner
- Gemini CLI executor
- Gemini transcript adapter
- extension-first closure logic
- settings/policy validation

### Exit criteria

- one real Gemini katachi can be planned and executed
- extension packaging and conflict rules are represented
- unsupported SDK features are rejected cleanly

## Phase 7: cross-harness `have` command

### Deliverables

- `katachi have <id> ...`
- ambiguity diagnostics
- harness priority config
- backend priority config
- final selection reporting

### Exit criteria

- the same logical katachi id can exist on multiple harnesses
- config-based preference resolution works
- unresolved ambiguity produces a helpful error instead of guessing

## Phase 8: SDK projections

### Deliverables

- Claude SDK projection
- Codex TypeScript SDK projection
- Codex Python SDK feature-flagged projection
- Gemini TypeScript SDK projection for supported subset

### Exit criteria

- plans clearly state when projection is lossy
- supported SDK runs work for at least one realistic fixture
- unsupported combinations fail in planning, not mid-run

## Phase 9: polish and operator tooling

### Deliverables

- `katachi run list/show/transcript`
- better `doctor`
- redaction
- structured JSON output for automation
- richer diagnostics and docs

### Exit criteria

- operators can inspect past runs
- plan and run JSON are stable enough for external tooling
- developer ergonomics are acceptable for daily prototype use

## Suggested milestone order

Recommended sequence:

1. shared core
2. Claude end-to-end
3. Codex end-to-end
4. Gemini end-to-end
5. cross-harness `have`
6. SDK support
7. polish

This order is intentional:

- Claude and Gemini both strongly reward CLI-first planning around plugins/extensions.
- Codex is config-heavy and benefits from shared materialization utilities already existing.
- Cross-harness resolution should not be built until each harness can actually scan and execute.

## Acceptance test matrix

At the end of the prototype, the following should exist:

- one fixture and one runnable katachi per harness
- one ambiguous multi-harness katachi id
- one dry-run plan per harness
- one successful transcript capture per harness
- one intentional projection-loss diagnostic per harness

## Output of the full prototype

The final prototype should let a user do all of the following:

```bash
katachi harness claude scan
katachi harness codex graph
katachi harness gemini explain extension:workspace-a11y

katachi have accessibility-auditor describe
katachi have accessibility-auditor plan execute "Audit the current repo for accessibility risks"
katachi have accessibility-auditor execute "Audit the current repo for accessibility risks"

katachi run list
katachi run show <run-id>
katachi run transcript <run-id>
```
