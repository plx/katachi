//! Hook-set discovery from settings layers and plugin hook manifests.

use katachi_core::harness::RosterCatalog;

use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_hooks(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 7.
    Ok(())
}
