//! Gemini harness module for katachi.
//!
//! Implements [`katachi_core::harness::HarnessModule`] for the Gemini CLI
//! and (partially) TypeScript SDK backends. Gemini's native packaging
//! boundary is an *extension*, not a plugin, so extension-first scanning
//! drives the catalog.

pub mod config;
pub mod context;
pub mod execute;
pub mod extension;
pub mod harness;
pub mod hook;
pub mod item;
pub mod materialize;
pub mod mcp;
pub mod plan;
pub mod policy;
pub mod roster;
pub mod scan;
pub mod settings;
pub mod skill;
pub mod subagent;
pub mod transcript;
pub mod validate;

pub use harness::GeminiHarness;
pub use item::{GeminiEdgeKind, GeminiItemKind};
