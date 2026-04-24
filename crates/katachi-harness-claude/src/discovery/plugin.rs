//! Installed-plugin discovery.

use katachi_core::harness::RosterCatalog;

use crate::config::ClaudeConfig;
use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_plugins(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
    _config: &ClaudeConfig,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 6.
    Ok(())
}
