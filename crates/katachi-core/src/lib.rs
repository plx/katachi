//! Shared core library for katachi.
//!
//! This crate owns the cross-harness data model, config loading, storage path
//! discovery, and (in later phases) the roster graph, resolver, and run
//! persistence. Harness crates depend on this crate; the CLI depends on both.

pub mod config;
pub mod paths;
