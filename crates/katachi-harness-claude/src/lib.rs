//! Claude harness module for katachi.
//!
//! Implements [`katachi_core::harness::HarnessModule`] for the Claude CLI
//! and SDK backends.
//!
//! Public entry points of interest:
//!
//! - [`module::ClaudeHarness`] — the `HarnessModule` implementation
//! - [`discovery::scan`] — artifact discovery
//! - [`roster::ClaudeRoster`] — the Claude-specific roster file format
//! - [`plan::build_plan`] — translate a resolved katachi into an execution plan
//!
//! Most callers should only ever touch [`ClaudeHarness`].

pub mod config;
pub mod discovery;
pub mod error;
pub mod explain;
pub mod frontmatter;
pub mod item;
pub mod module;
pub mod paths;
pub mod plan;
pub mod roster;
pub mod transcript;

pub use module::ClaudeHarness;
