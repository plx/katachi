# Gemini conceptual overview for katachi

## Summary

Gemini CLI is the most **extension-centric** of the three initial harnesses.

Its meaningful reusable surfaces include:

- layered `settings.json`
- context files (`GEMINI.md` or custom names)
- skills
- subagents
- hooks
- MCP servers
- extensions
- policies/admin/security settings
- browser-agent settings
- headless CLI mode
- a TypeScript CLI SDK with a narrower feature surface

For `katachi`, the central lesson is:

> Gemini extensions are not just packaging. They are the place where many meaningful behaviors are bundled and distributed together.

## Upstream surfaces that matter

### Native CLI harness

Gemini CLI behavior depends on:

- layered settings from defaults through user/project/system/env/CLI args
- context discovery and `context.fileName`
- headless output formats (`text`, `json`, `stream-json`)
- skills
- subagents
- hooks
- MCP configuration
- extensions
- policy/security/admin settings
- browser-agent configuration
- experimental feature flags

This is the main backend for the first `katachi` prototype.

### SDK surface

Gemini CLI currently has an official TypeScript SDK (`@google/gemini-cli-sdk`).

Important limitations:

- core agent loop is implemented
- tools and skills are supported
- hooks, subagents, extensions, policies, and ACP are explicitly not implemented in the public SDK design yet
- there is no official Python Gemini CLI SDK yet

So for `katachi`, the Gemini SDK is a **subset projection**, not a peer of the CLI.

## The right roster concept for Gemini

A good Gemini roster should be **extension-first**.

Recommended first-class categories:

- extension
- context source
- skill
- subagent
- hook set
- MCP server
- policy set
- run profile

Secondary but preserved:

- custom commands
- themes
- browser-agent settings

Those secondary items often arrive through extensions and may remain extension-scoped in v1.

## Why extensions are first-class

Gemini extensions can bundle:

- extension-local context
- MCP servers
- install-time settings
- custom commands
- hooks
- skills
- subagents
- policies
- themes

That makes them the single most important packaging boundary in Gemini CLI.

## Key Gemini relationships

### Extension -> contained items

Extensions package many of the items that matter at runtime.

### Settings -> extension precedence

Workspace/user settings can override extension-provided behavior, and settings-defined MCP servers take precedence over same-named extension MCP servers.

### Extension -> policy tier

Extension policies run in a specific policy tier and cannot silently grant dangerous approvals.

### Skill -> files/resources

Skills bring in specialized instructions and may imply file/resource access patterns.

### Subagent -> isolated context and tools

Subagents are separate specialists with their own tools and instructions. They are valuable, but still relatively young and preview-oriented.

### Hooks -> workflow behavior

Hooks are part of real automation, but the exact schema has been evolving. `katachi` should model them carefully and not over-promise schema stability.

### Security/admin -> legality constraints

Gemini settings can disable:

- extensions entirely
- MCP usage
- dangerous approval modes
- certain extension sources via allowlists/blocklists

A requested loadout may therefore be illegal even if semantically complete.

## Recommended Gemini roster categories

```text
extensions/
context/
skills/
subagents/
hooks/
mcps/
policies/
profiles/
```

The “profile” category can remain mostly a `katachi`-side concept in v1, represented as a run profile rather than an upstream Gemini artifact.

## What a Gemini katachi should represent

A Gemini `katachi` should be:

- a selected set of extensions and/or loose artifacts
- a context selection (`GEMINI.md` family)
- selected skills/subagents/hooks/MCP/policies
- a run profile with approval mode, model, include directories, and output format
- a backend preference, usually CLI

## Important constraints for katachi

### CLI is much broader than the SDK

If a Gemini loadout depends on:

- extensions
- hooks
- subagents
- policies

then the CLI backend is the correct answer for the prototype.

### Some docs are still moving

Hooks and some preview areas have changed quickly. The implementation should be schema-tolerant and keep raw payloads.

### Preview feature gating matters

Some features, especially subagents, may depend on experimental settings or model selection.

### Search roots should be configurable

Extension and skill locations should be configurable rather than over-hardcoded.

## Recommended prototype stance

### CLI first

Headless Gemini CLI should be the primary backend.

### Extension first

Extensions should be the primary packaging-aware item.

### Preserve loose artifacts too

Support loose workspace/user skills, subagents, hooks, and context files where visible.

### Treat SDK as subset projection

The SDK path should explicitly reject unsupported features rather than trying to emulate them incompletely.

## Suggested v1 scope

First-class in v1:

- extensions
- context files
- skills
- subagents
- hooks
- MCP
- policies
- run profile

Secondary but preserved in raw metadata:

- custom commands
- themes
- browser-agent fine tuning

## Practical conclusion

Gemini is the clearest case for keeping the shared `katachi` abstraction minimal and the harness module rich.

The correct design is not:

> “Convert extensions into a generic set of common plugin objects.”

It is:

> “Understand that an extension is the native Gemini packaging boundary, then resolve and project accordingly.”

## Upstream references to keep nearby

- Gemini CLI configuration reference
- Gemini headless mode reference
- Gemini extensions docs and extension reference
- Gemini MCP docs
- Gemini subagent docs
- Gemini CLI SDK README and SDK design notes
