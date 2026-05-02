# Claude harness current implementation

This file describes the current `katachi-harness-claude` crate and
`katachi harness claude ...` CLI surface. Top-level flags and shared
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
| `project <roster> --sdk ts|py` | implemented with advisory SDK-code limitation |

## Configuration

Read from `[harnesses.claude]` by
`crates/katachi-harness-claude/src/config.rs::ClaudeConfig`.

| key | default | role |
|---|---|---|
| `enabled` | `true` | gates the harness |
| `binary` | `"claude"` | binary name or absolute path |
| `default_backend` | `"cli"` | backend fallback |
| `plugin_roots` | `["~/.claude/plugins"]` | installed plugin roots |
| `user_root` | `"~/.claude"` | user-scoped Claude root |
| `project_roots` | `["."]` | project-scoped Claude roots |
| `default_setting_sources` | `["user", "project"]` | roster fallback |
| `prefer_materialized_cli` | `true` | parsed; not currently consulted by planner |
| `preserve_failed_overlays` | `true` | keep failed temp overlays |
| `roster_dir` | `<data_root>/rosters/claude/` | roster directory override |

## Roster format

Path: `<data_root>/rosters/claude/<file>.toml`. The `id` field is the
lookup key.

```toml
version = 1
id = "greet"

[selection]
plugins = []
skills = ["greeter"]
agents = []
hooks = []
mcp_servers = []
instructions = ["project:CLAUDE.md"]
output_styles = ["brief"]

[run_profile]
backend = "cli"                  # cli | sdk-ts | sdk-py
model = "claude-sonnet-4-6"
permission_mode = "acceptEdits"
output_format = "stream-json"
setting_sources = ["user", "project"]
allowed_tools = ["Read", "Edit"]
disallowed_tools = []
timeout_secs = 1200

[resolution]
include_transitive = true
materialization = "temp-overlay" # ambient | temp-overlay
strict_mcp_config = true
bare = false
```

## Discovery

Source: `crates/katachi-harness-claude/src/discovery/`.

| kind | source |
|---|---|
| `plugin` | installed plugin directories |
| `skill` | loose and plugin-packaged skills |
| `agent` | loose and plugin-packaged agents |
| `hook_set` | settings hooks |
| `mcp_server` | `settings.json` and `mcp.json` |
| `instruction_source` | `CLAUDE.md` and rule files |
| `output_style` | plugin-packaged and loose `output-styles/*.md` |

Loose output styles are scanned from user and project Claude roots.
Duplicate style ids emit `claude.duplicate-output-style`.

`scan --json` returns the full shared `RosterCatalog`. Human scan output
is grouped by item kind.

## Plan and execute

Backend precedence is roster pin, then `--prefer-backend`, then config
default, then `cli`. The CLI planner only executes the CLI backend.
SDK backends are projected through `project` and direct SDK execution is
not wired.

`plan <roster> execute <prompt>` resolves the roster, validates the
selection, builds an `ExecutionPlan`, and returns:

- `Resolve` (4) for missing rosters or selector resolution errors,
- `Validate` (5) for validation errors,
- `Plan` (6) for projection/planning errors,
- `Ok` (0) on success.

`execute <roster> <prompt>` builds the same plan, honors `--dry-run`,
materializes temp overlays when needed, spawns the child, writes the run
artifacts, and commits the run directory only on success. Failed child
processes or persistence failures return `Execute` (7) and preserve a
`.partial` run directory.

Transcript normalization maps Claude stream-json lines to shared typed
events: `assistant_message`, `user_message`, `tool_use`, `tool_result`,
`warning`, `result`, `json_event`, or `stdout_text`.

## Debug surfaces

`dump-roster <roster>` emits JSON containing:

- `roster`,
- `resolved`,
- `projection_diagnostics`,
- `validator_diagnostics`,
- `catalog`.

`dump-settings <roster>` emits JSON containing:

- selected run-profile fields,
- parsed inline `settings` and `mcp` overlay JSON when present,
- `selected_hooks`,
- detailed `overlay_files` with destination, source kind, inline byte
  count, and parsed inline JSON,
- diagnostics and argv.

`effective-config <roster>` emits JSON containing:

- `roster`, `harness`, `backend`,
- `materialization.mode`, `materialization.files`,
  `materialization.overlay_root`, and `materialization.env`,
- run-profile fields including system prompts and timeout,
- argv, env, cwd, and diagnostics.

## SDK projection

`project <roster> --sdk ts|py` emits advisory code for
`@anthropic-ai/claude-agent-sdk` or `claude_agent_sdk`.

Runtime behavior:

- success with warning diagnostics exits `0`,
- any error diagnostic exits `Plan` (6),
- JSON shape is `{ "backend", "code", "diagnostics" }`.

Known projection diagnostics include:

- `claude.sdk-projection-plugin`,
- `claude.sdk-projection-hook`,
- `claude.sdk-projection-hook-loss`,
- `claude.sdk-projection-allowed-tools`,
- `claude.sdk-unsupported-backend`.

## Known limitations

| limitation | runtime behavior | exit | JSON shape | coverage |
|---|---|---:|---|---|
| Direct SDK execution | `plan` rejects `sdk-ts`/`sdk-py`; use `project` for advisory code | `6` | plan error envelope or project diagnostics | `katachi_harness_claude::plan::tests::sdk_backend_is_projection_loss_error` |
| `prefer_materialized_cli` | parsed but ignored by planner | `0` | visible only through normal plan/effective-config output | unit coverage around planner materialization |
| Plugin-bundled hooks | travel with copied plugin; not separately serialized as settings hooks | `0` | debug surfaces show selected overlay files only | scanner and plan tests |
| `SymlinkTo` materialization | Unix-only; non-Unix rejects symlink overlay sources | `7` on execute if encountered | execute error envelope | materializer tests |

Primary CLI coverage: `crates/katachi-cli/tests/harness_claude.rs`,
`crates/katachi-cli/tests/per_harness_surfaces.rs`, and
`crates/katachi-cli/tests/have_execute.rs`.
