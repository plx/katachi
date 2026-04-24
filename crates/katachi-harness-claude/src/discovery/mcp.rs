//! MCP server discovery from settings fragments and plugin manifests.

use crate::discovery::ScanState;
use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_mcp(
    _state: &mut ScanState,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 7.
    Ok(())
}
