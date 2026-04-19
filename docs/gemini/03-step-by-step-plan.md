# Gemini step-by-step implementation plan

## Step 1: crate skeleton

Create `katachi-harness-gemini` and wire it into the CLI.

**Done when:** `katachi harness gemini scan` exists as a stub.

## Step 2: settings-layer discovery

Implement loaders for:

- user settings
- project settings
- temp/generated settings overlays

Preserve precedence metadata.

**Done when:** explain output can show active settings layers in order.

## Step 3: context discovery

Discover context files:

- `GEMINI.md`
- alternate context file names from settings
- extension-local context files

**Done when:** context items are visible and explainable.

## Step 4: extension scanner

Parse installed extensions from configured roots.

Read:
- `gemini-extension.json`
- `commands/`
- `hooks/hooks.json`
- `skills/`
- `agents/`
- `policies/`
- theme assets when present

Emit extension containment edges.

**Done when:** at least one realistic extension fixture is fully scanned.

## Step 5: loose skill and subagent discovery

Add support for loose user/workspace skills and subagents.

Keep parsing tolerant and preserve raw metadata.

**Done when:** loose and extension-provided items can coexist in the graph.

## Step 6: hook and MCP modeling

Model:

- hook sets
- MCP servers from settings and extensions

Preserve conflict metadata such as settings-layer precedence over extension MCP of the same name.

**Done when:** explain output shows where a selected MCP/hook came from.

## Step 7: policy/security/admin modeling

Parse the subset of settings relevant to legality:

- extension enable/disable
- MCP enable/disable
- approval/security restrictions
- allowed/blocklisted extensions
- preview/experimental flags for selected capabilities

**Done when:** illegal loadouts can be rejected during planning.

## Step 8: roster file parser

Add support for `rosters/gemini/*.toml`.

Parse:
- selection
- run profile
- resolution settings

**Done when:** a Gemini roster can be loaded and matched to discovered items.

## Step 9: resolver and validator

Implement:

- extension packaging closure
- context/MCP/policy closure
- preview feature checks
- backend projection checks

**Done when:** `katachi harness gemini plan <roster-id>` returns a resolved selection plus warnings/errors.

## Step 10: CLI planner

Implement headless CLI projection:

- output format selection
- model selection
- approval mode
- extension selection / disable-all behavior
- include-directories
- temp settings generation

**Done when:** dry-run output prints a full execution plan.

## Step 11: temp overlay materializer

Build a temp Gemini environment with:

- temp home `.gemini/`
- temp project `.gemini/`
- selected extensions
- generated settings
- generated or copied context files

**Done when:** the materializer produces a reproducible manifest.

## Step 12: CLI executor and transcript adapter

Run Gemini headlessly and parse:

- raw stdout/stderr
- `stream-json` events
- final result and stats

**Done when:** a real or stub Gemini run yields an `ExecutionRecord`.

## Step 13: TypeScript SDK projection

Add the `sdk-ts` projection for the supported subset only.

Explicitly reject unsupported features such as extensions, hooks, subagents, and policies.

**Done when:** one simple roster can be projected to the SDK or rejected with a projection diagnostic.

## Step 14: operator commands

Finish:

- `explain`
- `graph`
- `doctor`
- `dump-settings`

**Done when:** an operator can understand what the Gemini module discovered and why a plan was formed.

## Step 15: test matrix

Add tests for:

- extension containment
- settings overrides
- MCP name conflicts
- admin/security restrictions
- preview feature requirements
- unsupported SDK projection
- stream-json transcript parsing

**Done when:** the core Gemini-specific behaviors have fixture coverage.

## Final Gemini milestone

The Gemini milestone is complete when this works:

```bash
katachi harness gemini scan
katachi harness gemini explain extension:workspace-a11y
katachi harness gemini plan accessibility-auditor execute "Audit this repo"
katachi harness gemini execute accessibility-auditor "Audit this repo"
```
