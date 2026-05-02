# katachi CLI usage and implementation status

This document describes the `katachi` CLI as it exists on this branch.
The design goals are covered by
[01-katachi-conceptual-overview.md](./01-katachi-conceptual-overview.md)
and [02-katachi-implementation-overview.md](./02-katachi-implementation-overview.md).

Per-harness references:

- [claude/04-current-implementation.md](./claude/04-current-implementation.md)
- [codex/04-current-implementation.md](./codex/04-current-implementation.md)
- [gemini/04-current-implementation.md](./gemini/04-current-implementation.md)

## Basic usage

```bash
cargo build -p katachi-cli
katachi --help
katachi doctor
```

Direct harness flow:

```bash
katachi harness claude scan --json
katachi harness codex effective-config audit --json
katachi harness gemini plan demo execute "Audit this repo"
katachi harness claude execute greet "Audit this repo"
```

Cross-harness `have` flow:

```bash
katachi have accessibility-auditor describe --json
katachi have accessibility-auditor graph --format dot
katachi have accessibility-auditor plan execute "Audit this repo"
katachi have accessibility-auditor execute "Audit this repo"
```

Read-side/admin flow:

```bash
katachi katachi list --json
katachi katachi show accessibility-auditor
katachi katachi validate accessibility-auditor --json
katachi run list --json
katachi run show <run-id> --json
katachi run transcript <run-id>
```

## Command tree

All commands currently present in the clap tree are wired to real
implementations. Remaining limitations are listed in the status tables
below.

```text
katachi
├── doctor
├── have <katachi-id>
│   ├── describe
│   ├── graph [--format text|json|dot]
│   ├── plan execute <prompt>
│   └── execute <prompt>
├── katachi
│   ├── list
│   ├── show     <id>
│   └── validate <id>
├── harness <claude|codex|gemini>
│   ├── scan
│   ├── explain          <item-id>
│   ├── graph            [--format text|json|dot]
│   ├── plan             <roster-id> execute <prompt>
│   ├── execute          <roster-id> <prompt>
│   ├── doctor
│   ├── effective-config <roster-id>
│   ├── dump-settings    <roster-id>
│   ├── dump-roster      <roster-id>
│   └── project          <roster-id> [--sdk ts|py]
└── run
    ├── list
    ├── show       <run-id>
    └── transcript <run-id>
```

## Global flags

| flag | env var | meaning |
|---|---|---|
| `--config <PATH>` | `KATACHI_CONFIG` | config file location |
| `--data-root <PATH>` | `KATACHI_DATA` | data root containing `runs/`, `katachis/`, `rosters/` |
| `--cache-root <PATH>` | `KATACHI_CACHE` | cache root |
| `--cwd <PATH>` | | resolve and run as if started in this directory |
| `--json` | | emit JSON where the command supports it |
| `--materialization <MODE>` | | `ambient` or `temp-overlay`; overrides roster materialization |
| `--prefer-harness <NAME>` | | repeatable harness preference override |
| `--prefer-backend <NAME>` | | repeatable backend preference override |
| `--dry-run` | | plan execute paths without spawning the child |
| `-v` / `-vv` | | log verbosity |

Logging precedence is `KATACHI_LOG > RUST_LOG > -v`. Logs always go to
stderr.

## Exit codes

Defined in `crates/katachi-cli/src/exit.rs`.

| code | name | meaning |
|---:|---|---|
| `0` | `Ok` | success |
| `2` | `Usage` | clap usage error |
| `3` | `Config` | config or environment problem |
| `4` | `Resolve` | katachi id, roster id, selector, or run id could not resolve |
| `5` | `Validate` | validation emitted at least one error diagnostic |
| `6` | `Plan` | projection or planning failed after resolution |
| `7` | `Execute` | child execution, persistence, or manifest finalization failed |
| `64` | `NotImplemented` | retained for compatibility; no current clap command uses it |

JSON error envelopes use the command-specific shape, but all include an
`error.kind` and `error.message` when the failure is not a diagnostic
array.

## Status categories

### Implemented

| command family | coverage |
|---|---|
| `doctor` | top-level and all three per-harness doctors |
| `have describe/graph/plan execute/execute` | real Claude, Codex, and Gemini harness modules plus fixture override support |
| `katachi list/show/validate` | definition admin and roster-backed validation |
| `run list/show/transcript` | committed, partial, missing, inconsistent, and malformed states |
| `harness <name> scan/explain/graph` | all three harnesses |
| `harness <name> plan/execute` | all three harnesses, including dry-run and failure persistence |
| `harness <name> dump-settings/dump-roster/effective-config/project` | all three harnesses |

Primary tests:

- `crates/katachi-cli/tests/have_execute.rs`
- `crates/katachi-cli/tests/admin_surfaces.rs`
- `crates/katachi-cli/tests/harness_claude.rs`
- `crates/katachi-cli/tests/harness_codex.rs`
- `crates/katachi-cli/tests/harness_gemini.rs`
- `crates/katachi-cli/tests/per_harness_surfaces.rs`
- `crates/katachi-cli/tests/policy_contracts.rs`

### Implemented with documented limitation

| surface | runtime behavior | exit | JSON shape | pinned by |
|---|---|---:|---|---|
| `have` target with `roster_id` and explicit selectors | rejected during roster expansion; roster-backed targets must use the roster selection only | `4` | `{ "error": { "kind": "resolve", "message": "..." } }` | `admin_surfaces::katachi_validate_roster_id_plus_selectors_returns_resolve` |
| Claude `project --sdk ts|py` | emits advisory SDK code only; `execute` remains CLI-backed and direct SDK planning is projection loss | `0` unless projection diagnostics contain errors | `{ "backend", "code", "diagnostics" }` | `harness_claude::project_ts_emits_sdk_runner` |
| Codex `project --sdk py` with `enable_python_sdk = false` | emits code plus a blocking projection diagnostic | `6` | `{ "roster", "sdk", "backend", "code", "diagnostics", "validation", "argv" }` with `codex.projection.sdk-py.disabled` | `per_harness_surfaces::codex_project_py_disabled_exits_plan` |
| Codex `writable_dirs` | accepted in policy, but no stable Codex CLI flag is emitted; a warning diagnostic is returned | `0` unless other diagnostics block | plan/project JSON `projection` or `diagnostics` contains `codex.projection.writable-dirs-advisory` | `katachi_harness_codex::projection::tests::writable_dirs_warn_as_advisory` |
| Gemini `project --sdk ts` with extensions, subagents, hooks, or policies | rejected because the SDK projection cannot faithfully carry those selected artifacts | `6` | `{ "validator_diagnostics": [ { "code": "gemini.projection.sdk-ts-unsupported", ... } ] }` | `per_harness_surfaces::gemini_project_extension_blocks_sdk_ts` |
| `run show` on a partial run without `manifest.json` | returns a discovered manifest from files on disk and includes a diagnostic | `0` | `{ "state": "partial", "manifest": ..., "diagnostics": [...] }` | `admin_surfaces::run_show_partial_discovers_artifacts_without_manifest` |
| `run transcript` malformed lines | skips malformed lines, renders later typed events, and reports line diagnostics | `0` | `{ "events": [...], "diagnostics": ["line N: ..."] }` | `admin_surfaces::run_transcript_human_uses_flat_event_schema_and_keeps_later_events` |

### Intentionally unsupported

| surface | runtime behavior | exit | JSON shape | pinned by |
|---|---|---:|---|---|
| `katachi harness gemini project <roster> --sdk py` | Gemini has no supported Python SDK projection; command returns an advisory unsupported snippet and diagnostic | `6` | `{ "diagnostics": [ { "code": "gemini.project.projection", ... } ], "code": "# Gemini Python SDK projection is unsupported.\n" }` | `per_harness_surfaces::gemini_project_py_exits_plan` |

### Not yet implemented

No current clap-tree command is a stub on this branch. The
`NotImplemented` exit code and helper remain in the codebase for future
command additions.

## `have` resolution and execution

`katachi have <id> ...` loads `<data_root>/katachis/*.toml`, expands any
`roster_id` targets into selectors and run-profile overlays, registers
enabled real harness modules, and then resolves the chosen target. Fixture
harnesses can still be prepended through `KATACHI_FIXTURE_HARNESSES` for
tests.

Backend precedence is:

```text
target backend pin > roster backend pin > --prefer-backend > config default > cli
```

Run-profile overlay precedence is target field over roster field.
`--materialization` overrides the roster materialization for the
invocation.

`have execute` writes `request.json`, `plan.json`, raw logs,
`transcript.jsonl`, `record.json`, and `manifest.json` under
`<data_root>/runs/<run-id>.partial/`, then commits to
`<data_root>/runs/<run-id>/` only on success. Failed child processes and
manifest write failures preserve the `.partial` directory and return
`Execute` (7).

## Run inspection

`katachi run list` includes committed and `.partial` run directories.
Malformed entries stay visible with per-entry diagnostics.

`katachi run show <run-id>` accepts either the bare id or the
`.partial` suffix. If both committed and partial directories exist for
the same id, it returns `Resolve` (4) with a JSON resolve error.

`katachi run transcript <run-id>` parses the real
`TranscriptEvent` schema. Forward-compatible JSON shapes are preserved
as raw events with diagnostics; malformed lines are reported and skipped.

## Quality gates

Required for this branch:

```bash
cargo fmt --check
git diff --check origin/main...HEAD
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
