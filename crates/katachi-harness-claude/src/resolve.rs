//! Claude roster resolver.
//!
//! Takes a parsed [`ClaudeRoster`] plus a scanned [`RosterCatalog`] and
//! produces a `ResolvedClaudeRoster` containing:
//!
//! - every directly-selected item (or a diagnostic for each miss)
//! - the packaging closure (a selected plugin implicitly selects every
//!   item it contains; a selected plugin-packaged item implicitly
//!   selects its parent plugin)
//! - the semantic closure (skill ↔ agent, agent → preloaded skill,
//!   item → required/suggested MCP, and item → added hooks)
//! - projection-loss warnings when the chosen backend can't faithfully
//!   represent parts of the selection

use std::collections::{BTreeSet, VecDeque};

use katachi_core::diagnostic::Diagnostic;
use katachi_core::harness::{EdgeKind, RosterCatalog};
use katachi_core::model::{BackendKind, HarnessKind, ItemRef};
use katachi_core::plan::{ResolvedItemRef, ResolvedKatachi, RunProfile, SelectionReason};

use crate::roster::{ClaudeRoster, RosterResolution};

/// The resolver's full output: a [`ResolvedKatachi`] plus extra
/// Claude-specific diagnostics (projection loss, selection gaps) and the
/// scoped catalog slice used by the CLI planner.
#[derive(Clone, Debug)]
pub struct ResolvedClaudeRoster {
    pub roster: ClaudeRoster,
    pub resolved: ResolvedKatachi,
    pub catalog: RosterCatalog,
    pub projection_diagnostics: Vec<Diagnostic>,
}

/// Entry point. Resolves `roster` against `catalog`, picking `backend` as
/// the desired Claude backend.
pub fn resolve_roster(
    roster: &ClaudeRoster,
    catalog: RosterCatalog,
    backend: BackendKind,
) -> ResolvedClaudeRoster {
    let mut diagnostics = Vec::new();

    // 1. Direct selections. Unknown ids become error diagnostics; every
    //    match becomes a seed.
    let (seeds, missing) = collect_seeds(roster, &catalog);
    for (kind, id) in missing {
        diagnostics.push(Diagnostic::error(
            "resolve.missing-item",
            format!("roster `{}` references unknown {kind} `{id}`", roster.id),
        ));
    }

    // 2. Closure.
    let trace = expand_closure(&catalog, seeds.iter().cloned().collect(), &roster.resolution);

    // 3. Assemble ResolvedItemRef set in deterministic order.
    let mut resolved_items: Vec<ResolvedItemRef> = Vec::with_capacity(trace.len());
    let mut sorted_refs: Vec<&ItemRef> = trace.iter().map(|(ir, _)| ir).collect();
    sorted_refs.sort();
    for ir in sorted_refs {
        let (reason, pulled_in_by) = trace
            .iter()
            .find(|(i, _)| i == ir)
            .map(|(_, v)| v.clone())
            .unwrap();
        resolved_items.push(ResolvedItemRef {
            item: ir.clone(),
            reason,
            pulled_in_by,
        });
    }

    // 4. Projection-loss diagnostics. These are advisory; they don't
    //    kill resolution, but they get surfaced everywhere the resolver
    //    output is shown.
    let projection_diagnostics = project_diagnostics(backend, &resolved_items, &catalog);

    let run_profile = RunProfile {
        backend: Some(backend),
        extras: serde_json::to_value(&roster.run_profile).unwrap_or(serde_json::Value::Null),
    };
    let resolved = ResolvedKatachi {
        katachi_id: roster.id.clone(),
        harness: HarnessKind::Claude,
        backend,
        selected_items: resolved_items,
        run_profile,
        diagnostics: diagnostics.clone(),
    };
    ResolvedClaudeRoster {
        roster: roster.clone(),
        resolved,
        catalog,
        projection_diagnostics,
    }
}

fn collect_seeds(
    roster: &ClaudeRoster,
    catalog: &RosterCatalog,
) -> (Vec<ItemRef>, Vec<(&'static str, String)>) {
    let mut seeds = Vec::new();
    let mut missing = Vec::new();
    for (kind, id) in roster.selection_entries() {
        let ir = ItemRef::new(HarnessKind::Claude, kind, id);
        // `instruction_source` ids carry scope prefixes; the selection
        // may want to reference them by short name (e.g.
        // `project:CLAUDE.md` vs `CLAUDE.md`). Accept either.
        if catalog.contains(&ir) {
            seeds.push(ir);
            continue;
        }
        // Fallback: scan for a matching short id.
        let matches: Vec<_> = catalog
            .iter_items()
            .filter(|(c, _)| c.kind == kind && short_id(&c.id) == id)
            .map(|(c, _)| c.clone())
            .collect();
        if matches.len() == 1 {
            seeds.push(matches.into_iter().next().unwrap());
        } else if matches.len() > 1 {
            missing.push((kind, format!("{id} (ambiguous across scopes)")));
        } else {
            missing.push((kind, id.to_string()));
        }
    }
    (seeds, missing)
}

fn short_id(id: &str) -> &str {
    // `project:CLAUDE.md` → `CLAUDE.md`; `project:rule:style` → `rule:style`.
    id.split_once(':').map(|(_, rest)| rest).unwrap_or(id)
}

/// BFS expansion through packaging + semantic edges. Follows packaging
/// in both directions (child ↔ parent) so a selected plugin-packaged
/// item auto-includes its plugin and vice versa.
fn expand_closure(
    catalog: &RosterCatalog,
    seeds: Vec<ItemRef>,
    resolution: &RosterResolution,
) -> Vec<(ItemRef, (SelectionReason, Option<ItemRef>))> {
    let mut trace: Vec<(ItemRef, (SelectionReason, Option<ItemRef>))> = Vec::new();
    let mut seen: BTreeSet<ItemRef> = BTreeSet::new();
    let mut queue: VecDeque<(ItemRef, SelectionReason, Option<ItemRef>)> = VecDeque::new();
    for s in seeds {
        queue.push_back((s, SelectionReason::Direct, None));
    }

    while let Some((ir, reason, parent)) = queue.pop_front() {
        if !seen.insert(ir.clone()) {
            continue;
        }
        trace.push((ir.clone(), (reason, parent.clone())));

        if !resolution.include_transitive {
            continue;
        }

        // Packaging closure: both directions.
        for edge in catalog.edges_from(&ir) {
            if edge.kind == EdgeKind::Packaging && !seen.contains(&edge.to) {
                queue.push_back((
                    edge.to.clone(),
                    SelectionReason::PackagingClosure,
                    Some(ir.clone()),
                ));
            }
        }
        for edge in catalog.edges_to(&ir) {
            if edge.kind == EdgeKind::Packaging && !seen.contains(&edge.from) {
                queue.push_back((
                    edge.from.clone(),
                    SelectionReason::PackagingClosure,
                    Some(ir.clone()),
                ));
            }
        }
        // Semantic closure: outgoing only. Semantic edges are directional
        // (skill → agent, etc.), so following them forward is the
        // correct behavior.
        for edge in catalog.edges_from(&ir) {
            if edge.kind == EdgeKind::Semantic && !seen.contains(&edge.to) {
                queue.push_back((
                    edge.to.clone(),
                    SelectionReason::SemanticClosure,
                    Some(ir.clone()),
                ));
            }
        }
    }
    trace
}

fn project_diagnostics(
    backend: BackendKind,
    resolved: &[ResolvedItemRef],
    catalog: &RosterCatalog,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    match backend {
        BackendKind::SdkTs | BackendKind::SdkPy => {
            // SDK loses: file-based hooks in settings fragments + CLI-only
            // allowed-tools frontmatter.
            for r in resolved {
                let Some(item) = catalog.get(&r.item) else {
                    continue;
                };
                if item.item_ref.kind == "hook_set"
                    && item.source.provenance.as_deref() == Some("settings-hook")
                {
                    out.push(Diagnostic::warning(
                        "claude.sdk-projection-hook-loss",
                        format!(
                            "hook `{}` is defined in settings.json; SDK projections need callback wiring and may not run it verbatim",
                            r.item
                        ),
                    ));
                }
                if item.item_ref.kind == "skill"
                    && item
                        .raw
                        .get("allowed_tools")
                        .and_then(|v| v.as_array())
                        .map(|v| !v.is_empty())
                        .unwrap_or(false)
                {
                    out.push(Diagnostic::warning(
                        "claude.sdk-projection-allowed-tools",
                        format!(
                            "skill `{}` declares `allowed-tools`; CLI enforces that automatically but SDK callers must pass equivalent tools explicitly",
                            r.item
                        ),
                    ));
                }
            }
        }
        BackendKind::McpServer | BackendKind::AppServer => {
            out.push(Diagnostic::warning(
                "claude.unsupported-backend",
                format!(
                    "backend `{}` is not a supported Claude projection target",
                    backend
                ),
            ));
        }
        BackendKind::Cli => {}
    }
    out
}

/// Validate the resolution: look for structural problems that should
/// stop a user from executing. Returns every error *and* warning so
/// callers can render both.
pub fn validate(resolved: &ResolvedClaudeRoster) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let selected: BTreeSet<ItemRef> = resolved
        .resolved
        .selected_items
        .iter()
        .map(|r| r.item.clone())
        .collect();

    // 1. Packaging: packaged-required items must have their plugin.
    for r in &resolved.resolved.selected_items {
        let Some(item) = resolved.catalog.get(&r.item) else {
            continue;
        };
        if let Some(pkg) = &item.packaging {
            if pkg.required && !selected.contains(&pkg.item_ref) {
                out.push(Diagnostic::error(
                    "claude.validate.packaging-missing",
                    format!(
                        "selected `{}` requires plugin `{}` which is not selected",
                        r.item, pkg.item_ref
                    ),
                ));
            }
        }
    }

    // 2. Semantic: required edges must resolve. We treat edges marked
    //    `required` (skill → required MCP, agent → required MCP) as
    //    blocking.
    for r in &resolved.resolved.selected_items {
        for edge in resolved.catalog.edges_from(&r.item) {
            if edge.kind != EdgeKind::Semantic {
                continue;
            }
            if !edge.required {
                continue;
            }
            if !selected.contains(&edge.to) {
                out.push(Diagnostic::error(
                    "claude.validate.required-edge-missing",
                    format!(
                        "selected `{}` requires `{}` ({})",
                        r.item,
                        edge.to,
                        edge.note.as_deref().unwrap_or("semantic")
                    ),
                ));
            }
        }
    }

    // 3. Forward the projection diagnostics verbatim.
    out.extend(resolved.projection_diagnostics.iter().cloned());

    // 4. Forward resolver diagnostics (missing items, etc.).
    out.extend(resolved.resolved.diagnostics.iter().cloned());

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use katachi_core::harness::{
        DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, PackageRef, RosterCatalog,
    };
    use katachi_core::model::HarnessKind;

    fn item(kind: &str, id: &str) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(HarnessKind::Claude, kind, id),
            display_name: id.into(),
            source: ItemSource::default(),
            packaging: None,
            raw: serde_json::Value::Null,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    fn parse(toml: &str) -> ClaudeRoster {
        ClaudeRoster::from_toml_str(&Utf8PathBuf::from("t.toml"), toml).unwrap()
    }

    #[test]
    fn packaging_closure_in_both_directions() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("plugin", "p")).unwrap();
        let mut skill = item("skill", "s");
        skill.packaging = Some(PackageRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            required: true,
        });
        cat.insert_item(skill).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            to: ItemRef::new(HarnessKind::Claude, "skill", "s"),
            kind: EdgeKind::Packaging,
            required: true,
            note: Some("contains".into()),
        })
        .unwrap();

        // Selecting only the skill should pull in the plugin.
        let roster = parse(
            r#"
version = 1
id = "r1"
[selection]
skills = ["s"]
"#,
        );
        let out = resolve_roster(&roster, cat.clone(), BackendKind::Cli);
        let ids: Vec<_> = out
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.id.clone())
            .collect();
        assert!(ids.contains(&"s".to_string()));
        assert!(ids.contains(&"p".to_string()));

        // Selecting only the plugin should pull in the packaged skill.
        let roster2 = parse(
            r#"
version = 1
id = "r2"
[selection]
plugins = ["p"]
"#,
        );
        let out2 = resolve_roster(&roster2, cat, BackendKind::Cli);
        let ids2: Vec<_> = out2
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.id.clone())
            .collect();
        assert!(ids2.contains(&"s".to_string()));
        assert!(ids2.contains(&"p".to_string()));
    }

    #[test]
    fn semantic_closure_follows_skill_to_agent() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("skill", "axe")).unwrap();
        cat.insert_item(item("agent", "reviewer")).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe"),
            to: ItemRef::new(HarnessKind::Claude, "agent", "reviewer"),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some("skill_uses_agent".into()),
        })
        .unwrap();

        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
skills = ["axe"]
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        assert!(out
            .resolved
            .selected_items
            .iter()
            .any(|r| r.item.id == "reviewer"));
    }

    #[test]
    fn missing_selection_yields_resolve_error_diagnostic() {
        let cat = RosterCatalog::empty(HarnessKind::Claude);
        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
skills = ["ghost"]
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        assert!(out
            .resolved
            .diagnostics
            .iter()
            .any(|d| d.code == "resolve.missing-item"));
    }

    #[test]
    fn disabled_transitive_closure_keeps_selection_minimal() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("skill", "axe")).unwrap();
        cat.insert_item(item("agent", "reviewer")).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe"),
            to: ItemRef::new(HarnessKind::Claude, "agent", "reviewer"),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some("skill_uses_agent".into()),
        })
        .unwrap();

        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
skills = ["axe"]
[resolution]
include_transitive = false
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        assert_eq!(out.resolved.selected_items.len(), 1);
    }

    #[test]
    fn validate_flags_missing_required_package() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        let mut skill = item("skill", "s");
        skill.packaging = Some(PackageRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "p"),
            required: true,
        });
        cat.insert_item(skill).unwrap();
        // Deliberately do NOT insert the plugin so the closure can't
        // pull it in either.
        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
skills = ["s"]
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        let diags = validate(&out);
        assert!(diags
            .iter()
            .any(|d| d.code == "claude.validate.packaging-missing"));
    }

    #[test]
    fn sdk_projection_emits_hook_warning() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        let mut hook = item("hook_set", "pre");
        hook.source = ItemSource {
            path: Some(Utf8PathBuf::from("/x/.claude/settings.json")),
            scope: Some("project".into()),
            provenance: Some("settings-hook".into()),
        };
        cat.insert_item(hook).unwrap();

        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
hooks = ["pre"]
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::SdkTs);
        assert!(out
            .projection_diagnostics
            .iter()
            .any(|d| d.code == "claude.sdk-projection-hook-loss"));
    }

    #[test]
    fn required_mcp_edge_missing_is_error() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("skill", "axe")).unwrap();
        cat.insert_item(item("mcp_server", "chrome")).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe"),
            to: ItemRef::new(HarnessKind::Claude, "mcp_server", "chrome"),
            kind: EdgeKind::Semantic,
            required: true,
            note: Some("item_requires_mcp".into()),
        })
        .unwrap();

        let roster = parse(
            r#"
version = 1
id = "r"
[selection]
skills = ["axe"]
[resolution]
include_transitive = false
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        let diags = validate(&out);
        assert!(diags
            .iter()
            .any(|d| d.code == "claude.validate.required-edge-missing"));
    }

    /// Fixture: minimal roster that selects a plugin with a packaged
    /// skill and agent. Used as a basis for plan/execute smoke tests
    /// in later steps; keeping the fixture here since the resolver is
    /// the natural place to stitch it together.
    #[test]
    fn fixture_closure_smoke_test() {
        let mut cat = RosterCatalog::empty(HarnessKind::Claude);
        cat.insert_item(item("plugin", "web-a11y")).unwrap();
        let mut skill = item("skill", "axe-runner");
        skill.packaging = Some(PackageRef {
            item_ref: ItemRef::new(HarnessKind::Claude, "plugin", "web-a11y"),
            required: true,
        });
        cat.insert_item(skill).unwrap();
        cat.insert_item(item("agent", "reviewer")).unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "plugin", "web-a11y"),
            to: ItemRef::new(HarnessKind::Claude, "skill", "axe-runner"),
            kind: EdgeKind::Packaging,
            required: true,
            note: Some("contains".into()),
        })
        .unwrap();
        cat.insert_edge(DependencyEdge {
            from: ItemRef::new(HarnessKind::Claude, "skill", "axe-runner"),
            to: ItemRef::new(HarnessKind::Claude, "agent", "reviewer"),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some("skill_uses_agent".into()),
        })
        .unwrap();

        let roster = parse(
            r#"
version = 1
id = "a11y"
[selection]
plugins = ["web-a11y"]
"#,
        );
        let out = resolve_roster(&roster, cat, BackendKind::Cli);
        let ids: BTreeSet<_> = out
            .resolved
            .selected_items
            .iter()
            .map(|r| r.item.id.clone())
            .collect();
        assert!(ids.contains("web-a11y"));
        assert!(ids.contains("axe-runner"));
        assert!(ids.contains("reviewer"));
    }
}

