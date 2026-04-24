//! Loose agent discovery under `<root>/agents/*.md`.

use katachi_core::harness::RosterCatalog;

use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_loose_agents(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 4.
    Ok(())
}
