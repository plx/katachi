//! Claude artifact discovery.
//!
//! The top-level [`scan`] function walks the configured roots, parses loose
//! artifacts and plugin-packaged artifacts, and returns a populated
//! [`RosterCatalog`] containing [`DiscoveredItem`]s plus semantic and
//! packaging edges.
//!
//! Callers typically go through [`crate::module::ClaudeHarness`], which
//! wraps this function behind [`HarnessModule::scan`].

use katachi_core::diagnostic::Diagnostic;
use katachi_core::harness::{RosterCatalog, ScanContext};
use katachi_core::model::HarnessKind;

use crate::config::ClaudeConfig;
use crate::error::ClaudeDiscoveryError;
use crate::paths::{discover_roots, DiscoveredRoots};

pub mod agent;
pub mod hooks;
pub mod instruction;
pub mod mcp;
pub mod plugin;
pub mod skill;

/// Run the full Claude scan against the given scan context.
pub fn scan(ctx: &ScanContext<'_>) -> Result<RosterCatalog, ClaudeDiscoveryError> {
    let config = ClaudeConfig::from_shared(ctx.config);
    let roots = discover_roots(ctx.cwd, &config);
    scan_from_roots(&roots, &config)
}

/// Scan against a concrete set of discovered roots. Split out so tests can
/// construct bespoke roots without needing a full `ScanContext`.
pub fn scan_from_roots(
    roots: &DiscoveredRoots,
    config: &ClaudeConfig,
) -> Result<RosterCatalog, ClaudeDiscoveryError> {
    let mut catalog = RosterCatalog::empty(HarnessKind::Claude);

    // Order matters only for the diagnostic stream: plugins first so their
    // packaged items are present before loose items reference them.
    plugin::scan_plugins(&mut catalog, roots, config)?;
    skill::scan_loose_skills(&mut catalog, roots)?;
    agent::scan_loose_agents(&mut catalog, roots)?;
    instruction::scan_instructions(&mut catalog, roots)?;
    hooks::scan_hooks(&mut catalog, roots)?;
    mcp::scan_mcp(&mut catalog, roots)?;

    Ok(catalog)
}

/// Helper used by sub-scanners: push a warning diagnostic into the catalog.
#[allow(dead_code)]
pub(crate) fn push_warning(
    catalog: &mut RosterCatalog,
    code: &str,
    message: impl Into<String>,
) {
    catalog.diagnostics.push(Diagnostic::warning(code, message));
}

/// Helper used by sub-scanners: push an info diagnostic into the catalog.
#[allow(dead_code)]
pub(crate) fn push_info(
    catalog: &mut RosterCatalog,
    code: &str,
    message: impl Into<String>,
) {
    catalog.diagnostics.push(Diagnostic::info(code, message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::DiscoveredRoots;

    #[test]
    fn scan_from_empty_roots_yields_empty_catalog() {
        let roots = DiscoveredRoots::empty();
        let config = ClaudeConfig::default();
        let catalog = scan_from_roots(&roots, &config).unwrap();
        assert!(catalog.items.is_empty());
        assert!(catalog.edges.is_empty());
        assert_eq!(catalog.harness, Some(HarnessKind::Claude));
    }
}
