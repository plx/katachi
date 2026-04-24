//! Installed-plugin discovery.

use crate::config::ClaudeConfig;
use crate::discovery::ScanState;
use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_plugins(
    _state: &mut ScanState,
    _roots: &DiscoveredRoots,
    _config: &ClaudeConfig,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 6.
    Ok(())
}
