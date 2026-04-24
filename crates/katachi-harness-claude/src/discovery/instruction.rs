//! Instruction-source discovery (`CLAUDE.md` + `.claude/rules/*.md`).

use crate::discovery::ScanState;
use crate::error::ClaudeDiscoveryError;
use crate::paths::DiscoveredRoots;

pub fn scan_instructions(
    _state: &mut ScanState,
    _roots: &DiscoveredRoots,
) -> Result<(), ClaudeDiscoveryError> {
    // Filled in during Step 5.
    Ok(())
}
