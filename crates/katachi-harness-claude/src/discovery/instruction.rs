//! Instruction-source discovery (`CLAUDE.md` + `.claude/rules/*.md`).

use katachi_core::harness::RosterCatalog;

use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_instructions(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 5.
    Ok(())
}
