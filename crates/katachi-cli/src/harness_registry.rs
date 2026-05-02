//! CLI-level registry of `HarnessModule` instances available to
//! cross-harness commands like `katachi have ...`.
//!
//! `from_config` builds the set of *real* harness modules that are
//! enabled in the loaded config. Missing or disabled harnesses are
//! omitted. `with_fixtures_from_env` appends fixture harnesses when
//! `KATACHI_FIXTURE_HARNESSES` is set, so legacy integration tests
//! continue to work.

use katachi_core::config::KatachiConfig;
use katachi_core::harness::HarnessModule;

use katachi_harness_claude::ClaudeHarness;
use katachi_harness_codex::CodexHarness;
use katachi_harness_gemini::GeminiHarness;

use crate::fixtures;

/// Collection of harness modules used by `have` resolution.
pub struct HarnessRegistry {
    modules: Vec<Box<dyn HarnessModule>>,
}

impl HarnessRegistry {
    /// Construct from the parsed config: every harness section that is
    /// `enabled = true` (or omits the field) becomes available.
    pub fn from_config(config: &KatachiConfig) -> Self {
        let mut modules: Vec<Box<dyn HarnessModule>> = Vec::new();
        if harness_enabled(config, "claude") {
            modules.push(Box::new(ClaudeHarness::new()));
        }
        if harness_enabled(config, "codex") {
            modules.push(Box::new(CodexHarness::new()));
        }
        if harness_enabled(config, "gemini") {
            modules.push(Box::new(GeminiHarness::new()));
        }
        Self { modules }
    }

    /// Prepend fixture harnesses requested via `KATACHI_FIXTURE_HARNESSES`.
    /// Fixtures shadow real modules of the same kind because the resolver
    /// picks the first matching harness for a target, and tests rely on
    /// fixture catalogs deterministically replacing scan output.
    pub fn with_fixtures_from_env(mut self) -> Self {
        let fixture_modules = fixtures::load_from_env();
        if fixture_modules.is_empty() {
            return self;
        }
        let mut combined: Vec<Box<dyn HarnessModule>> = fixture_modules;
        combined.append(&mut self.modules);
        self.modules = combined;
        self
    }

    /// Borrow the modules as trait references for [`ResolveInputs`].
    pub fn as_refs(&self) -> Vec<&dyn HarnessModule> {
        self.modules.iter().map(|m| m.as_ref()).collect()
    }
}

fn harness_enabled(config: &KatachiConfig, name: &str) -> bool {
    match config.harnesses.get(name) {
        Some(cfg) => cfg.enabled,
        None => true,
    }
}
