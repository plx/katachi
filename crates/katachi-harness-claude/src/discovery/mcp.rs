//! MCP server discovery from settings fragments and plugin manifests.

use katachi_core::harness::RosterCatalog;

use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_mcp(
    _catalog: &mut RosterCatalog,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 7.
    Ok(())
}
