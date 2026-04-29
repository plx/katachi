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
use katachi_core::harness::{DependencyEdge, EdgeKind, RosterCatalog};
use katachi_core::model::{HarnessKind, ItemRef};

use crate::config::ClaudeConfig;
use crate::error::ClaudeDiscoveryError;
use crate::item::{ClaudeEdgeLabel, ClaudeItemKind};
use crate::paths::{discover_roots, DiscoveredRoots};

pub mod agent;
pub mod hooks;
pub mod instruction;
pub mod mcp;
pub mod plugin;
pub mod skill;

/// Run the full Claude scan against the given scan context.
pub fn scan(ctx: &katachi_core::harness::ScanContext<'_>) -> Result<RosterCatalog, ClaudeDiscoveryError> {
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
    let mut state = ScanState::new();

    plugin::scan_plugins(&mut state, roots, config)?;
    skill::scan_loose_skills(&mut state, roots)?;
    agent::scan_loose_agents(&mut state, roots)?;
    instruction::scan_instructions(&mut state, roots)?;
    hooks::scan_hooks(&mut state, roots)?;
    mcp::scan_mcp(&mut state, roots)?;

    Ok(state.finalize())
}

/// Mutable scan state shared by the sub-scanners.
///
/// Sub-scanners append items to `catalog` immediately but queue
/// semantic/packaging edges in `pending_edges`. Only once every item has
/// been discovered does [`ScanState::finalize`] resolve the edges,
/// emitting them when the target exists and adding
/// `claude.*-missing-target` diagnostics otherwise. This matters because
/// a skill may reference an agent whose `.md` file is scanned later, or
/// vice versa.
pub struct ScanState {
    pub catalog: RosterCatalog,
    pub pending_edges: Vec<PendingEdge>,
}

impl Default for ScanState {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanState {
    pub fn new() -> Self {
        Self {
            catalog: RosterCatalog::empty(HarnessKind::Claude),
            pending_edges: Vec::new(),
        }
    }

    /// Resolve every pending edge against the completed catalog and
    /// return it. Dangling targets turn into diagnostics keyed by the
    /// pending edge's `from_kind`.
    pub fn finalize(mut self) -> RosterCatalog {
        for pending in std::mem::take(&mut self.pending_edges) {
            let target = ItemRef::new(
                HarnessKind::Claude,
                pending.target_kind.as_str(),
                pending.target_id.clone(),
            );
            if self.catalog.contains(&target) {
                let edge = DependencyEdge {
                    from: pending.from.clone(),
                    to: target,
                    kind: pending.edge_kind,
                    required: pending.required,
                    note: Some(pending.label.as_str().to_string()),
                };
                if let Err(err) = self.catalog.insert_edge(edge) {
                    push_warning(
                        &mut self.catalog,
                        "claude.duplicate-edge",
                        format!("ignoring duplicate edge: {err}"),
                    );
                }
            } else {
                push_warning(
                    &mut self.catalog,
                    &pending.missing_code,
                    format!(
                        "{from_kind} `{from}` references missing {target_kind} `{id}` ({label})",
                        from_kind = pending.from.kind,
                        from = pending.from,
                        target_kind = pending.target_kind.as_str(),
                        id = pending.target_id,
                        label = pending.label.as_str()
                    ),
                );
            }
        }
        self.catalog
    }
}

/// An edge produced during scanning whose target may not yet exist.
#[derive(Clone, Debug)]
pub struct PendingEdge {
    pub from: ItemRef,
    pub target_kind: ClaudeItemKind,
    pub target_id: String,
    pub edge_kind: EdgeKind,
    pub label: ClaudeEdgeLabel,
    pub required: bool,
    /// Diagnostic code used when the target is missing.
    pub missing_code: String,
}

/// Helper used by sub-scanners: push a warning diagnostic into the catalog.
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
    use crate::paths::{ClaudeDir, ClaudeScope, DiscoveredRoots};
    use camino::Utf8PathBuf;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scan_from_empty_roots_yields_empty_catalog() {
        let roots = DiscoveredRoots::empty();
        let config = ClaudeConfig::default();
        let catalog = scan_from_roots(&roots, &config).unwrap();
        assert!(catalog.items.is_empty());
        assert!(catalog.edges.is_empty());
        assert_eq!(catalog.harness, Some(HarnessKind::Claude));
    }

    /// Two-way forward reference: skill references agent, agent preloads
    /// skill. Both files exist on disk, so regardless of scan order every
    /// edge should be wired up in the final catalog.
    #[test]
    fn forward_refs_between_skill_and_agent_resolve() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".claude/skills/axe")).unwrap();
        fs::create_dir_all(root.join(".claude/agents")).unwrap();
        fs::write(
            root.join(".claude/skills/axe/SKILL.md"),
            "---\nname: axe\nagent: reviewer\n---\n",
        )
        .unwrap();
        fs::write(
            root.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\npreloaded_skills: [axe]\n---\n",
        )
        .unwrap();

        let roots = DiscoveredRoots {
            claude_dirs: vec![ClaudeDir {
                path: root.join(".claude"),
                scope: ClaudeScope::Project,
            }],
            top_level_claude_mds: Vec::new(),
            plugin_roots: Vec::new(),
        };
        let config = ClaudeConfig::default();
        let catalog = scan_from_roots(&roots, &config).unwrap();

        let notes: std::collections::BTreeSet<_> = catalog
            .edges
            .iter()
            .filter_map(|e| e.note.clone())
            .collect();
        assert!(notes.contains("skill_uses_agent"));
        assert!(notes.contains("agent_preloads_skill"));
        assert_eq!(catalog.items.len(), 2);
    }
}
