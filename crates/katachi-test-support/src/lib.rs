//! Test support utilities for katachi.
//!
//! Houses fake harnesses, fixture builders, and other helpers that
//! exercise the shared core without requiring a real harness binary.

pub mod fake;

pub use fake::FakeHarness;
