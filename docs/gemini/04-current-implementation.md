# Gemini harness current implementation

This file describes the current `katachi-harness-gemini` crate and
`katachi harness gemini ...` CLI surface. Top-level flags and shared
status are in [../04-cli-usage-and-status.md](../04-cli-usage-and-status.md).

## Subcommand status

| subcommand | status |
|---|---|
| `scan` | implemented |
| `explain <item-id>` | implemented |
| `graph [--format text|json|dot]` | implemented |
| `doctor` | implemented |
| `plan <roster> execute <prompt>` | implemented |
| `execute <roster> <prompt>` | implemented |
| `dump-settings <roster>` | implemented |
| `dump-roster <roster>` | implemented |
| `effective-config <roster>` | implemented |
| `project <roster> --sdk ts` | implemented with documented projection limits |
| `project <roster> --sdk py` | intentionally unsupported |

## Configuration

Read from `[harnesses.gemini]` by
`crates/katachi-harness-gemini/src/config.rs::GeminiConfig`.

| key | default | role |
|---|---|---|
| `enabled` | `true` | gates the harness |
| `binary` | `"gemini"` | binary name or absolute path |
| `default_backend` | `"cli"` | backend fallback |
| `home` | `~/.gemini` at use time | Gemini home |
| `extension_roots` | `<home>/extensions` | extension roots |
| `user_roots` | `[home]` | user artifact roots |
| `project_roots` | `[cwd]` | project artifact roots |
| `preserve_failed_overlays` | `true` | keep failed overlays |
| `treat_preview_features_as_opt_in` | `true` | preview subagent gate |

## Roster format

Path: `<data_root>/rosters/gemini/<file>.toml`. The `id` field is the
lookup key.

```toml
version = 1
id = "demo"

[selection]
extensions = []
context = ["project:GEMINI.md"]
skills = []
subagents = []
hooks = []
mcp_servers = []
policies = []

[run_profile]
backend = "cli"                  # cli | sdk-ts
model = "gemini-3-pro-preview"
approval_mode = "plan"
output_format = "stream-json"
include_directories = ["docs"]
extensions_mode = "selected-only"
extra_flags = []
binary = "gemini-preview"

[resolution]
include_transitive = true
materialization = "temp-overlay" # ambient | temp-overlay
require_preview_features = false
```

## Discovery and validation

Scanner roots:

- `home`,
- `user_roots`,
- `project_roots`,
- `extension_roots`.

Discovered item kinds include `settings_layer`, `context_source`,
`extension`, `skill`, `subagent`, `hook_set`, `mcp_server`, and
`policy_set`.

Gemini validators add policy, preview-feature, MCP-conflict, and SDK
projection diagnostics. SDK-TS projection rejects selected extensions,
subagents, hooks, and policies because those surfaces are not faithfully
represented by the SDK runner.

## Plan and execute

Backend precedence is roster pin, then `--prefer-backend`, then config
default. Backend `sdk-py` is rejected as unsupported. CLI argv shape:

```text
<binary> --output-format <fmt>
         --approval-mode <mode>
         [--model <model>]
         [--include-directory <dir>]...
         [--extension <name>]...
         [--no-extensions]
         <extra_flags>...
         -p <prompt>
```

`execute` materializes the Gemini overlay when requested, writes the
standard run artifacts, commits only on success, and preserves
`.partial` on failure.

Main `transcript.jsonl` is now written through Gemini's normalizer, so
Gemini JSON lines can become shared typed events. The executor also
writes `transcript.gemini.jsonl` as a sidecar with Gemini-specific event
projections. Sidecar write failures are logged and do not fail the run.

## Debug surfaces

`dump-settings <roster>` folds selected settings layers into:

```jsonc
{
  "roster": "demo",
  "layers": [{ "scope": "project", "body": { "...": "..." } }],
  "effective": { "...": "..." }
}
```

`dump-roster <roster>` emits `{ roster, projected_definition }`.

`effective-config <roster>` emits:

- `roster`, `harness`, `backend`, `materialization`,
- resolved policy details,
- selected item ids,
- `plan_argv` when planning succeeds.

## Project

`project <roster> --sdk ts` emits advisory TS code for
`@google/gemini-cli-sdk`, plus `plan_argv`, validator diagnostics, and
projection diagnostics.

`project <roster> --sdk py` is intentionally unsupported. Runtime
behavior:

- exits `Plan` (6),
- emits code `# Gemini Python SDK projection is unsupported.\n`,
- JSON contains `diagnostics` with `gemini.project.projection`.

SDK-TS limitation runtime behavior:

- nontrivial loadouts that select extensions, subagents, hooks, or
  policies exit `Plan` (6),
- JSON contains `validator_diagnostics` with
  `gemini.projection.sdk-ts-unsupported`.

## Known limitations

| limitation | runtime behavior | exit | JSON shape | coverage |
|---|---|---:|---|---|
| Python SDK projection | intentionally unsupported | `6` | project JSON `diagnostics` contains `gemini.project.projection` | `per_harness_surfaces::gemini_project_py_exits_plan` |
| SDK-TS nontrivial selections | blocked by validator | `6` | `validator_diagnostics` contains `gemini.projection.sdk-ts-unsupported` | `per_harness_surfaces::gemini_project_extension_blocks_sdk_ts` |
| SDK-TS generated code | advisory and not executed by `katachi execute` | `0` when validators pass | project JSON contains `code` and `plan_argv` | `per_harness_surfaces::gemini_project_ts_emits_advisory_code` |
| `extra_flags` | passed through verbatim after planner flags | `0` unless the child rejects them | normal plan/execute JSON | planner tests |
| Extension symlinks | symlink on Unix, copy fallback elsewhere | `7` only if materialization fails | execute error envelope | materializer tests |

Primary CLI coverage: `crates/katachi-cli/tests/harness_gemini.rs`,
`crates/katachi-cli/tests/per_harness_surfaces.rs`, and
`crates/katachi-cli/tests/have_execute.rs`.
