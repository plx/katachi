//! Temp-overlay materializer for Gemini.
//!
//! Produces an ephemeral directory tree that the Gemini CLI can point at
//! via env overrides, preserving a manifest of every action taken. The
//! layout mirrors the one described in the implementation spec:
//!
//! ```text
//! /tmp/katachi-gemini-<run-id>/
//!   home/.gemini/
//!     settings.json
//!     extensions/...
//!   project/.gemini/
//!     settings.json
//!     skills/...
//!   project/GEMINI.md
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::harness::RosterCatalog;
use katachi_core::materialize::TempOverlay;
use katachi_core::model::ItemRef;
use katachi_core::plan::ResolvedKatachi;

use crate::extension::DiscoveredExtension;
use crate::item::GeminiItemKind;

/// Relative paths inside the overlay.
pub mod paths {
    pub const HOME: &str = "home";
    pub const HOME_GEMINI: &str = "home/.gemini";
    pub const HOME_SETTINGS: &str = "home/.gemini/settings.json";
    pub const HOME_EXTENSIONS: &str = "home/.gemini/extensions";
    pub const PROJECT: &str = "project";
    pub const PROJECT_GEMINI: &str = "project/.gemini";
    pub const PROJECT_SETTINGS: &str = "project/.gemini/settings.json";
    pub const PROJECT_CONTEXT: &str = "project/GEMINI.md";
}

/// Manifest describing what the overlay produced, so the planner and
/// executor can pass the right env vars.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverlayManifest {
    pub overlay_root: Utf8PathBuf,
    pub home_dir: Utf8PathBuf,
    pub project_dir: Utf8PathBuf,
    pub extensions: Vec<Utf8PathBuf>,
    pub generated_files: Vec<Utf8PathBuf>,
    pub env: BTreeMap<String, String>,
}

/// Build a temp overlay for a resolved Gemini run.
///
/// The returned overlay defaults to [`KeepPolicy::Discard`]; callers that
/// want to preserve overlays after a failed run must flip the policy to
/// `Keep` once the failure is observed (see the execute call site in
/// `katachi-cli`). Doing it here would leak the overlay even on
/// successful executes, eventually exhausting `/tmp` for long-lived
/// users.
pub fn materialize_overlay(
    resolved: &ResolvedKatachi,
    catalog: &RosterCatalog,
    extensions: &[DiscoveredExtension],
) -> io::Result<(TempOverlay, OverlayManifest)> {
    let mut overlay = TempOverlay::with_prefix("katachi-gemini-")?;

    let overlay_root = overlay.root().to_owned();
    let home_dir = overlay_root.join(paths::HOME);
    let project_dir = overlay_root.join(paths::PROJECT);

    let mut manifest = OverlayManifest {
        overlay_root: overlay_root.clone(),
        home_dir: home_dir.clone(),
        project_dir: project_dir.clone(),
        extensions: Vec::new(),
        generated_files: Vec::new(),
        env: BTreeMap::new(),
    };

    // Generate `home/.gemini/settings.json` from the highest-ranked user
    // settings layer present in the catalog.
    let user_settings = collect_settings_body(catalog, "user");
    let home_settings = serde_json::to_string_pretty(&user_settings)?;
    let generated = overlay.write_inline(paths::HOME_SETTINGS, &home_settings)?;
    manifest
        .generated_files
        .push(path_or_rel(&generated, &overlay_root));

    // Generate `project/.gemini/settings.json` from project-layer.
    let project_settings = collect_settings_body(catalog, "project");
    let generated = overlay.write_inline(
        paths::PROJECT_SETTINGS,
        &serde_json::to_string_pretty(&project_settings)?,
    )?;
    manifest
        .generated_files
        .push(path_or_rel(&generated, &overlay_root));

    // Write context file (prefer project scope, fallback to user).
    if let Some(context) = pick_context(catalog) {
        let generated = overlay.write_inline(paths::PROJECT_CONTEXT, &context)?;
        manifest
            .generated_files
            .push(path_or_rel(&generated, &overlay_root));
    }

    // Symlink or copy each selected extension directory into
    // `home/.gemini/extensions/<name>`.
    let selected_extension_ids: Vec<&str> = resolved
        .selected_items
        .iter()
        .filter(|i| i.item.kind == GeminiItemKind::Extension.as_str())
        .map(|i| i.item.id.as_str())
        .collect();
    for id in &selected_extension_ids {
        if let Some(ext) = extensions.iter().find(|e| e.name() == *id) {
            let rel = Utf8PathBuf::from(paths::HOME_EXTENSIONS).join(ext.name());
            #[cfg(unix)]
            {
                let created = overlay.symlink(&rel, &ext.root)?;
                manifest
                    .extensions
                    .push(path_or_rel(&created, &overlay_root));
            }
            #[cfg(not(unix))]
            {
                // Fallback: deep-copy the extension directory. We leave
                // this unimplemented for the prototype.
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "extension materialization requires symlink support",
                ));
            }
        }
    }

    // Populate env vars so the child process sees the temp home as its
    // real home and the temp project as its cwd.
    manifest.env.insert("HOME".into(), home_dir.to_string());
    manifest
        .env
        .insert("GEMINI_HOME".into(), home_dir.join(".gemini").to_string());
    manifest.env.insert(
        "XDG_CONFIG_HOME".into(),
        home_dir.join(".config").to_string(),
    );

    Ok((overlay, manifest))
}

fn path_or_rel(abs: &Utf8Path, root: &Utf8Path) -> Utf8PathBuf {
    abs.strip_prefix(root)
        .ok()
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| abs.to_owned())
}

fn collect_settings_body(catalog: &RosterCatalog, scope: &str) -> Value {
    for (_, item) in catalog.iter_items() {
        if item.item_ref.kind != GeminiItemKind::SettingsLayer.as_str() {
            continue;
        }
        if item.source.scope.as_deref() == Some(scope) {
            return item.raw.clone();
        }
    }
    Value::Object(serde_json::Map::new())
}

fn pick_context(catalog: &RosterCatalog) -> Option<String> {
    // Prefer project context, fall back to user. Read the full file from
    // the source path; the `preview` field on the discovered item is
    // truncated and would silently drop content past ~512 bytes.
    for scope in ["project", "user"] {
        for (_, item) in catalog.iter_items() {
            if item.item_ref.kind != GeminiItemKind::ContextSource.as_str() {
                continue;
            }
            if item.source.scope.as_deref() != Some(scope) {
                continue;
            }
            if let Some(path) = item.source.path.as_ref() {
                if let Ok(body) = fs::read_to_string(path.as_std_path()) {
                    return Some(body);
                }
            }
        }
    }
    None
}

/// Look up the source extension manifests from the catalog.
///
/// The planner passes `selected_extension_ids` to the scanner so it can
/// ship full extension details (including `root`) to the materializer.
pub fn find_extension<'a>(
    extensions: &'a [DiscoveredExtension],
    id: &str,
) -> Option<&'a DiscoveredExtension> {
    extensions.iter().find(|e| e.name() == id)
}

/// Extract the subset of extensions that were selected in the resolved
/// katachi.
pub fn selected_extensions<'a>(
    resolved: &ResolvedKatachi,
    extensions: &'a [DiscoveredExtension],
) -> Vec<&'a DiscoveredExtension> {
    let wanted: Vec<&str> = resolved
        .selected_items
        .iter()
        .filter(|i| i.item.kind == GeminiItemKind::Extension.as_str())
        .map(|i| i.item.id.as_str())
        .collect();
    let mut out = Vec::new();
    for id in wanted {
        if let Some(e) = find_extension(extensions, id) {
            out.push(e);
        }
    }
    out
}

/// Translate an overlay manifest into a `MaterializationPlan` so the
/// result can be serialized alongside the rest of the execution plan.
pub fn to_materialization_plan(
    manifest: &OverlayManifest,
) -> katachi_core::plan::MaterializationPlan {
    use katachi_core::plan::{FileSource, MaterializationPlan, MaterializedFile};
    let mut files = Vec::new();
    for rel in &manifest.generated_files {
        files.push(MaterializedFile {
            dest: rel.clone(),
            source: FileSource::Inline {
                contents: "<generated>".into(),
            },
        });
    }
    for rel in &manifest.extensions {
        files.push(MaterializedFile {
            dest: rel.clone(),
            source: FileSource::SymlinkTo {
                target: Utf8PathBuf::from("<extension root>"),
            },
        });
    }
    MaterializationPlan {
        mode: katachi_core::model::MaterializationMode::TempOverlay,
        overlay_root: Some(manifest.overlay_root.clone()),
        files,
        env: manifest.env.clone(),
    }
}

// Expose ItemRef so call sites don't need two imports.
pub use katachi_core::model::HarnessKind as _HarnessKind;

/// Debug helper used by `explain` or audit commands to render a summary
/// of the materialized overlay.
pub fn summarize_manifest(manifest: &OverlayManifest) -> String {
    let mut s = String::new();
    s.push_str("overlay: ");
    s.push_str(manifest.overlay_root.as_str());
    s.push_str("\nhome: ");
    s.push_str(manifest.home_dir.as_str());
    s.push_str("\nproject: ");
    s.push_str(manifest.project_dir.as_str());
    s.push_str(&format!("\nextensions: {}\n", manifest.extensions.len()));
    for (k, v) in &manifest.env {
        s.push_str(&format!("env {k}={v}\n"));
    }
    s
}

// Re-export so callers can build their own manifests.
pub use katachi_core::materialize::TempOverlay as _TempOverlay;

/// Return the item ref of the first user-scope settings layer in the
/// catalog, if any. Used by diagnostics.
pub fn user_settings_ref(catalog: &RosterCatalog) -> Option<ItemRef> {
    for (_, item) in catalog.iter_items() {
        if item.item_ref.kind == GeminiItemKind::SettingsLayer.as_str()
            && item.source.scope.as_deref() == Some("user")
        {
            return Some(item.item_ref.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use katachi_core::diagnostic::Diagnostic;
    use katachi_core::model::{BackendKind, HarnessKind};
    use katachi_core::plan::{ResolvedItemRef, ResolvedKatachi, RunProfile, SelectionReason};
    use katachi_core::roster::{DiscoveredItem, ItemSource, RosterCatalog};
    use std::fs;
    use tempfile::TempDir;

    fn resolved_with(items: Vec<ResolvedItemRef>) -> ResolvedKatachi {
        ResolvedKatachi {
            katachi_id: "t".into(),
            harness: HarnessKind::Gemini,
            backend: BackendKind::Cli,
            selected_items: items,
            run_profile: RunProfile::default(),
            diagnostics: Vec::<Diagnostic>::new(),
        }
    }

    fn pick(kind: &str, id: &str) -> ResolvedItemRef {
        ResolvedItemRef {
            item: ItemRef::new(HarnessKind::Gemini, kind, id),
            reason: SelectionReason::Direct,
            pulled_in_by: None,
        }
    }

    fn settings_item(scope: &str, body: Value) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::SettingsLayer.as_str(),
                format!("settings:{scope}"),
            ),
            display_name: format!("settings.{scope}"),
            source: ItemSource {
                scope: Some(scope.into()),
                ..Default::default()
            },
            packaging: None,
            raw: body,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    fn make_catalog() -> RosterCatalog {
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(settings_item("user", serde_json::json!({"theme": "dark"})))
            .unwrap();
        cat.insert_item(settings_item(
            "project",
            serde_json::json!({"model": "gemini-3"}),
        ))
        .unwrap();
        cat
    }

    fn fixture_extension(root_parent: &Utf8Path, name: &str) -> DiscoveredExtension {
        let root = root_parent.join(name);
        fs::create_dir_all(root.as_std_path()).unwrap();
        fs::write(
            root.join("gemini-extension.json").as_std_path(),
            format!(r#"{{"name": "{name}"}}"#),
        )
        .unwrap();
        DiscoveredExtension {
            root,
            manifest: crate::extension::ExtensionManifest::from_json(
                serde_json::json!({"name": name}),
            )
            .unwrap(),
            contexts: Vec::new(),
            skills: Vec::new(),
            subagents: Vec::new(),
            hook_sets: Vec::new(),
            policy_sets: Vec::new(),
            mcp_servers: Vec::new(),
            themes: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn materialize_produces_expected_layout() {
        let tmp = TempDir::new().unwrap();
        let utf_tmp = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let ext = fixture_extension(&utf_tmp, "workspace-a11y");
        let resolved = resolved_with(vec![pick("extension", "workspace-a11y")]);
        let catalog = make_catalog();
        let (_overlay, manifest) = materialize_overlay(&resolved, &catalog, &[ext]).unwrap();

        assert!(manifest.home_dir.exists());
        assert!(manifest
            .generated_files
            .iter()
            .any(|p| p.ends_with("settings.json")));
        assert!(manifest.env.contains_key("HOME"));
        assert_eq!(manifest.extensions.len(), 1);
        // Symlink is present on Unix.
        #[cfg(unix)]
        {
            let link = manifest
                .overlay_root
                .join("home/.gemini/extensions/workspace-a11y");
            assert!(link.exists());
        }
    }

    #[test]
    fn materialization_plan_is_serializable() {
        let manifest = OverlayManifest {
            overlay_root: Utf8PathBuf::from("/tmp/x"),
            home_dir: Utf8PathBuf::from("/tmp/x/home"),
            project_dir: Utf8PathBuf::from("/tmp/x/project"),
            extensions: vec![Utf8PathBuf::from("home/.gemini/extensions/a")],
            generated_files: vec![Utf8PathBuf::from("home/.gemini/settings.json")],
            env: BTreeMap::from([("HOME".into(), "/tmp/x/home".into())]),
        };
        let plan = to_materialization_plan(&manifest);
        let j = serde_json::to_value(&plan).unwrap();
        assert_eq!(j["mode"], "temp-overlay");
        assert_eq!(j["overlay_root"], "/tmp/x");
    }

    #[test]
    fn summary_includes_extensions_and_env() {
        let manifest = OverlayManifest {
            overlay_root: Utf8PathBuf::from("/x"),
            home_dir: Utf8PathBuf::from("/x/home"),
            project_dir: Utf8PathBuf::from("/x/project"),
            extensions: vec![Utf8PathBuf::from("a")],
            generated_files: Vec::new(),
            env: BTreeMap::from([("FOO".into(), "bar".into())]),
        };
        let summary = summarize_manifest(&manifest);
        assert!(summary.contains("extensions: 1"));
        assert!(summary.contains("env FOO=bar"));
    }

    #[test]
    fn user_settings_ref_finds_user_layer() {
        let cat = make_catalog();
        let item = user_settings_ref(&cat).unwrap();
        assert_eq!(item.kind, "settings_layer");
        assert_eq!(item.id, "settings:user");
    }

    #[test]
    fn context_longer_than_preview_is_materialized_in_full() {
        use crate::context::{to_discovered_item, ContextScope, ContextSource};

        let tmp = TempDir::new().unwrap();
        let utf_tmp = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let ctx_path = utf_tmp.join("GEMINI.md");
        // Body well past the 512-byte preview cutoff with a sentinel near
        // the end to prove the tail survived.
        let body = format!("{}TAIL_SENTINEL\n", "x".repeat(2048));
        fs::write(ctx_path.as_std_path(), &body).unwrap();
        let cs = ContextSource {
            path: ctx_path.clone(),
            scope: ContextScope::Project,
            file_name: "GEMINI.md".into(),
            body_bytes: body.len() as u64,
        };
        let mut cat = make_catalog();
        cat.insert_item(to_discovered_item(&cs)).unwrap();

        let resolved = resolved_with(Vec::new());
        let (_overlay, manifest) = materialize_overlay(&resolved, &cat, &[]).unwrap();

        let written = fs::read_to_string(
            manifest
                .overlay_root
                .join(paths::PROJECT_CONTEXT)
                .as_std_path(),
        )
        .unwrap();
        assert_eq!(written, body);
        assert!(written.contains("TAIL_SENTINEL"));
    }

    #[test]
    fn overlay_defaults_to_discard_so_successful_runs_clean_up() {
        // The materializer must not pre-mark overlays as `Keep` — that
        // would leak `/tmp/katachi-gemini-*` directories on every
        // successful run. Preservation is the caller's responsibility,
        // applied only after a failure is observed.
        let resolved = resolved_with(Vec::new());
        let catalog = make_catalog();
        let overlay_root = {
            let (overlay, _manifest) = materialize_overlay(&resolved, &catalog, &[]).unwrap();
            let root = overlay.root().to_owned();
            assert!(root.exists(), "overlay root should exist before drop");
            root
            // overlay drops here
        };
        assert!(
            !overlay_root.exists(),
            "overlay at `{overlay_root}` should be cleaned up on drop"
        );
    }

    #[test]
    fn overlay_survives_drop_when_caller_flips_to_keep() {
        // Mirrors what `katachi-cli` does after a failed run: it flips
        // the keep policy to `Keep` so the overlay can be inspected.
        use katachi_core::materialize::KeepPolicy;
        let resolved = resolved_with(Vec::new());
        let catalog = make_catalog();
        let overlay_root = {
            let (mut overlay, _manifest) = materialize_overlay(&resolved, &catalog, &[]).unwrap();
            overlay.set_keep(KeepPolicy::Keep);
            overlay.root().to_owned()
            // overlay drops here, but Keep should preserve it
        };
        assert!(
            overlay_root.exists(),
            "overlay at `{overlay_root}` should survive drop after Keep flip"
        );
        // Clean up after ourselves so we don't litter /tmp.
        let _ = fs::remove_dir_all(overlay_root.as_std_path());
    }
}
