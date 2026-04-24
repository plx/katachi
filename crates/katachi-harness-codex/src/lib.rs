//! Codex harness module for katachi.
//!
//! Implements [`katachi_core::harness::HarnessModule`] for the Codex CLI
//! (and later SDK) backends. Codex is a configuration-centric harness: the
//! roster it produces mixes discrete artifacts (skills, custom agents,
//! hooks, rules, MCP servers, plugins) with configuration layers
//! (`~/.codex/config.toml`, project `.codex/config.toml`, profiles, and
//! the `AGENTS.md` instruction chain).
//!
//! The entry point is [`CodexHarness`], which implements
//! `HarnessModule` and delegates to the submodules below.

pub mod agents;
pub mod cli_flags;
pub mod config_layers;
pub mod discovery;
pub mod effective;
pub mod executor;
pub mod harness;
pub mod hooks;
pub mod items;
pub mod legality;
pub mod materialize;
pub mod mcp;
pub mod planner;
pub mod plugins;
pub mod roster;
pub mod roster_file;
pub mod rules;
pub mod runtime;
pub mod skills;

pub use harness::CodexHarness;
pub use items::{CodexEdgeKind, CodexItemKind};
pub use runtime::CodexSettings;
