//! Loose skill discovery under `<root>/skills/<name>/SKILL.md`.
//!
//! A loose skill is any `SKILL.md` that lives under a scoped `.claude/`
//! directory (user or project) rather than inside a plugin bundle. Each
//! skill emits a `ClaudeItemKind::Skill` and — when its frontmatter names
//! an `agent` — a semantic edge from the skill to that agent.

use katachi_core::harness::RosterCatalog;

use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

/// Entry point invoked from [`super::scan_from_roots`].
pub fn scan_loose_skills(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 3.
    Ok(())
}
