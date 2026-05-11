//! End-to-end Gemini scan: settings → context → extensions → loose
//! artifacts, returning a fully-built [`RosterCatalog`] with edges.

use camino::{Utf8Path, Utf8PathBuf};

use katachi_core::diagnostic::Diagnostic;
use katachi_core::harness::{ExplainSection, RosterCatalog};
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DependencyEdge, DiscoveredItem, EdgeKind};

use crate::config::GeminiConfig;
use crate::context::{self, ContextDiscovery};
use crate::extension::{self, ExtensionDiscovery};
use crate::hook;
use crate::item::{GeminiEdgeKind, GeminiItemKind};
use crate::mcp;
use crate::policy;
use crate::settings::{SettingsDiscovery, SettingsLayer};
use crate::skill;
use crate::subagent;

/// Complete scan entrypoint.
pub fn scan_gemini(config: &GeminiConfig, cwd: &Utf8Path) -> RosterCatalog {
    let home = config
        .resolved_home()
        .unwrap_or_else(|_| Utf8PathBuf::from("/"));
    let user_roots = config.resolved_user_roots(&home);
    let project_roots = config.resolved_project_roots(cwd);
    let ext_roots = config.resolved_extension_roots(&home);

    let settings = SettingsDiscovery::discover(&user_roots, &project_roots);
    let effective_ctx_name = context::effective_context_name(&settings.layers);
    let contexts = ContextDiscovery::discover(&user_roots, &project_roots, &effective_ctx_name);
    let extensions = ExtensionDiscovery::discover(&ext_roots);

    let loose_skills = discover_loose_skills(&user_roots, &project_roots);
    let loose_subagents = discover_loose_subagents(&user_roots, &project_roots);

    build_catalog(
        &settings,
        &contexts,
        &extensions,
        &loose_skills,
        &loose_subagents,
    )
}

fn discover_loose_skills(
    user_roots: &[Utf8PathBuf],
    project_roots: &[Utf8PathBuf],
) -> Vec<crate::skill::Skill> {
    let mut out: Vec<crate::skill::Skill> = Vec::new();
    for root in user_roots {
        for candidate in &[root.join("skills"), root.join(".gemini").join("skills")] {
            if let Some(skills) = skill::scan_user_dir(candidate) {
                out.extend(skills);
            }
        }
    }
    for root in project_roots {
        for candidate in &[root.join("skills"), root.join(".gemini").join("skills")] {
            if let Some(skills) = skill::scan_project_dir(candidate) {
                out.extend(skills);
            }
        }
    }
    out
}

fn discover_loose_subagents(
    user_roots: &[Utf8PathBuf],
    project_roots: &[Utf8PathBuf],
) -> Vec<crate::subagent::Subagent> {
    let mut out: Vec<crate::subagent::Subagent> = Vec::new();
    for root in user_roots {
        for candidate in &[root.join("agents"), root.join(".gemini").join("agents")] {
            if let Some(ags) = subagent::scan_user_dir(candidate) {
                out.extend(ags);
            }
        }
    }
    for root in project_roots {
        for candidate in &[root.join("agents"), root.join(".gemini").join("agents")] {
            if let Some(ags) = subagent::scan_project_dir(candidate) {
                out.extend(ags);
            }
        }
    }
    out
}

/// Turn the piecewise discoveries into a [`RosterCatalog`].
pub fn build_catalog(
    settings: &SettingsDiscovery,
    contexts: &ContextDiscovery,
    extensions: &ExtensionDiscovery,
    loose_skills: &[crate::skill::Skill],
    loose_subagents: &[crate::subagent::Subagent],
) -> RosterCatalog {
    let mut catalog = RosterCatalog::empty(HarnessKind::Gemini);

    // --- Settings layers ---
    for layer in &settings.layers {
        let item = crate::settings::to_discovered_item(layer);
        let _ = catalog.insert_item(item);
    }
    // Edges between settings layers (higher precedence overrides lower).
    for window in settings.layers.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        let from = settings_ref(a);
        let to = settings_ref(b);
        if catalog.contains(&from) && catalog.contains(&to) {
            let _ = catalog.insert_edge(DependencyEdge {
                from: to.clone(),
                to: from.clone(),
                kind: EdgeKind::Semantic,
                required: false,
                note: Some(format!(
                    "{}::settings_overrides::{}",
                    GeminiEdgeKind::SettingsOverrides,
                    a.scope.as_str()
                )),
            });
        }
    }

    // --- Top-level context items ---
    for cs in &contexts.sources {
        let item = context::to_discovered_item(cs);
        let _ = catalog.insert_item(item);
    }

    // --- Extensions and their packaged items ---
    for ext in &extensions.extensions {
        let ext_item = extension::to_discovered_item(ext);
        let ext_ref = ext_item.item_ref.clone();
        let _ = catalog.insert_item(ext_item);

        // Extension-owned context.
        for cs in &ext.contexts {
            let mut item = context::to_discovered_item(cs);
            // Override id with extension-scoped id to avoid collisions.
            item.item_ref = ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::ContextSource.as_str(),
                format!("context:extension:{}:{}", ext.name(), cs.file_name),
            );
            item.packaging = Some(extension::package_ref_for(ext));
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ = catalog.insert_edge(containment_edge(
                    &ext_ref,
                    &to,
                    GeminiEdgeKind::ExtensionAddsContext,
                ));
            }
        }

        // Extension-owned skills.
        for s in &ext.skills {
            let item = skill::to_discovered_item(s);
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ =
                    catalog.insert_edge(containment_edge(&ext_ref, &to, GeminiEdgeKind::Contains));
            }
        }

        // Extension-owned subagents.
        for a in &ext.subagents {
            let item = subagent::to_discovered_item(a);
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ =
                    catalog.insert_edge(containment_edge(&ext_ref, &to, GeminiEdgeKind::Contains));
            }
        }

        // Extension-owned hooks.
        for h in &ext.hook_sets {
            let item = hook::to_discovered_item(h);
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ = catalog.insert_edge(containment_edge(
                    &ext_ref,
                    &to,
                    GeminiEdgeKind::ExtensionAddsHook,
                ));
            }
        }

        // Extension-owned policies.
        for p in &ext.policy_sets {
            let item = policy::to_discovered_item(p);
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ = catalog.insert_edge(containment_edge(
                    &ext_ref,
                    &to,
                    GeminiEdgeKind::ExtensionAddsPolicy,
                ));
            }
        }

        // Extension-owned MCP servers.
        for m in &ext.mcp_servers {
            let item = mcp::to_discovered_item(m);
            let to = item.item_ref.clone();
            if catalog.insert_item(item).is_ok() {
                let _ =
                    catalog.insert_edge(containment_edge(&ext_ref, &to, GeminiEdgeKind::Contains));
            }
        }
    }

    // --- Loose skills and subagents ---
    for s in loose_skills {
        let item = skill::to_discovered_item(s);
        let _ = catalog.insert_item(item);
    }
    for a in loose_subagents {
        let item = subagent::to_discovered_item(a);
        let _ = catalog.insert_item(item);
    }

    // --- Settings-sourced MCP and hook items, plus conflict edges ---
    for layer in &settings.layers {
        if let Some(servers) =
            mcp::parse_settings_servers(&layer.body, layer.scope.as_str(), &layer.path)
        {
            for server in &servers {
                let item = mcp::to_discovered_item(server);
                let to = item.item_ref.clone();
                let _ = catalog.insert_item(item);
                // Conflict edge against any same-named extension server.
                for ext_item in extensions
                    .extensions
                    .iter()
                    .flat_map(|e| e.mcp_servers.iter())
                    .filter(|ext_server| ext_server.name == server.name)
                {
                    let from = ItemRef::new(
                        HarnessKind::Gemini,
                        GeminiItemKind::McpServer.as_str(),
                        mcp::item_id(ext_item),
                    );
                    if catalog.contains(&from) && catalog.contains(&to) {
                        let _ = catalog.insert_edge(DependencyEdge {
                            from,
                            to: to.clone(),
                            kind: EdgeKind::Projection,
                            required: false,
                            note: Some(format!(
                                "{}::same-name::{}",
                                GeminiEdgeKind::SettingsWinsMcpNameConflict,
                                server.name
                            )),
                        });
                    }
                }
            }
        }
        if let Some(hook_set) = hook::scan_settings(&layer.body, layer.scope.as_str(), &layer.path)
        {
            let item = hook::to_discovered_item(&hook_set);
            let _ = catalog.insert_item(item);
        }
        if let Some(policy_set) =
            policy::scan_settings(&layer.body, layer.scope.as_str(), &layer.path)
        {
            let item = policy::to_discovered_item(&policy_set);
            let _ = catalog.insert_item(item);
        }
    }

    // --- Diagnostics for errors raised during discovery ---
    for e in &settings.errors {
        catalog
            .diagnostics
            .push(Diagnostic::warning("gemini.settings", e.to_string()).with_pointer("/settings"));
    }
    for e in &extensions.errors {
        catalog.diagnostics.push(
            Diagnostic::warning("gemini.extension", e.to_string()).with_pointer("/extensions"),
        );
    }

    catalog
}

fn settings_ref(layer: &SettingsLayer) -> ItemRef {
    ItemRef::new(
        HarnessKind::Gemini,
        GeminiItemKind::SettingsLayer.as_str(),
        layer.scope.item_id(),
    )
}

fn containment_edge(from: &ItemRef, to: &ItemRef, kind: GeminiEdgeKind) -> DependencyEdge {
    DependencyEdge {
        from: from.clone(),
        to: to.clone(),
        kind: kind.role_hint(),
        required: true,
        note: Some(kind.to_string()),
    }
}

/// Enrich an `explain` response with extra sections tailored to the
/// item's kind.
pub fn explain_sections_for(item: &DiscoveredItem, catalog: &RosterCatalog) -> Vec<ExplainSection> {
    let mut sections = Vec::new();
    if item.source.path.is_some() {
        sections.push(ExplainSection {
            title: "Source".into(),
            body: format!(
                "path: {}\nscope: {}",
                item.source
                    .path
                    .as_ref()
                    .map(|p| p.as_str())
                    .unwrap_or("(n/a)"),
                item.source.scope.as_deref().unwrap_or("(n/a)")
            ),
        });
    }
    if !item.capabilities.is_empty() {
        sections.push(ExplainSection {
            title: "Capabilities".into(),
            body: item.capabilities.join(", "),
        });
    }
    if let Some(pkg) = &item.packaging {
        sections.push(ExplainSection {
            title: "Packaging".into(),
            body: format!("ships in {} (required: {})", pkg.item_ref, pkg.required),
        });
    }
    // Neighbors: outgoing edges.
    let outs: Vec<String> = catalog
        .edges_from(&item.item_ref)
        .map(|e| {
            format!(
                "-> {} ({} {})",
                e.to,
                edge_kind_label(e.kind),
                e.note.clone().unwrap_or_default()
            )
        })
        .collect();
    if !outs.is_empty() {
        sections.push(ExplainSection {
            title: "Outgoing edges".into(),
            body: outs.join("\n"),
        });
    }
    let ins: Vec<String> = catalog
        .edges_to(&item.item_ref)
        .map(|e| {
            format!(
                "<- {} ({} {})",
                e.from,
                edge_kind_label(e.kind),
                e.note.clone().unwrap_or_default()
            )
        })
        .collect();
    if !ins.is_empty() {
        sections.push(ExplainSection {
            title: "Incoming edges".into(),
            body: ins.join("\n"),
        });
    }
    sections
}

fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Packaging => "packaging",
        EdgeKind::Semantic => "semantic",
        EdgeKind::Projection => "projection",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn utf8(d: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap()
    }

    fn write(path: &Utf8Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path.as_std_path(), body).unwrap();
    }

    /// Build a realistic fixture: a home dir with a user settings.json,
    /// an extension with a skill and hooks, plus a project `.gemini` dir
    /// with its own settings and GEMINI.md.
    fn build_fixture_env() -> (TempDir, GeminiConfig, Utf8PathBuf) {
        let tmp = TempDir::new().unwrap();
        let home = utf8(&tmp).join("home");
        let ext_root = home.join("extensions");
        let project = utf8(&tmp).join("project");

        // User settings
        write(
            &home.join("settings.json"),
            r#"{"theme": "dark", "contextFileName": "GEMINI.md"}"#,
        );
        write(&home.join("GEMINI.md"), "user-level context");

        // Extension
        let ext_dir = ext_root.join("workspace-a11y");
        write(
            &ext_dir.join("gemini-extension.json"),
            r#"{"name": "workspace-a11y", "version": "0.1.0", "description": "a11y tools", "mcpServers": {"chrome-devtools": {"command": "node"}}}"#,
        );
        write(&ext_dir.join("GEMINI.md"), "extension context");
        write(
            &ext_dir.join("skills/audit.md"),
            "---\ndescription: accessibility audit\n---\nbody\n",
        );
        write(
            &ext_dir.join("hooks/hooks.json"),
            r#"{"PreToolUse": [{"command": "echo"}]}"#,
        );
        write(&ext_dir.join("policies/readonly.json"), r#"{"allow": []}"#);
        write(
            &ext_dir.join("agents/explorer.md"),
            "---\ndescription: explore\nexperimental: true\n---\n",
        );

        // Project settings
        write(
            &project.join(".gemini/settings.json"),
            r#"{"model": "gemini-3-pro-preview", "mcpServers": {"chrome-devtools": {"command": "override"}}}"#,
        );
        write(&project.join(".gemini/GEMINI.md"), "project context");

        // Loose user skill
        write(
            &home.join("skills/loose.md"),
            "---\ndescription: loose skill\n---\n",
        );

        let cfg = GeminiConfig {
            home: Some(home.clone()),
            user_roots: vec![home],
            project_roots: vec![project.clone()],
            extension_roots: vec![ext_root],
            ..GeminiConfig::default()
        };
        (tmp, cfg, project)
    }

    #[test]
    fn realistic_scan_populates_catalog() {
        let (_g, cfg, cwd) = build_fixture_env();
        let catalog = scan_gemini(&cfg, &cwd);
        // Extension present
        let ext_ref = ItemRef::new(HarnessKind::Gemini, "extension", "workspace-a11y");
        assert!(catalog.contains(&ext_ref));
        // Settings layers
        assert!(catalog.contains(&ItemRef::new(
            HarnessKind::Gemini,
            "settings_layer",
            "settings:user"
        )));
        assert!(catalog.contains(&ItemRef::new(
            HarnessKind::Gemini,
            "settings_layer",
            "settings:project"
        )));
        // Extension-owned skill
        assert!(catalog.contains(&ItemRef::new(HarnessKind::Gemini, "skill", "audit")));
        // Extension-owned subagent marked preview.
        let explorer = catalog
            .get(&ItemRef::new(HarnessKind::Gemini, "subagent", "explorer"))
            .unwrap();
        assert!(explorer
            .capabilities
            .iter()
            .any(|c| c == "preview-required"));
        // Loose user skill surfaces
        assert!(catalog.contains(&ItemRef::new(HarnessKind::Gemini, "skill", "loose")));
    }

    #[test]
    fn extension_contains_skill_edge_present() {
        let (_g, cfg, cwd) = build_fixture_env();
        let catalog = scan_gemini(&cfg, &cwd);
        let ext = ItemRef::new(HarnessKind::Gemini, "extension", "workspace-a11y");
        let skill = ItemRef::new(HarnessKind::Gemini, "skill", "audit");
        let hits: Vec<&DependencyEdge> = catalog
            .edges_from(&ext)
            .filter(|e| e.to == skill && e.kind == EdgeKind::Packaging)
            .collect();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn mcp_name_conflict_emits_projection_edge() {
        let (_g, cfg, cwd) = build_fixture_env();
        let catalog = scan_gemini(&cfg, &cwd);
        let projection_edges: Vec<_> = catalog
            .iter_edges()
            .filter(|e| e.kind == EdgeKind::Projection)
            .collect();
        assert!(
            !projection_edges.is_empty(),
            "expected at least one settings-wins-mcp conflict edge"
        );
    }
}
