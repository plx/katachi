# Claude Code step-by-step implementation plan

## Step 1: crate skeleton

Create `katachi-harness-claude` with:

- public module entry point
- harness-specific enums and structs
- empty trait implementation wired into `katachi-cli`

**Done when:** `katachi harness claude scan` exists and returns a stub result.

## Step 2: path and scope discovery

Implement helpers to locate:

- user `.claude/`
- project `.claude/`
- `CLAUDE.md`
- `.claude/rules/`
- configured plugin roots

Persist scope metadata on discovered paths.

**Done when:** a fixture repo returns the expected discovered roots.

## Step 3: loose skill parser

Parse `SKILL.md` directories:

- frontmatter
- description
- context mode
- designated agent
- raw markdown body
- source scope

Emit `Skill` items plus edge stubs for agent/MCP references.

**Done when:** snapshot tests show parsed skills from user and project scopes.

## Step 4: loose agent parser

Parse `.claude/agents/*.md`:

- frontmatter
- description
- model / effort / tools
- preloaded skills
- MCP references if any

Emit `Agent` items and `AgentPreloadsSkill` edges.

**Done when:** fixture agents are graph-connected to skills.

## Step 5: instruction discovery

Parse:

- user `CLAUDE.md`
- project/root `CLAUDE.md`
- `.claude/rules/*.md`

Emit `InstructionSource` items with precedence metadata.

**Done when:** the explain output shows the instruction chain in order.

## Step 6: plugin scanner

Parse configured plugin roots and each plugin’s `plugin.json` plus conventional subdirs.

Emit:

- `Plugin`
- contained skills
- contained agents
- contained hooks
- contained MCP defs
- contained output styles

Emit `Contains` edges.

**Done when:** one realistic plugin fixture expands into a graph correctly.

## Step 7: hook and MCP modeling

Add `HookSet` and `McpServer` items.

Preserve raw payloads even when full deep parsing is deferred.

**Done when:** selected hooks and MCP items appear in the graph and in explain output.

## Step 8: roster file parser

Support Claude roster files under `rosters/claude/`.

Implement parsing for:

- `selection`
- `run_profile`
- `resolution`

**Done when:** a Claude roster can be loaded and linked to discovered item ids.

## Step 9: resolver and validator

Implement:

- packaging closure
- skill -> agent closure
- agent -> skill closure
- MCP closure
- backend validation
- missing-item diagnostics

**Done when:** `katachi harness claude plan <roster-id>` returns a resolved selection and warnings.

## Step 10: CLI planner

Implement a concrete CLI projection:

- select command/flags
- create temp overlay when requested
- write settings and MCP fragments
- select plugin dirs
- choose system prompt behavior

**Done when:** dry-run output prints a complete execution plan.

## Step 11: CLI executor

Spawn `claude` and capture:

- stdout
- stderr
- exit code
- structured events where possible

**Done when:** a real or stub Claude binary can be run end-to-end.

## Step 12: transcript adapter

Build a tolerant parser for Claude print-mode output.

Store:

- raw lines
- normalized events
- final result extraction

**Done when:** run records include both raw and normalized transcript data.

## Step 13: SDK projection

Add `sdk-ts` and `sdk-py` planners behind explicit backend selection.

Start with:

- skills via `settingSources`
- agents via programmatic definitions or filesystem roots
- system prompt selection
- limited hook mapping

**Done when:** at least one simple roster can be projected to each SDK backend, or a clear projection diagnostic is emitted.

## Step 14: operator commands

Finish:

- `explain`
- `graph`
- `doctor`
- `dump-roster`

**Done when:** an operator can inspect discovered items and understand why a plan was formed.

## Step 15: test matrix

Add tests for:

- loose artifacts only
- plugin-packaged artifacts
- mixed loose + plugin
- SDK projection loss
- missing agent / missing MCP
- scope precedence

**Done when:** the module has fixture coverage for every important edge type.

## Final Claude milestone

The Claude milestone is complete when this works:

```bash
katachi harness claude scan
katachi harness claude explain plugin:web-a11y
katachi harness claude plan accessibility-auditor execute "Audit this repo"
katachi harness claude execute accessibility-auditor "Audit this repo"
```
