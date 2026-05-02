# Codex harness current implementation

This file describes the current `katachi-harness-codex` crate and
`katachi harness codex ...` CLI surface. Top-level flags and shared
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
| `effective-config <roster>` | implemented |
| `dump-settings <roster>` | implemented |
| `dump-roster <roster>` | implemented |
| `project <roster> --sdk ts|py` | implemented with documented projection limitations |

## Configuration

Read from `[harnesses.codex]` by
`crates/katachi-harness-codex/src/runtime.rs::CodexSettings`.

| key | default | role |
|---|---|---|
| `enabled` | `true` | gates the harness |
| `binary` | `"codex"` | binary name or absolute path |
| `default_backend` | `"cli"` | backend fallback |
| `codex_home` | `~/.codex` | `$CODEX_HOME` for discovery and overlays |
| `project_roots` | `["."]` | roots walked for `.codex/` and `AGENTS.md` |
| `marketplace_roots` | `["~/.agents/plugins", ".agents/plugins"]` | plugin package roots |
| `respect_project_trust` | `true` | inactive project layers unless trusted |
| `preserve_failed_overlays` | `true` | keep failed overlays |
| `enable_python_sdk` | `false` | required for `sdk-py` projection |

## Roster format

Path: `<data_root>/rosters/codex/<file>.toml`. The `id` field is the
lookup key.

```toml
version = 1
id = "audit"

[selection]
config_layers = []
profiles = ["review"]
instructions = ["global:AGENTS.md", "project:AGENTS.md"]
skills = []
agents = []
hooks = []
mcp_servers = []
rules = []
plugins = []

[run_profile]
backend = "cli"                  # cli | sdk-ts | sdk-py
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
profile = "review"
output_mode = "machine-readable"
output_schema_file = "schemas/audit.json"
writable_dirs = []               # advisory; no CLI flag is emitted
timeout_secs = 1200

[resolution]
include_transitive = true
materialization = "temp-overlay" # ambient | temp-overlay
respect_project_trust = true
```

## Discovery and effective config

Discovery combines:

- config layers from `/etc/codex/config.toml`, `$CODEX_HOME/config.toml`,
  and project `.codex/config.toml`,
- `AGENTS.md` instruction chains,
- skills, custom agents, hooks, rules, MCP servers, and plugins.

`effective-config <roster>` builds the layered effective config and
prints the complete `EffectiveConfig` JSON with `--json`.

`dump-settings <roster>` is the compact settings view:

```jsonc
{
  "roster": "audit",
  "layer_order": ["..."],
  "active_profile": "review",
  "policy": { "...": "..." },
  "mcp_servers": ["chrome-devtools"]
}
```

`dump-roster <roster>` emits `{ roster, effective, validation, selected }`
and returns `Validate` (5) if validation contains an error.

## Plan and execute

Backend precedence is roster pin, then first valid `--prefer-backend`,
then config default, then `cli`. Invalid roster or config backend
strings return config/planning errors; CLI `--prefer-backend` values that
do not parse are ignored.

CLI argv shape:

```text
<binary> exec --skip-git-repo-check
              [--experimental-json]
              [--ask-for-approval <policy>]
              [--sandbox <mode>]
              [--model <model>]
              [--profile <profile>]
              [--output-schema <path>]
              --cd <project>
              <prompt>
```

`execute` writes the standard run artifacts and commits only on success.
Failed child processes and manifest write failures return `Execute` (7)
and preserve a `.partial` run directory.

Transcript normalization maps Codex JSON stdout lines to typed shared
events when possible: `assistant_message`, `user_message`, `tool_use`,
`tool_result`, `result`, `json_event`, or `stdout_text`.

## Project

`project <roster> --sdk ts|py` emits advisory code and projection
diagnostics.

JSON shape:

```jsonc
{
  "roster": "audit",
  "sdk": "ts",
  "backend": "sdk-ts",
  "code": "...",
  "diagnostics": [],
  "validation": [],
  "argv": ["..."]
}
```

Exit behavior:

- validation errors return `Validate` (5),
- blocking projection errors return `Plan` (6),
- otherwise `Ok` (0).

## Known limitations

| limitation | runtime behavior | exit | JSON shape | coverage |
|---|---|---:|---|---|
| `sdk-py` disabled by default | `project --sdk py` emits `codex.projection.sdk-py.disabled` unless `enable_python_sdk = true` | `6` | `diagnostics` array in project JSON | `per_harness_surfaces::codex_project_py_disabled_exits_plan` |
| `writable_dirs` | retained in policy but no stable Codex CLI flag is emitted | `0` unless another diagnostic blocks | `projection` or `diagnostics` includes `codex.projection.writable-dirs-advisory` | `katachi_harness_codex::projection::tests::writable_dirs_warn_as_advisory` |
| SDK projections are advisory | generated TS/Py code is not executed by `katachi execute` | `0` or `6` depending on diagnostics | project JSON shown above | `per_harness_surfaces::codex_project_ts_emits_advisory_code` |
| Doctor missing binary | per-harness doctor treats missing binary as explicit config failure | `3` | `DoctorReport` JSON has `binary_on_path: false` | `per_harness_surfaces::codex_doctor_returns_config_when_binary_missing` |

Primary CLI coverage: `crates/katachi-cli/tests/harness_codex.rs`,
`crates/katachi-cli/tests/per_harness_surfaces.rs`,
`crates/katachi-cli/tests/policy_contracts.rs`, and
`crates/katachi-cli/tests/have_execute.rs`.
