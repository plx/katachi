//! Hook-set discovery from settings layers and plugin hook manifests.

use crate::discovery::ScanState;
use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_hooks(
    _state: &mut ScanState,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 7.
    Ok(())
}
