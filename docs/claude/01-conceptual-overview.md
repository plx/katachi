# Claude Code conceptual overview for katachi

## Summary

Claude Code is the most obviously “loadout-oriented” of the three initial harnesses. It has a rich surface area for:

- plugins
- skills
- subagents
- hooks
- MCP servers
- CLAUDE.md and rules
- output styles
- settings scopes
- CLI and Agent SDK execution paths

For `katachi`, the important lesson is that Claude’s useful reusable unit is often **the plugin**, not the individual skill or subagent in isolation.

## Upstream surfaces that matter

### CLI/native harness

Native Claude Code behavior is shaped by:

- layered settings scopes
- `CLAUDE.md` and rules files
- discovered skills
- discovered subagents
- hooks
- MCP configuration
- plugins
- CLI flags for print mode, output format, settings sources, system prompt changes, plugin dirs, MCP config, agents, permission mode, and session persistence

This is the highest-fidelity backend for a `katachi` prototype.

### SDK surface

Claude’s Agent SDK exists for both TypeScript and Python and exposes the same agent loop in a programmatic form.

Important practical differences:

- the SDK is more explicit and less ambient
- filesystem-based discovery depends on `settingSources` / `setting_sources`
- SDK hooks are callback-oriented, while CLI hooks are file/config oriented
- CLI-oriented skill frontmatter and some plugin behaviors do not map perfectly to SDK use

`katachi` should treat the SDK as a **projection target**, not as proof that the CLI and SDK are equivalent.

## The right roster concept for Claude

A good Claude roster for `katachi` has two layers.

### Layer 1: structural inventory

These are the artifacts that exist.

- plugin
- skill
- subagent
- hook set
- MCP server
- instruction source (`CLAUDE.md`, rules)
- output style

### Layer 2: run profile

These are runtime choices.

- backend (`cli`, `sdk-ts`, `sdk-py`)
- model
- effort
- permission mode
- settings sources
- prompt append/replace behavior
- output format
- session persistence
- materialization mode

A `katachi` should select from both layers.

## Recommended first-class roster items

### 1. Plugin

Plugins are first-class because they are the packaging unit that can bundle:

- skills
- hooks
- subagents
- MCP servers
- other plugin-local assets

In practice, many Claude configurations are not safely representable without explicitly modeling plugins.

### 2. Skill

Skills matter because they are often the main reusable workflow unit.

Relevant properties for `katachi`:

- skill name and description
- scope and origin
- whether it is loose or plugin-packaged
- designated agent for forked execution
- skill-local tool assumptions
- implied MCP affinity

### 3. Subagent

Subagents matter because they:

- define specialized roles
- can preload skills
- can have model/tool settings
- may be CLI-defined or filesystem-defined
- can be packaged in plugins

### 4. Hook set

Hooks matter because they are deterministic and often represent critical workflow behavior such as:

- audit logging
- formatting
- validation
- policy enforcement
- workflow notifications

### 5. MCP server

MCP is a major capability-expansion surface and is often what makes a skill or subagent actually useful.

### 6. Instruction source

`CLAUDE.md`, rules files, and output-style files are not “mere text”. They materially affect behavior and should be represented.

## Key Claude relationships

These relationships are the reason the roster must be a graph, not a flat list.

### Plugin -> contained items

A plugin may contain multiple skills, hooks, subagents, and MCP definitions. This creates a packaging dependency.

### Skill -> agent

A skill can run in a forked context using a designated agent.

### Subagent -> skills

A subagent can preload skills, which means selecting the subagent may imply selecting skills or at least validating their presence.

### Skill/subagent -> MCP affinity

Some skills or agents are effectively designed around a specific MCP server even when the linkage is not strictly encoded.

### Skill/subagent -> hooks

Additional hooks may be introduced by plugin or local configuration that should be active only when that loadout is present.

### Scope precedence

Claude has user/project/local/managed layers and CLI flags on top. Some behavior depends not only on what exists, but where it came from.

## Important constraints for katachi

### Plugin packaging cannot be ignored

If a desired skill is available only via a plugin, `katachi` should not pretend the skill is independently installable.

### SDK projection is not fully faithful

Examples of projection pressure:

- CLI hook files vs SDK callbacks
- ambient discovery vs explicit `settingSources`
- skill frontmatter behavior differences
- plugin-shipped agent restrictions

### Some plugin agent fields are restricted

Plugin-shipped agents have security-driven limitations and do not support every field that a looser configuration surface might allow.

### Search paths should be configurable

The prototype should not hardcode every possible installed-plugin location unless documented and stable. Expose scan roots in `katachi` config.

## Recommended prototype stance

### CLI first

Claude CLI should be the primary backend for the first runnable prototype.

### Plugins first

Make plugins the primary packaging-aware item.

### Support loose artifacts too

Do not require everything to be plugin-packaged. Team-local `.claude/skills/` and `.claude/agents/` matter.

### Model instructions separately from packaging

`CLAUDE.md`, rules, and output styles should be modeled as instruction sources rather than folded into plugins by assumption.

## Proposed Claude roster categories

```text
plugins/
skills/
agents/
hooks/
mcps/
instructions/
output-styles/
profiles/
```

Each category should retain:

- item id
- display name
- source path
- scope
- packaging parent if any
- raw parsed metadata
- edges to related items

## What the Claude katachi should look like

A Claude `katachi` should be:

- a named selection of plugins and/or loose artifacts
- plus an instruction selection
- plus a run profile
- plus a backend preference

Example intent:

- enable plugin `web-a11y`
- include loose project rule `a11y-review.md`
- include loose MCP `chrome-devtools`
- run in CLI print mode
- use permission mode `plan`
- append a system prompt requiring WCAG 2.2 AA reporting

## Suggested v1 scope

First-class in v1:

- plugins
- skills
- subagents
- hooks
- MCP
- instruction sources
- run profile

Deferred but recognized:

- LSP servers
- monitors
- channels
- marketplace management UX

Deferred means:

- the discovery layer may preserve them in raw metadata
- but the resolver does not need deep support on day one

## Practical conclusion

Claude is the clearest example of why `katachi` must model both:

- **semantic dependencies**
- **artificial packaging dependencies**

The correct abstraction is not “a common agent config”. It is “a resolver that understands Claude’s native structure well enough to build faithful headless executions.”

## Upstream references to keep nearby

- Claude Code settings
- Claude CLI reference
- Claude plugins reference
- Claude hooks reference
- Claude skills docs
- Claude subagent docs
- Claude Agent SDK overview / skills / subagents / system prompt docs
