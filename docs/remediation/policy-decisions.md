# Locked policy decisions

This document captures the contracts the CLI must follow across harnesses,
referenced by the remediation plans (`01-…` through `07-…`). The plans
implement these decisions; this file is the canonical statement of
intent. When implementation changes, update this file too.

## 1. Dry-run

A `--dry-run` invocation:

1. Must not spawn a child process.
2. Must not create a run directory under `<data_root>/runs/`.
3. May perform read-only discovery, resolution, validation, and
   planning.
4. With `--json`, emits the same plan-report shape as the corresponding
   `plan` command for the same harness.

Direct harness `execute --dry-run` therefore behaves identically to
`plan ... execute` for the human/JSON output, with the additional
guarantee that no child runs and no run directory exists afterward.

## 2. Failed-run persistence

The contract is: **commit only on success; failed runs remain
`<run-id>.partial/`**.

1. On successful child outcome (`Outcome::Success`), the run is
   committed via `RunDirectory::commit()`.
2. On any non-success outcome (`Failure`, `Timeout`) or executor error,
   the partial directory is preserved for post-mortem.
3. `manifest.json` is written before commit/abandonment so the partial
   has a manifest where possible.
4. `Outcome::Planned` is reserved for plan-only paths and never touches
   the runs directory (see §1).
5. Persistence errors (writing manifest, commit failure on success) map
   to `ExitCode::Execute (7)` and surface the partial path on stderr.

## 3. Backend precedence

Resolution order, highest precedence first:

1. Hard pin from a katachi target (`KatachiTarget::backend`) or roster
   (`run_profile.backend`).
2. `--prefer-backend` from the invocation.
3. Harness config default (`default_backend`).
4. Built-in default `BackendKind::Cli`.

Notes:

- `--prefer-backend` may be passed more than once; the first one parsed
  wins among CLI overrides.
- An unknown backend string in a roster is a config-level error
  (`ExitCode::Validate (5)` for direct commands, `ExitCode::Plan (6)`
  for projection-blocking failures), not a silent fall-through to
  `cli`.
- `--prefer-harness` only steers selection among multiple targets in a
  cross-harness `have` invocation; direct `katachi harness <name> ...`
  ignores it.

## 4. Materialization precedence

Resolution order, highest precedence first:

1. `--materialization` from the invocation.
2. Roster resolution settings.
3. Shared default (`MaterializationMode::TempOverlay`).

## 5. Duplicate-id behavior

1. Duplicate katachi definition ids under `katachis/` are an error.
2. Duplicate roster ids within a harness roster directory are an error.
3. Diagnostics include both source paths where possible.
4. CLI maps duplicate-id errors to `ExitCode::Config (3)` for store
   loading; commands resolving a named item use `ExitCode::Resolve (4)`
   when the duplicate masks the requested id.

## 6. `roster_id` semantics

1. `roster_id` is *optional* on a katachi target. Empty/whitespace is
   rejected with a clear error.
2. When present, the target loads the harness-native roster for its
   selectors and run profile.
3. `roster_id` plus explicit selectors in the same target is rejected
   until merge semantics are designed (initial guard).
4. Missing roster is `ExitCode::Resolve (4)`.
5. Target-level fields override roster fields in this order:
   1. target `backend` overrides roster `run_profile.backend`.
   2. target `run_profile_overlay` field-by-field overrides the
      roster's run profile.

## 7. Stubbed JSON output

When the user passes `--json` and invokes a not-yet-implemented
subcommand, the CLI must emit a structured error object on stdout:

```json
{
  "error": {
    "kind": "not_implemented",
    "command": "<label>",
    "message": "`<label>` is not yet implemented in this phase"
  }
}
```

Plain text fallback (no `--json`) remains:

```text
katachi: `<label>` is not yet implemented in this phase
```

In both cases the exit code is `ExitCode::NotImplemented (64)`.
