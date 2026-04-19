# Codex conceptual overview for katachi

## Summary

Codex is less “plugin-centric” than Claude and more **configuration-centric**. Its behavior is spread across:

- `config.toml` layers
- profiles
- `AGENTS.md`
- skills
- custom agents / subagents
- hooks
- rules
- MCP
- plugins
- managed `requirements.toml`
- CLI, SDK, app-server, and MCP-server execution surfaces

For `katachi`, this means a Codex roster must model **effective configuration layering**, not just discrete packaged artifacts.

## Upstream surfaces that matter

### Native harness

Codex’s local harness behavior depends on:

- user/project/system config layers
- trusted-project loading rules
- profile selection
- `AGENTS.md` discovery from global through project path
- rules directories
- `hooks.json` files
- skills
- custom agents
- MCP servers
- plugin marketplaces/installations
- requirements and enterprise constraints

### SDK surface

Codex currently exposes:

- TypeScript SDK (`@openai/codex-sdk`)
- an experimental Python SDK that drives the app-server

The TypeScript SDK is strong for threads, streaming, structured outputs, and local control, but much of Codex’s customization still lives in config/home/project layers rather than a purely typed SDK object model.

`katachi` should therefore treat Codex as a harness where **materialized filesystem/config overlays** are often the right implementation technique.

## The right roster concept for Codex

Codex needs a roster that mixes “artifact inventory” with “config layer inventory.”

Recommended first-class categories:

- config/profile layer
- instruction document (`AGENTS.md`)
- skill
- custom agent
- hook set
- MCP server
- rule file / rule set
- plugin
- requirement policy
- run profile

## Why config layers are first-class

In Codex, the same skill or agent behaves differently depending on:

- which `config.toml` layers are active
- whether the project is trusted
- which profile is selected
- which `AGENTS.md` files were loaded
- which rules and hooks are in scope
- whether enterprise requirements ban the requested sandbox or approval policy

A flat “skills/plugins only” view is not sufficient.

## Key Codex relationships

### Config/profile -> effective behavior

Profiles and config layers are not passive metadata. They change:

- model
- provider
- approval policy
- sandbox mode
- MCP definitions
- feature flags
- agent behavior

### AGENTS.md -> instruction chain

Codex builds an instruction chain from global and project documents. Order matters.

### Skill -> MCP dependency

Skills can declare metadata and dependencies that imply MCP availability.

### Custom agent -> inherited config

A custom agent may carry its own config and also inherit from the enclosing session.

### Hooks -> additive merge

Codex hooks are notable because matching hooks from multiple layers all run; higher precedence does not simply replace lower precedence hooks.

### Rules / requirements -> legality constraints

A loadout may be semantically complete but still disallowed because:

- approval policy is not permitted
- sandbox mode is not permitted
- specific rules forbid dangerous shell behavior

### Plugin -> packaging boundary

Plugins are the installable/shareable unit for reusable skills and integrations.

## Recommended Codex roster categories

```text
config-layers/
profiles/
instructions/
skills/
agents/
hooks/
mcps/
rules/
plugins/
requirements/
```

## What a Codex katachi should represent

A Codex `katachi` should be thought of as:

- a selected instruction chain
- a selected config/profile overlay
- selected skills/custom agents
- selected MCP and hooks
- selected rules/policies
- selected plugins if needed
- a concrete run profile for headless execution

In other words, a Codex katachi is closer to a **curated effective Team Config** than to a simple list of plugin assets.

## Important constraints for katachi

### Trust matters

Project `.codex/` config only loads when the project is trusted. `katachi` must account for this explicitly.

### Profiles are not universally supported across surfaces

For example, some profile behavior is CLI-local and does not map one-to-one to every other Codex surface.

### Python SDK is more experimental

Treat the Python SDK as opt-in and lower priority than CLI and TypeScript SDK support.

### App-server and MCP-server are distinct surfaces

They are valuable, but they are not the same as the local SDK or simple `exec` automation path.

## Recommended prototype stance

### CLI first

Use the local Codex CLI as the primary prototype backend.

### Materialized `CODEX_HOME` second to none

For reproducibility, a temp `CODEX_HOME` plus generated project overlays are likely the most reliable strategy.

### Keep app-server as a future adapter boundary

Do not build the first prototype around app-server, but do not prevent that adapter later.

### Make legality validation first-class

Codex loadouts must be checked for:

- trust applicability
- requirements constraints
- rule conflicts
- unsupported backend projections

## Suggested v1 scope

First-class in v1:

- profiles/config layers
- `AGENTS.md`
- skills
- custom agents
- hooks
- MCP
- rules
- plugins
- run profile

Secondary but preserved:

- app-server details
- MCP-server orchestration mode
- cloud/web-specific behavior

## Practical conclusion

Codex is the harness where `katachi` must most strongly understand **effective configuration assembly**.

The core abstraction is:

> “Build the right effective local Codex environment, then run the job.”

not:

> “Select a few standalone reusable items and hope the harness figures out the rest.”

## Upstream references to keep nearby

- Codex config basics
- Codex config reference
- Codex advanced config / profiles
- AGENTS.md docs
- skills docs
- subagents/custom agents docs
- hooks docs
- rules docs
- MCP docs
- plugins docs
- Codex SDK docs
- app-server docs
- managed configuration / requirements docs
