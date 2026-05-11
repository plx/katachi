//! Codex discovery pipeline.
//!
//! The discovery stage walks the Codex home and each project root, parses
//! config layers, the `AGENTS.md` chain, skills, agents, hooks, rules, MCP
//! servers, and plugins, then assembles a [`RosterCatalog`] with
//! provenance metadata preserved in each item's `raw` payload.
//!
//! Each phase is implemented in its own submodule; this file contains the
//! orchestrator.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::error::ResolveError;
use katachi_core::roster::{RosterBuildError, RosterCatalog};

use crate::agents::discover_agents;
use crate::config_layers::discover_config_layers;
use crate::hooks::discover_hooks;
use crate::mcp::discover_mcp_servers;
use crate::plugins::discover_plugins;
use crate::roster::discover_instruction_chain;
use crate::rules::discover_rules;
use crate::skills::discover_skills;
use crate::CodexSettings;

/// Inputs for Codex discovery.
#[derive(Clone, Debug)]
pub struct DiscoveryInputs {
    pub settings: CodexSettings,
    /// Current working directory (used as the nominal project root when the
    /// settings do not declare explicit roots).
    pub cwd: Utf8PathBuf,
}

/// Top-level discovery entry.
pub fn discover(inputs: &DiscoveryInputs) -> Result<RosterCatalog, ResolveError> {
    let mut catalog = RosterCatalog::empty(katachi_core::model::HarnessKind::Codex);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    let project_roots = resolve_project_roots(&inputs.settings.project_roots, &inputs.cwd);

    // 1. Config layers (user + system + per-project).
    let layers = discover_config_layers(
        &inputs.settings,
        &project_roots,
        &inputs.cwd,
        &mut diagnostics,
    );
    for layer in &layers {
        insert_item(&mut catalog, layer.to_item(), &mut diagnostics);
        for profile in layer.emit_profiles() {
            insert_item(&mut catalog, profile, &mut diagnostics);
        }
    }
    // Layer override edges (higher precedence overrides lower precedence).
    crate::config_layers::insert_layer_edges(&layers, &mut catalog, &mut diagnostics);

    // 2. Instruction chain.
    let instructions = discover_instruction_chain(
        &inputs.settings,
        &project_roots,
        &inputs.cwd,
        &mut diagnostics,
    );
    for doc in &instructions {
        insert_item(&mut catalog, doc.to_item(), &mut diagnostics);
    }
    crate::roster::insert_instruction_edges(&instructions, &mut catalog, &mut diagnostics);

    // 3. Skills.
    let skills = discover_skills(&inputs.settings, &project_roots, &mut diagnostics);
    for skill in &skills {
        insert_item(&mut catalog, skill.to_item(), &mut diagnostics);
    }

    // 4. Custom agents.
    let agents = discover_agents(&inputs.settings, &project_roots, &mut diagnostics);
    for agent in &agents {
        insert_item(&mut catalog, agent.to_item(), &mut diagnostics);
    }

    // 5. Hooks and rules (both live near config layers).
    let hooks = discover_hooks(&layers, &mut diagnostics);
    for hook in &hooks {
        insert_item(&mut catalog, hook.to_item(), &mut diagnostics);
    }
    let rules = discover_rules(&layers, &mut diagnostics);
    for rule in &rules {
        insert_item(&mut catalog, rule.to_item(), &mut diagnostics);
    }

    // 6. MCP servers defined within active config layers.
    let mcps = discover_mcp_servers(&layers, &mut diagnostics);
    for mcp in &mcps {
        insert_item(&mut catalog, mcp.to_item(), &mut diagnostics);
    }

    // 7. Plugins discovered under marketplace roots.
    let plugins = discover_plugins(&inputs.settings, &mut diagnostics);
    for plugin in &plugins {
        insert_item(&mut catalog, plugin.to_item(), &mut diagnostics);
        for (child_item, edge) in plugin.packaged_items() {
            // Child items (packaged skills/agents/MCP shipped by the plugin)
            // must exist before we can wire up packaging edges.
            insert_item(&mut catalog, child_item, &mut diagnostics);
            if let Err(err) = catalog.insert_edge(edge) {
                diagnostics.push(Diagnostic::warning(
                    "codex.discovery.duplicate-edge",
                    format!("skipping duplicate plugin edge: {err}"),
                ));
            }
        }
    }

    // 8. Cross-cutting edges: skill -> MCP, agent -> config-layer, etc.
    crate::skills::insert_skill_edges(&skills, &mcps, &mut catalog, &mut diagnostics);
    crate::agents::insert_agent_edges(&agents, &layers, &mut catalog, &mut diagnostics);
    crate::hooks::insert_hook_edges(&hooks, &layers, &mut catalog, &mut diagnostics);
    crate::rules::insert_rule_edges(&rules, &layers, &mut catalog, &mut diagnostics);

    catalog.diagnostics = diagnostics;
    Ok(catalog)
}

fn insert_item(
    catalog: &mut RosterCatalog,
    item: katachi_core::roster::DiscoveredItem,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if catalog.contains(&item.item_ref) {
        // A catalog should not receive the same item twice. When it does
        // (e.g. a plugin packages a skill that was also discovered via
        // skills roots) we skip and emit a diagnostic — the first-wins
        // semantics means the free-standing copy takes precedence.
        diagnostics.push(Diagnostic::info(
            "codex.discovery.duplicate-item",
            format!(
                "item `{}` discovered more than once; keeping first occurrence",
                item.item_ref
            ),
        ));
        return;
    }
    if let Err(err) = catalog.insert_item(item) {
        let RosterBuildError::DuplicateItem { item_ref } = err else {
            diagnostics.push(Diagnostic::warning(
                "codex.discovery.insert-failed",
                format!("catalog rejected item: {err}"),
            ));
            return;
        };
        diagnostics.push(Diagnostic::info(
            "codex.discovery.duplicate-item",
            format!("duplicate item `{item_ref}` skipped"),
        ));
    }
}

/// Expand project roots against `cwd`. Relative roots become absolute;
/// `.` maps to `cwd`.
pub fn resolve_project_roots(roots: &[Utf8PathBuf], cwd: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let absolute = if root.is_absolute() {
            root.clone()
        } else if root.as_str() == "." {
            cwd.to_path_buf()
        } else {
            cwd.join(root)
        };
        // Deduplicate.
        if !out
            .iter()
            .any(|existing: &Utf8PathBuf| existing == &absolute)
        {
            out.push(absolute);
        }
    }
    if out.is_empty() {
        out.push(cwd.to_path_buf());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_roots_default_to_cwd() {
        let cwd = Utf8PathBuf::from("/repo");
        let out = resolve_project_roots(&[], &cwd);
        assert_eq!(out, vec![cwd]);
    }

    #[test]
    fn project_roots_absolute_preserved() {
        let cwd = Utf8PathBuf::from("/repo");
        let out = resolve_project_roots(
            &[Utf8PathBuf::from("/abs"), Utf8PathBuf::from("./sub")],
            &cwd,
        );
        assert_eq!(
            out,
            vec![Utf8PathBuf::from("/abs"), Utf8PathBuf::from("/repo/sub")]
        );
    }

    #[test]
    fn project_roots_dedup() {
        let cwd = Utf8PathBuf::from("/repo");
        let out = resolve_project_roots(
            &[
                Utf8PathBuf::from("."),
                Utf8PathBuf::from("/repo"),
                Utf8PathBuf::from("."),
            ],
            &cwd,
        );
        assert_eq!(out, vec![Utf8PathBuf::from("/repo")]);
    }
}
