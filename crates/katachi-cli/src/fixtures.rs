//! Test-only fixture harness hatch.
//!
//! When the `KATACHI_FIXTURE_HARNESSES` env var is set to a comma-separated
//! list of fixture names, the CLI instantiates the corresponding
//! [`katachi_test_support::FixtureHarness`] modules and uses them in place
//! of real harness plugins. This is how Phase 2 integration tests drive
//! `katachi have <id> describe` without a real Claude/Codex/Gemini install.
//!
//! Real harness wiring lands in Phase 3; this module stays but becomes
//! strictly opt-in behind the same env var.

use katachi_core::harness::HarnessModule;
use katachi_test_support::FixtureHarness;

pub const ENV_VAR: &str = "KATACHI_FIXTURE_HARNESSES";

/// Parse [`ENV_VAR`] and instantiate the requested fixture harnesses.
/// Unknown names are skipped with a stderr warning.
pub fn load_from_env() -> Vec<Box<dyn HarnessModule>> {
    let Ok(raw) = std::env::var(ENV_VAR) else {
        return Vec::new();
    };
    let mut out: Vec<Box<dyn HarnessModule>> = Vec::new();
    for name in raw.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        match name {
            "toy_claude" => out.push(Box::new(FixtureHarness::toy_claude())),
            "toy_claude_cycle" => out.push(Box::new(FixtureHarness::toy_with_cycle())),
            other => {
                eprintln!("katachi: unknown fixture harness `{other}` — skipping");
            }
        }
    }
    out
}
