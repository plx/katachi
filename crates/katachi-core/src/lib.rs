//! Shared core library for katachi.
//!
//! This crate owns the cross-harness data model, config loading, storage path
//! discovery, and (in later phases) the roster graph, resolver, and run
//! persistence. Harness crates depend on this crate; the CLI depends on both.

pub mod config;
pub mod diagnostic;
pub mod error;
pub mod execute;
pub mod graph;
pub mod harness;
pub mod katachi;
pub mod materialize;
pub mod model;
pub mod paths;
pub mod persist;
pub mod plan;
pub mod record;
pub mod resolve;
pub mod roster;
pub mod selector;
pub mod transcript;
pub mod validate;
